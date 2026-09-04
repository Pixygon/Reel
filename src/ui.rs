//! The egui UI — player transport, editor timeline, export dialog, shortcuts.
//! Draws entirely from ReelApp state; interactions call back into the
//! Player / Project / export job.

use crate::app::{Mode, ReelApp};
use crate::edit::TrackKind;
use crate::export::{self, AudioMode, Codec, Fit, Quality, Resolution};
use crate::theme;
use egui::{Color32, Key, Rect, RichText, Sense, Stroke, Vec2};

pub fn draw(ctx: &egui::Context, app: &mut ReelApp) {
    app.poll_picker();
    app.poll_opening();
    app.poll_captures();
    app.poll_queue();
    app.poll_audition();
    app.poll_tracking();
    app.poll_nesting();
    #[cfg(target_os = "linux")]
    app.poll_streamer();
    if let Some(z) = app.pending_zoom.take() {
        ctx.set_zoom_factor(z.clamp(0.7, 2.0));
    }
    app.poll_autosave();
    app.poll_captions();
    app.update_editor_playback();
    app.track_status();
    dropped_files(ctx, app);
    shortcuts(ctx, app);

    // Any real input keeps the control overlay awake.
    let active = ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving() || i.pointer.any_down());
    if active {
        app.touch_activity();
    }

    defaults_banner(ctx, app);

    // Panel order is shared by both modes so the side panel can animate
    // between them: bottom panels first (they own the full width), then the
    // side panel (which therefore never resizes the timeline), then the
    // picture in whatever space is left.
    // The timeline is the only full-width element, and it slides in and out
    // with the mode. Everything else (side panel, preview, transport) shares
    // the space above it.
    let editing = app.mode == Mode::Editor;
    egui::TopBottomPanel::bottom("timeline_panel")
        .resizable(true)
        .default_height(240.0)
        .show_animated(ctx, editing, |ui| {
            timeline(ui, app);
        });
    media_panel_frame(ctx, editing, |ui| media_panel_contents(ui, app));
    match app.mode {
        Mode::Player => player_view(ctx, app),
        Mode::Editor => {
            egui::CentralPanel::default().show(ctx, |ui| {
                // The transport belongs to the PREVIEW, so it lives inside
                // the central area and matches the picture's width — not the
                // full-width timeline below it.
                egui::TopBottomPanel::bottom("editor_chrome")
                    .exact_height(94.0)
                    .show_inside(ui, |ui| {
                        let inner = ui.max_rect().shrink2(Vec2::new(10.0, 6.0));
                        let mut child = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(inner)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        chrome(&mut child, app);
                    });
                viewport(ui, app);
            });
        }
    }

    export_window(ctx, app);
    keymap_window(ctx, app);
    defaults_window(ctx, app);
}

/// Transient status toast (top center) — the player has no status bar.
fn toast(ctx: &egui::Context, app: &ReelApp) {
    if app.status.is_empty() || app.status_at.elapsed().as_secs_f32() > 5.0 {
        return;
    }
    egui::Area::new(egui::Id::new("toast"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 16.0])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(Color32::from_black_alpha(190))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    ui.label(RichText::new(&app.status).color(theme::CYAN));
                });
        });
}

/// One-time nudge: Reel's front door is a double-click on a media file, so
/// becoming the default handler is the setup step that matters.
fn defaults_banner(ctx: &egui::Context, app: &mut ReelApp) {
    if !app.defaults_banner {
        return;
    }
    egui::TopBottomPanel::top("defaults_banner").show(ctx, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Open your media with Reel by default?").color(theme::STAR));
            ui.checkbox(&mut app.def_video, "Video");
            ui.checkbox(&mut app.def_audio, "Music");
            ui.checkbox(&mut app.def_images, "Images");
            if ui.button(RichText::new("Make default").color(theme::CYAN)).clicked() {
                app.status = app.apply_defaults();
            }
            if ui.button("Later").clicked() {
                app.finish_defaults_prompt();
                app.status = "You can set defaults any time under ⚙ → Default apps.".into();
            }
        });
    });
}

/// ⚙ → Default apps — same choices as the banner, reachable forever.
fn defaults_window(ctx: &egui::Context, app: &mut ReelApp) {
    if !app.defaults_open {
        return;
    }
    let mut keep_open = app.defaults_open;
    egui::Window::new("Default apps")
        .open(&mut keep_open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Open these with Reel when you double-click them:");
            ui.add_space(6.0);
            #[cfg(target_os = "linux")]
            {
                let current = |m: &str| {
                    if crate::integration::is_default_for(m) { "  (current default: Reel)" } else { "" }
                };
                ui.checkbox(&mut app.def_video, format!("Video — mp4, mkv, webm, …{}", current("video/mp4")));
                ui.checkbox(&mut app.def_audio, format!("Music — mp3, flac, opus, …{}", current("audio/mpeg")));
                ui.checkbox(&mut app.def_images, format!("Images — png, jpg, svg, …{}", current("image/png")));
            }
            #[cfg(not(target_os = "linux"))]
            {
                ui.checkbox(&mut app.def_video, "Video — mp4, mkv, webm, …");
                ui.checkbox(&mut app.def_audio, "Music — mp3, flac, opus, …");
                ui.checkbox(&mut app.def_images, "Images — png, jpg, svg, …");
            }
            ui.add_space(8.0);
            if ui.button(RichText::new("Apply").color(theme::CYAN)).clicked() {
                app.status = app.apply_defaults();
            }
            ui.label(
                RichText::new("Reel always stays available under “Open with” either way.")
                    .small()
                    .color(egui::Color32::from_gray(120)),
            );
        });
    app.defaults_open = keep_open;
}

/// A file dragged onto the window opens it — the shortest path to playing.
fn dropped_files(ctx: &egui::Context, app: &mut ReelApp) {
    let dropped = ctx.input(|i| {
        i.raw.dropped_files.first().and_then(|f| f.path.clone())
    });
    if let Some(path) = dropped {
        app.open(&path.to_string_lossy());
    }
}

/// Global keyboard control, mpv/VLC muscle-memory compatible. Inactive while
/// a text field (or any focused widget) wants the keyboard.
fn shortcuts(ctx: &egui::Context, app: &mut ReelApp) {
    if ctx.wants_keyboard_input() {
        return;
    }
    struct Keys {
        space: bool,
        right: bool,
        left: bool,
        shift: bool,
        step_fwd: bool,
        step_back: bool,
        vol_up: bool,
        vol_down: bool,
        mute: bool,
        looping: bool,
        shuttle_fwd: bool,
        shuttle_back: bool,
        shuttle_stop: bool,
        fullscreen: bool,
        escape: bool,
        edit: bool,
        speed_up: bool,
        speed_down: bool,
        speed_reset: bool,
        viz: bool,
    }
    let k = ctx.input(|i| Keys {
        space: i.key_pressed(Key::Space),
        // Ctrl+arrow jumps between markers in the editor, so plain seek has
        // to stay out of its way.
        right: i.key_pressed(Key::ArrowRight) && !i.modifiers.ctrl && !i.modifiers.command,
        left: i.key_pressed(Key::ArrowLeft) && !i.modifiers.ctrl && !i.modifiers.command,
        shift: i.modifiers.shift,
        step_fwd: i.key_pressed(Key::Period),
        step_back: i.key_pressed(Key::Comma),
        vol_up: i.key_pressed(Key::ArrowUp),
        vol_down: i.key_pressed(Key::ArrowDown),
        // Ctrl+M drops a marker; it must not also mute.
        mute: i.key_pressed(Key::M) && !i.modifiers.ctrl && !i.modifiers.command,
        looping: i.key_pressed(Key::L) && i.modifiers.shift,
        shuttle_fwd: i.key_pressed(Key::L) && !i.modifiers.shift,
        shuttle_back: i.key_pressed(Key::J),
        shuttle_stop: i.key_pressed(Key::K),
        fullscreen: i.key_pressed(Key::F) || i.key_pressed(Key::F11),
        escape: i.key_pressed(Key::Escape),
        edit: i.key_pressed(Key::E),
        speed_up: i.key_pressed(Key::CloseBracket),
        speed_down: i.key_pressed(Key::OpenBracket),
        speed_reset: i.key_pressed(Key::Backspace),
        viz: i.key_pressed(Key::V),
    });

    let mut shuttle_note: Option<String> = None;
    if let Some(player) = app.player.as_mut() {
        if k.space {
            player.toggle_play();
        }
        let jump = if k.shift { 60.0 } else { 5.0 };
        if k.right {
            player.seek_by(jump);
        }
        if k.left {
            player.seek_by(-jump);
        }
        if k.step_fwd {
            player.frame_step(true);
        }
        if k.step_back {
            player.frame_step(false);
        }
        if k.vol_up {
            player.set_volume(player.volume + 5.0);
        }
        if k.vol_down {
            player.set_volume(player.volume - 5.0);
        }
        if k.mute {
            app.user_muted = !app.user_muted;
            player.set_muted(app.user_muted);
        }
        if k.looping {
            player.set_looping(!player.looping);
        }
        // J-K-L: the shuttle every editor's left hand already knows.
        if k.shuttle_back || k.shuttle_fwd {
            let rate = player.shuttle(k.shuttle_fwd);
            shuttle_note = Some(if rate < 0.0 {
                format!("◀◀ {:.0}× reverse", -rate)
            } else {
                format!("{rate:.0}× forward")
            });
        }
        if k.shuttle_stop {
            player.shuttle_stop();
            shuttle_note = Some("paused".into());
        }
        // In the editor the effective mpv rate is user_rate × the active
        // clip's own rate — so the keys drive the USER's dial, and a sped
        // clip never swallows the adjustment.
        if k.speed_up {
            if app.mode == Mode::Editor {
                app.user_rate = (app.user_rate + 0.25).min(4.0);
            } else {
                player.set_speed(player.speed + 0.25);
            }
        }
        if k.speed_down {
            if app.mode == Mode::Editor {
                app.user_rate = (app.user_rate - 0.25).max(0.25);
            } else {
                player.set_speed(player.speed - 0.25);
            }
        }
        if k.speed_reset {
            if app.mode == Mode::Editor {
                app.user_rate = 1.0;
            } else {
                player.set_speed(1.0);
            }
        }
        if k.viz && player.supports_visualizer() {
            player.set_visualizer(player.visualizer.next());
        }
    }
    if let Some(note) = shuttle_note {
        app.status = note;
    }
    if k.fullscreen {
        app.fullscreen = !app.fullscreen;
    }
    if k.escape && app.fullscreen {
        app.fullscreen = false;
    }
    if k.edit {
        if app.mode == Mode::Editor {
            app.mode = Mode::Player;
        } else {
            app.enter_editor();
        }
    }

    // Editor-only keys.
    if app.mode == Mode::Editor {
        struct EdKeys {
            split: bool,
            delete: bool,
            ripple_delete: bool,
            trim_head: bool,
            trim_tail: bool,
            undo: bool,
            redo: bool,
            save: bool,
            copy: bool,
            paste: bool,
            duplicate: bool,
            marker: bool,
            next_marker: bool,
            prev_marker: bool,
        }
        let ek = ctx.input(|i| EdKeys {
            split: (i.key_pressed(Key::S) && !i.modifiers.ctrl && !i.modifiers.command)
                || ((i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(Key::K)),
            delete: i.key_pressed(Key::Delete) && !i.modifiers.shift,
            ripple_delete: i.key_pressed(Key::Delete) && i.modifiers.shift,
            trim_head: i.key_pressed(Key::Q),
            trim_tail: i.key_pressed(Key::W),
            undo: (i.modifiers.ctrl || i.modifiers.command) && !i.modifiers.shift && i.key_pressed(Key::Z),
            redo: (i.modifiers.ctrl || i.modifiers.command)
                && (i.key_pressed(Key::Y) || (i.modifiers.shift && i.key_pressed(Key::Z))),
            save: (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(Key::S),
            copy: (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(Key::C),
            paste: (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(Key::V),
            duplicate: (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(Key::D),
            marker: (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(Key::M),
            next_marker: (i.modifiers.ctrl || i.modifiers.command)
                && i.key_pressed(Key::ArrowRight),
            prev_marker: (i.modifiers.ctrl || i.modifiers.command)
                && i.key_pressed(Key::ArrowLeft),
        });
        if ek.split {
            app.editor_split();
        }
        if ek.delete {
            app.editor_delete();
        }
        if ek.ripple_delete {
            app.editor_ripple_delete();
        }
        if ek.trim_head {
            app.editor_ripple_trim(true);
        }
        if ek.trim_tail {
            app.editor_ripple_trim(false);
        }
        if ek.undo {
            app.editor.undo(&mut app.project);
        }
        if ek.redo {
            app.editor.redo(&mut app.project);
        }
        if ek.save {
            app.editor_save();
        }
        if ek.copy {
            app.editor_copy();
        }
        if ek.paste {
            app.editor_paste();
        }
        if ek.duplicate {
            app.editor_duplicate();
        }
        if ek.marker {
            app.editor_toggle_marker();
        }
        // Multicam: number keys cut to an angle at the playhead, live.
        if !app.project.multicam.is_empty() {
            let angle = ctx.input(|i| {
                [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5,
                 Key::Num6, Key::Num7, Key::Num8, Key::Num9]
                    .iter()
                    .position(|k| i.key_pressed(*k) && !i.modifiers.ctrl && !i.modifiers.command)
            });
            if let Some(a) = angle {
                if a < app.project.multicam.len() {
                    let t = app.editor.playhead;
                    app.editor.push_undo(&app.project);
                    if app.project.cut_to_angle(t, a) {
                        app.editor.mark_changed();
                        app.status = format!("Cut to angle {} at {t:.2}s.", a + 1);
                    }
                }
            }
        }
        if ek.next_marker {
            app.editor_jump_marker(true);
        }
        if ek.prev_marker {
            app.editor_jump_marker(false);
        }
        // In / out range markers.
        let (set_in, set_out, clear) = ctx.input(|i| {
            (
                i.key_pressed(Key::I) && !i.modifiers.shift,
                i.key_pressed(Key::O) && !i.modifiers.shift,
                (i.key_pressed(Key::I) || i.key_pressed(Key::O)) && i.modifiers.shift,
            )
        });
        if clear {
            app.editor.range_in = None;
            app.editor.range_out = None;
            app.status = "Export range cleared.".into();
        } else if set_in {
            let t = app.editor.playhead;
            app.editor.range_in = Some(t);
            if app.editor.range_out.is_some_and(|o| o <= t) {
                app.editor.range_out = None;
            }
            app.status = format!("Range in at {t:.2}s (Shift+I/O clears).");
        } else if set_out {
            let t = app.editor.playhead;
            app.editor.range_out = Some(t);
            if app.editor.range_in.is_some_and(|i| i >= t) {
                app.editor.range_in = None;
            }
            app.status = format!("Range out at {t:.2}s (Shift+I/O clears).");
        }
    }
}

/// The keyboard map — `?` (or F1) opens it; the shortcuts have outgrown
/// what anyone can hold in their head.
fn keymap_window(ctx: &egui::Context, app: &mut ReelApp) {
    if !app.show_keymap {
        return;
    }
    let mut open = true;
    egui::Window::new("Keyboard")
        .open(&mut open)
        .collapsible(false)
        .default_width(560.0)
        .show(ctx, |ui| {
            let sections: [(&str, &[(&str, &str)]); 4] = [
                ("Playback", &[
                    ("Space", "Play / pause"),
                    ("J · K · L", "Shuttle back / stop / forward (tap J or L again: faster)"),
                    (", · .", "Frame step back / forward"),
                    ("← / →", "Seek 5 s (Shift: 60 s)"),
                    ("↑ / ↓", "Volume"),
                    ("M", "Mute"),
                    ("[ · ] · Backspace", "Playback speed down / up / reset"),
                    ("Shift+L", "Loop"),
                    ("F / F11", "Fullscreen"),
                    ("V", "Audio visualizer (audio files)"),
                ]),
                ("Editing", &[
                    ("E", "Editor ↔ player"),
                    ("S (or Ctrl+K)", "Split at playhead"),
                    ("Q / W", "Trim head / tail to playhead"),
                    ("Delete", "Delete clip (Shift: ripple delete)"),
                    ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
                    ("Ctrl+C / V / D", "Copy / paste (inserts + ripples) / duplicate"),
                    ("I / O", "Export range in / out (Shift+I/O clears)"),
                    ("Ctrl+M", "Drop a marker"),
                    ("Ctrl+← / →", "Jump between markers"),
                    ("1-9", "Multicam: cut to angle at the playhead"),
                ]),
                ("Timeline pointer", &[
                    ("Drag clip", "Move (multi-selection moves together)"),
                    ("Drag edge", "Trim"),
                    ("Ctrl+edge", "Roll the cut (tail edge rolls the next clip's head)"),
                    ("Alt+drag body", "Slip the source window"),
                    ("Ctrl+Alt+drag", "Slide between neighbours"),
                    ("Shift+click", "Add to selection"),
                    ("Shift+drag empty space", "Lasso-select clips"),
                    ("Ctrl+scroll", "Zoom the timeline"),
                ]),
                ("Source monitor", &[
                    ("▶ (pool item)", "Preview a pool item over the live edit"),
                    ("I / O", "Mark the piece you want"),
                    ("Enter", "Insert it at the playhead (three-point edit)"),
                ]),
            ];
            for (title, rows) in sections {
                ui.label(RichText::new(title).color(theme::CYAN));
                egui::Grid::new(title).num_columns(2).spacing([18.0, 2.0]).show(ui, |ui| {
                    for (key, what) in rows {
                        ui.label(RichText::new(*key).monospace().color(theme::STAR));
                        ui.label(RichText::new(*what).small());
                        ui.end_row();
                    }
                });
                ui.add_space(6.0);
            }
        });
    if !open {
        app.show_keymap = false;
    }
}

/// ☰ REEL — the app menu, living in the bottom control bar. Capture appears
/// here only as a fallback when no system tray is available (its real home).
fn reel_menu(ui: &mut egui::Ui, app: &mut ReelApp) {
    ui.menu_button(RichText::new("☰ REEL").strong().color(theme::STAR), |ui| {
        if ui.button("Open…").clicked() {
            app.open_picker();
            ui.close_menu();
        }
        ui.menu_button("UI scale", |ui| {
            for (label, z) in [("90%", 0.9f32), ("100%", 1.0), ("115%", 1.15), ("130%", 1.3), ("150%", 1.5)] {
                let current = (ui.ctx().zoom_factor() - z).abs() < 0.01;
                if ui.selectable_label(current, label).clicked() {
                    ui.ctx().set_zoom_factor(z);
                    // Persisted with the desktop-integration settings —
                    // which live in the Linux module today; elsewhere the
                    // choice holds for the session.
                    #[cfg(target_os = "linux")]
                    {
                        let mut s = crate::integration::load_settings();
                        s.ui_scale = z;
                        crate::integration::save_settings(&s);
                    }
                    ui.close_menu();
                }
            }
        });
        if ui.button("Default apps…").clicked() {
            app.defaults_open = true;
            ui.close_menu();
        }
        if ui.button("Website — reel.pixygon.io").clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab("https://reel.pixygon.io"));
            ui.close_menu();
        }
        if !app.tray_available {
            ui.separator();
            ui.menu_button("📷 Screenshot", |ui| {
                for (label, mode, delay) in [
                    ("Full screen", crate::capture::ShotMode::Full, 0.0),
                    ("Region…", crate::capture::ShotMode::Region, 0.0),
                    ("Window…", crate::capture::ShotMode::Window, 0.0),
                    ("Full screen after 3s", crate::capture::ShotMode::Full, 3.0),
                    ("Full screen after 10s", crate::capture::ShotMode::Full, 10.0),
                ] {
                    if ui.button(label).clicked() {
                        app.take_screenshot_after(mode, delay);
                        ui.close_menu();
                    }
                }
            });
            let rec_label = if app.recorder.is_some() {
                "⏹ Stop recording"
            } else if app.record_starting() {
                "⏺ starting…"
            } else {
                "⏺ Record screen…"
            };
            if ui.button(rec_label).clicked() {
                if !app.record_starting() {
                    app.toggle_record();
                }
                ui.close_menu();
            }
            #[cfg(target_os = "linux")]
            if app.recorder.is_none()
                && !app.record_starting()
                && ui.button("⏺ Record webcam").on_hover_text(
                    "Record the camera (plus the default mic when one answers) to ~/Videos",
                ).clicked()
            {
                app.toggle_webcam_record();
                ui.close_menu();
            }
            #[cfg(target_os = "linux")]
            {
                let streamer_label = if app.streamer.is_some() {
                    "⏹ Stop streamer layout"
                } else {
                    "⏺ Streamer layout (screen + cam)"
                };
                if ui.button(streamer_label).on_hover_text(
                    "Record the screen AND the camera together; stopping builds a \
                     ready-made project — screen full-frame, camera as a corner \
                     PiP you can still move, resize or recut",
                ).clicked() {
                    app.toggle_streamer_record();
                    ui.close_menu();
                }
            }
        }
        ui.separator();
        if ui.button("Quit").clicked() {
            app.quit_requested = true;
        }
    });
}

fn player_view(ctx: &egui::Context, app: &mut ReelApp) {
    if app.player.is_none() && app.image.is_none() {
        egui::CentralPanel::default().show(ctx, |ui| empty_state(ui, app));
        toast(ctx, app);
        return;
    }

    // The media owns the whole window; controls are an overlay that fades
    // after a few idle seconds during playback.
    let playing = app.player.as_ref().map(|p| p.playing).unwrap_or(false);
    const SHOW_FOR: f32 = 2.5;
    const FADE_OVER: f32 = 0.45;
    const CHROME_H: f32 = 86.0;
    let idle = app.last_activity.elapsed().as_secs_f32();
    let alpha = if !playing {
        1.0
    } else if idle < SHOW_FOR {
        1.0
    } else {
        (1.0 - (idle - SHOW_FOR) / FADE_OVER).clamp(0.0, 1.0)
    };

    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
        viewport(ui, app);
        if alpha <= 0.0 {
            // Fully faded: the video is the interface. Hide the cursor too.
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            return;
        }
        // The overlay is a child ui pinned to the panel's bottom strip —
        // an explicit max_rect, so the width-greedy seek slider and
        // columns() are hard-bounded to the window. (An anchored Area has
        // an unbounded max_rect: its layout inflated off-screen — the
        // invisible-controls bug.)
        let screen = ui.max_rect();
        let strip = Rect::from_min_max(
            egui::pos2(screen.left(), screen.bottom() - CHROME_H),
            screen.right_bottom(),
        );
        ui.painter()
            .rect_filled(strip, 0.0, Color32::from_black_alpha((170.0 * alpha) as u8));
        let inner = strip.shrink2(Vec2::new(16.0, 9.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_opacity(alpha);
        chrome(&mut child, app);
        if ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|p| strip.contains(p)) {
            app.touch_activity();
        }
    });
    toast(ctx, app);
}

/// Nothing open — normally never seen (Reel is opened by clicking a file),
/// so make the one action obvious.
fn empty_state(ui: &mut egui::Ui, app: &mut ReelApp) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.30);
        ui.label(RichText::new("𝄞").size(56.0).color(theme::CYAN));
        ui.add_space(12.0);
        if ui
            .add(egui::Button::new(RichText::new("  Open a file…  ").size(18.0).color(theme::STAR)))
            .clicked()
        {
            app.open_picker();
        }
        ui.add_space(10.0);
        let dnd = !cfg!(target_os = "linux") || std::env::var("WAYLAND_DISPLAY").is_err();
        let hint = if dnd {
            "…or drop one here, or double-click any video, song or image in your file manager."
        } else {
            "…or double-click any video, song or image in your file manager — Reel opens it directly."
        };
        ui.label(RichText::new(hint).color(egui::Color32::from_gray(120)));
    });
}

/// The media/inspector panel's chrome: resizable, animated in and out, and
/// scrollable so its contents can never impose a minimum width that fights
/// the user's drag (that was the "resize snaps back" bug).
pub fn media_panel_frame(
    ctx: &egui::Context,
    open: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::SidePanel::left("media")
        .resizable(true)
        .default_width(240.0)
        .width_range(150.0..=560.0)
        .show_animated(ctx, open, |ui| {
            // `both` (not `vertical`): with only vertical scrolling, content
            // that is wider than the panel pushes the panel's minimum width
            // out, and the user's drag springs back. Here it just scrolls.
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // Sliders default to a fixed width plus their label; size
                    // them to the panel instead so a narrow panel stays narrow.
                    ui.spacing_mut().slider_width = (ui.available_width() - 96.0).clamp(48.0, 220.0);
                    add_contents(ui);
                });
        });
}

/// The media/inspector contents — drawn inside `media_panel_frame`.
fn media_panel_contents(ui: &mut egui::Ui, app: &mut ReelApp) {
    {
        ui.heading("Project");
        ui.label(format!("{} — {}×{} @ {:.0}fps", app.project.name, app.project.width, app.project.height, app.project.fps));
        ui.separator();
        // ── Media pool ──────────────────────────────────────────────────
        // Everything gathered for the edit, on the timeline or not. Search
        // filters; offline files (moved out from under the project) get a
        // relink button instead of a broken render later.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Media pool").color(theme::CYAN));
            if ui.small_button("+ Gather").on_hover_text("Add media to the pool without opening it").clicked() {
                app.picker_target = crate::app::PickerTarget::Pool;
                app.open_picker_inner_for_target();
            }
        });
        app.project.absorb_sources_into_pool();
        ui.add(egui::TextEdit::singleline(&mut app.pool_search).hint_text("search…").desired_width(f32::INFINITY));
        let filter = app.pool_search.to_lowercase();
        let mut items: Vec<(String, String, bool)> = app
            .project
            .pool
            .iter()
            .filter(|i| filter.is_empty() || i.path.to_lowercase().contains(&filter) || i.bin.to_lowercase().contains(&filter))
            .map(|i| (i.path.clone(), i.bin.clone(), std::path::Path::new(&i.path).exists()))
            .collect();
        items.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        let mut last_bin: Option<String> = None;
        for (path, bin, online) in items {
            if last_bin.as_deref() != Some(bin.as_str()) {
                if !bin.is_empty() {
                    ui.label(RichText::new(format!("• {bin}")).small().color(egui::Color32::from_gray(150)));
                }
                last_bin = Some(bin.clone());
            }
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            ui.horizontal(|ui| {
                if online {
                    if path.ends_with(".reel") {
                        if ui.small_button("nest").on_hover_text(
                            "Compound clip: render this edit flat and drop it at the playhead. \
                             It refreshes automatically when the nested edit changes.",
                        ).clicked() {
                            app.start_nesting(&path, true);
                        }
                    } else if ui.small_button("▶").on_hover_text("Source monitor: preview, set I/O, insert").clicked() {
                        app.preview_source(&path);
                    }
                    ui.label(RichText::new(&name).small());
                } else {
                    ui.label(RichText::new(&name).small().color(theme::EMBER).strikethrough());
                    if ui.small_button("relink…").on_hover_text("The file moved — point at where it is now").clicked() {
                        app.picker_target = crate::app::PickerTarget::Relink(path.clone());
                        app.open_picker_inner_for_target();
                    }
                }
            });
        }

        ui.add_space(4.0);
        ui.label(RichText::new("Clips").color(theme::CYAN));
        for track in &app.project.tracks {
            for clip in &track.clips {
                let selected = app.editor.selected == Some(clip.id);
                if ui.selectable_label(selected, format!("• {} ({:.1}s)", clip.name, clip.duration)).clicked() {
                    app.editor.selected = Some(clip.id);
                }
            }
        }

        // ── Multicam ────────────────────────────────────────────────────
        if !app.project.multicam.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new("Multicam").color(theme::CYAN));
            let angles: Vec<(usize, String)> = app
                .project
                .multicam
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let n = std::path::Path::new(&a.source)
                        .file_stem()
                        .map(|x| x.to_string_lossy().into_owned())
                        .unwrap_or_else(|| a.source.clone());
                    (i, n)
                })
                .collect();
            for (i, n) in angles {
                ui.horizontal(|ui| {
                    if ui.small_button(format!("{}", i + 1)).on_hover_text("Cut to this angle at the playhead").clicked() {
                        let t = app.editor.playhead;
                        app.editor.push_undo(&app.project);
                        if app.project.cut_to_angle(t, i) {
                            app.editor.mark_changed();
                        }
                    }
                    ui.label(RichText::new(n).small());
                });
            }
            ui.label(
                RichText::new("Keys 1-9 cut to an angle at the playhead — live, while it plays.")
                    .small()
                    .color(egui::Color32::from_gray(140)),
            );
        }
        if let Some(id) = app.editor.selected {
            let info = app.project.clip(id).map(|c| (c.name.clone(), c.start, c.duration, c.in_point, c.effects));
            if let Some((name, start, duration, in_point, before)) = info {
                ui.separator();
                ui.label(RichText::new("Selected clip").color(theme::CYAN));
                ui.label(name);
                ui.label(RichText::new(format!(
                    "at {}  ·  {duration:.2}s long\nsource in-point {}",
                    fmt_time(start), fmt_time(in_point)
                )).small().color(egui::Color32::from_gray(150)));

                // ── Effects ─────────────────────────────────────────────
                // What you set here is what the export renders — the preview
                // and the ffmpeg filters share one formula (effects.rs).
                ui.add_space(6.0);
                ui.label(RichText::new("Look").color(theme::CYAN));
                let mut fx = before;
                ui.add(egui::Slider::new(&mut fx.exposure, 0.2..=2.0).text("Exposure"));
                ui.add(egui::Slider::new(&mut fx.contrast, 0.2..=2.5).text("Contrast"));
                ui.add(egui::Slider::new(&mut fx.saturation, 0.0..=2.5).text("Saturation"));
                // Levels + white balance — baked into the grade lattice
                // with the LUT and curves, so preview cost stays one sample.
                ui.collapsing("Levels & balance", |ui| {
                    ui.add(egui::Slider::new(&mut fx.levels_black, 0.0..=0.5).text("Black point"));
                    ui.add(egui::Slider::new(&mut fx.levels_white, 0.5..=1.5).text("White point"));
                    ui.add(egui::Slider::new(&mut fx.levels_gamma, 0.2..=3.0).logarithmic(true).text("Gamma"));
                    ui.add(egui::Slider::new(&mut fx.wb_temp, -1.0..=1.0).text("Temperature"));
                    ui.add(egui::Slider::new(&mut fx.wb_tint, -1.0..=1.0).text("Tint"));
                    if ui.small_button("reset").clicked() {
                        fx.levels_black = 0.0;
                        fx.levels_white = 1.0;
                        fx.levels_gamma = 1.0;
                        fx.wb_temp = 0.0;
                        fx.wb_tint = 0.0;
                    }
                });
                // HSL qualifier — grab a hue/sat/lightness window, push it.
                ui.collapsing("HSL qualifier", |ui| {
                    let mut on = fx.hsl.is_some();
                    if ui.checkbox(&mut on, "Qualify").on_hover_text(
                        "Select pixels by hue, saturation and lightness, then push only those — the make-the-sky-bluer tool.",
                    ).changed() {
                        fx.hsl = on.then(crate::effects::Hsl::default);
                    }
                    if let Some(q) = &mut fx.hsl {
                        ui.add(egui::Slider::new(&mut q.hue, 0.0..=360.0).text("Hue centre"));
                        ui.add(egui::Slider::new(&mut q.hue_width, 5.0..=180.0).text("Hue width"));
                        ui.add(egui::Slider::new(&mut q.sat_min, 0.0..=1.0).text("Sat min"));
                        ui.add(egui::Slider::new(&mut q.sat_max, 0.0..=1.0).text("Sat max"));
                        ui.add(egui::Slider::new(&mut q.lum_min, 0.0..=1.0).text("Light min"));
                        ui.add(egui::Slider::new(&mut q.lum_max, 0.0..=1.0).text("Light max"));
                        ui.add(egui::Slider::new(&mut q.soft, 0.02..=0.5).text("Softness"));
                        ui.separator();
                        ui.add(egui::Slider::new(&mut q.push_hue, -180.0..=180.0).text("Push hue"));
                        ui.add(egui::Slider::new(&mut q.push_sat, 0.0..=2.0).text("Push sat"));
                        ui.add(egui::Slider::new(&mut q.push_lum, 0.0..=2.0).text("Push light"));
                    }
                });
                ui.add(egui::Slider::new(&mut fx.fade_in, 0.0..=duration.min(5.0)).text("Fade in (s)"));
                ui.add(egui::Slider::new(&mut fx.fade_out, 0.0..=duration.min(5.0)).text("Fade out (s)"));
                // Reframe — how you put a landscape shot into a vertical
                // frame without blurred sides.
                ui.horizontal(|ui| {
                    if ui.selectable_label(fx.flip_h, "Flip ↔").clicked() {
                        fx.flip_h = !fx.flip_h;
                    }
                    if ui.selectable_label(fx.flip_v, "Flip ↕").clicked() {
                        fx.flip_v = !fx.flip_v;
                    }
                    if ui.button("Rotate 90°").on_hover_text(
                        "Quarter turn clockwise (click again for 180°, 270°, back). \
                         The render rotates before fitting, so a turned portrait \
                         letterboxes correctly.",
                    ).clicked() {
                        fx.rotate = (fx.rotate + 1) % 4;
                    }
                    if fx.rotate % 4 != 0 {
                        ui.label(RichText::new(format!("{}°", fx.rotate as u16 * 90)).small().color(theme::EMBER));
                    }
                });
                ui.add(egui::Slider::new(&mut fx.zoom, 1.0..=3.0).text("Zoom"));
                if fx.zoom > 1.0001 {
                    ui.add(egui::Slider::new(&mut fx.pan_x, -1.0..=1.0).text("Pan ↔"));
                    ui.add(egui::Slider::new(&mut fx.pan_y, -1.0..=1.0).text("Pan ↕"));
                }

                // Picture-in-picture placement, for overlay clips only.
                let is_overlay = app.project.tracks.iter().any(|t| {
                    t.kind == crate::edit::TrackKind::Overlay
                        && t.clips.iter().any(|c| c.id == id)
                });
                if is_overlay {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Picture-in-picture").color(theme::CYAN));
                    let mut pip = app.project.clip(id).map(|c| c.pip).unwrap_or_default();
                    let before_pip = pip;
                    ui.add(egui::Slider::new(&mut pip.scale, 0.05..=1.0).text("Size"));
                    ui.add(egui::Slider::new(&mut pip.x, 0.02..=0.98).text("Across"));
                    ui.add(egui::Slider::new(&mut pip.y, 0.02..=0.98).text("Down"));
                    ui.horizontal(|ui| {
                        for (label, x, y) in [
                            ("↖", 0.24, 0.26), ("↗", 0.76, 0.26),
                            ("↙", 0.24, 0.74), ("↘", 0.76, 0.74),
                        ] {
                            if ui.small_button(label).clicked() {
                                pip.x = x;
                                pip.y = y;
                            }
                        }
                    });
                    if pip != before_pip {
                        if let Some(c) = app.project.clip_mut(id) {
                            c.pip = pip;
                        }
                        app.editor.mark_changed();
                    }
                    ui.label(
                        RichText::new("Drag it on the preview to place it. The inset shows a still, not live video.")
                            .small()
                            .color(egui::Color32::from_gray(140)),
                    );
                }

                // ── Keyframes ───────────────────────────────────────────
                // Animate any parameter: set a value at the playhead, and the
                // render evaluates the curve per frame. The preview shows the
                // same evaluation, so scrubbing plays the animation.
                ui.add_space(6.0);
                ui.label(RichText::new("Animate").color(theme::CYAN));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("keyparam")
                        .selected_text(app.editor.key_param.name())
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for q in crate::edit::Param::ALL {
                                ui.selectable_value(&mut app.editor.key_param, q, q.name());
                            }
                        });
                    if ui
                        .button("+ Key at playhead")
                        .on_hover_text("Capture this parameter's current value as a keyframe here")
                        .clicked()
                    {
                        let t = app.editor.playhead;
                        app.editor.push_undo(&app.project);
                        let param = app.editor.key_param;
                        if let Some(c) = app.project.clip_mut(id) {
                            let local = (t - c.start).clamp(0.0, c.duration);
                            let (fx2, pip2, op) = c.animated(local);
                            let v = match param {
                                crate::edit::Param::Exposure => fx2.exposure,
                                crate::edit::Param::Contrast => fx2.contrast,
                                crate::edit::Param::Saturation => fx2.saturation,
                                crate::edit::Param::Zoom => fx2.zoom,
                                crate::edit::Param::PanX => fx2.pan_x,
                                crate::edit::Param::PanY => fx2.pan_y,
                                crate::edit::Param::Opacity => op,
                                crate::edit::Param::PipX => pip2.x,
                                crate::edit::Param::PipY => pip2.y,
                                crate::edit::Param::PipScale => pip2.scale,
                                crate::edit::Param::Speed => c.speed,
                                crate::edit::Param::MaskX => {
                                    fx2.mask.map(|m| m.cx).unwrap_or(0.5)
                                }
                                crate::edit::Param::MaskY => {
                                    fx2.mask.map(|m| m.cy).unwrap_or(0.5)
                                }
                                crate::edit::Param::MaskW => {
                                    fx2.mask.map(|m| m.w).unwrap_or(0.25)
                                }
                                crate::edit::Param::MaskH => {
                                    fx2.mask.map(|m| m.h).unwrap_or(0.25)
                                }
                                crate::edit::Param::Plugin1 => fx2.plugin_params[0],
                                crate::edit::Param::Plugin2 => fx2.plugin_params[1],
                                crate::edit::Param::Plugin3 => fx2.plugin_params[2],
                                crate::edit::Param::Plugin4 => fx2.plugin_params[3],
                            };
                            c.set_key(param, local, v, crate::edit::Interp::Linear);
                        }
                        app.editor.mark_changed();
                    }
                });
                // ── The curve editor ────────────────────────────────────
                // The selected parameter's curve over the clip, live: drag a
                // diamond to move it in time and value, double-click to add
                // a key under the cursor, right-click a diamond to remove
                // it. The playhead line ties it to what the preview shows.
                // Follow the keys: when the picked parameter has none but
                // the clip is animated, show the first animated curve rather
                // than an empty strip.
                if app
                    .project
                    .clip(id)
                    .is_some_and(|c| c.key_track(app.editor.key_param).is_none() && !c.keys.is_empty())
                {
                    if let Some(q) = app.project.clip(id).and_then(|c| c.keys.first().map(|(q, _)| *q)) {
                        app.editor.key_param = q;
                    }
                }
                curve_editor(ui, app, id, duration);

                let key_rows: Vec<(crate::edit::Param, f64, f32)> = app
                    .project
                    .clip(id)
                    .map(|c| {
                        c.keys
                            .iter()
                            .flat_map(|(q, ks)| ks.iter().map(|k| (*q, k.t, k.value)))
                            .collect()
                    })
                    .unwrap_or_default();
                if !key_rows.is_empty() {
                    let mut remove: Option<(crate::edit::Param, f64)> = None;
                    for (q, t, v) in &key_rows {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{} @ {t:.2}s = {v:.2}", q.name()))
                                    .small()
                                    .color(egui::Color32::from_gray(160)),
                            );
                            if ui.small_button("−").clicked() {
                                remove = Some((*q, *t));
                            }
                        });
                    }
                    if let Some((q, t)) = remove {
                        app.editor.push_undo(&app.project);
                        if let Some(c) = app.project.clip_mut(id) {
                            c.clear_key(q, t);
                        }
                        app.editor.mark_changed();
                    }
                }

                // ── LUT ─────────────────────────────────────────────────
                ui.horizontal(|ui| {
                    let lut_idx = app.project.clip(id).and_then(|c| c.effects.lut);
                    match lut_idx.and_then(|i| app.project.lut_path(i)).map(String::from) {
                        Some(path) => {
                            let name = std::path::Path::new(&path)
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or(path);
                            ui.label(RichText::new(format!("LUT: {name}")).small());
                            if ui.small_button("−").on_hover_text("Remove the LUT").clicked() {
                                app.editor.push_undo(&app.project);
                                if let Some(c) = app.project.clip_mut(id) {
                                    c.effects.lut = None;
                                }
                                app.editor.mark_changed();
                            }
                        }
                        None => {
                            if ui
                                .button("LUT…")
                                .on_hover_text("Grade through a .cube 3D LUT — applied before the sliders above")
                                .clicked()
                            {
                                app.pick_lut(id);
                            }
                        }
                    }
                });

                // ── Curves ──────────────────────────────────────────────
                {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Curves").color(theme::CYAN));
                        for (i, label) in ["All", "R", "G", "B"].iter().enumerate() {
                            if ui
                                .selectable_label(app.curve_channel == i, *label)
                                .clicked()
                            {
                                app.curve_channel = i;
                            }
                        }
                        let has = app
                            .project
                            .clip(id)
                            .and_then(|c| c.effects.curves)
                            .map(|c| !c.is_identity())
                            .unwrap_or(false);
                        if has && ui.small_button("Reset").clicked() {
                            app.editor.push_undo(&app.project);
                            if let Some(c) = app.project.clip_mut(id) {
                                c.effects.curves = None;
                            }
                            app.editor.mark_changed();
                        }
                    });
                    tone_curve_editor(ui, app, id);
                }

                // ── Power window ────────────────────────────────────────
                {
                    let mut fxm = app.project.clip(id).map(|c| c.effects).unwrap_or_default();
                    let before_m = fxm;
                    let mut kind = match fxm.mask {
                        None => 0,
                        Some(m) if m.shape == crate::effects::MaskShape::Ellipse => 1,
                        Some(_) => 2,
                    };
                    egui::ComboBox::from_id_salt("maskkind")
                        .selected_text(["No mask", "Ellipse mask", "Rectangle mask"][kind])
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut kind, 0, "No mask");
                            ui.selectable_value(&mut kind, 1, "Ellipse mask");
                            ui.selectable_value(&mut kind, 2, "Rectangle mask");
                        });
                    match kind {
                        0 => fxm.mask = None,
                        k => {
                            let mut m = fxm.mask.unwrap_or_default();
                            m.shape = if k == 1 {
                                crate::effects::MaskShape::Ellipse
                            } else {
                                crate::effects::MaskShape::Rect
                            };
                            fxm.mask = Some(m);
                        }
                    }
                    if let Some(m) = &mut fxm.mask {
                        ui.add(egui::Slider::new(&mut m.cx, 0.0..=1.0).text("Across"));
                        ui.add(egui::Slider::new(&mut m.cy, 0.0..=1.0).text("Down"));
                        ui.add(egui::Slider::new(&mut m.w, 0.02..=0.8).text("Width"));
                        ui.add(egui::Slider::new(&mut m.h, 0.02..=0.8).text("Height"));
                        ui.add(egui::Slider::new(&mut m.feather, 0.0..=0.3).text("Feather"));
                        ui.checkbox(&mut m.invert, "Grade outside instead");
                        // Auto tracking: follow whatever is under the window
                        // and keyframe the window onto its path.
                        if app.tracking.as_ref().map(|(_, tid)| *tid) == Some(id) {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("tracking the subject…");
                            });
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                        } else if ui
                            .button("Track subject")
                            .on_hover_text(
                                "Follow what's under the window through the clip and keyframe \
                                 the window onto its path (replaces mask-x/y keyframes)",
                            )
                            .clicked()
                        {
                            app.start_tracking(id);
                        }
                        ui.label(
                            RichText::new("The colour grade (LUT + sliders) applies only where the window says. Track subject follows what's under it.")
                                .small()
                                .color(egui::Color32::from_gray(140)),
                        );
                    }
                    if fxm != before_m {
                        if let Some(c) = app.project.clip_mut(id) {
                            c.effects = fxm;
                        }
                        app.editor.mark_changed();
                    }
                }

                // ── Stabilise ───────────────────────────────────────────
                {
                    let mut st = app.project.clip(id).map(|c| c.stabilize).unwrap_or(false);
                    if ui
                        .checkbox(&mut st, "Stabilize on export")
                        .on_hover_text(
                            "Two-pass camera-shake smoothing at render time. The preview \
                             shows the raw footage — the analysis costs a full decode.",
                        )
                        .changed()
                    {
                        app.editor.push_undo(&app.project);
                        if let Some(c) = app.project.clip_mut(id) {
                            c.stabilize = st;
                        }
                        app.editor.mark_changed();
                    }
                    // The audition: render THIS clip stabilized (720p, its
                    // own window only) and play the result — the one way to
                    // judge vidstab before committing to a full export.
                    if app.audition.is_some() {
                        let frac = app.audition.as_ref().map(|(j, _, _)| j.state().fraction).unwrap_or(0.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("auditioning… {:.0}%", frac * 100.0));
                            if ui.small_button("cancel").clicked() {
                                if let Some((job, out, _)) = app.audition.take() {
                                    job.cancel();
                                    let _ = std::fs::remove_file(out);
                                }
                            }
                        });
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                    } else if ui
                        .button("Audition stabilization")
                        .on_hover_text(
                            "Render just this clip with stabilization (720p) and play it. \
                             The edit is untouched — press E to come back.",
                        )
                        .clicked()
                    {
                        app.start_audition(id);
                    }
                }

                // ── Green screen ────────────────────────────────────────
                {
                    let mut fx2 = app.project.clip(id).map(|c| c.effects).unwrap_or_default();
                    let before2 = fx2;
                    let mut keyed = fx2.key_color.is_some();
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut keyed, "Green screen").changed() {
                            fx2.key_color = if keyed {
                                Some([0.0, 0.69, 0.25])
                            } else {
                                None
                            };
                        }
                        if let Some(c) = &mut fx2.key_color {
                            let mut rgb = [
                                (c[0] * 255.0) as u8,
                                (c[1] * 255.0) as u8,
                                (c[2] * 255.0) as u8,
                            ];
                            if ui.color_edit_button_srgb(&mut rgb).changed() {
                                *c = [
                                    rgb[0] as f32 / 255.0,
                                    rgb[1] as f32 / 255.0,
                                    rgb[2] as f32 / 255.0,
                                ];
                            }
                        }
                    });
                    if fx2.key_color.is_some() {
                        ui.add(egui::Slider::new(&mut fx2.key_similarity, 0.02..=0.8).text("Reach"));
                        ui.add(egui::Slider::new(&mut fx2.key_softness, 0.0..=0.5).text("Soften"));
                        ui.label(
                            RichText::new("Keys live in the preview and the render alike. Put the clip on the overlay track to composite it over the cut.")
                                .small()
                                .color(egui::Color32::from_gray(140)),
                        );
                    }
                    if fx2 != before2 {
                        if let Some(c) = app.project.clip_mut(id) {
                            c.effects = fx2;
                        }
                        app.editor.mark_changed();
                    }
                }

                // Playback rate. Changing it keeps the footage and resizes
                // the clip's slot, which is what "speed this bit up" means.
                let mut rate = app.project.clip(id).map(|c| c.speed).unwrap_or(1.0);
                let before_rate = rate;
                ui.add(
                    egui::Slider::new(&mut rate, 0.25..=4.0)
                        .logarithmic(true)
                        .text("Speed")
                        .custom_formatter(|v, _| format!("{v:.2}×")),
                );
                if (rate - before_rate).abs() > 1e-4 {
                    if let Some(c) = app.project.clip_mut(id) {
                        let source = c.source_len();
                        c.speed = rate;
                        c.duration = source / rate.max(0.01) as f64;
                    }
                    app.editor.mark_changed();
                }

                // Level for this clip's own audio.
                let mut gain = app.project.clip(id).map(|c| c.gain_db).unwrap_or(0.0);
                let before_gain = gain;
                ui.add(
                    egui::Slider::new(&mut gain, -30.0..=12.0)
                        .text("Volume (dB)")
                        .custom_formatter(|v, _| {
                            if v <= -29.9 { "silent".into() } else { format!("{v:+.1} dB") }
                        }),
                );
                if (gain - before_gain).abs() > 1e-4 {
                    if let Some(c) = app.project.clip_mut(id) {
                        c.gain_db = if gain <= -29.9 { -100.0 } else { gain };
                    }
                    app.editor.mark_changed();
                }

                // Effect plugin: pick a .wgsl, get its declared sliders.
                {
                    ui.horizontal(|ui| {
                        let label = fx.plugin
                            .and_then(|i| app.project.plugin_path(i))
                            .and_then(|path| crate::plugins::load(path).ok().map(|p| p.name.clone()))
                            .unwrap_or_else(|| "Effect plugin…".into());
                        if ui.button(label).on_hover_text(
                            "A WGSL effect file — runs identically in the preview and the render. \
                             Drop files in ~/.config/reel/effects; see docs/PLUGINS.md",
                        ).clicked() {
                            app.picker_target = crate::app::PickerTarget::Plugin(id);
                            app.open_picker_inner_for_target();
                        }
                        if fx.plugin.is_some() && ui.small_button("off").clicked() {
                            fx.plugin = None;
                        }
                    });
                    if let Some(plug) = fx.plugin
                        .and_then(|i| app.project.plugin_path(i))
                        .and_then(|path| crate::plugins::load(path).ok())
                    {
                        for (i, d) in plug.params.iter().enumerate() {
                            ui.add(
                                egui::Slider::new(&mut fx.plugin_params[i], d.min..=d.max)
                                    .text(&d.name),
                            );
                        }
                    }
                }

                // Expert escape hatch: raw ffmpeg filters on this clip.
                {
                    let current = app.project.clip(id).and_then(|c| c.raw_filter.clone());
                    if let Some(raw) = &current {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("ffmpeg: {raw}"))
                                    .small()
                                    .monospace()
                                    .color(theme::EMBER),
                            );
                            if ui.small_button("off").clicked() {
                                app.editor.push_undo(&app.project);
                                if let Some(c) = app.project.clip_mut(id) {
                                    c.raw_filter = None;
                                }
                                app.editor.mark_changed();
                            }
                        });
                        ui.label(
                            RichText::new("Expert filter — applied at render; the live preview plays without it.")
                                .small()
                                .color(egui::Color32::from_gray(140)),
                        );
                    }
                }

                // Audio processing — pan, tone, dynamics, repair. One
                // model (Clip.audio) feeding the live mix and the export.
                {
                    let mut afx = app.project.clip(id).map(|c| c.audio).unwrap_or_default();
                    let before_afx = afx;
                    egui::CollapsingHeader::new("Audio")
                        .default_open(std::env::var("REEL_DEBUG_AUDIOPANEL").is_ok())
                        .show(ui, |ui| {
                        // A live spectrum of what's playing, right where the
                        // EQ decisions get made (low | mid | high thirds).
                        #[cfg(any(target_os = "linux", target_os = "windows"))]
                        if let Some(mixer) = &app.mixer {
                            let bins = 42usize;
                            let spec = mixer.spectrum(bins);
                            let width = ui.available_width().max(60.0);
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::hover());
                            let sp = ui.painter_at(rect);
                            sp.rect_filled(rect, 3.0, theme::VOID_2);
                            let bw = rect.width() / bins as f32;
                            for (i, v) in spec.iter().enumerate() {
                                let h = v * (rect.height() - 3.0);
                                if h > 0.5 {
                                    sp.rect_filled(
                                        Rect::from_min_max(
                                            egui::pos2(rect.left() + i as f32 * bw + 0.5, rect.bottom() - 1.5 - h),
                                            egui::pos2(rect.left() + (i + 1) as f32 * bw - 0.5, rect.bottom() - 1.5),
                                        ),
                                        1.0,
                                        Color32::from_rgba_unmultiplied(120, 220, 235, 130),
                                    );
                                }
                            }
                            // Band boundaries roughly at the shelf corners.
                            for f in [0.33f32, 0.66] {
                                let x = rect.left() + rect.width() * f;
                                sp.line_segment(
                                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                                    Stroke::new(0.5, Color32::from_gray(70)),
                                );
                            }
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
                            if std::env::var("REEL_DEBUG_AUDIOPANEL").is_ok() {
                                ui.scroll_to_rect(rect, Some(egui::Align::Center));
                            }
                        }
                        ui.add(egui::Slider::new(&mut afx.pan, -1.0..=1.0).text("Pan"));
                        ui.add(egui::Slider::new(&mut afx.eq_low, -18.0..=18.0).text("Low (dB)"));
                        ui.add(egui::Slider::new(&mut afx.eq_mid, -18.0..=18.0).text("Mid (dB)"));
                        if afx.eq_mid.abs() > 0.01 {
                            ui.add(
                                egui::Slider::new(&mut afx.eq_mid_freq, 200.0..=8000.0)
                                    .logarithmic(true)
                                    .text("Mid freq (Hz)"),
                            );
                        }
                        ui.add(egui::Slider::new(&mut afx.eq_high, -18.0..=18.0).text("High (dB)"));
                        ui.checkbox(&mut afx.comp, "Compressor").on_hover_text(
                            "Even out the level: quiet parts stay, loud parts come down",
                        );
                        if afx.comp {
                            ui.add(
                                egui::Slider::new(&mut afx.comp_thresh, -40.0..=0.0)
                                    .text("Threshold (dB)"),
                            );
                            ui.add(egui::Slider::new(&mut afx.comp_ratio, 1.5..=10.0).text("Ratio"));
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Fade shape").small());
                            for (label, v) in [
                                ("linear", crate::edit::FadeCurve::Linear),
                                ("smooth", crate::edit::FadeCurve::Smooth),
                                ("exp", crate::edit::FadeCurve::Exp),
                            ] {
                                ui.selectable_value(&mut afx.fade_curve, v, label);
                            }
                        });
                        ui.add(egui::Slider::new(&mut afx.deess, 0.0..=1.0).text("De-ess"))
                            .on_hover_text("Tame harsh S sounds — render-time, like Fix voice");
                        ui.checkbox(&mut afx.voice_fix, "Fix voice on export")
                            .on_hover_text(
                                "Rumble/hum off, background noise down, clicks patched. \
                                 A render-time FFT pass — the preview plays without it.",
                            );
                        if ui.small_button("reset audio").clicked() {
                            afx = Default::default();
                        }
                    });
                    if afx != before_afx {
                        // One undo step per gesture, like the look sliders.
                        if !ui.ctx().input(|i| i.pointer.any_down())
                            || app.editor.fx_gesture != Some(id)
                        {
                            app.editor.push_undo(&app.project);
                            app.editor.fx_gesture = Some(id);
                        }
                        if let Some(c) = app.project.clip_mut(id) {
                            c.audio = afx;
                        }
                        app.editor.mark_changed();
                    }
                }

                // Crossfade from the previous clip. Only meaningful when
                // there IS a previous clip on the same track.
                let has_prev = app
                    .project
                    .clip_before(crate::edit::TrackKind::Video, start)
                    .is_some();
                if has_prev {
                    let mut xf = app.project.clip(id).map(|c| c.transition_in).unwrap_or(0.0);
                    let before_xf = xf;
                    ui.add(egui::Slider::new(&mut xf, 0.0..=duration.min(3.0)).text("Crossfade in (s)"));
                    if (xf - before_xf).abs() > 1e-6 {
                        if let Some(c) = app.project.clip_mut(id) {
                            c.transition_in = xf;
                        }
                        app.editor.mark_changed();
                    }
                    if xf > 0.0 {
                        let mut kind = app
                            .project
                            .clip(id)
                            .map(|c| c.transition_kind)
                            .unwrap_or_default();
                        let before_kind = kind;
                        egui::ComboBox::from_id_salt("transkind")
                            .selected_text(kind.label())
                            .show_ui(ui, |ui| {
                                for k in crate::edit::TransitionKind::ALL {
                                    ui.selectable_value(&mut kind, k, k.label());
                                }
                            });
                        if kind != before_kind {
                            if let Some(c) = app.project.clip_mut(id) {
                                c.transition_kind = kind;
                            }
                            app.editor.mark_changed();
                        }
                        ui.label(
                            RichText::new(
                                "Previewed live; the render overlaps the clips and the \
                                 edit shortens by the transition.",
                            )
                            .small()
                            .color(egui::Color32::from_gray(140)),
                        );
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("Reset look").clicked() {
                        fx = crate::effects::Effects::default();
                    }
                    if !fx.is_identity() {
                        ui.label(RichText::new("• applied on export").small().color(theme::EMBER));
                    }
                });
                // A grade is a look — carry it between clips, or stamp it
                // on the whole edit as the project's look.
                ui.horizontal(|ui| {
                    if ui.small_button("Copy grade").on_hover_text(
                        "Copy the colour work (trims, levels, balance, HSL, LUT, curves) — not fades or reframe",
                    ).clicked() {
                        app.grade_clipboard = Some(fx);
                    }
                    if let Some(g) = app.grade_clipboard {
                        if ui.small_button("Paste grade").clicked() {
                            fx.copy_grade_from(&g);
                        }
                        if ui.small_button("Paste to ALL clips").on_hover_text(
                            "Apply the copied grade to every video clip — the project look",
                        ).clicked() {
                            app.editor.push_undo(&app.project);
                            for t in &mut app.project.tracks {
                                for c in &mut t.clips {
                                    c.effects.copy_grade_from(&g);
                                }
                            }
                            app.editor.mark_changed();
                        }
                    }
                });
                if fx != before {
                    // One undo step per gesture, not per pixel of slider drag.
                    if !ui.ctx().input(|i| i.pointer.any_down()) || app.editor.fx_gesture != Some(id) {
                        app.editor.push_undo(&app.project);
                        app.editor.fx_gesture = Some(id);
                    }
                    if let Some(c) = app.project.clip_mut(id) {
                        c.effects = fx;
                    }
                    app.editor.mark_changed();
                }
                if !ui.ctx().input(|i| i.pointer.any_down()) {
                    app.editor.fx_gesture = None;
                }
            }
        }
        // ── Mixer ───────────────────────────────────────────────────────
        ui.separator();
        ui.label(RichText::new("Mixer").color(theme::CYAN));
        {
            // Live levels from the mixer, when it runs (Linux + PipeWire).
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            let levels = app.mixer.as_ref().map(|m| m.levels());
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            let levels: Option<crate::audio::Levels> = None;
            if std::env::var("REEL_DEBUG_METER").is_ok() {
                if let Some(lv) = &levels {
                    eprintln!("METER buses={:?} master={:?} lufs={:.1}", lv.buses, lv.master, lv.lufs);
                }
            }
            let meter = |ui: &mut egui::Ui, peak: f32| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(46.0, 8.0), Sense::hover());
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 2.0, theme::VOID_2);
                // dB scale, -48..0 across the bar.
                let db = 20.0 * peak.max(1e-6).log10();
                let frac = ((db + 48.0) / 48.0).clamp(0.0, 1.0);
                if frac > 0.0 {
                    let w = rect.width() * frac;
                    let color = if db > -6.0 {
                        theme::EMBER
                    } else {
                        Color32::from_rgb(80, 220, 140)
                    };
                    p.rect_filled(
                        Rect::from_min_size(rect.min, Vec2::new(w, rect.height())),
                        2.0,
                        color,
                    );
                }
            };
            let track_rows: Vec<(usize, u64, String)> = app
                .project
                .tracks
                .iter()
                .enumerate()
                .map(|(i, t)| (i, t.id, t.name.clone()))
                .collect();
            let mut changed = false;
            for (ti, tid, name) in track_rows {
                ui.horizontal(|ui| {
                    // Two sliders + meter + M/S must fit the panel width —
                    // width-greedy sliders push the rest off the edge.
                    let w = ui.available_width();
                    ui.spacing_mut().slider_width = ((w - 200.0) / 2.0).clamp(36.0, 110.0);
                    ui.label(RichText::new(name).small().color(egui::Color32::from_gray(170)));
                    if let Some(lv) = &levels {
                        meter(ui, lv.buses.get(ti).copied().unwrap_or(0.0));
                    }
                    let Some(t) = app.project.tracks.iter_mut().find(|t| t.id == tid) else {
                        return;
                    };
                    let mut gain = t.gain_db;
                    if ui
                        .add(
                            egui::Slider::new(&mut gain, -30.0..=12.0)
                                .show_value(false)
                                .text(""),
                        )
                        .on_hover_text(format!("{:+.1} dB", t.gain_db))
                        .changed()
                    {
                        t.gain_db = gain;
                        changed = true;
                    }
                    let mut m = t.muted;
                    if ui.selectable_label(m, "M").on_hover_text("Mute").clicked() {
                        m = !m;
                        t.muted = m;
                        changed = true;
                    }
                    let mut so = t.solo;
                    if ui.selectable_label(so, "S").on_hover_text("Solo").clicked() {
                        so = !so;
                        t.solo = so;
                        changed = true;
                    }
                    let mut pan = t.pan;
                    if ui
                        .add(
                            egui::Slider::new(&mut pan, -1.0..=1.0)
                                .show_value(false)
                                .text(""),
                        )
                        .on_hover_text(format!("Pan {:+.2}", t.pan))
                        .changed()
                    {
                        t.pan = pan;
                        changed = true;
                    }
                });
            }
            // The master: stereo peaks and momentary loudness — the number
            // delivery specs ask about, live while you listen.
            if let Some(lv) = &levels {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("out").small().color(egui::Color32::from_gray(170)));
                    meter(ui, lv.master[0]);
                    meter(ui, lv.master[1]);
                    let lufs = if lv.lufs < -70.0 {
                        "–".to_string()
                    } else {
                        format!("{:+.1} LUFS", lv.lufs)
                    };
                    ui.label(RichText::new(lufs).small().monospace());
                });
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
            }
            if changed {
                app.editor.mark_changed();
            }
        }

        // ── Voice recording ─────────────────────────────────────────────
        #[cfg(target_os = "linux")]
        {
            if let Some(at) = app.punch_in_at {
                let secs = app.voice_rec.as_ref().map(|r| r.seconds()).unwrap_or(0.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⏺").color(theme::EMBER));
                    ui.label(
                        RichText::new(format!("recording… {secs:.1}s (from {})", fmt_time(at)))
                            .small()
                            .color(theme::EMBER),
                    );
                    if ui.button("Stop").clicked() {
                        app.finish_record_voice();
                    }
                });
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
            } else if ui
                .button("⏺ Record voice")
                .on_hover_text(
                    "Punch in: the timeline rolls from the playhead and your \
                     microphone records over it — the take lands on A1 where \
                     you started. Press again to stop.",
                )
                .clicked()
            {
                app.toggle_record_voice();
            }
        }

        // ── Tighten ─────────────────────────────────────────────────────
        if ui
            .button("✂ Tighten silence")
            .on_hover_text(
                "Cut the quiet air out of the whole edit and close up — \
                 keeps 0.15s of breathing room around every cut. Undoable.",
            )
            .clicked()
        {
            app.editor.push_undo(&app.project);
            let mut cache: std::collections::HashMap<String, Option<(Vec<f32>, f64)>> =
                Default::default();
            let mut supplier = |src: &str| -> Option<(Vec<f32>, f64)> {
                cache
                    .entry(src.to_string())
                    .or_insert_with(|| {
                        crate::waveform::compute(src)
                            .map(|p| (p.data, crate::waveform::BUCKETS_PER_SEC))
                    })
                    .clone()
            };
            let (cuts, removed) = app.project.tighten(&mut supplier, 0.06, 0.6, 0.15);
            app.status = if cuts == 0 {
                "Nothing to tighten — no silences found.".into()
            } else {
                app.editor.mark_changed();
                format!("Tightened: {cuts} cut(s), {removed:.1}s of silence removed.")
            };
        }

        // ── Frame export ────────────────────────────────────────────────
        if ui
            .button("📷 Export this frame")
            .on_hover_text("The frame under the playhead — effects, overlays and animation included — as a PNG next to the project")
            .clicked()
        {
            app.export_current_frame();
        }

        if ui
            .button("⏺ Snapshot")
            .on_hover_text(
                "Freeze the whole project as a named copy under \
                 <project>.snapshots/ — `reel snapshot --list` shows them, \
                 --restore rolls back",
            )
            .clicked()
        {
            if let Some(path) = app.editor.project_path.clone() {
                // Flush the very latest state first — the debounce would
                // otherwise leave a mid-drag change out of the snapshot.
                app.editor.changed_at = std::time::Instant::now() - std::time::Duration::from_secs(2);
                app.poll_autosave();
                match crate::cli::take_snapshot(&path, None) {
                    Ok(dest) => app.status = format!("Snapshot saved: {}", dest.display()),
                    Err(e) => app.status = format!("Snapshot failed: {e}"),
                }
            } else {
                app.status = "The project has no file yet — add media first.".into();
            }
        }

        // ── Scopes ──────────────────────────────────────────────────────
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Scopes").color(theme::CYAN));
            let label = if app.show_scopes { "Hide" } else { "Show" };
            if ui.small_button(label).clicked() {
                app.show_scopes = !app.show_scopes;
                app.scroll_to_scopes = app.show_scopes;
            }
        });
        if app.show_scopes {
            let drew = scopes(ui, app);
            if app.scroll_to_scopes {
                // Bring the freshly opened scopes into view — they live far
                // down a panel that is taller than most windows. Keep
                // scrolling until a real picture fills them (the first
                // frames draw a placeholder a fraction of the height).
                let r = ui.min_rect();
                ui.scroll_to_rect(Rect::from_min_max(r.left_bottom() - Vec2::new(0.0, 1.0), r.left_bottom()), Some(egui::Align::Max));
                if drew {
                    app.scroll_to_scopes = false;
                }
            }
        }

        // ── Captions ────────────────────────────────────────────────────
        ui.separator();
        ui.label(RichText::new("Captions").color(theme::CYAN));
        if let Some(job) = &app.captions_job {
            let st = job.state();
            ui.label(RichText::new(&st.stage).small());
            if st.fraction > 0.0 {
                ui.add(egui::ProgressBar::new(st.fraction).show_percentage());
            } else {
                ui.spinner();
            }
            if ui.button("Cancel").clicked() {
                job.cancel();
            }
        } else if !app.project.captions.is_empty() {
            ui.label(
                RichText::new(format!("{} captions · burned in on export", app.project.captions.len()))
                    .small()
                    .color(egui::Color32::from_gray(150)),
            );
            ui.add(egui::Slider::new(&mut app.project.caption_size, 12..=40).text("Size"));
            // The cue editor: fix a word, nudge a timing, drop a line —
            // whisper gets it almost right, and "almost" is editable.
            let head = app.editor.playhead;
            egui::ScrollArea::vertical()
                .id_salt("cues")
                .max_height(150.0)
                .show(ui, |ui| {
                    let mut delete: Option<usize> = None;
                    let n = app.project.captions.len();
                    for i in 0..n {
                        let (start, active) = {
                            let cue = &app.project.captions[i];
                            (cue.start, head >= cue.start && head < cue.end)
                        };
                        ui.horizontal(|ui| {
                            if ui
                                .small_button(RichText::new(format!("{start:.1}s")).monospace())
                                .on_hover_text("Jump the playhead here")
                                .clicked()
                            {
                                app.seek_timeline(start);
                            }
                            let cue = &mut app.project.captions[i];
                            let mut text = cue.text.clone();
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut text)
                                    .desired_width(ui.available_width() - 50.0)
                                    .text_color(if active { theme::CYAN } else { theme::STAR }),
                            );
                            if edit.changed() {
                                cue.text = text;
                                app.editor.mark_changed();
                            }
                            if ui.small_button("−").on_hover_text("Delete this cue").clicked() {
                                delete = Some(i);
                            }
                        });
                    }
                    if let Some(i) = delete {
                        app.editor.push_undo(&app.project);
                        app.project.captions.remove(i);
                        app.editor.mark_changed();
                    }
                });
            ui.horizontal(|ui| {
                if ui.button("Redo captions").clicked() {
                    app.start_captions();
                }
                if ui.button("Remove all").clicked() {
                    app.editor.push_undo(&app.project);
                    app.project.captions.clear();
                    app.editor.mark_changed();
                }
            });
        } else {
            let btn = egui::Button::new(RichText::new("Generate captions").color(theme::VOID))
                .fill(theme::CYAN)
                .corner_radius(8.0);
            if ui.add_sized([ui.available_width(), 30.0], btn)
                .on_hover_text(
                    "Speech to captions, entirely on this machine — nothing is uploaded.\n\
                     The engine and model are fetched once, automatically.",
                )
                .clicked()
            {
                app.start_captions();
            }
            egui::ComboBox::from_id_salt("capmodel")
                .selected_text(app.caption_model.label())
                .show_ui(ui, |ui| {
                    for m in crate::captions::Model::ALL {
                        ui.selectable_value(&mut app.caption_model, m, m.label());
                    }
                });
        }

        // ── Music ───────────────────────────────────────────────────────
        ui.separator();
        ui.label(RichText::new("Music").color(theme::CYAN));
        match app.project.music.clone() {
            None => {
                if ui
                    .button("♪ Add music bed")
                    .on_hover_text("A track under the whole edit — ducked under speech automatically")
                    .clicked()
                {
                    app.pick_music();
                }
            }
            Some(mut m) => {
                let name = std::path::Path::new(&m.source)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| m.source.clone());
                ui.label(RichText::new(name).small().color(egui::Color32::from_gray(160)));
                let before = m.clone();
                ui.add(egui::Slider::new(&mut m.gain_db, -40.0..=6.0).text("Level (dB)"));
                ui.checkbox(&mut m.duck, "Duck under speech")
                    .on_hover_text("Pull the music down whenever the edit's own audio speaks");
                ui.add(egui::Slider::new(&mut m.fade, 0.0..=5.0).text("Fade (s)"));
                ui.horizontal(|ui| {
                    if ui.button("Replace…").clicked() {
                        app.pick_music();
                    }
                    if ui.button("Remove").clicked() {
                        app.editor.push_undo(&app.project);
                        app.project.music = None;
                        app.editor.mark_changed();
                    }
                });
                if app.project.music.is_some() && m != before {
                    app.project.music = Some(m);
                    app.editor.mark_changed();
                }
            }
        }

        // ── Titles ──────────────────────────────────────────────────────
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Titles").color(theme::CYAN));
            if ui.button("+ Add").on_hover_text("Text on the picture at the playhead").clicked() {
                app.editor.push_undo(&app.project);
                let head = app.editor.playhead;
                app.project.titles.push(crate::titles::Title {
                    start: head,
                    end: head + 3.0,
                    ..Default::default()
                });
                app.editor.selected_title = Some(app.project.titles.len() - 1);
            }
        });
        let mut remove = None;
        for i in 0..app.project.titles.len() {
            let selected = app.editor.selected_title == Some(i);
            let head = app.editor.playhead;
            let label = {
                let t = &app.project.titles[i];
                let shown = if t.text.chars().count() > 18 {
                    format!("{}…", t.text.chars().take(18).collect::<String>())
                } else {
                    t.text.clone()
                };
                format!("{} {}", if t.covers(head) { "•" } else { "·" }, shown)
            };
            if ui.selectable_label(selected, label).clicked() {
                app.editor.selected_title = if selected { None } else { Some(i) };
            }
            if !selected {
                continue;
            }
            let dur = app.project.titles[i].end - app.project.titles[i].start;
            let mut changed = false;
            ui.group(|ui| {
                let t = &mut app.project.titles[i];
                changed |= ui.text_edit_singleline(&mut t.text).changed();
                changed |= ui
                    .add(egui::Slider::new(&mut t.size, 0.03..=0.30).text("Size"))
                    .changed();
                ui.horizontal(|ui| {
                    changed |= ui.color_edit_button_srgb(&mut t.color).changed();
                    changed |= ui.checkbox(&mut t.bold, "Bold").changed();
                    changed |= ui.checkbox(&mut t.outline, "Outline").changed();
                });
                // Motion: fades + a slide-in edge; and the preset browser —
                // apply one, or save this title's look for reuse.
                ui.horizontal(|ui| {
                    let mut fi = t.fade_in as f32;
                    if ui.add(egui::DragValue::new(&mut fi).speed(0.05).range(0.0..=3.0).prefix("in ").suffix("s")).changed() {
                        t.fade_in = fi as f64;
                        changed = true;
                    }
                    let mut fo = t.fade_out as f32;
                    if ui.add(egui::DragValue::new(&mut fo).speed(0.05).range(0.0..=3.0).prefix("out ").suffix("s")).changed() {
                        t.fade_out = fo as f64;
                        changed = true;
                    }
                    egui::ComboBox::from_id_salt(("slide", i))
                        .selected_text(match t.slide_from {
                            crate::titles::Slide::None => "no slide",
                            crate::titles::Slide::Left => "from left",
                            crate::titles::Slide::Right => "from right",
                            crate::titles::Slide::Top => "from top",
                            crate::titles::Slide::Bottom => "from bottom",
                        })
                        .width(92.0)
                        .show_ui(ui, |ui| {
                            for (label, v) in [
                                ("no slide", crate::titles::Slide::None),
                                ("from left", crate::titles::Slide::Left),
                                ("from right", crate::titles::Slide::Right),
                                ("from top", crate::titles::Slide::Top),
                                ("from bottom", crate::titles::Slide::Bottom),
                            ] {
                                if ui.selectable_value(&mut t.slide_from, v, label).changed() {
                                    changed = true;
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("preset", i))
                        .selected_text("Preset…")
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for name in crate::titles::list_presets() {
                                if ui.button(&name).clicked() {
                                    if let Ok(preset) = crate::titles::load_preset(&name) {
                                        let keep = (t.text.clone(), t.start, t.end);
                                        *t = preset;
                                        (t.text, t.start, t.end) = keep;
                                        changed = true;
                                    }
                                    ui.close_menu();
                                }
                            }
                        });
                    if ui.small_button("save preset").on_hover_text(
                        "Save this title's style + motion as a reusable preset (a JSON file — share it)",
                    ).clicked() {
                        let dir = crate::titles::preset_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        let slug: String = t.text.chars().take(20)
                            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
                            .collect();
                        let path = dir.join(format!("{}.json", if slug.is_empty() { "preset".into() } else { slug }));
                        let saved = serde_json::to_string_pretty(&t)
                            .ok()
                            .and_then(|json| std::fs::write(&path, json).ok())
                            .is_some();
                        app.status = if saved {
                            format!("Preset saved: {}", path.display())
                        } else {
                            "Could not save the preset.".into()
                        };
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Start here").on_hover_text("Begin at the playhead").clicked() {
                        t.start = head;
                        t.end = head + dur.max(0.5);
                        changed = true;
                    }
                    if ui.button("End here").clicked() {
                        t.end = head.max(t.start + 0.2);
                        changed = true;
                    }
                });
                ui.label(
                    RichText::new(format!("{:.2}s → {:.2}s · drag it on the picture", t.start, t.end))
                        .small()
                        .color(egui::Color32::from_gray(140)),
                );
                if ui.button("🗑 Remove").clicked() {
                    remove = Some(i);
                }
            });
            if changed {
                app.editor.mark_changed();
            }
        }
        if let Some(i) = remove {
            app.editor.push_undo(&app.project);
            app.project.titles.remove(i);
            app.editor.selected_title = None;
        }

        ui.separator();
        ui.label(RichText::new(
            "J K L shuttle · S or Ctrl+K split · Q W ripple-trim to playhead\n\
             Del delete · Shift+Del ripple delete · right-click to close gaps\n\
             Ctrl+C/V/D copy, paste, duplicate · Ctrl+M marker · Ctrl+Left/Right jump\n\
             Ctrl+drag an edge: roll · Alt+drag: slip · Ctrl+Alt+drag: slide",
        ).small().color(egui::Color32::from_gray(120)));
    }
}

/// The media viewport — aspect-fit the current frame / image / cover art,
/// a ♪ card for pure audio, or the drop hint.
fn viewport(ui: &mut egui::Ui, app: &mut ReelApp) {
    let avail = ui.available_size();
    let (rect, response) = ui.allocate_exact_size(avail, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::VOID);

    // Tell the player how big the picture actually is on screen (physical
    // pixels) so it renders exactly those pixels — see Player::set_display_size.
    let ppp = ui.ctx().pixels_per_point();
    if let Some(player) = app.player.as_mut() {
        player.set_display_size(rect.width() * ppp, rect.height() * ppp);
    }

    // Native size of whatever is on the texture (frame, art, visualizer, image).
    let dims = app.tex_dims().map(|(w, h)| (w as f32, h as f32));

    if let (Some(view), Some((vw, vh))) = (app.tex_view(), dims) {
        if vw > 0.0 && vh > 0.0 {
            let scale = (rect.width() / vw).min(rect.height() / vh);
            let size = Vec2::new(vw * scale, vh * scale);
            let img_rect = Rect::from_center_size(rect.center(), size);
            if app.image.is_some() {
                // Stills can be transparent — show it honestly, viewer-style.
                checkerboard(&painter, img_rect);
            }
            // Reel's own pipeline draws the picture (see video_pass.rs):
            // opaque alpha without a CPU pass, and the seam where colour and
            // compositing will live.
            // In the editor, preview the clip under the playhead exactly as
            // it will render: its colour adjustments, and its fade at this
            // moment. (Same formula as the export — see effects.rs.)
            let (effects, fade) = app.preview_effects();
            painter.add(egui_wgpu::Callback::new_paint_callback(
                img_rect,
                crate::video_pass::VideoDraw {
                    view,
                    tint: [1.0, 1.0, 1.0, fade],
                    use_src_alpha: app.image.is_some(),
                    effects,
                    // The stacked lattice (clip grade + adjustment layers)
                    // when in the editor; the plain per-fx one elsewhere.
                    lut: app.preview_lut().or_else(|| app.lut_view(effects)),
                    plugin: app.plugin_for(effects),
                },
            ));
            // The incoming half of a crossfade, blended over the outgoing
            // picture at the ramp's opacity — drawn through the same video
            // pass so its colour effects apply. This is what makes a fade
            // preview as a fade instead of a hard cut.
            if let Some((clip_id, progress, kind)) = app.transition_preview {
                if let Some(ov) = app.overlay_previews.get(&clip_id) {
                    if let Some(tex) = &ov.tex {
                        let fx = app
                            .project
                            .clip(clip_id)
                            .map(|c| c.animated((app.editor.playhead - c.start).max(0.0)).0);
                        // The SAME geometry the frame server renders: mods
                        // give the incoming rect/uv and both opacities.
                        let (out_mul, in_mul, r, _) =
                            crate::engine::render::transition_mods(kind, progress);
                        // Outgoing side dimming (dip-to-black) — a black
                        // veil over the base picture.
                        if out_mul < 0.999 {
                            painter.rect_filled(
                                img_rect,
                                0.0,
                                Color32::from_black_alpha(((1.0 - out_mul) * 255.0) as u8),
                            );
                        }
                        if in_mul > 0.001 {
                            let draw_rect = Rect::from_min_max(
                                egui::pos2(
                                    img_rect.left() + r[0] * img_rect.width(),
                                    img_rect.top() + r[1] * img_rect.height(),
                                ),
                                egui::pos2(
                                    img_rect.left() + r[2] * img_rect.width(),
                                    img_rect.top() + r[3] * img_rect.height(),
                                ),
                            );
                            // Wipes crop; slides move a full frame. Either
                            // way, clip the callback to the picture so a
                            // slide never draws over the panels.
                            let clipped = painter.with_clip_rect(draw_rect.intersect(img_rect));
                            clipped.add(egui_wgpu::Callback::new_paint_callback(
                                match kind {
                                    crate::edit::TransitionKind::SlideLeft
                                    | crate::edit::TransitionKind::SlideRight => draw_rect,
                                    _ => img_rect,
                                },
                                crate::video_pass::VideoDraw {
                                    view: tex
                                        .texture
                                        .create_view(&wgpu::TextureViewDescriptor::default()),
                                    tint: [1.0, 1.0, 1.0, in_mul],
                                    use_src_alpha: false,
                                    effects: fx,
                                    lut: app.lut_view(fx),
                                    plugin: app.plugin_for(fx),
                                },
                            ));
                        }
                    }
                }
            }
            // Captions sit on top of the picture, so they must be painted
            // AFTER the video callback — and inside this branch, which
            // returns early for every real frame.
            draw_pip(app, ui.ctx(), &painter, img_rect);
            draw_overlays(app, &painter, img_rect);
            drag_title(app, &response, img_rect);
            drag_pip(app, &response, img_rect);
            proxy_chip(app, &painter, rect);
            return;
        }
    }

    if let Some(p) = app.player.as_ref() {
        // Audio with no cover art: a simple sound card.
        let name = std::path::Path::new(&p.path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| p.path.clone());
        painter.text(
            rect.center() - Vec2::new(0.0, 24.0),
            egui::Align2::CENTER_CENTER,
            "♪",
            egui::FontId::proportional(72.0),
            theme::CYAN,
        );
        painter.text(
            rect.center() + Vec2::new(0.0, 40.0),
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::proportional(18.0),
            theme::STAR,
        );
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "drop a video, song or image here",
            egui::FontId::proportional(20.0),
            Color32::from_gray(90),
        );
    }

    proxy_chip(app, &painter, rect);
}

/// Honesty chip: when the editor preview is playing a 720p proxy (or one is
/// still baking), say so in the corner — the export always uses the original.
fn proxy_chip(app: &ReelApp, painter: &egui::Painter, rect: Rect) {
    if app.mode != Mode::Editor {
        return;
    }
    let label = if app
        .player
        .as_ref()
        .map(|p| app.proxies.is_proxy(&p.path))
        .unwrap_or(false)
    {
        "PROXY"
    } else if app.proxies.is_busy() {
        "PROXY…"
    } else {
        return;
    };
    let pos = rect.right_top() + Vec2::new(-10.0, 10.0);
    let galley = painter.layout_no_wrap(
        label.into(),
        egui::FontId::monospace(11.0),
        Color32::from_gray(210),
    );
    let chip = Rect::from_min_size(
        pos - Vec2::new(galley.size().x + 12.0, 0.0),
        galley.size() + Vec2::new(12.0, 6.0),
    );
    painter.rect_filled(chip, 4.0, Color32::from_black_alpha(150));
    painter.galley(chip.min + Vec2::new(6.0, 3.0), galley, Color32::from_gray(210));
}

/// The classic transparency checkerboard, drawn under a still image. Base
/// fill plus alternate cells only, so the rect count stays modest.
/// Preview the caption at the playhead where it will burn in: bottom-centre,
/// white with a dark outline. Approximate — the render itself is drawn by
/// ffmpeg from the same SRT — but the wording, timing and placement match, so
/// what you read here is what ships.
fn draw_overlays(app: &ReelApp, painter: &egui::Painter, pic: Rect) {
    if app.mode != Mode::Editor {
        return;
    }
    draw_titles(app, painter, pic);
    let Some(cue) = app.project.caption_at(app.editor.playhead) else { return };
    // Every number comes from captions::metrics, which is also what the
    // render is built from — one formula, two drawings of it.
    let m = crate::captions::metrics(app.project.caption_size);
    let font = egui::FontId::proportional((m.font * pic.height()).max(7.0));
    let pos = egui::pos2(pic.center().x, pic.bottom() - m.margin_bottom * pic.height());
    let outline = (m.outline * pic.height()).max(1.0);
    for (dx, dy) in [
        (-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0),
        (-0.7, -0.7), (0.7, 0.7), (-0.7, 0.7), (0.7, -0.7),
    ] {
        painter.text(
            pos + Vec2::new(dx * outline, dy * outline),
            egui::Align2::CENTER_BOTTOM,
            &cue.text,
            font.clone(),
            Color32::BLACK,
        );
    }
    // The render is bold; egui's default proportional face has no bold cut,
    // so thicken it by overdrawing. Keeps the preview honest about weight.
    for dx in [-0.35, 0.0, 0.35] {
        painter.text(
            pos + Vec2::new(dx * outline, 0.0),
            egui::Align2::CENTER_BOTTOM,
            &cue.text,
            font.clone(),
            Color32::WHITE,
        );
    }
}

/// The overlay under the playhead, previewed in place.
///
/// The picture comes from the clip's thumbnail sheet rather than a second
/// live decoder: the POSITION and SIZE are exact — they read the same Pip
/// fractions the renderer uses — while the frame itself is the nearest
/// thumbnail. That is the honest trade: composition is what you place a PiP
/// by, and it is exactly right; the inset just doesn't play.
fn draw_pip(app: &mut ReelApp, ctx: &egui::Context, painter: &egui::Painter, pic: Rect) {
    if app.mode != Mode::Editor {
        return;
    }
    let t = app.editor.playhead;
    let shots: Vec<(u64, crate::edit::Pip, String, f64, bool)> = app
        .project
        .tracks
        .iter()
        .filter(|tr| tr.kind == crate::edit::TrackKind::Overlay && !tr.muted)
        .flat_map(|tr| tr.clips.iter())
        .filter(|c| t >= c.start && t < c.end())
        .map(|c| {
            let (_, pip, _) = c.animated(t - c.start);
            (
                c.id,
                pip,
                c.source.clone(),
                c.in_point + (t - c.start),
                app.editor.selected == Some(c.id),
            )
        })
        .collect();

    for (clip_id, pip, source, src_t, selected) in shots {
        let w = pip.scale.clamp(0.02, 1.0) * pic.width();
        // Height follows the source's own aspect, as the render does.
        let aspect = app
            .project
            .width
            .max(1) as f32
            / app.project.height.max(1) as f32;
        let h = w / aspect;
        let centre = pic.min + Vec2::new(pip.x * pic.width(), pip.y * pic.height());
        let box_rect = Rect::from_center_size(centre, Vec2::new(w, h));

        let mut drew = false;
        // Live frames from the preview pool play in the inset, drawn through
        // Reel's own video pass so the clip's colour effects — chroma key
        // included — preview exactly as they render.
        if let Some(ov) = app.overlay_previews.get(&clip_id) {
            if let Some(tex) = &ov.tex {
                let fx = app.project.clip(clip_id).map(|c| {
                    let local = (app.editor.playhead - c.start).max(0.0);
                    c.animated(local).0
                });
                painter.add(egui_wgpu::Callback::new_paint_callback(
                    box_rect,
                    crate::video_pass::VideoDraw {
                        view: tex.texture.create_view(&wgpu::TextureViewDescriptor::default()),
                        tint: [1.0, 1.0, 1.0, 1.0],
                        use_src_alpha: false,
                        effects: fx,
                        lut: app.lut_view(fx),
                        plugin: app.plugin_for(fx),
                    },
                ));
                drew = true;
            }
        }
        if !drew {
            if let Some(sheet) = app.thumbs.get(ctx, &source, src_t.max(0.1) + 1.0) {
                if let Some(uv) = sheet.uv_at(src_t) {
                    painter.add(egui::Shape::image(sheet.tex.id(), box_rect, uv, Color32::WHITE));
                    drew = true;
                }
            }
        }
        if !drew {
            painter.rect_filled(box_rect, 2.0, Color32::from_black_alpha(180));
        }
        painter.rect_stroke(
            box_rect,
            2.0,
            Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { theme::STAR } else { theme::CYAN }),
            egui::StrokeKind::Outside,
        );
    }
}

/// Drag the selected overlay around the frame. Same contract as titles:
/// position is a fraction, so what you place is what renders at any size.
fn drag_pip(app: &mut ReelApp, response: &egui::Response, pic: Rect) {
    if app.mode != Mode::Editor || pic.width() <= 0.0 || !response.dragged() {
        return;
    }
    let Some(id) = app.editor.selected else { return };
    let t = app.editor.playhead;
    let is_overlay_now = app
        .project
        .tracks
        .iter()
        .any(|tr| {
            tr.kind == crate::edit::TrackKind::Overlay
                && tr.clips.iter().any(|c| c.id == id && t >= c.start && t < c.end())
        });
    if !is_overlay_now {
        return;
    }
    if let (Some(p), Some(c)) = (response.interact_pointer_pos(), app.project.clip_mut(id)) {
        c.pip.x = ((p.x - pic.min.x) / pic.width()).clamp(0.02, 0.98);
        c.pip.y = ((p.y - pic.min.y) / pic.height()).clamp(0.02, 0.98);
        app.editor.mark_changed();
    }
}

/// Titles at the playhead, drawn from the very fractions the renderer uses
/// (see titles.rs) so placing one on the preview places it in the export.
fn draw_titles(app: &ReelApp, painter: &egui::Painter, pic: Rect) {
    let t = app.editor.playhead;
    for (i, title) in app.project.titles.iter().enumerate() {
        if !title.covers(t) {
            continue;
        }
        // Motion previews exactly as it burns: same formula as the ASS
        // \fad/\move tags (titles::Title::animated_at).
        let ([ax, ay], alpha) = title.animated_at(t);
        let fade = |c: Color32| -> Color32 {
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (alpha * 255.0) as u8)
        };
        let pos = pic.min + Vec2::new(ax * pic.width(), ay * pic.height());
        let font = egui::FontId::proportional((title.size * pic.height()).max(7.0));
        let colour = fade(Color32::from_rgb(title.color[0], title.color[1], title.color[2]));
        if title.outline {
            let o = (crate::titles::OUTLINE_FRAC * pic.height()).max(1.0);
            for (dx, dy) in [
                (-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0),
                (-0.7, -0.7), (0.7, 0.7), (-0.7, 0.7), (0.7, -0.7),
            ] {
                painter.text(
                    pos + Vec2::new(dx * o, dy * o),
                    egui::Align2::CENTER_CENTER,
                    &title.text,
                    font.clone(),
                    fade(Color32::BLACK),
                );
            }
        }
        let bold_pass: &[f32] = if title.bold { &[-0.35, 0.0, 0.35] } else { &[0.0] };
        let weight = (title.size * pic.height() * 0.04).max(0.5);
        for dx in bold_pass {
            painter.text(
                pos + Vec2::new(dx * weight, 0.0),
                egui::Align2::CENTER_CENTER,
                &title.text,
                font.clone(),
                colour,
            );
        }
        // The selected title shows its box, so you can see what you're moving.
        if app.editor.selected_title == Some(i) {
            let galley = painter.layout_no_wrap(title.text.clone(), font, colour);
            let box_rect = Rect::from_center_size(pos, galley.size() + Vec2::splat(8.0));
            painter.rect_stroke(
                box_rect,
                4.0,
                Stroke::new(1.0, theme::CYAN),
                egui::StrokeKind::Outside,
            );
        }
    }
}

/// Drag the selected title around the picture. Position is stored as
/// fractions, so a title placed here lands in the same spot at any export
/// resolution — the thing that makes this safe to do by eye.
fn drag_title(app: &mut ReelApp, response: &egui::Response, pic: Rect) {
    if app.mode != Mode::Editor || pic.width() <= 0.0 {
        return;
    }
    let Some(i) = app.editor.selected_title else { return };
    if i >= app.project.titles.len() || !app.project.titles[i].covers(app.editor.playhead) {
        return;
    }
    if response.dragged() {
        if let Some(p) = response.interact_pointer_pos() {
            let t = &mut app.project.titles[i];
            t.x = ((p.x - pic.min.x) / pic.width()).clamp(0.02, 0.98);
            t.y = ((p.y - pic.min.y) / pic.height()).clamp(0.02, 0.98);
        }
    }
    if response.drag_stopped() {
        app.editor.mark_changed();
    }
}

/// The tone-curve editor: five points at fixed inputs, dragged vertically.
/// Master and per-channel; the drawn curve is `curve_eval`, the same
/// function the lattice bakes — so the widget IS the grade.
fn tone_curve_editor(ui: &mut egui::Ui, app: &mut ReelApp, id: u64) {
    use crate::effects::{curve_eval, Curves};
    let curves = app
        .project
        .clip(id)
        .and_then(|c| c.effects.curves)
        .unwrap_or_default();
    let ch = app.curve_channel;
    let pts = match ch {
        0 => curves.master,
        1 => curves.r,
        2 => curves.g,
        _ => curves.b,
    };

    let width = ui.available_width().max(60.0);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, 110.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, theme::VOID_2);
    let to_x = |x: f32| rect.left() + x * rect.width();
    let to_y = |y: f32| rect.bottom() - y * rect.height();
    // The identity diagonal, for reference.
    painter.line_segment(
        [egui::pos2(to_x(0.0), to_y(0.0)), egui::pos2(to_x(1.0), to_y(1.0))],
        Stroke::new(0.5, egui::Color32::from_gray(60)),
    );
    let colour = [theme::STAR, Color32::from_rgb(255, 90, 90), Color32::from_rgb(90, 230, 120), Color32::from_rgb(110, 150, 255)][ch];
    let n = 96;
    let line: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let x = i as f32 / n as f32;
            egui::pos2(to_x(x), to_y(curve_eval(&pts, x)))
        })
        .collect();
    painter.add(egui::Shape::line(line, Stroke::new(1.5, colour)));
    for (i, y) in pts.iter().enumerate() {
        let p = egui::pos2(to_x(i as f32 / 4.0), to_y(*y));
        painter.circle_filled(p, 4.0, colour);
        painter.circle_stroke(p, 4.0, Stroke::new(1.0, theme::VOID));
    }

    if response.dragged() || response.clicked() {
        if let Some(m) = response.interact_pointer_pos() {
            let xi = (((m.x - rect.left()) / rect.width()) * 4.0).round().clamp(0.0, 4.0) as usize;
            let y = (1.0 - (m.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            if response.drag_started() || response.clicked() {
                app.editor.push_undo(&app.project);
            }
            if let Some(c) = app.project.clip_mut(id) {
                let mut cv = c.effects.curves.unwrap_or_default();
                let arr = match ch {
                    0 => &mut cv.master,
                    1 => &mut cv.r,
                    2 => &mut cv.g,
                    _ => &mut cv.b,
                };
                arr[xi] = y;
                c.effects.curves = if cv.is_identity() { None } else { Some(cv) };
            }
            app.editor.mark_changed();
        }
    }
    let _ = Curves::default();
}

/// The keyframe curve editor for one clip + the panel's selected parameter.
fn curve_editor(ui: &mut egui::Ui, app: &mut ReelApp, id: u64, duration: f64) {
    use crate::edit::{Interp, Keyframe, Param};
    let param = app.editor.key_param;
    let (lo, hi) = param.range();
    let track: Vec<Keyframe> = app
        .project
        .clip(id)
        .and_then(|c| c.key_track(param).map(|t| t.to_vec()))
        .unwrap_or_default();
    if track.is_empty() {
        return; // nothing to edit — the + Key button is the way in
    }

    let width = ui.available_width().max(60.0);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, 96.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, theme::VOID_2);

    let to_x = |t: f64| rect.left() + (t / duration.max(1e-9)) as f32 * rect.width();
    let to_y = |v: f32| {
        rect.bottom() - ((v - lo) / (hi - lo)).clamp(0.0, 1.0) * rect.height()
    };
    let from_pos = |p: egui::Pos2| {
        let t = ((p.x - rect.left()) / rect.width()) as f64 * duration;
        let v = lo + (1.0 - (p.y - rect.top()) / rect.height()).clamp(0.0, 1.0) * (hi - lo);
        (t.clamp(0.0, duration), v)
    };

    // Grid: quarters, plus the parameter's neutral line where it has one.
    for q in 1..4 {
        let y = rect.top() + rect.height() * q as f32 / 4.0;
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(0.5, egui::Color32::from_gray(45)),
        );
    }
    // The curve itself, sampled densely.
    let n = (rect.width() as usize).clamp(16, 400);
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let t = duration * i as f64 / n as f64;
            let v = crate::edit::eval_keys(&track, t).unwrap_or(lo);
            egui::pos2(to_x(t), to_y(v))
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::CYAN)));

    // The playhead, in clip-local time.
    let head = app.editor.playhead - app.project.clip(id).map(|c| c.start).unwrap_or(0.0);
    if head >= 0.0 && head <= duration {
        let x = to_x(head);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0, theme::EMBER),
        );
    }

    // Diamonds, hit-tested against the pointer.
    let pointer = response.hover_pos().or_else(|| response.interact_pointer_pos());
    let mut hover: Option<usize> = None;
    for (i, k) in track.iter().enumerate() {
        let p = egui::pos2(to_x(k.t), to_y(k.value));
        if let Some(m) = pointer {
            if (m - p).length() < 9.0 && hover.is_none() {
                hover = Some(i);
            }
        }
        let big = hover == Some(i) || app.editor.curve_drag == Some(i);
        let r = if big { 5.5 } else { 4.0 };
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(p.x, p.y - r),
                egui::pos2(p.x + r, p.y),
                egui::pos2(p.x, p.y + r),
                egui::pos2(p.x - r, p.y),
            ],
            if big { theme::STAR } else { theme::CYAN },
            Stroke::new(1.0, theme::VOID),
        ));
    }

    // Bezier handles: for a Bezier segment, the outgoing handle of key i
    // and the incoming handle of the SAME segment (drawn near key i+1).
    // Both live in segment-normalised space and are dragged in it.
    let mut handle_hover: Option<(usize, bool)> = None;
    for (i, k) in track.iter().enumerate() {
        let Interp::Bezier { x1, y1, x2, y2 } = k.interp else { continue };
        let Some(next) = track.get(i + 1) else { continue };
        let (pa, pb) = (
            egui::pos2(to_x(k.t), to_y(k.value)),
            egui::pos2(to_x(next.t), to_y(next.value)),
        );
        let h1 = egui::pos2(pa.x + (pb.x - pa.x) * x1, pa.y + (pb.y - pa.y) * y1);
        let h2 = egui::pos2(pa.x + (pb.x - pa.x) * x2, pa.y + (pb.y - pa.y) * y2);
        painter.line_segment([pa, h1], Stroke::new(1.0, Color32::from_gray(110)));
        painter.line_segment([pb, h2], Stroke::new(1.0, Color32::from_gray(110)));
        for (h, incoming) in [(h1, false), (h2, true)] {
            if let Some(m) = pointer {
                if (m - h).length() < 8.0 && handle_hover.is_none() && hover.is_none() {
                    handle_hover = Some((i, incoming));
                }
            }
            let active = handle_hover == Some((i, incoming))
                || app.editor.curve_handle_drag == Some((i, incoming));
            painter.circle_filled(h, if active { 4.5 } else { 3.0 },
                if active { theme::STAR } else { Color32::from_gray(160) });
        }
    }

    // Interactions. One undo step per gesture.
    if response.drag_started() {
        if let Some(hh) = handle_hover {
            app.editor.push_undo(&app.project);
            app.editor.curve_handle_drag = Some(hh);
        } else if let Some(i) = hover {
            app.editor.push_undo(&app.project);
            app.editor.curve_drag = Some(i);
        }
    }
    if response.dragged() {
        if let (Some((i, incoming)), Some(m)) =
            (app.editor.curve_handle_drag, response.interact_pointer_pos())
        {
            // Back-map the pointer into segment-normalised space.
            if let (Some(a), Some(b)) = (track.get(i), track.get(i + 1)) {
                let (pa, pb) = (
                    egui::pos2(to_x(a.t), to_y(a.value)),
                    egui::pos2(to_x(b.t), to_y(b.value)),
                );
                let nx = ((m.x - pa.x) / (pb.x - pa.x).max(1.0)).clamp(0.0, 1.0);
                let ny = if (pb.y - pa.y).abs() > 0.5 {
                    (m.y - pa.y) / (pb.y - pa.y)
                } else {
                    // A flat segment: map vertical drag against the panel
                    // height so overshoot handles are still reachable.
                    (pa.y - m.y) / rect.height() + if incoming { 1.0 } else { 0.0 }
                };
                let ny = ny.clamp(-1.0, 2.0);
                if let Some(c) = app.project.clip_mut(id) {
                    if let Some((_, keys)) = c.keys.iter_mut().find(|(q, _)| *q == param) {
                        if let Some(k) = keys.get_mut(i) {
                            if let Interp::Bezier { x1, y1, x2, y2 } = &mut k.interp {
                                if incoming {
                                    *x2 = nx;
                                    *y2 = ny;
                                } else {
                                    *x1 = nx;
                                    *y1 = ny;
                                }
                            }
                        }
                    }
                }
                app.editor.mark_changed();
            }
        }
    }
    if response.dragged() {
        if let (Some(i), Some(m)) = (app.editor.curve_drag, response.interact_pointer_pos()) {
            let (t, v) = from_pos(m);
            if let Some(c) = app.project.clip_mut(id) {
                if let Some((_, keys)) =
                    c.keys.iter_mut().find(|(q, _)| *q == param)
                {
                    if let Some(k) = keys.get_mut(i) {
                        k.t = t;
                        k.value = v;
                    }
                    // Keep time order without losing which key we hold.
                    let held = keys.get(i).cloned();
                    keys.sort_by(|a, b| a.t.total_cmp(&b.t));
                    if let Some(h) = held {
                        app.editor.curve_drag =
                            keys.iter().position(|k| (k.t - h.t).abs() < 1e-9);
                    }
                }
            }
            app.editor.mark_changed();
        }
    }
    if response.drag_stopped() {
        app.editor.curve_drag = None;
        app.editor.curve_handle_drag = None;
    }
    // Middle-click a key: cycle how it leaves — linear, ease, hold, bezier
    // (which grows draggable handles).
    if response.clicked_by(egui::PointerButton::Middle) {
        if let Some(i) = hover {
            let t = track[i].t;
            app.editor.push_undo(&app.project);
            if let Some(c) = app.project.clip_mut(id) {
                if let Some((_, keys)) = c.keys.iter_mut().find(|(q, _)| *q == param) {
                    if let Some(k) = keys.iter_mut().find(|k| (k.t - t).abs() < 1e-9) {
                        k.interp = match k.interp {
                            Interp::Linear => Interp::Ease,
                            Interp::Ease => Interp::Hold,
                            Interp::Hold => Interp::bezier_default(),
                            Interp::Bezier { .. } => Interp::Linear,
                        };
                    }
                }
            }
            app.editor.mark_changed();
        }
    }
    if response.double_clicked() {
        if let Some(m) = response.interact_pointer_pos() {
            let (t, v) = from_pos(m);
            app.editor.push_undo(&app.project);
            if let Some(c) = app.project.clip_mut(id) {
                c.set_key(param, t, v, Interp::Linear);
            }
            app.editor.mark_changed();
        }
    }
    if response.secondary_clicked() {
        if let Some(i) = hover {
            let t = track[i].t;
            app.editor.push_undo(&app.project);
            if let Some(c) = app.project.clip_mut(id) {
                c.clear_key(param, t);
            }
            app.editor.mark_changed();
        }
    }
    ui.label(
        RichText::new("drag a key · double-click adds · right-click removes · middle-click cycles linear/ease/hold/bezier")
            .small()
            .color(egui::Color32::from_gray(120)),
    );
    let _ = Param::ALL; // (silence an unused-import trap if params shrink)
}

/// Video scopes, read from the CURRENT preview frame: an RGB histogram and
/// a luma waveform. CPU on a downsampled grid — a preview frame is already
/// CPU-visible on the mpv software path, so this costs a fraction of a
/// millisecond and updates live during playback.
fn scopes(ui: &mut egui::Ui, app: &ReelApp) -> bool {
    let Some(frame) = app.player.as_ref().and_then(|p| p.current.as_ref()) else {
        ui.label(RichText::new("No picture yet.").small().color(egui::Color32::from_gray(120)));
        return false;
    };
    let (w, h) = (frame.width as usize, frame.height as usize);
    if frame.data.len() < w * h * 4 || w == 0 || h == 0 {
        return false;
    }
    // Sample a ~120×68 grid: plenty for scopes, nothing for the CPU.
    let (gx, gy) = (120usize.min(w), 68usize.min(h));
    let mut hist = [[0u32; 64]; 3];
    const COLS: usize = 96;
    let mut wave_min = [255u8; COLS];
    let mut wave_max = [0u8; COLS];
    let mut wave_sum = [0u32; COLS];
    let mut wave_n = [0u32; COLS];
    for iy in 0..gy {
        let y = iy * h / gy;
        for ix in 0..gx {
            let x = ix * w / gx;
            let i = (y * w + x) * 4;
            let (r, g, b) = (frame.data[i], frame.data[i + 1], frame.data[i + 2]);
            hist[0][(r >> 2) as usize] += 1;
            hist[1][(g >> 2) as usize] += 1;
            hist[2][(b >> 2) as usize] += 1;
            let luma =
                (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) as u8;
            let col = ix * COLS / gx;
            wave_min[col] = wave_min[col].min(luma);
            wave_max[col] = wave_max[col].max(luma);
            wave_sum[col] += luma as u32;
            wave_n[col] += 1;
        }
    }

    // Histogram strip.
    let width = ui.available_width().max(60.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 56.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::VOID_2);
    let peak = hist.iter().flatten().copied().max().unwrap_or(1).max(1) as f32;
    let bar_w = rect.width() / 64.0;
    let colors = [
        Color32::from_rgba_unmultiplied(255, 70, 70, 140),
        Color32::from_rgba_unmultiplied(80, 255, 120, 140),
        Color32::from_rgba_unmultiplied(90, 140, 255, 140),
    ];
    for ch in 0..3 {
        for (bin, count) in hist[ch].iter().enumerate() {
            if *count == 0 {
                continue;
            }
            let hgt = (*count as f32 / peak).sqrt() * (rect.height() - 4.0);
            let x = rect.left() + bin as f32 * bar_w;
            painter.rect_filled(
                Rect::from_min_max(
                    egui::pos2(x, rect.bottom() - 2.0 - hgt),
                    egui::pos2(x + bar_w.max(1.0), rect.bottom() - 2.0),
                ),
                0.0,
                colors[ch],
            );
        }
    }

    // Luma waveform: per column, the min→max envelope plus the mean.
    let (wrect, _) = ui.allocate_exact_size(Vec2::new(width, 56.0), Sense::hover());
    let wp = ui.painter_at(wrect);
    wp.rect_filled(wrect, 4.0, theme::VOID_2);
    let col_w = wrect.width() / COLS as f32;
    let to_y = |v: f32| wrect.bottom() - 2.0 - (v / 255.0) * (wrect.height() - 4.0);
    for c in 0..COLS {
        if wave_n[c] == 0 {
            continue;
        }
        let x = wrect.left() + c as f32 * col_w + col_w * 0.5;
        wp.line_segment(
            [
                egui::pos2(x, to_y(wave_min[c] as f32)),
                egui::pos2(x, to_y(wave_max[c] as f32)),
            ],
            Stroke::new(col_w.max(1.0), Color32::from_rgba_unmultiplied(120, 220, 235, 60)),
        );
        let mean = wave_sum[c] as f32 / wave_n[c] as f32;
        wp.line_segment(
            [egui::pos2(x - col_w * 0.5, to_y(mean)), egui::pos2(x + col_w * 0.5, to_y(mean))],
            Stroke::new(1.2, theme::CYAN),
        );
    }
    // RGB parade: three side-by-side waveforms, one per channel — where a
    // colour cast shows as one channel's envelope riding high.
    let (prect, _) = ui.allocate_exact_size(Vec2::new(width, 56.0), Sense::hover());
    let pp = ui.painter_at(prect);
    pp.rect_filled(prect, 4.0, theme::VOID_2);
    const PCOLS: usize = 32;
    let mut pmin = [[255u8; PCOLS]; 3];
    let mut pmax = [[0u8; PCOLS]; 3];
    for iy in 0..gy {
        let y = iy * h / gy;
        for ix in 0..gx {
            let x = ix * w / gx;
            let i = (y * w + x) * 4;
            let col = ix * PCOLS / gx;
            for ch in 0..3 {
                let v = frame.data[i + ch];
                pmin[ch][col] = pmin[ch][col].min(v);
                pmax[ch][col] = pmax[ch][col].max(v);
            }
        }
    }
    let third = prect.width() / 3.0;
    let pto_y = |v: f32| prect.bottom() - 2.0 - (v / 255.0) * (prect.height() - 4.0);
    for ch in 0..3 {
        let x0 = prect.left() + ch as f32 * third + 2.0;
        let cw = (third - 4.0) / PCOLS as f32;
        for c in 0..PCOLS {
            let x = x0 + c as f32 * cw + cw * 0.5;
            pp.line_segment(
                [egui::pos2(x, pto_y(pmin[ch][c] as f32)), egui::pos2(x, pto_y(pmax[ch][c] as f32))],
                Stroke::new(cw.max(1.0), colors[ch]),
            );
        }
    }

    // Vectorscope: chroma (Cb, Cr) scatter — hue is the angle, saturation
    // the radius. Skin tones gather on the classic ~123° line.
    let vs = 72.0f32;
    let (vrect, _) = ui.allocate_exact_size(Vec2::new(width, vs), Sense::hover());
    let vp = ui.painter_at(vrect);
    vp.rect_filled(vrect, 4.0, theme::VOID_2);
    let centre = vrect.center();
    let radius = (vs * 0.5 - 4.0).max(10.0);
    vp.circle_stroke(centre, radius, Stroke::new(1.0, Color32::from_gray(60)));
    vp.circle_stroke(centre, radius * 0.5, Stroke::new(0.5, Color32::from_gray(50)));
    // Skin-tone line (I axis, ~123° in vectorscope convention).
    let skin = egui::Vec2::angled(-123.0f32.to_radians()) * radius;
    vp.line_segment([centre, centre + skin], Stroke::new(0.5, Color32::from_gray(70)));
    for iy in 0..gy {
        let y = iy * h / gy;
        for ix in 0..gx {
            let x = ix * w / gx;
            let i = (y * w + x) * 4;
            let (r, g, b) = (
                frame.data[i] as f32 / 255.0,
                frame.data[i + 1] as f32 / 255.0,
                frame.data[i + 2] as f32 / 255.0,
            );
            // BT.709 chroma differences, scaled so saturated primaries land
            // near the outer ring.
            let cb = (b - (0.2126 * r + 0.7152 * g + 0.0722 * b)) / 1.8556;
            let cr = (r - (0.2126 * r + 0.7152 * g + 0.0722 * b)) / 1.5748;
            let px = centre + egui::vec2(cb * 2.0 * radius, -cr * 2.0 * radius);
            if vrect.contains(px) {
                vp.circle_filled(px, 0.8, Color32::from_rgba_unmultiplied(140, 230, 240, 90));
            }
        }
    }
    ui.label(
        RichText::new("Histogram · luma waveform · RGB parade · vectorscope")
            .small()
            .color(egui::Color32::from_gray(120)),
    );
    true
}

fn checkerboard(painter: &egui::Painter, rect: Rect) {
    const CELL: f32 = 14.0;
    painter.rect_filled(rect, 0.0, Color32::from_gray(52));
    let dark = Color32::from_gray(36);
    let (cols, rows) = (
        (rect.width() / CELL).ceil() as i32,
        (rect.height() / CELL).ceil() as i32,
    );
    for row in 0..rows {
        for col in 0..cols {
            if (row + col) % 2 == 0 {
                continue;
            }
            let min = egui::pos2(rect.left() + col as f32 * CELL, rect.top() + row as f32 * CELL);
            let cell = Rect::from_min_size(min, Vec2::splat(CELL)).intersect(rect);
            painter.rect_filled(cell, 0.0, dark);
        }
    }
}

/// The control chrome: a window-wide seek bar, then one row — ☰ REEL on the
/// left, the transport centered, tools on the right. Used as the player's
/// fading overlay and as the editor's fixed bottom bar.
fn chrome(ui: &mut egui::Ui, app: &mut ReelApp) {
    let mode = app.mode;
    let mut goto_editor = false;
    let mut do_insert = false;
    let mut goto_player = false;
    let mut open_export = false;
    let mut toggle_fullscreen = false;

    // Row 1: the seek bar, edge to edge. In the editor it scrubs the WHOLE
    // EDIT (timeline time), not the source clip that happens to be loaded —
    // this is the player for the cut you are making.
    let editing = mode == Mode::Editor;
    let edit_len = if editing {
        crate::edit::render_duration(&app.project.export_segments()).max(0.001)
    } else {
        0.0
    };
    if editing {
        // The scrubber speaks EDIT time — the render's clock, gaps skipped
        // and overlaps collapsed — mapped to timeline positions only at the
        // seek. One truth of time: 8 s of edit is 8 s of scrubber.
        let mut pos = app.project.timeline_to_edit(app.editor.playhead).min(edit_len);
        let normal_slider = ui.spacing().slider_width;
        ui.spacing_mut().slider_width = ui.available_width();
        let resp = ui.add(
            egui::Slider::new(&mut pos, 0.0..=edit_len).show_value(false).trailing_fill(true),
        );
        ui.spacing_mut().slider_width = normal_slider;
        if resp.dragged() || resp.drag_stopped() {
            let t = app.project.edit_to_timeline(pos);
            app.seek_timeline(t);
        }
        if resp.drag_stopped() {
            resp.surrender_focus();
        }
    } else if let Some(player) = app.player.as_mut() {
        let dur = player.info.duration.max(0.001);
        let mut pos = player.position;
        // Scoped: slider_width is inherited by child uis — leaking the
        // full-window width here turns the volume slider gigantic.
        let normal_slider = ui.spacing().slider_width;
        ui.spacing_mut().slider_width = ui.available_width();
        let resp = ui.add(
            egui::Slider::new(&mut pos, 0.0..=dur).show_value(false).trailing_fill(true),
        );
        ui.spacing_mut().slider_width = normal_slider;
        if resp.drag_stopped() {
            player.seek(pos);
            resp.surrender_focus(); // keep arrow keys on the player, not the slider
        } else if resp.dragged() && player.cheap_seek() {
            player.seek(pos); // live scrub — frame-exact, mpv coalesces the seeks
        }
    }

    // Row 2: three clusters with EXPLICIT widths. Equal thirds overflowed —
    // the right-hand tools spilled left and drew on top of the transport.
    let row_h = 48.0;
    let total = ui.available_width();
    let left_w = 116.0_f32.min(total * 0.25);
    let right_w = 430.0_f32.min((total - left_w) * 0.62);
    let center_w = (total - left_w - right_w).max(80.0);
    let show_time = right_w > 380.0; // drop the readout before it can collide
    ui.horizontal(|ui| {
        let mut cols: Vec<egui::Ui> = Vec::new();
        for (w, layout) in [
            (left_w, egui::Layout::left_to_right(egui::Align::Center)),
            (center_w, egui::Layout::left_to_right(egui::Align::Center)),
            (right_w, egui::Layout::right_to_left(egui::Align::Center)),
        ] {
            let rect = ui.cursor().intersect(egui::Rect::everything_right_of(ui.cursor().left()));
            let rect = egui::Rect::from_min_size(rect.min, Vec2::new(w, row_h));
            ui.advance_cursor_after_rect(rect);
            cols.push(ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout)));
        }
        let mut cols = cols;
        cols[0].scope(|ui| {
            reel_menu(ui, app);
        });

        // Center: the transport (or the image's identity).
        cols[1].scope(|ui| {
            if let Some(img) = &app.image {
                let name = std::path::Path::new(&img.path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| img.path.clone());
                let text = format!("{name}  ·  {}×{}", img.width, img.height);
                let est = 20.0 + text.chars().count() as f32 * 7.0;
                ui.add_space(((ui.available_width() - est) / 2.0).max(0.0));
                ui.label(RichText::new(text).color(theme::STAR));
            } else if let Some(player) = app.player.as_mut() {
                // Big, round transport keys, centred under the picture. The
                // time readout lives with the other numbers on the right.
                let round = |label: &str, big: bool| {
                    egui::Button::new(RichText::new(label).size(if big { 22.0 } else { 16.0 }))
                        .min_size(Vec2::new(if big { 46.0 } else { 38.0 }, if big { 46.0 } else { 38.0 }))
                        .corner_radius(if big { 23.0 } else { 19.0 })
                };
                let gap = 8.0;
                let est = 3.0 * 38.0 + 46.0 + 4.0 * gap;
                ui.add_space(((ui.available_width() - est) / 2.0).max(0.0));
                ui.spacing_mut().item_spacing.x = gap;
                if ui.add(round("⏮", false)).on_hover_text("Back to start").clicked() {
                    player.seek(0.0);
                }
                if ui.add(round("◀", false)).on_hover_text("Frame back (,)").clicked() {
                    player.frame_step(false);
                }
                let label = if player.playing { "⏸" } else { "▶" };
                if ui.add(round(label, true)).on_hover_text("Play/pause (Space)").clicked() {
                    player.toggle_play();
                }
                if ui.add(round("▶", false)).on_hover_text("Frame forward (.)").clicked() {
                    player.frame_step(true);
                }
            }
        });

        cols[2].scope(|ui| {
            if let Some(img_open) = app.image.is_some().then_some(()) {
                let _ = img_open;
                if ui
                    .button(RichText::new("⬇ Export").color(theme::CYAN))
                    .on_hover_text("Convert — PNG/JPEG/WebP, resize")
                    .clicked()
                {
                    open_export = true;
                }
                if mode == Mode::Player {
                    if ui.button(RichText::new("✂ Edit").color(theme::EMBER)).clicked() {
                        goto_editor = true;
                    }
                } else if ui.button("▶ Done").on_hover_text("Back to the player (E)").clicked() {
                    goto_player = true;
                }
                return;
            }
            let Some(player) = app.player.as_mut() else { return };
            let time_text = if editing {
                format!(
                    "{} / {}",
                    fmt_time(app.project.timeline_to_edit(app.editor.playhead)),
                    fmt_time(edit_len)
                )
            } else {
                format!("{} / {}", fmt_time(player.position), fmt_time(player.info.duration))
            };
            if ui
                .button(RichText::new("⬇ Export").color(theme::CYAN))
                .on_hover_text("Convert this file — codec, quality, size")
                .clicked()
            {
                open_export = true;
            }
            if mode == Mode::Player {
                // Source monitor: previewing a pool item over a live edit —
                // I/O mark a window, Insert performs the three-point edit.
                if app.source_preview.is_some() {
                    let label = match (app.src_mark_in, app.src_mark_out) {
                        (Some(a), Some(b)) => format!("Insert {:.1}s", (b - a).max(0.0)),
                        (Some(_), None) | (None, Some(_)) => "Insert (half-marked)".into(),
                        (None, None) => "Insert all".into(),
                    };
                    if ui
                        .button(RichText::new(label).color(theme::STAR))
                        .on_hover_text("Three-point edit: source I/O + timeline playhead → a clip at the playhead")
                        .clicked()
                    {
                        do_insert = true;
                    }
                }
                if ui
                    .button(RichText::new("✂ Edit").color(theme::EMBER))
                    .on_hover_text("Open on the timeline (E)")
                    .clicked()
                {
                    goto_editor = true;
                }
            } else if ui.button("▶ Done").on_hover_text("Back to the player (E)").clicked() {
                goto_player = true;
            }
            if ui.button("⛶").on_hover_text("Fullscreen (F)").clicked() {
                toggle_fullscreen = true;
            }

            // Volume — only when the backend actually produces sound.
            if player.has_audio() {
                ui.spacing_mut().slider_width = 90.0;
                let mut vol = player.volume;
                if ui
                    .add(egui::Slider::new(&mut vol, 0.0..=130.0).show_value(false).trailing_fill(true))
                    .changed()
                {
                    player.set_volume(vol);
                }
                let speaker = if app.user_muted || player.volume <= 0.0 { "🔇" } else { "🔊" };
                if ui.button(speaker).on_hover_text("Mute (M)").clicked() {
                    app.user_muted = !app.user_muted;
                    player.set_muted(app.user_muted);
                }
            }

            // Speed: the control shows the USER's rate. In the editor the
            // active clip's own rate multiplies in underneath (a badge
            // says so), instead of hijacking this dial.
            let editing_here = app.mode == Mode::Editor;
            let mut speed = if editing_here { app.user_rate } else { player.speed };
            egui::ComboBox::from_id_salt("speed")
                .selected_text(format!("{speed}×"))
                .width(64.0)
                .show_ui(ui, |ui| {
                    for s in [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0] {
                        ui.selectable_value(&mut speed, s, format!("{s}×"));
                    }
                });
            if editing_here {
                if (speed - app.user_rate).abs() > 1e-9 {
                    app.user_rate = speed;
                }
                let clip_rate = app
                    .editor
                    .active_clip
                    .and_then(|id| app.project.clip(id))
                    .map(|c| c.source_len() / c.duration.max(1e-9))
                    .unwrap_or(1.0);
                if (clip_rate - 1.0).abs() > 0.01 {
                    ui.label(
                        RichText::new(format!("clip {clip_rate:.2}×"))
                            .small()
                            .color(theme::EMBER),
                    )
                    .on_hover_text("This clip has its own playback rate; it multiplies with yours.");
                }
            } else if speed != player.speed {
                player.set_speed(speed);
            }

            let mut looping = player.looping;
            if ui.toggle_value(&mut looping, "🔁").on_hover_text("Loop (L)").changed() {
                player.set_looping(looping);
            }

            // Audio visualizer picker (V cycles).
            if player.supports_visualizer() {
                let mut viz = player.visualizer;
                egui::ComboBox::from_id_salt("visualizer")
                    .selected_text(format!("〜 {}", viz.label()))
                    .show_ui(ui, |ui| {
                        for v in crate::video::player::Visualizer::ALL {
                            ui.selectable_value(&mut viz, v, v.label());
                        }
                    });
                if viz != player.visualizer {
                    player.set_visualizer(viz);
                }
            }
            if show_time {
                ui.label(RichText::new(time_text).monospace().size(13.0));
            }
        });
    });

    if do_insert {
        app.insert_source_into_edit();
    }
    if goto_editor {
        app.enter_editor();
    }
    if goto_player {
        app.mode = Mode::Player;
    }
    if open_export {
        app.open_export();
    }
    if toggle_fullscreen {
        app.fullscreen = !app.fullscreen;
    }
}

/// The HandBrake seam: convert the open file — codec, quality, resolution,
/// audio — without touching the editor.
fn export_window(ctx: &egui::Context, app: &mut ReelApp) {
    if !app.export_open {
        return;
    }
    let (source, kind) = match (app.media_path(), app.media_kind()) {
        (Some(s), Some(k)) => (s, k),
        _ => {
            app.export_open = false;
            return;
        }
    };
    let duration = app.player.as_ref().map(|p| p.info.duration).unwrap_or(0.0);

    let mut keep_open = app.export_open;
    egui::Window::new("Export / Convert")
        .open(&mut keep_open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(std::path::Path::new(&source).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(source.clone()))
                    .color(theme::STAR),
            );

            // What are we exporting — the source file, or the edit (and if
            // in/out markers are set, only the range they enclose)?
            let segments = app
                .project
                .export_segments_range(app.editor.range_in, app.editor.range_out);
            let cut_len = crate::edit::render_duration(&segments);
            let ranged = app.editor.range_in.is_some() || app.editor.range_out.is_some();
            let can_timeline = kind != crate::media::MediaKind::Image && !segments.is_empty();
            if can_timeline && app.export.is_none() {
                ui.add_space(4.0);
                let before = app.export_timeline;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut app.export_timeline, false, "Source file");
                    let label = if ranged {
                        format!("✂ Range ({} clip{}, {:.1}s)", segments.len(), if segments.len() == 1 { "" } else { "s" }, cut_len)
                    } else {
                        format!("✂ The edit ({} clip{}, {:.1}s)", segments.len(), if segments.len() == 1 { "" } else { "s" }, cut_len)
                    };
                    ui.selectable_value(&mut app.export_timeline, true, label)
                        .on_hover_text(if ranged {
                            "Only what the in/out markers enclose"
                        } else {
                            "The whole timeline as cut"
                        });
                });
                if app.export_timeline && !app.project.captions.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "• {} captions will be burned in",
                            app.project.captions.len()
                        ))
                        .small()
                        .color(theme::CYAN),
                    );
                }
                if app.export_timeline != before {
                    app.export_out = if app.export_timeline {
                        app.timeline_output()
                    } else {
                        export::default_output(&source, app.export_settings.codec)
                    };
                }
            } else if !can_timeline {
                app.export_timeline = false;
            }
            ui.add_space(6.0);

            // The render queue, whenever it has anything to say.
            if app.queue.is_busy() || !app.queue.done.is_empty() {
                egui::Frame::NONE
                    .fill(theme::VOID_2)
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Render queue").color(theme::CYAN).small());
                        if let Some((label, st)) = app.queue.current() {
                            ui.label(format!("⏵ {label}"));
                            ui.add(egui::ProgressBar::new(st.fraction).show_percentage().animate(true));
                        }
                        let waiting = app.queue.labels_pending();
                        if !waiting.is_empty() {
                            ui.label(
                                RichText::new(format!("waiting: {}", waiting.join(", ")))
                                    .small()
                                    .color(egui::Color32::from_gray(150)),
                            );
                        }
                        for (label, outcome) in &app.queue.done {
                            match outcome {
                                export::Outcome::Ok(path) => {
                                    ui.label(RichText::new(format!("✓ {label} → {path}")).small().color(theme::CYAN));
                                }
                                export::Outcome::Failed(e) => {
                                    ui.label(RichText::new(format!("✗ {label}: {e}")).small().color(theme::EMBER));
                                }
                            }
                        }
                        ui.horizontal(|ui| {
                            if app.queue.is_busy() && ui.button("Cancel all").clicked() {
                                app.queue.cancel_all();
                            }
                            if !app.queue.done.is_empty() && ui.button("Clear finished").clicked() {
                                app.queue.clear_done();
                            }
                        });
                    });
                ui.add_space(8.0);
            }

            // A job in flight (or just finished) owns the dialog.
            if let Some(job) = &app.export {
                let st = job.state();
                if !st.finished {
                    ui.label(format!("Encoding → {}", job.output));
                    ui.add(egui::ProgressBar::new(st.fraction).show_percentage().animate(true));
                    if st.speed > 0.0 {
                        ui.label(RichText::new(format!("{:.2}× realtime", st.speed)).small());
                    }
                    if ui.button("Cancel").clicked() {
                        job.cancel();
                    }
                } else {
                    match &st.error {
                        None => {
                            ui.colored_label(theme::CYAN, format!("✓ Done → {}", job.output));
                        }
                        Some(e) if e == "cancelled" => {
                            ui.colored_label(theme::EMBER, "Export cancelled.");
                        }
                        Some(e) => {
                            ui.colored_label(theme::EMBER, format!("✗ {e}"));
                        }
                    }
                    if ui.button("OK").clicked() {
                        app.export = None;
                        app.export_out = if app.export_timeline {
                            app.timeline_output()
                        } else {
                            export::default_output(&source, app.export_settings.codec)
                        };
                    }
                }
                return;
            }

            // ── One-click destinations ───────────────────────────────────
            // Pick where it's going; Reel picks the frame, fit and codec.
            if kind != crate::media::MediaKind::Image {
                ui.label(
                    RichText::new("Where is this going?")
                        .size(15.0)
                        .color(theme::STAR),
                );
                ui.add_space(6.0);
                let mut chosen: Option<&export::Preset> = None;
                // Big cards, three to a row: the name you think in, and the
                // frame you get. Choosing one is the whole decision.
                let card_w = (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0;
                for row in export::Preset::ALL.chunks(3) {
                    ui.horizontal(|ui| {
                        for p in row {
                            let active = p.is_active(&app.export_settings);
                            let (fg, bg) = if active {
                                (theme::VOID, theme::CYAN)
                            } else {
                                (theme::STAR, theme::VOID_3)
                            };
                            let label = format!("{}\n{}", p.name, p.note);
                            let btn = egui::Button::new(RichText::new(label).color(fg).size(13.0))
                                .fill(bg)
                                .corner_radius(10.0)
                                .min_size(Vec2::new(card_w, 46.0));
                            if ui.add(btn).on_hover_text(p.fit.label()).clicked() {
                                chosen = Some(p);
                            }
                        }
                    });
                }
                if let Some(p) = chosen {
                    p.apply(&mut app.export_settings);
                    app.export_out = app.preset_output(p);
                }
                // Publish everywhere: one click queues a render per
                // platform — the queue runs them in order while you keep
                // editing.
                if app.export_timeline {
                    // Delivery knobs for the batch: filename template and
                    // which platforms burn captions.
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Names").small().color(egui::Color32::from_gray(160)));
                        ui.add(
                            egui::TextEdit::singleline(&mut app.publish_template)
                                .desired_width(ui.available_width())
                        ).on_hover_text("{name} = project, {platform} = preset slug");
                    });
                    if !app.project.captions.is_empty() {
                        ui.collapsing("Captions per platform", |ui| {
                            for preset in export::Preset::ALL {
                                let slug = preset.slug();
                                let mut on = !app.publish_no_captions.contains(&slug);
                                if ui.checkbox(&mut on, preset.name).changed() {
                                    if on {
                                        app.publish_no_captions.remove(&slug);
                                    } else {
                                        app.publish_no_captions.insert(slug);
                                    }
                                }
                            }
                        });
                    }
                    let every = egui::Button::new(
                        RichText::new("Queue ALL platforms").color(theme::VOID),
                    )
                    .fill(theme::EMBER)
                    .corner_radius(8.0);
                    if ui
                        .add_sized([ui.available_width(), 26.0], every)
                        .on_hover_text(
                            "One render per preset — YouTube, TikTok, Reels, feed, square, \
                             Facebook, X — named after the platform, delivered at its loudness",
                        )
                        .clicked()
                    {
                        let count = app.queue_all_platforms();
                        app.status = format!("{count} renders queued — keep editing.");
                    }
                }
                ui.add_space(6.0);
                let custom = export::Preset::ALL.iter().all(|p| !p.is_active(&app.export_settings));
                if ui
                    .selectable_label(custom, RichText::new("⚙ Custom settings").size(13.0))
                    .on_hover_text("Choose format, quality and size yourself")
                    .clicked()
                {
                    app.export_settings.target = None;
                }
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
            }

            let timeline_mode = app.export_timeline;
            let s = &mut app.export_settings;
            // Keep the codec legal for what's open. A rendered timeline is
            // always a video file — audio-only/remux targets don't apply.
            let codecs: &[Codec] = if timeline_mode {
                &[Codec::H264, Codec::H265, Codec::Av1, Codec::Vp9]
            } else {
                Codec::for_kind(kind)
            };
            if !codecs.contains(&s.codec) {
                s.codec = codecs[0];
            }
            let prev_codec = s.codec;

            egui::Grid::new("export_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Format");
                egui::ComboBox::from_id_salt("codec")
                    .selected_text(s.codec.label())
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for &c in codecs {
                            ui.selectable_value(&mut s.codec, c, c.label());
                        }
                    });
                ui.end_row();

                let is_video_out = !s.codec.is_audio_only() && !s.codec.is_image() && s.codec != Codec::Remux;

                if s.codec.has_quality() {
                    ui.label("Quality");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("quality")
                            .selected_text(s.quality.label())
                            .show_ui(ui, |ui| {
                                for q in [Quality::High, Quality::Balanced, Quality::Small] {
                                    ui.selectable_value(&mut s.quality, q, q.label());
                                }
                                // Raw CRF is a video-encoder knob.
                                if is_video_out
                                    && ui
                                        .selectable_label(matches!(s.quality, Quality::Custom(_)), Quality::Custom(23).label())
                                        .clicked()
                                {
                                    s.quality = Quality::Custom(23);
                                }
                            });
                        if is_video_out {
                            if let Quality::Custom(crf) = &mut s.quality {
                                let mut v = *crf as i32;
                                ui.add(egui::Slider::new(&mut v, 10..=50).text("CRF"));
                                *crf = v as u8;
                            }
                        }
                    });
                    ui.end_row();
                }

                if let Some((tw, th)) = s.target {
                    // A preset owns the frame; the choice left is how the
                    // picture is placed inside it.
                    ui.label("Frame");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{tw}×{th}")).monospace());
                        egui::ComboBox::from_id_salt("fit")
                            .selected_text(s.fit.label())
                            .show_ui(ui, |ui| {
                                for f in Fit::ALL {
                                    ui.selectable_value(&mut s.fit, f, f.label());
                                }
                            });
                    });
                    ui.end_row();
                } else if is_video_out || s.codec.is_image() {
                    ui.label("Resolution");
                    egui::ComboBox::from_id_salt("resolution")
                        .selected_text(s.resolution.label())
                        .show_ui(ui, |ui| {
                            for r in Resolution::ALL {
                                ui.selectable_value(&mut s.resolution, r, r.label());
                            }
                        });
                    ui.end_row();
                }

                if is_video_out && timeline_mode {
                    // Timeline renders carry the sources' audio automatically.
                    ui.label("Audio");
                    ui.label(
                        RichText::new("from the clips")
                            .small()
                            .color(egui::Color32::from_gray(150)),
                    );
                    ui.end_row();
                } else if is_video_out {
                    ui.label("Audio");
                    egui::ComboBox::from_id_salt("audio")
                        .selected_text(match s.audio {
                            AudioMode::Copy => "Copy (untouched)".to_string(),
                            AudioMode::Encode { kbps } => format!("Encode {kbps} kb/s"),
                        })
                        .show_ui(ui, |ui| {
                            for kbps in [128u32, 160, 256] {
                                ui.selectable_value(&mut s.audio, AudioMode::Encode { kbps }, format!("Encode {kbps} kb/s"));
                            }
                            ui.selectable_value(&mut s.audio, AudioMode::Copy, "Copy (untouched)");
                        });
                    ui.end_row();
                }

                // Hardware encoding, when this machine can do it.
                let hw_for_codec = if s.codec.is_audio_only() || s.codec.is_image() || s.codec == Codec::Remux {
                    None
                } else {
                    export::hw_encoder_for(if timeline_mode && !matches!(s.codec, Codec::H265 | Codec::Av1 | Codec::Vp9) {
                        Codec::H264
                    } else {
                        s.codec
                    })
                };
                if let Some(hw) = hw_for_codec {
                    ui.label("Encoder");
                    ui.checkbox(&mut s.hardware, format!("{} (faster)", hw.label()))
                        .on_hover_text("Uncheck for the software encoder — slower, slightly smaller files");
                    ui.end_row();
                }

                ui.label("Save to");
                ui.add(egui::TextEdit::singleline(&mut app.export_out).desired_width(260.0));
                ui.end_row();
            });

            if s.codec != prev_codec {
                app.export_out = if timeline_mode {
                    app.timeline_output()
                } else {
                    export::default_output(&source, s.codec)
                };
            }

            ui.add_space(8.0);
            // Queueing is how you get every platform in one go: pick a
            // destination, queue it, pick the next, queue it, walk away.
            let mut queue_clicked = false;
            ui.horizontal(|ui| {
                let label = export::Preset::ALL
                    .iter()
                    .find(|p| p.is_active(&app.export_settings))
                    .map(|p| p.name.to_string())
                    .unwrap_or_else(|| "Custom".to_string());
                let btn = egui::Button::new(RichText::new(format!("+ Queue {label}")).color(theme::CYAN))
                    .corner_radius(8.0)
                    .min_size(Vec2::new(0.0, 30.0));
                if ui
                    .add(btn)
                    .on_hover_text("Add this to the render queue and keep choosing")
                    .clicked()
                {
                    queue_clicked = true;
                }
            });
            if queue_clicked {
                let label = export::Preset::ALL
                    .iter()
                    .find(|p| p.is_active(&app.export_settings))
                    .map(|p| p.name.to_string())
                    .unwrap_or_else(|| "Custom".to_string());
                app.queue_current_export(label);
                // Suggest a fresh name so the next queue entry can't collide.
                if let Some(p) = export::Preset::ALL.iter().find(|p| p.is_active(&app.export_settings)) {
                    app.export_out = app.preset_output(p);
                }
            }
            ui.add_space(4.0);
            // add_sized (not min_size): it centres the label in the button.
            let start = ui.add_sized(
                [ui.available_width(), 38.0],
                egui::Button::new(RichText::new("Start export").color(theme::VOID).strong().size(15.0))
                    .fill(theme::CYAN)
                    .corner_radius(10.0),
            );
            if start.clicked() && app.export_timeline {
                match export::start_timeline_with_captions(
                    &segments,
                    &app.export_out,
                    &app.export_settings,
                    (app.project.width, app.project.height, app.project.fps),
                    export::Overlays {
                        captions: &app.project.captions,
                        caption_size: app.project.caption_size,
                        titles: &app.project.titles,
                        music: app.project.music.as_ref(),
                        roomtone: app.project.roomtone.as_ref(),
                        overlays: &app.project.overlay_segments(),
                        markers: &app.project.markers,
                        marker_labels: &app.project.marker_labels,
                        luts: &app.project.luts,
                        plugins: &app.project.plugins,
                        audio_clips: &app.project.audio_clips(),
                    },
                ) {
                    Ok(job) => app.export = Some(job),
                    Err(e) => app.status = format!("Export: {e}"),
                }
            } else if start.clicked() {
                // ffmpeg can't read SVG — hand it the rasterized copy instead.
                let input = if crate::media::is_svg_path(&source) {
                    app.image.as_ref().map(|img| img.write_temp_png())
                } else {
                    None
                };
                let input = match input {
                    Some(Ok(p)) => p.to_string_lossy().into_owned(),
                    Some(Err(e)) => {
                        app.status = format!("Export: {e}");
                        return;
                    }
                    None => source.clone(),
                };
                match export::start(&input, &app.export_out, &app.export_settings, duration) {
                    Ok(job) => app.export = Some(job),
                    Err(e) => app.status = format!("Export: {e}"),
                }
            }
        });
    app.export_open = keep_open;
}

/// The NLE timeline: adaptive ruler, zoom/pan, clip select/move/trim with
/// snapping, split/delete, undo/redo, and a draggable playhead that previews
/// (and during playback, sequences) the edit.
fn timeline(ui: &mut egui::Ui, app: &mut ReelApp) {
    use crate::edit::{Drag, EditorState};

    // ── Toolbar ──────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        if ui.button("−").on_hover_text("Zoom out (Ctrl+scroll)").clicked() {
            app.editor.px_per_s = (app.editor.px_per_s / 1.5).max(2.0);
        }
        if ui.button("+").on_hover_text("Zoom in (Ctrl+scroll)").clicked() {
            app.editor.px_per_s = (app.editor.px_per_s * 1.5).min(600.0);
        }
        if ui.button("↔").on_hover_text("Fit the whole edit").clicked() {
            let w = ui.available_width().max(200.0);
            app.editor.px_per_s = (w / app.project.duration().max(1.0) as f32).clamp(2.0, 600.0);
            app.editor.scroll_x = 0.0;
        }
        ui.separator();
        if ui.button("✂ Split").on_hover_text("Split under the playhead (S)").clicked() {
            app.editor_split();
        }
        let lane_on = app.editor.show_curve_lane;
        if ui
            .selectable_label(lane_on, "Curve lane")
            .on_hover_text(
                "A full-width lane under the tracks: the selected clip's \
                 keyframes for the Animate panel's parameter, aligned with \
                 the ruler. Drag keys, double-click adds, right-click removes.",
            )
            .clicked()
        {
            app.editor.show_curve_lane = !lane_on;
        }
        if ui
            .button("+ Adjust")
            .on_hover_text(
                "Add an ADJUSTMENT LAYER at the playhead: a 4s span whose \
                 colour grade applies to everything beneath it — select it \
                 and use the Look panel",
            )
            .clicked()
        {
            app.editor.push_undo(&app.project);
            let at = app.editor.playhead;
            let id = app.project.add_clip("", crate::edit::TrackKind::Overlay, at, 0.0, 4.0);
            if let Some(c) = app.project.clip_mut(id) {
                c.adjustment = true;
                c.name = "adjust".into();
            }
            app.editor.selected = Some(id);
            app.editor.mark_changed();
        }
        let del = ui.add_enabled(app.editor.selected.is_some(), egui::Button::new("🗑 Delete"));
        if del.on_hover_text("Delete selected clip (Del)").clicked() {
            app.editor_delete();
        }
        ui.separator();
        if ui.add_enabled(app.editor.can_undo(), egui::Button::new("Undo")).on_hover_text("Undo (Ctrl+Z)").clicked() {
            app.editor.undo(&mut app.project);
        }
        if ui.add_enabled(app.editor.can_redo(), egui::Button::new("Redo")).on_hover_text("Redo (Ctrl+Shift+Z)").clicked() {
            app.editor.redo(&mut app.project);
        }
        ui.separator();
        if ui.button("[").on_hover_text("Range in at playhead (I)").clicked() {
            app.editor.range_in = Some(app.editor.playhead);
        }
        if ui.button("]").on_hover_text("Range out at playhead (O)").clicked() {
            app.editor.range_out = Some(app.editor.playhead);
        }
        let has_range = app.editor.range_in.is_some() || app.editor.range_out.is_some();
        if ui
            .add_enabled(has_range, egui::Button::new("✕"))
            .on_hover_text("Clear export range (Shift+I/O)")
            .clicked()
        {
            app.editor.range_in = None;
            app.editor.range_out = None;
        }
        ui.separator();
        // No Save button: edits save themselves (see app::poll_autosave).
        let saved_label = if app.editor.dirty { "saving…" } else { "saved" };
        ui.label(RichText::new(saved_label).small().color(egui::Color32::from_gray(130)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{}  ·  {:.1}s", app.project.name, app.project.duration()))
                    .color(theme::CYAN)
                    .small(),
            );
        });
    });
    ui.add_space(2.0);

    // ── Canvas ───────────────────────────────────────────────────────────
    // Allocate the space explicitly — a painter-only canvas claims nothing,
    // and an unclaimed resizable panel collapses to its toolbar.
    let want = Vec2::new(ui.available_width(), ui.available_height().max(120.0));
    let (full, _) = ui.allocate_exact_size(want, Sense::hover());
    if full.height() < 40.0 {
        return;
    }
    let ruler_h = 18.0;
    // Lanes get taller than they used to: a clip now carries its thumbnails
    // and its waveform, and both are useless when squeezed into 40 px.
    let curve_lane_h: f32 = if app.editor.show_curve_lane { 74.0 } else { 0.0 };
    let lane_h = ((full.height() - ruler_h - 8.0 - curve_lane_h)
        / app.project.tracks.len().max(1) as f32)
        .clamp(24.0, 72.0);

    // Wheel: pan; Ctrl+wheel: zoom around the cursor.
    if ui.rect_contains_pointer(full) {
        let (scroll, modifiers, pointer) =
            ui.ctx().input(|i| (i.raw_scroll_delta, i.modifiers, i.pointer.hover_pos()));
        let pps = app.editor.px_per_s;
        if modifiers.ctrl || modifiers.command {
            if scroll.y.abs() > 0.0 {
                let factor = (scroll.y * 0.0035).exp();
                let new_pps = (pps * factor).clamp(2.0, 600.0);
                if let Some(p) = pointer {
                    let anchor_t = ((p.x - full.left()) / pps) + app.editor.scroll_x;
                    app.editor.scroll_x = anchor_t - (p.x - full.left()) / new_pps;
                }
                app.editor.px_per_s = new_pps;
            }
        } else {
            let d = if scroll.x.abs() > scroll.y.abs() { scroll.x } else { scroll.y };
            app.editor.scroll_x -= d / pps;
        }
        app.editor.scroll_x = app.editor.scroll_x.max(0.0);
    }
    let pps = app.editor.px_per_s;
    let scroll_x = app.editor.scroll_x;
    let t_to_x = move |t: f64| full.left() + (t as f32 - scroll_x) * pps;
    let x_to_t = move |x: f32| ((((x - full.left()) / pps) + scroll_x).max(0.0)) as f64;
    let snap_tol = (8.0 / pps) as f64;

    let painter = ui.painter_at(full);
    painter.rect_filled(full, 0.0, theme::VOID);

    // Ruler with adaptive tick spacing (a "nice" step at least ~70 px wide).
    let step = {
        let candidates = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0];
        *candidates.iter().find(|&&s| s as f32 * pps >= 70.0).unwrap_or(&600.0)
    };
    let t_end = x_to_t(full.right());
    let mut t = ((scroll_x as f64 / step).floor() * step).max(0.0);
    while t <= t_end {
        let x = t_to_x(t);
        painter.line_segment(
            [egui::pos2(x, full.top()), egui::pos2(x, full.top() + ruler_h)],
            Stroke::new(1.0, Color32::from_gray(80)),
        );
        painter.text(
            egui::pos2(x + 4.0, full.top() + 1.0),
            egui::Align2::LEFT_TOP,
            fmt_time(t),
            egui::FontId::monospace(10.0),
            Color32::from_gray(140),
        );
        for k in 1..4 {
            let xm = t_to_x(t + step * k as f64 / 4.0);
            painter.line_segment(
                [egui::pos2(xm, full.top() + ruler_h - 5.0), egui::pos2(xm, full.top() + ruler_h)],
                Stroke::new(1.0, Color32::from_gray(55)),
            );
        }
        t += step;
    }

    // Background interact FIRST — clips registered afterwards sit on top.
    let bg = ui.interact(full, ui.id().with("tl_bg"), Sense::click_and_drag());
    if bg.drag_started() {
        // Shift+drag lassoes clips; a plain drag scrubs the playhead.
        let shift = ui.input(|i| i.modifiers.shift);
        app.editor.drag = match (shift, bg.interact_pointer_pos()) {
            (true, Some(p)) => Some(Drag::Lasso { x0: p.x, y0: p.y }),
            _ => Some(Drag::Playhead),
        };
    }
    if bg.clicked() {
        app.editor.selected = None;
        if let Some(p) = bg.interact_pointer_pos() {
            app.seek_timeline(x_to_t(p.x));
        }
    }
    if matches!(app.editor.drag, Some(Drag::Playhead)) && bg.dragged() {
        if let Some(p) = bg.interact_pointer_pos() {
            app.seek_timeline(x_to_t(p.x));
        }
    }
    // The lasso: draw the band while dragging; select what it crosses on
    // release. Clip rectangles are recomputed with the SAME lane geometry
    // the drawing loop uses below.
    if let Some(Drag::Lasso { x0, y0 }) = app.editor.drag {
        if let Some(p) = bg.interact_pointer_pos() {
            let band = Rect::from_two_pos(egui::pos2(x0, y0), p);
            painter.rect_stroke(band, 2.0, Stroke::new(1.0, theme::CYAN), egui::StrokeKind::Inside);
            painter.rect_filled(band, 2.0, Color32::from_rgba_unmultiplied(60, 180, 200, 18));
            if bg.drag_stopped() {
                let mut hits: Vec<u64> = Vec::new();
                for (i, track) in app.project.tracks.iter().enumerate() {
                    let top = full.top() + ruler_h + 4.0 + i as f32 * (lane_h + 4.0);
                    for c in &track.clips {
                        let r = Rect::from_min_max(
                            egui::pos2(t_to_x(c.start), top + 2.0),
                            egui::pos2(t_to_x(c.end()), top + lane_h - 2.0),
                        );
                        if band.intersects(r) {
                            hits.push(c.id);
                        }
                    }
                }
                app.editor.selected = hits.first().copied();
                app.editor.multi = hits.into_iter().collect();
            }
        }
    }
    if bg.drag_stopped() {
        app.editor.drag = None;
    }
    let mut close_all_bg = false;
    bg.context_menu(|ui| {
        if ui.button("Close every gap").clicked() {
            close_all_bg = true;
            ui.close_menu();
        }
    });
    if close_all_bg {
        app.editor.push_undo(&app.project);
        let moved = app.project.close_all_gaps();
        app.status = format!("Closed {moved:.2}s of gaps.");
    }

    // Lanes + clips (drawn from a snapshot; edits go through clip_mut).
    let mut snap_line: Option<f64> = None;
    let track_data: Vec<(u64, TrackKind, String, Vec<crate::edit::Clip>)> = app
        .project
        .tracks
        .iter()
        .map(|tr| (tr.id, tr.kind.clone(), tr.name.clone(), tr.clips.clone()))
        .collect();
    for (i, (tid, kind, tname, clips)) in track_data.iter().enumerate() {
        let top = full.top() + ruler_h + 4.0 + i as f32 * (lane_h + 4.0);
        let lane = Rect::from_min_size(egui::pos2(full.left(), top), Vec2::new(full.width(), lane_h));
        painter.rect_filled(lane, 4.0, theme::VOID_2);
        // The lane's name doubles as the TARGET toggle: click it and
        // Ctrl+V lands here (kind permitting).
        let name_rect = Rect::from_min_size(
            egui::pos2(lane.left() + 2.0, lane.top() + 2.0),
            Vec2::new(30.0, 16.0),
        );
        let name_resp = ui.interact(name_rect, ui.id().with(("lane_target", *tid)), Sense::click());
        let targeted = app.editor.target_track == Some(*tid);
        if name_resp.clicked() {
            app.editor.target_track = if targeted { None } else { Some(*tid) };
        }
        if targeted {
            painter.rect_filled(name_rect, 3.0, Color32::from_rgba_unmultiplied(60, 180, 200, 40));
        }
        painter.text(
            egui::pos2(lane.left() + 5.0, lane.center().y),
            egui::Align2::LEFT_CENTER,
            tname,
            egui::FontId::monospace(10.0),
            if targeted { theme::CYAN } else { Color32::from_gray(110) },
        );

        let base = match kind {
            TrackKind::Video => theme::CYAN,
            TrackKind::Audio => theme::EMBER,
            TrackKind::Overlay => theme::STAR,
        };
        let _ = &base;
        for clip in clips {
            let x0 = t_to_x(clip.start);
            let x1 = t_to_x(clip.end());
            if x1 < full.left() || x0 > full.right() {
                continue;
            }
            let cr = Rect::from_min_max(
                egui::pos2(x0, top + 2.0),
                egui::pos2(x1.max(x0 + 2.0), top + lane_h - 2.0),
            );
            let selected =
                app.editor.selected == Some(clip.id) || app.editor.multi.contains(&clip.id);
            let fill = if selected { base.linear_multiply(0.55) } else { base.linear_multiply(0.30) };
            painter.rect_filled(cr, 5.0, fill);
            painter.rect_stroke(
                cr,
                5.0,
                Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { theme::STAR } else { base }),
                egui::StrokeKind::Inside,
            );
            // Thumbnails fill the clip, waveform sits in a band along the
            // bottom — the arrangement every NLE settled on, because you
            // scan for a shot by picture and for a cut by sound.
            let mut wave_rect = cr;
            if cr.width() > 8.0
                && cr.height() > 26.0
                && matches!(kind, TrackKind::Video | TrackKind::Overlay)
            {
                let source_len = clip.in_point + clip.source_len();
                if let Some(sheet) = app.thumbs.get(ui.ctx(), &clip.source, source_len.max(0.1)) {
                    let cell_w = (cr.height() - 2.0) * 16.0 / 9.0;
                    let n = (cr.width() / cell_w).ceil().max(1.0) as usize;
                    let step = cr.width() / n as f32;
                    let tex = sheet.tex.id();
                    for i in 0..n {
                        let x0 = cr.left() + i as f32 * step;
                        let cell = Rect::from_min_max(
                            egui::pos2(x0, cr.top() + 1.0),
                            egui::pos2((x0 + step).min(cr.right()), cr.bottom() - 1.0),
                        );
                        let t = clip.in_point
                            + clip.source_len() * ((i as f64 + 0.5) / n as f64);
                        let Some(mut uv) = sheet.uv_at(t) else { continue };
                        // The last cell is usually clipped; crop its UV to
                        // match so the picture isn't squashed.
                        if cell.width() < step - 0.5 {
                            uv.max.x = uv.min.x + uv.width() * (cell.width() / step);
                        }
                        painter.add(egui::Shape::image(tex, cell, uv, Color32::WHITE));
                    }
                    // Waveform gets the bottom third, over a scrim.
                    let band = Rect::from_min_max(
                        egui::pos2(cr.left(), cr.bottom() - cr.height() * 0.34),
                        cr.right_bottom(),
                    );
                    painter.rect_filled(band, 0.0, Color32::from_black_alpha(120));
                    wave_rect = band;
                }
            }

            // The waveform, so you can cut on a word instead of hunting for
            // it. Peaks are cached per source and computed off-thread; until
            // they land the clip just draws plain.
            if wave_rect.width() > 8.0 {
                if let Some(peaks) = app.waveforms.get(&clip.source) {
                    let slots = (wave_rect.width() as usize).clamp(1, 4000);
                    let vals =
                        peaks.window(clip.in_point, clip.in_point + clip.source_len(), slots);
                    if !vals.is_empty() {
                        let mid = wave_rect.center().y;
                        let half = (wave_rect.height() * 0.5) - 2.0;
                        let colour = if wave_rect == cr {
                            base.linear_multiply(if selected { 0.95 } else { 0.6 })
                        } else {
                            theme::STAR.linear_multiply(0.75)
                        };
                        let step = wave_rect.width() / vals.len() as f32;
                        for (i, v) in vals.iter().enumerate() {
                            let x = wave_rect.left() + i as f32 * step + step * 0.5;
                            let h = (v * half).max(0.5);
                            painter.line_segment(
                                [egui::pos2(x, mid - h), egui::pos2(x, mid + h)],
                                Stroke::new(step.min(1.5).max(0.8), colour),
                            );
                        }
                    }
                }
            }

            // Adjustment layers wear their difference: a violet wash and a
            // label — they grade what's below, they aren't footage.
            if clip.adjustment {
                painter.rect_filled(cr, 3.0, Color32::from_rgba_unmultiplied(190, 140, 255, 46));
            }
            if cr.width() > 40.0 {
                // A slab behind the label so it stays readable over the wave.
                let text = if clip.adjustment {
                    format!("ADJUST  {:.1}s", clip.duration)
                } else {
                    format!("{}  {:.1}s", clip.name, clip.duration)
                };
                let galley = painter.layout_no_wrap(
                    text,
                    egui::FontId::proportional(11.0),
                    theme::STAR,
                );
                let at = egui::pos2(cr.left() + 6.0, cr.top() + 3.0);
                painter.rect_filled(
                    Rect::from_min_size(at, galley.size()).expand2(Vec2::new(3.0, 1.0)),
                    3.0,
                    theme::VOID.linear_multiply(0.65),
                );
                painter.galley(at, galley, theme::STAR);
            }

            // Keyframe diamonds along the clip's lower edge — painted, not
            // glyphs (the bundled font has no ◆), so they render everywhere.
            if !clip.keys.is_empty() {
                let y = cr.bottom() - 6.0;
                for (_, track) in &clip.keys {
                    for k in track {
                        let x = t_to_x(clip.start + k.t);
                        if x < cr.left() - 4.0 || x > cr.right() + 4.0 {
                            continue;
                        }
                        painter.add(egui::Shape::convex_polygon(
                            vec![
                                egui::pos2(x, y - 4.0),
                                egui::pos2(x + 4.0, y),
                                egui::pos2(x, y + 4.0),
                                egui::pos2(x - 4.0, y),
                            ],
                            theme::STAR,
                            Stroke::new(1.0, theme::VOID),
                        ));
                    }
                }
            }

            // A crossfade marker at the clip's head: the wedge shows where
            // the two clips overlap in the render.
            if clip.transition_in > 0.0 && *kind == TrackKind::Video {
                let w = (clip.transition_in as f32 * pps).min(cr.width());
                let wedge = Rect::from_min_max(cr.left_top(), egui::pos2(cr.left() + w, cr.bottom()));
                painter.rect_filled(wedge, 5.0, theme::STAR.linear_multiply(0.18));
                painter.line_segment(
                    [wedge.left_bottom(), wedge.right_top()],
                    Stroke::new(1.0, theme::STAR),
                );
            }

            // Interaction: edges trim, body moves.
            let resp = ui.interact(cr, ui.id().with(("clip", clip.id)), Sense::click_and_drag());
            let zone = resp
                .hover_pos()
                .map(|p| {
                    if p.x < cr.left() + 7.0 {
                        1
                    } else if p.x > cr.right() - 7.0 {
                        2
                    } else {
                        0
                    }
                })
                .unwrap_or(0);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(if zone == 0 {
                    egui::CursorIcon::Grab
                } else {
                    egui::CursorIcon::ResizeHorizontal
                });
            }
            if resp.clicked() || resp.drag_started() {
                let shift = ui.ctx().input(|i| i.modifiers.shift);
                if shift {
                    // Shift-click grows (or shrinks) the selection set; the
                    // clicked clip becomes the primary the panel edits.
                    if let Some(primary) = app.editor.selected {
                        app.editor.multi.insert(primary);
                    }
                    if !app.editor.multi.insert(clip.id) {
                        app.editor.multi.remove(&clip.id);
                    }
                    app.editor.selected = Some(clip.id);
                } else if !app.editor.multi.contains(&clip.id) {
                    app.editor.multi.clear();
                    app.editor.selected = Some(clip.id);
                } else {
                    app.editor.selected = Some(clip.id);
                }
            }
            let mut close_this = false;
            let mut close_all = false;
            let mut delete_this = false;
            resp.context_menu(|ui| {
                ui.label(RichText::new(&clip.name).small().color(theme::CYAN));
                if ui.button("Close gap before this clip").clicked() {
                    close_this = true;
                    ui.close_menu();
                }
                if ui.button("Close every gap").clicked() {
                    close_all = true;
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Delete clip").clicked() {
                    delete_this = true;
                    ui.close_menu();
                }
            });
            if close_this || close_all || delete_this {
                app.editor.selected = Some(clip.id);
                app.editor.push_undo(&app.project);
            }
            if close_this {
                let moved = app.project.close_gap_before(clip.id);
                app.status = if moved > 0.0 {
                    format!("Closed a {moved:.2}s gap.")
                } else {
                    "No gap before that clip.".into()
                };
            }
            if close_all {
                let moved = app.project.close_all_gaps();
                app.status = format!("Closed {moved:.2}s of gaps.");
            }
            if delete_this {
                app.project.delete_clip(clip.id);
                app.editor.selected = None;
                app.status = "Clip deleted.".into();
            }
            if resp.drag_started() {
                app.editor.push_undo(&app.project);
                let mods = ui.ctx().input(|i| i.modifiers);
                let at = resp
                    .interact_pointer_pos()
                    .map(|p| x_to_t(p.x))
                    .unwrap_or(clip.start);
                app.editor.drag = Some(match (zone, mods.ctrl || mods.command, mods.alt) {
                    // Ctrl on the head edge: ROLL the cut with the left
                    // neighbour. (Rolling the tail is the neighbour's head.)
                    (1, true, _) => Drag::Roll { id: clip.id, last: at },
                    (2, true, _) => Drag::Roll { id: clip.id, last: at }, // resolved below
                    (1, false, _) => Drag::TrimL { id: clip.id },
                    (2, false, _) => Drag::TrimR { id: clip.id },
                    (_, true, true) => Drag::Slide { id: clip.id, last: at },
                    (_, false, true) => Drag::Slip { id: clip.id, last: at },
                    _ => Drag::Move {
                        id: clip.id,
                        grab: resp
                            .interact_pointer_pos()
                            .map(|p| x_to_t(p.x) - clip.start)
                            .unwrap_or(0.0),
                    },
                });
                // Ctrl on the TAIL edge rolls the cut with the RIGHT
                // neighbour — which is that neighbour's head roll.
                if zone == 2 && (mods.ctrl || mods.command) {
                    if let Some(next) = app
                        .project
                        .clip_after(crate::edit::TrackKind::Video, clip.start)
                        .map(|n| n.id)
                    {
                        app.editor.drag = Some(Drag::Roll { id: next, last: at });
                    }
                }
            }
            if resp.drag_stopped() {
                app.editor.mark_changed(); // trims/moves land in the autosave
            }
            if let (Some(drag), true, Some(p)) = (app.editor.drag, resp.dragged(), resp.interact_pointer_pos()) {
                let pt = x_to_t(p.x);
                let mut targets = app.project.snap_targets(Some(clip.id));
                targets.push(app.editor.playhead);
                let (lo, hi) = app.project.move_range(clip.id);
                match drag {
                    Drag::Move { id, grab } if id == clip.id => {
                        let (snapped, hit) = EditorState::snap(pt - grab, &targets, snap_tol);
                        snap_line = hit;
                        let clamped = snapped.clamp(lo, hi.max(lo));
                        let (delta, old_span) = app
                            .project
                            .clip(id)
                            .map(|c| (clamped - c.start, (c.start, c.end())))
                            .unwrap_or((0.0, (0.0, 0.0)));
                        if let Some(c) = app.project.clip_mut(id) {
                            c.start = clamped;
                        }
                        // Captions/markers/titles inside the clip ride along.
                        if delta.abs() > 1e-9 {
                            app.project.carry_annotations(old_span.0, old_span.1, delta);
                        }
                        // A selection moves as one object.
                        if delta.abs() > 1e-9 {
                            let others: Vec<u64> = app
                                .editor
                                .multi
                                .iter()
                                .copied()
                                .filter(|o| *o != id)
                                .collect();
                            for o in others {
                                if let Some(c) = app.project.clip_mut(o) {
                                    c.start = (c.start + delta).max(0.0);
                                }
                            }
                        }
                    }
                    Drag::TrimL { id } if id == clip.id => {
                        let (snapped, hit) = EditorState::snap(pt, &targets, snap_tol);
                        snap_line = hit;
                        if let Some(c) = app.project.clip_mut(id) {
                            let rate = c.speed.max(0.01) as f64;
                            let min_start = lo.max(c.start - c.in_point / rate);
                            let new_start = snapped.clamp(min_start, c.end() - 0.05);
                            let delta = new_start - c.start;
                            c.start = new_start;
                            // The source moves faster than the timeline on a
                            // sped-up clip — trim the head in SOURCE seconds.
                            c.in_point += delta * rate;
                            c.duration -= delta;
                        }
                    }
                    Drag::TrimR { id } if id == clip.id => {
                        let (snapped, hit) = EditorState::snap(pt, &targets, snap_tol);
                        snap_line = hit;
                        let next_start = if hi.is_finite() { hi + clip.duration } else { f64::INFINITY };
                        if let Some(c) = app.project.clip_mut(id) {
                            let new_end = snapped.clamp(c.start + 0.05, next_start);
                            c.duration = new_end - c.start;
                        }
                    }
                    Drag::Roll { id, last } => {
                        let applied = app.project.roll(id, pt - last);
                        app.editor.drag = Some(Drag::Roll { id, last: last + applied });
                        app.status = "Roll: moving the cut — the total length stays.".into();
                    }
                    Drag::Slip { id, last } if id == clip.id => {
                        // Dragging RIGHT shows earlier material — the
                        // convention every NLE uses.
                        let applied = app.project.slip(id, -(pt - last));
                        app.editor.drag = Some(Drag::Slip { id, last: last - applied });
                        let ip = app.project.clip(id).map(|c| c.in_point).unwrap_or(0.0);
                        app.status = format!("Slip: source in-point {ip:.2}s");
                    }
                    Drag::Slide { id, last } if id == clip.id => {
                        let applied = app.project.slide(id, pt - last);
                        app.editor.drag = Some(Drag::Slide { id, last: last + applied });
                        app.status = "Slide: neighbours absorb the move.".into();
                    }
                    _ => {}
                }
            }
            if resp.drag_stopped() {
                app.editor.drag = None;
            }
        }
    }

    // Export range: dim everything outside [in, out], mark the edges.
    let (rin, rout) = (app.editor.range_in, app.editor.range_out);
    if rin.is_some() || rout.is_some() {
        let shade = Color32::from_black_alpha(120);
        let body = Rect::from_min_max(
            egui::pos2(full.left(), full.top() + ruler_h),
            full.right_bottom(),
        );
        if let Some(i) = rin {
            let x = t_to_x(i).clamp(full.left(), full.right());
            painter.rect_filled(
                Rect::from_min_max(body.left_top(), egui::pos2(x, body.bottom())),
                0.0,
                shade,
            );
            painter.line_segment(
                [egui::pos2(x, full.top()), egui::pos2(x, full.bottom())],
                Stroke::new(1.5, theme::CYAN),
            );
        }
        if let Some(o) = rout {
            let x = t_to_x(o).clamp(full.left(), full.right());
            painter.rect_filled(
                Rect::from_min_max(egui::pos2(x, body.top()), body.right_bottom()),
                0.0,
                shade,
            );
            painter.line_segment(
                [egui::pos2(x, full.top()), egui::pos2(x, full.bottom())],
                Stroke::new(1.5, theme::CYAN),
            );
        }
    }

    // ── The curve lane ──────────────────────────────────────────────────
    // Full-width keyframe editing aligned with the ruler: the selected
    // clip's track for the Animate panel's parameter, in TIMELINE time.
    if app.editor.show_curve_lane {
        let lane_top = full.top() + ruler_h + 4.0
            + app.project.tracks.len() as f32 * (lane_h + 4.0);
        let lane = Rect::from_min_size(
            egui::pos2(full.left(), lane_top),
            Vec2::new(full.width(), curve_lane_h - 6.0),
        );
        painter.rect_filled(lane, 4.0, theme::VOID_2);
        let param = app.editor.key_param;
        let sel = app.editor.selected.and_then(|id| app.project.clip(id).cloned());
        match sel {
            Some(clip) if clip.key_track(param).is_some() => {
                let (lo, hi) = param.range();
                let keys: Vec<crate::edit::Keyframe> =
                    clip.key_track(param).map(|k| k.to_vec()).unwrap_or_default();
                let to_y = |v: f32| {
                    lane.bottom() - 4.0
                        - ((v - lo) / (hi - lo)).clamp(0.0, 1.0) * (lane.height() - 8.0)
                };
                // The clip's span, tinted, and the evaluated curve across it.
                let (x0, x1) = (t_to_x(clip.start), t_to_x(clip.end()));
                painter.rect_filled(
                    Rect::from_min_max(egui::pos2(x0, lane.top()), egui::pos2(x1, lane.bottom())),
                    0.0,
                    Color32::from_rgba_unmultiplied(60, 180, 200, 14),
                );
                let n = 96;
                let pts: Vec<egui::Pos2> = (0..=n)
                    .map(|i| {
                        let tl = clip.duration * i as f64 / n as f64;
                        let v = crate::edit::eval_keys(&keys, tl).unwrap_or(lo);
                        egui::pos2(t_to_x(clip.start + tl), to_y(v))
                    })
                    .collect();
                painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::CYAN)));
                // Keys: hit-test, drag, add, remove — the panel editor's
                // gestures, on the ruler's clock.
                let resp = ui.interact(lane, ui.id().with("curve_lane"), Sense::click_and_drag());
                let pointer = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
                let mut hover: Option<usize> = None;
                for (i, k) in keys.iter().enumerate() {
                    let pnt = egui::pos2(t_to_x(clip.start + k.t), to_y(k.value));
                    if let Some(m) = pointer {
                        if (m - pnt).length() < 9.0 && hover.is_none() {
                            hover = Some(i);
                        }
                    }
                    let big = hover == Some(i);
                    let r = if big { 5.5 } else { 4.0 };
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(pnt.x, pnt.y - r),
                            egui::pos2(pnt.x + r, pnt.y),
                            egui::pos2(pnt.x, pnt.y + r),
                            egui::pos2(pnt.x - r, pnt.y),
                        ],
                        if big { theme::STAR } else { theme::CYAN },
                        Stroke::new(1.0, theme::VOID),
                    ));
                }
                if resp.drag_started() {
                    if let Some(i) = hover {
                        app.editor.push_undo(&app.project);
                        app.editor.curve_drag = Some(i);
                    }
                }
                if resp.dragged() {
                    if let (Some(i), Some(m)) = (app.editor.curve_drag, resp.interact_pointer_pos()) {
                        let tl = (x_to_t(m.x) - clip.start).clamp(0.0, clip.duration);
                        let v = lo
                            + ((lane.bottom() - 4.0 - m.y) / (lane.height() - 8.0)).clamp(0.0, 1.0)
                                * (hi - lo);
                        if let Some(c) = app.project.clip_mut(clip.id) {
                            if let Some((_, ks)) = c.keys.iter_mut().find(|(q, _)| *q == param) {
                                if let Some(k) = ks.get_mut(i) {
                                    k.t = tl;
                                    k.value = v;
                                }
                                let held = ks.get(i).cloned();
                                ks.sort_by(|a, b| a.t.total_cmp(&b.t));
                                if let Some(h) = held {
                                    app.editor.curve_drag =
                                        ks.iter().position(|k| (k.t - h.t).abs() < 1e-9);
                                }
                            }
                        }
                        app.editor.mark_changed();
                    }
                }
                if resp.drag_stopped() {
                    app.editor.curve_drag = None;
                }
                if resp.double_clicked() {
                    if let Some(m) = resp.interact_pointer_pos() {
                        let tl = (x_to_t(m.x) - clip.start).clamp(0.0, clip.duration);
                        let v = lo
                            + ((lane.bottom() - 4.0 - m.y) / (lane.height() - 8.0)).clamp(0.0, 1.0)
                                * (hi - lo);
                        app.editor.push_undo(&app.project);
                        if let Some(c) = app.project.clip_mut(clip.id) {
                            c.set_key(param, tl, v, crate::edit::Interp::Linear);
                        }
                        app.editor.mark_changed();
                    }
                }
                if resp.secondary_clicked() {
                    if let Some(i) = hover {
                        let kt = keys[i].t;
                        app.editor.push_undo(&app.project);
                        if let Some(c) = app.project.clip_mut(clip.id) {
                            c.clear_key(param, kt);
                        }
                        app.editor.mark_changed();
                    }
                }
                painter.text(
                    egui::pos2(lane.left() + 6.0, lane.top() + 8.0),
                    egui::Align2::LEFT_CENTER,
                    param.name(),
                    egui::FontId::monospace(10.0),
                    Color32::from_gray(130),
                );
            }
            _ => {
                painter.text(
                    lane.center(),
                    egui::Align2::CENTER_CENTER,
                    "select a clip with keyframes (Animate panel picks the parameter)",
                    egui::FontId::proportional(11.0),
                    Color32::from_gray(110),
                );
            }
        }
    }

    // Snap indicator.
    if let Some(st) = snap_line {
        let x = t_to_x(st);
        painter.line_segment(
            [egui::pos2(x, full.top()), egui::pos2(x, full.bottom())],
            Stroke::new(1.0, theme::STAR),
        );
    }

    // Markers: places you flagged to come back to (M).
    for m in &app.project.markers {
        let x = t_to_x(*m);
        if x < full.left() - 8.0 || x > full.right() + 8.0 {
            continue;
        }
        painter.line_segment(
            [egui::pos2(x, full.top()), egui::pos2(x, full.bottom())],
            Stroke::new(1.0, theme::STAR.linear_multiply(0.5)),
        );
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x - 5.0, full.top()),
                egui::pos2(x + 5.0, full.top()),
                egui::pos2(x, full.top() + 8.0),
            ],
            theme::STAR,
            Stroke::NONE,
        ));
    }

    // Playhead: ember line + grab triangle in the ruler.
    let px = t_to_x(app.editor.playhead);
    painter.line_segment(
        [egui::pos2(px, full.top()), egui::pos2(px, full.bottom())],
        Stroke::new(1.5, theme::EMBER),
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(px - 6.0, full.top()),
            egui::pos2(px + 6.0, full.top()),
            egui::pos2(px, full.top() + 10.0),
        ],
        theme::EMBER,
        Stroke::NONE,
    ));
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0);
    let h = (s / 3600.0).floor() as u64;
    let m = ((s % 3600.0) / 60.0).floor() as u64;
    let rem = s - (h * 3600 + m * 60) as f64;
    if h > 0 {
        format!("{h}:{m:02}:{rem:05.2}")
    } else {
        format!("{m:02}:{rem:05.2}")
    }
}

//! The egui UI — player transport, editor timeline, export dialog, shortcuts.
//! Draws entirely from ReelApp state; interactions call back into the
//! Player / Project / export job.

use crate::app::{Mode, ReelApp};
use crate::edit::TrackKind;
use crate::export::{self, AudioMode, Codec, Quality, Resolution};
use crate::theme;
use egui::{Color32, Key, Rect, RichText, Sense, Stroke, Vec2};

pub fn draw(ctx: &egui::Context, app: &mut ReelApp) {
    app.poll_picker();
    app.poll_opening();
    app.poll_captures();
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

    match app.mode {
        Mode::Player => player_view(ctx, app),
        Mode::Editor => editor_view(ctx, app),
    }

    export_window(ctx, app);
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
        right: i.key_pressed(Key::ArrowRight),
        left: i.key_pressed(Key::ArrowLeft),
        shift: i.modifiers.shift,
        step_fwd: i.key_pressed(Key::Period),
        step_back: i.key_pressed(Key::Comma),
        vol_up: i.key_pressed(Key::ArrowUp),
        vol_down: i.key_pressed(Key::ArrowDown),
        mute: i.key_pressed(Key::M),
        looping: i.key_pressed(Key::L),
        fullscreen: i.key_pressed(Key::F) || i.key_pressed(Key::F11),
        escape: i.key_pressed(Key::Escape),
        edit: i.key_pressed(Key::E),
        speed_up: i.key_pressed(Key::CloseBracket),
        speed_down: i.key_pressed(Key::OpenBracket),
        speed_reset: i.key_pressed(Key::Backspace),
        viz: i.key_pressed(Key::V),
    });

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
            player.set_muted(!player.muted);
        }
        if k.looping {
            player.set_looping(!player.looping);
        }
        if k.speed_up {
            player.set_speed(player.speed + 0.25);
        }
        if k.speed_down {
            player.set_speed(player.speed - 0.25);
        }
        if k.speed_reset {
            player.set_speed(1.0);
        }
        if k.viz && player.supports_visualizer() {
            player.set_visualizer(player.visualizer.next());
        }
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
            undo: bool,
            redo: bool,
            save: bool,
        }
        let ek = ctx.input(|i| EdKeys {
            split: i.key_pressed(Key::S) && !i.modifiers.ctrl && !i.modifiers.command,
            delete: i.key_pressed(Key::Delete),
            undo: (i.modifiers.ctrl || i.modifiers.command) && !i.modifiers.shift && i.key_pressed(Key::Z),
            redo: (i.modifiers.ctrl || i.modifiers.command)
                && (i.key_pressed(Key::Y) || (i.modifiers.shift && i.key_pressed(Key::Z))),
            save: (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(Key::S),
        });
        if ek.split {
            app.editor_split();
        }
        if ek.delete {
            app.editor_delete();
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
                for (label, mode) in [
                    ("Full screen", crate::capture::ShotMode::Full),
                    ("Region…", crate::capture::ShotMode::Region),
                    ("Window…", crate::capture::ShotMode::Window),
                ] {
                    if ui.button(label).clicked() {
                        app.take_screenshot(mode);
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

fn editor_view(ctx: &egui::Context, app: &mut ReelApp) {
    // The controls live at the very bottom (added first → outermost), always
    // visible in the editor — this is a workspace, nothing fades here.
    // exact_height + a bounded child: chrome's greedy slider/columns layout
    // must never see unbounded space (same trap as the player overlay).
    egui::TopBottomPanel::bottom("editor_chrome").exact_height(102.0).show(ctx, |ui| {
        let inner = ui.max_rect().shrink2(Vec2::new(8.0, 4.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        chrome(&mut child, app);
        child.label(RichText::new(&app.status).color(theme::CYAN).small());
    });
    egui::SidePanel::left("media").resizable(true).default_width(220.0).show(ctx, |ui| {
        ui.heading("Project");
        ui.label(format!("{} — {}×{} @ {:.0}fps", app.project.name, app.project.width, app.project.height, app.project.fps));
        ui.separator();
        ui.label(RichText::new("Media / Clips").color(theme::CYAN));
        for track in &app.project.tracks {
            for clip in &track.clips {
                let selected = app.editor.selected == Some(clip.id);
                if ui.selectable_label(selected, format!("• {} ({:.1}s)", clip.name, clip.duration)).clicked() {
                    app.editor.selected = Some(clip.id);
                }
            }
        }
        if let Some(clip) = app.editor.selected.and_then(|id| app.project.clip(id)) {
            ui.separator();
            ui.label(RichText::new("Selected clip").color(theme::CYAN));
            ui.label(format!("{}", clip.name));
            ui.label(RichText::new(format!(
                "at {}  ·  {:.2}s long\nsource in-point {}",
                fmt_time(clip.start), clip.duration, fmt_time(clip.in_point)
            )).small().color(egui::Color32::from_gray(150)));
        }
        ui.separator();
        ui.label(RichText::new("S split · Del delete · drag edges to trim\nCtrl+scroll zoom · Ctrl+S save").small().color(egui::Color32::from_gray(120)));
    });
    egui::TopBottomPanel::bottom("timeline_panel").resizable(true).default_height(220.0).show(ctx, |ui| {
        timeline(ui, app);
    });
    egui::CentralPanel::default().show(ctx, |ui| {
        viewport(ui, app);
    });
}

/// The media viewport — aspect-fit the current frame / image / cover art,
/// a ♪ card for pure audio, or the drop hint.
fn viewport(ui: &mut egui::Ui, app: &ReelApp) {
    let avail = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(avail, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::VOID);

    // Native size of whatever is on the texture (frame, art, visualizer, image).
    let dims = app.tex_dims().map(|(w, h)| (w as f32, h as f32));

    if let (Some(id), Some((vw, vh))) = (app.tex_id, dims) {
        if vw > 0.0 && vh > 0.0 {
            let scale = (rect.width() / vw).min(rect.height() / vh);
            let size = Vec2::new(vw * scale, vh * scale);
            let img_rect = Rect::from_center_size(rect.center(), size);
            if app.image.is_some() {
                // Stills can be transparent — show it honestly, viewer-style.
                checkerboard(&painter, img_rect);
            }
            painter.image(id, img_rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
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
}

/// The classic transparency checkerboard, drawn under a still image. Base
/// fill plus alternate cells only, so the rect count stays modest.
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
    let mut goto_player = false;
    let mut open_export = false;
    let mut toggle_fullscreen = false;

    // Row 1: the seek bar, edge to edge (media with a duration only).
    if let Some(player) = app.player.as_mut() {
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

    // Row 2: three clusters.
    ui.columns(3, |cols| {
        cols[0].with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            reel_menu(ui, app);
        });

        // Center: the transport (or the image's identity).
        cols[1].with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
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
                let time_text =
                    format!("{}  /  {}", fmt_time(player.position), fmt_time(player.info.duration));
                let est = 4.0 * 34.0 + time_text.chars().count() as f32 * 8.0 + 24.0;
                ui.add_space(((ui.available_width() - est) / 2.0).max(0.0));
                if ui.button("⏮").on_hover_text("Back to start").clicked() {
                    player.seek(0.0);
                }
                if ui.button("◀").on_hover_text("Frame back (,)").clicked() {
                    player.frame_step(false);
                }
                let label = if player.playing { "⏸" } else { "▶" };
                if ui.button(RichText::new(label).size(18.0)).on_hover_text("Play/pause (Space)").clicked() {
                    player.toggle_play();
                }
                if ui.button("▶").on_hover_text("Frame forward (.)").clicked() {
                    player.frame_step(true);
                }
                ui.label(RichText::new(time_text).monospace());
            }
        });

        cols[2].with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
            if ui
                .button(RichText::new("⬇ Export").color(theme::CYAN))
                .on_hover_text("Convert this file — codec, quality, size")
                .clicked()
            {
                open_export = true;
            }
            if mode == Mode::Player {
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
                let speaker = if player.muted || player.volume <= 0.0 { "🔇" } else { "🔊" };
                if ui.button(speaker).on_hover_text("Mute (M)").clicked() {
                    player.set_muted(!player.muted);
                }
            }

            // Speed.
            let mut speed = player.speed;
            egui::ComboBox::from_id_salt("speed")
                .selected_text(format!("{speed}×"))
                .width(64.0)
                .show_ui(ui, |ui| {
                    for s in [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0] {
                        ui.selectable_value(&mut speed, s, format!("{s}×"));
                    }
                });
            if speed != player.speed {
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
        });
    });

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
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(std::path::Path::new(&source).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(source.clone()))
                    .color(theme::STAR),
            );

            // What are we exporting — the source file, or the edit?
            let segments = app.project.export_segments();
            let cut_len: f64 = segments.iter().map(|(_, _, d)| d).sum();
            let can_timeline = kind != crate::media::MediaKind::Image && !segments.is_empty();
            if can_timeline && app.export.is_none() {
                ui.add_space(4.0);
                let before = app.export_timeline;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut app.export_timeline, false, "Source file");
                    ui.selectable_value(
                        &mut app.export_timeline,
                        true,
                        format!("✂ The edit ({} clip{}, {:.1}s)", segments.len(), if segments.len() == 1 { "" } else { "s" }, cut_len),
                    );
                });
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

                if is_video_out || s.codec.is_image() {
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
            let start = ui.add_sized(
                [ui.available_width(), 28.0],
                egui::Button::new(RichText::new("Start export").color(theme::STAR).strong()),
            );
            if start.clicked() && app.export_timeline {
                match export::start_timeline(&segments, &app.export_out, &app.export_settings) {
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
        if ui.button("💾 Save").on_hover_text("Save .reel project (Ctrl+S)").clicked() {
            app.editor_save();
        }
        if app.editor.dirty {
            ui.label(RichText::new("●").color(theme::EMBER).small());
        }
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
    let lane_h = ((full.height() - ruler_h - 8.0) / app.project.tracks.len().max(1) as f32)
        .clamp(24.0, 46.0);

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
        app.editor.drag = Some(Drag::Playhead);
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
    if bg.drag_stopped() {
        app.editor.drag = None;
    }

    // Lanes + clips (drawn from a snapshot; edits go through clip_mut).
    let mut snap_line: Option<f64> = None;
    let track_data: Vec<(TrackKind, String, Vec<crate::edit::Clip>)> = app
        .project
        .tracks
        .iter()
        .map(|tr| (tr.kind.clone(), tr.name.clone(), tr.clips.clone()))
        .collect();
    for (i, (kind, tname, clips)) in track_data.iter().enumerate() {
        let top = full.top() + ruler_h + 4.0 + i as f32 * (lane_h + 4.0);
        let lane = Rect::from_min_size(egui::pos2(full.left(), top), Vec2::new(full.width(), lane_h));
        painter.rect_filled(lane, 4.0, theme::VOID_2);
        painter.text(
            egui::pos2(lane.left() + 5.0, lane.center().y),
            egui::Align2::LEFT_CENTER,
            tname,
            egui::FontId::monospace(10.0),
            Color32::from_gray(110),
        );

        let base = match kind {
            TrackKind::Video => theme::CYAN,
            TrackKind::Audio => theme::EMBER,
        };
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
            let selected = app.editor.selected == Some(clip.id);
            let fill = if selected { base.linear_multiply(0.55) } else { base.linear_multiply(0.30) };
            painter.rect_filled(cr, 5.0, fill);
            painter.rect_stroke(
                cr,
                5.0,
                Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { theme::STAR } else { base }),
                egui::StrokeKind::Inside,
            );
            if cr.width() > 40.0 {
                painter.text(
                    egui::pos2(cr.left() + 6.0, cr.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{}  {:.1}s", clip.name, clip.duration),
                    egui::FontId::proportional(11.0),
                    theme::STAR,
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
                app.editor.selected = Some(clip.id);
            }
            if resp.drag_started() {
                app.editor.push_undo(&app.project);
                app.editor.drag = Some(match zone {
                    1 => Drag::TrimL { id: clip.id },
                    2 => Drag::TrimR { id: clip.id },
                    _ => Drag::Move {
                        id: clip.id,
                        grab: resp
                            .interact_pointer_pos()
                            .map(|p| x_to_t(p.x) - clip.start)
                            .unwrap_or(0.0),
                    },
                });
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
                        if let Some(c) = app.project.clip_mut(id) {
                            c.start = snapped.clamp(lo, hi.max(lo));
                        }
                    }
                    Drag::TrimL { id } if id == clip.id => {
                        let (snapped, hit) = EditorState::snap(pt, &targets, snap_tol);
                        snap_line = hit;
                        if let Some(c) = app.project.clip_mut(id) {
                            let min_start = lo.max(c.start - c.in_point);
                            let new_start = snapped.clamp(min_start, c.end() - 0.05);
                            let delta = new_start - c.start;
                            c.start = new_start;
                            c.in_point += delta;
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
                    _ => {}
                }
            }
            if resp.drag_stopped() {
                app.editor.drag = None;
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

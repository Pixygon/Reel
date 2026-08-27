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
        app.mode = if app.mode == Mode::Editor { Mode::Player } else { Mode::Editor };
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
    egui::TopBottomPanel::bottom("editor_chrome").show(ctx, |ui| {
        chrome(ui, app);
        ui.label(RichText::new(&app.status).color(theme::CYAN).small());
    });
    egui::SidePanel::left("media").resizable(true).default_width(220.0).show(ctx, |ui| {
        ui.heading("Project");
        ui.label(format!("{} — {}×{} @ {:.0}fps", app.project.name, app.project.width, app.project.height, app.project.fps));
        ui.separator();
        ui.label(RichText::new("Media / Clips").color(theme::CYAN));
        for track in &app.project.tracks {
            for clip in &track.clips {
                ui.label(format!("• {} ({:.1}s)", clip.name, clip.duration));
            }
        }
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
        app.mode = Mode::Editor;
    }
    if goto_player {
        app.mode = Mode::Player;
    }
    if open_export {
        app.export_open = true;
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
                        app.export_out = export::default_output(&source, app.export_settings.codec);
                    }
                }
                return;
            }

            let s = &mut app.export_settings;
            // Keep the codec legal for what's open (kind can change between opens).
            let codecs = Codec::for_kind(kind);
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

                if is_video_out {
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
                app.export_out = export::default_output(&source, s.codec);
            }

            ui.add_space(8.0);
            let start = ui.add_sized(
                [ui.available_width(), 28.0],
                egui::Button::new(RichText::new("Start export").color(theme::STAR).strong()),
            );
            if start.clicked() {
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

/// The NLE timeline — a time ruler, one lane per track, clips as blocks, and a
/// playhead at the current position. v0.1 renders + shows structure; trimming
/// and drag are on the roadmap.
fn timeline(ui: &mut egui::Ui, app: &mut ReelApp) {
    ui.horizontal(|ui| {
        ui.heading("Timeline");
        ui.label(RichText::new(format!("{:.1}s", app.project.duration())).color(theme::CYAN).small());
    });
    let total = app.project.duration().max(10.0);
    let full = ui.available_rect_before_wrap();
    let px_per_s = (full.width() / total as f32).max(2.0);
    let lane_h = 34.0;

    let painter = ui.painter();
    // Ruler ticks every second.
    let mut t = 0.0;
    while t <= total {
        let x = full.left() + t as f32 * px_per_s;
        painter.line_segment([egui::pos2(x, full.top()), egui::pos2(x, full.top() + 8.0)], Stroke::new(1.0, Color32::from_gray(70)));
        t += 1.0;
    }

    for (i, track) in app.project.tracks.iter().enumerate() {
        let top = full.top() + 12.0 + i as f32 * (lane_h + 4.0);
        let lane = Rect::from_min_size(egui::pos2(full.left(), top), Vec2::new(full.width(), lane_h));
        painter.rect_filled(lane, 4.0, theme::VOID_2);
        painter.text(egui::pos2(lane.left() + 4.0, lane.top() + 2.0), egui::Align2::LEFT_TOP,
            &track.name, egui::FontId::monospace(11.0), Color32::from_gray(130));

        let col = match track.kind {
            TrackKind::Video => theme::CYAN.linear_multiply(0.5),
            TrackKind::Audio => theme::EMBER.linear_multiply(0.5),
        };
        for clip in &track.clips {
            let x0 = full.left() + clip.start as f32 * px_per_s;
            let w = (clip.duration as f32 * px_per_s).max(2.0);
            let cr = Rect::from_min_size(egui::pos2(x0, top + 2.0), Vec2::new(w, lane_h - 4.0));
            painter.rect_filled(cr, 4.0, col);
            painter.rect_stroke(cr, 4.0, Stroke::new(1.0, theme::STAR), egui::StrokeKind::Inside);
            painter.text(egui::pos2(cr.left() + 4.0, cr.center().y), egui::Align2::LEFT_CENTER,
                &clip.name, egui::FontId::proportional(11.0), theme::STAR);
        }
    }

    // Playhead — click/drag the timeline to scrub it.
    if let Some(player) = app.player.as_mut() {
        let resp = ui.interact(full, ui.id().with("timeline_scrub"), Sense::click_and_drag());
        if resp.clicked() || resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = (((p.x - full.left()) / px_per_s) as f64).clamp(0.0, total);
                player.seek(t);
            }
        }
        let x = full.left() + player.position as f32 * px_per_s;
        ui.painter().line_segment([egui::pos2(x, full.top()), egui::pos2(x, full.bottom())], Stroke::new(1.5, theme::EMBER));
    }
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

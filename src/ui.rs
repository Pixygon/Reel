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
    dropped_files(ctx, app);
    shortcuts(ctx, app);

    top_bar(ctx, app);
    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&app.status).color(theme::CYAN).small());
        });
    });

    match app.mode {
        Mode::Player => player_view(ctx, app),
        Mode::Editor => editor_view(ctx, app),
    }

    export_window(ctx, app);
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

fn top_bar(ctx: &egui::Context, app: &mut ReelApp) {
    egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("REEL").color(theme::STAR).strong().size(18.0));
            ui.separator();

            if ui.button("Open…").clicked() {
                app.open_picker();
            }
            // Or paste/type a path directly.
            let resp = ui.add(
                egui::TextEdit::singleline(&mut app.open_field)
                    .hint_text("…or paste a path")
                    .desired_width(220.0),
            );
            let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if submit {
                let path = app.open_field.trim().to_string();
                if !path.is_empty() {
                    app.open(&path);
                }
            }

            ui.separator();
            ui.selectable_value(&mut app.mode, Mode::Player, "▶ Player");
            ui.selectable_value(&mut app.mode, Mode::Editor, "✂ Editor");
        });
    });
}

fn player_view(ctx: &egui::Context, app: &mut ReelApp) {
    egui::TopBottomPanel::bottom("transport").show(ctx, |ui| transport(ui, app));
    egui::CentralPanel::default().show(ctx, |ui| {
        viewport(ui, app);
    });
}

fn editor_view(ctx: &egui::Context, app: &mut ReelApp) {
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
        transport(ui, app);
        viewport(ui, app);
    });
}

/// The video viewport — aspect-fit the current frame, or a placeholder.
fn viewport(ui: &mut egui::Ui, app: &ReelApp) {
    let avail = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(avail, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::VOID);

    if let (Some(id), Some(player)) = (app.tex_id, app.player.as_ref()) {
        let (vw, vh) = (player.info.width as f32, player.info.height as f32);
        if vw > 0.0 && vh > 0.0 {
            let scale = (rect.width() / vw).min(rect.height() / vh);
            let size = Vec2::new(vw * scale, vh * scale);
            let img_rect = Rect::from_center_size(rect.center(), size);
            painter.image(id, img_rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "drop a video here",
            egui::FontId::proportional(20.0),
            Color32::from_gray(90),
        );
    }
}

fn transport(ui: &mut egui::Ui, app: &mut ReelApp) {
    let mode = app.mode;
    let mut goto_editor = false;
    let mut open_export = false;
    let mut toggle_fullscreen = false;

    let Some(player) = app.player.as_mut() else {
        ui.horizontal(|ui| ui.label("—"));
        return;
    };

    // Row 1: the seek bar, full width.
    let dur = player.info.duration.max(0.001);
    let mut pos = player.position;
    let slider = egui::Slider::new(&mut pos, 0.0..=dur).show_value(false).trailing_fill(true);
    let resp = ui.add_sized([ui.available_width(), 18.0], slider);
    if resp.drag_stopped() {
        player.seek(pos);
        resp.surrender_focus(); // keep arrow keys on the player, not the slider
    } else if resp.dragged() && player.cheap_seek() {
        player.seek(pos); // live scrub — frame-exact, mpv coalesces the seeks
    }

    // Row 2: controls. Left cluster: transport. Right cluster: modes/tools.
    ui.horizontal(|ui| {
        let label = if player.playing { "⏸" } else { "▶" };
        if ui.button(RichText::new(label).size(18.0)).on_hover_text("Play/pause (Space)").clicked() {
            player.toggle_play();
        }
        if ui.button("⏮").on_hover_text("Back to start").clicked() {
            player.seek(0.0);
        }
        if ui.button("⧏").on_hover_text("Frame back (,)").clicked() {
            player.frame_step(false);
        }
        if ui.button("⧐").on_hover_text("Frame forward (.)").clicked() {
            player.frame_step(true);
        }
        ui.label(
            RichText::new(format!("{}  /  {}", fmt_time(player.position), fmt_time(player.info.duration)))
                .monospace(),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
            }
            if ui.button("⛶").on_hover_text("Fullscreen (F)").clicked() {
                toggle_fullscreen = true;
            }

            // Volume — only when the backend actually produces sound.
            if player.has_audio() {
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
        });
    });

    if goto_editor {
        app.mode = Mode::Editor;
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
    let (source, duration) = match app.player.as_ref() {
        Some(p) => (p.path.clone(), p.info.duration),
        None => {
            app.export_open = false;
            return;
        }
    };

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
            let prev_codec = s.codec;

            egui::Grid::new("export_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Format");
                egui::ComboBox::from_id_salt("codec")
                    .selected_text(s.codec.label())
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for c in Codec::ALL {
                            ui.selectable_value(&mut s.codec, c, c.label());
                        }
                    });
                ui.end_row();

                if s.codec != Codec::Remux {
                    ui.label("Quality");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("quality")
                            .selected_text(s.quality.label())
                            .show_ui(ui, |ui| {
                                for q in [Quality::High, Quality::Balanced, Quality::Small] {
                                    ui.selectable_value(&mut s.quality, q, q.label());
                                }
                                if ui
                                    .selectable_label(matches!(s.quality, Quality::Custom(_)), Quality::Custom(23).label())
                                    .clicked()
                                {
                                    s.quality = Quality::Custom(23);
                                }
                            });
                        if let Quality::Custom(crf) = &mut s.quality {
                            let mut v = *crf as i32;
                            ui.add(egui::Slider::new(&mut v, 10..=50).text("CRF"));
                            *crf = v as u8;
                        }
                    });
                    ui.end_row();

                    ui.label("Resolution");
                    egui::ComboBox::from_id_salt("resolution")
                        .selected_text(s.resolution.label())
                        .show_ui(ui, |ui| {
                            for r in Resolution::ALL {
                                ui.selectable_value(&mut s.resolution, r, r.label());
                            }
                        });
                    ui.end_row();

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
                match export::start(&source, &app.export_out, &app.export_settings, duration) {
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

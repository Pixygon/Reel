//! The egui UI — player transport and the editor timeline. Draws entirely from
//! ReelApp state; interactions call back into the Player / Project.

use crate::app::{Mode, ReelApp};
use crate::edit::TrackKind;
use crate::theme;
use egui::{Color32, Rect, RichText, Sense, Stroke, Vec2};

pub fn draw(ctx: &egui::Context, app: &mut ReelApp) {
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
}

fn top_bar(ctx: &egui::Context, app: &mut ReelApp) {
    egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("REEL").color(theme::STAR).strong().size(18.0));
            ui.separator();

            // Open field (v0.1 — paste/type a path; native picker is on the roadmap).
            let resp = ui.add(
                egui::TextEdit::singleline(&mut app.open_field)
                    .hint_text("path to a video…")
                    .desired_width(280.0),
            );
            let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Open").clicked() || submit {
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
            "no signal",
            egui::FontId::proportional(20.0),
            Color32::from_gray(90),
        );
    }
}

fn transport(ui: &mut egui::Ui, app: &mut ReelApp) {
    let Some(player) = app.player.as_mut() else {
        ui.horizontal(|ui| ui.label("—"));
        return;
    };
    ui.horizontal(|ui| {
        let label = if player.playing { "⏸" } else { "▶" };
        if ui.button(RichText::new(label).size(18.0)).clicked() {
            player.toggle_play();
        }
        if ui.button("⏮").clicked() {
            player.seek(0.0);
        }
        ui.label(format!("{}  /  {}", fmt_time(player.position), fmt_time(player.info.duration)));

        let dur = player.info.duration.max(0.001);
        let mut pos = player.position;
        let slider = egui::Slider::new(&mut pos, 0.0..=dur).show_value(false).trailing_fill(true);
        if ui.add_sized([ui.available_width().max(80.0), 18.0], slider).drag_stopped() {
            player.seek(pos);
        }
    });
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

    // Playhead.
    if let Some(player) = app.player.as_ref() {
        let x = full.left() + player.position as f32 * px_per_s;
        painter.line_segment([egui::pos2(x, full.top()), egui::pos2(x, full.bottom())], Stroke::new(1.5, theme::EMBER));
    }
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0);
    let m = (s / 60.0).floor() as u64;
    let rem = s - (m as f64) * 60.0;
    format!("{:02}:{:05.2}", m, rem)
}

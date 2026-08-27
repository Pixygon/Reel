//! Pixygon design tokens applied to egui — the master-brand voice: void grounds,
//! star-white text, signal cyan, ember red. (Foundation layer of
//! @pixygon/design/tokens.mjs, transcribed for a native egui app.)

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const VOID: Color32 = Color32::from_rgb(0x0A, 0x0B, 0x10); // deepest ground
pub const VOID_2: Color32 = Color32::from_rgb(0x12, 0x14, 0x1C); // panels
pub const VOID_3: Color32 = Color32::from_rgb(0x1B, 0x1E, 0x2A); // raised
pub const STAR: Color32 = Color32::from_rgb(0xEC, 0xF1, 0xFF); // text
pub const CYAN: Color32 = Color32::from_rgb(0x35, 0xD0, 0xE8); // signal
pub const EMBER: Color32 = Color32::from_rgb(0xE8, 0x4B, 0x3B); // alert / record

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.override_text_color = Some(STAR);
    v.panel_fill = VOID;
    v.window_fill = VOID_2;
    v.extreme_bg_color = VOID;
    v.faint_bg_color = VOID_2;

    v.widgets.noninteractive.bg_fill = VOID_2;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, STAR);
    v.widgets.inactive.bg_fill = VOID_3;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, STAR);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x24, 0x2A, 0x3A);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, CYAN);
    v.widgets.active.bg_fill = CYAN;
    v.widgets.active.fg_stroke = Stroke::new(1.0, VOID);
    v.selection.bg_fill = CYAN.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, CYAN);
    v.hyperlink_color = CYAN;

    let r = CornerRadius::same(6);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.corner_radius = r;

    ctx.set_visuals(v);
}

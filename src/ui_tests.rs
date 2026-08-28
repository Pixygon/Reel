//! Headless egui tests. egui runs perfectly well without a window, so layout
//! behaviour that would otherwise need a human with a mouse (dragging a panel
//! edge, for instance) can be driven and asserted here.

#![cfg(test)]

use egui::{Context, Event, PointerButton, Pos2, RawInput, Vec2};

/// Run one frame with the given raw input and report the width the side
/// panel actually laid out with (measured from inside the panel itself).
fn frame(ctx: &Context, input: RawInput) -> (f32, f32) {
    let width = std::rc::Rc::new(std::cell::Cell::new(f32::NAN));
    // Where the central area starts == where the side panel ends: read it
    // INSIDE the frame (available_rect() outside a pass is a debug panic).
    let sep = std::rc::Rc::new(std::cell::Cell::new(f32::NAN));
    let (w, sp) = (width.clone(), sep.clone());
    ctx.run(input, move |ctx| {
        crate::ui::media_panel_frame(ctx, true, |ui| {
            w.set(ui.max_rect().width());
            // Content shaped like Reel's real panel: a heading, a long line
            // of text and a slider — the things that impose a minimum width.
            ui.heading("Project");
            ui.label("demo-cut — 1280×720 @ 30fps, a deliberately long line of text");
            let mut v = 1.0f32;
            ui.add(egui::Slider::new(&mut v, 0.0..=2.0).text("Saturation"));
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            sp.set(ui.max_rect().left());
            ui.label("viewport");
        });
    });
    (width.get(), sep.get())
}

fn pointer_input(pos: Pos2, down: Option<bool>) -> RawInput {
    let mut input = RawInput {
        screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
        ..Default::default()
    };
    input.events.push(Event::PointerMoved(pos));
    if let Some(pressed) = down {
        input.events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        });
    }
    input
}

/// Dragging the side panel's edge must actually resize it — and the new width
/// must survive the following frames. (It didn't: content with a large
/// minimum width silently clamped the panel back, so the drag "worked" until
/// you let go.)
#[test]
fn side_panel_resize_sticks() {
    let ctx = Context::default();
    // Settle the animation (show_animated eases the width in).
    for _ in 0..30 {
        frame(&ctx, pointer_input(Pos2::new(600.0, 400.0), None));
    }
    let (start_w, sep) = frame(&ctx, pointer_input(Pos2::new(600.0, 400.0), None));
    assert!(start_w > 100.0, "panel should start with a sensible width, got {start_w}");

    // Grab the separator, drag left (narrower), release.
    let target = 190.0_f32;
    frame(&ctx, pointer_input(Pos2::new(sep, 400.0), Some(true)));
    frame(&ctx, pointer_input(Pos2::new(target, 400.0), None));
    frame(&ctx, pointer_input(Pos2::new(target, 400.0), None));
    let (while_dragging, sep_after) = frame(&ctx, pointer_input(Pos2::new(target, 400.0), Some(false)));
    assert!(
        (sep_after - target).abs() < 12.0,
        "drag should have moved the panel edge to ~{target}, it sits at {sep_after}"
    );
    assert!(
        while_dragging < start_w - 20.0,
        "panel should actually be narrower now: {start_w} → {while_dragging}"
    );

    // …and it must STAY there over subsequent frames with no input.
    let mut after = while_dragging;
    for _ in 0..5 {
        after = frame(&ctx, pointer_input(Pos2::new(600.0, 400.0), None)).0;
    }
    assert!(
        (after - while_dragging).abs() < 2.0,
        "panel snapped back after release: {while_dragging} → {after}"
    );
}

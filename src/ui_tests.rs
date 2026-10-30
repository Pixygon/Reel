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

/// One frame of a lone adjust slider; returns where it sits.
fn slider_frame(ctx: &Context, input: RawInput, value: &mut f32) -> Pos2 {
    let mut at = Pos2::ZERO;
    ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.spacing_mut().slider_width = 200.0;
            at = ui.next_widget_position();
            crate::ui::adjust_slider(ui, "Exposure", value, -5.0..=5.0, 2);
        });
    });
    at
}

#[test]
fn double_click_sends_a_photo_slider_home() {
    let ctx = Context::default();
    let mut value = 2.5f32;
    let start = slider_frame(&ctx, pointer_input(Pos2::ZERO, None), &mut value);
    let on = Pos2::new(start.x + 60.0, start.y + 2.0);
    for _ in 0..2 {
        for down in [true, false] {
            slider_frame(&ctx, pointer_input(on, Some(down)), &mut value);
        }
    }
    slider_frame(&ctx, pointer_input(on, None), &mut value);
    assert_eq!(value, 0.0, "a double-click resets the slider");
}

/// One frame of the real UI; returns where the text `label` was drawn.
fn app_frame(ctx: &Context, app: &mut crate::app::ReelApp, input: RawInput, label: &str) -> Option<egui::Rect> {
    let out = ctx.run(input, |ctx| crate::ui::draw(ctx, app));
    fn find(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == label => Some(t.visual_bounding_rect()),
            egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
            _ => None,
        }
    }
    out.shapes.iter().find_map(|c| find(&c.shape, label))
}

/// The whole loop through the real UI: a still opens with the panel, a
/// slider moves, the picture re-renders and the settings reach the document
/// while the original's bytes stay as they were.
#[test]
fn opening_a_still_gives_a_working_adjust_panel() {
    let dir = std::env::temp_dir().join(format!("reel-panel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.png");
    image::RgbaImage::from_pixel(64, 48, image::Rgba([90, 120, 150, 255])).save(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let path_s = path.to_string_lossy().into_owned();

    let ctx = Context::default();
    let mut app = crate::app::ReelApp::new();
    app.open(&path_s);
    assert!(app.photo.is_some(), "a raster still opens with the panel");
    assert!(app.image.is_some());

    let pointer = |pos, down| pointer_input(pos, down);
    let mut label = None;
    for _ in 0..4 {
        label = app_frame(&ctx, &mut app, RawInput::default(), "Exposure");
    }
    let label = label.expect("the panel draws an Exposure slider");
    let on = Pos2::new(label.left() - 90.0, label.center().y); // on the rail
    for down in [Some(true), Some(false)] {
        app_frame(&ctx, &mut app, pointer(on, down), "Exposure");
    }
    let moved = app.photo.as_ref().unwrap().adjust.exposure;
    assert!(moved > 0.5, "the click reached the slider: {moved}");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.photo.as_ref().unwrap().busy() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app_frame(&ctx, &mut app, RawInput::default(), "Exposure");
    }
    let photo = app.photo.as_ref().unwrap();
    assert_ne!(photo.developed, photo.base.data, "the preview re-rendered");
    app.flush_photo();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(crate::photo::load_settings(&path_s).exposure, moved, "settings saved themselves");
    assert_eq!(std::fs::read(&path).unwrap(), before, "the original is untouched");

    // Every door to Export reaches the developed render, not the source
    // conversion that would drop the adjustments.
    app.open_export();
    assert!(app.photo.as_ref().unwrap().export_open && !app.export_open);
    let _ = std::fs::remove_dir_all(&dir);
}

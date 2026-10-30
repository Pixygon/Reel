//! The picture stage (blur, split, glitch, rotation, build, shake) drawn
//! through BOTH real pipelines — the preview's VideoPass and the render's
//! Compositor — and compared pixel by pixel, at the tolerance the effect
//! tests have always used.

use super::compositor::{Compositor, FORMAT};
use super::{Layer, Scene};
use crate::effects::Effects;
use crate::video_pass::{VideoDraw, VideoPass};
use egui_wgpu::CallbackTrait;

const W: u32 = 192;
const H: u32 = 108;
const AVERAGE_LIMIT: f32 = 3.0;

fn comp() -> Option<Compositor> {
    Compositor::headless().ok()
}

/// A soft gradient with hard-edged blocks, so geometry errors can't hide.
fn picture() -> Vec<u8> {
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let block = (x / 24 + y / 18) % 2 == 0;
            let (r, g, b) = ((x * 255 / W) as u8, (y * 255 / H) as u8, if block { 230 } else { 30 });
            px.extend([r, g, b, 255]);
        }
    }
    px
}

fn render(c: &Compositor, view: &wgpu::TextureView, fx: Effects) -> Vec<u8> {
    let target = c.target(W, H);
    let layers = vec![Layer {
        view: view.clone(),
        rect: [0.0, 0.0, 1.0, 1.0],
        uv: [0.0, 0.0, 1.0, 1.0],
        opacity: 1.0,
        effects: fx,
        use_src_alpha: true,
        lut: None,
        plugin: None,
    }];
    c.render(&Scene { layers }, &target);
    c.read_back(&target)
}

fn preview(c: &Compositor, view: &wgpu::TextureView, fx: Effects) -> Vec<u8> {
    preview_with(c, view, fx, false)
}

fn preview_with(c: &Compositor, view: &wgpu::TextureView, fx: Effects, use_src_alpha: bool) -> Vec<u8> {
    let target = c.target(W, H);
    let tv = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut res = egui_wgpu::CallbackResources::default();
    res.insert(VideoPass::new(&c.device, &c.queue, FORMAT));
    let draw = VideoDraw {
        view: view.clone(),
        tint: [1.0; 4],
        use_src_alpha,
        effects: Some(fx),
        lut: None,
        plugin: None,
        aspect: W as f32 / H as f32,
        bounds: [1.0, 1.0],
    };
    let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [W, H], pixels_per_point: 1.0 };
    let mut enc = c.device.create_command_encoder(&Default::default());
    draw.prepare(&c.device, &c.queue, &screen, &mut enc, &mut res);
    {
        let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &tv,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        let mut pass = pass.forget_lifetime();
        let whole = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(W as f32, H as f32));
        let info = egui::PaintCallbackInfo {
            viewport: whole,
            clip_rect: whole,
            pixels_per_point: 1.0,
            screen_size_px: [W, H],
        };
        draw.paint(info, &mut pass, &res);
    }
    c.queue.submit(Some(enc.finish()));
    c.read_back(&target)
}

fn mean_diff(a: &[u8], b: &[u8]) -> f32 {
    let sum: u64 = a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum();
    sum as f32 / a.len() as f32
}

/// A masked frame through the whole of each route: the chain, then the real
/// VideoPass (preview) or the compositor (render), the clip's grade on top.
#[test]
fn a_masked_frame_is_the_same_through_the_preview_pass_and_the_render() {
    use crate::engine::render::local_chain;
    use crate::masks::{LocalMask, Plane};
    let Some(c) = comp() else { return };
    let mut plane = Plane::rect(W, H, [0.0, 0.0], [96.0, H as f32]);
    plane.feather(6.0);
    let mut m = LocalMask::new("Left", plane).unwrap();
    m.adjust.exposure = 1.6;
    m.adjust.wb_temp = 0.3;
    let masks = [m];
    let clip = Effects { contrast: 1.2, ..Default::default() };
    let chain = |alpha| {
        local_chain(&c, &masks, (W, H), |_| [0.0, 0.0, 1.0, 1.0], 0, alpha, &[], &[]).unwrap()
    };
    let worst = |a: &[u8], b: &[u8]| {
        a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap()
    };

    // Video: the viewport feeds the chain an opaque frame.
    let src = c.upload(&picture(), W, H);
    let render_chain = chain(true);
    let exported = render(&c, &render_chain.run(&c, &src), clip);
    let previewed = preview(&c, &chain(false).run(&c, &src), clip);
    assert!(worst(&exported, &previewed) <= 2, "video: {}", worst(&exported, &previewed));
    let plain = render(&c, &src, clip);
    let red = |b: &[u8], x: u32| b[((54 * W + x) * 4) as usize] as i32;
    assert!(red(&exported, 40) > red(&plain, 40) + 10, "inside changes");
    assert!((red(&exported, 150) - red(&plain, 150)).abs() <= 1, "outside holds");

    // A still with real alpha: premultiplied by the compositor on the way in.
    // No clip grade here: video.wgsl grades a still after premultiplying, so
    // semi-transparent pixels under a clip grade differ with or without masks.
    let clip = Effects::default();
    let mut rgba = picture();
    rgba.chunks_exact_mut(4).step_by(3).for_each(|p| p[3] = 128);
    let src = c.upload(&rgba, W, H);
    let exported = render(&c, &render_chain.run(&c, &src), clip);
    let out = c.target(W, H);
    c.premultiply(&render_chain.run(&c, &src), &out);
    let view = out.create_view(&wgpu::TextureViewDescriptor::default());
    let previewed = preview_with(&c, &view, clip, true);
    assert!(worst(&exported, &previewed) <= 2, "still: {}", worst(&exported, &previewed));
}

#[test]
fn preview_and_render_draw_the_same_picture() {
    let Some(c) = comp() else { return };
    let view = c.upload(&picture(), W, H);
    let plain = render(&c, &view, Effects::default());
    let cases = [
        ("blur", Effects { blur: 60.0, ..Default::default() }),
        ("rgb split", Effects { rgb_split: 30.0, ..Default::default() }),
        ("glitch", Effects { rgb_split: 12.0, glitch: Some(7), ..Default::default() }.at_time(0.4)),
        ("shake", Effects { shake: 40.0, ..Default::default() }.at_time(0.13)),
        ("rotate", Effects { rotate_deg: 23.0, ..Default::default() }),
        ("build", Effects { build_tiles: 16, ..Default::default() }.at_time(0.5)),
    ];
    for (name, fx) in cases {
        let (a, b) = (preview(&c, &view, fx), render(&c, &view, fx));
        let diff = mean_diff(&a, &b);
        assert!(diff <= AVERAGE_LIMIT, "{name}: preview and render differ by {diff:.2}");
        // And the effect did something: this is not two identical no-ops.
        assert!(mean_diff(&plain, &b) > 2.0, "{name} changed nothing");
    }
}

#[test]
fn identity_stage_leaves_the_picture_alone() {
    let Some(c) = comp() else { return };
    let view = c.upload(&picture(), W, H);
    let out = render(&c, &view, Effects::default());
    assert!(mean_diff(&out, &picture()) <= 1.0);
}

#[test]
fn shake_moves_the_picture_and_never_shows_an_edge() {
    let Some(c) = comp() else { return };
    let grey: Vec<u8> = [200, 200, 200, 255].iter().copied().cycle().take((W * H * 4) as usize).collect();
    let view = c.upload(&grey, W, H);
    let shake = Effects { shake: 60.0, ..Default::default() };
    for i in 0..12 {
        let fx = shake.at_time(i as f64 * 0.041);
        assert!(fx.zoom > 1.0);
        assert!(fx.pan_x.abs() <= 1.0 && fx.pan_y.abs() <= 1.0);
        let px = render(&c, &view, fx);
        assert!(px.iter().step_by(4).all(|v| *v > 190), "an edge showed at step {i}");
    }
    let (a, b) = (shake.at_time(0.05), shake.at_time(0.11));
    assert!((a.pan_x - b.pan_x).abs() + (a.pan_y - b.pan_y).abs() > 0.01);
}

#[test]
fn a_rotated_stamp_lands_at_its_angle_and_keeps_its_size() {
    let Some(c) = comp() else { return };
    // A white 2:1 bar on a transparent ground, turned 90 degrees.
    let view = c.upload(&vec![255; 4 * 40 * 20], 40, 20);
    let target = c.target(200, 200);
    let layers = vec![Layer {
        view,
        rect: [0.3, 0.45, 0.7, 0.55],
        uv: [0.0, 0.0, 1.0, 1.0],
        opacity: 1.0,
        effects: Effects { rotate_deg: 90.0, ..Default::default() },
        use_src_alpha: true,
        lut: None,
        plugin: None,
    }];
    c.render(&Scene { layers }, &target);
    let px = c.read_back(&target);
    let at = |x: usize, y: usize| px[4 * (y * 200 + x)];
    assert!(at(100, 70) > 200, "the turned bar should reach up the frame");
    assert!(at(70, 100) < 40, "and no longer lie across it");
}

#[test]
fn a_build_starts_empty_passes_through_tiles_and_lands_whole() {
    let Some(c) = comp() else { return };
    let view = c.upload(&vec![255; (W * H * 4) as usize], W, H);
    let fx = Effects { build_tiles: 25, build_seconds: 2.0, ..Default::default() };
    let lit = |p: &[u8]| p.chunks(4).filter(|px| px[0] > 128).count() as f32 / (W * H) as f32;
    assert!(lit(&render(&c, &view, fx.at_time(0.0))) < 0.01, "nothing before the build");
    let mid = lit(&render(&c, &view, fx.at_time(1.0)));
    assert!(mid > 0.1 && mid < 0.9, "mid-build coverage {mid}");
    assert!(lit(&render(&c, &view, fx.at_time(2.0))) > 0.99, "whole after the build");
    assert_eq!(fx.build_grid(16.0 / 9.0).0 * fx.build_grid(16.0 / 9.0).1 >= 25, true);
}


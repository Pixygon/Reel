import fs from 'fs';
function sub(s,a,b){ if(!s.includes(a)) throw new Error('missing: '+a); return s.replace(a,b); }
let f='src/engine/picture_tests.rs', s=fs.readFileSync(f,'utf8');
s=sub(s,`fn preview(c: &Compositor, view: &wgpu::TextureView, fx: Effects) -> Vec<u8> {
    let target`,`fn preview(c: &Compositor, view: &wgpu::TextureView, fx: Effects) -> Vec<u8> {
    preview_with(c, view, fx, false)
}

fn preview_with(c: &Compositor, view: &wgpu::TextureView, fx: Effects, use_src_alpha: bool) -> Vec<u8> {
    let target`);
s=sub(s,`        tint: [1.0; 4],
        use_src_alpha: false,`,`        tint: [1.0; 4],
        use_src_alpha,`);
s=sub(s,`#[test]
fn preview_and_render_draw_the_same_picture() {`,`/// A masked frame through the whole of each route: the chain, then the real
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
fn preview_and_render_draw_the_same_picture() {`);
fs.writeFileSync(f,s);

// Reel's video shader. A single oversized triangle covers the callback's
// viewport (egui scissors it to the exact rect), so there are no vertex
// buffers to manage.
//
// The fragment stage is deliberately the place where picture quality lives:
// alpha is forced opaque (mpv's software target leaves a padding byte there),
// the colour is premultiplied to match egui's blending, and the tint uniform
// is the hook compositing/fades will use.

struct Uniforms {
    tint: vec4<f32>,
    // x: 1 = honour the texture's alpha (still images, which really can be
    //    transparent); 0 = force opaque (video — mpv's software target leaves
    //    a padding byte in the alpha channel, and fixing that on the CPU
    //    costs a full pass over every pixel of every frame).
    // yzw: reserved for effect parameters. Kept as one vec4 so the Rust and
    //      WGSL layouts can't drift (vec3 padding rules are a trap).
    params: vec4<f32>,
};

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VsOut {
    // (-1,-1), (3,-1), (-1,3) — one triangle covering clip space.
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) & 1) * 4.0 - 1.0;
    var out: VsOut;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    // Texture origin is top-left; clip space is bottom-up.
    out.uv = vec2<f32>((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5);
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(frame_tex, frame_sampler, in.uv);
    // Images arrive premultiplied (see app::sync_frame); video is opaque.
    let a_src = mix(1.0, s.a, u.params.x);
    let rgb = s.rgb * u.tint.rgb;
    // Premultiplied output: egui's pipeline blends the UI over this.
    return vec4<f32>(rgb * u.tint.a, a_src * u.tint.a);
}

// Reel's video shader. A single oversized triangle covers the callback's
// viewport (egui scissors it to the exact rect), so there are no vertex
// buffers to manage.
//
// The fragment stage is deliberately the place where picture quality lives:
// alpha is forced opaque (mpv's software target leaves a padding byte there),
// the colour is premultiplied to match egui's blending, and the tint uniform
// is the hook compositing/fades will use.

// FIELD ORDER MUST MATCH `Uniforms` in video_pass.rs, exactly. (Getting this
// wrong is silent: the shader reads one field's bytes as another's. It once
// made exposure read 0 and the whole picture went black.)
struct Uniforms {
    tint: vec4<f32>,
    // x: 1 = honour the texture's alpha (still images, which really can be
    //    transparent); 0 = force opaque (video — mpv's software target leaves
    //    a padding byte in the alpha channel, and fixing that on the CPU
    //    costs a full pass over every pixel of every frame).
    // y: 1 = apply the effects block below; 0 = show the picture untouched.
    // zw: reserved. Kept as one vec4 so the Rust and WGSL layouts can't drift
    //     (vec3 padding rules are a trap).
    params: vec4<f32>,
    // Per-clip effects: x = exposure, y = contrast, z = saturation, w unused.
    // These MUST match effects::Effects::apply_reference — that formula is
    // also what the ffmpeg render performs, and a test holds the two together.
    fx: vec4<f32>,
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

// The frame texture is sRGB, so sampling returns LINEAR values — but the
// effects formula (and ffmpeg's filters) are defined on sRGB-encoded values.
// Convert around the adjustment so preview and render agree.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Mirrors effects::Effects::apply_reference exactly: exposure, then contrast
// about mid-grey, then saturation about Rec.709 luma.
fn apply_effects(rgb: vec3<f32>) -> vec3<f32> {
    let exposure = u.fx.x;
    let contrast = u.fx.y;
    let saturation = u.fx.z;
    var c = rgb * exposure;
    c = (c - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5);
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = vec3<f32>(luma) + (c - vec3<f32>(luma)) * saturation;
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(frame_tex, frame_sampler, in.uv);
    // Images arrive premultiplied (see app::sync_frame); video is opaque.
    let a_src = mix(1.0, s.a, u.params.x);
    var rgb = s.rgb;
    if (u.params.y > 0.5) {
        rgb = srgb_to_linear(apply_effects(linear_to_srgb(rgb)));
    }
    rgb = rgb * u.tint.rgb;
    // Premultiplied output: egui's pipeline blends the UI over this.
    return vec4<f32>(rgb * u.tint.a, a_src * u.tint.a);
}

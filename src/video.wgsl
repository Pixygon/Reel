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
    // Reframe: x = zoom, y = pan_x, z = pan_y, w unused. Mirrors
    // Effects::reframe_filter — pan ±1 lands the window exactly on an edge.
    reframe: vec4<f32>,
    // Per-clip effects: x = exposure, y = contrast, z = saturation, w unused.
    // These MUST match effects::Effects::apply_reference — that formula is
    // also what the ffmpeg render performs, and a test holds the two together.
    fx: vec4<f32>,
    // Chroma key: rgb = key colour (sRGB), w = similarity. Softness lives in
    // params.z; params.w = 1 turns keying on.
    key: vec4<f32>,
    // Power window; see mask_factor.
    mask: vec4<f32>,
    mask2: vec4<f32>,
};
// The grade-limiting window: how much of the LUT + trims applies at uv `q`.
// mask = cx, cy, half-w, half-h; mask2 = feather, invert, shape (1=rect),
// enable.
fn mask_factor(q: vec2<f32>) -> f32 {
    if (u.mask2.w < 0.5) {
        return 1.0;
    }
    let f = max(u.mask2.x, 0.001);
    var m: f32;
    if (u.mask2.z > 0.5) {
        // Rectangle: distance beyond the box edge, feathered.
        let d = abs(q - u.mask.xy) - u.mask.zw;
        let outside = max(d.x, d.y);
        m = 1.0 - smoothstep(0.0, f, outside);
    } else {
        // Ellipse: normalised radial distance 1.0 at the rim.
        let n = (q - u.mask.xy) / max(u.mask.zw, vec2<f32>(0.001));
        let r = length(n);
        m = 1.0 - smoothstep(1.0, 1.0 + f / max(min(u.mask.z, u.mask.w), 0.001), r);
    }
    if (u.mask2.y > 0.5) {
        m = 1.0 - m;
    }
    return m;
}


// Distance between two colours in a luma/chroma space — chroma weighted up,
// because a green screen varies in brightness far more than in hue.
fn key_distance(a: vec3<f32>, b: vec3<f32>) -> f32 {
    let ya = dot(a, vec3<f32>(0.299, 0.587, 0.114));
    let yb = dot(b, vec3<f32>(0.299, 0.587, 0.114));
    let ca = vec2<f32>(a.b - ya, a.r - ya);
    let cb = vec2<f32>(b.b - yb, b.r - yb);
    return length(ca - cb) * 1.6 + abs(ya - yb) * 0.4;
}

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;
// The clip's 3D LUT (identity when none); reframe.w = 1 enables it.
@group(0) @binding(3) var lut_tex: texture_3d<f32>;

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
    // Reframe first: sample a zoomed, panned window of the picture.
    let z = max(u.reframe.x, 1.0);
    let uv = (in.uv - vec2<f32>(0.5)) / z
        + vec2<f32>(0.5)
        + vec2<f32>(u.reframe.y, u.reframe.z) * (1.0 - 1.0 / z) * 0.5;
    let s = textureSample(frame_tex, frame_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    // Images arrive premultiplied (see app::sync_frame); video is opaque.
    var a_src = mix(1.0, s.a, u.params.x);
    var rgb = s.rgb;
    if (u.params.w > 0.5) {
        // Chroma key on sRGB-encoded values (the key colour is picked there).
        let enc = linear_to_srgb(rgb);
        let d = key_distance(enc, u.key.rgb);
        let alpha = smoothstep(u.key.w, u.key.w + max(u.params.z, 0.001), d);
        // Despill: pull the keyed channel down toward the others so kept
        // edges lose the green cast.
        let dominant = max(max(u.key.r, u.key.g), u.key.b);
        var spilled = enc;
        if (u.key.g >= dominant) {
            spilled.g = min(enc.g, max(enc.r, enc.b) + 0.08);
        } else if (u.key.b >= dominant) {
            spilled.b = min(enc.b, max(enc.r, enc.g) + 0.08);
        } else {
            spilled.r = min(enc.r, max(enc.g, enc.b) + 0.08);
        }
        rgb = srgb_to_linear(mix(enc, spilled, 1.0 - alpha * alpha));
        a_src = a_src * alpha;
    }
    let graded_from = rgb;
    if (u.reframe.w > 0.5) {
        let enc = linear_to_srgb(rgb);
        let n = f32(textureDimensions(lut_tex).x);
        let coord = enc * (n - 1.0) / n + 0.5 / n;
        rgb = srgb_to_linear(textureSampleLevel(lut_tex, frame_sampler, coord, 0.0).rgb);
    }
    if (u.params.y > 0.5) {
        rgb = srgb_to_linear(apply_effects(linear_to_srgb(rgb)));
    }
    // The power window: grade only where the mask says.
    rgb = mix(graded_from, rgb, mask_factor(in.uv));
    rgb = rgb * u.tint.rgb;
    // Premultiplied output: egui's pipeline blends the UI over this.
    return vec4<f32>(rgb * u.tint.a, a_src * u.tint.a);
}

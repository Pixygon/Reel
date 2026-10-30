// The picture stage: rotation, tile build, blur, RGB split, glitch. ONE text,
// spliced in front of BOTH video.wgsl (preview) and engine/compose.wgsl
// (render) — so "the same maths in preview and render" is a property of the
// build, not a promise. `Effects::picture_uniforms` fills the three vec4s.

const BLUR_TAPS: i32 = 64;
const GOLDEN_ANGLE: f32 = 2.3999632;

// Arithmetic, not sin(): identical on every GPU.
fn hash1(v: f32) -> f32 {
    var x = fract(v * 0.1031);
    x = x * (x + 33.33);
    x = x * (x + x);
    return fract(x);
}

// Free rotation about the picture's centre (clockwise, y down). rot = cos,
// sin, picture aspect, enable. Returns the unrotated uv + edge coverage, so
// an angled edge is antialiased rather than a staircase.
fn place_uv(uv: vec2<f32>, rot: vec4<f32>, fw: vec2<f32>) -> vec3<f32> {
    if (rot.w < 0.5) {
        return vec3<f32>(uv, 1.0);
    }
    let a = rot.z;
    let p = (uv - vec2<f32>(0.5)) * vec2<f32>(a, 1.0);
    let q = vec2<f32>(rot.x * p.x + rot.y * p.y, rot.x * p.y - rot.y * p.x);
    let out = vec2<f32>(q.x / a, q.y) + vec2<f32>(0.5);
    let e = min(out, vec2<f32>(1.0) - out);
    let f = max(fw, vec2<f32>(0.00001));
    return vec3<f32>(out, clamp(min(e.x / f.x, e.y / f.y) + 0.5, 0.0, 1.0));
}

// The logo build: each tile grows into its own cell, in a shuffled order.
// b = cols, rows, progress 0..1, seed. cols = 0 turns it off.
fn build_uv(uv: vec2<f32>, b: vec4<f32>) -> vec3<f32> {
    if (b.x < 0.5) {
        return vec3<f32>(uv, 1.0);
    }
    let grid = b.xy;
    let cell = floor(clamp(uv, vec2<f32>(0.0), vec2<f32>(0.99999)) * grid);
    let local = uv * grid - cell;
    let span = 0.35;
    let order = hash1(cell.x + cell.y * b.x + 1.0 + b.w);
    let t = clamp((b.z - order * (1.0 - span)) / span, 0.0, 1.0);
    let s = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
    if (s <= 0.0) {
        return vec3<f32>(uv, 0.0);
    }
    let q = (local - vec2<f32>(0.5)) / s + vec2<f32>(0.5);
    if (q.x < 0.0 || q.y < 0.0 || q.x > 1.0 || q.y > 1.0) {
        return vec3<f32>(uv, 0.0);
    }
    return vec3<f32>((cell + q) / grid, 1.0);
}

fn weighted(c: vec4<f32>, by_alpha: bool) -> vec4<f32> {
    if (by_alpha) {
        return vec4<f32>(c.rgb * c.a, c.a);
    }
    return c;
}

// One sample of the picture, blurred when r (the Gaussian sigma, in uv of the
// picture height) is set: a Vogel disc out to 2 sigma, Gaussian weighted.
fn tap(t: texture_2d<f32>, s: sampler, uv: vec2<f32>, r: f32, aspect: f32, by_alpha: bool) -> vec4<f32> {
    let lo = vec2<f32>(0.0);
    let hi = vec2<f32>(1.0);
    if (r <= 0.0) {
        return weighted(textureSampleLevel(t, s, clamp(uv, lo, hi), 0.0), by_alpha);
    }
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = 0; i < BLUR_TAPS; i++) {
        let f = (f32(i) + 0.5) / f32(BLUR_TAPS);
        let rad = sqrt(f) * 2.0;
        let ang = f32(i) * GOLDEN_ANGLE;
        let o = vec2<f32>(cos(ang) / aspect, sin(ang)) * rad * r;
        let w = exp(-0.5 * rad * rad);
        sum += weighted(textureSampleLevel(t, s, clamp(uv + o, lo, hi), 0.0), by_alpha) * w;
        total += w;
    }
    return sum / total;
}

// The picture at uv. m = blur sigma, split, glitch phase, tear strength (uv).
// `by_alpha` = straight-alpha input (the compositor's stills); blur and split
// then work premultiplied, so a transparent edge can't bleed dark.
fn sample_picture(t: texture_2d<f32>, s: sampler, uv: vec2<f32>, m: vec4<f32>, aspect: f32, by_alpha: bool) -> vec4<f32> {
    var at = uv;
    var d = m.y;
    if (m.w > 0.0) {
        // Horizontal tears: about one band in four, re-rolled each step.
        let band = floor(uv.y * 18.0);
        if (hash1(band + m.z) > 0.72) {
            at.x += (hash1(band * 1.7 + m.z + 3.1) - 0.5) * 2.0 * m.w;
        }
        d *= 0.5 + hash1(m.z + 7.3);
    }
    var out: vec4<f32>;
    if (d > 0.0) {
        let dx = vec2<f32>(d / aspect, 0.0);
        let r = tap(t, s, at + dx, m.x, aspect, by_alpha);
        let c = tap(t, s, at, m.x, aspect, by_alpha);
        let b = tap(t, s, at - dx, m.x, aspect, by_alpha);
        out = vec4<f32>(r.r, c.g, b.b, max(max(r.a, c.a), b.a));
    } else {
        out = tap(t, s, at, m.x, aspect, by_alpha);
    }
    if (by_alpha && out.a > 0.0) {
        return vec4<f32>(out.rgb / out.a, out.a);
    }
    return out;
}


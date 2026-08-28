// The compositor's layer shader. One draw = one layer, placed as a quad at
// its rect, effected, and blended (premultiplied) over what's already in the
// target. Derived from video.wgsl — the effects block is the SAME formula
// (effects::apply_reference), which is what keeps preview, compositor and
// the old ffmpeg path telling one story.

// FIELD ORDER MUST MATCH `Uniforms` in compositor.rs, exactly.
struct Uniforms {
    // Placement: min x, min y, max x, max y in 0..1 output fractions
    // (y down, matching texture space).
    rect: vec4<f32>,
    // x: honour src alpha (stills) / force opaque (video frames);
    // y: apply effects; z: layer opacity; w reserved.
    params: vec4<f32>,
    // zoom, pan_x, pan_y, unused.
    reframe: vec4<f32>,
    // exposure, contrast, saturation, unused.
    fx: vec4<f32>,
};

@group(0) @binding(0) var layer_tex: texture_2d<f32>;
@group(0) @binding(1) var layer_sampler: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Two triangles covering the layer's rect. Vertex order: (0,0)(1,0)(0,1),
// (1,0)(1,1)(0,1) in rect-local space.
@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let c = corners[idx];
    // rect is in texture-style space (y down); clip space is y up.
    let fx = mix(u.rect.x, u.rect.z, c.x);
    let fy = mix(u.rect.y, u.rect.w, c.y);
    var out: VsOut;
    out.pos = vec4<f32>(fx * 2.0 - 1.0, 1.0 - fy * 2.0, 0.0, 1.0);
    out.uv = c;
    return out;
}

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

// Mirrors effects::Effects::apply_reference exactly.
fn apply_effects(rgb: vec3<f32>) -> vec3<f32> {
    var c = rgb * u.fx.x;
    c = (c - vec3<f32>(0.5)) * u.fx.y + vec3<f32>(0.5);
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = vec3<f32>(luma) + (c - vec3<f32>(luma)) * u.fx.z;
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Reframe: sample a zoomed, panned window of the layer.
    let z = max(u.reframe.x, 1.0);
    let uv = (in.uv - vec2<f32>(0.5)) / z
        + vec2<f32>(0.5)
        + vec2<f32>(u.reframe.y, u.reframe.z) * (1.0 - 1.0 / z) * 0.5;
    let s = textureSample(layer_tex, layer_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    let a_src = mix(1.0, s.a, u.params.x);
    var rgb = s.rgb;
    if (u.params.y > 0.5) {
        rgb = srgb_to_linear(apply_effects(linear_to_srgb(rgb)));
    }
    let a = a_src * u.params.z;
    // Premultiplied out; a fade therefore fades to whatever is beneath —
    // black for the base layer, the base picture for overlays.
    return vec4<f32>(rgb * a, a);
}

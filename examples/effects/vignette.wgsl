//! name: Vignette
//! param: strength 0..1 = 0.6
//! param: radius 0.2..1.5 = 0.75
fn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32> {
    let d = distance(uv, vec2<f32>(0.5, 0.5)) / max(p.y, 0.01);
    let dark = smoothstep(0.6, 1.2, d) * p.x;
    return rgb * (1.0 - dark);
}

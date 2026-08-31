//! name: Posterize
//! param: levels 2..16 = 5
fn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32> {
    let n = max(floor(p.x), 2.0);
    return floor(rgb * n) / (n - 1.0);
}

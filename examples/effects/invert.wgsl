//! name: Invert
fn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32> {
    return vec3<f32>(1.0) - rgb;
}

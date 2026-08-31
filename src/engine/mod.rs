//! The engine — Reel's own compositor and frame-server renderer.
//!
//! This is the Phase-1 seam from the roadmap: a scene of layers rendered by
//! our GPU pipeline into a texture. The editor preview and the export both
//! build their pictures here, which is what turns "the preview never lies"
//! from a test suite into a property.
//!
//! A `Scene` is deliberately dumb data: which textures, placed where, with
//! which effects, at what opacity. Everything clever — what is on screen at
//! time T, keyframe evaluation, transition overlap — happens in the code
//! that *builds* scenes, so it is shared by preview and render and testable
//! without a GPU.

pub mod compositor;
pub mod render;
pub mod sources;

/// One layer of a composed frame.
pub struct Layer {
    /// The picture, as a GPU texture view (RGBA, sRGB view).
    pub view: wgpu::TextureView,
    /// Where the layer lands in the output, in 0..1 fractions of the frame
    /// (min x, min y, max x, max y). Fractions are the house rule: the same
    /// scene renders identically at any output size.
    pub rect: [f32; 4],
    /// Which window of the layer's own texture is shown (uv min/max).
    /// [0,0,1,1] = all of it; wipes crop this together with `rect` so the
    /// picture is revealed, not squashed.
    pub uv: [f32; 4],
    /// Multiplied into the layer: fades and crossfades ride here.
    pub opacity: f32,
    /// Colour adjustments + reframe, applied exactly as the preview shader
    /// always has (`effects::apply_reference`).
    pub effects: crate::effects::Effects,
    /// Honour the texture's alpha (stills) or force opaque (video frames,
    /// where the decoder leaves a padding byte in the alpha channel).
    pub use_src_alpha: bool,
    /// A 3D LUT texture on the compositor's device, when the clip grades
    /// through one (`Effects.lut` resolved via the project's table).
    pub lut: Option<wgpu::TextureView>,
    /// The clip's effect plugin (WGSL), when one is bound and loads.
    pub plugin: Option<std::sync::Arc<crate::plugins::Plugin>>,
}

/// A frame to be composed: layers back-to-front over opaque black.
pub struct Scene {
    pub layers: Vec<Layer>,
}

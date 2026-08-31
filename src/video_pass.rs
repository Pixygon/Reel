//! The video render pass — Reel's own wgpu pipeline for drawing the picture,
//! instead of handing the frame to egui as a generic image.
//!
//! Why this exists (and why it's worth a pipeline of our own):
//!   * **Alpha.** mpv's software target writes a padding byte where alpha
//!     lives; the shader forces opacity, so no CPU pass over every pixel.
//!   * **Colour.** The frame is linearised here rather than trusting an sRGB
//!     texture view — the place where colour management and tone-mapping go.
//!   * **Compositing.** This is the seam effects, transitions and multi-track
//!     blending plug into: more inputs, more uniforms, same pass.
//!
//! It runs as an egui paint callback, so it draws inside egui's render pass
//! at exactly the rect the viewport laid out, correctly ordered with the UI
//! chrome that sits on top of it.

use egui_wgpu::{CallbackTrait, ScreenDescriptor};
use wgpu::util::DeviceExt;

/// Per-draw uniforms. Mirrors `Uniforms` in video.wgsl — two vec4s, so the
/// two layouts cannot drift (WGSL aligns vec3 to 16 bytes; a naive f32+vec3
/// tail is 48 bytes in the shader and 32 in Rust, which the validation layer
/// rightly rejects).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// Multiplied into the sampled colour (fades, opacity — compositing hook).
    tint: [f32; 4],
    /// x = use the texture's alpha (1.0) or force opaque (0.0);
    /// y = apply the effects block (1.0) or skip it (0.0); zw reserved.
    params: [f32; 4],
    /// zoom, pan_x, pan_y, unused — see Effects::reframe_filter.
    reframe: [f32; 4],
    /// exposure, contrast, saturation, unused — see effects::Effects.
    fx: [f32; 4],
    /// Chroma key: r, g, b of the key colour; w = similarity. Softness rides
    /// in params.z; params.w = 1 enables keying.
    key: [f32; 4],
    /// Power window: cx, cy, half-w, half-h.
    mask: [f32; 4],
    /// feather, invert, shape (1 = rect), enable.
    mask2: [f32; 4],
    /// x = flip horizontal, y = flip vertical; zw reserved.
    flip: [f32; 4],
    /// Effect-plugin parameters. Appended LAST — field order is
    /// load-bearing on both sides of the FFI.
    plug: [f32; 4],
}

pub struct VideoPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    identity_lut: wgpu::TextureView,
    /// For plugin pipeline variants: the layout + target the base was
    /// built with, and a cache keyed by the plugin's path+mtime hash.
    pipeline_layout: wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    variants: std::sync::Mutex<std::collections::HashMap<u64, Option<wgpu::RenderPipeline>>>,
}

impl VideoPass {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, target: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("reel-video-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("video.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("reel-video-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("reel-video-pl"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("reel-video-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target,
                    // Premultiplied — matches egui's own blending so the
                    // chrome composites over the video correctly.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("reel-video-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let identity_lut = crate::lut::to_texture(&crate::lut::identity(), device, queue);
        Self {
            pipeline,
            layout,
            sampler,
            identity_lut,
            pipeline_layout,
            target_format: target,
            variants: Default::default(),
        }
    }
}

/// One video draw: which texture, where, and how it's tinted.
pub struct VideoDraw {
    pub view: wgpu::TextureView,
    pub tint: [f32; 4],
    /// Still images can be genuinely transparent; video cannot.
    pub use_src_alpha: bool,
    /// The active clip's colour adjustments, previewed live. `None` = show
    /// the picture untouched.
    pub effects: Option<crate::effects::Effects>,
    /// The clip's LUT, already resolved to a texture on this device.
    pub lut: Option<wgpu::TextureView>,
    /// The clip's effect plugin (WGSL), when one is bound and loads.
    pub plugin: Option<std::sync::Arc<crate::plugins::Plugin>>,
}

/// Bind groups built during `prepare`, consumed in `paint` — a QUEUE, not a
/// slot: egui's CallbackResources is a type-keyed map, so with two video
/// draws in one frame (the picture plus a crossfade's incoming layer) a
/// single slot gets overwritten by whichever prepared last, and both paints
/// draw the same layer. egui prepares callbacks in paint order, so a FIFO
/// pairs them back up.
#[derive(Default)]
struct Prepared {
    queue: std::sync::Mutex<std::collections::VecDeque<(wgpu::BindGroup, Option<u64>)>>,
}

impl CallbackTrait for VideoDraw {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(pass) = resources.get::<VideoPass>() else { return Vec::new() };
        let ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("reel-video-ubo"),
            contents: bytemuck::bytes_of(&Uniforms {
                tint: self.tint,
                params: [
                    if self.use_src_alpha { 1.0 } else { 0.0 },
                    if self.effects.is_some_and(|e| e.has_colour()) { 1.0 } else { 0.0 },
                    self.effects.map(|e| e.key_softness).unwrap_or(0.0),
                    if self.effects.is_some_and(|e| e.key_color.is_some()) { 1.0 } else { 0.0 },
                ],
                reframe: self
                    .effects
                    .map(|e| [e.zoom, e.pan_x, e.pan_y, 0.0])
                    .map(|mut r| {
                        r[3] = if self.lut.is_some() { 1.0 } else { 0.0 };
                        r
                    })
                    .unwrap_or([1.0, 0.0, 0.0, 0.0]),
                fx: self
                    .effects
                    .map(|e| [e.exposure, e.contrast, e.saturation, 0.0])
                    .unwrap_or([1.0, 1.0, 1.0, 0.0]),
                key: self
                    .effects
                    .and_then(|e| e.key_color.map(|c| [c[0], c[1], c[2], e.key_similarity]))
                    .unwrap_or([0.0, 0.0, 0.0, 0.0]),
            mask: self.effects
                .and_then(|e| e.mask)
                .map(|m| [m.cx, m.cy, m.w, m.h])
                .unwrap_or([0.0; 4]),
            mask2: self.effects
                .and_then(|e| e.mask)
                .map(|m| [
                    m.feather,
                    if m.invert { 1.0 } else { 0.0 },
                    if m.shape == crate::effects::MaskShape::Rect { 1.0 } else { 0.0 },
                    1.0,
                ])
                .unwrap_or([0.0; 4]),
            flip: self
                .effects
                .map(|e| [
                    if e.flip_h { 1.0 } else { 0.0 },
                    if e.flip_v { 1.0 } else { 0.0 },
                    0.0,
                    0.0,
                ])
                .unwrap_or([0.0; 4]),
            plug: self.effects.map(|e| e.plugin_params).unwrap_or([0.0; 4]),
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reel-video-bg"),
            layout: &pass.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&self.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&pass.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: ubo.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(
                        self.lut.as_ref().unwrap_or(&pass.identity_lut),
                    ),
                },
            ],
        });
        // Plugin pipelines compile lazily, once per (file, mtime) — a
        // broken plugin caches None and the base pipeline draws instead.
        let plugin_key = self.plugin.as_ref().map(|p| {
            let mut variants = pass.variants.lock().unwrap();
            variants.entry(p.key).or_insert_with(|| {
                crate::plugins::build_variant(
                    device,
                    include_str!("video.wgsl"),
                    p,
                    "reel-video-plugin",
                    &pass.pipeline_layout,
                    pass.target_format,
                    wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                )
            });
            p.key
        });
        if resources.get::<Prepared>().is_none() {
            resources.insert(Prepared::default());
        }
        let prepared = resources.get::<Prepared>().unwrap();
        prepared.queue.lock().unwrap().push_back((bind_group, plugin_key));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let (Some(pass), Some(prepared)) =
            (resources.get::<VideoPass>(), resources.get::<Prepared>())
        else {
            return;
        };
        let Some((bind_group, plugin_key)) = prepared.queue.lock().unwrap().pop_front() else {
            return;
        };
        let variants = pass.variants.lock().unwrap();
        let pipeline = plugin_key
            .and_then(|k| variants.get(&k))
            .and_then(|v| v.as_ref())
            .unwrap_or(&pass.pipeline);
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &bind_group, &[]);
        // Three vertices, no buffers: the vertex shader builds the triangle
        // that covers the callback's viewport.
        render_pass.draw(0..3, 0..1);
    }
}

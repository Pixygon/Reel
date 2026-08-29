//! The compositor: scenes in, pixels out.
//!
//! Owns a wgpu device of its own when running headless (export, tests) or
//! borrows the app's device when compositing for the preview. One pipeline
//! draws one layer as a placed quad; a scene is drawn back-to-front with
//! premultiplied blending over opaque black — the same blend the preview has
//! always used, so a fade here looks exactly like a fade there.

use super::{Layer, Scene};
use anyhow::{anyhow, Result};
use wgpu::util::DeviceExt;

/// Mirrors `Uniforms` in compose.wgsl — field order is load-bearing.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// Placement: min x, min y, max x, max y in 0..1 output fractions.
    rect: [f32; 4],
    /// x = use src alpha, y = apply effects, z = opacity, w reserved.
    params: [f32; 4],
    /// zoom, pan_x, pan_y, unused.
    reframe: [f32; 4],
    /// exposure, contrast, saturation, unused.
    fx: [f32; 4],
    /// Chroma key: rgb + similarity (softness in params[3]; enable in fx[3]).
    key: [f32; 4],
    /// The uv window of the layer shown across `rect` — wipes crop both.
    uvr: [f32; 4],
}

pub struct Compositor {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Bound for every layer without a LUT, so the shader samples freely.
    identity_lut: wgpu::TextureView,
}

/// The output format. sRGB so the shader samples linear and writes linear,
/// with the hardware doing the encode — identical to the preview surface.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

impl Compositor {
    /// A compositor with its own headless device — for export and tests.
    /// Fails cleanly on machines with no GPU adapter; callers fall back to
    /// the ffmpeg-graph renderer.
    pub fn headless() -> Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| anyhow!("no GPU adapter for the compositor"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("reel-compositor"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))?;
        Ok(Self::with_device(device, queue))
    }

    pub fn with_device(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("reel-compose-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("compose.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("reel-compose-bgl"),
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
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("reel-compose-pl"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("reel-compose-pipeline"),
            layout: Some(&pl),
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
                    format: FORMAT,
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
            label: Some("reel-compose-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let identity_lut = crate::lut::to_texture(&crate::lut::identity(), &device, &queue);
        Self { device, queue, pipeline, layout, sampler, identity_lut }
    }

    /// Upload a lattice for use on this compositor's device.
    pub fn lut_texture(&self, lut: &crate::lut::Lut) -> wgpu::TextureView {
        crate::lut::to_texture(lut, &self.device, &self.queue)
    }

    /// Upload straight-alpha RGBA pixels as a compositor input texture.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn upload(&self, rgba: &[u8], w: u32, h: u32) -> wgpu::TextureView {
        let tex = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("reel-compose-input"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            rgba,
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// A texture a scene can be rendered into (and read back from).
    pub fn target(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("reel-compose-target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    /// Render `scene` into `target`: layers back-to-front over opaque black.
    pub fn render(&self, scene: &Scene, target: &wgpu::Texture) {
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        // Bind groups first — building them inside the pass fights the borrow
        // checker and gains nothing.
        let binds: Vec<wgpu::BindGroup> = scene
            .layers
            .iter()
            .map(|l| {
                let ubo = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("reel-compose-ubo"),
                    contents: bytemuck::bytes_of(&Self::uniforms(l)),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("reel-compose-bg"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&l.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry { binding: 2, resource: ubo.as_entire_binding() },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(
                                l.lut.as_ref().unwrap_or(&self.identity_lut),
                            ),
                        },
                    ],
                })
            })
            .collect();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("reel-compose") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("reel-compose-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            for bg in &binds {
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
    }

    fn uniforms(l: &Layer) -> Uniforms {
        Uniforms {
            rect: l.rect,
            params: [
                if l.use_src_alpha { 1.0 } else { 0.0 },
                if l.effects.has_colour() { 1.0 } else { 0.0 },
                l.opacity.clamp(0.0, 1.0),
                l.effects.key_softness,
            ],
            reframe: [
                l.effects.zoom,
                l.effects.pan_x,
                l.effects.pan_y,
                if l.lut.is_some() { 1.0 } else { 0.0 },
            ],
            fx: [
                l.effects.exposure,
                l.effects.contrast,
                l.effects.saturation,
                if l.effects.key_color.is_some() { 1.0 } else { 0.0 },
            ],
            key: l
                .effects
                .key_color
                .map(|c| [c[0], c[1], c[2], l.effects.key_similarity])
                .unwrap_or([0.0; 4]),
            uvr: l.uv,
        }
    }

    /// Read a rendered target back as straight RGBA rows — the export path.
    /// Handles wgpu's 256-byte row-pitch requirement.
    pub fn read_back(&self, target: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (target.width(), target.height());
        let bpr = (w * 4).div_ceil(256) * 256; // padded bytes per row
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("reel-compose-readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("reel-readback") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::Maintain::Wait);
        let _ = rx.recv();
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let s = (row * bpr) as usize;
            out.extend_from_slice(&data[s..s + (w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::Effects;

    fn comp() -> Option<Compositor> {
        match Compositor::headless() {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("no GPU adapter — skipping compositor test ({e})");
                None
            }
        }
    }

    fn solid(c: &Compositor, rgba: [u8; 4], w: u32, h: u32) -> wgpu::TextureView {
        let px: Vec<u8> = rgba.iter().copied().cycle().take((w * h * 4) as usize).collect();
        c.upload(&px, w, h)
    }

    /// The foundation claim: a layer lands exactly where its rect says, at
    /// the size it says, and nowhere else.
    #[test]
    fn a_layer_lands_exactly_where_its_rect_says() {
        let Some(c) = comp() else { return };
        let scene = Scene {
            layers: vec![Layer {
                view: solid(&c, [255, 0, 0, 255], 8, 8),
                rect: [0.25, 0.25, 0.75, 0.75],
                uv: [0.0, 0.0, 1.0, 1.0],
                opacity: 1.0,
                effects: Effects::default(),
                use_src_alpha: false,
                lut: None,
            }],
        };
        let target = c.target(200, 100);
        c.render(&scene, &target);
        let px = c.read_back(&target);
        let at = |x: u32, y: u32| {
            let i = ((y * 200 + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };
        assert_eq!(at(100, 50), [255, 0, 0], "centre is the layer");
        assert_eq!(at(10, 50), [0, 0, 0], "left of the rect is background");
        assert_eq!(at(190, 50), [0, 0, 0], "right of the rect is background");
        assert_eq!(at(100, 10), [0, 0, 0], "above the rect is background");
        // The edge is where 0.25 says it is (±1 px of filtering).
        assert_eq!(at(52, 50), [255, 0, 0], "just inside the left edge");
        assert_eq!(at(47, 50), [0, 0, 0], "just outside the left edge");
    }

    /// Layers stack back-to-front, and opacity blends toward what's beneath.
    #[test]
    fn layers_stack_and_opacity_blends() {
        let Some(c) = comp() else { return };
        let scene = Scene {
            layers: vec![
                Layer {
                    view: solid(&c, [0, 0, 255, 255], 4, 4),
                    rect: [0.0, 0.0, 1.0, 1.0],
                    uv: [0.0, 0.0, 1.0, 1.0],
                    opacity: 1.0,
                    effects: Effects::default(),
                    use_src_alpha: false,
                    lut: None,
                },
                Layer {
                    view: solid(&c, [255, 0, 0, 255], 4, 4),
                    rect: [0.5, 0.0, 1.0, 1.0],
                    uv: [0.0, 0.0, 1.0, 1.0],
                    opacity: 0.5,
                    effects: Effects::default(),
                    use_src_alpha: false,
                    lut: None,
                },
            ],
        };
        let target = c.target(100, 50);
        c.render(&scene, &target);
        let px = c.read_back(&target);
        let at = |x: u32, y: u32| {
            let i = ((y * 100 + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };
        assert_eq!(at(20, 25), [0, 0, 255], "left half is the base layer");
        let mixed = at(80, 25);
        // 50% red over blue, blended in LINEAR light then sRGB-encoded:
        // each channel is linear 0.5 → encoded ≈ 188.
        assert!(
            mixed[0] > 170 && mixed[0] < 205 && mixed[2] > 170 && mixed[2] < 205,
            "half-opacity red over blue should meet in linear light, got {mixed:?}"
        );
    }

    /// The LUT sampled on the GPU must match the CPU reference lattice walk
    /// — that reference is also what the parity story hangs on.
    #[test]
    fn the_gpu_lut_matches_the_reference_lattice() {
        let Some(c) = comp() else { return };
        // A channel-rotating, slightly-dimming LUT — obviously not identity.
        let mut text = String::from("LUT_3D_SIZE 4\n");
        let n = 3.0f32;
        for b in 0..4 {
            for g in 0..4 {
                for r in 0..4 {
                    let (rf, gf, bf) = (r as f32 / n, g as f32 / n, b as f32 / n);
                    text.push_str(&format!("{} {} {}\n", bf * 0.9, rf * 0.9, gf * 0.9));
                }
            }
        }
        let lut = crate::lut::parse_cube(&text).unwrap();
        let input = [200u8, 120, 40];
        let scene = Scene {
            layers: vec![Layer {
                view: solid(&c, [input[0], input[1], input[2], 255], 4, 4),
                rect: [0.0, 0.0, 1.0, 1.0],
                uv: [0.0, 0.0, 1.0, 1.0],
                opacity: 1.0,
                effects: Effects { lut: Some(0), ..Default::default() },
                use_src_alpha: false,
                lut: Some(c.lut_texture(&lut)),
            }],
        };
        let target = c.target(16, 16);
        c.render(&scene, &target);
        let px = c.read_back(&target);
        let got = [px[4 * (8 * 16 + 8)], px[4 * (8 * 16 + 8) + 1], px[4 * (8 * 16 + 8) + 2]];
        let srgb = [input[0] as f32 / 255.0, input[1] as f32 / 255.0, input[2] as f32 / 255.0];
        let want = crate::lut::apply_reference(&lut, srgb).map(|v| (v * 255.0).round() as i32);
        for ch in 0..3 {
            assert!(
                (got[ch] as i32 - want[ch]).abs() <= 4,
                "channel {ch}: GPU {got:?} vs reference {want:?}"
            );
        }
    }

    /// The effects formula on the GPU is the same one ffmpeg renders and the
    /// same one the preview has always shown — checked against the reference
    /// implementation directly.
    #[test]
    fn compositor_effects_match_the_reference_formula() {
        let Some(c) = comp() else { return };
        let cases = [
            Effects { exposure: 1.4, ..Default::default() },
            Effects { contrast: 1.6, ..Default::default() },
            Effects { saturation: 0.3, ..Default::default() },
            Effects { exposure: 0.8, contrast: 1.3, saturation: 1.7, ..Default::default() },
        ];
        for fx in cases {
            let input = [180u8, 90, 45];
            let scene = Scene {
                layers: vec![Layer {
                    view: solid(&c, [input[0], input[1], input[2], 255], 4, 4),
                    rect: [0.0, 0.0, 1.0, 1.0],
                    uv: [0.0, 0.0, 1.0, 1.0],
                    opacity: 1.0,
                    effects: fx,
                    use_src_alpha: false,
                    lut: None,
                }],
            };
            let target = c.target(16, 16);
            c.render(&scene, &target);
            let px = c.read_back(&target);
            let got = [px[4 * (8 * 16 + 8)], px[4 * (8 * 16 + 8) + 1], px[4 * (8 * 16 + 8) + 2]];
            let srgb = [input[0] as f32 / 255.0, input[1] as f32 / 255.0, input[2] as f32 / 255.0];
            let want = fx.apply_reference(srgb).map(|v| (v * 255.0).round() as i32);
            for ch in 0..3 {
                assert!(
                    (got[ch] as i32 - want[ch]).abs() <= 3,
                    "channel {ch}: compositor {got:?} vs reference {want:?} for {fx:?}"
                );
            }
        }
    }
}

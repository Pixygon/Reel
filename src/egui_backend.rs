//! egui integration via egui-wgpu (modelled on Infinite's egui_backend).
//! egui draws the entire Reel UI, including the video frame (registered as a
//! native texture), so there is no separate 3D render pipeline.

use crate::gpu::Gpu;
use winit::window::Window;

pub struct EguiPrepared {
    pub paint_jobs: Vec<egui::ClippedPrimitive>,
    pub screen_desc: egui_wgpu::ScreenDescriptor,
    pub free_textures: Vec<egui::TextureId>,
}

pub struct EguiBackend {
    pub state: egui_winit::State,
    pub renderer: egui_wgpu::Renderer,
    pub ctx: egui::Context,
}

impl EguiBackend {
    pub fn new(gpu: &Gpu, window: &Window) -> Self {
        let ctx = egui::Context::default();
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let renderer = egui_wgpu::Renderer::new(&gpu.device, gpu.surface_format, None, 1, false);
        Self { state, renderer, ctx }
    }

    pub fn handle_event(
        &mut self,
        window: &Window,
        event: &winit::event::WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    pub fn begin_frame(&mut self, window: &Window) {
        let raw_input = self.state.take_egui_input(window);
        self.ctx.begin_pass(raw_input);
    }

    pub fn end_frame(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        window: &Window,
    ) -> EguiPrepared {
        let full_output = self.ctx.end_pass();
        self.state
            .handle_platform_output(window, full_output.platform_output);

        let ppp = self.ctx.pixels_per_point();
        let paint_jobs = self.ctx.tessellate(full_output.shapes, ppp);
        let screen_desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gpu.size.0, gpu.size.1],
            pixels_per_point: ppp,
        };

        for (id, delta) in &full_output.textures_delta.set {
            self.renderer.update_texture(&gpu.device, &gpu.queue, *id, delta);
        }
        self.renderer
            .update_buffers(&gpu.device, &gpu.queue, encoder, &paint_jobs, &screen_desc);

        EguiPrepared {
            paint_jobs,
            screen_desc,
            free_textures: full_output.textures_delta.free,
        }
    }

    pub fn render(&self, render_pass: &mut wgpu::RenderPass<'_>, prepared: &EguiPrepared) {
        // egui-wgpu 0.31 wants RenderPass<'static>; the pass is created, used
        // and dropped inside one function so this transmute is sound. (Same
        // pattern Infinite uses.)
        let static_pass: &mut wgpu::RenderPass<'static> =
            unsafe { std::mem::transmute(render_pass) };
        self.renderer
            .render(static_pass, &prepared.paint_jobs, &prepared.screen_desc);
    }

    pub fn post_render(&mut self, prepared: EguiPrepared) {
        for id in &prepared.free_textures {
            self.renderer.free_texture(id);
        }
    }

    pub fn register_texture(
        &mut self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
    ) -> egui::TextureId {
        self.renderer
            .register_native_texture(device, view, wgpu::FilterMode::Linear)
    }

    pub fn update_registered(
        &mut self,
        id: egui::TextureId,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
    ) {
        self.renderer
            .update_egui_texture_from_wgpu_texture(device, view, wgpu::FilterMode::Linear, id);
    }
}

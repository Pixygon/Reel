//! Reel — a cross-platform video player and editor. Linux-first.
//! Native stack: winit + wgpu + egui (the Pixygon app stack); playback via
//! libmpv when present, ffmpeg-subprocess fallback otherwise.
//!
//! v0.1 goal: open and play a video with frame-accurate scrubbing, and show the
//! editor timeline. The road to "better than VLC / Premiere-class" is in
//! ROADMAP.md; this is the running foundation it builds on.

mod app;
mod edit;
mod egui_backend;
mod gpu;
mod theme;
mod ui;
mod video;

use app::ReelApp;
use egui_backend::EguiBackend;
use gpu::Gpu;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct Reel {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    egui: Option<EguiBackend>,
    app: ReelApp,
    initial_open: Option<String>,
}

impl ApplicationHandler for Reel {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // already initialised (e.g. after suspend/resume)
        }
        let attrs = Window::default_attributes()
            .with_title("Reel")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 760.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let gpu = pollster::block_on(Gpu::new(window.clone())).expect("init gpu");
        let egui = EguiBackend::new(&gpu, &window);
        theme::apply(&egui.ctx);

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.egui = Some(egui);

        if let Some(path) = self.initial_open.take() {
            self.app.open(&path);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else { return };
        let Some(gpu) = self.gpu.as_mut() else { return };
        let Some(egui) = self.egui.as_mut() else { return };

        let response = egui.handle_event(&window, &event);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => gpu.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                // 1) advance playback and push any new frame to the GPU + egui.
                self.app.sync_frame(gpu, egui);

                // 2) build the UI.
                egui.begin_frame(&window);
                let ctx = egui.ctx.clone();
                ui::draw(&ctx, &mut self.app);

                // 3) render: clear the swapchain and draw egui over it.
                let frame = match gpu.surface.get_current_texture() {
                    Ok(f) => f,
                    Err(_) => {
                        gpu.resize(gpu.size.0, gpu.size.1);
                        return;
                    }
                };
                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder = gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("reel-frame") });

                let prepared = egui.end_frame(gpu, &mut encoder, &window);
                {
                    let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("reel-egui"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.039, g: 0.043, b: 0.063, a: 1.0 }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    egui.render(&mut rpass, &prepared);
                }
                gpu.queue.submit(Some(encoder.finish()));
                frame.present();
                egui.post_render(prepared);
            }
            _ => {}
        }

        if response.repaint {
            window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Keep frames flowing while playing (plus a short grace after open/seek
        // so async frames land); otherwise sit idle until an event (egui
        // repaint request) wakes us — no busy loop when paused.
        if self.app.wants_redraw() {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // Uses the system ffmpeg on PATH (no bundled/download path).

    let initial_open = std::env::args().nth(1);
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut reel = Reel {
        window: None,
        gpu: None,
        egui: None,
        app: ReelApp::new(),
        initial_open,
    };
    event_loop.run_app(&mut reel).expect("run");
}

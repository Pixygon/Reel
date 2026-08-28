//! Reel — a cross-platform video player and editor. Linux-first.
//! Native stack: winit + wgpu + egui (the Pixygon app stack); playback via
//! libmpv when present, ffmpeg-subprocess fallback otherwise.
//!
//! v0.1 goal: open and play a video with frame-accurate scrubbing, and show the
//! editor timeline. The road to "better than VLC / Premiere-class" is in
//! ROADMAP.md; this is the running foundation it builds on.

mod app;
mod captions;
mod capture;
mod cli;
mod edit;
mod effects;
mod egui_backend;
mod export;
mod gpu;
#[cfg(target_os = "linux")]
mod integration;
mod media;
mod perf;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "linux")]
mod runtime;
#[cfg(target_os = "linux")]
mod tray;

/// Events that wake the winit loop from outside (tray menu clicks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserEvent {
    Show,
    Shot(capture::ShotMode),
    ToggleRecord,
    Quit,
}
mod theme;
mod titles;
pub mod ui;
mod ui_tests;
mod thumbs;
mod video;
mod waveform;
mod video_pass;

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
    /// Last title applied to the window (avoids redundant set_title calls).
    window_title: String,
    #[cfg(target_os = "linux")]
    tray: Option<ksni::Handle<tray::ReelTray>>,
    /// Last recording state mirrored into the tray label.
    tray_recording: bool,
}

impl Reel {
    /// Keep the tray's Start/Stop label in sync with reality.
    fn sync_tray(&mut self) {
        let rec = self.app.recorder.is_some();
        if rec != self.tray_recording {
            self.tray_recording = rec;
            #[cfg(target_os = "linux")]
            if let Some(handle) = &self.tray {
                tray::set_recording(handle, rec);
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for Reel {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // already initialised (e.g. after suspend/resume)
        }
        let attrs = Window::default_attributes()
            .with_title("Reel")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 760.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        timing!("window created");
        let gpu = pollster::block_on(Gpu::new(window.clone())).expect("init gpu");
        timing!("gpu ready");
        let egui = EguiBackend::new(&gpu, &window);
        theme::apply(&egui.ctx);

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.egui = Some(egui);

        if let Some(path) = self.initial_open.take() {
            self.app.open(&path);
            timing!("media opened");
        }
        self.app.init_integration();
        timing!("integration done");
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
                perf::note_redraw();
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

                // Apply window-level state the UI requested (fullscreen, title).
                let want_fs = self.app.fullscreen;
                if want_fs != window.fullscreen().is_some() {
                    window.set_fullscreen(
                        want_fs.then_some(winit::window::Fullscreen::Borderless(None)),
                    );
                }
                if self.app.window_title != self.window_title {
                    self.window_title = self.app.window_title.clone();
                    window.set_title(&self.window_title);
                }
                self.sync_tray();
                if self.app.quit_requested {
                    event_loop.exit();
                }
            }
            _ => {}
        }

        if response.repaint {
            window.request_redraw();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Show => {
                if let Some(w) = &self.window {
                    w.set_visible(true);
                    w.focus_window();
                }
            }
            UserEvent::Shot(mode) => self.app.take_screenshot(mode),
            UserEvent::ToggleRecord => {
                self.app.toggle_record();
                self.sync_tray();
            }
            UserEvent::Quit => event_loop.exit(),
        }
        if let Some(w) = &self.window {
            w.request_redraw();
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

/// Process start, for cold-open timing logs.
static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
pub fn t0() -> std::time::Instant {
    *T0.get_or_init(std::time::Instant::now)
}
#[macro_export]
macro_rules! timing {
    ($($arg:tt)*) => {
        log::info!("[t+{:>5.0}ms] {}", crate::t0().elapsed().as_secs_f64() * 1000.0, format!($($arg)*));
    };
}

fn main() {
    t0();
    perf::init();
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,zbus=warn,tracing=warn"),
    )
    .init();

    // No ffmpeg on this machine? Fetch a static build in the background so
    // export/convert and the fallback decoder Just Work (mainly Windows).
    if !ffmpeg_sidecar::command::ffmpeg_is_installed() {
        std::thread::spawn(|| match ffmpeg_sidecar::download::auto_download() {
            Ok(()) => log::info!("downloaded a private ffmpeg build"),
            Err(e) => log::warn!("ffmpeg missing and auto-download failed: {e}"),
        });
    }

    // Reel is two programs sharing one binary: a window you drop media into,
    // and a headless tool an agent can drive. The first argument decides —
    // and an argument that names a real file always means "open this",
    // so a video called `render.mp4` never trips the command parser.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(arg) = argv.first().map(String::as_str) {
        match arg {
            "--help" | "-h" | "help" => {
                cli::print_help();
                return;
            }
            "--version" | "-V" => {
                println!("reel {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ if cli::is_command(arg) => {
                std::process::exit(cli::run(&argv));
            }
            // Neither a command nor a file that exists. Opening a window here
            // would mean a mistyped verb silently launches a GUI — and hangs
            // forever on a headless machine, which is exactly where scripts
            // and agents run.
            _ if !std::path::Path::new(arg).exists() => {
                eprintln!(
                    "reel: no such file {arg:?}, and it isn't a command.\n\
                     Try `reel help` for the command list."
                );
                std::process::exit(2);
            }
            _ => {}
        }
    }
    let initial_open = argv.into_iter().next();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    #[cfg(target_os = "linux")]
    let tray = tray::spawn(event_loop.create_proxy());
    timing!("tray registered");

    let mut reel = Reel {
        window: None,
        gpu: None,
        egui: None,
        app: ReelApp::new(),
        initial_open,
        window_title: "Reel".into(),
        #[cfg(target_os = "linux")]
        tray,
        tray_recording: false,
    };
    #[cfg(target_os = "linux")]
    {
        reel.app.tray_available = reel.tray.is_some();
    }
    event_loop.run_app(&mut reel).expect("run");
}

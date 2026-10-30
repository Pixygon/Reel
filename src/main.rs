//! Reel — a cross-platform video player and editor. Linux-first.
//! Native stack: winit + wgpu + egui (the Pixygon app stack); playback via
//! libmpv when present, ffmpeg-subprocess fallback otherwise.
//!
//! v0.1 goal: open and play a video with frame-accurate scrubbing, and show the
//! editor timeline. The road to "better than VLC / Premiere-class" is in
//! ROADMAP.md; this is the running foundation it builds on.

mod app;
#[cfg_attr(not(any(target_os = "linux", target_os = "windows", target_os = "macos")), allow(dead_code))]
mod audio;
mod beats;
mod captions;
mod compound;
mod capture;
mod cli;
mod edit;
mod effects;
mod engine;
mod egui_backend;
mod export;
mod gpu;
#[cfg(target_os = "linux")]
mod integration;
#[cfg(target_os = "macos")]
mod macos;
mod lut;
mod masks;
mod media;
mod perf;
#[cfg(target_os = "linux")]
mod portal;
mod plugins;
mod proxy;
#[cfg(target_os = "linux")]
mod runtime;
mod serve;
#[cfg(target_os = "linux")]
mod tray;

/// Events that wake the winit loop from outside (tray menu clicks,
/// AccessKit's screen-reader plumbing).
#[derive(Debug)]
pub enum UserEvent {
    Show,
    Shot(capture::ShotMode),
    /// A screenshot after a countdown — the tray's way to catch a menu.
    ShotAfter(capture::ShotMode, f32),
    ToggleRecord,
    ToggleWebcam,
    Quit,
    /// A file macOS handed over (Finder double-click, Open With, Dock drop).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    OpenFile(String),
    /// AccessKit adapter events (screen readers) — egui consumes these.
    Access(egui_winit::accesskit_winit::Event),
}

impl From<egui_winit::accesskit_winit::Event> for UserEvent {
    fn from(e: egui_winit::accesskit_winit::Event) -> Self {
        UserEvent::Access(e)
    }
}
mod theme;
mod track;
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
    /// Loop proxy — AccessKit wakes the loop through it.
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
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
        let mut egui = EguiBackend::new(&gpu, &window);
        theme::apply(&egui.ctx);
        // Screen readers: AccessKit gets the window + a way to wake the
        // loop. egui builds and pushes the accessibility tree from here on.
        egui.state.init_accesskit(&window, self.proxy.clone());

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
                self.app.sync_local_preview(gpu);

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
                debug_shot(gpu, &frame.texture);
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
            UserEvent::ShotAfter(mode, delay) => self.app.take_screenshot_after(mode, delay),
            UserEvent::ToggleRecord => {
                self.app.toggle_record();
                self.sync_tray();
            }
            UserEvent::ToggleWebcam => {
                #[cfg(target_os = "linux")]
                self.app.toggle_webcam_record();
                self.sync_tray();
            }
            UserEvent::Quit => event_loop.exit(),
            UserEvent::OpenFile(path) => {
                if let Some(w) = &self.window {
                    self.app.open(&path);
                    w.focus_window();
                    // ControlFlow::Wait: nothing else wakes the loop to land
                    // the worker's open, so the file would sit unplayed.
                    w.request_redraw();
                } else {
                    // Launched by the file: the window isn't up yet.
                    self.initial_open = Some(path);
                }
            }
            UserEvent::Access(ev) => {
                // Route AccessKit's action requests into egui.
                if let egui_winit::accesskit_winit::WindowEvent::ActionRequested(req) =
                    ev.window_event
                {
                    if let Some(egui) = &mut self.egui {
                        egui.state.on_accesskit_action_request(req);
                    }
                }
            }
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
/// `REEL_DEBUG_SHOT=<file.png>` saves the window's own pixels once, after
/// `REEL_DEBUG_SHOT_AFTER` seconds (default 3) — a screenshot that needs no
/// screen-recording permission (macOS) and no X server (Xvfb's `import`).
fn debug_shot(gpu: &Gpu, tex: &wgpu::Texture) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    let Some(path) = std::env::var_os("REEL_DEBUG_SHOT") else { return };
    let after: f64 = std::env::var("REEL_DEBUG_SHOT_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(3.0);
    if DONE.load(Ordering::Relaxed)
        || t0().elapsed().as_secs_f64() < after
        || !tex.usage().contains(wgpu::TextureUsages::COPY_SRC)
    {
        return;
    }
    DONE.store(true, Ordering::Relaxed);
    let (w, h) = (tex.width(), tex.height());
    let bpr = (w * 4).div_ceil(256) * 256;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("reel-debug-shot"),
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("reel-debug-shot") });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gpu.queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = gpu.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let bgra = matches!(tex.format(), wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let s = (row * bpr) as usize;
        for px in data[s..s + (w * 4) as usize].chunks_exact(4) {
            let (r, b) = if bgra { (px[2], px[0]) } else { (px[0], px[2]) };
            rgba.extend_from_slice(&[r, px[1], b, 255]);
        }
    }
    drop(data);
    buf.unmap();
    match image::save_buffer(&path, &rgba, w, h, image::ColorType::Rgba8) {
        Ok(()) => log::info!("debug shot saved: {}", std::path::Path::new(&path).display()),
        Err(e) => log::warn!("debug shot failed: {e}"),
    }
}

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

/// PATH for a Mac launch: Reel.app/Contents/Resources/bin (the bundled
/// ffmpeg + ffprobe), the binary's own folder, then Homebrew's prefixes.
#[cfg(target_os = "macos")]
fn mac_path() {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        dirs.push(exe_dir.join("../Resources/bin"));
        dirs.push(exe_dir);
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    let old = std::env::var_os("PATH").unwrap_or_default();
    let mut all: Vec<std::path::PathBuf> = dirs.into_iter().filter(|d| d.is_dir()).collect();
    all.extend(std::env::split_paths(&old));
    if let Ok(path) = std::env::join_paths(all) {
        std::env::set_var("PATH", path);
    }
}

fn main() {
    t0();
    perf::init();
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,zbus=warn,tracing=warn"),
    )
    .init();

    // A Mac app launched from Finder gets a bare PATH (/usr/bin:/bin…), so
    // every `Command::new("ffmpeg")` would miss both the copy inside
    // Reel.app and Homebrew's. Put them in front, once, before any thread.
    #[cfg(target_os = "macos")]
    mac_path();

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
    #[cfg(target_os = "macos")]
    macos::install_open_handler(event_loop.create_proxy());
    event_loop.set_control_flow(ControlFlow::Wait);

    #[cfg(target_os = "linux")]
    let tray = tray::spawn(event_loop.create_proxy());
    timing!("tray registered");

    let proxy = event_loop.create_proxy();
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
        proxy,
    };
    #[cfg(target_os = "linux")]
    {
        reel.app.tray_available = reel.tray.is_some();
    }
    event_loop.run_app(&mut reel).expect("run");
}

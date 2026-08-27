//! Reel application state and the glue that moves a decoded frame onto the GPU
//! and into egui for display.

use crate::capture;
use crate::edit::Project;
use crate::egui_backend::EguiBackend;
use crate::export::{self, ExportJob, ExportSettings};
use crate::gpu::{Gpu, VideoTexture};
use crate::media::{self, ImageDoc, MediaKind};
use crate::video::Player;
use crossbeam_channel::Receiver;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Player,
    Editor,
}

pub struct ReelApp {
    pub mode: Mode,
    pub player: Option<Player>,
    /// A still image being viewed (mutually exclusive with `player`).
    pub image: Option<ImageDoc>,
    image_uploaded: bool,
    pub project: Project,
    pub tex_id: Option<egui::TextureId>,
    tex: Option<VideoTexture>,
    pub status: String,

    // Export ("convert") — available straight from the player.
    pub export_open: bool,
    pub export_settings: ExportSettings,
    /// Output path shown in the dialog; refreshed when source/codec changes.
    pub export_out: String,
    pub export: Option<ExportJob>,

    /// Result channel of a native file-picker running on its own thread.
    picker: Option<Receiver<Option<String>>>,
    /// Result channel of a screenshot being taken on a worker thread.
    shot_rx: Option<Receiver<Result<PathBuf, String>>>,
    /// A screen recording in progress.
    pub recorder: Option<capture::Recording>,
    /// Result channel of a recording start (the system picker runs there).
    rec_start_rx: Option<Receiver<Result<capture::Recording, String>>>,
    /// Result channel of a recording being finalized on a worker thread.
    rec_rx: Option<Receiver<Result<PathBuf, String>>>,
    pub fullscreen: bool,
    /// Desired window title; main.rs applies it when it changes.
    pub window_title: String,

    // "Make Reel the default player" — first-run banner + ⚙ dialog.
    pub defaults_banner: bool,
    pub defaults_open: bool,
    pub def_video: bool,
    pub def_audio: bool,
    pub def_images: bool,

    /// Last pointer/keyboard activity — drives the control-overlay fade.
    pub last_activity: std::time::Instant,
    /// When `status` last changed — drives the transient toast.
    pub status_at: std::time::Instant,
    status_prev: String,
    /// REEL menu → Quit.
    pub quit_requested: bool,
    /// A system tray is registered — capture lives there, not in the app UI.
    pub tray_available: bool,
}

impl ReelApp {
    pub fn new() -> Self {
        Self {
            mode: Mode::Player,
            player: None,
            image: None,
            image_uploaded: false,
            project: Project::default(),
            tex_id: None,
            tex: None,
            status: "Ready.".into(),
            export_open: false,
            export_settings: ExportSettings::default(),
            export_out: String::new(),
            export: None,
            picker: None,
            shot_rx: None,
            recorder: None,
            rec_start_rx: None,
            rec_rx: None,
            fullscreen: false,
            window_title: "Reel".into(),
            defaults_banner: false,
            defaults_open: false,
            def_video: true,
            def_audio: true,
            def_images: false,
            last_activity: std::time::Instant::now(),
            status_at: std::time::Instant::now(),
            status_prev: String::new(),
            quit_requested: false,
            tray_available: false,
        }
    }

    pub fn touch_activity(&mut self) {
        self.last_activity = std::time::Instant::now();
    }

    /// Detect status changes (assignments happen all over) for the toast.
    pub fn track_status(&mut self) {
        if self.status != self.status_prev {
            self.status_prev = self.status.clone();
            self.status_at = std::time::Instant::now();
        }
    }

    /// Linux desktop integration: make sure "Open with Reel" exists, and show
    /// the make-me-default banner exactly once. Call after window creation.
    pub fn init_integration(&mut self) {
        #[cfg(target_os = "linux")]
        {
            if let Err(e) = crate::integration::install_desktop_entry() {
                log::warn!("could not install desktop entry: {e}");
            }
            self.defaults_banner = !crate::integration::load_settings().defaults_prompted;
        }
    }

    /// Apply the chosen default-app categories. Returns a status line.
    pub fn apply_defaults(&mut self) -> String {
        #[cfg(target_os = "linux")]
        {
            use crate::integration as integ;
            let mut mimes: Vec<&str> = Vec::new();
            if self.def_video {
                mimes.extend(integ::VIDEO_MIMES);
            }
            if self.def_audio {
                mimes.extend(integ::AUDIO_MIMES);
            }
            if self.def_images {
                mimes.extend(integ::IMAGE_MIMES);
            }
            self.finish_defaults_prompt();
            if mimes.is_empty() {
                return "Nothing selected — Reel stays available under “Open with”.".into();
            }
            return match integ::set_default_for(&mimes) {
                Ok(()) => "✓ Reel is now the default player for your selection.".into(),
                Err(e) => format!("Could not set defaults: {e}"),
            };
        }
        #[allow(unreachable_code)]
        "Default-app setup is currently Linux-only.".into()
    }

    /// Dismiss the banner and remember the answer.
    pub fn finish_defaults_prompt(&mut self) {
        self.defaults_banner = false;
        #[cfg(target_os = "linux")]
        {
            let mut s = crate::integration::load_settings();
            if !s.defaults_prompted {
                s.defaults_prompted = true;
                crate::integration::save_settings(&s);
            }
        }
    }

    /// What's open right now, if anything.
    pub fn media_kind(&self) -> Option<MediaKind> {
        if self.image.is_some() {
            Some(MediaKind::Image)
        } else {
            self.player.as_ref().map(|p| p.kind)
        }
    }

    /// The path of whatever is open (export source).
    pub fn media_path(&self) -> Option<String> {
        if let Some(img) = &self.image {
            Some(img.path.clone())
        } else {
            self.player.as_ref().map(|p| p.path.clone())
        }
    }

    /// Kick off the native file picker on a worker thread (it must not block
    /// the UI/event loop); `poll_picker` collects the choice.
    pub fn open_picker(&mut self) {
        if self.picker.is_some() {
            return; // one picker at a time
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let filters: [(&str, &[&str]); 4] = [
                ("Media", &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "wmv", "flv", "gif",
                            "mp3", "flac", "ogg", "opus", "m4a", "wav",
                            "png", "jpg", "jpeg", "webp", "bmp", "svg"]),
                ("Video", &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "wmv", "flv", "gif"]),
                ("Audio", &["mp3", "flac", "ogg", "opus", "m4a", "wav"]),
                ("Images", &["png", "jpg", "jpeg", "webp", "bmp", "svg", "tif", "tiff", "qoi", "tga"]),
            ];
            // Linux: rfd talks to the portal over zbus built in tokio mode.
            // MUST run on the process-wide runtime (see runtime.rs) — a
            // throwaway runtime here works exactly once, then the cached
            // D-Bus connection is bound to a dead reactor and the dialog
            // never opens again.
            #[cfg(target_os = "linux")]
            let picked = crate::runtime::rt().block_on(async {
                let mut d = rfd::AsyncFileDialog::new();
                for (name, ext) in filters {
                    d = d.add_filter(name, ext);
                }
                d.pick_file().await.map(|h| h.path().to_string_lossy().into_owned())
            });
            #[cfg(not(target_os = "linux"))]
            let picked = {
                let mut d = rfd::FileDialog::new();
                for (name, ext) in filters {
                    d = d.add_filter(name, ext);
                }
                d.pick_file().map(|p| p.to_string_lossy().into_owned())
            };
            let _ = tx.send(picked);
        });
        self.picker = Some(rx);
    }

    pub fn poll_picker(&mut self) {
        let Some(rx) = &self.picker else { return };
        match rx.try_recv() {
            Ok(Some(path)) => {
                self.picker = None;
                self.open(&path);
            }
            Ok(None) => self.picker = None, // dialog dismissed
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                // The dialog thread died without answering (e.g. no portal).
                self.picker = None;
                self.status = "File dialog unavailable — drop a file or paste a path instead.".into();
            }
        }
    }

    /// Open any media path — video, audio or image — and register it on the
    /// timeline so the editor has something to work with. Video/audio starts
    /// playing immediately; images just appear.
    pub fn open(&mut self, path: &str) {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());

        if media::is_image_path(path) {
            match ImageDoc::open(path) {
                Ok(img) => {
                    self.player = None;
                    // Stills live on the video track with a default 5 s hold.
                    self.project.append_video(&name, path, 5.0);
                    self.status = format!("{name} — {}×{} image", img.width, img.height);
                    self.window_title = format!("{name} — Reel");
                    self.export_settings.codec = crate::export::Codec::Png;
                    self.export_out = export::default_output(path, self.export_settings.codec);
                    self.image = Some(img);
                    self.image_uploaded = false;
                    self.tex_id = None;
                    self.tex = None;
                }
                Err(e) => self.status = format!("Could not open {path}: {e}"),
            }
            return;
        }

        match Player::open(path) {
            Ok(mut p) => {
                p.toggle_play();
                match p.kind {
                    MediaKind::Audio => {
                        self.project.append_audio(&name, path, p.info.duration);
                        self.status = format!(
                            "♪ {name} — {:.1}s [{}]",
                            p.info.duration,
                            p.backend_name()
                        );
                        self.export_settings.codec = crate::export::Codec::Mp3;
                    }
                    _ => {
                        self.project.append_video(&name, path, p.info.duration);
                        self.project.fps = p.info.fps;
                        self.project.width = p.info.width;
                        self.project.height = p.info.height;
                        self.status = format!(
                            "{name} — {}×{} @ {:.2}fps, {:.1}s [{}]",
                            p.info.width, p.info.height, p.info.fps, p.info.duration,
                            p.backend_name()
                        );
                        self.export_settings.codec = crate::export::Codec::H264;
                    }
                }
                self.window_title = format!("{name} — Reel");
                self.export_out = export::default_output(path, self.export_settings.codec);
                self.image = None;
                self.image_uploaded = false;
                self.tex_id = None;
                self.tex = None;
                self.player = Some(p);
            }
            Err(e) => {
                self.status = format!("Could not open {path}: {e}");
            }
        }
    }

    /// Take a screenshot (full/region/window) on a worker thread; when it
    /// lands, open it.
    pub fn take_screenshot(&mut self, mode: capture::ShotMode) {
        if self.shot_rx.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(capture::screenshot(mode).map_err(|e| e.to_string()));
        });
        self.shot_rx = Some(rx);
        self.status = "Taking screenshot…".into();
    }

    /// Is a recording being started (system picker open) right now?
    pub fn record_starting(&self) -> bool {
        self.rec_start_rx.is_some()
    }

    /// Start/stop screen recording. Starting runs on a worker thread (the
    /// system's screen/window picker may be shown); the stopped file opens
    /// in the player.
    pub fn toggle_record(&mut self) {
        if let Some(rec) = self.recorder.take() {
            let (tx, rx) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send(rec.stop().map_err(|e| e.to_string()));
            });
            self.rec_rx = Some(rx);
            self.status = "Finalizing recording…".into();
        } else if self.rec_start_rx.is_none() {
            let (tx, rx) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send(capture::start_recording().map_err(|e| e.to_string()));
            });
            self.rec_start_rx = Some(rx);
            self.status = "Starting recording — pick what to share…".into();
        }
    }

    /// Collect finished captures (screenshot / recording) and open them.
    pub fn poll_captures(&mut self) {
        if let Some(rx) = &self.rec_start_rx {
            match rx.try_recv() {
                Ok(Ok(rec)) => {
                    self.rec_start_rx = None;
                    self.status = "⏺ Recording… click ⏹ to stop".into();
                    self.recorder = Some(rec);
                }
                Ok(Err(e)) => {
                    self.rec_start_rx = None;
                    self.status = format!("Recording: {e}");
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.rec_start_rx = None,
            }
        }
        let mut done: Vec<Result<PathBuf, String>> = Vec::new();
        for rx_slot in [&mut self.shot_rx, &mut self.rec_rx] {
            let Some(rx) = rx_slot else { continue };
            match rx.try_recv() {
                Ok(res) => {
                    *rx_slot = None;
                    done.push(res);
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => *rx_slot = None,
            }
        }
        for res in done {
            match res {
                Ok(path) => self.open(&path.to_string_lossy()),
                Err(e) => self.status = format!("Capture: {e}"),
            }
        }
    }

    /// Advance playback and, if a new frame arrived, push it to the GPU texture
    /// and (re)register it with egui so the viewport shows it. Still images
    /// upload once and stay put.
    pub fn sync_frame(&mut self, gpu: &Gpu, egui: &mut EguiBackend) {
        if let Some(img) = &mut self.image {
            if !self.image_uploaded {
                img.clamp_to(gpu.max_texture_dim);
                // egui blends premultiplied-alpha textures; ImageDoc keeps
                // straight alpha (exports need it), so premultiply the copy
                // we upload — without this, transparency renders wrong.
                let mut rgba = img.data.clone();
                for px in rgba.chunks_exact_mut(4) {
                    let a = px[3] as u32;
                    if a < 255 {
                        px[0] = (px[0] as u32 * a / 255) as u8;
                        px[1] = (px[1] as u32 * a / 255) as u8;
                        px[2] = (px[2] as u32 * a / 255) as u8;
                    }
                }
                let tex = VideoTexture::new(&gpu.device, img.width, img.height);
                tex.write(&gpu.queue, &rgba);
                self.tex_id = Some(egui.register_texture(&gpu.device, &tex.view));
                self.tex = Some(tex);
                self.image_uploaded = true;
            }
            return;
        }
        let Some(player) = &mut self.player else { return };
        player.update();
        if !player.take_dirty() {
            return;
        }
        let Some(frame) = &player.current else { return };
        if frame.data.len() < (frame.width * frame.height * 4) as usize {
            return; // guard against a short/partial frame
        }

        let need_new = match &self.tex {
            Some(t) => t.width != frame.width || t.height != frame.height,
            None => true,
        };
        if need_new {
            self.tex = Some(VideoTexture::new(&gpu.device, frame.width, frame.height));
            self.tex_id = None;
        }
        let tex = self.tex.as_ref().unwrap();
        tex.write(&gpu.queue, &frame.data);

        match self.tex_id {
            Some(id) => egui.update_registered(id, &gpu.device, &tex.view),
            None => self.tex_id = Some(egui.register_texture(&gpu.device, &tex.view)),
        }
    }

    /// Should the run loop keep requesting redraws? While playing (or just
    /// after open/seek), while an export reports progress, and while a file
    /// picker is pending.
    pub fn wants_redraw(&self) -> bool {
        self.player.as_ref().map(|p| p.wants_redraw()).unwrap_or(false)
            || self.export.as_ref().map(|j| !j.state().finished).unwrap_or(false)
            || self.picker.is_some()
            || self.shot_rx.is_some()
            || self.rec_start_rx.is_some()
            || self.rec_rx.is_some()
    }

    /// Native size of what the viewport is currently showing (frame, cover
    /// art, visualizer or image) — drives aspect-fit.
    pub fn tex_dims(&self) -> Option<(u32, u32)> {
        self.tex.as_ref().map(|t| (t.width, t.height))
    }
}

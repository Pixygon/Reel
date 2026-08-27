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
    /// Path text buffer for the in-UI open field.
    pub open_field: String,

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
    pub recorder: Option<capture::Recorder>,
    /// Result channel of a recording being finalized on a worker thread.
    rec_rx: Option<Receiver<Result<PathBuf, String>>>,
    pub fullscreen: bool,
    /// Desired window title; main.rs applies it when it changes.
    pub window_title: String,
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
            status: "Open a video to begin — drop a file, Open…, or reel <path>".into(),
            open_field: String::new(),
            export_open: false,
            export_settings: ExportSettings::default(),
            export_out: String::new(),
            export: None,
            picker: None,
            shot_rx: None,
            recorder: None,
            rec_rx: None,
            fullscreen: false,
            window_title: "Reel".into(),
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
            let picked = rfd::FileDialog::new()
                .add_filter(
                    "Video",
                    &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "wmv", "flv", "gif"],
                )
                .add_filter("All files", &["*"])
                .pick_file();
            let _ = tx.send(picked.map(|p| p.to_string_lossy().into_owned()));
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
            Err(crossbeam_channel::TryRecvError::Disconnected) => self.picker = None,
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

    /// Take a screenshot on a worker thread; when it lands, open it.
    pub fn take_screenshot(&mut self) {
        if self.shot_rx.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(capture::screenshot().map_err(|e| e.to_string()));
        });
        self.shot_rx = Some(rx);
        self.status = "Taking screenshot…".into();
    }

    /// Start/stop screen recording. The stopped file opens in the player.
    pub fn toggle_record(&mut self) {
        if let Some(rec) = self.recorder.take() {
            let (tx, rx) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send(rec.stop().map_err(|e| e.to_string()));
            });
            self.rec_rx = Some(rx);
            self.status = "Finalizing recording…".into();
        } else {
            match capture::start_recording() {
                Ok(rec) => {
                    self.status = format!("⏺ Recording via {}… click ⏹ to stop", rec.tool);
                    self.recorder = Some(rec);
                }
                Err(e) => self.status = format!("Recording: {e}"),
            }
        }
    }

    /// Collect finished captures (screenshot / recording) and open them.
    pub fn poll_captures(&mut self) {
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
                let tex = VideoTexture::new(&gpu.device, img.width, img.height);
                tex.write(&gpu.queue, &img.data);
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
            || self.rec_rx.is_some()
    }
}

//! Reel application state and the glue that moves a decoded frame onto the GPU
//! and into egui for display.

use crate::edit::Project;
use crate::egui_backend::EguiBackend;
use crate::export::{self, ExportJob, ExportSettings};
use crate::gpu::{Gpu, VideoTexture};
use crate::video::Player;
use crossbeam_channel::Receiver;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Player,
    Editor,
}

pub struct ReelApp {
    pub mode: Mode,
    pub player: Option<Player>,
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
    pub fullscreen: bool,
    /// Desired window title; main.rs applies it when it changes.
    pub window_title: String,
}

impl ReelApp {
    pub fn new() -> Self {
        Self {
            mode: Mode::Player,
            player: None,
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
            fullscreen: false,
            window_title: "Reel".into(),
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

    /// Open a media path as the active player source, and register it on the
    /// timeline so the editor has something to work with. Playback starts
    /// immediately — opening a video means you want to watch it.
    pub fn open(&mut self, path: &str) {
        match Player::open(path) {
            Ok(mut p) => {
                p.toggle_play();
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string());
                self.project.append_video(&name, path, p.info.duration);
                self.project.fps = p.info.fps;
                self.project.width = p.info.width;
                self.project.height = p.info.height;
                self.status = format!(
                    "{name} — {}×{} @ {:.2}fps, {:.1}s [{}]",
                    p.info.width, p.info.height, p.info.fps, p.info.duration,
                    p.backend_name()
                );
                self.window_title = format!("{name} — Reel");
                self.export_out = export::default_output(path, self.export_settings.codec);
                self.player = Some(p);
            }
            Err(e) => {
                self.status = format!("Could not open {path}: {e}");
            }
        }
    }

    /// Advance playback and, if a new frame arrived, push it to the GPU texture
    /// and (re)register it with egui so the viewport shows it.
    pub fn sync_frame(&mut self, gpu: &Gpu, egui: &mut EguiBackend) {
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
    }
}

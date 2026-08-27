//! Reel application state and the glue that moves a decoded frame onto the GPU
//! and into egui for display.

use crate::edit::Project;
use crate::egui_backend::EguiBackend;
use crate::gpu::{Gpu, VideoTexture};
use crate::video::Player;

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
}

impl ReelApp {
    pub fn new() -> Self {
        Self {
            mode: Mode::Player,
            player: None,
            project: Project::default(),
            tex_id: None,
            tex: None,
            status: "Open a video to begin — File ▸ Open, or reel <path>".into(),
            open_field: String::new(),
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

    /// Should the run loop keep requesting redraws (playing, or a frame is
    /// expected to land shortly after an open/seek)?
    pub fn wants_redraw(&self) -> bool {
        self.player.as_ref().map(|p| p.wants_redraw()).unwrap_or(false)
    }
}

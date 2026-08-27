//! Reel application state and the glue that moves a decoded frame onto the GPU
//! and into egui for display.

use crate::capture;
use crate::edit::{EditorState, Project, TrackKind};
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
    pub editor: EditorState,
    /// The in-flight open belongs to a loaded .reel project (don't re-append
    /// the media onto the timeline).
    opening_for_project: bool,
    pub tex_id: Option<egui::TextureId>,
    tex: Option<VideoTexture>,
    pub status: String,

    // Export ("convert") — available straight from the player.
    pub export_open: bool,
    pub export_settings: ExportSettings,
    /// Output path shown in the dialog; refreshed when source/codec changes.
    pub export_out: String,
    pub export: Option<ExportJob>,
    /// Export the edited timeline rather than the source file.
    pub export_timeline: bool,

    /// Result channel of a native file-picker running on its own thread.
    picker: Option<Receiver<Option<String>>>,
    /// A video/audio open in progress on a worker thread.
    opening: Option<Receiver<Result<Player, String>>>,
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
    /// REEL_DEBUG_OPEN=export — open the dialog once media is ready.
    debug_open_export: bool,
}

impl ReelApp {
    pub fn new() -> Self {
        Self {
            mode: Mode::Player,
            player: None,
            image: None,
            image_uploaded: false,
            project: Project::default(),
            editor: EditorState::default(),
            opening_for_project: false,
            tex_id: None,
            tex: None,
            status: "Ready.".into(),
            export_open: false,
            export_settings: ExportSettings::default(),
            export_out: String::new(),
            export: None,
            export_timeline: false,
            picker: None,
            opening: None,
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
            debug_open_export: false,
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
        // Test hook: open a panel for visual verification once media lands.
        self.debug_open_export = std::env::var("REEL_DEBUG_OPEN").as_deref() == Ok("export");
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
            let filters: [(&str, &[&str]); 5] = [
                ("Media", &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "wmv", "flv", "gif",
                            "mp3", "flac", "ogg", "opus", "m4a", "wav",
                            "png", "jpg", "jpeg", "webp", "bmp", "svg"]),
                ("Video", &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "wmv", "flv", "gif"]),
                ("Audio", &["mp3", "flac", "ogg", "opus", "m4a", "wav"]),
                ("Images", &["png", "jpg", "jpeg", "webp", "bmp", "svg", "tif", "tiff", "qoi", "tga"]),
                ("Reel project", &["reel"]),
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

    /// Open any media path — video, audio or image. Images decode inline
    /// (instant); video/audio open on a worker thread so the window never
    /// blocks on a demuxer — `poll_opening` lands the player when ready.
    pub fn open(&mut self, path: &str) {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());

        // A .reel document opens the whole edit: load the project, open its
        // first source, land in the editor.
        if path.to_lowercase().ends_with(".reel") {
            match Project::load(path) {
                Ok(project) => {
                    let first_source = project
                        .tracks
                        .iter()
                        .flat_map(|t| t.clips.iter())
                        .map(|c| c.source.clone())
                        .next();
                    self.project = project;
                    self.editor = EditorState::default();
                    self.editor.project_path = Some(path.to_string());
                    self.mode = Mode::Editor;
                    self.window_title = format!("{name} — Reel");
                    self.status = format!("Project {name} loaded.");
                    if let Some(src) = first_source {
                        self.open_media_async(&src, true);
                    }
                }
                Err(e) => self.status = format!("Could not open project {path}: {e}"),
            }
            return;
        }

        if !media::is_image_path(path) {
            self.open_media_async(path, false);
            self.status = format!("Opening {name}…");
            return;
        }

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

    }

    fn open_media_async(&mut self, path: &str, for_project: bool) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let t_path = path.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(Player::open(&t_path).map_err(|e| e.to_string()));
        });
        self.opening = Some(rx);
        self.opening_for_project = for_project;
    }

    /// Land a player opened on the worker thread.
    pub fn poll_opening(&mut self) {
        if self.debug_open_export && self.player.is_some() {
            self.debug_open_export = false;
            self.open_export();
        }
        let Some(rx) = &self.opening else { return };
        match rx.try_recv() {
            Ok(Ok(p)) => {
                self.opening = None;
                self.finish_open(p);
            }
            Ok(Err(e)) => {
                self.opening = None;
                self.status = format!("Could not open: {e}");
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.opening = None;
                self.status = "Open failed unexpectedly.".into();
            }
        }
    }

    fn finish_open(&mut self, mut p: Player) {
        let path = p.path.clone();
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        // Opening a file while editing an existing timeline IMPORTS it.
        if self.mode == Mode::Editor
            && !self.opening_for_project
            && !self.project.export_segments().is_empty()
        {
            self.import_into_timeline(p);
            return;
        }
        if std::mem::take(&mut self.opening_for_project) {
            // Source of a loaded .reel: stay paused at the playhead's mapped
            // source position; the timeline already has its clips.
            if let Some(clip) = self.project.clip_at(TrackKind::Video, self.editor.playhead) {
                if clip.source == path {
                    p.seek(clip.in_point + (self.editor.playhead - clip.start));
                }
            }
            self.image = None;
            self.image_uploaded = false;
            self.tex_id = None;
            self.tex = None;
            self.player = Some(p);
            return;
        }
        p.toggle_play();
        match p.kind {
            MediaKind::Audio => {
                self.project.append_audio(&name, &path, p.info.duration);
                self.status = format!(
                    "♪ {name} — {:.1}s [{}]",
                    p.info.duration,
                    p.backend_name()
                );
                self.export_settings.codec = crate::export::Codec::Mp3;
            }
            _ => {
                self.project.append_video(&name, &path, p.info.duration);
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
        self.export_out = export::default_output(&path, self.export_settings.codec);
        self.image = None;
        self.image_uploaded = false;
        self.tex_id = None;
        self.tex = None;
        self.player = Some(p);
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
            crate::timing!("first frame on GPU ({}×{})", frame.width, frame.height);
            self.tex = Some(VideoTexture::new(&gpu.device, frame.width, frame.height));
            self.tex_id = None;
        }
        let tex = self.tex.as_ref().unwrap();
        let t_up = std::time::Instant::now();
        tex.write(&gpu.queue, &frame.data);
        crate::perf::note_upload(t_up.elapsed().as_micros() as f64, frame.width, frame.height);

        match self.tex_id {
            Some(id) => egui.update_registered(id, &gpu.device, &tex.view),
            None => self.tex_id = Some(egui.register_texture(&gpu.device, &tex.view)),
        }
    }

    /// Open the export dialog, defaulting to whatever the user is most
    /// likely to want: in the editor with clips on the timeline, that's the
    /// EDIT; in the player, the source file.
    pub fn open_export(&mut self) {
        let has_cut = !self.project.export_segments().is_empty();
        let is_image = self.image.is_some();
        self.export_timeline = self.mode == Mode::Editor && has_cut && !is_image;
        self.export_out = if self.export_timeline {
            self.timeline_output()
        } else if let Some(src) = self.media_path() {
            export::default_output(&src, self.export_settings.codec)
        } else {
            String::new()
        };
        self.export_open = true;
    }

    /// A sensible default output path for a timeline export: the project
    /// name (or first source) with a `-cut` suffix, never clobbering.
    pub fn timeline_output(&self) -> String {
        let base = self
            .editor
            .project_path
            .clone()
            .or_else(|| {
                self.project
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .map(|c| c.source.clone())
                    .next()
            })
            .unwrap_or_else(|| "timeline".into());
        let p = std::path::Path::new(&base);
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "timeline".into());
        let dir = p.parent().unwrap_or_else(|| std::path::Path::new("."));
        let ext = self.export_settings.codec.extension();
        let ext = if matches!(ext, "mp4" | "webm") { ext } else { "mp4" };
        let mut candidate = dir.join(format!("{stem}-cut.{ext}"));
        let mut n = 1;
        while candidate.exists() {
            candidate = dir.join(format!("{stem}-cut-{n}.{ext}"));
            n += 1;
        }
        candidate.to_string_lossy().into_owned()
    }

    /// Enter the editor with the playhead where the player is.
    pub fn enter_editor(&mut self) {
        self.mode = Mode::Editor;
        if let Some(p) = &self.player {
            if let Some(t) = self.project.source_to_timeline(&p.path, p.position) {
                self.editor.playhead = t;
                self.editor.active_clip =
                    self.project.clip_at(TrackKind::Video, t).map(|c| c.id);
            }
        }
    }

    /// Timeline scrub: move the playhead and preview the frame under it —
    /// switching the previewed file when the playhead crosses into a clip
    /// from a different source.
    pub fn seek_timeline(&mut self, t: f64) {
        self.editor.playhead = t.max(0.0);
        if let Some(clip) = self.project.clip_at(TrackKind::Video, self.editor.playhead) {
            let (id, src, in_point, start) =
                (clip.id, clip.source.clone(), clip.in_point, clip.start);
            let want = in_point + (self.editor.playhead - start);
            if let Some(player) = self.player.as_mut() {
                if src == player.path {
                    player.seek(want);
                } else {
                    player.switch_source(&src, want);
                }
                self.editor.active_clip = Some(id);
            }
        }
    }

    /// Editor playback = sequencing: advance the timeline playhead from the
    /// source position, and when the active clip's window runs out, jump to
    /// the next clip on the timeline (skipping gaps).
    pub fn update_editor_playback(&mut self) {
        if self.mode != Mode::Editor {
            return;
        }
        let Some(player) = self.player.as_mut() else { return };
        if !player.playing {
            return;
        }
        let pos = player.position;
        let active = self
            .editor
            .active_clip
            .and_then(|id| self.project.clip(id))
            .filter(|c| c.source == player.path)
            .or_else(|| self.project.clip_at(TrackKind::Video, self.editor.playhead))
            .cloned();
        let Some(clip) = active else { return };
        self.editor.active_clip = Some(clip.id);
        // Stop at the out-marker when a range is set.
        if let Some(out) = self.editor.range_out {
            if self.editor.playhead >= out {
                if player.playing {
                    player.toggle_play();
                }
                self.editor.playhead = out;
                return;
            }
        }
        if pos <= clip.in_point + clip.duration + 0.02 {
            self.editor.playhead = clip.start + (pos - clip.in_point).max(0.0);
        } else {
            match self.project.clip_after(TrackKind::Video, clip.start).cloned() {
                Some(next) => {
                    self.editor.playhead = next.start;
                    self.editor.active_clip = Some(next.id);
                    if next.source == player.path {
                        player.seek(next.in_point);
                    } else {
                        // Multi-source timeline: roll the preview onto the
                        // next clip's file without rebuilding the player.
                        player.switch_source(&next.source, next.in_point);
                    }
                }
                None => {
                    // End of the edit.
                    if player.playing {
                        player.toggle_play();
                    }
                    self.editor.playhead = clip.end();
                }
            }
        }
    }

    /// Add media to the current edit instead of replacing it (used when a
    /// file is opened while the editor has clips).
    fn import_into_timeline(&mut self, p: Player) {
        let name = std::path::Path::new(&p.path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| p.path.clone());
        self.editor.push_undo(&self.project);
        match p.kind {
            MediaKind::Audio => self.project.append_audio(&name, &p.path, p.info.duration),
            _ => self.project.append_video(&name, &p.path, p.info.duration),
        }
        self.status = format!("Added {name} to the timeline.");
        self.image = None;
        self.image_uploaded = false;
        self.tex_id = None;
        self.tex = None;
        self.player = Some(p);
        // Preview where the new clip landed.
        let t = self
            .project
            .tracks
            .iter()
            .flat_map(|tr| tr.clips.iter())
            .filter(|c| c.source == self.player.as_ref().map(|p| p.path.clone()).unwrap_or_default())
            .map(|c| c.start)
            .fold(0.0, f64::max);
        self.seek_timeline(t);
    }

    /// Split every clip under the playhead (S).
    pub fn editor_split(&mut self) {
        let t = self.editor.playhead;
        let would = self
            .project
            .tracks
            .iter()
            .flat_map(|tr| tr.clips.iter())
            .any(|c| c.start + 0.05 < t && t < c.end() - 0.05);
        if !would {
            self.status = "Nothing under the playhead to split.".into();
            return;
        }
        self.editor.push_undo(&self.project);
        let n = self.project.split_at(t);
        self.status = format!("Split {n} clip(s) at {t:.2}s.");
    }

    /// Delete the selected clip (Del).
    pub fn editor_delete(&mut self) {
        if let Some(id) = self.editor.selected {
            self.editor.push_undo(&self.project);
            self.project.delete_clip(id);
            self.editor.selected = None;
            self.status = "Clip deleted.".into();
        }
    }

    /// Save the project as a .reel document (Ctrl+S). Defaults to sitting
    /// next to the first source file.
    pub fn editor_save(&mut self) {
        let path = match self.editor.project_path.clone() {
            Some(p) => p,
            None => {
                let src = self
                    .project
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .map(|c| c.source.clone())
                    .next();
                match src {
                    Some(s) => std::path::Path::new(&s)
                        .with_extension("reel")
                        .to_string_lossy()
                        .into_owned(),
                    None => {
                        self.status = "Nothing on the timeline to save yet.".into();
                        return;
                    }
                }
            }
        };
        match self.project.save(&path) {
            Ok(()) => {
                self.editor.project_path = Some(path.clone());
                self.editor.dirty = false;
                self.status = format!("Project saved → {path}");
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    /// Should the run loop keep requesting redraws? While playing (or just
    /// after open/seek), while an export reports progress, and while a file
    /// picker is pending.
    pub fn wants_redraw(&self) -> bool {
        self.player.as_ref().map(|p| p.wants_redraw()).unwrap_or(false)
            || self.export.as_ref().map(|j| !j.state().finished).unwrap_or(false)
            || self.picker.is_some()
            || self.opening.is_some()
            || self.shot_rx.is_some()
            || self.rec_start_rx.is_some()
            || self.rec_rx.is_some()
    }

    /// Native size of what the viewport is currently showing (frame, cover
    /// art, visualizer or image) — drives aspect-fit.
    pub fn tex_dims(&self) -> Option<(u32, u32)> {
        self.tex.as_ref().map(|t| (t.width, t.height))
    }

    /// A view of the current picture for Reel's own render pass.
    pub fn tex_view(&self) -> Option<wgpu::TextureView> {
        self.tex
            .as_ref()
            .map(|t| t.texture.create_view(&wgpu::TextureViewDescriptor::default()))
    }
}

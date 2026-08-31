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

/// What the open dialog is currently being used for.
#[derive(Clone, PartialEq, Eq, Default)]
pub enum PickerTarget {
    #[default]
    Media,
    Music,
    /// A .cube LUT for this clip.
    Lut(u64),
    /// Gather into the media pool without opening.
    Pool,
    /// Find where this offline file went.
    Relink(String),
    /// A .wgsl effect plugin for this clip.
    Plugin(u64),
}

/// A live PiP preview: its own muted player plus the texture its frames
/// land on.
pub struct OverlayPreview {
    pub player: Player,
    pub tex: Option<VideoTexture>,
    pub tex_id: Option<egui::TextureId>,
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
    /// Queued exports — line up every platform, then walk away.
    pub queue: export::Queue,
    /// A stabilization audition mid-render: (job, temp output, clip id).
    pub audition: Option<(ExportJob, String, u64)>,
    /// The copied grade, waiting to be pasted onto other clips.
    pub grade_clipboard: Option<crate::effects::Effects>,
    /// A compound-clip render in flight: (receiver of (flat, reel), and
    /// whether to add a clip when it lands — false = a staleness refresh).
    #[allow(clippy::type_complexity)]
    pub nesting: Option<(std::sync::mpsc::Receiver<anyhow::Result<(String, String)>>, bool)>,
    /// Last time compounds were checked for staleness.
    pub nest_checked: Option<std::time::Instant>,
    /// A window-track in flight: (receiver, clip id).
    #[allow(clippy::type_complexity)]
    pub tracking: Option<(
        std::sync::mpsc::Receiver<anyhow::Result<(Vec<crate::edit::Keyframe>, Vec<crate::edit::Keyframe>)>>,
        u64,
    )>,
    /// A captioning run in progress (local, on a worker thread).
    pub captions_job: Option<crate::captions::Job>,
    /// Audio peaks per source, for the timeline. Decoded in the background.
    pub waveforms: crate::waveform::Cache,
    /// Tiled thumbnail sheets per source, for the timeline.
    pub thumbs: crate::thumbs::Cache,
    /// Live preview players for clips beyond the main one — PiP overlays and
    /// the incoming side of a crossfade — keyed by CLIP id (two clips can
    /// share one source file). The seed of the decoder pool.
    pub overlay_previews: std::collections::HashMap<u64, OverlayPreview>,
    /// The transition being previewed at the playhead, if any:
    /// (incoming clip id, 0..1 progress, kind).
    pub transition_preview: Option<(u64, f32, crate::edit::TransitionKind)>,
    /// The live timeline audio mix (editor mode): every sounding clip, the
    /// music bed, gains, fades and ducking — not just the main clip's own
    /// track. The video clock stays master; this chases it.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub mixer: Option<crate::audio::Mixer>,
    /// The voice recorder (Linux, PipeWire) — opened on first punch-in.
    #[cfg(target_os = "linux")]
    pub voice_rec: Option<crate::audio::Recorder>,
    /// A punch-in in progress: the timeline position recording began at.
    pub punch_in_at: Option<f64>,
    /// Media-pool search box contents.
    pub pool_search: String,
    /// The source being auditioned in the source monitor (player mode over
    /// a live project), with its in/out marks for the three-point insert.
    pub source_preview: Option<String>,
    pub src_mark_in: Option<f64>,
    pub src_mark_out: Option<f64>,
    /// The `?` keyboard-map overlay.
    pub show_keymap: bool,
    /// A saved UI scale to apply on the first frame (ctx isn't here yet).
    pub pending_zoom: Option<f32>,
    /// The quarter-turn currently applied to the preview player.
    pub preview_rotate: u8,
    /// Publish-everywhere filename template — {name}, {platform} expand.
    pub publish_template: String,
    /// Preset slugs whose publish render should NOT burn captions.
    pub publish_no_captions: std::collections::HashSet<String>,
    /// Whether we've tried to open the mixer (it opens lazily on first
    /// entering the editor — an audio stream at app start would tax the
    /// cold-open budget for people who only came to watch something).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    mixer_attempted: bool,
    /// Decoded PCM per source for the mixer.
    pub samples: crate::audio::SampleCache,
    /// Editing proxies for heavy sources — the PREVIEW plays these; export
    /// and every analysis path keep the originals.
    pub proxies: crate::proxy::Cache,
    /// When the current mix plan was built — rebuilt after edits.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    mix_built_at: std::time::Instant,
    /// The user's mute intent — kept apart from `player.muted`, which the
    /// editor borrows while the mixer speaks for the timeline.
    pub user_muted: bool,
    /// Scopes panel visibility (histogram + waveform).
    pub show_scopes: bool,
    /// One-shot: scroll the panel to the scopes next frame (set by the Show
    /// button and the headless hook — the panel is taller than the window).
    pub scroll_to_scopes: bool,
    /// Which tone curve the panel edits: 0 master, 1 R, 2 G, 3 B.
    pub curve_channel: usize,
    /// The USER's playback rate (the [ ] keys and the transport control).
    /// In the editor the effective mpv rate is `user_rate × clip rate`, so
    /// a clip's speed never hijacks the user's own control.
    pub user_rate: f64,
    /// Baked grade lattices on the preview device, keyed by grade content.
    lut_previews: std::collections::HashMap<u64, Option<wgpu::TextureView>>,
    pub caption_model: crate::captions::Model,

    /// Result channel of a native file-picker running on its own thread.
    picker: Option<Receiver<Option<String>>>,
    /// Where the next picked file goes — the same dialog serves both.
    pub picker_target: PickerTarget,
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
    debug_autoplay: bool,
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
            queue: export::Queue::default(),
            captions_job: None,
            waveforms: crate::waveform::Cache::default(),
            thumbs: crate::thumbs::Cache::default(),
            overlay_previews: std::collections::HashMap::new(),
            transition_preview: None,
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            mixer: None,
            mixer_attempted: false,
            samples: crate::audio::SampleCache::default(),
            proxies: crate::proxy::Cache::default(),
            audition: None,
            grade_clipboard: None,
            nesting: None,
            nest_checked: None,
            tracking: None,
            #[cfg(target_os = "linux")]
            voice_rec: None,
            punch_in_at: None,
            pool_search: String::new(),
            source_preview: None,
            src_mark_in: None,
            src_mark_out: None,
            show_keymap: false,
            pending_zoom: None,
            preview_rotate: 0,
            publish_template: "{name}-{platform}".into(),
            publish_no_captions: std::collections::HashSet::new(),
            mix_built_at: std::time::Instant::now() - std::time::Duration::from_secs(3600),
            user_muted: false,
            show_scopes: false,
            scroll_to_scopes: false,
            curve_channel: 0,
            user_rate: 1.0,
            lut_previews: std::collections::HashMap::new(),
            caption_model: crate::captions::Model::BaseEn,
            picker: None,
            picker_target: PickerTarget::Media,
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
            debug_autoplay: false,
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
        self.debug_autoplay = std::env::var("REEL_DEBUG_PLAY").as_deref() == Ok("1");
        crate::plugins::seed_examples();
        crate::titles::seed_presets();
        #[cfg(target_os = "linux")]
        {
            if let Err(e) = crate::integration::install_desktop_entry() {
                log::warn!("could not install desktop entry: {e}");
            }
            let settings = crate::integration::load_settings();
            self.defaults_banner = !settings.defaults_prompted;
            if settings.ui_scale > 0.5 {
                self.pending_zoom = Some(settings.ui_scale);
            }
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
    /// Pick a music bed instead of opening media. Same picker, different
    /// destination — see `picker_target`.
    pub fn pick_music(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.picker_target = PickerTarget::Music;
        self.open_picker_inner(true);
    }

    /// Pick a .cube LUT for a clip.
    pub fn pick_lut(&mut self, clip: u64) {
        if self.picker.is_some() {
            return;
        }
        self.picker_target = PickerTarget::Lut(clip);
        self.open_picker_inner(false);
    }

    pub fn open_picker(&mut self) {
        if self.picker.is_some() {
            return; // one picker at a time
        }
        self.picker_target = PickerTarget::Media;
        self.open_picker_inner(false);
    }

    /// Open the media picker for whatever `picker_target` is already set
    /// to (pool gather, relink, …).
    pub fn open_picker_inner_for_target(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.open_picker_inner(false);
    }

    fn open_picker_inner(&mut self, audio_only: bool) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let audio_filters: [(&str, &[&str]); 2] = [
                ("Audio", &["mp3", "flac", "ogg", "opus", "m4a", "wav", "aac", "aiff"]),
                ("Any media", &["mp4", "mkv", "webm", "mov", "m4v", "mp3", "flac", "ogg",
                                "opus", "m4a", "wav"]),
            ];
            let media_filters: [(&str, &[&str]); 6] = [
                ("Media", &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "wmv", "flv", "gif",
                            "mp3", "flac", "ogg", "opus", "m4a", "wav",
                            "png", "jpg", "jpeg", "webp", "bmp", "svg"]),
                ("Video", &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "wmv", "flv", "gif"]),
                ("Audio", &["mp3", "flac", "ogg", "opus", "m4a", "wav"]),
                ("Images", &["png", "jpg", "jpeg", "webp", "bmp", "svg", "tif", "tiff", "qoi", "tga"]),
                ("Reel project", &["reel"]),
                ("Effects & LUTs", &["wgsl", "cube"]),
            ];
            // Linux: rfd talks to the portal over zbus built in tokio mode.
            // MUST run on the process-wide runtime (see runtime.rs) — a
            // throwaway runtime here works exactly once, then the cached
            // D-Bus connection is bound to a dead reactor and the dialog
            // never opens again.
            #[cfg(target_os = "linux")]
            let picked = crate::runtime::rt().block_on(async {
                let mut d = rfd::AsyncFileDialog::new();
                if audio_only {
                    for (name, ext) in audio_filters {
                        d = d.add_filter(name, ext);
                    }
                } else {
                    for (name, ext) in media_filters {
                        d = d.add_filter(name, ext);
                    }
                }
                d.pick_file().await.map(|h| h.path().to_string_lossy().into_owned())
            });
            #[cfg(not(target_os = "linux"))]
            let picked = {
                let mut d = rfd::FileDialog::new();
                if audio_only {
                    for (name, ext) in audio_filters {
                        d = d.add_filter(name, ext);
                    }
                } else {
                    for (name, ext) in media_filters {
                        d = d.add_filter(name, ext);
                    }
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
                match self.picker_target.clone() {
                    PickerTarget::Media => self.open(&path),
                    PickerTarget::Lut(clip) => {
                        let abs = std::fs::canonicalize(&path)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or(path);
                        match crate::lut::load(&abs) {
                            Ok(_) => {
                                self.editor.push_undo(&self.project);
                                let idx = self.project.add_lut(&abs);
                                if let Some(c) = self.project.clip_mut(clip) {
                                    c.effects.lut = Some(idx);
                                }
                                self.editor.mark_changed();
                                self.status = "LUT applied — previewed and rendered alike.".into();
                            }
                            Err(e) => self.status = format!("Not a usable LUT: {e}"),
                        }
                    }
                    PickerTarget::Plugin(clip) => {
                        let abs = std::fs::canonicalize(&path)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or(path);
                        match crate::plugins::load(&abs) {
                            Ok(plug) => {
                                self.editor.push_undo(&self.project);
                                let idx = self.project.add_plugin(&abs);
                                if let Some(c) = self.project.clip_mut(clip) {
                                    c.effects.plugin = Some(idx);
                                    if c.effects.plugin_params == [0.0; 4] {
                                        for (i, d) in plug.params.iter().enumerate() {
                                            c.effects.plugin_params[i] = d.default;
                                        }
                                    }
                                }
                                self.editor.mark_changed();
                                self.status = format!("Effect '{}' applied — previewed and rendered alike.", plug.name);
                            }
                            Err(e) => self.status = format!("Not a usable effect: {e}"),
                        }
                    }
                    PickerTarget::Pool => {
                        let abs = std::fs::canonicalize(&path)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or(path);
                        self.editor.push_undo(&self.project);
                        self.project.pool_add(&abs, "");
                        self.editor.mark_changed();
                        self.status = "Gathered into the pool.".into();
                    }
                    PickerTarget::Relink(old) => {
                        let abs = std::fs::canonicalize(&path)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or(path);
                        self.editor.push_undo(&self.project);
                        let n = self.project.relink(&old, &abs);
                        self.editor.mark_changed();
                        self.status = format!("Relinked {n} reference(s).");
                    }
                    PickerTarget::Music => {
                        self.editor.push_undo(&self.project);
                        let keep = self.project.music.clone().unwrap_or_default();
                        self.project.music = Some(crate::edit::Music { source: path, ..keep });
                        self.editor.mark_changed();
                        self.status = "Music bed added — it ducks under speech on export.".into();
                    }
                }
                self.picker_target = PickerTarget::Media;
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
                    // Resume exactly where the last session stood.
                    let ses = self.project.session.clone();
                    self.editor.playhead = ses.playhead.max(0.0);
                    if ses.zoom > 1.0 {
                        self.editor.px_per_s = ses.zoom;
                    }
                    self.editor.scroll_x = ses.scroll_x;
                    self.editor.selected =
                        ses.selected.filter(|id| self.project.clip(*id).is_some());
                    self.mode = Mode::Editor;
                    self.window_title = format!("{name} — Reel");
                    self.status = format!("Project {name} loaded.");
                    if let Some(src) = first_source {
                        // The editor previews through proxies: a heavy source
                        // starts its background proxy build here, and the
                        // preview opens whatever is best right now.
                        let path = self.proxies.preview_path(&src);
                        self.open_media_async(&path, true);
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
        // REEL_DEBUG_PLAY=1 — start playback as soon as media lands, so a
        // headless Xvfb check can watch the preview move without a keyboard.
        if self.debug_autoplay && self.player.as_ref().is_some_and(|p| !p.playing) {
            self.debug_autoplay = false;
            if let Some(p) = self.player.as_mut() {
                p.toggle_play();
            }
        }
        // REEL_DEBUG_AUDITION=1 — start a stabilization audition of the
        // first clip, so a headless check can watch the whole loop land.
        if std::env::var("REEL_DEBUG_AUDITION").as_deref() == Ok("1")
            && self.audition.is_none()
            && self.mode == Mode::Editor
        {
            if let Some(id) = self
                .project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .map(|c| c.id)
                .next()
            {
                std::env::remove_var("REEL_DEBUG_AUDITION");
                self.start_audition(id);
            }
        }
        // REEL_DEBUG_KEYMAP=1 — open the keyboard map (headless photography).
        if std::env::var("REEL_DEBUG_KEYMAP").as_deref() == Ok("1") {
            self.show_keymap = true;
        }
        // REEL_DEBUG_MONITOR=1 — source-preview the first pool item once.
        if std::env::var("REEL_DEBUG_MONITOR").as_deref() == Ok("1")
            && self.mode == Mode::Editor
            && self.source_preview.is_none()
        {
            self.project.absorb_sources_into_pool();
            if let Some(path) = self.project.pool.first().map(|i| i.path.clone()) {
                std::env::remove_var("REEL_DEBUG_MONITOR");
                self.preview_source(&path);
            }
        }
        // REEL_DEBUG_SELECT=1 — select the first clip, so headless checks can
        // photograph the clip panel (there is no pointer under Xvfb).
        if std::env::var("REEL_DEBUG_SELECT").as_deref() == Ok("1")
            && self.editor.selected.is_none()
        {
            self.show_scopes = true; // the headless check photographs these too
            self.scroll_to_scopes = true;
            self.editor.show_curve_lane = true;
            let first = self
                .project
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .map(|c| c.id)
                .next();
            self.editor.selected = first;
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

    /// Start/stop a WEBCAM recording — same finalize plumbing as the
    /// screen path; one recording at a time (whichever slot is live stops).
    #[cfg(target_os = "linux")]
    pub fn toggle_webcam_record(&mut self) {
        if self.recorder.is_some() {
            self.toggle_record(); // stop whatever is rolling
            return;
        }
        if self.rec_start_rx.is_none() {
            let (tx, rx) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send(capture::start_webcam_recording().map_err(|e| e.to_string()));
            });
            self.rec_start_rx = Some(rx);
            self.status = "Starting webcam recording…".into();
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
    /// Keep the overlay (PiP) preview players in step with the timeline:
    /// open them for the clips under the playhead, chase the main player's
    /// play/pause, correct drift, and land their frames on GPU textures the
    /// viewport draws. Always muted — the main player owns the audio.
    fn sync_overlay_previews(&mut self, gpu: &Gpu, egui: &mut EguiBackend) {
        self.transition_preview = None;
        if self.mode != Mode::Editor {
            // Leaving the editor drops the pool; the player screen has no PiP.
            self.overlay_previews.clear();
            return;
        }
        let t = self.editor.playhead;
        let want_playing = self.player.as_ref().is_some_and(|p| p.playing);

        // Overlay (PiP) clips under the playhead.
        let mut active: Vec<(u64, String, f64)> = self
            .project
            .tracks
            .iter()
            .filter(|tr| tr.kind == crate::edit::TrackKind::Overlay && !tr.muted)
            .flat_map(|tr| tr.clips.iter())
            .filter(|c| !c.adjustment && t >= c.start && t < c.end())
            .map(|c| (c.id, c.source.clone(), c.in_point + (t - c.start) * c.speed.max(0.01) as f64))
            .collect();

        // The incoming half of a crossfade: while the playhead is inside the
        // last `d` seconds of a clip whose successor fades in, that successor
        // plays here — so the fade previews as a fade, not a hard cut.
        let video_clips: Vec<crate::edit::Clip> = self
            .project
            .tracks
            .iter()
            .filter(|tr| tr.kind == crate::edit::TrackKind::Video)
            .flat_map(|tr| tr.clips.iter().cloned())
            .collect();
        for b in &video_clips {
            if b.transition_in <= 0.0 {
                continue;
            }
            let Some(a) = video_clips
                .iter()
                .filter(|c| c.end() <= b.start + 1e-6 && c.id != b.id)
                .max_by(|x, y| x.end().total_cmp(&y.end()))
            else {
                continue;
            };
            let d = b.transition_in.min(a.duration).min(b.duration);
            let fade_start = a.end() - d;
            if t >= fade_start && t < a.end() {
                let into = t - fade_start;
                let progress = (into / d).clamp(0.0, 1.0) as f32;
                active.push((b.id, b.source.clone(), b.in_point + into * b.speed.max(0.01) as f64));
                self.transition_preview = Some((b.id, progress, b.transition_kind));
                log::debug!("transition preview: clip {} at {progress:.2}", b.id);
                break;
            }
        }

        // Drop players whose clip has left the playhead.
        let keep: std::collections::HashSet<u64> = active.iter().map(|(id, _, _)| *id).collect();
        self.overlay_previews.retain(|k, _| keep.contains(k));

        for (id, source, src_t) in active {
            let path = self.proxies.preview_path(&source);
            let entry = match self.overlay_previews.entry(id) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    let Ok(mut p) = Player::open(&path) else { continue };
                    p.set_muted(true);
                    p.seek(src_t);
                    v.insert(OverlayPreview { player: p, tex: None, tex_id: None })
                }
            };
            let p = &mut entry.player;
            // A quarter-frame inset deserves a quarter-frame decode: cap the
            // pooled player's render size well under the viewport. (Audit
            // item C1 — these were decoding at full source size.)
            p.set_display_size(640.0, 360.0);
            if p.playing != want_playing {
                p.toggle_play();
            }
            // Chase, don't fight: nudge only when visibly out of step.
            if (p.position - src_t).abs() > 0.3 {
                p.seek(src_t);
            }
            p.update();
            if p.take_dirty() {
                if let Some(frame) = &p.current {
                    if frame.data.len() >= (frame.width * frame.height * 4) as usize {
                        let need_new = match &entry.tex {
                            Some(tx) => tx.width != frame.width || tx.height != frame.height,
                            None => true,
                        };
                        if need_new {
                            entry.tex = Some(VideoTexture::new(&gpu.device, frame.width, frame.height));
                            entry.tex_id = None;
                        }
                        let tex = entry.tex.as_ref().unwrap();
                        // mpv writes a padding byte where alpha lives. The
                        // main picture fixes that in its shader; these frames
                        // are also drawn by egui's own pipeline for the PiP
                        // inset, which honours alpha — so force it here.
                        let mut rgba = frame.data.clone();
                        for px in rgba.chunks_exact_mut(4) {
                            px[3] = 255;
                        }
                        tex.write(&gpu.queue, &rgba);
                        match entry.tex_id {
                            Some(id2) => egui.update_registered(id2, &gpu.device, &tex.view),
                            None => entry.tex_id = Some(egui.register_texture(&gpu.device, &tex.view)),
                        }
                    }
                }
            }
        }
    }

    /// Keep the live audio mix in step with the editor: the plan tracks the
    /// project, playback tracks the main player, position chases the
    /// playhead, and the main player is muted while the mixer speaks.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn sync_mixer(&mut self) {
        if self.mode == Mode::Editor && !self.mixer_attempted {
            self.mixer_attempted = true;
            self.mixer = crate::audio::Mixer::open();
        }
        let Some(mixer) = &self.mixer else { return };
        if self.mode != Mode::Editor {
            mixer.set_playing(false);
            // Give mpv its voice back for player mode.
            if let Some(p) = self.player.as_mut() {
                if p.muted && !self.user_muted {
                    p.set_muted(false);
                }
            }
            return;
        }
        // The mixer speaks for the timeline; mpv would double every voice.
        if let Some(p) = self.player.as_mut() {
            if !p.muted {
                p.set_muted(true);
            }
        }
        // Rebuild the plan after edits (and as decoded PCM arrives), at a
        // gentle cadence — building is cheap, but not per-frame cheap.
        let stale = self.editor.changed_at > self.mix_built_at
            || self.mix_built_at.elapsed() > std::time::Duration::from_millis(700);
        if stale {
            self.mix_built_at = std::time::Instant::now();
            let mut plan = crate::audio::Plan::default();
            plan.buses = self.project.tracks.len();
            let soloing = self.project.tracks.iter().any(|t| t.solo);
            let clips: Vec<(f32, f32, usize, crate::edit::Clip)> = self
                .project
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    !t.muted
                        && (!soloing || t.solo)
                        && matches!(
                            t.kind,
                            crate::edit::TrackKind::Video
                                | crate::edit::TrackKind::Audio
                                | crate::edit::TrackKind::Overlay
                        )
                })
                .flat_map(|(ti, t)| {
                    t.clips.iter().cloned().map(move |c| (t.gain_db, t.pan, ti, c))
                })
                .collect();
            for (track_gain, track_pan, bus, c) in clips {
                let Some(pcm) = self.samples.get(&c.source) else { continue };
                let avg = (c.source_len() / c.duration.max(1e-9)).max(0.01);
                plan.clips.push(crate::audio::PlanClip {
                    pcm,
                    start: c.start,
                    duration: c.duration,
                    in_point: c.in_point,
                    gain: crate::audio::db_to_gain(c.gain_db + track_gain),
                    fade_in: c.effects.fade_in,
                    fade_out: c.effects.fade_out,
                    speed: avg,
                    fx: c.audio.with_track_pan(track_pan),
                    fade_curve: c.audio.fade_curve,
                    bus,
                });
            }
            if let Some(rt) = &self.project.roomtone {
                if let Some(pcm) = self.samples.get(&rt.source.clone()) {
                    plan.roomtone = Some((pcm, crate::audio::db_to_gain(rt.gain_db)));
                }
            }
            if let Some(m) = &self.project.music {
                if let Some(pcm) = self.samples.get(&m.source.clone()) {
                    let total = crate::edit::render_duration(&self.project.export_segments());
                    let rate = if m.fit {
                        let src = pcm.frames() as f64 / crate::audio::RATE as f64;
                        (src / (total - m.start).max(0.5)).clamp(0.25, 4.0)
                    } else {
                        1.0
                    };
                    plan.music = Some(crate::audio::PlanMusic {
                        pcm,
                        start: m.start,
                        gain: crate::audio::db_to_gain(m.gain_db),
                        duck: m.duck,
                        fade: m.fade,
                        total,
                        rate,
                    });
                }
            }
            mixer.set_plan(plan);
        }
        let (playing, volume, muted) = self
            .player
            .as_ref()
            .map(|p| (p.playing, p.volume, self.user_muted))
            .unwrap_or((false, 100.0, false));
        mixer.set_playing(playing);
        mixer.set_master(if muted { 0.0 } else { (volume / 100.0) as f32 });
        // Chase the playhead; nudge only on real drift so playback stays
        // smooth (the clocks tick within a few ms of each other).
        if (mixer.position() - self.editor.playhead).abs() > 0.08 {
            mixer.seek(self.editor.playhead);
        }
    }

    /// The preview-device lattice for a clip's grade, if it has one.
    pub fn lut_view(&self, fx: Option<crate::effects::Effects>) -> Option<wgpu::TextureView> {
        let fx = fx?;
        if !fx.has_lattice() {
            return None;
        }
        let key = crate::lut::grade_key(&fx);
        self.lut_previews.get(&key).cloned().flatten()
    }

    /// The active clip's effect plugin, loaded (or None when unset,
    /// missing, or unparseable — the preview then shows the built-in look
    /// and the log says why).
    pub fn plugin_for(&self, fx: Option<crate::effects::Effects>) -> Option<std::sync::Arc<crate::plugins::Plugin>> {
        let idx = fx?.plugin?;
        let path = self.project.plugin_path(idx)?.to_string();
        match crate::plugins::load(&path) {
            Ok(p) => Some(p),
            Err(e) => {
                log::warn!("effect plugin {path}: {e}");
                None
            }
        }
    }

    /// The MAIN preview's lattice: the clip under the playhead plus any
    /// adjustment layers — the stacked bake, same as the render's.
    pub fn preview_lut(&self) -> Option<wgpu::TextureView> {
        let clip = self.project.clip_at(TrackKind::Video, self.editor.playhead)?;
        let (fx, _, _) = clip.animated(self.editor.playhead - clip.start);
        let stack = self.adjustment_stack(fx);
        if !stack.iter().any(|f| f.has_lattice()) {
            return None;
        }
        let refs: Vec<&crate::effects::Effects> = stack.iter().collect();
        let key = crate::lut::grade_key_stack(&refs);
        self.lut_previews.get(&key).cloned().flatten()
    }

    fn sync_lut_previews(&mut self, gpu: &Gpu) {
        // Bake every grade the project references; a shared look shares one
        // lattice, and edits to the curves re-bake under a new key. (Old
        // keys linger until the next session — a few hundred KB at most.)
        let wanted: Vec<crate::effects::Effects> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.effects.has_lattice())
            .map(|c| c.effects)
            .collect();
        // Bounded: editing curves mints a new key per tweak — cap the map so
        // a long grading session can't grow it without limit.
        if self.lut_previews.len() > 48 {
            self.lut_previews.clear(); // rebuilt below for what's in use
        }
        for fx in wanted {
            let key = crate::lut::grade_key(&fx);
            if self.lut_previews.contains_key(&key) {
                continue;
            }
            let base = fx
                .lut
                .and_then(|i| self.project.lut_path(i).map(String::from))
                .and_then(|p| crate::lut::load(&p).ok());
            let lattice = crate::lut::bake_grade(base.as_deref(), &fx);
            self.lut_previews
                .insert(key, Some(crate::lut::to_texture(&lattice, &gpu.device, &gpu.queue)));
        }
        // The playhead's STACK (clip + adjustment layers), when one is live.
        if self.mode == Mode::Editor {
            if let Some(clip) = self.project.clip_at(TrackKind::Video, self.editor.playhead) {
                let (fx, _, _) = clip.animated(self.editor.playhead - clip.start);
                let stack = self.adjustment_stack(fx);
                if stack.len() > 1 && stack.iter().any(|f| f.has_lattice()) {
                    let refs: Vec<&crate::effects::Effects> = stack.iter().collect();
                    let key = crate::lut::grade_key_stack(&refs);
                    if !self.lut_previews.contains_key(&key) {
                        let bases: Vec<Option<std::sync::Arc<crate::lut::Lut>>> = refs
                            .iter()
                            .map(|f| {
                                f.lut
                                    .and_then(|i| self.project.lut_path(i).map(String::from))
                                    .and_then(|p| crate::lut::load(&p).ok())
                            })
                            .collect();
                        let entries: Vec<(Option<&crate::lut::Lut>, &crate::effects::Effects)> =
                            refs.iter().zip(&bases).map(|(f, b)| (b.as_deref(), *f)).collect();
                        let lattice = crate::lut::bake_stack(&entries);
                        self.lut_previews.insert(
                            key,
                            Some(crate::lut::to_texture(&lattice, &gpu.device, &gpu.queue)),
                        );
                    }
                }
            }
        }
    }

    pub fn sync_frame(&mut self, gpu: &Gpu, egui: &mut EguiBackend) {
        self.sync_overlay_previews(gpu, egui);
        self.sync_lut_previews(gpu);
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        self.sync_mixer();
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

    /// Turn the dialog's current settings into a queued job.
    pub fn queue_current_export(&mut self, label: String) {
        let job = if self.export_timeline {
            let segments = self
                .project
                .export_segments_range(self.editor.range_in, self.editor.range_out);
            if segments.is_empty() {
                self.status = "Nothing on the timeline to queue.".into();
                return;
            }
            export::Job::Timeline {
                segments,
                project: (self.project.width, self.project.height, self.project.fps),
                captions: self.project.captions.clone(),
                caption_size: self.project.caption_size,
                titles: self.project.titles.clone(),
                music: self.project.music.clone(),
                roomtone: self.project.roomtone.clone(),
                overlays: self.project.overlay_segments(),
                markers: self.project.markers.clone(),
                marker_labels: self.project.marker_labels.clone(),
                luts: self.project.luts.clone(),
                plugins: self.project.plugins.clone(),
                audio_clips: self.project.audio_clips(),
            }
        } else {
            let Some(path) = self.media_path() else { return };
            let duration = self.player.as_ref().map(|p| p.info.duration).unwrap_or(0.0);
            export::Job::Source { path, duration }
        };
        let output = self.export_out.clone();
        if std::path::Path::new(&output).exists() {
            self.status = format!("{output} already exists — change the name first.");
            return;
        }
        self.queue.push(export::Queued { label: label.clone(), output, settings: self.export_settings.clone(), job });
        self.status = format!("Queued {label} ({} waiting).", self.queue.len_pending());
    }

    /// Save the project automatically, shortly after edits stop. Serialising
    /// happens here (it's a small JSON document) but the write goes to a
    /// worker thread, so a slow disk can never stall a frame.
    pub fn poll_autosave(&mut self) {
        // Nothing to save, or nothing changed.
        if !self.editor.dirty || self.project.export_segments().is_empty() {
            return;
        }
        // Debounce: wait for a quiet moment so a slider drag writes once.
        if self.editor.changed_at.elapsed() < std::time::Duration::from_millis(700) {
            return;
        }
        // Somewhere to put it: next to the first source, once.
        let path = match self.editor.project_path.clone() {
            Some(p) => p,
            None => {
                let Some(src) = self
                    .project
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .map(|c| c.source.clone())
                    .next()
                else {
                    return;
                };
                let p = std::path::Path::new(&src).with_extension("reel").to_string_lossy().into_owned();
                self.editor.project_path = Some(p.clone());
                p
            }
        };
        // Crash recovery: the resume point travels with the document.
        self.project.session = crate::edit::Session {
            playhead: self.editor.playhead,
            zoom: self.editor.px_per_s,
            scroll_x: self.editor.scroll_x,
            selected: self.editor.selected,
        };
        let json = match serde_json::to_string_pretty(&self.project) {
            Ok(j) => j,
            Err(e) => {
                self.status = format!("Could not save project: {e}");
                self.editor.dirty = false; // don't spin on a broken document
                return;
            }
        };
        self.editor.dirty = false;
        if !self.editor.announced_path {
            self.editor.announced_path = true;
            self.status = format!("Saving automatically to {path}");
        }
        std::thread::spawn(move || {
            if let Err(e) = crate::edit::write_atomic(&path, &json) {
                log::warn!("autosave failed for {path}: {e}");
            }
        });
    }

    /// Generate captions for the edit, entirely on this machine. One button:
    /// the model is fetched on first use, nothing is uploaded.
    pub fn start_captions(&mut self) {
        if self.captions_job.is_some() {
            return;
        }
        let Some(src) = self.media_path() else { return };
        self.captions_job = Some(crate::captions::start(&src, self.caption_model));
        self.status = "Captioning — this stays on your machine.".into();
    }

    /// Collect a finished captioning run.
    pub fn poll_captions(&mut self) {
        let Some(job) = &self.captions_job else { return };
        let st = job.state();
        if !st.finished {
            return;
        }
        self.captions_job = None;
        match st.error {
            Some(e) if e == "cancelled" => self.status = "Captioning cancelled.".into(),
            Some(e) => self.status = format!("Captions: {e}"),
            None => {
                // Cues come back in SOURCE time. Map each window onto every
                // place it survives in the edit, so trims, splits, reorders
                // and duplicated clips all caption correctly.
                let mut mapped = Vec::new();
                let src = self.media_path().unwrap_or_default();
                for cue in st.cues {
                    for (start, end) in self.project.map_source_window(&src, cue.start, cue.end) {
                        mapped.push(crate::captions::Cue {
                            start,
                            end,
                            text: cue.text.clone(),
                        });
                    }
                }
                mapped.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
                let n = mapped.len();
                self.editor.push_undo(&self.project);
                self.project.captions = mapped;
                self.status = format!("{n} captions added — they burn in on export.");
            }
        }
    }

    pub fn editor_copy(&mut self) {
        let Some(id) = self.editor.selected else { return };
        if let Some((clip, kind)) = self.project.clip_with_kind(id) {
            let name = clip.name.clone();
            self.editor.clipboard = Some((clip, kind));
            self.status = format!("Copied {name} — Ctrl+V pastes at the playhead.");
        }
    }

    pub fn editor_paste(&mut self) {
        let Some((clip, kind)) = self.editor.clipboard.clone() else {
            self.status = "Nothing copied yet (select a clip, then Ctrl+C).".into();
            return;
        };
        self.editor.push_undo(&self.project);
        let at = self.editor.playhead;
        // Track targeting: a clicked lane of the SAME kind receives the
        // paste (V1↔V2 count as the same family — both carry picture).
        let kind = self
            .editor
            .target_track
            .and_then(|tid| self.project.tracks.iter().find(|t| t.id == tid))
            .filter(|t| {
                use crate::edit::TrackKind::*;
                matches!(
                    (&t.kind, &kind),
                    (Video, Video) | (Overlay, Overlay) | (Audio, Audio)
                        | (Video, Overlay) | (Overlay, Video)
                )
            })
            .map(|t| t.kind.clone())
            .unwrap_or(kind);
        let id = self.project.paste_clip(&clip, at, kind);
        self.editor.selected = Some(id);
        self.editor.mark_changed();
        self.status = format!("Pasted at {at:.2}s — everything after it moved along.");
    }

    pub fn editor_duplicate(&mut self) {
        let Some(id) = self.editor.selected else { return };
        self.editor.push_undo(&self.project);
        if let Some(new_id) = self.project.duplicate_clip(id) {
            self.editor.selected = Some(new_id);
            self.editor.mark_changed();
            self.status = "Duplicated.".into();
        }
    }

    /// Drop a marker at the playhead, or lift the one already there.
    pub fn editor_toggle_marker(&mut self) {
        let t = self.editor.playhead;
        self.editor.push_undo(&self.project);
        // "Already there" has to mean visibly there, not bit-identical.
        let near = self.project.markers.iter().position(|m| (m - t).abs() < 0.05);
        match near {
            Some(i) => {
                self.project.markers.remove(i);
                self.status = "Marker removed.".into();
            }
            None => {
                self.project.markers.push(t);
                self.project.markers.sort_by(|a, b| a.total_cmp(b));
                self.status = format!("Marker at {t:.2}s (Ctrl+Left/Right to jump).");
            }
        }
        self.editor.mark_changed();
    }

    pub fn editor_jump_marker(&mut self, forward: bool) {
        let t = self.editor.playhead;
        let target = if forward {
            self.project.markers.iter().copied().find(|m| *m > t + 0.01)
        } else {
            self.project.markers.iter().copied().rev().find(|m| *m < t - 0.01)
        };
        if let Some(m) = target {
            self.seek_timeline(m);
        }
    }

    /// Export the frame under the playhead as a PNG, through the engine —
    /// so what lands on disk is the composed edit, not just the raw source.
    pub fn export_current_frame(&mut self) {
        let segments = self.project.export_segments();
        if segments.is_empty() {
            self.status = "Nothing on the timeline yet.".into();
            return;
        }
        let t = self.editor.playhead;
        let dir = self
            .editor
            .project_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let out = dir.join(format!("frame-{t:.2}s.png"));
        let settings = export::ExportSettings {
            hardware: false,
            ..self.export_settings.clone()
        };
        let overlays_owned = self.project.overlay_segments();
        let ov = export::Overlays {
            captions: &self.project.captions,
            caption_size: self.project.caption_size,
            titles: &self.project.titles,
            music: None,
            roomtone: None,
            overlays: &overlays_owned,
            markers: &[],
            marker_labels: &[],
            luts: &self.project.luts,
            plugins: &self.project.plugins,
            audio_clips: &[],
        };
        match crate::engine::render::still_png(
            &segments,
            &ov,
            (self.project.width, self.project.height, self.project.fps),
            &settings,
            t,
            &out.to_string_lossy(),
        ) {
            Ok(_) => self.status = format!("Frame saved: {}", out.display()),
            Err(e) => self.status = format!("Frame export failed: {e}"),
        }
    }

    /// Queue one timeline render per social preset — the whole publish
    /// matrix in a click. Outputs land beside the project, named by
    /// platform; already-queued or existing files are skipped, not clobbered.
    pub fn queue_all_platforms(&mut self) -> usize {
        let segments = self.project.export_segments();
        if segments.is_empty() {
            return 0;
        }
        let dir = self
            .editor
            .project_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        // {name}: the project's stem; {platform}: the preset's slug.
        let name = self
            .editor
            .project_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "publish".into());
        let mut queued = 0;
        for preset in export::Preset::ALL {
            let mut settings = self.export_settings.clone();
            preset.apply(&mut settings);
            let slug = preset.slug();
            let file = self
                .publish_template
                .replace("{name}", &name)
                .replace("{platform}", &slug);
            let file = if file.trim().is_empty() { format!("{name}-{slug}") } else { file };
            let out = dir.join(format!("{file}.mp4"));
            if out.exists() {
                continue;
            }
            // Per-platform caption choice: a toggled-off platform renders
            // clean, everything else burns the captions as usual.
            let captions = if self.publish_no_captions.contains(&slug) {
                Vec::new()
            } else {
                self.project.captions.clone()
            };
            self.queue.push(export::Queued {
                job: export::Job::Timeline {
                    segments: segments.clone(),
                    project: (self.project.width, self.project.height, self.project.fps),
                    captions,
                    caption_size: self.project.caption_size,
                    titles: self.project.titles.clone(),
                    music: self.project.music.clone(),
                    roomtone: self.project.roomtone.clone(),
                    overlays: self.project.overlay_segments(),
                    markers: self.project.markers.clone(),
                    marker_labels: self.project.marker_labels.clone(),
                    luts: self.project.luts.clone(),
                    plugins: self.project.plugins.clone(),
                    audio_clips: self.project.audio_clips(),
                },
                output: out.to_string_lossy().into_owned(),
                settings,
                label: preset.name.to_string(),
            });
            queued += 1;
        }
        queued
    }

    /// Advance the render queue; keeps the UI repainting while it works.
    pub fn poll_queue(&mut self) {
        self.queue.poll();
    }

    /// Audition stabilization: render JUST this clip's window — shake
    /// analysis and all — to a temp file, then play it. The only honest way
    /// to preview vidstab without paying its full decode in the live preview.
    pub fn start_audition(&mut self, clip_id: u64) {
        if self.project.clip(clip_id).is_none() {
            return;
        }
        // Reuse the exact clip→segment flattening the export uses, on a
        // scratch project holding only this clip.
        let mut scratch = self.project.clone();
        for t in &mut scratch.tracks {
            t.clips.retain(|c| c.id == clip_id);
        }
        let mut segs = scratch.export_segments();
        let [seg] = &mut segs[..] else { return };
        seg.stabilize = true;
        seg.transition_in = 0.0;
        let out = std::env::temp_dir()
            .join(format!("reel-audition-{}-{clip_id}.mp4", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let settings = ExportSettings {
            codec: export::Codec::H264,
            quality: export::Quality::Balanced,
            resolution: export::Resolution::H720,
            audio: export::AudioMode::Encode { kbps: 128 },
            hardware: false,
            target: None,
            fit: export::Fit::Letterbox,
            loudness: None,
            hdr_passthrough: false,
        };
        let overlays = export::Overlays {
            captions: &[],
            caption_size: self.project.caption_size,
            titles: &[],
            music: None,
            roomtone: None,
            overlays: &[],
            markers: &[],
            marker_labels: &[],
            luts: &self.project.luts,
            plugins: &self.project.plugins,
            audio_clips: &[],
        };
        match export::start_timeline_with_captions(
            &segs,
            &out,
            &settings,
            (self.project.width, self.project.height, self.project.fps),
            overlays,
        ) {
            Ok(job) => self.audition = Some((job, out, clip_id)),
            Err(e) => log::error!("audition failed to start: {e:#}"),
        }
    }

    /// Source monitor: play this file in the player while the edit stays
    /// loaded — I/O set marks, and Insert performs the three-point edit.
    pub fn preview_source(&mut self, path: &str) {
        self.src_mark_in = None;
        self.src_mark_out = None;
        self.source_preview = Some(path.to_string());
        self.open(path);
        self.mode = Mode::Player;
    }

    /// The three-point edit: source in/out (defaulting to the whole file)
    /// + the timeline playhead = a new V1 clip, and back to the editor.
    pub fn insert_source_into_edit(&mut self) {
        let Some(src) = self.source_preview.clone() else { return };
        let total = self
            .player
            .as_ref()
            .map(|p| p.info.duration)
            .filter(|d| *d > 0.0)
            .unwrap_or(0.0);
        let mark_in = self.src_mark_in.unwrap_or(0.0);
        let mark_out = self.src_mark_out.unwrap_or(total).max(mark_in + 0.05);
        let dur = (mark_out - mark_in).max(0.05);
        self.editor.push_undo(&self.project);
        self.project
            .add_clip(&src, crate::edit::TrackKind::Video, self.editor.playhead, mark_in, dur);
        self.project.pool_add(&src, "");
        self.editor.mark_changed();
        self.source_preview = None;
        self.src_mark_in = None;
        self.src_mark_out = None;
        self.mode = Mode::Editor;
        self.status = format!("Inserted {dur:.2}s at the playhead.");
    }

    /// Punch in: arm the recorder at the playhead and roll the timeline —
    /// you speak over the cut and the take lands exactly where you started.
    #[cfg(target_os = "linux")]
    pub fn toggle_record_voice(&mut self) {
        if self.punch_in_at.is_some() {
            self.finish_record_voice();
            return;
        }
        if self.voice_rec.is_none() {
            self.voice_rec = crate::audio::Recorder::open();
        }
        let Some(rec) = &self.voice_rec else {
            log::warn!("no microphone available to record from");
            return;
        };
        rec.start();
        self.punch_in_at = Some(self.editor.playhead);
        // Roll playback so the take is spoken against the cut.
        if let Some(p) = self.player.as_mut() {
            if !p.playing {
                p.toggle_play();
            }
        }
    }

    /// Stop the take, write the WAV beside the project, drop it on A1 at
    /// the punch-in point.
    #[cfg(target_os = "linux")]
    pub fn finish_record_voice(&mut self) {
        let Some(at) = self.punch_in_at.take() else { return };
        let Some(rec) = &self.voice_rec else { return };
        let samples = rec.stop();
        let seconds = samples.len() as f64 / (crate::audio::RATE as usize * 2) as f64;
        if seconds < 0.2 {
            log::warn!("take too short ({seconds:.2}s) — nothing added");
            return;
        }
        // Somewhere to put the file: beside the project when it has a home.
        let dir = self
            .editor
            .project_path
            .as_ref()
            .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| {
                let d = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join(".local/share/reel/recordings");
                let _ = std::fs::create_dir_all(&d);
                d
            });
        let n = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.name.starts_with("voiceover"))
            .count()
            + 1;
        let path = dir.join(format!("voiceover-{n}.wav"));
        if let Err(e) = crate::audio::write_wav(&path, &samples) {
            log::error!("could not write the take: {e}");
            return;
        }
        self.editor.push_undo(&self.project);
        self.project.add_clip(
            &path.to_string_lossy(),
            crate::edit::TrackKind::Audio,
            at,
            0.0,
            seconds,
        );
        self.editor.mark_changed();
        log::info!("voiceover: {:.2}s onto A1 at {at:.2}s ({})", seconds, path.display());
    }

    /// Nest another project: render it flat on a worker, then (on poll)
    /// drop the flat file on the timeline as a compound clip.
    pub fn start_nesting(&mut self, reel: &str, add_clip: bool) {
        if self.nesting.is_some() {
            return; // one at a time — renders are heavy
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.nesting = Some((rx, add_clip));
        let reel = reel.to_string();
        self.status = format!("Rendering nested edit {reel}…");
        std::thread::spawn(move || {
            let r = crate::compound::render_flat(&reel)
                .map(|flat| (flat.to_string_lossy().into_owned(), reel));
            let _ = tx.send(r);
        });
    }

    pub fn poll_nesting(&mut self) {
        // Finished nest renders land first.
        if let Some((rx, add)) = &self.nesting {
            let add = *add;
            match rx.try_recv() {
                Ok(Ok((flat, reel))) => {
                    self.nesting = None;
                    if add {
                        self.editor.push_undo(&self.project);
                        let dur = crate::video::decoder::probe(&flat)
                            .map(|i| i.duration)
                            .unwrap_or(1.0);
                        let id = self.project.add_clip(
                            &flat,
                            crate::edit::TrackKind::Video,
                            self.editor.playhead,
                            0.0,
                            dur,
                        );
                        if let Some(c) = self.project.clip_mut(id) {
                            c.nested = Some(reel.clone());
                            c.name = std::path::Path::new(&reel)
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "nested".into());
                        }
                        self.editor.selected = Some(id);
                        self.editor.mark_changed();
                        self.status = "Nested edit added at the playhead.".into();
                    } else {
                        self.status =
                            "Nested edit re-rendered — it plays fresh on the next seek.".into();
                    }
                }
                Ok(Err(e)) => {
                    self.nesting = None;
                    self.status = format!("Nested render failed: {e:#}");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.nesting = None,
            }
        }
        // Staleness sweep, gently: every few seconds, refresh ONE stale
        // compound in the background.
        if self.mode != Mode::Editor || self.nesting.is_some() {
            return;
        }
        let due = self
            .nest_checked
            .map(|t| t.elapsed() > std::time::Duration::from_secs(3))
            .unwrap_or(true);
        if !due {
            return;
        }
        self.nest_checked = Some(std::time::Instant::now());
        let stale: Option<String> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter_map(|c| c.nested.clone())
            .find(|r| crate::compound::is_stale(r));
        if let Some(reel) = stale {
            self.start_nesting(&reel, false);
        }
    }

    /// Kick off a subject track for the clip's power window on a worker —
    /// the decode takes seconds and must never block a frame.
    pub fn start_tracking(&mut self, clip_id: u64) {
        let Some(clip) = self.project.clip(clip_id).cloned() else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        self.tracking = Some((rx, clip_id));
        std::thread::spawn(move || {
            let _ = tx.send(crate::track::track_clip(&clip));
        });
    }

    /// Land finished tracks: the path becomes MaskX/MaskY keyframes, played
    /// by the same `animated()` everything else uses.
    pub fn poll_tracking(&mut self) {
        let Some((rx, id)) = &self.tracking else { return };
        let id = *id;
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.tracking = None;
                return;
            }
        };
        self.tracking = None;
        match result {
            Ok((kx, ky)) => {
                self.editor.push_undo(&self.project);
                if let Some(c) = self.project.clip_mut(id) {
                    c.keys.retain(|(q, _)| {
                        !matches!(q, crate::edit::Param::MaskX | crate::edit::Param::MaskY)
                    });
                    c.keys.push((crate::edit::Param::MaskX, kx));
                    c.keys.push((crate::edit::Param::MaskY, ky));
                }
                self.editor.mark_changed();
            }
            Err(e) => log::error!("tracking failed: {e:#}"),
        }
    }

    /// When the audition render lands, open it in the player. E returns to
    /// the editor exactly where it was — the project is untouched.
    pub fn poll_audition(&mut self) {
        let Some((job, _, _)) = &self.audition else { return };
        let st = job.state();
        if !st.finished {
            return;
        }
        let (_, out, _) = self.audition.take().unwrap();
        if let Some(e) = st.error {
            log::error!("audition render failed: {e}");
            return;
        }
        self.open(&out);
        self.mode = Mode::Player;
    }

    /// Output path for a preset export: `<name>-<platform>.mp4`, so the
    /// TikTok cut and the YouTube cut sit side by side without clobbering.
    pub fn preset_output(&self, p: &export::Preset) -> String {
        let base = self
            .media_path()
            .or_else(|| self.editor.project_path.clone())
            .unwrap_or_else(|| "video".into());
        let path = std::path::Path::new(&base);
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "video".into());
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let slug = p.slug();
        let ext = p.codec.extension();
        let mut candidate = dir.join(format!("{stem}-{slug}.{ext}"));
        let mut n = 1;
        while candidate.exists() {
            candidate = dir.join(format!("{stem}-{slug}-{n}.{ext}"));
            n += 1;
        }
        candidate.to_string_lossy().into_owned()
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
            // Timeline → source through the clip's speed curve — a 2× clip
            // consumes two source seconds per timeline second, and a ramp
            // consumes its integral. Mapping linearly here made every seek
            // into a sped clip land on the wrong frame.
            let want = in_point
                + self
                    .project
                    .clip(id)
                    .map(|c| c.source_offset_at(self.editor.playhead - start))
                    .unwrap_or(self.editor.playhead - start);
            // The preview may be playing this source's PROXY — compare and
            // switch against the preview path, never the original, or every
            // seek would bounce back to the heavy file.
            let path = self.proxies.preview_path(&src);
            if let Some(player) = self.player.as_mut() {
                if path == player.path {
                    player.seek(want);
                } else {
                    player.switch_source(&path, want);
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
        let player_speed = player.speed;
        let player_path = player.path.clone();
        let active = self
            .editor
            .active_clip
            .and_then(|id| self.project.clip(id))
            .filter(|c| {
                c.source == player_path || self.proxies.is_proxy(&player_path)
            })
            .or_else(|| self.project.clip_at(TrackKind::Video, self.editor.playhead))
            .cloned();
        let Some(clip) = active else { return };
        self.editor.active_clip = Some(clip.id);
        // The preview PLAYS at the clip's rate: a 2× clip moves at 2×, and a
        // ramp approximates with its average (the render walks the true
        // curve). Without this the picture ambles at 1× while the timeline
        // crawls or sprints.
        let clip_rate = (clip.source_len() / clip.duration.max(1e-9)).clamp(0.05, 20.0);
        let want_rate = (clip_rate * self.user_rate).clamp(0.05, 20.0);
        if (player_speed - want_rate).abs() > 0.01 {
            if let Some(p) = self.player.as_mut() {
                p.set_speed(want_rate);
            }
        }
        // The active clip's quarter-turn, mirrored by mpv (tracked so the
        // property isn't spammed every frame).
        let want_rot = clip.effects.rotate % 4;
        if self.preview_rotate != want_rot {
            self.preview_rotate = want_rot;
            if let Some(p) = self.player.as_mut() {
                p.set_rotate(want_rot);
            }
        }
        // Stop at the out-marker when a range is set.
        if let Some(out) = self.editor.range_out {
            if self.editor.playhead >= out {
                if let Some(p) = self.player.as_mut() {
                    if p.playing {
                        p.toggle_play();
                    }
                }
                self.editor.playhead = out;
                return;
            }
        }
        if pos <= clip.in_point + clip.source_len() + 0.02 {
            // Source → timeline through the inverse of the speed curve, so
            // the playhead crosses the clip in its TIMELINE duration. The
            // picture's own pace is handled below by driving mpv at the
            // clip's rate.
            self.editor.playhead =
                clip.start + clip.output_time_for_source((pos - clip.in_point).max(0.0));
        } else {
            match self.project.clip_after(TrackKind::Video, clip.start).cloned() {
                Some(next) => {
                    // A transition's head already PLAYED — blended over this
                    // clip's tail by the transition preview. Resume the next
                    // clip past it, so edit time flows continuously instead
                    // of replaying the overlap. (One truth of time.)
                    let d = next
                        .transition_in
                        .min(clip.duration)
                        .min(next.duration)
                        .max(0.0);
                    let skip_src = next.source_offset_at(d);
                    self.editor.playhead = next.start + d;
                    self.editor.active_clip = Some(next.id);
                    let path = self.proxies.preview_path(&next.source);
                    let player = self.player.as_mut().unwrap();
                    if path == player.path {
                        player.seek(next.in_point + skip_src);
                    } else {
                        // Multi-source timeline: roll the preview onto the
                        // next clip's file without rebuilding the player.
                        player.switch_source(&path, next.in_point + skip_src);
                    }
                }
                None => {
                    // End of the edit.
                    if let Some(p) = self.player.as_mut() {
                        if p.playing {
                            p.toggle_play();
                        }
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

    /// Ripple delete: remove the selected clip and close the hole
    /// (Shift+Delete). Falls back to the clip under the playhead.
    pub fn editor_ripple_delete(&mut self) {
        let id = self
            .editor
            .selected
            .or_else(|| self.project.clip_at(TrackKind::Video, self.editor.playhead).map(|c| c.id));
        let Some(id) = id else {
            self.status = "Nothing selected to ripple-delete.".into();
            return;
        };
        self.editor.push_undo(&self.project);
        let removed = self.project.ripple_delete(id);
        self.editor.selected = None;
        self.status = if removed > 0.0 {
            format!("Rippled out {removed:.2}s.")
        } else {
            "Nothing to remove there.".into()
        };
    }

    /// Q / W — trim the clip under the playhead back to it and close up.
    pub fn editor_ripple_trim(&mut self, head: bool) {
        self.editor.push_undo(&self.project);
        let removed = self.project.ripple_trim_to_playhead(self.editor.playhead, head);
        if removed <= 0.0 {
            self.editor.undo(&mut self.project); // nothing happened; don't leave an undo step
            self.status = "No edit to trim at the playhead.".into();
            return;
        }
        if head {
            // The material before the playhead is gone, so the playhead is
            // now where that clip begins.
            let t = self.editor.playhead - removed;
            self.seek_timeline(t.max(0.0));
        } else {
            self.seek_timeline(self.editor.playhead);
        }
        self.status = format!("Trimmed {removed:.2}s and closed up.");
    }

    /// Delete the selected clip (Del).
    pub fn editor_delete(&mut self) {
        let mut doomed: Vec<u64> = self.editor.multi.iter().copied().collect();
        if let Some(id) = self.editor.selected {
            if !doomed.contains(&id) {
                doomed.push(id);
            }
        }
        if doomed.is_empty() {
            return;
        }
        self.editor.push_undo(&self.project);
        let n = doomed.len();
        for id in doomed {
            self.project.delete_clip(id);
        }
        self.editor.selected = None;
        self.editor.multi.clear();
        self.status = if n == 1 {
            "Clip deleted.".into()
        } else {
            format!("{n} clips deleted.")
        };
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
            || self.overlay_previews.values().any(|o| o.player.wants_redraw())
            || self.export.as_ref().map(|j| !j.state().finished).unwrap_or(false)
            || self.queue.is_busy()
            || self.captions_job.is_some()
            || self.waveforms.is_busy()
            || self.thumbs.is_busy()
            || self.samples.is_busy()
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

    /// What the viewport should apply to the picture right now: the clip
    /// under the playhead's colour adjustments, and its fade level at this
    /// instant. Only in the editor — the player shows media as it is.
    pub fn preview_effects(&self) -> (Option<crate::effects::Effects>, f32) {
        if self.mode != Mode::Editor {
            return (None, 1.0);
        }
        let Some(clip) = self.project.clip_at(TrackKind::Video, self.editor.playhead) else {
            return (None, 1.0);
        };
        let t = self.editor.playhead - clip.start;
        // Keyframes evaluated HERE, in the same call the frame server makes —
        // an animated exposure previews mid-ramp exactly as it renders.
        let (fx, _, opacity) = clip.animated(t);
        // Adjustment layers over the playhead compose onto the preview,
        // exactly as the frame server composes them into the render.
        let stack = self.adjustment_stack(fx);
        let composed = if stack.len() > 1 {
            let refs: Vec<&crate::effects::Effects> = stack.iter().collect();
            crate::effects::Effects::compose_stack(&refs)
        } else {
            fx
        };
        (Some(composed), fx.fade_alpha(t, clip.duration) * opacity)
    }

    /// The active grade stack at the playhead: the clip's own effects plus
    /// every adjustment layer covering this moment, in track order.
    pub fn adjustment_stack(&self, base: crate::effects::Effects) -> Vec<crate::effects::Effects> {
        let mut stack = vec![base];
        let t = self.editor.playhead;
        for tr in self.project.tracks.iter().filter(|t| t.kind == TrackKind::Overlay && !t.muted) {
            for c in &tr.clips {
                if c.adjustment && t >= c.start && t < c.end() {
                    stack.push(c.effects);
                }
            }
        }
        stack
    }

    /// A view of the current picture for Reel's own render pass.
    pub fn tex_view(&self) -> Option<wgpu::TextureView> {
        self.tex
            .as_ref()
            .map(|t| t.texture.create_view(&wgpu::TextureViewDescriptor::default()))
    }
}

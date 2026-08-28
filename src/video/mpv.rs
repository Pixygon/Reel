//! libmpv playback backend — the Milestone 1 hot path. mpv brings hardware
//! decode (VA-API/D3D11VA, copy-back for now), correct colour conversion,
//! audio output with real A/V sync, subtitles and frame-exact seeking; we
//! drive it through its render API and hand frames up as the same `Frame`
//! the subprocess decoder produces, so `Player`'s public surface is untouched.
//!
//! libmpv is loaded at **runtime** (dlopen), never linked: machines without it
//! (and the Windows cross-build) fall back to the ffmpeg-subprocess decoder.
//! This step uses mpv's software render target (mpv decodes + converts, we
//! upload RGBA); the zero-copy GPU surface is the next roadmap step.

use super::decoder::{Frame, VideoInfo};
use anyhow::{anyhow, bail, Result};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

// ---- raw libmpv ABI (client.h + render.h, stable since libmpv 1.x) ----

const MPV_FORMAT_FLAG: c_int = 3;
const MPV_FORMAT_INT64: c_int = 4;
const MPV_FORMAT_DOUBLE: c_int = 5;

const MPV_EVENT_NONE: c_int = 0;
const MPV_EVENT_SHUTDOWN: c_int = 1;
const MPV_EVENT_END_FILE: c_int = 7;
const MPV_EVENT_FILE_LOADED: c_int = 8;

const MPV_RENDER_PARAM_INVALID: c_int = 0;
const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;

const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

macro_rules! mpv_fns {
    ($($name:ident : $ty:ty),+ $(,)?) => {
        pub struct Lib {
            $( $name: $ty, )+
            // Keep the dlopen handle alive as long as the fn pointers above.
            _lib: libloading::Library,
        }
        impl Lib {
            unsafe fn from(lib: libloading::Library) -> Result<Self> {
                $(
                    let $name: $ty = *lib
                        .get(concat!(stringify!($name), "\0").as_bytes())
                        .map_err(|e| anyhow!("libmpv missing {}: {e}", stringify!($name)))?;
                )+
                Ok(Self { $( $name, )+ _lib: lib })
            }
        }
    };
}

mpv_fns! {
    mpv_create: unsafe extern "C" fn() -> *mut c_void,
    mpv_initialize: unsafe extern "C" fn(*mut c_void) -> c_int,
    mpv_terminate_destroy: unsafe extern "C" fn(*mut c_void),
    mpv_set_option_string: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    mpv_command: unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> c_int,
    mpv_set_property: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    mpv_get_property: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    mpv_wait_event: unsafe extern "C" fn(*mut c_void, f64) -> *mut MpvEvent,
    mpv_error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    mpv_render_context_create:
        unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut RenderParam) -> c_int,
    mpv_render_context_update: unsafe extern "C" fn(*mut c_void) -> u64,
    mpv_render_context_render: unsafe extern "C" fn(*mut c_void, *mut RenderParam) -> c_int,
    mpv_render_context_free: unsafe extern "C" fn(*mut c_void),
}

#[cfg(target_os = "linux")]
const LIB_NAMES: &[&str] = &["libmpv.so.2", "libmpv.so", "libmpv.so.1"];
#[cfg(target_os = "windows")]
const LIB_NAMES: &[&str] = &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"];
#[cfg(target_os = "macos")]
const LIB_NAMES: &[&str] = &["libmpv.2.dylib", "libmpv.dylib"];

/// The process-wide libmpv, loaded on first use. `None` means it isn't on this
/// machine (or REEL_BACKEND=ffmpeg forced it off) — callers fall back.
pub fn lib() -> Option<Arc<Lib>> {
    static LIB: OnceLock<Option<Arc<Lib>>> = OnceLock::new();
    LIB.get_or_init(|| {
        if std::env::var("REEL_BACKEND").as_deref() == Ok("ffmpeg") {
            log::info!("REEL_BACKEND=ffmpeg — skipping libmpv");
            return None;
        }
        for name in LIB_NAMES {
            match unsafe { libloading::Library::new(name) } {
                Ok(l) => match unsafe { Lib::from(l) } {
                    Ok(lib) => {
                        log::info!("playback backend: libmpv ({name})");
                        return Some(Arc::new(lib));
                    }
                    Err(e) => log::warn!("{name}: {e}"),
                },
                Err(_) => continue,
            }
        }
        log::info!("libmpv not found — using ffmpeg-subprocess backend");
        None
    })
    .clone()
}

/// One open media file driven by libmpv.
pub struct MpvPlayer {
    lib: Arc<Lib>,
    handle: *mut c_void,
    render: *mut c_void,
    pub info: VideoInfo,
    /// False for pure audio (no video track, no cover art): nothing to render.
    pub has_video: bool,
    /// True when the "video" is an audio file's embedded cover art.
    pub albumart: bool,
    /// (width, height, has_video) before a visualizer took over.
    orig_video: Option<(u32, u32, bool)>,
    /// Size mpv renders into — the on-screen size, never larger than the
    /// source. Rendering a 4K frame just to draw it 1280px wide costs ~9×
    /// the conversion, copy and upload for pixels nobody sees.
    render_size: Option<(u32, u32)>,
}

// SAFETY: libmpv's client API is documented fully thread-safe, and the render
// context may be used from any thread as long as calls aren't concurrent.
// Reel opens the player on a worker thread (so the UI never blocks on
// demuxing) and then hands it to the UI thread, which does all further calls.
unsafe impl Send for MpvPlayer {}

impl MpvPlayer {
    pub fn open(lib: Arc<Lib>, path: &str) -> Result<Self> {
        let handle = unsafe { (lib.mpv_create)() };
        if handle.is_null() {
            bail!("mpv_create returned null");
        }
        // Wrap immediately so any early bail! below still terminates the core.
        let mut p = Self {
            lib,
            handle,
            render: ptr::null_mut(),
            info: VideoInfo { width: 0, height: 0, fps: 30.0, duration: 0.0 },
            has_video: false,
            albumart: false,
            orig_video: None,
            render_size: None,
        };

        // Options must land before mpv_initialize. `config=no`: never load the
        // user's mpv.conf — Reel owns its playback behaviour.
        for (k, v) in [
            ("config", "no"),
            ("terminal", "no"),
            ("vo", "libmpv"),
            // Start decoding in software: hwdec probing (CUDA/VAAPI init)
            // costs ~half a second before the first pixel. `enable_hwdec`
            // upgrades once playback is rolling.
            ("hwdec", "no"),
            ("keep-open", "yes"),
            ("pause", "yes"),
            ("input-default-bindings", "no"),
            ("audio-client-name", "reel"),
            // Cold-open speed: no built-in scripts (ytdl_hook etc.) — Reel
            // opens local files; script init measurably delays FILE_LOADED.
            ("load-scripts", "no"),
            ("ytdl", "no"),
        ] {
            p.check(unsafe {
                (p.lib.mpv_set_option_string)(
                    p.handle,
                    cstr(k).as_ptr(),
                    cstr(v).as_ptr(),
                )
            })
            .map_err(|e| anyhow!("mpv option {k}={v}: {e}"))?;
        }
        crate::timing!("mpv options set");
        p.check(unsafe { (p.lib.mpv_initialize)(p.handle) })
            .map_err(|e| anyhow!("mpv_initialize: {e}"))?;
        crate::timing!("mpv initialized");

        // Software render target: mpv decodes (hw where possible), converts to
        // RGBA with proper colour management, we upload. See module docs.
        let api = cstr("sw");
        let mut params = [
            RenderParam { kind: MPV_RENDER_PARAM_API_TYPE, data: api.as_ptr() as *mut c_void },
            RenderParam { kind: MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
        ];
        let mut render = ptr::null_mut();
        p.check(unsafe {
            (p.lib.mpv_render_context_create)(&mut render, p.handle, params.as_mut_ptr())
        })
        .map_err(|e| anyhow!("mpv render context: {e}"))?;
        p.render = render;

        crate::timing!("mpv render ctx ready");
        p.command(&["loadfile", path])?;

        // Block until the file is demuxed (or fails) so `open` can return real
        // metadata, matching the subprocess backend's synchronous probe.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ev = unsafe { &*(p.lib.mpv_wait_event)(p.handle, 0.25) };
            match ev.event_id {
                MPV_EVENT_FILE_LOADED => break,
                MPV_EVENT_END_FILE | MPV_EVENT_SHUTDOWN => {
                    bail!("mpv could not open {path}");
                }
                _ => {}
            }
            if Instant::now() > deadline {
                bail!("mpv timed out opening {path}");
            }
        }
        crate::timing!("mpv FILE_LOADED");

        let width = p.get_i64("width").unwrap_or(0) as u32;
        let height = p.get_i64("height").unwrap_or(0) as u32;
        let duration = p.get_f64("duration").unwrap_or(0.0);
        p.has_video = width > 0 && height > 0;
        p.albumart = p.get_flag("current-tracks/video/albumart");
        // Pure audio is welcome — it just needs *something* to play.
        if !p.has_video && duration <= 0.0 {
            bail!("nothing playable in {path}");
        }
        let fps = p
            .get_f64("container-fps")
            .or_else(|| p.get_f64("estimated-vf-fps"))
            .filter(|f| *f > 0.0)
            .unwrap_or(30.0);
        p.info = VideoInfo { width, height, fps, duration };
        Ok(p)
    }

    pub fn set_pause(&mut self, paused: bool) {
        let mut flag: c_int = paused as c_int;
        let _ = unsafe {
            (self.lib.mpv_set_property)(
                self.handle,
                cstr("pause").as_ptr(),
                MPV_FORMAT_FLAG,
                &mut flag as *mut c_int as *mut c_void,
            )
        };
    }

    /// Frame-exact seek: mpv decodes from the keyframe and steps to the target.
    pub fn seek(&mut self, secs: f64) {
        let _ = self.command(&["seek", &format!("{secs:.4}"), "absolute+exact"]);
    }

    /// Load a different file into this same mpv instance — the multi-source
    /// timeline preview: far cheaper than tearing down and rebuilding the
    /// player (no core init, no audio-device re-open).
    pub fn load_file(&mut self, path: &str, start: f64) -> Result<()> {
        self.command(&["loadfile", path])?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ev = unsafe { &*(self.lib.mpv_wait_event)(self.handle, 0.05) };
            match ev.event_id {
                MPV_EVENT_FILE_LOADED => break,
                MPV_EVENT_END_FILE | MPV_EVENT_SHUTDOWN => bail!("mpv could not open {path}"),
                _ => {}
            }
            if Instant::now() > deadline {
                bail!("mpv timed out opening {path}");
            }
        }
        let width = self.get_i64("width").unwrap_or(0) as u32;
        let height = self.get_i64("height").unwrap_or(0) as u32;
        self.has_video = width > 0 && height > 0;
        self.albumart = self.get_flag("current-tracks/video/albumart");
        self.orig_video = None;
        let fps = self
            .get_f64("container-fps")
            .or_else(|| self.get_f64("estimated-vf-fps"))
            .filter(|f| *f > 0.0)
            .unwrap_or(30.0);
        self.info = VideoInfo { width, height, fps, duration: self.get_f64("duration").unwrap_or(0.0) };
        if start > 0.0 {
            self.seek(start);
        }
        Ok(())
    }

    /// Ask mpv to render at this size (already aspect-matched to the video by
    /// the caller, and never upscaled). Returns true when it changed, so the
    /// caller can drop the stale frame.
    pub fn set_render_size(&mut self, w: u32, h: u32) -> bool {
        let want = (w.max(2) / 2 * 2, h.max(2) / 2 * 2); // even dims
        if self.render_size == Some(want) {
            return false;
        }
        self.render_size = Some(want);
        true
    }

    /// The size mpv is currently rendering into.
    pub fn render_size(&self) -> (u32, u32) {
        self.render_size.unwrap_or((self.info.width, self.info.height))
    }

    /// Switch to hardware decode (copy-back). Called shortly after playback
    /// starts — mpv reinitializes the decoder in the background.
    pub fn enable_hwdec(&mut self) {
        let _ = self.command(&["set", "hwdec", "auto-copy-safe"]);
    }

    /// Step exactly one frame forward/back; mpv pauses as part of the step.
    pub fn frame_step(&mut self, forward: bool) {
        let _ = self.command(&[if forward { "frame-step" } else { "frame-back-step" }]);
    }

    /// 0–130, mpv's own scale (100 = source level, above amplifies).
    pub fn set_volume(&mut self, vol: f64) {
        self.set_f64("volume", vol);
    }

    pub fn set_muted(&mut self, muted: bool) {
        let mut flag: c_int = muted as c_int;
        let _ = unsafe {
            (self.lib.mpv_set_property)(
                self.handle,
                cstr("mute").as_ptr(),
                MPV_FORMAT_FLAG,
                &mut flag as *mut c_int as *mut c_void,
            )
        };
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.set_f64("speed", speed);
    }

    /// Play backwards (J in every NLE). mpv can genuinely decode in reverse;
    /// it is expensive, so callers should keep it to shuttle speeds.
    pub fn set_direction(&mut self, backward: bool) {
        let _ = self.command(&["set", "play-direction", if backward { "backward" } else { "forward" }]);
    }

    pub fn set_looping(&mut self, looping: bool) {
        let _ = self.command(&["set", "loop-file", if looping { "inf" } else { "no" }]);
    }

    /// Route the audio through a lavfi visualizer graph whose video output
    /// becomes the rendered "video" track (`None` restores the original
    /// video/cover-art/none state). `size` is the graph's output size.
    pub fn set_visualizer(&mut self, graph: Option<(&str, (u32, u32))>) {
        match graph {
            Some((g, (w, h))) => {
                if self.command(&["set", "lavfi-complex", g]).is_ok() {
                    if self.orig_video.is_none() {
                        self.orig_video = Some((self.info.width, self.info.height, self.has_video));
                    }
                    self.info.width = w;
                    self.info.height = h;
                    self.has_video = true;
                }
            }
            None => {
                let _ = self.command(&["set", "lavfi-complex", ""]);
                if let Some((w, h, hv)) = self.orig_video.take() {
                    self.info.width = w;
                    self.info.height = h;
                    self.has_video = hv;
                }
            }
        }
    }

    fn set_f64(&mut self, name: &str, mut v: f64) {
        let _ = unsafe {
            (self.lib.mpv_set_property)(
                self.handle,
                cstr(name).as_ptr(),
                MPV_FORMAT_DOUBLE,
                &mut v as *mut f64 as *mut c_void,
            )
        };
    }

    /// Pump events + timing. When mpv has a new frame ready for display (paced
    /// by mpv's own A/V clock), renders it into `slot` — reusing the old
    /// frame's allocation — and returns true. Otherwise leaves `slot` alone.
    pub fn update(&mut self, slot: &mut Option<Frame>) -> bool {
        // Drain the event queue; we only care that it doesn't back up.
        loop {
            let ev = unsafe { &*(self.lib.mpv_wait_event)(self.handle, 0.0) };
            if ev.event_id == MPV_EVENT_NONE {
                break;
            }
        }

        let flags = unsafe { (self.lib.mpv_render_context_update)(self.render) };
        if flags & MPV_RENDER_UPDATE_FRAME == 0 || !self.has_video {
            return false;
        }

        let (w, h) = self.render_size.unwrap_or((self.info.width, self.info.height));
        let len = (w * h * 4) as usize;
        let mut buf = match slot.take() {
            Some(f) if f.data.len() == len => f.data,
            _ => vec![0u8; len],
        };

        let mut size = [w as c_int, h as c_int];
        let format = cstr("rgb0");
        let mut stride: usize = (w * 4) as usize;
        let mut params = [
            RenderParam { kind: MPV_RENDER_PARAM_SW_SIZE, data: size.as_mut_ptr() as *mut c_void },
            RenderParam { kind: MPV_RENDER_PARAM_SW_FORMAT, data: format.as_ptr() as *mut c_void },
            RenderParam { kind: MPV_RENDER_PARAM_SW_STRIDE, data: &mut stride as *mut usize as *mut c_void },
            RenderParam { kind: MPV_RENDER_PARAM_SW_POINTER, data: buf.as_mut_ptr() as *mut c_void },
            RenderParam { kind: MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
        ];
        let t_render = Instant::now();
        let r = unsafe { (self.lib.mpv_render_context_render)(self.render, params.as_mut_ptr()) };
        if r < 0 {
            log::warn!("mpv render: {}", self.err(r));
            return false;
        }
        let render_us = t_render.elapsed().as_micros();
        // No CPU alpha fixup: "rgb0"'s padding byte lands in the alpha
        // channel, and Reel's video shader forces opacity instead — a whole
        // pass over every pixel of every frame, deleted (video.wgsl).
        crate::perf::note_decode(render_us as f64, 0.0);
        *slot = Some(Frame { data: buf, width: w, height: h, pts: self.position() });
        true
    }

    pub fn position(&self) -> f64 {
        self.get_f64("time-pos").unwrap_or(0.0)
    }

    pub fn eof_reached(&self) -> bool {
        self.get_flag("eof-reached")
    }

    fn get_flag(&self, name: &str) -> bool {
        let mut flag: c_int = 0;
        let r = unsafe {
            (self.lib.mpv_get_property)(
                self.handle,
                cstr(name).as_ptr(),
                MPV_FORMAT_FLAG,
                &mut flag as *mut c_int as *mut c_void,
            )
        };
        r >= 0 && flag != 0
    }

    fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        self.check(unsafe { (self.lib.mpv_command)(self.handle, ptrs.as_mut_ptr()) })
            .map_err(|e| anyhow!("mpv {}: {e}", args.join(" ")))
    }

    fn get_f64(&self, name: &str) -> Option<f64> {
        let mut v: f64 = 0.0;
        let r = unsafe {
            (self.lib.mpv_get_property)(
                self.handle,
                cstr(name).as_ptr(),
                MPV_FORMAT_DOUBLE,
                &mut v as *mut f64 as *mut c_void,
            )
        };
        (r >= 0).then_some(v)
    }

    fn get_i64(&self, name: &str) -> Option<i64> {
        let mut v: i64 = 0;
        let r = unsafe {
            (self.lib.mpv_get_property)(
                self.handle,
                cstr(name).as_ptr(),
                MPV_FORMAT_INT64,
                &mut v as *mut i64 as *mut c_void,
            )
        };
        (r >= 0).then_some(v)
    }

    fn check(&self, code: c_int) -> Result<()> {
        if code >= 0 {
            Ok(())
        } else {
            Err(anyhow!("{}", self.err(code)))
        }
    }

    fn err(&self, code: c_int) -> String {
        unsafe { CStr::from_ptr((self.lib.mpv_error_string)(code)) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        unsafe {
            if !self.render.is_null() {
                (self.lib.mpv_render_context_free)(self.render);
            }
            (self.lib.mpv_terminate_destroy)(self.handle);
        }
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        format!("{}/tests/fixture.mp4", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn mpv_backend_opens_and_renders_frames() {
        let Some(lib) = lib() else {
            eprintln!("libmpv not installed — skipping mpv backend test");
            return;
        };
        let mut p = MpvPlayer::open(lib, &fixture()).expect("open fixture via libmpv");
        assert_eq!(p.info.width, 320);
        assert_eq!(p.info.height, 240);
        assert!(p.info.duration > 1.5 && p.info.duration < 2.5, "≈2s, got {}", p.info.duration);

        p.set_pause(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut frames = 0;
        let mut slot: Option<Frame> = None;
        while frames < 3 && Instant::now() < deadline {
            if p.update(&mut slot) {
                let f = slot.as_ref().unwrap();
                assert_eq!(f.data.len(), 320 * 240 * 4, "RGBA frame size");
                // The alpha byte is mpv's padding and is deliberately NOT
                // fixed up here — video.wgsl forces opacity on the GPU, which
                // saves a full CPU pass over every pixel of every frame. (If
                // that shader ever stops doing it, video renders invisible.)
                frames += 1;
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(frames >= 3, "expected several rendered frames, got {frames}");
    }
}

//! Screen capture — screenshots and recording, straight into Reel, and
//! drivable headlessly from the CLI (which is what makes it reachable from
//! `reel serve` / `reel mcp`, i.e. from agents).
//!
//! Nothing is bundled: we drive the best capture tool the system offers and
//! open the result in the player the moment it lands. Backends are probed at
//! runtime, so installing a tool lights the feature up on next use — and
//! **ffmpeg is the universal floor**: a machine with nothing but ffmpeg can
//! still grab a frame and record a screen on X11/Windows/macOS.
//!
//! The two planners here — [`plan_shot`] and [`plan_recording`] — are PURE
//! functions of an [`Env`] (platform + which tools exist). That is what
//! makes every backend's argument list unit-testable on one machine, and
//! what lets an option that cannot be honoured fail with a sentence saying
//! which tool would honour it, instead of silently capturing the wrong
//! thing.
//!
//! Screenshots (first hit wins):
//!   Linux/Wayland: spectacle (KDE) → grim → the desktop portal dialog
//!   Linux/X11:     spectacle → maim → scrot → ffmpeg x11grab → import
//!   Windows:       ffmpeg gdigrab (one frame)   macOS: screencapture
//!
//! Recording (first hit wins):
//!   Linux/Wayland: wf-recorder / wl-screenrec (both take a region) →
//!                  gpu-screen-recorder → the built-in portal + PipeWire
//!   Linux/X11:     ffmpeg x11grab (region, fps, audio, cursor) → the above
//!   Windows:       ffmpeg gdigrab            macOS: ffmpeg avfoundation
//! Stopped with SIGINT (or 'q' on ffmpeg's stdin) so the file finalizes.
//!
//! A CLI recording outlives the process that started it: the child is
//! spawned detached and described in `~/.cache/reel/recording.json`, so
//! `reel record --stop` from any later process finishes it cleanly.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

fn have(tool: &str) -> bool {
    let finder = if cfg!(windows) { "where" } else { "which" };
    Command::new(finder)
        .arg(tool)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
}

/// `~/Pictures` / `~/Videos` (or the profile dir on Windows), with a `Reel`
/// subfolder created on demand.
fn out_dir(kind: &str) -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    let dir = Path::new(&home).join(kind).join("Reel");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// `reel-<stem>-YYYYMMDD-HHMMSS.<ext>` — sortable, collision-free enough.
fn stamped(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil date from epoch days (Howard Hinnant's algorithm) — avoids a
    // chrono dependency for one filename.
    let days = (secs / 86_400) as i64;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    dir.join(format!("reel-{stem}-{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}.{ext}"))
}

// ── Geometry ─────────────────────────────────────────────────────────────

/// A screen rectangle in physical pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// `X,Y,WxH` — also accepts `X,Y WxH`, `WxH+X+Y` and a bare `WxH`
    /// (top-left). Encoders demand even dimensions, so odd sizes round
    /// DOWN, which keeps the rectangle inside what the user asked for.
    pub fn parse(s: &str) -> Result<Rect> {
        let t = s.trim();
        let bad = || anyhow!("expected a rectangle like 0,0,1280x720 (X,Y,WIDTHxHEIGHT), got {t:?}");
        let (pos, size) = if let Some((size, rest)) = t.split_once('+') {
            // WxH+X+Y
            let (x, y) = rest.split_once('+').ok_or_else(bad)?;
            ((x.trim().to_string(), y.trim().to_string()), size.trim().to_string())
        } else {
            let parts: Vec<&str> = t.split([',', ' ']).filter(|p| !p.is_empty()).collect();
            match parts.len() {
                1 => (("0".into(), "0".into()), parts[0].to_string()),
                3 => ((parts[0].into(), parts[1].into()), parts[2].to_string()),
                _ => return Err(bad()),
            }
        };
        let (w, h) = size.split_once(['x', 'X']).ok_or_else(bad)?;
        let r = Rect {
            x: pos.0.parse().map_err(|_| bad())?,
            y: pos.1.parse().map_err(|_| bad())?,
            w: w.trim().parse().map_err(|_| bad())?,
            h: h.trim().parse().map_err(|_| bad())?,
        };
        if r.w < 2 || r.h < 2 {
            bail!("a capture rectangle needs a real size, got {}x{}", r.w, r.h);
        }
        // Even dimensions: every video encoder demands them, and a
        // screenshot loses nothing by it.
        Ok(Rect { w: r.w & !1, h: r.h & !1, ..r })
    }

    pub fn size(&self) -> String {
        format!("{}x{}", self.w, self.h)
    }
}

// ── Environment (what this machine can actually do) ──────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Linux,
    Windows,
    Mac,
}

/// Everything the planners are allowed to know about the machine. Tests
/// build one by hand; [`Env::probe`] builds the real one.
#[derive(Clone, Debug)]
pub struct Env {
    pub os: Os,
    pub wayland: bool,
    pub tools: Vec<String>,
    /// The X display to grab (`:0`), used by the x11grab paths.
    pub display: String,
}

/// Every capture tool Reel knows how to drive.
pub const KNOWN_TOOLS: &[&str] = &[
    "spectacle",
    "grim",
    "slurp",
    "maim",
    "scrot",
    "import",
    "ffmpeg",
    "wf-recorder",
    "wl-screenrec",
    "gpu-screen-recorder",
    "screencapture",
];

impl Env {
    pub fn probe() -> Env {
        let os = if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        };
        Env {
            os,
            wayland: is_wayland(),
            tools: KNOWN_TOOLS.iter().filter(|t| have(t)).map(|t| t.to_string()).collect(),
            display: std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()),
        }
    }

    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }
}

// ── Screenshots ──────────────────────────────────────────────────────────

/// What to capture in a screenshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShotMode {
    /// The whole desktop.
    Full,
    /// Drag-select a rectangle (interactive).
    Region,
    /// The active window (interactive on some backends).
    Window,
    /// An exact rectangle — no picker, no user. The agent path.
    Area(Rect),
}

impl ShotMode {
    /// Does this mode wait for a human?
    pub fn interactive(&self) -> bool {
        matches!(self, ShotMode::Region | ShotMode::Window)
    }
}

/// Everything a screenshot can be asked for.
#[derive(Clone, Debug)]
pub struct ShotOpts {
    pub mode: ShotMode,
    /// Where to write it. `None` = a stamped name under ~/Pictures/Reel.
    pub out: Option<PathBuf>,
    /// Wait this long before grabbing (menus, hover states, tooltips).
    pub delay: f32,
    /// A specific monitor by name (see `reel devices`).
    pub display: Option<String>,
}

impl Default for ShotOpts {
    fn default() -> Self {
        Self { mode: ShotMode::Full, out: None, delay: 0.0, display: None }
    }
}

/// One backend invocation, plus a crop to apply afterwards when the backend
/// can only grab the whole screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub tool: String,
    pub args: Vec<String>,
    pub crop: Option<Rect>,
}

fn attempt(tool: &str, args: Vec<String>) -> Attempt {
    Attempt { tool: tool.into(), args, crop: None }
}

/// Build the ordered list of screenshot attempts for this machine. Pure —
/// the whole backend matrix is unit-tested through this function.
pub fn plan_shot(opts: &ShotOpts, out: &Path, env: &Env) -> Vec<Attempt> {
    let o = out.to_string_lossy().to_string();
    let mut v: Vec<Attempt> = Vec::new();
    match env.os {
        Os::Windows => {
            let mut a: Vec<String> = vec!["-y".into(), "-f".into(), "gdigrab".into()];
            if let ShotMode::Area(r) = opts.mode {
                a.extend([
                    "-offset_x".into(), r.x.to_string(),
                    "-offset_y".into(), r.y.to_string(),
                    "-video_size".into(), r.size(),
                ]);
            }
            a.extend(["-i".into(), "desktop".into(), "-frames:v".into(), "1".into(), o.clone()]);
            v.push(attempt("ffmpeg", a));
        }
        Os::Mac => {
            let mut a: Vec<String> = vec!["-x".into()];
            match opts.mode {
                ShotMode::Full => {}
                ShotMode::Region => a.push("-i".into()),
                ShotMode::Window => a.extend(["-i".into(), "-W".into()]),
                ShotMode::Area(r) => {
                    a.push("-R".into());
                    a.push(format!("{},{},{},{}", r.x, r.y, r.w, r.h));
                }
            }
            a.push(o.clone());
            v.push(attempt("screencapture", a));
        }
        Os::Linux => {
            // spectacle covers screen/region/window from the CLI, but has no
            // rectangle flag — so an exact area skips it.
            if !matches!(opts.mode, ShotMode::Area(_)) {
                let m = match opts.mode {
                    ShotMode::Region => vec!["-r".into()],
                    ShotMode::Window => vec!["-a".into()],
                    _ => vec![],
                };
                v.push(attempt(
                    "spectacle",
                    [vec!["-b".into(), "-n".into()], m, vec!["-o".into(), o.clone()]].concat(),
                ));
            }
            if env.wayland {
                match opts.mode {
                    ShotMode::Full => {
                        let mut a = vec![];
                        if let Some(d) = &opts.display {
                            a.extend(["-o".to_string(), d.clone()]);
                        }
                        a.push(o.clone());
                        v.push(attempt("grim", a));
                    }
                    ShotMode::Area(r) => {
                        v.push(attempt(
                            "grim",
                            vec!["-g".into(), format!("{},{} {}x{}", r.x, r.y, r.w, r.h), o.clone()],
                        ));
                        // No grim (a KDE session, say): spectacle can still
                        // grab the whole screen without asking anybody, and
                        // the rectangle is cut out of that. Keeps an exact
                        // area headless where it would otherwise need a
                        // human to answer a dialog.
                        v.push(Attempt {
                            tool: "spectacle".into(),
                            args: vec!["-b".into(), "-n".into(), "-o".into(), o.clone()],
                            crop: Some(r),
                        });
                    }
                    ShotMode::Region | ShotMode::Window => {
                        // grim needs slurp for selection; run through a shell.
                        if env.has("grim") && env.has("slurp") {
                            v.push(attempt(
                                "sh",
                                vec!["-c".into(), format!("grim -g \"$(slurp)\" '{o}'")],
                            ));
                        }
                    }
                }
            } else {
                match opts.mode {
                    ShotMode::Full => {
                        v.push(attempt("maim", vec![o.clone()]));
                        v.push(attempt("scrot", vec![o.clone()]));
                    }
                    ShotMode::Region => v.push(attempt("maim", vec!["-s".into(), o.clone()])),
                    ShotMode::Window => v.push(attempt("scrot", vec!["-u".into(), o.clone()])),
                    ShotMode::Area(r) => v.push(attempt(
                        "maim",
                        vec!["-g".into(), format!("{}x{}+{}+{}", r.w, r.h, r.x, r.y), o.clone()],
                    )),
                }
                // ffmpeg is the floor: a box with nothing else installed can
                // still grab a frame, with or without a rectangle.
                if !opts.mode.interactive() {
                    let mut a: Vec<String> =
                        vec!["-y".into(), "-v".into(), "error".into(), "-f".into(), "x11grab".into()];
                    let mut target = env.display.clone();
                    if let ShotMode::Area(r) = opts.mode {
                        a.extend(["-video_size".into(), r.size()]);
                        target = format!("{}+{},{}", env.display, r.x, r.y);
                    }
                    a.extend(["-i".into(), target, "-frames:v".into(), "1".into(), o.clone()]);
                    v.push(attempt("ffmpeg", a));
                    // ImageMagick, last: whole root window, cropped after.
                    v.push(Attempt {
                        tool: "import".into(),
                        args: vec!["-window".into(), "root".into(), o.clone()],
                        crop: match opts.mode {
                            ShotMode::Area(r) => Some(r),
                            _ => None,
                        },
                    });
                }
            }
        }
    }
    v.retain(|a| a.tool == "sh" || env.has(&a.tool));
    v
}

/// Wait until `path` exists and its size has stopped growing. A capture
/// tool that has returned may still be flushing; reading the file then
/// yields a truncated image.
fn wait_for_stable_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last = 0u64;
    let mut stable = 0;
    while Instant::now() < deadline {
        match std::fs::metadata(path).map(|m| m.len()) {
            Ok(n) if n > 0 && n == last => {
                stable += 1;
                if stable >= 2 {
                    return;
                }
            }
            Ok(n) => {
                last = n;
                stable = 0;
            }
            Err(_) => stable = 0,
        }
        std::thread::sleep(Duration::from_millis(60));
    }
}

/// Crop a file in place to `r` (ffmpeg; used when a backend could only give
/// us the whole screen).
fn crop_in_place(file: &Path, r: Rect) -> Result<()> {
    let tmp = file.with_extension("crop.png");
    let st = Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-i", &file.to_string_lossy(),
            "-vf", &format!("crop={}:{}:{}:{}", r.w, r.h, r.x.max(0), r.y.max(0)),
            &tmp.to_string_lossy(),
        ])
        .status()?;
    if !st.success() || !tmp.exists() {
        bail!("could not crop the screenshot to {}x{}", r.w, r.h);
    }
    std::fs::rename(&tmp, file)?;
    Ok(())
}

/// Take a screenshot. Region/Window modes wait for the user's selection —
/// call from a worker thread. Returns the saved file.
pub fn screenshot_with(opts: &ShotOpts) -> Result<PathBuf> {
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| stamped(&out_dir("Pictures"), "shot", "png"));
    if let Some(dir) = out.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    if opts.delay > 0.0 {
        std::thread::sleep(Duration::from_secs_f32(opts.delay));
    }
    let env = Env::probe();
    let attempts = plan_shot(opts, &out, &env);
    let _ = std::fs::remove_file(&out);

    for a in &attempts {
        let status = Command::new(&a.tool)
            .args(&a.args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Ok(st) = status {
            // Some tools return before the file hits disk — and some return
            // while it is still being WRITTEN, which once handed a
            // half-finished PNG to the cropper. Wait for a size that stops
            // changing, not merely for the name to appear.
            if st.success() {
                wait_for_stable_file(&out, Duration::from_secs(4));
            }
            if out.exists() {
                if let Some(r) = a.crop {
                    crop_in_place(&out, r)?;
                }
                log::info!("screenshot via {} → {}", a.tool, out.display());
                return Ok(out);
            }
        }
        log::warn!("screenshot via {} failed; trying next backend", a.tool);
    }

    // Built-in fallback: the portal's interactive dialog (its own UI offers
    // screen/window/region). An exact area is cropped out of what it gives.
    #[cfg(target_os = "linux")]
    if crate::portal::available() {
        let got = crate::portal::screenshot_interactive(out)?;
        if let ShotMode::Area(r) = opts.mode {
            crop_in_place(&got, r)?;
        }
        return Ok(got);
    }
    if attempts.is_empty() {
        bail!(
            "no screenshot backend on this machine — install one of: {}",
            if env.wayland { "grim, spectacle" } else { "maim, scrot, ffmpeg, imagemagick" }
        );
    }
    bail!("no screenshot backend worked")
}

// ── Recording ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StopMethod {
    /// SIGINT — the tool finalizes its file and exits.
    Interrupt,
    /// Write 'q' to stdin (ffmpeg's graceful quit).
    QuitKey,
}

/// Where a recording's sound comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AudioSource {
    None,
    /// What the machine is playing (the default sink's monitor).
    #[default]
    System,
    /// The default microphone.
    Mic,
    /// Both, mixed.
    Both,
}

impl AudioSource {
    pub fn parse(s: &str) -> Result<AudioSource> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "silent" => AudioSource::None,
            "system" | "desktop" | "output" => AudioSource::System,
            "mic" | "microphone" | "input" => AudioSource::Mic,
            "both" | "all" => AudioSource::Both,
            other => bail!("--audio takes none, system, mic or both, got {other:?}"),
        })
    }
    pub fn wants_mic(&self) -> bool {
        matches!(self, AudioSource::Mic | AudioSource::Both)
    }
    pub fn wants_system(&self) -> bool {
        matches!(self, AudioSource::System | AudioSource::Both)
    }
    pub fn name(&self) -> &'static str {
        match self {
            AudioSource::None => "none",
            AudioSource::System => "system",
            AudioSource::Mic => "mic",
            AudioSource::Both => "both",
        }
    }
}

/// Everything a recording can be asked for.
#[derive(Clone, Debug)]
pub struct RecordOpts {
    pub out: Option<PathBuf>,
    /// An exact rectangle of the screen.
    pub area: Option<Rect>,
    pub fps: u32,
    pub audio: AudioSource,
    /// A monitor by name (see `reel devices`).
    pub display: Option<String>,
    /// Stop by itself after this many seconds.
    pub duration: Option<f64>,
    pub cursor: bool,
    /// Record this camera instead of the screen.
    pub webcam: Option<String>,
}

impl Default for RecordOpts {
    fn default() -> Self {
        Self {
            out: None,
            area: None,
            fps: 30,
            audio: AudioSource::System,
            display: None,
            duration: None,
            cursor: true,
            webcam: None,
        }
    }
}

/// A planned recording command: what to run, and how to end it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecPlan {
    pub tool: String,
    pub args: Vec<String>,
    pub stop: StopMethod,
    /// True when the tool puts a picker in front of the user.
    pub interactive: bool,
}

/// The system-audio monitor source name, if one is discoverable.
pub fn audio_monitor() -> Option<String> {
    let out = Command::new("pactl").arg("get-default-sink").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sink = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sink.is_empty()).then(|| format!("{sink}.monitor"))
}

/// ffmpeg audio input legs + the maps/filters that mix them, for a plan
/// whose video is input 0. `monitor` is the system-audio source name.
fn ffmpeg_audio_legs(audio: AudioSource, monitor: Option<&str>) -> (Vec<String>, Vec<String>) {
    let mut inputs: Vec<String> = Vec::new();
    let mut n = 0;
    if audio.wants_system() {
        if let Some(m) = monitor {
            inputs.extend(["-f".into(), "pulse".into(), "-i".into(), m.to_string()]);
            n += 1;
        }
    }
    if audio.wants_mic() {
        inputs.extend(["-f".into(), "pulse".into(), "-i".into(), "default".into()]);
        n += 1;
    }
    let tail: Vec<String> = match n {
        0 => vec![],
        1 => vec![
            "-map".into(), "0:v".into(), "-map".into(), "1:a".into(),
            "-c:a".into(), "aac".into(), "-b:a".into(), "160k".into(),
        ],
        _ => vec![
            // normalize=0: the default divides every input by their count,
            // which would halve both the desktop and the voice.
            "-filter_complex".into(), "[1:a][2:a]amix=inputs=2:normalize=0[a]".into(),
            "-map".into(), "0:v".into(), "-map".into(), "[a]".into(),
            "-c:a".into(), "aac".into(), "-b:a".into(), "160k".into(),
        ],
    };
    (inputs, tail)
}

const X264: [&str; 6] = ["-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p"];

/// Plan a screen (or camera) recording for this machine. Pure — every
/// backend's arguments are unit-tested through here, and an option no
/// available backend can honour is an error naming the tool that would.
pub fn plan_recording(opts: &RecordOpts, out: &Path, env: &Env, monitor: Option<&str>) -> Result<RecPlan> {
    let o = out.to_string_lossy().to_string();
    let fps = opts.fps.clamp(1, 240);
    let dur: Vec<String> = match opts.duration {
        Some(d) if d > 0.0 => vec!["-t".into(), format!("{d:.3}")],
        _ => vec![],
    };
    let x264: Vec<String> = X264.iter().map(|s| s.to_string()).collect();

    // A camera is the same everywhere ffmpeg can see it.
    if let Some(dev) = &opts.webcam {
        if !env.has("ffmpeg") {
            bail!("recording a camera needs ffmpeg on PATH");
        }
        let (input_fmt, dev_arg) = match env.os {
            Os::Linux => ("v4l2", dev.clone()),
            Os::Windows => ("dshow", format!("video={dev}")),
            Os::Mac => ("avfoundation", dev.clone()),
        };
        let mut a: Vec<String> = vec![
            "-y".into(), "-f".into(), input_fmt.into(),
            "-framerate".into(), fps.to_string(), "-i".into(), dev_arg,
        ];
        let (inputs, tail) = if env.os == Os::Linux {
            ffmpeg_audio_legs(opts.audio, monitor)
        } else {
            (vec![], vec![])
        };
        a.extend(inputs);
        a.extend(x264.clone());
        a.extend(tail);
        a.extend(dur);
        a.push(o);
        return Ok(RecPlan { tool: "ffmpeg".into(), args: a, stop: StopMethod::QuitKey, interactive: false });
    }

    match env.os {
        Os::Windows => {
            if !env.has("ffmpeg") {
                bail!("screen recording on Windows needs ffmpeg on PATH");
            }
            let mut a: Vec<String> = vec![
                "-y".into(), "-f".into(), "gdigrab".into(),
                "-framerate".into(), fps.to_string(),
                "-draw_mouse".into(), if opts.cursor { "1".into() } else { "0".into() },
            ];
            if let Some(r) = opts.area {
                a.extend([
                    "-offset_x".into(), r.x.to_string(),
                    "-offset_y".into(), r.y.to_string(),
                    "-video_size".into(), r.size(),
                ]);
            }
            a.extend(["-i".into(), "desktop".into()]);
            a.extend(x264);
            a.extend(dur);
            a.push(o);
            Ok(RecPlan { tool: "ffmpeg".into(), args: a, stop: StopMethod::QuitKey, interactive: false })
        }
        Os::Mac => {
            if !env.has("ffmpeg") {
                bail!("screen recording on macOS needs ffmpeg on PATH");
            }
            let screen = opts.display.clone().unwrap_or_else(|| "1".into());
            let mut a: Vec<String> = vec![
                "-y".into(), "-f".into(), "avfoundation".into(),
                "-framerate".into(), fps.to_string(),
                "-capture_cursor".into(), if opts.cursor { "1".into() } else { "0".into() },
                "-i".into(), format!("{screen}:{}", if opts.audio == AudioSource::None { "none" } else { "0" }),
            ];
            if let Some(r) = opts.area {
                a.extend(["-vf".into(), format!("crop={}:{}:{}:{}", r.w, r.h, r.x.max(0), r.y.max(0))]);
            }
            a.extend(x264);
            a.extend(dur);
            a.push(o);
            Ok(RecPlan { tool: "ffmpeg".into(), args: a, stop: StopMethod::QuitKey, interactive: false })
        }
        Os::Linux if !env.wayland && env.has("ffmpeg") => {
            // x11grab honours every option we expose, so it leads on X11.
            let mut a: Vec<String> = vec![
                "-y".into(), "-f".into(), "x11grab".into(),
                "-framerate".into(), fps.to_string(),
                "-draw_mouse".into(), if opts.cursor { "1".into() } else { "0".into() },
            ];
            let mut target = env.display.clone();
            if let Some(r) = opts.area {
                a.extend(["-video_size".into(), r.size()]);
                target = format!("{}+{},{}", env.display, r.x, r.y);
            }
            a.extend(["-i".into(), target]);
            let (inputs, tail) = ffmpeg_audio_legs(opts.audio, monitor);
            a.extend(inputs);
            a.extend(x264);
            a.extend(tail);
            a.extend(dur);
            a.push(o);
            Ok(RecPlan { tool: "ffmpeg".into(), args: a, stop: StopMethod::QuitKey, interactive: false })
        }
        Os::Linux => {
            // Wayland: the compositor decides. wf-recorder and wl-screenrec
            // both take a geometry; gpu-screen-recorder does not.
            if env.has("wf-recorder") {
                let mut a: Vec<String> = vec!["-f".into(), o.clone(), "-r".into(), fps.to_string()];
                if let Some(r) = opts.area {
                    a.extend(["-g".into(), format!("{},{} {}x{}", r.x, r.y, r.w, r.h)]);
                }
                if let Some(d) = &opts.display {
                    a.extend(["-o".into(), d.clone()]);
                }
                match opts.audio {
                    AudioSource::None => {}
                    AudioSource::Mic => a.push("--audio=default".into()),
                    _ => a.push(match monitor {
                        Some(m) => format!("--audio={m}"),
                        None => "--audio".into(),
                    }),
                }
                return Ok(RecPlan { tool: "wf-recorder".into(), args: a, stop: StopMethod::Interrupt, interactive: false });
            }
            if env.has("wl-screenrec") {
                let mut a: Vec<String> = vec!["-f".into(), o.clone()];
                if let Some(r) = opts.area {
                    a.extend(["-g".into(), format!("{},{} {}x{}", r.x, r.y, r.w, r.h)]);
                }
                if let Some(d) = &opts.display {
                    a.extend(["-o".into(), d.clone()]);
                }
                if opts.audio != AudioSource::None {
                    a.push("--audio".into());
                }
                return Ok(RecPlan { tool: "wl-screenrec".into(), args: a, stop: StopMethod::Interrupt, interactive: false });
            }
            if env.has("gpu-screen-recorder") {
                if opts.area.is_some() {
                    bail!("gpu-screen-recorder cannot record a rectangle — install wf-recorder or wl-screenrec for --area on Wayland");
                }
                let mut a: Vec<String> = vec![
                    "-w".into(), opts.display.clone().unwrap_or_else(|| "screen".into()),
                    "-f".into(), fps.to_string(),
                ];
                if opts.audio != AudioSource::None {
                    a.extend(["-a".into(), "default_output".into()]);
                }
                a.extend(["-o".into(), o.clone()]);
                return Ok(RecPlan { tool: "gpu-screen-recorder".into(), args: a, stop: StopMethod::Interrupt, interactive: false });
            }
            bail!(
                "no headless screen recorder on this Wayland session — install wf-recorder, wl-screenrec or gpu-screen-recorder (Reel's built-in portal capture works in the app, but it asks the system picker to choose a source)"
            )
        }
    }
}

/// A screen recording in progress. `stop()` finalizes and returns the file.
pub struct Recorder {
    child: Child,
    stop: StopMethod,
    pub path: PathBuf,
    pub tool: String,
}

/// A screen recording in progress, over whichever backend engaged.
pub enum Recording {
    /// Reel's built-in portal + PipeWire capture (Linux).
    #[cfg(target_os = "linux")]
    Portal(crate::portal::PortalRecorder),
    /// An external capture tool.
    Tool(Recorder),
}

impl Recording {
    pub fn stop(self) -> Result<PathBuf> {
        match self {
            #[cfg(target_os = "linux")]
            Recording::Portal(r) => r.stop(),
            Recording::Tool(r) => r.stop(),
        }
    }

    /// Let it run for exactly this long, then finish it.
    pub fn stop_after(self, seconds: f64) -> Result<PathBuf> {
        std::thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
        self.stop()
    }
}

/// Start a recording for a HEADLESS caller (the CLI, an agent): a planned
/// external tool when one exists, and otherwise Reel's own portal capture —
/// which is what keeps `reel record` working on a Wayland desktop with no
/// capture tools installed at all. Returns the recording, its file, and the
/// backend's name.
pub fn start_headless(opts: &RecordOpts) -> Result<(Recording, PathBuf, String)> {
    let stem = if opts.webcam.is_some() { "webcam" } else { "rec" };
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| stamped(&out_dir("Videos"), stem, "mp4"));
    let opts = RecordOpts { out: Some(out.clone()), ..opts.clone() };
    match start_tool_recording(&opts) {
        Ok(r) => {
            let tool = r.tool.clone();
            Ok((Recording::Tool(r), out, tool))
        }
        #[cfg(target_os = "linux")]
        Err(e) if opts.webcam.is_none() && crate::portal::available() => {
            log::info!("no headless recorder ({e}); using the built-in portal capture");
            if opts.area.is_some() {
                bail!("{e}");
            }
            let r = crate::portal::start_recording(out.clone())?;
            Ok((Recording::Portal(r), out, "portal".into()))
        }
        Err(e) => Err(e),
    }
}

/// Start a screen recording for the APP: on Linux the built-in portal path
/// runs first — the system picker lets the user choose screen/window/region,
/// no external tools needed; external recorders remain as fallbacks. Blocks
/// through the picker: call from a worker thread.
pub fn start_recording() -> Result<Recording> {
    #[cfg(target_os = "linux")]
    if crate::portal::available() {
        let out = stamped(&out_dir("Videos"), "rec", "mp4");
        match crate::portal::start_recording(out) {
            Ok(r) => return Ok(Recording::Portal(r)),
            Err(e) => log::warn!("built-in portal recording unavailable ({e}); trying external tools"),
        }
    }
    start_tool_recording(&RecordOpts::default()).map(Recording::Tool)
}

/// Start a recording through a planned external tool. Never interactive —
/// this is the path the CLI and agents use.
pub fn start_tool_recording(opts: &RecordOpts) -> Result<Recorder> {
    let stem = if opts.webcam.is_some() { "webcam" } else { "rec" };
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| stamped(&out_dir("Videos"), stem, "mp4"));
    if let Some(dir) = out.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    let env = Env::probe();
    let monitor = opts.audio.wants_system().then(audio_monitor).flatten();
    let plan = plan_recording(opts, &out, &env, monitor.as_deref())?;

    let mut cmd = Command::new(&plan.tool);
    cmd.args(&plan.args).stdout(Stdio::null()).stderr(Stdio::null());
    if plan.stop == StopMethod::QuitKey {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let child = cmd
        .spawn()
        .map_err(|e| anyhow!("{} failed to start: {e}", plan.tool))?;
    log::info!("recording via {} → {}", plan.tool, out.display());
    Ok(Recorder { child, stop: plan.stop, path: out, tool: plan.tool })
}

/// Record the WEBCAM (with the default microphone when one answers).
#[cfg(target_os = "linux")]
pub fn start_webcam_recording() -> Result<Recording> {
    let device = webcam_device().ok_or_else(|| anyhow!("no webcam found (looked for /dev/video*)"))?;
    let audio = if microphone_answers() { AudioSource::Mic } else { AudioSource::None };
    start_tool_recording(&RecordOpts { webcam: Some(device), audio, ..Default::default() })
        .map(Recording::Tool)
}

/// Does the default microphone actually deliver? (A dead `-f pulse` input
/// fails the whole ffmpeg command, so this is probed before it is used.)
pub fn microphone_answers() -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-f", "pulse", "-i", "default", "-t", "0.1", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build the streamer project: screen on V1, camera as a bottom-right PiP
/// overlay, saved beside the recordings. Returns the .reel path. Pure over
/// two existing files — unit-tested with fixtures.
pub fn assemble_streamer_project(screen: &str, webcam: &str) -> Result<String> {
    let s_info = crate::video::decoder::probe(screen)
        .map_err(|e| anyhow!("could not probe the screen recording: {e}"))?;
    let c_info = crate::video::decoder::probe(webcam)
        .map_err(|e| anyhow!("could not probe the camera recording: {e}"))?;
    let mut p = crate::edit::Project::default();
    p.width = s_info.width.max(2);
    p.height = s_info.height.max(2);
    p.fps = if s_info.fps > 1.0 { s_info.fps } else { 30.0 };
    let len = s_info.duration.min(c_info.duration).max(0.2);
    p.add_clip(screen, crate::edit::TrackKind::Video, 0.0, 0.0, len);
    let cam = p.add_clip(webcam, crate::edit::TrackKind::Overlay, 0.0, 0.0, len);
    if let Some(c) = p.clip_mut(cam) {
        // The classic corner cam: bottom-right quarter-ish.
        c.pip.x = 0.82;
        c.pip.y = 0.8;
        c.pip.scale = 0.28;
    }
    let out = std::path::Path::new(screen).with_extension("streamer.reel");
    p.save(&out.to_string_lossy())
        .map_err(|e| anyhow!("could not save the streamer project: {e}"))?;
    Ok(out.to_string_lossy().into_owned())
}

/// The first webcam that actually delivers frames.
#[cfg(target_os = "linux")]
pub fn webcam_device() -> Option<String> {
    cameras().into_iter().next().map(|c| c.path)
}

impl Recorder {
    /// Signal the tool to finish, wait for it, and hand back the file.
    pub fn stop(mut self) -> Result<PathBuf> {
        match self.stop {
            StopMethod::QuitKey => {
                if let Some(stdin) = self.child.stdin.as_mut() {
                    let _ = stdin.write_all(b"q");
                    let _ = stdin.flush();
                }
            }
            StopMethod::Interrupt => interrupt(self.child.id()),
        }
        // Wait for a clean exit; don't hang the caller forever.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if self.path.exists() {
            Ok(self.path)
        } else {
            Err(anyhow!("{} produced no file at {}", self.tool, self.path.display()))
        }
    }

    /// Let it run for exactly this long, then finish it.
    pub fn stop_after(self, seconds: f64) -> Result<PathBuf> {
        std::thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
        self.stop()
    }
}

/// SIGINT a pid — every recorder we drive finalizes its file on it.
fn interrupt(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-INT", &pid.to_string()]).status();
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill").args(["/PID", &pid.to_string()]).status();
    }
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

// ── Detached sessions (the CLI's start / stop) ───────────────────────────
//
// A CLI recording has to outlive the process that started it: `reel record`
// returns immediately, and `reel record --stop` — minutes later, from a
// different process, possibly a different agent — finishes it. The child is
// spawned detached and described in one small JSON file.

/// A recording running in another process.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub pid: u32,
    pub path: String,
    pub tool: String,
    /// Unix seconds.
    pub started: u64,
    /// What this recording is of: "screen" or "webcam".
    pub kind: String,
    /// How to end it: "signal" (SIGINT the tool) or "file" (delete this
    /// session file — a portal recording watches for that, because it has
    /// to finalize from inside its own process).
    #[serde(default = "signal_stop")]
    pub stop: String,
}

fn signal_stop() -> String {
    "signal".into()
}

/// `~/.cache/reel/recording.json`
pub fn session_file() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache")
        });
    base.join("reel").join("recording.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The recording in progress, if there is one. A session whose process has
/// died is not one — it is cleaned up and reported as gone.
pub fn active_session() -> Option<Session> {
    let raw = std::fs::read_to_string(session_file()).ok()?;
    let s: Session = serde_json::from_str(&raw).ok()?;
    if alive(s.pid) {
        Some(s)
    } else {
        let _ = std::fs::remove_file(session_file());
        None
    }
}

fn write_session(s: &Session) -> Result<()> {
    let f = session_file();
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(f, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

/// Start a recording that outlives this process. Returns the session.
pub fn start_detached(opts: &RecordOpts) -> Result<Session> {
    if let Some(s) = active_session() {
        bail!(
            "a recording is already running (pid {}, into {}) — stop it first with `reel record --stop`",
            s.pid,
            s.path
        );
    }
    let stem = if opts.webcam.is_some() { "webcam" } else { "rec" };
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| stamped(&out_dir("Videos"), stem, "mp4"));
    if let Some(dir) = out.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    let env = Env::probe();
    if env.os == Os::Windows {
        // No SIGINT on Windows: a detached recorder could only be killed,
        // which loses the file. Say so instead of writing a broken mp4.
        bail!("on Windows a recording must be given a length: `reel record --duration SECONDS`");
    }
    let monitor = opts.audio.wants_system().then(audio_monitor).flatten();
    let plan = match plan_recording(opts, &out, &env, monitor.as_deref()) {
        Ok(p) => p,
        #[cfg(target_os = "linux")]
        Err(e) if opts.webcam.is_none() && opts.area.is_none() && crate::portal::available() => {
            log::info!("no headless recorder ({e}); keeping a portal recording alive instead");
            return spawn_portal_session(&out);
        }
        Err(e) => return Err(e),
    };
    let child = Command::new(&plan.tool)
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("{} failed to start: {e}", plan.tool))?;
    let s = Session {
        pid: child.id(),
        path: out.to_string_lossy().into_owned(),
        tool: plan.tool,
        started: now_secs(),
        kind: if opts.webcam.is_some() { "webcam".into() } else { "screen".into() },
        stop: signal_stop(),
    };
    // Give the tool a moment to fail loudly (a bad device, a busy camera)
    // rather than reporting a session that is already dead.
    std::thread::sleep(Duration::from_millis(600));
    if !alive(s.pid) {
        bail!(
            "{} exited immediately — check the source is capturable (try `reel devices`)",
            s.tool
        );
    }
    write_session(&s)?;
    Ok(s)
}

/// Finish the detached recording and return its file.
pub fn stop_detached() -> Result<(Session, PathBuf)> {
    let raw = std::fs::read_to_string(session_file())
        .map_err(|_| anyhow!("no recording is running — start one with `reel record`"))?;
    let s: Session = serde_json::from_str(&raw)
        .map_err(|e| anyhow!("the recording session file is unreadable: {e}"))?;
    let path = PathBuf::from(&s.path);
    // A portal recording ends when its session file disappears: it polls for
    // that and finalizes from inside its own process, where the recorder
    // object lives. Everything else takes a SIGINT.
    let watches_file = s.stop == "file";
    let _ = std::fs::remove_file(session_file());
    if alive(s.pid) {
        if !watches_file {
            interrupt(s.pid);
        }
        let deadline = Instant::now() + Duration::from_secs(25);
        while alive(s.pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // The trailer is written on exit; wait for a file that is really there.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if !path.exists() {
        bail!("{} produced no file at {}", s.tool, s.path);
    }
    Ok((s, path))
}

/// Keep a portal recording alive in a child process, so `reel record`
/// returns immediately on a Wayland desktop with no capture tools. The
/// child writes the session file itself (it owns the recorder) and stops
/// when that file disappears.
#[cfg(target_os = "linux")]
fn spawn_portal_session(out: &Path) -> Result<Session> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("cannot find reel itself: {e}"))?;
    let child = Command::new(exe)
        .args(["record", &out.to_string_lossy()])
        .env("REEL_PORTAL_SESSION", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("could not start the portal recording: {e}"))?;
    // The system picker may be up: wait generously for the child to report
    // that frames are flowing, but not forever, and not past its death.
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Some(s) = active_session() {
            if s.pid == child.id() {
                return Ok(s);
            }
        }
        if !alive(child.id()) {
            bail!("the portal recording ended before it started — was the source picker cancelled?");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = interrupt(child.id());
    bail!("the portal recording never started (no answer from the system picker)")
}

/// The child half of the above: record until the session file is removed.
/// Returns the finished file.
#[cfg(target_os = "linux")]
pub fn run_portal_session(out: PathBuf) -> Result<PathBuf> {
    if let Some(dir) = out.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    let rec = crate::portal::start_recording(out.clone())?;
    let me = std::process::id();
    write_session(&Session {
        pid: me,
        path: out.to_string_lossy().into_owned(),
        tool: "portal".into(),
        started: now_secs(),
        kind: "screen".into(),
        stop: "file".into(),
    })?;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        // Gone, or replaced by somebody else's session: our turn is over.
        match active_session() {
            Some(s) if s.pid == me => {}
            _ => break,
        }
    }
    rec.stop()
}

// ── Devices (what an agent can point at) ─────────────────────────────────

#[derive(Clone, Debug, Serialize)]
pub struct Display {
    pub name: String,
    pub geometry: String,
    pub primary: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Camera {
    pub path: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Microphone {
    pub name: String,
    pub description: String,
    pub monitor: bool,
}

/// Monitors, by name and geometry — the names `--display` accepts.
pub fn displays() -> Vec<Display> {
    let mut out = Vec::new();
    // Wayland compositors that answer wlr-randr / swaymsg.
    if let Ok(o) = Command::new("wlr-randr").output() {
        if o.status.success() {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            let mut name = String::new();
            for line in text.lines() {
                if !line.starts_with(char::is_whitespace) && line.contains('"') {
                    name = line.split_whitespace().next().unwrap_or("").to_string();
                } else if line.trim().contains("current") && !name.is_empty() {
                    let geo = line.trim().split_whitespace().next().unwrap_or("").to_string();
                    out.push(Display { name: std::mem::take(&mut name), geometry: geo, primary: out.is_empty() });
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    // X11 (also XWayland, which is why this runs on Wayland desktops too).
    if let Ok(o) = Command::new("xrandr").arg("--listmonitors").output() {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).lines().skip(1) {
                // " 0: +*eDP-1 2560/344x1440/193+0+0  eDP-1"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 4 {
                    continue;
                }
                let primary = parts[1].contains('*');
                let name = parts[3].to_string();
                let geo = parts[2]
                    .split('+')
                    .next()
                    .unwrap_or("")
                    .split('x')
                    .map(|d| d.split('/').next().unwrap_or("").to_string())
                    .collect::<Vec<_>>()
                    .join("x");
                let off = parts[2].split_once('+').map(|(_, o)| o.replace('+', ",")).unwrap_or_default();
                out.push(Display { name, geometry: format!("{geo}+{off}"), primary });
            }
        }
    }
    out
}

/// Cameras that actually deliver a frame (probed, not just listed).
pub fn cameras() -> Vec<Camera> {
    #[cfg(target_os = "linux")]
    {
        linux_cameras()
    }
    #[cfg(not(target_os = "linux"))]
    {
        listed_cameras()
    }
}

/// Windows/macOS: ask ffmpeg what the OS offers. Both back-ends print their
/// device list to stderr and then exit non-zero on purpose, so the exit
/// status is deliberately ignored.
#[cfg(not(target_os = "linux"))]
fn listed_cameras() -> Vec<Camera> {
    let args: &[&str] = if cfg!(target_os = "windows") {
        &["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"]
    } else {
        &["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""]
    };
    let Ok(o) = Command::new("ffmpeg").args(args).stdin(Stdio::null()).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&o.stderr).to_string();
    let mut out = Vec::new();
    let mut in_video = false;
    for line in text.lines() {
        let l = line.trim();
        if cfg!(target_os = "windows") {
            // `"Integrated Camera" (video)` — the alternative-name lines
            // that follow are the same device by another spelling.
            if !l.ends_with("(video)") {
                continue;
            }
            if let Some(name) = l.split('"').nth(1) {
                out.push(Camera { path: name.to_string(), name: name.to_string() });
            }
        } else {
            if l.contains("video devices") {
                in_video = true;
                continue;
            }
            if l.contains("audio devices") {
                in_video = false;
                continue;
            }
            // `[0] FaceTime HD Camera` — the index is what -i takes.
            if in_video {
                if let Some((idx, name)) = l.rsplit_once(']').and_then(|(a, b)| {
                    a.rsplit_once('[').map(|(_, i)| (i.to_string(), b.trim().to_string()))
                }) {
                    if idx.chars().all(|c| c.is_ascii_digit()) && !name.is_empty() {
                        out.push(Camera { path: idx, name });
                    }
                }
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn linux_cameras() -> Vec<Camera> {
    let mut names: std::collections::HashMap<String, String> = Default::default();
    if let Ok(o) = Command::new("v4l2-ctl").arg("--list-devices").output() {
        // "Integrated Camera (usb-...):\n\t/dev/video0\n\t/dev/video1"
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        let mut label = String::new();
        for line in text.lines() {
            if !line.starts_with(char::is_whitespace) {
                label = line.trim_end_matches(':').trim().to_string();
            } else if let Some(dev) = line.split_whitespace().next() {
                names.insert(dev.to_string(), label.clone());
            }
        }
    }
    let mut out = Vec::new();
    for i in 0..8 {
        let dev = format!("/dev/video{i}");
        if !Path::new(&dev).exists() {
            continue;
        }
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "v4l2", "-i", &dev, "-frames:v", "1", "-f", "null", "-"])
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            let name = names.get(&dev).cloned().unwrap_or_else(|| dev.clone());
            out.push(Camera { path: dev, name });
        }
    }
    out
}

/// Audio sources: microphones and the `.monitor` sources that carry what
/// the machine is playing.
pub fn microphones() -> Vec<Microphone> {
    let mut out = Vec::new();
    let Ok(o) = Command::new("pactl").args(["list", "short", "sources"]).output() else {
        return out;
    };
    if !o.status.success() {
        return out;
    }
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let mut it = line.split('\t');
        let _idx = it.next();
        let Some(name) = it.next() else { continue };
        let monitor = name.ends_with(".monitor");
        out.push(Microphone {
            name: name.to_string(),
            description: it.next().unwrap_or("").to_string(),
            monitor,
        });
    }
    out
}

/// The whole capture picture for this machine, as an agent sees it.
pub fn devices_report() -> serde_json::Value {
    let env = Env::probe();
    let recorder = plan_recording(&RecordOpts::default(), Path::new("/tmp/probe.mp4"), &env, None)
        .map(|p| p.tool)
        .unwrap_or_else(|_| {
            #[cfg(target_os = "linux")]
            if crate::portal::available() {
                return "portal".into();
            }
            "none".into()
        });
    // The whole chain, in order: the first entry is what will normally run,
    // and the rest is what happens when it doesn't (a KDE tool on a bare X
    // server, say). An agent reading this can tell "works" from "works if".
    let shot: Vec<String> = plan_shot(&ShotOpts::default(), Path::new("/tmp/probe.png"), &env)
        .into_iter()
        .map(|a| a.tool)
        .collect();
    serde_json::json!({
        "session": if env.wayland { "wayland" } else { "x11" },
        "display": env.display,
        "tools": env.tools,
        "screenshot_backends": shot,
        "recording_backend": recorder,
        "displays": displays(),
        "cameras": cameras(),
        "audio_sources": microphones(),
        "system_audio": audio_monitor(),
        "recording": active_session(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_x11(tools: &[&str]) -> Env {
        Env {
            os: Os::Linux,
            wayland: false,
            tools: tools.iter().map(|s| s.to_string()).collect(),
            display: ":0".into(),
        }
    }

    #[test]
    fn stamped_names_are_sane() {
        let p = stamped(Path::new("/tmp"), "shot", "png");
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("reel-shot-2"), "{name}");
        assert!(name.ends_with(".png"));
        // reel-shot-YYYYMMDD-HHMMSS.png
        assert_eq!(name.len(), "reel-shot-20260827-120000.png".len(), "{name}");
    }

    /// Rectangles come in the three spellings people actually type.
    #[test]
    fn rectangles_parse_in_every_spelling() {
        let a = Rect::parse("10,20,640x480").unwrap();
        assert_eq!(a, Rect { x: 10, y: 20, w: 640, h: 480 });
        assert_eq!(Rect::parse("10,20 640x480").unwrap(), a);
        assert_eq!(Rect::parse("640x480+10+20").unwrap(), a);
        assert_eq!(Rect::parse("640x480").unwrap(), Rect { x: 0, y: 0, w: 640, h: 480 });
        // Odd sizes round down — encoders demand even dimensions.
        assert_eq!(Rect::parse("0,0,641x481").unwrap().size(), "640x480");
        for bad in ["", "nonsense", "10,20", "0,0,0x0", "0,0,10"] {
            assert!(Rect::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }

    /// The whole point of the planner: an exact rectangle reaches every
    /// backend as that backend spells geometry, and never as a picker.
    #[test]
    fn an_exact_area_reaches_each_backend_in_its_own_dialect() {
        let r = Rect { x: 100, y: 50, w: 640, h: 480 };
        let opts = ShotOpts { mode: ShotMode::Area(r), ..Default::default() };
        let out = Path::new("/tmp/a.png");

        // X11 with only ffmpeg: video_size + a display with an offset.
        let plan = plan_shot(&opts, out, &linux_x11(&["ffmpeg"]));
        let a = &plan[0];
        assert_eq!(a.tool, "ffmpeg");
        let joined = a.args.join(" ");
        assert!(joined.contains("-video_size 640x480"), "{joined}");
        assert!(joined.contains(":0+100,50"), "{joined}");
        assert!(a.crop.is_none());

        // maim leads when it is there, in its WxH+X+Y spelling.
        let plan = plan_shot(&opts, out, &linux_x11(&["maim", "ffmpeg"]));
        assert_eq!(plan[0].tool, "maim");
        assert!(plan[0].args.join(" ").contains("640x480+100+50"));

        // Wayland: grim's "x,y wxh". spectacle can't do rectangles, so it
        // must not be first in line for one.
        let wl = Env { os: Os::Linux, wayland: true, tools: vec!["grim".into(), "spectacle".into()], display: ":0".into() };
        let plan = plan_shot(&opts, out, &wl);
        assert_eq!(plan[0].tool, "grim");
        assert!(plan[0].args.join(" ").contains("100,50 640x480"));
        // spectacle has no rectangle flag, so it may only stand in as a
        // whole-screen grab that is CROPPED afterwards — never as a plain
        // full-screen shot pretending to be the area asked for.
        let spec = plan.iter().find(|a| a.tool == "spectacle").expect("spectacle as the KDE fallback");
        assert_eq!(spec.crop, Some(r));
        assert!(!spec.args.iter().any(|a| a.contains("100,50")));

        // ImageMagick can only grab the root window — so it is planned with
        // a crop to finish the job.
        let plan = plan_shot(&opts, out, &linux_x11(&["import"]));
        assert_eq!(plan[0].tool, "import");
        assert_eq!(plan[0].crop, Some(r));

        // macOS and Windows speak their own geometry.
        let mac = Env { os: Os::Mac, wayland: false, tools: vec!["screencapture".into()], display: String::new() };
        assert!(plan_shot(&opts, out, &mac)[0].args.join(" ").contains("-R 100,50,640,480"));
        let win = Env { os: Os::Windows, wayland: false, tools: vec!["ffmpeg".into()], display: String::new() };
        let j = plan_shot(&opts, out, &win)[0].args.join(" ");
        assert!(j.contains("-offset_x 100") && j.contains("-offset_y 50") && j.contains("-video_size 640x480"), "{j}");
    }

    /// A tool that isn't installed is never planned, and an interactive
    /// mode never silently becomes a headless one.
    #[test]
    fn the_planner_only_offers_tools_that_exist() {
        let opts = ShotOpts { mode: ShotMode::Full, ..Default::default() };
        assert!(plan_shot(&opts, Path::new("/tmp/a.png"), &linux_x11(&[])).is_empty());
        let plan = plan_shot(&opts, Path::new("/tmp/a.png"), &linux_x11(&["scrot", "ffmpeg"]));
        assert_eq!(plan.iter().map(|a| a.tool.as_str()).collect::<Vec<_>>(), ["scrot", "ffmpeg"]);
        // Region is a human dragging a box — ffmpeg must not stand in for it.
        let region = ShotOpts { mode: ShotMode::Region, ..Default::default() };
        let plan = plan_shot(&region, Path::new("/tmp/a.png"), &linux_x11(&["maim", "ffmpeg"]));
        assert!(plan.iter().all(|a| a.tool != "ffmpeg"), "{plan:?}");
    }

    /// Recording options reach the tool, and one that cannot honour them
    /// says so instead of recording the wrong thing.
    #[test]
    fn recording_options_reach_the_backend_or_fail_loudly() {
        let out = Path::new("/tmp/r.mp4");
        let opts = RecordOpts {
            area: Some(Rect { x: 8, y: 16, w: 320, h: 240 }),
            fps: 60,
            audio: AudioSource::Both,
            cursor: false,
            duration: Some(2.5),
            ..Default::default()
        };
        let plan = plan_recording(&opts, out, &linux_x11(&["ffmpeg"]), Some("sink.monitor")).unwrap();
        let j = plan.args.join(" ");
        assert_eq!(plan.tool, "ffmpeg");
        assert!(j.contains("-framerate 60"), "{j}");
        assert!(j.contains("-video_size 320x240") && j.contains(":0+8,16"), "{j}");
        assert!(j.contains("-draw_mouse 0"), "cursor off: {j}");
        assert!(j.contains("-i sink.monitor") && j.contains("-i default"), "both sources: {j}");
        assert!(j.contains("amix=inputs=2:normalize=0"), "mixed without halving: {j}");
        assert!(j.contains("-t 2.500"), "fixed length: {j}");

        // System audio only = one leg, mapped straight through.
        let one = plan_recording(
            &RecordOpts { audio: AudioSource::System, ..Default::default() },
            out,
            &linux_x11(&["ffmpeg"]),
            Some("sink.monitor"),
        )
        .unwrap();
        assert!(one.args.join(" ").contains("-map 1:a"));
        assert!(!one.args.join(" ").contains("amix"));

        // Wayland with wf-recorder: its own geometry spelling.
        let wl = Env { os: Os::Linux, wayland: true, tools: vec!["wf-recorder".into()], display: ":0".into() };
        let plan = plan_recording(&opts, out, &wl, Some("sink.monitor")).unwrap();
        assert_eq!(plan.tool, "wf-recorder");
        assert_eq!(plan.stop, StopMethod::Interrupt);
        assert!(plan.args.join(" ").contains("-g 8,16 320x240"));

        // gpu-screen-recorder has no rectangle — that is an error naming
        // the tools that do, not a full-screen recording nobody asked for.
        let gsr = Env { os: Os::Linux, wayland: true, tools: vec!["gpu-screen-recorder".into()], display: ":0".into() };
        let err = plan_recording(&opts, out, &gsr, None).unwrap_err().to_string();
        assert!(err.contains("wf-recorder"), "{err}");
        // …and without an area it plans fine.
        assert!(plan_recording(&RecordOpts::default(), out, &gsr, None).is_ok());

        // A Wayland session with no headless recorder says which to install.
        let bare = Env { os: Os::Linux, wayland: true, tools: vec![], display: ":0".into() };
        let err = plan_recording(&RecordOpts::default(), out, &bare, None).unwrap_err().to_string();
        assert!(err.contains("wf-recorder") && err.contains("wl-screenrec"), "{err}");

        // A camera is ffmpeg's v4l2 input, with the mic mixed in.
        let cam = plan_recording(
            &RecordOpts { webcam: Some("/dev/video0".into()), audio: AudioSource::Mic, ..Default::default() },
            out,
            &linux_x11(&["ffmpeg"]),
            None,
        )
        .unwrap();
        let j = cam.args.join(" ");
        assert!(j.contains("-f v4l2") && j.contains("-i /dev/video0") && j.contains("-i default"), "{j}");
    }

    #[test]
    fn audio_sources_parse_the_words_people_use() {
        assert_eq!(AudioSource::parse("system").unwrap(), AudioSource::System);
        assert_eq!(AudioSource::parse("Desktop").unwrap(), AudioSource::System);
        assert_eq!(AudioSource::parse("mic").unwrap(), AudioSource::Mic);
        assert_eq!(AudioSource::parse("both").unwrap(), AudioSource::Both);
        assert_eq!(AudioSource::parse("none").unwrap(), AudioSource::None);
        assert!(AudioSource::parse("loud").is_err());
    }
}

/// Live tests against a real X server (Xvfb in CI and on the bench). These
/// prove the capability end to end — a rectangle really comes back at that
/// size, a timed recording really lands as a playable file, and a detached
/// session really survives the process that started it.
#[cfg(all(test, target_os = "linux"))]
mod live_tests {
    use super::*;

    struct Xvfb {
        child: Child,
        display: String,
    }

    impl Drop for Xvfb {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// A private X server so the tests never touch the user's screen.
    fn xvfb(num: u32, size: &str) -> Option<Xvfb> {
        if !have("Xvfb") || !have("ffmpeg") {
            return None;
        }
        let display = format!(":{num}");
        let child = Command::new("Xvfb")
            .args([&display, "-screen", "0", size])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // Wait for the server to accept connections.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let ok = Command::new("ffmpeg")
                .args(["-v", "error", "-f", "x11grab", "-video_size", "64x64", "-i", &display, "-frames:v", "1", "-f", "null", "-"])
                .env("DISPLAY", &display)
                .stdin(Stdio::null())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                return Some(Xvfb { child, display });
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let mut x = Xvfb { child, display };
        let _ = x.child.kill();
        None
    }

    fn env_for(display: &str) -> Env {
        Env {
            os: Os::Linux,
            wayland: false,
            tools: KNOWN_TOOLS.iter().filter(|t| have(t)).map(|t| t.to_string()).collect(),
            display: display.to_string(),
        }
    }

    /// A rectangle asked for is a rectangle delivered — measured on a real
    /// X server, through whichever backend the planner picked.
    #[test]
    fn an_area_screenshot_comes_back_at_that_exact_size() {
        let Some(x) = xvfb(96, "800x600x24") else {
            eprintln!("no Xvfb/ffmpeg — skipping");
            return;
        };
        let out = std::env::temp_dir().join(format!("reel-shot-area-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let r = Rect { x: 40, y: 20, w: 320, h: 240 };
        let opts = ShotOpts { mode: ShotMode::Area(r), out: Some(out.clone()), ..Default::default() };
        let env = env_for(&x.display);
        let attempts = plan_shot(&opts, &out, &env);
        assert!(!attempts.is_empty(), "no screenshot backend planned");
        let a = attempts.iter().find(|a| a.tool == "ffmpeg").expect("ffmpeg is the floor");
        let st = Command::new(&a.tool)
            .args(&a.args)
            .env("DISPLAY", &x.display)
            .status()
            .expect("run backend");
        assert!(st.success(), "{} failed", a.tool);
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe png");
        assert_eq!((info.width, info.height), (320, 240), "the rectangle, exactly");
        let _ = std::fs::remove_file(&out);
    }

    /// A timed recording of a real screen: the planner's command, run, and
    /// the file it leaves behind is playable and about the right length.
    #[test]
    fn a_timed_recording_lands_as_a_playable_file() {
        let Some(x) = xvfb(95, "640x480x24") else {
            eprintln!("no Xvfb/ffmpeg — skipping");
            return;
        };
        let out = std::env::temp_dir().join(format!("reel-rec-timed-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let opts = RecordOpts {
            out: Some(out.clone()),
            area: Some(Rect { x: 0, y: 0, w: 320, h: 240 }),
            fps: 15,
            audio: AudioSource::None,
            duration: Some(1.5),
            ..Default::default()
        };
        let plan = plan_recording(&opts, &out, &env_for(&x.display), None).expect("plan");
        let st = Command::new(&plan.tool)
            .args(&plan.args)
            .env("DISPLAY", &x.display)
            .stdin(Stdio::null())
            .status()
            .expect("run recorder");
        assert!(st.success(), "{} failed", plan.tool);
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe recording");
        assert_eq!((info.width, info.height), (320, 240), "the area, exactly");
        assert!(info.duration > 1.0 && info.duration < 2.5, "≈1.5s, got {}", info.duration);
        let _ = std::fs::remove_file(&out);
    }

    /// The agent contract: start returns immediately, the recording keeps
    /// running in another process, and a later stop finalizes a real file.
    #[test]
    fn a_detached_session_outlives_the_call_that_started_it() {
        let Some(x) = xvfb(94, "640x480x24") else {
            eprintln!("no Xvfb/ffmpeg — skipping");
            return;
        };
        // Point the session file and the grab at this test's own X server.
        let cache = std::env::temp_dir().join(format!("reel-sess-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&cache);
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::env::set_var("DISPLAY", &x.display);
        std::env::remove_var("WAYLAND_DISPLAY");

        let out = std::env::temp_dir().join(format!("reel-rec-sess-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let opts = RecordOpts {
            out: Some(out.clone()),
            fps: 15,
            audio: AudioSource::None,
            ..Default::default()
        };
        let s = start_detached(&opts).expect("start detached");
        assert!(alive(s.pid), "the recorder should be running");
        assert!(active_session().is_some(), "the session should be discoverable");
        // A second start must refuse rather than fight over the file.
        assert!(start_detached(&opts).is_err(), "one recording at a time");
        std::thread::sleep(Duration::from_millis(1200));
        let (stopped, path) = stop_detached().expect("stop detached");
        assert_eq!(stopped.pid, s.pid);
        assert!(active_session().is_none(), "the session is cleared on stop");
        let info = crate::video::decoder::probe(&path.to_string_lossy()).expect("probe recording");
        assert!(info.duration > 0.4, "a real recording, got {}", info.duration);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// The streamer assembler: two recordings become one editable project —
    /// screen on V1, camera as a corner PiP, length = the shorter of the two.
    #[test]
    fn streamer_project_assembles_from_two_files() {
        let dir = std::env::temp_dir();
        let mk = |name: &str, size: &str, dur: f64| -> std::path::PathBuf {
            let f = dir.join(format!("reel-strm-{name}-{}.mp4", std::process::id()));
            assert!(Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-f", "lavfi",
                       "-i", &format!("testsrc2=size={size}:rate=30:duration={dur}"),
                       "-pix_fmt", "yuv420p", &f.to_string_lossy()])
                .status().map(|s| s.success()).unwrap_or(false));
            f
        };
        let screen = mk("screen", "640x360", 3.0);
        let cam = mk("cam", "320x240", 2.5);
        let path = assemble_streamer_project(&screen.to_string_lossy(), &cam.to_string_lossy())
            .expect("assemble");
        let p = crate::edit::Project::load(&path).expect("load project");
        assert_eq!((p.width, p.height), (640, 360), "canvas = the screen");
        let v1 = p.tracks.iter().find(|t| t.kind == crate::edit::TrackKind::Video).unwrap();
        let ov = p.tracks.iter().find(|t| t.kind == crate::edit::TrackKind::Overlay).unwrap();
        assert_eq!(v1.clips.len(), 1);
        assert_eq!(ov.clips.len(), 1);
        assert!((v1.clips[0].duration - 2.5).abs() < 0.2, "trimmed to the shorter take");
        assert!(ov.clips[0].pip.x > 0.7 && ov.clips[0].pip.scale < 0.4, "corner cam");
        for f in [&screen, &cam, &std::path::PathBuf::from(&path)] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// End to end on real hardware: 2 s from the webcam lands as a
    /// playable clip. Skips cleanly on machines without a camera.
    #[test]
    fn webcam_records_a_real_clip() {
        if webcam_device().is_none() {
            eprintln!("no webcam on this machine — skipping");
            return;
        }
        let rec = match start_webcam_recording() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("webcam busy or unavailable — skipping ({e})");
                return;
            }
        };
        std::thread::sleep(std::time::Duration::from_secs(2));
        let path = rec.stop().expect("finalize webcam clip");
        let info = crate::video::decoder::probe(&path.to_string_lossy()).expect("probe clip");
        assert!(info.width > 0 && info.duration > 0.5, "real frames: {info:?}");
        let _ = std::fs::remove_file(&path);
    }
}

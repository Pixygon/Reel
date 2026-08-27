//! Screen capture — screenshots and screen recording, straight into Reel.
//! Nothing is bundled: we drive the best capture tool the system offers and
//! open the result in the player the moment it lands. Backends are probed at
//! runtime, so installing a tool lights the feature up on next use.
//!
//! Screenshots (first hit wins):
//!   Linux/Wayland: spectacle (KDE) → grim (wlroots) → flameshot
//!   Linux/X11:     spectacle → maim → scrot → import (ImageMagick)
//!   Windows:       ffmpeg gdigrab (one frame)   macOS: screencapture
//!
//! Recording (first hit wins):
//!   Linux/Wayland: gpu-screen-recorder → wf-recorder → wl-screenrec
//!   Linux/X11:     the same, then ffmpeg x11grab
//!   Windows:       ffmpeg gdigrab                macOS: screencapture -v
//! Stopped with SIGINT (or 'q' on ffmpeg's stdin) so the file finalizes.

use anyhow::{anyhow, bail, Result};
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

/// What to capture in a screenshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShotMode {
    /// The whole desktop.
    Full,
    /// Drag-select a rectangle.
    Region,
    /// The active window.
    Window,
}

/// Take a screenshot. Region/Window modes wait for the user's selection —
/// call from a worker thread. Returns the saved file.
pub fn screenshot(mode: ShotMode) -> Result<PathBuf> {
    let out = stamped(&out_dir("Pictures"), "shot", "png");
    let out_s = out.to_string_lossy().to_string();

    let attempts: Vec<(&str, Vec<String>)> = if cfg!(target_os = "windows") {
        vec![("ffmpeg", vec!["-y".into(), "-f".into(), "gdigrab".into(), "-i".into(), "desktop".into(), "-frames:v".into(), "1".into(), out_s.clone()])]
    } else if cfg!(target_os = "macos") {
        let flag = match mode {
            ShotMode::Full => vec!["-x".into()],
            ShotMode::Region => vec!["-x".into(), "-i".into()],
            ShotMode::Window => vec!["-x".into(), "-i".into(), "-W".into()],
        };
        vec![("screencapture", [flag, vec![out_s.clone()]].concat())]
    } else {
        // Linux: spectacle covers all three modes from the CLI; grim/maim
        // handle full/region; the portal's interactive dialog is the
        // built-in, tool-free fallback (it offers all modes itself).
        let spectacle_mode = match mode {
            ShotMode::Full => vec![],
            ShotMode::Region => vec!["-r".into()],
            ShotMode::Window => vec!["-a".into()],
        };
        let mut v: Vec<(&str, Vec<String>)> = vec![(
            "spectacle",
            [vec!["-b".into(), "-n".into()], spectacle_mode, vec!["-o".into(), out_s.clone()]].concat(),
        )];
        if is_wayland() {
            match mode {
                ShotMode::Full => v.push(("grim", vec![out_s.clone()])),
                ShotMode::Region | ShotMode::Window => {
                    // grim needs slurp for selection; run through a shell.
                    if have("grim") && have("slurp") {
                        v.push(("sh", vec!["-c".into(), format!("grim -g \"$(slurp)\" '{out_s}'")]));
                    }
                }
            }
        } else {
            match mode {
                ShotMode::Full => {
                    v.push(("maim", vec![out_s.clone()]));
                    v.push(("scrot", vec![out_s.clone()]));
                }
                ShotMode::Region => v.push(("maim", vec!["-s".into(), out_s.clone()])),
                ShotMode::Window => v.push(("scrot", vec!["-u".into(), out_s.clone()])),
            }
        }
        v
    };

    for (tool, args) in &attempts {
        if !have(tool) {
            continue;
        }
        let status = Command::new(tool)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Ok(st) = status {
            // Some tools return before the file hits disk; give it a moment.
            let deadline = Instant::now() + Duration::from_secs(3);
            while st.success() && !out.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if out.exists() {
                return Ok(out);
            }
        }
        log::warn!("screenshot via {tool} failed; trying next backend");
    }

    // Built-in fallback: the portal's interactive dialog (its own UI offers
    // screen/window/region, whatever `mode` asked for).
    #[cfg(target_os = "linux")]
    if crate::portal::available() {
        return crate::portal::screenshot_interactive(out);
    }
    bail!("no screenshot backend worked")
}

enum StopMethod {
    /// SIGINT — the tool finalizes its file and exits.
    Interrupt,
    /// Write 'q' to stdin (ffmpeg's graceful quit).
    QuitKey,
}

/// A screen recording in progress. `stop()` finalizes and returns the file.
pub struct Recorder {
    child: Child,
    stop: StopMethod,
    pub path: PathBuf,
    pub tool: &'static str,
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
}

fn external_recording_tool() -> Option<&'static str> {
    let tools: &[&str] = if cfg!(target_os = "windows") {
        &["ffmpeg"]
    } else if cfg!(target_os = "macos") {
        &["screencapture"]
    } else if is_wayland() {
        &["gpu-screen-recorder", "wf-recorder", "wl-screenrec"]
    } else {
        &["gpu-screen-recorder", "wf-recorder", "wl-screenrec", "ffmpeg"]
    };
    tools.iter().copied().find(|t| have(t))
}

/// Start a screen recording. On Linux the built-in portal path runs first —
/// the system picker lets the user choose screen/window/region, no external
/// tools needed; external recorders remain as fallbacks. Blocks through the
/// picker: call from a worker thread.
pub fn start_recording() -> Result<Recording> {
    #[cfg(target_os = "linux")]
    if crate::portal::available() {
        let out = stamped(&out_dir("Videos"), "rec", "mp4");
        match crate::portal::start_recording(out) {
            Ok(r) => return Ok(Recording::Portal(r)),
            Err(e) => log::warn!("built-in portal recording unavailable ({e}); trying external tools"),
        }
    }
    start_tool_recording().map(Recording::Tool)
}

/// Start a full-screen recording via an external capture tool.
fn start_tool_recording() -> Result<Recorder> {
    let out = stamped(&out_dir("Videos"), "rec", "mp4");
    let out_s = out.to_string_lossy().to_string();
    let tool = external_recording_tool().ok_or_else(|| {
        anyhow!("no screen recording backend available on this system")
    })?;

    let (args, stop): (Vec<String>, StopMethod) = match tool {
        "gpu-screen-recorder" => (
            // -w screen: the focused monitor; NVENC/VA-API encode; system audio.
            vec!["-w".into(), "screen".into(), "-f".into(), "60".into(), "-a".into(), "default_output".into(), "-o".into(), out_s.clone()],
            StopMethod::Interrupt,
        ),
        "wf-recorder" => (vec!["-f".into(), out_s.clone()], StopMethod::Interrupt),
        "wl-screenrec" => (vec!["-f".into(), out_s.clone()], StopMethod::Interrupt),
        "screencapture" => (vec!["-v".into(), out_s.clone()], StopMethod::Interrupt),
        "ffmpeg" if cfg!(target_os = "windows") => (
            vec!["-y".into(), "-f".into(), "gdigrab".into(), "-framerate".into(), "30".into(), "-i".into(), "desktop".into(),
                 "-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-pix_fmt".into(), "yuv420p".into(), out_s.clone()],
            StopMethod::QuitKey,
        ),
        "ffmpeg" => (
            vec!["-y".into(), "-f".into(), "x11grab".into(), "-framerate".into(), "30".into(),
                 "-i".into(), std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()),
                 "-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-pix_fmt".into(), "yuv420p".into(), out_s.clone()],
            StopMethod::QuitKey,
        ),
        _ => unreachable!(),
    };

    let mut cmd = Command::new(tool);
    cmd.args(&args).stdout(Stdio::null()).stderr(Stdio::null());
    if matches!(stop, StopMethod::QuitKey) {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let child = cmd.spawn().map_err(|e| anyhow!("{tool} failed to start: {e}"))?;
    log::info!("screen recording via {tool} → {}", out.display());
    Ok(Recorder { child, stop, path: out, tool })
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
            StopMethod::Interrupt => {
                #[cfg(unix)]
                {
                    let _ = Command::new("kill")
                        .args(["-INT", &self.child.id().to_string()])
                        .status();
                }
                #[cfg(not(unix))]
                {
                    let _ = self.child.kill();
                }
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamped_names_are_sane() {
        let p = stamped(Path::new("/tmp"), "shot", "png");
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("reel-shot-2"), "{name}");
        assert!(name.ends_with(".png"));
        // reel-shot-YYYYMMDD-HHMMSS.png
        assert_eq!(name.len(), "reel-shot-20260827-120000.png".len(), "{name}");
    }
}

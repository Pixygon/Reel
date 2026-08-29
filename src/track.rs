//! Motion tracking for power windows — follow a subject so the grade does.
//!
//! Classic template matching: decode the clip's window as small grayscale
//! frames, take a patch around the mask's centre in the first frame, and in
//! each following frame find where that patch went by zero-mean normalised
//! cross-correlation over a local search area. The found path becomes
//! `Param::MaskX/MaskY` keyframes — evaluated by the SAME `animated()` call
//! sites everything else uses, so a tracked window renders identically in
//! the preview, the frame server and the graph.
//!
//! The correlation is pure and unit-tested; ffmpeg only supplies pixels.

use anyhow::{anyhow, Context, Result};
use std::io::Read;

/// Tracking resolution. Frames are scaled (distorting freely — the outputs
/// are per-axis fractions, which survive any linear scale) to this grid.
const TW: usize = 192;
const TH: usize = 108;
/// Samples per second of source — the keyframe density.
const FPS: f64 = 10.0;
/// Search radius around the last position, in grid pixels. At 192 wide this
/// is ±13% of the frame per step — fast pans still land inside it at 10 Hz.
const SEARCH: isize = 25;

/// One tracked position: seconds into the decoded window, centre fractions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackPoint {
    pub t: f64,
    pub cx: f32,
    pub cy: f32,
}

/// Track the patch under (cx, cy) through `src_len` seconds of `source`
/// starting at `src_start`. Half-extents pick the patch size (fractions of
/// the frame, clamped to something matchable).
pub fn track_region(
    source: &str,
    src_start: f64,
    src_len: f64,
    cx: f32,
    cy: f32,
    half_w: f32,
    half_h: f32,
) -> Result<Vec<TrackPoint>> {
    let frames = decode_gray(source, src_start, src_len)?;
    if frames.len() < 2 {
        return Err(anyhow!("not enough frames to track (got {})", frames.len()));
    }
    Ok(track_frames(&frames, cx, cy, half_w, half_h))
}

/// The pure half: follow the patch through already-decoded frames.
pub fn track_frames(
    frames: &[Vec<u8>],
    cx: f32,
    cy: f32,
    half_w: f32,
    half_h: f32,
) -> Vec<TrackPoint> {
    // Patch half-extents in grid pixels. 1.5× the window so the SUBJECT'S
    // EDGES are inside the patch — a patch fully inside a flat subject has
    // no variance and nothing to correlate on.
    let pw = ((half_w * 1.5 * TW as f32) as isize).clamp(6, 40);
    let ph = ((half_h * 1.5 * TH as f32) as isize).clamp(6, 32);
    let mut x = (cx * TW as f32) as isize;
    let mut y = (cy * TH as f32) as isize;
    let mut out = vec![TrackPoint { t: 0.0, cx, cy }];
    let mut template = patch(&frames[0], x, y, pw, ph);
    for (i, frame) in frames.iter().enumerate().skip(1) {
        let (bx, by, score) = best_match(frame, &template, x, y, pw, ph);
        // A collapsed correlation means the subject left the search area (or
        // the frame) — stop rather than wander.
        if score < 0.2 {
            break;
        }
        x = bx;
        y = by;
        out.push(TrackPoint {
            t: i as f64 / FPS,
            cx: (x as f32 + 0.5) / TW as f32,
            cy: (y as f32 + 0.5) / TH as f32,
        });
        // Refresh the template so slow appearance changes (lighting, angle)
        // don't decay the match. Drift risk is bounded by the short refresh
        // interval at 10 Hz.
        template = patch(frame, x, y, pw, ph);
    }
    out
}

/// Extract a (2pw+1)×(2ph+1) patch centred on (x, y), edge-clamped.
fn patch(frame: &[u8], x: isize, y: isize, pw: isize, ph: isize) -> Vec<f32> {
    let mut p = Vec::with_capacity(((2 * pw + 1) * (2 * ph + 1)) as usize);
    for dy in -ph..=ph {
        for dx in -pw..=pw {
            let sx = (x + dx).clamp(0, TW as isize - 1);
            let sy = (y + dy).clamp(0, TH as isize - 1);
            p.push(frame[(sy * TW as isize + sx) as usize] as f32);
        }
    }
    p
}

/// Zero-mean NCC over the search window; returns (x, y, best score).
fn best_match(
    frame: &[u8],
    template: &[f32],
    x0: isize,
    y0: isize,
    pw: isize,
    ph: isize,
) -> (isize, isize, f32) {
    let tmean = template.iter().sum::<f32>() / template.len() as f32;
    let tvar: f32 = template.iter().map(|v| (v - tmean) * (v - tmean)).sum();
    let (mut best, mut bx, mut by) = (-1.0f32, x0, y0);
    for dy in -SEARCH..=SEARCH {
        for dx in -SEARCH..=SEARCH {
            let (cx, cy) = (x0 + dx, y0 + dy);
            if cx - pw < 0 || cx + pw >= TW as isize || cy - ph < 0 || cy + ph >= TH as isize {
                continue;
            }
            let cand = patch(frame, cx, cy, pw, ph);
            let cmean = cand.iter().sum::<f32>() / cand.len() as f32;
            let mut num = 0.0f32;
            let mut cvar = 0.0f32;
            for (t, c) in template.iter().zip(&cand) {
                num += (t - tmean) * (c - cmean);
                cvar += (c - cmean) * (c - cmean);
            }
            let denom = (tvar * cvar).sqrt();
            let score = if denom > 1e-3 { num / denom } else { 0.0 };
            if score > best {
                best = score;
                bx = cx;
                by = cy;
            }
        }
    }
    (bx, by, best)
}

/// Decode a window of the source as TW×TH grayscale frames at `FPS`.
fn decode_gray(source: &str, start: f64, len: f64) -> Result<Vec<Vec<u8>>> {
    let mut child = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-ss", &format!("{start:.3}"),
            "-t", &format!("{:.3}", len.max(0.2)),
            "-i", source,
            "-vf", &format!("fps={FPS},scale={TW}:{TH}:flags=bilinear,format=gray"),
            "-f", "rawvideo", "-pix_fmt", "gray", "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawn ffmpeg for tracking")?;
    let mut stdout = child.stdout.take().unwrap();
    let mut frames = Vec::new();
    loop {
        let mut buf = vec![0u8; TW * TH];
        let mut got = 0usize;
        while got < buf.len() {
            match stdout.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => return Err(anyhow!("tracking decode read failed: {e}")),
            }
        }
        if got < buf.len() {
            break;
        }
        frames.push(buf);
    }
    let _ = child.wait();
    Ok(frames)
}

/// Track a clip's mask and return the MaskX/MaskY keyframe tracks in
/// clip-local OUTPUT time (what `Clip.keys` stores). Constant speed is
/// honoured; ramps sample at the clip's average rate (the track follows,
/// timing shifts slightly inside ramped sections).
pub fn track_clip(clip: &crate::edit::Clip) -> Result<(Vec<crate::edit::Keyframe>, Vec<crate::edit::Keyframe>)> {
    let mask = clip
        .effects
        .mask
        .ok_or_else(|| anyhow!("clip {} has no power window to track — add a mask first", clip.id))?;
    let src_len = clip.source_len();
    let points = track_region(
        &clip.source,
        clip.in_point,
        src_len,
        mask.cx,
        mask.cy,
        mask.w,
        mask.h,
    )?;
    let mut kx = Vec::with_capacity(points.len());
    let mut ky = Vec::with_capacity(points.len());
    for p in &points {
        let t_out = clip.output_time_for_source(p.t);
        kx.push(crate::edit::Keyframe { t: t_out, value: p.cx, interp: crate::edit::Interp::Linear });
        ky.push(crate::edit::Keyframe { t: t_out, value: p.cy, interp: crate::edit::Interp::Linear });
    }
    Ok((kx, ky))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with_square(cx: usize, cy: usize) -> Vec<u8> {
        let mut f = vec![10u8; TW * TH];
        for y in cy.saturating_sub(8)..(cy + 8).min(TH) {
            for x in cx.saturating_sub(8)..(cx + 8).min(TW) {
                f[y * TW + x] = 240;
            }
        }
        // A little texture so the patch is distinctive beyond its edges.
        for y in (cy.saturating_sub(3))..(cy + 3).min(TH) {
            for x in (cx.saturating_sub(3))..(cx + 3).min(TW) {
                f[y * TW + x] = 120;
            }
        }
        f
    }

    /// The pure tracker follows a synthetic square drifting across frames.
    #[test]
    fn the_tracker_follows_a_moving_square() {
        let frames: Vec<Vec<u8>> = (0..12)
            .map(|i| frame_with_square(40 + i * 8, 54 + i * 2))
            .collect();
        let pts = track_frames(&frames, 40.5 / TW as f32, 54.5 / TH as f32, 0.08, 0.14);
        assert_eq!(pts.len(), 12, "every frame yields a point");
        let last = pts.last().unwrap();
        let expect_x = (40.0 + 11.0 * 8.0 + 0.5) / TW as f32;
        let expect_y = (54.0 + 11.0 * 2.0 + 0.5) / TH as f32;
        assert!(
            (last.cx - expect_x).abs() < 0.02,
            "x drifted: got {} expected {expect_x}",
            last.cx
        );
        assert!(
            (last.cy - expect_y).abs() < 0.03,
            "y drifted: got {} expected {expect_y}",
            last.cy
        );
        // The path is monotone rightward — no wandering.
        for pair in pts.windows(2) {
            assert!(pair[1].cx >= pair[0].cx - 0.01);
        }
    }

    /// A square that leaves the frame stops the track instead of wandering.
    #[test]
    fn a_lost_subject_ends_the_track() {
        let mut frames: Vec<Vec<u8>> = (0..4)
            .map(|i| frame_with_square(160 + i * 10, 54))
            .collect();
        // Then it is gone: flat frames.
        frames.extend((0..4).map(|_| vec![10u8; TW * TH]));
        let pts = track_frames(&frames, 160.5 / TW as f32, 54.5 / TH as f32, 0.08, 0.14);
        assert!(pts.len() < 8, "the track must stop when the subject vanishes, got {} points", pts.len());
    }

    /// End to end through real ffmpeg: a white square marches across a real
    /// encoded video, and the tracked window follows it.
    #[test]
    fn tracks_a_real_rendered_square() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-track-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&src);
        // 2s, square moves left→right across the middle: x = 40 + 240·t.
        // (overlay, not drawbox: drawbox evaluates its expressions once at
        // init where t is NaN, so an animated drawbox never draws.)
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "color=c=0x202020:size=640x360:rate=30:duration=2",
                   "-f", "lavfi", "-i", "color=c=white:size=80x80:rate=30:duration=2",
                   "-filter_complex", "[0][1]overlay=x='40+240*t':y=140",
                   "-pix_fmt", "yuv420p", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        // Start on the square: at t=0 its centre is (80, 180) → (0.125, 0.5).
        let pts = track_region(&src.to_string_lossy(), 0.0, 2.0, 0.125, 0.5, 0.06, 0.11)
            .expect("track");
        assert!(pts.len() >= 15, "expected ~20 points, got {}", pts.len());
        let last = pts.last().unwrap();
        // At its last sampled moment the centre is ≈ (80 + 240·t) / 640.
        let expect_x = ((80.0 + 240.0 * last.t) / 640.0) as f32;
        assert!(
            (last.cx - expect_x).abs() < 0.05,
            "tracked x {} vs real {expect_x} at t={}",
            last.cx, last.t
        );
        assert!((last.cy - 0.5).abs() < 0.05, "y should hold the middle, got {}", last.cy);
        let _ = std::fs::remove_file(&src);
    }
}

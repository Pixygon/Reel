//! Frame sources for the frame server: sequential RGBA readers over ffmpeg.
//!
//! Export walks time forward, so each segment gets one ffmpeg process that
//! decodes exactly its window — seeked, trimmed, speed-adjusted, fitted to
//! the output frame and rate-conformed — and hands frames over a pipe, one
//! `read_exact` per frame. No seeking mid-stream, no cache: the renderer
//! asks for the next frame until the segment is done.
//!
//! The fit/rate work stays in ffmpeg on purpose: swscale's Lanczos and fps
//! conformance are exactly what the graph renderer used, so the pixels
//! entering the compositor match what the old path fed the encoder.

use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};

/// Sequential frames of one segment, fitted to `w`×`h` at `fps`.
pub struct SegmentReader {
    child: Child,
    out: ChildStdout,
    frame_len: usize,
    /// The last successfully read frame — repeated if the decoder comes up
    /// short, so an off-by-one at a segment edge never desyncs the render.
    last: Vec<u8>,
    any: bool,
}

impl SegmentReader {
    /// Open a reader for `duration` seconds of `source` starting `in_point`
    /// (source time), played at `speed`, fitted with `fit_chain` (the same
    /// scale/pad/setsar chain the graph renderer used, minus labels).
    pub fn open(
        source: &str,
        in_point: f64,
        duration: f64,
        speed: f64,
        pre_chain: Option<&str>,
        fit_chain: &str,
        (w, h, fps): (u32, u32, f64),
    ) -> Result<Self> {
        let src_len = duration * speed;
        let setpts = if (speed - 1.0).abs() > 1e-9 {
            format!("setpts=PTS/{speed:.6},")
        } else {
            String::new()
        };
        let pre = pre_chain.map(|c| format!("{c},")).unwrap_or_default();
        // -ss before -i: keyframe-fast seek, then accurate trim in the graph.
        let vf = format!(
            "trim=start=0:duration={src_len:.4},setpts=PTS-STARTPTS,{setpts}{pre}{fit_chain},fps={fps:.4}"
        );
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error",
                "-ss", &format!("{in_point:.4}"),
                "-i", source,
                "-vf", &vf,
                "-f", "rawvideo", "-pix_fmt", "rgba", "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .with_context(|| format!("could not start a decoder for {source}"))?;
        let out = child.stdout.take().ok_or_else(|| anyhow!("decoder has no stdout"))?;
        Ok(Self {
            child,
            out,
            frame_len: (w * h * 4) as usize,
            last: vec![0; (w * h * 4) as usize],
            any: false,
        })
    }

    /// The next frame, or the previous one repeated when the stream ends
    /// early. Returns false only when no frame was ever produced.
    pub fn next_into(&mut self, buf: &mut Vec<u8>) -> bool {
        buf.resize(self.frame_len, 0);
        match self.out.read_exact(buf) {
            Ok(()) => {
                self.last.copy_from_slice(buf);
                self.any = true;
                true
            }
            Err(_) => {
                if self.any {
                    buf.copy_from_slice(&self.last);
                    true
                } else {
                    false
                }
            }
        }
    }
}

impl Drop for SegmentReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Frames at the SOURCE's own rate, advanced to arbitrary source times —
/// what a speed RAMP needs: the output clock walks its curve and this reader
/// drops or holds source frames to keep up. No fps conformance, no setpts;
/// time is tracked by frame count over the probed rate.
pub struct NativeReader {
    child: Child,
    out: ChildStdout,
    frame_len: usize,
    /// Source seconds per decoded frame.
    frame_dt: f64,
    /// Source time of the frame currently in `last` (start-relative).
    at: f64,
    last: Vec<u8>,
    any: bool,
    eof: bool,
}

impl NativeReader {
    pub fn open(
        source: &str,
        in_point: f64,
        src_len: f64,
        pre_chain: Option<&str>,
        fit_chain: &str,
        (w, h): (u32, u32),
        src_fps: f64,
    ) -> Result<Self> {
        let pre = pre_chain.map(|c| format!("{c},")).unwrap_or_default();
        let vf =
            format!("trim=start=0:duration={src_len:.4},setpts=PTS-STARTPTS,{pre}{fit_chain}");
        let mut child = Command::new("ffmpeg")
            .args([
                "-v", "error",
                "-ss", &format!("{in_point:.4}"),
                "-i", source,
                "-vf", &vf,
                "-f", "rawvideo", "-pix_fmt", "rgba", "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .with_context(|| format!("could not start a native decoder for {source}"))?;
        let out = child.stdout.take().ok_or_else(|| anyhow!("decoder has no stdout"))?;
        let frame_len = (w * h * 4) as usize;
        Ok(Self {
            child,
            out,
            frame_len,
            frame_dt: 1.0 / src_fps.max(1.0),
            at: -1.0,
            last: vec![0; frame_len],
            any: false,
            eof: false,
        })
    }

    /// Land the frame nearest source time `want` (start-relative) in `buf`.
    /// Never rewinds — the ramp integral is monotonic, so neither do we.
    /// Holds the last frame at EOF. False only if nothing ever decoded.
    pub fn frame_at(&mut self, want: f64, buf: &mut Vec<u8>) -> bool {
        buf.resize(self.frame_len, 0);
        // Pull frames until the NEXT one would overshoot the ask.
        while !self.eof && self.at + self.frame_dt <= want + self.frame_dt * 0.5 {
            match self.out.read_exact(&mut self.last) {
                Ok(()) => {
                    self.any = true;
                    self.at = if self.at < 0.0 { 0.0 } else { self.at + self.frame_dt };
                }
                Err(_) => self.eof = true,
            }
        }
        if !self.any {
            return false;
        }
        buf.copy_from_slice(&self.last);
        true
    }
}

impl Drop for NativeReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The tone-mapping chain for an HDR source, or None for SDR (or when this
/// ffmpeg has no libplacebo). Runs BEFORE any scaling — tone-mapping resized
/// PQ pixels is wrong.
///
/// libplacebo, deliberately: the classic zscale+tonemap chain LOOKS right
/// but its final transfer encode silently no-ops on float RGB, leaving
/// linear values that read ~30% dark once quantized (found byte-by-byte at
/// the rawvideo pipe). libplacebo does the whole conversion in one filter,
/// mapped against the BT.2408 203-nit reference the industry grades by.
pub fn hdr_tonemap_chain(transfer: Option<&str>) -> Option<String> {
    match transfer {
        Some("smpte2084") | Some("arib-std-b67") if have_libplacebo() => Some(
            "libplacebo=colorspace=bt709:color_primaries=bt709:color_trc=bt709:tonemapping=hable:format=rgba"
                .to_string(),
        ),
        _ => None,
    }
}

/// Does this ffmpeg carry the libplacebo filter? Probed once per process.
pub fn have_libplacebo() -> bool {
    use std::sync::OnceLock;
    static HAVE: OnceLock<bool> = OnceLock::new();
    *HAVE.get_or_init(|| {
        Command::new("ffmpeg")
            .args(["-hide_banner", "-filters"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("libplacebo"))
            .unwrap_or(false)
    })
}

/// The scale/pad chain for overlay clips: fit the overlay's own frame into
/// its on-screen box while keeping aspect. The box height comes from the
/// scene rect, so the decode is only as large as the picture on screen.
pub fn overlay_fit_chain(w: u32, h: u32) -> String {
    format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,\
         pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0,setsar=1,format=rgba"
    )
}

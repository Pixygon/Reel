//! Compound clips — a whole edit used as one clip in another edit.
//!
//! Reel nests by RENDER-AND-REFRESH: a `.reel` added to a timeline is
//! rendered to a flat file beside it (`<name>.flat.mp4`), and THAT file is
//! what every pipeline consumes — preview, waveforms, thumbnails, export
//! all just see media. The clip remembers its origin (`Clip.nested`), and
//! whenever the nested project's file is newer than its flat render, the
//! flat render is refreshed. Honest trade: nesting costs a render, and in
//! exchange nothing else in the system needs to know nesting exists.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// Where a nested project's flat render lives: beside it, same stem.
pub fn flat_path(reel_path: &str) -> PathBuf {
    Path::new(reel_path).with_extension("flat.mp4")
}

/// Does the flat render need re-rendering? Missing counts as stale.
pub fn is_stale(reel_path: &str) -> bool {
    let flat = flat_path(reel_path);
    let m = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (m(Path::new(reel_path)), m(&flat)) {
        (Some(reel), Some(flat)) => reel > flat,
        _ => true,
    }
}

/// Render the nested project flat. Blocking — run on a worker or the CLI.
/// Returns the flat file's path.
pub fn render_flat(reel_path: &str) -> Result<PathBuf> {
    let proj = crate::edit::Project::load(reel_path)
        .map_err(|e| anyhow!("could not load {reel_path}: {e}"))?;
    let segments = proj.export_segments();
    if segments.is_empty() {
        return Err(anyhow!("{reel_path} has an empty timeline — nothing to nest"));
    }
    let out = flat_path(reel_path);
    let tmp = out.with_extension("part.mp4");
    let _ = std::fs::remove_file(&tmp);
    let settings = crate::export::ExportSettings {
        codec: crate::export::Codec::H264,
        quality: crate::export::Quality::High,
        resolution: crate::export::Resolution::Source,
        audio: crate::export::AudioMode::Encode { kbps: 192 },
        hardware: true,
        target: None,
        fit: crate::export::Fit::Letterbox,
        loudness: None,
        hdr_passthrough: false,
    };
    let overlays = crate::export::Overlays {
        captions: &proj.captions,
        caption_size: proj.caption_size,
        titles: &proj.titles,
        music: proj.music.as_ref(),
        overlays: &proj.overlay_segments(),
        markers: &[],
        marker_labels: &[],
        luts: &proj.luts,
        plugins: &proj.plugins,
        audio_clips: &proj.audio_clips(),
    };
    let job = crate::export::start_timeline_with_captions(
        &segments,
        &tmp.to_string_lossy(),
        &settings,
        (proj.width, proj.height, proj.fps),
        overlays,
    )?;
    loop {
        let st = job.state();
        if st.finished {
            if let Some(e) = st.error {
                let _ = std::fs::remove_file(&tmp);
                return Err(anyhow!("nested render failed: {e}"));
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    std::fs::rename(&tmp, &out)
        .map_err(|e| anyhow!("could not finalise the flat render: {e}"))?;
    Ok(out)
}

/// Refresh every stale compound a project uses. Returns how many rendered.
pub fn refresh_all(proj: &crate::edit::Project, quiet: bool) -> Result<usize> {
    let mut nested: Vec<String> = proj
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| c.nested.clone())
        .collect();
    nested.sort();
    nested.dedup();
    let mut n = 0;
    for reel in nested {
        if is_stale(&reel) {
            if !quiet {
                eprintln!("nested edit changed — re-rendering {reel}…");
            }
            render_flat(&reel)?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole nesting contract: a .reel becomes a flat file with the
    /// inner edit's length; editing the inner project makes it stale; a
    /// refresh re-renders to the new truth.
    #[test]
    fn a_nested_edit_renders_flat_and_refreshes_when_stale() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-nest-src-{}.mp4", std::process::id()));
        let inner = dir.join(format!("reel-nest-inner-{}.reel", std::process::id()));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=4",
                   "-f", "lavfi", "-i", "sine=frequency=440:duration=4",
                   "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                   "-c:a", "aac", "-shortest", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        let mut p = crate::edit::Project::default();
        p.width = 320;
        p.height = 240;
        p.append_video("in", &src.to_string_lossy(), 2.0);
        p.save(&inner.to_string_lossy()).expect("save inner");

        assert!(is_stale(&inner.to_string_lossy()), "no flat render yet = stale");
        let flat = render_flat(&inner.to_string_lossy()).expect("flatten");
        let info = crate::video::decoder::probe(&flat.to_string_lossy()).expect("probe flat");
        assert!((info.duration - 2.0).abs() < 0.15, "flat carries the inner cut: {}", info.duration);
        assert!(!is_stale(&inner.to_string_lossy()), "fresh after rendering");

        // Edit the inner project: longer cut, newer file → stale again.
        std::thread::sleep(std::time::Duration::from_millis(1100)); // mtime granularity
        let mut p2 = crate::edit::Project::default();
        p2.width = 320;
        p2.height = 240;
        p2.append_video("in", &src.to_string_lossy(), 3.5);
        p2.save(&inner.to_string_lossy()).expect("resave inner");
        assert!(is_stale(&inner.to_string_lossy()), "a newer inner project is stale");
        let flat = render_flat(&inner.to_string_lossy()).expect("re-flatten");
        let info = crate::video::decoder::probe(&flat.to_string_lossy()).expect("probe flat 2");
        assert!((info.duration - 3.5).abs() < 0.15, "refresh follows the edit: {}", info.duration);

        for f in [&src, &inner, &flat] {
            let _ = std::fs::remove_file(f);
        }
    }
}

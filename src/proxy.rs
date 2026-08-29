//! Editing proxies — how a laptop edits 4K.
//!
//! Heavy sources (4K+, long-GOP HEVC) decode slowly and seek worse. The
//! classic answer, and ours: transcode a light 720p H.264 copy in the
//! background with a tight keyframe interval, and let the PREVIEW play that
//! while every honest consumer — export, waveforms, thumbnails, captions —
//! keeps the original. Same duration, same timing, so positions map 1:1 and
//! nothing else has to know.
//!
//! Proxies live in `~/.cache/reel/proxies`, keyed by (path, size, mtime) so
//! an edited source re-proxies and an untouched one is found instantly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

/// Sources at or above this height get a proxy. 1080p editing is already
/// smooth everywhere; UHD is where machines start to hurt.
const THRESHOLD_H: u32 = 1440;
/// The proxy's own height.
const PROXY_H: u32 = 720;

fn proxy_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache")
        });
    base.join("reel/proxies")
}

/// Stable identity for a source file's current contents (cheap: no hashing
/// of the data itself — size+mtime changes when the file does). Shared by
/// every disk cache keyed on a media file: proxies, waveforms, thumbnails.
pub fn file_key(path: &str) -> Option<String> {
    key(path)
}

fn key(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path
        .as_bytes()
        .iter()
        .chain(meta.len().to_le_bytes().iter())
        .chain(mtime.to_le_bytes().iter())
    {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    Some(format!("{h:016x}"))
}

pub fn proxy_path_for(source: &str) -> Option<PathBuf> {
    Some(proxy_dir().join(format!("{}.mp4", key(source)?)))
}

/// Build the proxy, blocking. Small frames, fast preset, keyframes every
/// half-second so scrubbing lands instantly, audio carried so A/V sync in
/// the preview is unchanged.
pub fn generate(source: &str, dest: &PathBuf) -> bool {
    std::fs::create_dir_all(proxy_dir()).ok();
    let tmp = dest.with_extension("part.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-i", source,
            "-vf", &format!("scale=-2:{PROXY_H}:flags=bilinear"),
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-g", "15",
            "-c:a", "aac", "-b:a", "128k",
            "-movflags", "+faststart",
            &tmp.to_string_lossy(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    std::fs::rename(&tmp, dest).is_ok()
}

/// Does this source deserve a proxy at all?
pub fn wants_proxy(width: u32, height: u32) -> bool {
    height.min(width) >= THRESHOLD_H || height.max(width) >= THRESHOLD_H * 2
}

/// Per-source proxy state, with background generation.
#[derive(Default)]
pub struct Cache {
    ready: HashMap<String, PathBuf>,
    pending: HashMap<String, ()>,
    /// Sources that don't need (or failed) a proxy — don't ask again.
    skip: HashMap<String, ()>,
    channel: Option<(Sender<(String, Option<PathBuf>)>, Receiver<(String, Option<PathBuf>)>)>,
}

impl Cache {
    /// The path the PREVIEW should open for `source`: the proxy when one is
    /// ready, the original otherwise. Kicks off generation in the background
    /// the first time a heavy source is asked about.
    pub fn preview_path(&mut self, source: &str) -> String {
        self.drain();
        if let Some(p) = self.ready.get(source) {
            return p.to_string_lossy().into_owned();
        }
        if self.skip.contains_key(source) || self.pending.contains_key(source) {
            return source.to_string();
        }
        // First sight of this source: is it heavy, and is a proxy already on
        // disk from an earlier session?
        let Some(dest) = proxy_path_for(source) else {
            self.skip.insert(source.into(), ());
            return source.to_string();
        };
        if dest.exists() {
            self.ready.insert(source.into(), dest.clone());
            log::info!("proxy: using cached {} for {source}", dest.display());
            return dest.to_string_lossy().into_owned();
        }
        let heavy = crate::video::decoder::probe(source)
            .map(|i| wants_proxy(i.width, i.height))
            .unwrap_or(false);
        if !heavy {
            self.skip.insert(source.into(), ());
            return source.to_string();
        }
        let (tx, _) = self.channel.get_or_insert_with(mpsc::channel);
        let (tx, src, d) = (tx.clone(), source.to_string(), dest);
        self.pending.insert(source.into(), ());
        log::info!("proxy: building a {PROXY_H}p editing copy of {src}");
        std::thread::spawn(move || {
            let ok = generate(&src, &d);
            let _ = tx.send((src, ok.then_some(d)));
        });
        source.to_string()
    }

    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// True when `path` is one of our proxies (the UI badges it honestly).
    pub fn is_proxy(&self, path: &str) -> bool {
        self.ready.values().any(|p| p.to_string_lossy() == path)
    }

    fn drain(&mut self) {
        let Some((_, rx)) = &self.channel else { return };
        let mut done = Vec::new();
        while let Ok(m) = rx.try_recv() {
            done.push(m);
        }
        for (src, path) in done {
            self.pending.remove(&src);
            match path {
                Some(p) => {
                    log::info!("proxy ready for {src}");
                    self.ready.insert(src, p);
                }
                None => {
                    log::warn!("proxy build failed for {src}; editing the original");
                    self.skip.insert(src, ());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_heavy_sources_want_proxies() {
        assert!(!wants_proxy(1920, 1080));
        assert!(!wants_proxy(1280, 720));
        assert!(wants_proxy(3840, 2160));
        assert!(wants_proxy(2160, 3840), "vertical 4K counts too");
        assert!(wants_proxy(2560, 1440));
    }

    #[test]
    fn the_key_tracks_the_file_not_the_name_alone() {
        let dir = std::env::temp_dir();
        let f = dir.join(format!("reel-proxykey-{}.bin", std::process::id()));
        std::fs::write(&f, b"aaaa").unwrap();
        let k1 = key(&f.to_string_lossy()).unwrap();
        // Same content, same key.
        assert_eq!(k1, key(&f.to_string_lossy()).unwrap());
        // Changed content (size) → different key → a stale proxy can't be
        // matched to a re-exported file of the same name.
        std::fs::write(&f, b"aaaaaa").unwrap();
        let k2 = key(&f.to_string_lossy()).unwrap();
        assert_ne!(k1, k2);
        let _ = std::fs::remove_file(&f);
        assert!(key("/definitely/not/here.mp4").is_none());
    }

    /// The real thing: a UHD source becomes a playable 720p proxy with the
    /// same duration, so every timeline position maps 1:1.
    #[test]
    fn a_proxy_preserves_duration_at_a_fraction_of_the_size() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-proxysrc-{}.mp4", std::process::id()));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "testsrc2=size=3840x2160:rate=30:duration=2",
                   "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                   "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                   "-c:a", "aac", "-shortest", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        let dest = proxy_path_for(&src.to_string_lossy()).expect("key");
        let _ = std::fs::remove_file(&dest);
        assert!(generate(&src.to_string_lossy(), &dest), "proxy build failed");

        let src_info = crate::video::decoder::probe(&src.to_string_lossy()).unwrap();
        let px_info = crate::video::decoder::probe(&dest.to_string_lossy()).unwrap();
        assert_eq!(px_info.height, 720);
        assert!(
            (px_info.duration - src_info.duration).abs() < 0.15,
            "proxy duration {} vs source {} — positions would drift",
            px_info.duration,
            src_info.duration
        );
        let src_bytes = std::fs::metadata(&src).unwrap().len();
        let px_bytes = std::fs::metadata(&dest).unwrap().len();
        assert!(
            px_bytes < src_bytes,
            "a proxy larger than its source ({px_bytes} vs {src_bytes}) helps no one"
        );
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dest);
    }
}

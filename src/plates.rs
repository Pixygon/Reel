//! Generated plates: a solid colour (or a fully transparent blank) as a real
//! PNG, so preview, render, thumbnails and proxies all see an ordinary still.
//! The file is a cache — `Clip.plate` holds the spec and `Project::load`
//! rebuilds it, so a project moved to another machine still opens.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

/// How long a still or plate stays on the timeline without `--duration`.
pub const DEFAULT_STILL_SECONDS: f64 = 5.0;

/// `color:RRGGBB`, `color:transparent` → the plate spec, lower-cased. Anything
/// else is not a plate.
pub fn spec_of(media: &str) -> Option<Result<String>> {
    let v = media.strip_prefix("color:")?.to_lowercase();
    let ok = v == "transparent" || (v.len() == 6 && v.chars().all(|c| c.is_ascii_hexdigit()));
    Some(if ok {
        Ok(v)
    } else {
        Err(anyhow!("a plate is color:RRGGBB or color:transparent, got {media:?}"))
    })
}

fn pixel(spec: &str) -> [u8; 4] {
    let byte = |i: usize| u8::from_str_radix(spec.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    if spec == "transparent" {
        [0, 0, 0, 0]
    } else {
        [byte(0), byte(2), byte(4), 255]
    }
}

fn path_for(spec: &str, w: u32, h: u32) -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache")
    });
    base.join("reel/plates").join(format!("plate-{spec}-{w}x{h}.png"))
}

/// The plate's file at this frame size, written if it isn't there yet. The
/// plate is frame-sized so no fit mode can letterbox it.
pub fn ensure(spec: &str, w: u32, h: u32) -> Result<String> {
    let path = path_for(spec, w.max(2), h.max(2));
    if !path.exists() {
        let dir = path.parent().ok_or_else(|| anyhow!("no cache folder"))?;
        std::fs::create_dir_all(dir)?;
        let px = pixel(spec);
        let rgba: Vec<u8> = px.iter().copied().cycle().take((w * h * 4) as usize).collect();
        // Temp + rename: two renders may race to write the same plate.
        let tmp = path.with_extension(format!("{}.tmp.png", std::process::id()));
        image::save_buffer(&tmp, &rgba, w, h, image::ColorType::Rgba8)
            .with_context(|| format!("could not write a plate to {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_and_refuse_nonsense() {
        assert_eq!(spec_of("color:FF8800").unwrap().unwrap(), "ff8800");
        assert_eq!(spec_of("color:transparent").unwrap().unwrap(), "transparent");
        assert!(spec_of("color:red").unwrap().is_err());
        assert!(spec_of("clip.mp4").is_none());
    }

    #[test]
    fn a_plate_is_the_colour_it_says() {
        let path = ensure("ff8800", 8, 4).unwrap();
        let img = image::open(&path).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (8, 4));
        assert_eq!(img.get_pixel(3, 2).0, [255, 136, 0, 255]);
        let blank = image::open(ensure("transparent", 8, 4).unwrap()).unwrap().to_rgba8();
        assert_eq!(blank.get_pixel(3, 2).0[3], 0);
    }
}

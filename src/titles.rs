//! Titles: text you place on the picture yourself.
//!
//! Captions transcribe speech; titles are the other half — a name, a lower
//! third, a call to action, an intro card. Editors reach for this constantly,
//! and it is one of the things people most often go *back* to Premiere for.
//!
//! Everything here is expressed as **fractions of the frame**: position,
//! size, outline. That is deliberate — it means a title composed on a 720p
//! preview lands in exactly the same place in a 4K render, and it means the
//! preview can be drawn from the same numbers the renderer uses instead of
//! approximating them. (Captions learned this the hard way: see
//! `captions::PLAY_RES_Y`.)
//!
//! Rendering goes through libass via ffmpeg's `subtitles` filter, using an
//! ASS document we generate with an explicit `PlayResX/Y`. Explicit PlayRes
//! is what makes `\pos()` mean exact pixels, so there is no scaling guesswork
//! between what we compute and what libass draws.

use serde::{Deserialize, Serialize};

/// One piece of text on the picture, for a window of timeline time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Title {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Centre of the text, as fractions of the frame (0,0 = top-left).
    pub x: f32,
    pub y: f32,
    /// Text height as a fraction of the frame height.
    pub size: f32,
    pub color: [u8; 3],
    pub bold: bool,
    /// A dark outline — what makes text readable over a busy shot.
    pub outline: bool,
    /// Fade up over this many seconds at the title's start.
    #[serde(default)]
    pub fade_in: f64,
    /// Fade out over this many seconds at the end.
    #[serde(default)]
    pub fade_out: f64,
    /// Slide in from this edge over `fade_in` seconds (None = appear in
    /// place). Compiled to an ASS \move — libass animates it; the preview
    /// evaluates the same path.
    #[serde(default)]
    pub slide_from: Slide,
    /// How far the slide travels, as a fraction of the frame.
    #[serde(default = "default_slide_dist")]
    pub slide_dist: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Slide {
    #[default]
    None,
    Left,
    Right,
    Top,
    Bottom,
}

fn default_slide_dist() -> f32 {
    0.12
}

impl Title {
    /// The title's animated state at absolute time `t`: centre position
    /// (fractions) and opacity. ONE formula — the preview draws it and the
    /// ASS compiles to the same numbers, so motion cannot lie.
    pub fn animated_at(&self, t: f64) -> ([f32; 2], f32) {
        let mut alpha = 1.0f32;
        let local = t - self.start;
        let remain = self.end - t;
        if self.fade_in > 0.0 && local < self.fade_in {
            alpha *= (local / self.fade_in).clamp(0.0, 1.0) as f32;
        }
        if self.fade_out > 0.0 && remain < self.fade_out {
            alpha *= (remain / self.fade_out).clamp(0.0, 1.0) as f32;
        }
        let mut pos = [self.x, self.y];
        if self.slide_from != Slide::None && self.fade_in > 0.0 && local < self.fade_in {
            let p = (local / self.fade_in).clamp(0.0, 1.0) as f32;
            // Ease-out: fast arrival, gentle settle — what \move looks like
            // is linear, so keep LINEAR to match libass exactly.
            let d = self.slide_dist * (1.0 - p);
            match self.slide_from {
                Slide::Left => pos[0] -= d,
                Slide::Right => pos[0] += d,
                Slide::Top => pos[1] -= d,
                Slide::Bottom => pos[1] += d,
                Slide::None => {}
            }
        }
        (pos, alpha)
    }
}

impl Default for Title {
    fn default() -> Self {
        Self {
            text: "Title".into(),
            start: 0.0,
            end: 3.0,
            x: 0.5,
            y: 0.5,
            size: 0.09,
            color: [255, 255, 255],
            bold: true,
            outline: true,
            fade_in: 0.0,
            fade_out: 0.0,
            slide_from: Slide::None,
            slide_dist: default_slide_dist(),
        }
    }
}

impl Title {
    pub fn covers(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }
}

/// Outline thickness as a fraction of the frame height, when enabled.
pub const OUTLINE_FRAC: f32 = 0.006;

/// ASS wants colours as `&HBBGGRR`, which is backwards from every other
/// place a colour is written. Easy to get wrong; done once, here.
/// Where title presets live: one JSON file per preset — community presets
/// are just files you drop in.
pub fn preset_dir() -> std::path::PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        })
        .join("reel/titles")
}

/// Load a preset by NAME (in the preset dir) or by path. The preset is a
/// Title with placeholder text/timing — apply keeps your words and moment.
pub fn load_preset(name: &str) -> anyhow::Result<Title> {
    let path = if name.contains('/') || name.ends_with(".json") {
        std::path::PathBuf::from(name)
    } else {
        preset_dir().join(format!("{name}.json"))
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        anyhow::anyhow!(
            "no title preset {name:?} ({e}) — presets live in {} (`reel title presets` lists them)",
            preset_dir().display()
        )
    })?;
    Ok(serde_json::from_str(&text)?)
}

/// The names available in the preset dir.
pub fn list_presets() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(preset_dir())
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// First-run seeding: a few classic presets so the browser isn't empty.
pub fn seed_presets() {
    let dir = preset_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mk = |name: &str, t: Title| {
        let p = dir.join(format!("{name}.json"));
        if !p.exists() {
            if let Ok(json) = serde_json::to_string_pretty(&t) {
                let _ = std::fs::write(&p, json);
            }
        }
    };
    mk("lower-third", Title {
        x: 0.24, y: 0.86, size: 0.055,
        fade_in: 0.4, fade_out: 0.4,
        slide_from: Slide::Left, slide_dist: 0.1,
        ..Default::default()
    });
    mk("headline", Title {
        x: 0.5, y: 0.4, size: 0.12,
        fade_in: 0.5, fade_out: 0.5,
        slide_from: Slide::Bottom, slide_dist: 0.06,
        ..Default::default()
    });
    mk("caption-fade", Title {
        x: 0.5, y: 0.9, size: 0.05, bold: false,
        fade_in: 0.3, fade_out: 0.3,
        ..Default::default()
    });
}

fn ass_colour([r, g, b]: [u8; 3]) -> String {
    format!("&H00{b:02X}{g:02X}{r:02X}")
}

fn ass_time(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0) as u64;
    let m = ((t % 3600.0) / 60.0) as u64;
    let s = t % 60.0;
    format!("{h}:{m:02}:{:02}.{:02}", s as u64, ((s - s.floor()) * 100.0) as u64)
}

/// Text that would otherwise be read as ASS markup or break the line-based
/// format. Braces open override blocks; a newline ends the event.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('\r', "")
        .replace('\n', "\\N")
}

/// Build an ASS document placing `titles` on a `w`×`h` frame.
///
/// `PlayResX/Y` are set to the render frame, so `\pos()` is in real pixels
/// and every fraction above converts by a single multiply.
pub fn to_ass(titles: &[Title], w: u32, h: u32) -> String {
    let mut out = format!(
        "[Script Info]\n\
         ScriptType: v4.00+\n\
         PlayResX: {w}\n\
         PlayResY: {h}\n\
         WrapStyle: 0\n\
         ScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, \
         BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, \
         BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n"
    );
    for (i, t) in titles.iter().enumerate() {
        let size = (t.size * h as f32).round().max(4.0);
        let outline = if t.outline {
            (OUTLINE_FRAC * h as f32).max(1.0)
        } else {
            0.0
        };
        // Alignment 5 = centred on the \pos point, both axes.
        out.push_str(&format!(
            "Style: T{i},DejaVu Sans,{size},{},&H000000FF,&H00000000,&H00000000,{},0,0,0,\
             100,100,0,0,1,{outline},0,5,0,0,0,1\n",
            ass_colour(t.color),
            if t.bold { -1 } else { 0 },
        ));
    }
    out.push_str(
        "\n[Events]\n\
         Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );
    for (i, t) in titles.iter().enumerate() {
        let (px, py) = ((t.x * w as f32).round(), (t.y * h as f32).round());
        // Motion compiles to native ASS tags: \fad for fades, \move for a
        // slide-in — libass animates them; `animated_at` is the same maths,
        // which is what the preview draws.
        let mut tags = String::new();
        if t.fade_in > 0.0 || t.fade_out > 0.0 {
            tags.push_str(&format!(
                "\\fad({},{})",
                (t.fade_in * 1000.0).round() as u64,
                (t.fade_out * 1000.0).round() as u64
            ));
        }
        let slid = t.slide_from != Slide::None && t.fade_in > 0.0;
        if slid {
            let (dx, dy) = match t.slide_from {
                Slide::Left => (-t.slide_dist, 0.0),
                Slide::Right => (t.slide_dist, 0.0),
                Slide::Top => (0.0, -t.slide_dist),
                Slide::Bottom => (0.0, t.slide_dist),
                Slide::None => (0.0, 0.0),
            };
            let (sx, sy) = (
                ((t.x + dx) * w as f32).round(),
                ((t.y + dy) * h as f32).round(),
            );
            tags.push_str(&format!(
                "\\move({sx},{sy},{px},{py},0,{})",
                (t.fade_in * 1000.0).round() as u64
            ));
        } else {
            tags.push_str(&format!("\\pos({px},{py})"));
        }
        out.push_str(&format!(
            "Dialogue: 0,{},{},T{i},,0,0,0,,{{{tags}}}{}\n",
            ass_time(t.start),
            ass_time(t.end),
            escape(&t.text),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn colours_are_written_the_way_ass_reads_them() {
        // Pure red is &H000000FF — blue-green-red, not red-green-blue.
        assert_eq!(ass_colour([255, 0, 0]), "&H000000FF");
        assert_eq!(ass_colour([0, 0, 255]), "&H00FF0000");
        assert_eq!(ass_colour([255, 255, 255]), "&H00FFFFFF");
    }

    #[test]
    fn markup_in_the_users_text_cannot_break_the_document() {
        let t = Title { text: "{\\b1}50% off\nToday".into(), ..Default::default() };
        let ass = to_ass(&[t], 640, 360);
        let dialogue = ass.lines().last().unwrap();
        // Braces neutralised, newline folded into ASS's own line break, and
        // the event stayed on one line — otherwise the file is corrupt.
        assert!(dialogue.contains("\\{\\\\b1\\}50% off\\NToday"), "got: {dialogue}");
        assert_eq!(ass.lines().filter(|l| l.starts_with("Dialogue:")).count(), 1);
    }

    /// Where the text actually lands, as fractions of the frame:
    /// (centre x, centre y, height).
    fn burned_geometry(title: &Title, w: u32, h: u32) -> (f32, f32, f32) {
        let dir = std::env::temp_dir();
        let ass = dir.join(format!("reel-title-{}-{w}.ass", std::process::id()));
        let png = dir.join(format!("reel-title-{}-{w}.png", std::process::id()));
        std::fs::write(&ass, to_ass(std::slice::from_ref(title), w, h)).unwrap();
        let ok = Command::new("ffmpeg")
            .args([
                "-y", "-v", "error", "-f", "lavfi",
                "-i", &format!("color=c=black:size={w}x{h}:rate=1:duration=1"),
                "-vf", &format!("subtitles='{}'", ass.to_string_lossy()),
                "-frames:v", "1", &png.to_string_lossy(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "ffmpeg could not burn the title");
        let img = image::open(&png).expect("read burned png").to_luma8();
        let _ = std::fs::remove_file(&ass);
        let _ = std::fs::remove_file(&png);

        let (mut top, mut bottom, mut left, mut right) = (u32::MAX, 0u32, u32::MAX, 0u32);
        for (x, y, px) in img.enumerate_pixels() {
            if px.0[0] > 200 {
                top = top.min(y);
                bottom = bottom.max(y);
                left = left.min(x);
                right = right.max(x);
            }
        }
        assert!(bottom > 0, "no title pixels at {w}x{h}");
        (
            (left + right) as f32 / 2.0 / w as f32,
            (top + bottom) as f32 / 2.0 / h as f32,
            (bottom - top + 1) as f32 / h as f32,
        )
    }

    /// The contract: a title composed against the preview lands in the same
    /// place in the render, at any resolution. The preview draws from exactly
    /// the `x`/`y`/`size` fractions below, so measuring a real render against
    /// them is measuring preview-against-render.
    #[test]
    fn a_title_lands_where_it_was_placed_at_any_resolution() {
        let t = Title {
            text: "REEL".into(),
            x: 0.3,
            y: 0.25,
            size: 0.12,
            outline: false,
            ..Default::default()
        };
        let small = burned_geometry(&t, 640, 360);
        let large = burned_geometry(&t, 1920, 1080);

        for (label, g) in [("640x360", small), ("1920x1080", large)] {
            assert!((g.0 - t.x).abs() < 0.02, "{label}: x landed at {:.3}, asked {:.3}", g.0, t.x);
            assert!((g.1 - t.y).abs() < 0.02, "{label}: y landed at {:.3}, asked {:.3}", g.1, t.y);
            // Cap height is a fraction of the em box; this pins the scale.
            assert!(
                g.2 > t.size * 0.45 && g.2 < t.size * 1.15,
                "{label}: height {:.3} inconsistent with size {:.3}",
                g.2, t.size
            );
        }
        assert!(
            (small.0 - large.0).abs() < 0.01
                && (small.1 - large.1).abs() < 0.01
                && (small.2 - large.2).abs() < 0.015,
            "title geometry drifts with resolution: {small:?} vs {large:?}"
        );
    }

    #[test]
    fn a_title_only_shows_inside_its_window() {
        let t = Title { start: 1.0, end: 2.0, ..Default::default() };
        assert!(!t.covers(0.9));
        assert!(t.covers(1.0));
        assert!(t.covers(1.999));
        assert!(!t.covers(2.0), "the end is exclusive, or two titles overlap by a frame");
    }
}

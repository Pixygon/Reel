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
        out.push_str(&format!(
            "Dialogue: 0,{},{},T{i},,0,0,0,,{{\\pos({px},{py})}}{}\n",
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

//! Reuse inside one document: named SLOTS (an episode swaps a capture or a
//! card's text by name, never by index) and aspect VARIANTS (one edit, many
//! frames). A variant is resolved into a plain Project at render time, so
//! every renderer sees ordinary pips and titles and nothing else has to know
//! variants exist.

use super::{Clip, Param, Project, TrackKind};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

/// One extra frame shape for this edit. Layout is DERIVED from the main one
/// at render time; only what's listed here is hand-placed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// letterbox, crop or blur. Default: blur when the aspect changes, since
    /// a blind crop is exactly what variants exist to avoid.
    #[serde(default)]
    pub fit: Option<String>,
    #[serde(default)]
    pub clips: Vec<ClipOverride>,
    #[serde(default)]
    pub titles: Vec<TitleOverride>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipOverride {
    pub clip: u64,
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    #[serde(default)]
    pub scale: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleOverride {
    pub title: usize,
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    #[serde(default)]
    pub size: Option<f32>,
}

/// One slot as `reel slot list` reports it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SlotInfo {
    pub name: String,
    /// "media", "text", or "both" when one name binds a clip and a title.
    pub kind: String,
    /// The current media path or text.
    pub value: String,
    pub clips: Vec<u64>,
    pub titles: Vec<usize>,
}

/// Where text and insets may sit, as frame fractions (left, top, right,
/// bottom). Portrait keeps clear of the platform UI: its caption block along
/// the bottom and its buttons up the right.
pub fn safe_area(w: u32, h: u32) -> [f32; 4] {
    if h > w {
        [0.06, 0.10, 0.90, 0.80]
    } else {
        [0.04, 0.06, 0.96, 0.94]
    }
}

/// How the old frame maps onto the new: the whole old frame scaled to sit
/// inside the new one, centred. Returns the width and height ratios.
fn contain(from: (u32, u32), to: (u32, u32)) -> (f32, f32) {
    let (fw, fh, tw, th) = (from.0 as f32, from.1 as f32, to.0 as f32, to.1 as f32);
    let s = (tw / fw).min(th / fh);
    (fw * s / tw, fh * s / th)
}

fn remap(v: f32, ratio: f32) -> f32 {
    0.5 + (v - 0.5) * ratio
}

/// A generous estimate of a line's width in em. Text measurement proper lives
/// in libass; this only has to err wide enough to keep titles inside.
fn line_ems(line: &str) -> f32 {
    line.chars()
        .map(|c| match c {
            ' ' => 0.32,
            c if c.is_ascii_uppercase() || c.is_ascii_digit() => 0.74,
            c if c.is_ascii() => 0.6,
            _ => 1.1,
        })
        .sum()
}

/// A title's size and position in the new frame: scaled from the main layout,
/// grown if there's room (a portrait frame shrinks everything), then kept
/// inside the safe area. Pure, so "no title cut off" is testable.
pub fn fit_title(text: &str, (x, y, size): (f32, f32, f32), from: (u32, u32), to: (u32, u32)) -> (f32, f32, f32) {
    let (rx, ry) = contain(from, to);
    let safe = safe_area(to.0, to.1);
    let (sw, sh) = (safe[2] - safe[0], safe[3] - safe[1]);
    let frame = to.1 as f32 / to.0 as f32;
    let ems = text.lines().map(line_ems).fold(0.0, f32::max).max(0.5);
    let lines = text.lines().count().max(1) as f32;
    let grown = if ry < rx { 1.6 } else { 1.0 };
    let size = (size * ry * grown).min(sw / (ems * frame)).min(sh / (lines * 1.3));
    let (hw, hh) = (ems * size * frame / 2.0, lines * size * 0.65);
    (
        remap(x, rx).clamp(safe[0] + hw, (safe[2] - hw).max(safe[0] + hw)),
        remap(y, ry).clamp(safe[1] + hh, (safe[3] - hh).max(safe[1] + hh)),
        size,
    )
}

/// The same for an inset. `aspect` is the media's width/height.
pub fn fit_pip((x, y, scale): (f32, f32, f32), aspect: f32, from: (u32, u32), to: (u32, u32)) -> (f32, f32, f32) {
    let (rx, ry) = contain(from, to);
    let safe = safe_area(to.0, to.1);
    let scale = (scale * rx).min(safe[2] - safe[0]);
    let (hw, hh) = (scale / 2.0, scale * to.0 as f32 / to.1 as f32 / aspect.max(0.01) / 2.0);
    (
        remap(x, rx).clamp(safe[0] + hw, (safe[2] - hw).max(safe[0] + hw)),
        remap(y, ry).clamp(safe[1] + hh, (safe[3] - hh).max(safe[1] + hh)),
        scale,
    )
}

impl Project {
    fn clips_iter_mut(&mut self) -> impl Iterator<Item = &mut Clip> {
        self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut())
    }

    /// Every slot in the document, sorted by name.
    pub fn slots(&self) -> Vec<SlotInfo> {
        let mut out: Vec<SlotInfo> = Vec::new();
        fn entry<'a>(out: &'a mut Vec<SlotInfo>, name: &str, kind: &str, value: &str) -> &'a mut SlotInfo {
            let at = match out.iter().position(|s| s.name == name) {
                Some(i) => i,
                None => {
                    out.push(SlotInfo {
                        name: name.into(),
                        kind: kind.into(),
                        value: value.into(),
                        clips: Vec::new(),
                        titles: Vec::new(),
                    });
                    out.len() - 1
                }
            };
            if out[at].kind != kind {
                out[at].kind = "both".into();
            }
            &mut out[at]
        }
        for c in self.tracks.iter().flat_map(|t| t.clips.iter()) {
            if let Some(n) = &c.slot {
                entry(&mut out, n, "media", &c.source).clips.push(c.id);
            }
        }
        for (i, t) in self.titles.iter().enumerate() {
            if let Some(n) = &t.slot {
                entry(&mut out, n, "text", &t.text).titles.push(i);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn known_slots(&self) -> String {
        let names: Vec<String> = self.slots().into_iter().map(|s| s.name).collect();
        if names.is_empty() { "none yet — bind one with `reel slot bind`".into() } else { names.join(", ") }
    }

    /// Point every clip in slot `name` at new media. The clip keeps its slot
    /// on the timeline; `len` (None for a still or plate) shortens it only if
    /// the new media can't fill it. Returns (clips changed, clips shortened).
    pub fn set_slot_media(
        &mut self,
        name: &str,
        source: &str,
        nested: Option<String>,
        plate: Option<String>,
        len: Option<f64>,
    ) -> Result<(usize, usize)> {
        let label = plate.clone().unwrap_or_else(|| {
            let from = nested.as_deref().unwrap_or(source);
            std::path::Path::new(from)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| source.to_string())
        });
        let (mut changed, mut shortened) = (0, 0);
        for c in self.clips_iter_mut().filter(|c| c.slot.as_deref() == Some(name)) {
            c.source = source.into();
            c.name = label.clone();
            c.nested = nested.clone();
            c.plate = plate.clone();
            c.in_point = 0.0;
            if let Some(len) = len {
                if c.source_len() > len + 1e-3 {
                    c.duration = len / c.speed.max(0.01) as f64;
                    shortened += 1;
                }
            }
            changed += 1;
        }
        if changed == 0 {
            bail!("no media slot named {name:?}. Slots: {}", self.known_slots());
        }
        Ok((changed, shortened))
    }

    /// Set the words of every title in slot `name`.
    pub fn set_slot_text(&mut self, name: &str, text: &str) -> Result<usize> {
        let mut changed = 0;
        for t in self.titles.iter_mut().filter(|t| t.slot.as_deref() == Some(name)) {
            t.text = text.into();
            changed += 1;
        }
        if changed == 0 {
            bail!("no text slot named {name:?}. Slots: {}", self.known_slots());
        }
        Ok(changed)
    }

    pub fn variant_mut(&mut self, name: &str) -> Result<&mut Variant> {
        let known: Vec<String> = self.variants.iter().map(|v| v.name.clone()).collect();
        self.variants
            .iter_mut()
            .find(|v| v.name == name)
            .ok_or_else(|| anyhow!("no variant named {name:?}. Variants: {}", known.join(", ")))
    }

    /// Keep title overrides pointing at the right title after one is removed.
    pub fn title_removed(&mut self, index: usize) {
        for v in &mut self.variants {
            v.titles.retain(|o| o.title != index);
            for o in &mut v.titles {
                if o.title > index {
                    o.title -= 1;
                }
            }
        }
    }

    /// This edit as the named variant: a plain Project at the variant's size,
    /// with every inset, title and plate laid out for it. `aspect_of` answers
    /// a source's width/height. Overrides win over the derived layout.
    pub fn resolve_variant(&self, name: &str, aspect_of: impl Fn(&str) -> f32) -> Result<Project> {
        let v = self
            .variants
            .iter()
            .find(|v| v.name == name)
            .ok_or_else(|| anyhow!("no variant named {name:?}"))?;
        let (from, to) = ((self.width, self.height), (v.width, v.height));
        let mut out = self.clone();
        out.width = v.width;
        out.height = v.height;
        let (rx, ry) = contain(from, to);
        for c in out.tracks.iter_mut().flat_map(|t| {
            let base = t.kind == TrackKind::Video;
            t.clips.iter_mut().map(move |c| (base, c))
        }) {
            let (base, c) = c;
            // A base plate must fill the new frame; an overlay plate keeps
            // the shape it was placed with.
            if let (true, Some(spec)) = (base, &c.plate) {
                c.source = crate::plates::ensure(spec, v.width, v.height)?;
            }
            let (x, y, scale) = fit_pip((c.pip.x, c.pip.y, c.pip.scale), aspect_of(&c.source), from, to);
            c.pip.x = x;
            c.pip.y = y;
            c.pip.scale = scale;
            for (p, keys) in &mut c.keys {
                for k in keys.iter_mut() {
                    k.value = match p {
                        Param::PipX => remap(k.value, rx),
                        Param::PipY => remap(k.value, ry),
                        Param::PipScale => k.value * rx,
                        _ => k.value,
                    };
                }
            }
            if let Some(o) = v.clips.iter().find(|o| o.clip == c.id) {
                let mut hand = Vec::new();
                if let Some(x) = o.x {
                    c.pip.x = x;
                    hand.push(Param::PipX);
                }
                if let Some(y) = o.y {
                    c.pip.y = y;
                    hand.push(Param::PipY);
                }
                if let Some(s) = o.scale {
                    c.pip.scale = s;
                    hand.push(Param::PipScale);
                }
                c.keys.retain(|(p, _)| !hand.contains(p));
            }
        }
        for (i, t) in out.titles.iter_mut().enumerate() {
            let (x, y, size) = fit_title(&t.text, (t.x, t.y, t.size), from, to);
            t.x = x;
            t.y = y;
            t.size = size;
            if let Some(o) = v.titles.iter().find(|o| o.title == i) {
                t.x = o.x.unwrap_or(t.x);
                t.y = o.y.unwrap_or(t.y);
                t.size = o.size.unwrap_or(t.size);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: (u32, u32) = (1920, 1080);
    const TALL: (u32, u32) = (1080, 1920);

    #[test]
    fn a_long_title_stays_inside_the_portrait_safe_area() {
        let text = "THE OFFICE OF DYSON AND THE LANTERN";
        let (x, y, size) = fit_title(text, (0.5, 0.9, 0.09), WIDE, TALL);
        let safe = safe_area(TALL.0, TALL.1);
        let half = line_ems(text) * size * (1920.0 / 1080.0) / 2.0;
        assert!(x - half >= safe[0] - 1e-3 && x + half <= safe[2] + 1e-3, "x {x} ± {half}");
        assert!(y <= safe[3], "y {y}");
    }

    #[test]
    fn the_same_frame_shape_changes_nothing() {
        let (x, y, size) = fit_title("Hi", (0.3, 0.4, 0.08), WIDE, (3840, 2160));
        assert!((x - 0.3).abs() < 1e-5 && (y - 0.4).abs() < 1e-5 && (size - 0.08).abs() < 1e-5);
    }

    #[test]
    fn an_inset_is_kept_on_the_frame() {
        let (x, y, scale) = fit_pip((0.95, 0.95, 0.4), 16.0 / 9.0, WIDE, TALL);
        let safe = safe_area(TALL.0, TALL.1);
        assert!(x + scale / 2.0 <= safe[2] + 1e-3);
        assert!(y <= safe[3]);
    }

    #[test]
    fn overrides_win_and_a_slot_swap_reaches_every_binding() {
        let mut p = Project { width: 1920, height: 1080, ..Default::default() };
        let id = p.add_clip("a.png", crate::edit::TrackKind::Overlay, 0.0, 0.0, 2.0);
        p.clip_mut(id).unwrap().slot = Some("cam".into());
        p.titles.push(crate::titles::Title {
            text: "old".into(),
            slot: Some("card".into()),
            ..Default::default()
        });
        p.variants.push(Variant {
            name: "9x16".into(),
            width: 1080,
            height: 1920,
            clips: vec![ClipOverride { clip: id, x: Some(0.2), y: None, scale: None }],
            titles: vec![TitleOverride { title: 0, x: None, y: Some(0.3), size: None }],
            ..Default::default()
        });
        assert_eq!(p.slots().len(), 2);
        p.set_slot_text("card", "new").unwrap();
        p.set_slot_media("cam", "b.png", None, None, None).unwrap();
        assert!(p.set_slot_text("nope", "x").is_err());
        let v = p.resolve_variant("9x16", |_| 1.0).unwrap();
        assert_eq!((v.width, v.height), (1080, 1920));
        assert_eq!(v.titles[0].text, "new");
        assert!((v.titles[0].y - 0.3).abs() < 1e-5);
        let c = v.clip(id).unwrap();
        assert_eq!(c.source, "b.png");
        assert!((c.pip.x - 0.2).abs() < 1e-5);
    }
}

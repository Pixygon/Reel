//! Selections and local adjustments — Photoshop-style "edit only this part".
//!
//! A selection is a [`Plane`]: one byte of coverage per pixel at the picture's
//! own resolution, never a box or a path. Every tool (lasso, rectangle,
//! ellipse, brush, colour range) produces a plane, and every operation
//! (add/subtract/intersect, feather, expand/contract, invert) is a pure
//! function over planes — that is what makes them testable on real pixels.
//!
//! A committed selection becomes a [`LocalMask`]: a named, hideable matte plus
//! a full `Effects` applied inside it. The GPU side is one code path
//! (`engine::compositor::LocalChain`): the matte rewrites the SOURCE frame
//! before the clip's own grade, and both the preview and the frame server run
//! that same chain, so a masked edit cannot look different between them.
//!
//! The same matte on a video clip is a static matte: it applies to every
//! frame, because the chain runs on every frame.

use crate::effects::{Effects, Hsl};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Coverage per pixel, row-major: 0 = outside, 255 = fully selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plane {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

/// How a new selection joins the current one (none/Shift/Alt/Shift+Alt).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Replace,
    Add,
    Subtract,
    Intersect,
}

impl Plane {
    pub fn empty(w: u32, h: u32) -> Self {
        Self { w, h, data: vec![0; (w * h) as usize] }
    }

    pub fn is_empty(&self) -> bool {
        self.data.iter().all(|&v| v == 0)
    }

    /// Everything inside a closed polygon (pixel coordinates), even-odd, at
    /// pixel centres. Lasso, polygon lasso and the CLI all land here.
    pub fn polygon(w: u32, h: u32, pts: &[[f32; 2]]) -> Self {
        let mut out = Self::empty(w, h);
        if pts.len() < 3 {
            return out;
        }
        let mut xs: Vec<f32> = Vec::new();
        for y in 0..h {
            let sy = y as f32 + 0.5;
            xs.clear();
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                if (a[1] <= sy) != (b[1] <= sy) {
                    xs.push(a[0] + (sy - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                }
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.chunks_exact(2) {
                let from = (pair[0] - 0.5).ceil().max(0.0) as u32;
                let to = ((pair[1] - 0.5).ceil().max(0.0) as u32).min(w);
                let row = (y * w) as usize;
                if from < to {
                    out.data[row + from as usize..row + to as usize].fill(255);
                }
            }
        }
        out
    }

    /// An axis-aligned box between two corners, in pixels.
    pub fn rect(w: u32, h: u32, a: [f32; 2], b: [f32; 2]) -> Self {
        Self::polygon(w, h, &[a, [b[0], a[1]], b, [a[0], b[1]]])
    }

    /// The ellipse inscribed in the box between two corners, in pixels.
    pub fn ellipse(w: u32, h: u32, a: [f32; 2], b: [f32; 2]) -> Self {
        let mut out = Self::empty(w, h);
        let (cx, cy) = ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0);
        let (rx, ry) = (((b[0] - a[0]) / 2.0).abs(), ((b[1] - a[1]) / 2.0).abs());
        if rx <= 0.0 || ry <= 0.0 {
            return out;
        }
        for y in 0..h {
            for x in 0..w {
                let n = [(x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry];
                if n[0] * n[0] + n[1] * n[1] <= 1.0 {
                    out.data[(y * w + x) as usize] = 255;
                }
            }
        }
        out
    }

    /// One brush stroke: round stamps along the path, with a one-pixel soft
    /// rim so a painted edge isn't jagged.
    pub fn stroke(w: u32, h: u32, pts: &[[f32; 2]], radius: f32) -> Self {
        let mut out = Self::empty(w, h);
        let segs: Vec<([f32; 2], [f32; 2])> = match pts {
            [] => return out,
            [p] => vec![(*p, *p)],
            _ => pts.windows(2).map(|s| (s[0], s[1])).collect(),
        };
        let r = radius.max(0.5);
        for (a, b) in segs {
            let x0 = (a[0].min(b[0]) - r - 1.0).floor().max(0.0) as u32;
            let y0 = (a[1].min(b[1]) - r - 1.0).floor().max(0.0) as u32;
            let x1 = ((a[0].max(b[0]) + r + 1.0).ceil().max(0.0) as u32).min(w);
            let y1 = ((a[1].max(b[1]) + r + 1.0).ceil().max(0.0) as u32).min(h);
            let d = [b[0] - a[0], b[1] - a[1]];
            let len2 = d[0] * d[0] + d[1] * d[1];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = [x as f32 + 0.5 - a[0], y as f32 + 0.5 - a[1]];
                    let t = if len2 > 0.0 {
                        ((p[0] * d[0] + p[1] * d[1]) / len2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let e = [p[0] - d[0] * t, p[1] - d[1] * t];
                    let dist = (e[0] * e[0] + e[1] * e[1]).sqrt();
                    let cov = ((r + 0.5 - dist).clamp(0.0, 1.0) * 255.0) as u8;
                    let v = &mut out.data[(y * w + x) as usize];
                    *v = (*v).max(cov);
                }
            }
        }
        out
    }

    /// Colour-range pick — the magic wand, built on the HSL qualifier so the
    /// selection and the qualifier mean the same thing by "similar colour".
    /// `tolerance` 0..1 widens all three windows; `contiguous` keeps only the
    /// region connected to the clicked pixel. `rgba` is straight RGBA.
    pub fn colour_range(
        rgba: &[u8],
        w: u32,
        h: u32,
        seed: (u32, u32),
        tolerance: f32,
        contiguous: bool,
    ) -> Self {
        let mut out = Self::empty(w, h);
        if seed.0 >= w || seed.1 >= h || rgba.len() < (w * h * 4) as usize {
            return out;
        }
        let hsl_of = |i: usize| {
            let px = &rgba[i * 4..i * 4 + 3];
            crate::effects::rgb_to_hsl([
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ])
        };
        let s = hsl_of((seed.1 * w + seed.0) as usize);
        let t = tolerance.clamp(0.0, 1.0);
        let q = Hsl {
            hue: s[0],
            // A grey has no meaningful hue: lightness decides alone.
            hue_width: if s[1] < 0.08 { 180.0 } else { 2.0 + t * 90.0 },
            sat_min: s[1] - t * 0.6,
            sat_max: s[1] + t * 0.6,
            lum_min: s[2] - t * 0.5,
            lum_max: s[2] + t * 0.5,
            soft: 0.02,
            ..Hsl::default()
        };
        let weight = |i: usize| (q.weight(hsl_of(i)) * 255.0) as u8;
        if !contiguous {
            for i in 0..out.data.len() {
                out.data[i] = weight(i);
            }
            return out;
        }
        let mut seen = vec![false; out.data.len()];
        let start = (seed.1 * w + seed.0) as usize;
        seen[start] = true;
        let mut todo = vec![start];
        while let Some(i) = todo.pop() {
            let v = weight(i);
            if v == 0 {
                continue;
            }
            out.data[i] = v;
            let (x, y) = (i as u32 % w, i as u32 / w);
            let mut go = |n: usize| {
                if !seen[n] {
                    seen[n] = true;
                    todo.push(n);
                }
            };
            if x > 0 { go(i - 1); }
            if x + 1 < w { go(i + 1); }
            if y > 0 { go(i - w as usize); }
            if y + 1 < h { go(i + w as usize); }
        }
        out
    }

    /// Join `other` into this selection. Sizes must match; a mismatched plane
    /// (never produced by the tools) is resampled rather than panicking.
    pub fn combine(&mut self, other: &Plane, op: Combine) {
        let resized;
        let other = if (other.w, other.h) == (self.w, self.h) {
            other
        } else {
            resized = other.resized(self.w, self.h);
            &resized
        };
        for (a, &b) in self.data.iter_mut().zip(&other.data) {
            *a = match op {
                Combine::Replace => b,
                Combine::Add => (*a).max(b),
                Combine::Subtract => (*a).min(255 - b),
                Combine::Intersect => (*a).min(b),
            };
        }
    }

    pub fn invert(&mut self) {
        self.data.iter_mut().for_each(|v| *v = 255 - *v);
    }

    /// Soft edge: three box blurs (a Gaussian in all but name, and O(pixels)
    /// whatever the radius — a feather on a 24 MP photo must stay instant).
    pub fn feather(&mut self, radius: f32) {
        let r = (radius.max(0.0) / 1.5).round() as usize;
        if r == 0 {
            return;
        }
        let (w, h) = (self.w as usize, self.h as usize);
        for _ in 0..3 {
            box_blur(&mut self.data, w, h, r, true);
            box_blur(&mut self.data, w, h, r, false);
        }
    }

    /// Grow (`px` > 0) or shrink (`px` < 0) the selection along its edge,
    /// round corners included: a true Euclidean distance from the edge.
    /// Works on the hard edge — feather after this, not before.
    pub fn expand(&mut self, px: f32) {
        if px == 0.0 {
            return;
        }
        let (w, h) = (self.w as usize, self.h as usize);
        let inside: Vec<bool> = self.data.iter().map(|&v| v >= 128).collect();
        // Distance to the side we grow from.
        let source = |i: usize| if px > 0.0 { inside[i] } else { !inside[i] };
        let d = distance_field(w, h, source);
        let r = px.abs();
        for i in 0..self.data.len() {
            let cov = (r + 0.5 - d[i]).clamp(0.0, 1.0);
            self.data[i] = if px > 0.0 {
                (cov * 255.0) as u8
            } else {
                ((1.0 - cov) * 255.0) as u8
            };
        }
    }

    /// Bilinear resample — PNG mattes conform to the source they land on.
    pub fn resized(&self, w: u32, h: u32) -> Plane {
        let img = image::GrayImage::from_raw(self.w, self.h, self.data.clone())
            .unwrap_or_else(|| image::GrayImage::new(self.w, self.h));
        let out = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
        Plane { w, h, data: out.into_raw() }
    }

    /// Coverage as an 8-bit greyscale PNG — the on-disk form of a mask.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut out,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Sub,
        )
        .write_image(&self.data, self.w, self.h, image::ExtendedColorType::L8)?;
        Ok(out)
    }

    /// Read a matte: white = selected. A PNG with real transparency counts
    /// as cut out where it is transparent (what a design tool exports).
    pub fn from_image_bytes(bytes: &[u8]) -> Result<Plane> {
        let img = image::load_from_memory(bytes)?;
        let (w, h) = (img.width(), img.height());
        let la = img.to_luma_alpha8();
        let data = la.pixels().map(|p| (p.0[0] as u32 * p.0[1] as u32 / 255) as u8).collect();
        Ok(Plane { w, h, data })
    }
}

/// One direction of a running-sum box blur, edges clamped.
fn box_blur(data: &mut [u8], w: usize, h: usize, r: usize, horizontal: bool) {
    let (lines, len, step, stride) = if horizontal { (h, w, 1, w) } else { (w, h, w, 1) };
    let span = 2 * r as u32 + 1;
    let mut line = vec![0u8; len];
    for l in 0..lines {
        let base = l * stride;
        for (i, v) in line.iter_mut().enumerate() {
            *v = data[base + i * step];
        }
        let at = |i: isize| line[i.clamp(0, len as isize - 1) as usize] as u32;
        let mut sum: u32 = (-(r as isize)..=r as isize).map(at).sum();
        for i in 0..len {
            data[base + i * step] = ((sum + span / 2) / span) as u8;
            sum += at(i as isize + r as isize + 1);
            sum -= at(i as isize - r as isize);
        }
    }
}

/// Exact Euclidean distance to the nearest `source` pixel
/// (Felzenszwalb & Huttenlocher's two-pass parabola envelope).
fn distance_field(w: usize, h: usize, source: impl Fn(usize) -> bool) -> Vec<f32> {
    const FAR: f32 = 1e12;
    let mut g: Vec<f32> = (0..w * h).map(|i| if source(i) { 0.0 } else { FAR }).collect();
    let mut f = vec![0.0f32; w.max(h)];
    let mut pass = |g: &mut [f32], lines: usize, len: usize, step: usize, stride: usize| {
        let mut v = vec![0usize; len];
        let mut z = vec![0.0f32; len + 1];
        for l in 0..lines {
            for (i, s) in f.iter_mut().enumerate().take(len) {
                *s = g[l * stride + i * step];
            }
            let mut k = 0usize;
            v[0] = 0;
            z[0] = f32::NEG_INFINITY;
            z[1] = f32::INFINITY;
            for q in 1..len {
                let mut s;
                loop {
                    let p = v[k];
                    s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32))
                        / (2.0 * (q as f32 - p as f32));
                    if s <= z[k] && k > 0 {
                        k -= 1;
                    } else {
                        break;
                    }
                }
                k += 1;
                v[k] = q;
                z[k] = s;
                z[k + 1] = f32::INFINITY;
            }
            k = 0;
            for q in 0..len {
                while z[k + 1] < q as f32 {
                    k += 1;
                }
                let p = v[k];
                let d = q as f32 - p as f32;
                g[l * stride + q * step] = d * d + f[p];
            }
        }
    };
    pass(&mut g, h, w, 1, w);
    pass(&mut g, w, h, w, 1);
    g.iter().map(|v| v.sqrt()).collect()
}

/// Where a mask lives in a frame: the picture's own window inside the frame
/// (letterbox bars / crop overflow) plus the clip's quarter turns.
pub fn unrotate_uv(p: [f32; 2], turns: u8) -> [f32; 2] {
    match turns % 4 {
        1 => [p[1], 1.0 - p[0]],
        2 => [1.0 - p[0], 1.0 - p[1]],
        3 => [1.0 - p[1], p[0]],
        _ => p,
    }
}

/// A plane stored once and shared: undo snapshots, segments and the renderer
/// all hold the same bytes. It serialises as a base64 PNG inside the project,
/// encoded when the mask was made, so autosave never compresses on the UI
/// thread.
#[derive(Clone, Debug)]
pub struct Matte {
    pub plane: Arc<Plane>,
    png: Arc<String>,
    /// Process-unique: GPU caches key on it instead of hashing pixels.
    pub uid: u64,
}

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

impl Matte {
    pub fn new(plane: Plane) -> Result<Self> {
        let png = b64_encode(&plane.to_png()?);
        Ok(Self {
            plane: Arc::new(plane),
            png: Arc::new(png),
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct MatteOnDisk {
    w: u32,
    h: u32,
    /// Greyscale PNG, base64.
    png: String,
}

impl Serialize for Matte {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        MatteOnDisk { w: self.plane.w, h: self.plane.h, png: (*self.png).clone() }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Matte {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let disk = MatteOnDisk::deserialize(d)?;
        let plane = b64_decode(&disk.png)
            .and_then(|b| Plane::from_image_bytes(&b))
            .map_err(D::Error::custom)?;
        Ok(Self {
            plane: Arc::new(plane),
            png: Arc::new(disk.png),
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
        })
    }
}

/// One named mask with the adjustment it carries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalMask {
    pub name: String,
    /// A hidden mask stays in the project but changes no pixels.
    #[serde(default = "yes")]
    pub visible: bool,
    /// Adjust-panel settings applied inside the matte, on top of the frame.
    #[serde(default)]
    pub adjust: Effects,
    pub matte: Matte,
}

fn yes() -> bool {
    true
}

impl LocalMask {
    pub fn new(name: &str, plane: Plane) -> Result<Self> {
        Ok(Self {
            name: name.to_string(),
            visible: true,
            adjust: Effects::default(),
            matte: Matte::new(plane)?,
        })
    }

    /// What the pass actually applies: the colour work only. Fades, reframe,
    /// keying and rotation belong to the clip, not to a part of its picture.
    pub fn colour_adjust(&self) -> Effects {
        let mut fx = Effects::default();
        fx.copy_grade_from(&self.adjust);
        fx.mask = self.adjust.mask;
        fx.plugin = self.adjust.plugin;
        fx.plugin_params = self.adjust.plugin_params;
        fx
    }
}

/// Which selection tool the pointer drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Lasso,
    Polygon,
    Rectangle,
    Ellipse,
    Brush,
    Colour,
}

impl Tool {
    pub const ALL: [Tool; 6] =
        [Tool::Lasso, Tool::Polygon, Tool::Rectangle, Tool::Ellipse, Tool::Brush, Tool::Colour];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Lasso => "Lasso",
            Tool::Polygon => "Polygon",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Brush => "Brush",
            Tool::Colour => "Colour",
        }
    }
}

/// The selection being built on the picture. It lives outside the project —
/// nothing is saved until it becomes a [`LocalMask`] — so it never marks the
/// project dirty and a half-drawn lasso cannot litter a .reel file.
pub struct Selection {
    /// `None` = the pointer is the normal viewport pointer.
    pub tool: Option<Tool>,
    pub plane: Option<Plane>,
    /// The clip this selection was drawn on.
    pub clip: Option<u64>,
    /// The picture's size in pixels; the plane is always this big.
    pub size: (u32, u32),
    pub brush_radius: f32,
    /// The brush takes away instead of painting. Alt does the same, but a
    /// window manager may own Alt+drag.
    pub brush_remove: bool,
    pub tolerance: f32,
    pub contiguous: bool,
    pub feather: f32,
    pub grow: f32,
    pub show_overlay: bool,
    /// Points of the gesture in progress, in picture pixels.
    pub path: Vec<[f32; 2]>,
    /// Bumped on every change, so the overlay texture rebuilds only then.
    pub revision: u64,
    pub overlay: Option<(u64, egui::TextureHandle)>,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            tool: None,
            plane: None,
            clip: None,
            size: (0, 0),
            brush_radius: 40.0,
            brush_remove: false,
            tolerance: 0.2,
            contiguous: true,
            feather: 8.0,
            grow: 4.0,
            show_overlay: true,
            path: Vec::new(),
            revision: 0,
            overlay: None,
        }
    }
}

impl Selection {
    /// How a gesture joins the selection: the modifiers, then the brush's
    /// Add/Remove switch.
    pub fn combine(&self, shift: bool, alt: bool, tool: Tool) -> Combine {
        match combine_for(shift, alt, tool) {
            Combine::Add if tool == Tool::Brush && !shift && self.brush_remove => Combine::Subtract,
            op => op,
        }
    }

    /// Point the tools at a picture; a different clip or size starts fresh.
    pub fn target(&mut self, clip: u64, size: (u32, u32)) {
        if self.clip != Some(clip) || self.size != size {
            self.clip = Some(clip);
            self.size = size;
            self.plane = None;
            self.path.clear();
            self.revision += 1;
        }
    }

    /// Join a finished shape into the selection. With nothing selected yet,
    /// Subtract and Intersect have nothing to act on, and stay empty.
    pub fn apply(&mut self, shape: Plane, op: Combine) {
        match (&mut self.plane, op) {
            (Some(p), _) => p.combine(&shape, op),
            (slot, Combine::Replace | Combine::Add) => *slot = Some(shape),
            _ => {}
        }
        self.path.clear();
        self.revision += 1;
    }

    /// Finish the gesture in progress with the current tool.
    pub fn commit(&mut self, op: Combine) {
        let (w, h) = self.size;
        let Some(tool) = self.tool else { return };
        let shape = match (tool, self.path.as_slice()) {
            (Tool::Lasso | Tool::Polygon, p) => Plane::polygon(w, h, p),
            (Tool::Rectangle, [a, .., b]) => Plane::rect(w, h, *a, *b),
            (Tool::Ellipse, [a, .., b]) => Plane::ellipse(w, h, *a, *b),
            (Tool::Brush, p) if !p.is_empty() => Plane::stroke(w, h, p, self.brush_radius),
            _ => {
                self.path.clear();
                return;
            }
        };
        self.apply(shape, op);
    }

    /// Change the selection in place (feather, grow, invert).
    pub fn edit(&mut self, f: impl FnOnce(&mut Plane)) {
        if let Some(p) = &mut self.plane {
            f(p);
            self.revision += 1;
        }
    }

    pub fn clear(&mut self) {
        self.plane = None;
        self.path.clear();
        self.revision += 1;
    }
}

/// Modifier keys → how a gesture joins the selection (Photoshop's rule).
pub fn combine_for(shift: bool, alt: bool, tool: Tool) -> Combine {
    match (shift, alt) {
        (true, true) => Combine::Intersect,
        (true, false) => Combine::Add,
        (false, true) => Combine::Subtract,
        // A brush paints onto what is there; everything else starts over.
        (false, false) if tool == Tool::Brush => Combine::Add,
        (false, false) => Combine::Replace,
    }
}

/// The on-screen picture's uv → picture pixels: undoes the clip's reframe,
/// flips and quarter turns (the same order the shaders apply them).
pub fn view_to_source(uv: [f32; 2], fx: Option<&Effects>, turns: u8) -> [f32; 2] {
    let mut p = uv;
    if let Some(e) = fx {
        let z = e.zoom.max(1.0);
        p = [
            (uv[0] - 0.5) / z + 0.5 + e.pan_x * (1.0 - 1.0 / z) * 0.5,
            (uv[1] - 0.5) / z + 0.5 + e.pan_y * (1.0 - 1.0 / z) * 0.5,
        ];
        if e.flip_h {
            p[0] = 1.0 - p[0];
        }
        if e.flip_v {
            p[1] = 1.0 - p[1];
        }
    }
    unrotate_uv(p, turns)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

fn b64_decode(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in text.bytes().filter(|&b| b != b'=') {
        let v = B64.iter().position(|&c| c == b).ok_or_else(|| anyhow!("bad base64 in mask"))?;
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: &Plane, x: u32, y: u32) -> u8 {
        p.data[(y * p.w + x) as usize]
    }

    fn count(p: &Plane) -> usize {
        p.data.iter().filter(|&&v| v >= 128).count()
    }

    #[test]
    fn a_lasso_selects_its_inside_only() {
        // A concave L: the notch stays out, which no box path could do.
        let p = Plane::polygon(
            20,
            20,
            &[[2.0, 2.0], [18.0, 2.0], [18.0, 8.0], [8.0, 8.0], [8.0, 18.0], [2.0, 18.0]],
        );
        assert_eq!(at(&p, 4, 15), 255);
        assert_eq!(at(&p, 15, 4), 255);
        assert_eq!(at(&p, 15, 15), 0, "the notch of the L");
        assert_eq!(count(&p), 16 * 6 + 6 * 10);
    }

    #[test]
    fn rectangle_and_ellipse_have_the_right_area() {
        let r = Plane::rect(40, 40, [30.0, 30.0], [10.0, 20.0]);
        assert_eq!(count(&r), 20 * 10, "corners in either order");
        let e = Plane::ellipse(100, 100, [10.0, 20.0], [90.0, 80.0]);
        let want = std::f32::consts::PI * 40.0 * 30.0;
        assert!((count(&e) as f32 - want).abs() < want * 0.01);
        assert_eq!(at(&e, 50, 50), 255);
        assert_eq!(at(&e, 12, 22), 0, "the box corner is outside");
    }

    #[test]
    fn the_brush_paints_then_removes() {
        let mut p = Plane::empty(60, 30);
        p.combine(&Plane::stroke(60, 30, &[[10.0, 15.0], [50.0, 15.0]], 5.0), Combine::Add);
        assert_eq!(at(&p, 30, 15), 255);
        assert_eq!(at(&p, 30, 25), 0);
        p.combine(&Plane::stroke(60, 30, &[[30.0, 0.0], [30.0, 30.0]], 3.0), Combine::Subtract);
        assert_eq!(at(&p, 30, 15), 0, "erased across the stroke");
        assert_eq!(at(&p, 15, 15), 255, "the rest survives");
    }

    fn two_blobs() -> Vec<u8> {
        // Red on blue with two separate red blobs.
        let mut px = vec![0u8; 40 * 20 * 4];
        for y in 0..20 {
            for x in 0..40 {
                let red = (2..10).contains(&x) || (30..38).contains(&x);
                let c: [u8; 4] = if red { [220, 30, 30, 255] } else { [30, 40, 220, 255] };
                px[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4].copy_from_slice(&c);
            }
        }
        px
    }

    #[test]
    fn colour_range_picks_by_colour_and_contiguity() {
        let px = two_blobs();
        let all = Plane::colour_range(&px, 40, 20, (5, 5), 0.1, false);
        assert_eq!(at(&all, 5, 5), 255);
        assert_eq!(at(&all, 33, 5), 255, "same colour, other blob");
        assert_eq!(at(&all, 20, 5), 0, "blue stays out");
        let wand = Plane::colour_range(&px, 40, 20, (5, 5), 0.1, true);
        assert_eq!(at(&wand, 5, 5), 255);
        assert_eq!(at(&wand, 33, 5), 0, "contiguous stops at the gap");
    }

    #[test]
    fn the_tolerance_slider_widens_the_pick() {
        let mut px = vec![0u8; 10 * 4];
        // A ramp of blues, darker to the right.
        for x in 0..10usize {
            let b = 240 - x as u8 * 16;
            px[x * 4..x * 4 + 4].copy_from_slice(&[10, 10, b, 255]);
        }
        let tight = Plane::colour_range(&px, 10, 1, (0, 0), 0.05, false);
        let wide = Plane::colour_range(&px, 10, 1, (0, 0), 0.6, false);
        assert!(count(&wide) > count(&tight), "{} vs {}", count(&wide), count(&tight));
    }

    #[test]
    fn add_subtract_intersect_work_on_pixels() {
        let a = Plane::rect(10, 1, [0.0, 0.0], [6.0, 1.0]);
        let b = Plane::rect(10, 1, [4.0, 0.0], [10.0, 1.0]);
        let run = |op| {
            let mut m = a.clone();
            m.combine(&b, op);
            count(&m)
        };
        assert_eq!(run(Combine::Add), 10);
        assert_eq!(run(Combine::Subtract), 4);
        assert_eq!(run(Combine::Intersect), 2);
        assert_eq!(run(Combine::Replace), 6);
    }

    #[test]
    fn invert_flips_every_pixel_and_twice_is_identity() {
        let mut m = Plane::rect(10, 10, [2.0, 2.0], [5.0, 5.0]);
        let orig = m.clone();
        m.invert();
        assert_eq!(count(&m), 100 - 9);
        m.invert();
        assert_eq!(m, orig);
    }

    #[test]
    fn feather_makes_a_monotonic_edge_and_keeps_the_middle() {
        let mut m = Plane::rect(100, 100, [30.0, 30.0], [70.0, 70.0]);
        m.feather(9.0);
        let row: Vec<u8> = (0..50).map(|x| at(&m, x, 50)).collect();
        assert_eq!(row[0], 0);
        assert_eq!(row[49], 255, "the middle stays fully selected");
        assert!(row.windows(2).all(|w| w[0] <= w[1]), "monotonic: {row:?}");
        assert!((100..160).contains(&(at(&m, 30, 50) as i32)), "half way at the old edge");
        assert!(row.iter().filter(|&&v| v > 8 && v < 247).count() >= 8, "a real ramp");
    }

    #[test]
    fn expand_and_contract_move_the_edge_by_the_right_distance() {
        let base = Plane::rect(60, 60, [20.0, 20.0], [40.0, 40.0]);
        let mut grown = base.clone();
        grown.expand(5.0);
        assert_eq!(at(&grown, 16, 30), 255);
        assert_eq!(at(&grown, 13, 30), 0);
        // Round corner: the box corner's diagonal grows by 5, not 7.
        assert_eq!(at(&grown, 16, 16), 0);
        let mut shrunk = base.clone();
        shrunk.expand(-5.0);
        assert_eq!(at(&shrunk, 26, 30), 255);
        assert_eq!(at(&shrunk, 23, 30), 0);
        assert_eq!(at(&shrunk, 30, 30), 255);
    }

    #[test]
    fn a_matte_survives_the_project_file() {
        let mut p = Plane::ellipse(64, 48, [4.0, 4.0], [60.0, 44.0]);
        p.feather(6.0);
        let mut m = LocalMask::new("Sky", p.clone()).unwrap();
        m.visible = false;
        m.adjust.exposure = 1.4;
        let json = serde_json::to_string(&m).unwrap();
        let back: LocalMask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Sky");
        assert!(!back.visible);
        assert_eq!(back.adjust.exposure, 1.4);
        assert_eq!(*back.matte.plane, p, "every pixel, losslessly");
    }

    #[test]
    fn several_masks_come_back_when_the_project_reopens() {
        let path = std::env::temp_dir().join(format!("reel-masks-{}.reel", std::process::id()));
        let path = path.to_str().unwrap();
        let mut project = crate::edit::Project::default();
        project.append_video("photo", "photo.png", 5.0);
        let sky = Plane::ellipse(64, 48, [4.0, 4.0], [60.0, 44.0]);
        let ground = Plane::rect(64, 48, [0.0, 30.0], [64.0, 48.0]);
        let clip = &mut project.tracks[0].clips[0];
        clip.masks.push(LocalMask::new("Sky", sky.clone()).unwrap());
        clip.masks.push(LocalMask::new("Ground", ground.clone()).unwrap());
        clip.masks[1].visible = false;
        clip.masks[1].adjust.saturation = 0.5;
        project.save(path).unwrap();

        let back = crate::edit::Project::load(path).unwrap();
        let _ = std::fs::remove_file(path);
        let masks = &back.tracks[0].clips[0].masks;
        assert_eq!(masks.len(), 2);
        assert_eq!((masks[0].name.as_str(), masks[0].visible), ("Sky", true));
        assert_eq!((masks[1].name.as_str(), masks[1].visible), ("Ground", false));
        assert_eq!(masks[1].adjust.saturation, 0.5);
        assert_eq!(*masks[0].matte.plane, sky);
        assert_eq!(*masks[1].matte.plane, ground);
    }

    #[test]
    fn base64_round_trips_every_tail_length() {
        for len in 0..8usize {
            let bytes: Vec<u8> = (0..len as u8).map(|i| i.wrapping_mul(97)).collect();
            assert_eq!(b64_decode(&b64_encode(&bytes)).unwrap(), bytes);
        }
    }

    #[test]
    fn a_png_matte_reads_white_as_selected_and_transparent_as_out() {
        let img = image::RgbaImage::from_fn(4, 1, |x, _| match x {
            0 => image::Rgba([255, 255, 255, 255]),
            1 => image::Rgba([0, 0, 0, 255]),
            2 => image::Rgba([255, 255, 255, 0]),
            _ => image::Rgba([128, 128, 128, 255]),
        });
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        let p = Plane::from_image_bytes(&bytes).unwrap();
        assert_eq!(&p.data[..3], &[255, 0, 0]);
        assert!((120..136).contains(&p.data[3]));
    }

    #[test]
    fn the_brush_switch_removes_without_a_modifier() {
        let mut sel = Selection { brush_remove: true, ..Default::default() };
        sel.target(1, (100, 100));
        sel.apply(Plane::rect(100, 100, [0.0, 0.0], [100.0, 100.0]), Combine::Replace);
        let op = sel.combine(false, false, Tool::Brush);
        sel.apply(Plane::stroke(100, 100, &[[50.0, 50.0]], 10.0), op);
        let plane = sel.plane.as_ref().unwrap();
        assert_eq!(at(plane, 50, 50), 0);
        assert_eq!(at(plane, 5, 5), 255);
        assert_eq!(sel.combine(true, false, Tool::Brush), Combine::Add, "Shift still adds");
        assert_eq!(sel.combine(false, false, Tool::Lasso), Combine::Replace);
    }

    #[test]
    fn modifiers_choose_how_a_gesture_joins() {
        assert_eq!(combine_for(false, false, Tool::Lasso), Combine::Replace);
        assert_eq!(combine_for(false, false, Tool::Brush), Combine::Add);
        assert_eq!(combine_for(true, false, Tool::Ellipse), Combine::Add);
        assert_eq!(combine_for(false, true, Tool::Brush), Combine::Subtract);
        assert_eq!(combine_for(true, true, Tool::Rectangle), Combine::Intersect);
    }

    #[test]
    fn every_gesture_tool_produces_a_selection() {
        for tool in [Tool::Lasso, Tool::Polygon, Tool::Rectangle, Tool::Ellipse, Tool::Brush] {
            let mut s = Selection { tool: Some(tool), ..Selection::default() };
            s.target(7, (80, 60));
            s.brush_radius = 6.0;
            s.path = vec![[10.0, 10.0], [70.0, 12.0], [40.0, 50.0]];
            if matches!(tool, Tool::Rectangle | Tool::Ellipse) {
                s.path = vec![[10.0, 10.0], [70.0, 50.0]];
            }
            s.commit(combine_for(false, false, tool));
            let p = s.plane.as_ref().expect("a selection");
            assert!(count(p) > 50, "{tool:?} selected {} px", count(p));
            assert!(s.path.is_empty());
        }
    }

    #[test]
    fn subtracting_from_nothing_stays_empty_and_a_new_clip_starts_fresh() {
        let mut s = Selection::default();
        s.target(1, (10, 10));
        s.apply(Plane::rect(10, 10, [0.0, 0.0], [5.0, 5.0]), Combine::Subtract);
        assert!(s.plane.is_none());
        s.apply(Plane::rect(10, 10, [0.0, 0.0], [5.0, 5.0]), Combine::Add);
        assert!(s.plane.is_some());
        s.target(2, (10, 10));
        assert!(s.plane.is_none());
    }

    #[test]
    fn the_view_mapping_undoes_zoom_and_flip() {
        let mut fx = Effects::default();
        assert_eq!(view_to_source([0.25, 0.75], Some(&fx), 0), [0.25, 0.75]);
        fx.zoom = 2.0;
        assert_eq!(view_to_source([0.0, 0.0], Some(&fx), 0), [0.25, 0.25]);
        fx.flip_h = true;
        assert_eq!(view_to_source([0.0, 0.0], Some(&fx), 0), [0.75, 0.25]);
    }

    #[test]
    fn rotation_inverse_lands_on_the_source_corner() {
        // The top-left of a 90° clockwise frame is the source's bottom-left.
        assert_eq!(unrotate_uv([0.0, 0.0], 1), [0.0, 1.0]);
        assert_eq!(unrotate_uv([0.0, 0.0], 2), [1.0, 1.0]);
        assert_eq!(unrotate_uv([0.0, 0.0], 3), [1.0, 0.0]);
        assert_eq!(unrotate_uv([0.25, 0.5], 0), [0.25, 0.5]);
    }
}

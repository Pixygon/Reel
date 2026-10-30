//! Reel photo: a non-destructive develop for stills — Light, Colour and
//! Presence — that photographers can trust because there is exactly ONE
//! renderer.
//!
//! `render()` is both the preview and the export: the Adjust panel shows its
//! output at preview size, and `export()` calls it at the chosen size. Video
//! needs the shader/ffmpeg mirror in `effects.rs` because a frame cannot
//! wait for the CPU; a still can, so there is nothing to mirror here and
//! nothing to drift. Clarity and dehaze are spatial, which no 33³ lattice
//! could express anyway.
//!
//! The existing grade stack does what it already can: `base_fx()` maps the
//! panel's exposure/contrast/temperature/tint/saturation onto `Effects`, and
//! `Curves`, `rgb_to_hsl` and `hsl_to_rgb` are reused as they are. Like the
//! rest of the colour code, everything works on sRGB-encoded values in 0..1.
//!
//! Settings are a `.reel` document next to the picture (`photo.jpg.reel`,
//! the full name so `clip.jpg` never collides with `clip.mp4`'s project).
//! The original file is only ever read.

use crate::edit::{write_atomic, Project};
use crate::effects::{hsl_to_rgb, rgb_to_hsl, Curves, Effects, LUMA};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The preview's long edge. Local operators are defined in fractions of the
/// frame, so this changes how sharp the preview is, never what it looks like.
pub const PREVIEW_EDGE: u32 = 2560;

/// Grid the spatial operators measure on. Fixed, so a 2560 preview and a
/// 6000 px export see the same blurred picture.
const LOCAL_GRID: usize = 512;

/// The eight colour bands, by centre hue in degrees.
pub const BANDS: [(&str, f32); 8] = [
    ("red", 0.0),
    ("orange", 30.0),
    ("yellow", 60.0),
    ("green", 120.0),
    ("aqua", 180.0),
    ("blue", 240.0),
    ("purple", 285.0),
    ("magenta", 320.0),
];

/// One band of the per-colour HSL mixer. All -100..100, 0 = untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Band {
    pub hue: f32,
    pub sat: f32,
    pub lum: f32,
}

/// Everything the Adjust panel can do to a photo. Exposure is in stops, the
/// rest are the -100..100 a Lightroom user already knows. All-default is
/// identity, and every field defaults so older documents keep loading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Adjust {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub temperature: f32,
    pub tint: f32,
    pub vibrance: f32,
    pub saturation: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub vignette: f32,
    pub hsl: [Band; 8],
    pub curve: Curves,
}

/// The scalar operators: `(CLI flag, lowest, highest)`. One table for the
/// panel-independent surface — the CLI and its test both read it.
pub const SCALARS: [(&str, f32, f32); 13] = [
    ("exposure", -5.0, 5.0),
    ("contrast", -100.0, 100.0),
    ("highlights", -100.0, 100.0),
    ("shadows", -100.0, 100.0),
    ("whites", -100.0, 100.0),
    ("blacks", -100.0, 100.0),
    ("temperature", -100.0, 100.0),
    ("tint", -100.0, 100.0),
    ("vibrance", -100.0, 100.0),
    ("saturation", -100.0, 100.0),
    ("clarity", -100.0, 100.0),
    ("dehaze", -100.0, 100.0),
    ("vignette", -100.0, 100.0),
];

fn smooth(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn luma(c: [f32; 3]) -> f32 {
    c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2]
}

impl Adjust {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    pub fn scalar_mut(&mut self, name: &str) -> Option<&mut f32> {
        Some(match name {
            "exposure" => &mut self.exposure,
            "contrast" => &mut self.contrast,
            "highlights" => &mut self.highlights,
            "shadows" => &mut self.shadows,
            "whites" => &mut self.whites,
            "blacks" => &mut self.blacks,
            "temperature" => &mut self.temperature,
            "tint" => &mut self.tint,
            "vibrance" => &mut self.vibrance,
            "saturation" => &mut self.saturation,
            "clarity" => &mut self.clarity,
            "dehaze" => &mut self.dehaze,
            "vignette" => &mut self.vignette,
            _ => return None,
        })
    }

    /// The panel's global sliders on the existing stack: one stop of light
    /// is 2^(1/2.2) on these encoded values.
    fn base_fx(&self) -> Effects {
        Effects {
            exposure: 2f32.powf(self.exposure.clamp(-5.0, 5.0) / 2.2),
            contrast: 2f32.powf(self.contrast.clamp(-100.0, 100.0) / 100.0),
            wb_temp: self.temperature.clamp(-100.0, 100.0) / 100.0,
            wb_tint: self.tint.clamp(-100.0, 100.0) / 100.0,
            ..Default::default()
        }
    }

    /// Highlights, shadows, whites and blacks as a shift at luma `l`. Each
    /// slider owns a soft zone of the tonal range; the amplitudes are small
    /// enough that a slider alone can never invert the ramp.
    fn tone_shift(&self, l: f32) -> f32 {
        let shadows = smooth(0.0, 0.25, l) * (1.0 - smooth(0.25, 0.6, l));
        let highlights = smooth(0.4, 0.75, l) * (1.0 - smooth(0.75, 1.0, l));
        let whites = smooth(0.6, 1.0, l);
        let blacks = 1.0 - smooth(0.0, 0.4, l);
        (self.shadows * shadows * 0.15
            + self.highlights * highlights * 0.15
            + self.whites * whites * 0.25
            + self.blacks * blacks * 0.25)
            / 100.0
    }

    /// The per-colour mixer: `(hue degrees, saturation factor, lightness
    /// shift)` for a hue, blended between the two nearest band centres so
    /// no hue falls between bands.
    fn band_push(&self, hue: f32) -> (f32, f32, f32) {
        let mut b = 7;
        while b > 0 && hue < BANDS[b].1 {
            b -= 1;
        }
        let next = (b + 1) % 8;
        let span = (BANDS[next].1 - BANDS[b].1).rem_euclid(360.0);
        let t = (hue - BANDS[b].1).rem_euclid(360.0) / span;
        let (x, y) = (self.hsl[b], self.hsl[next]);
        (
            lerp(x.hue, y.hue, t) * 0.3,
            1.0 + lerp(x.sat, y.sat, t) / 100.0,
            lerp(x.lum, y.lum, t) / 100.0 * 0.35,
        )
    }

    fn has_bands(&self) -> bool {
        self.hsl.iter().any(|b| *b != Band::default())
    }

    /// One pixel. `local` is the blurred (luma, dark channel, airlight) under
    /// it, `uv` where it sits in the frame. THE formula; `develop` only
    /// walks it over the rows.
    fn pixel(&self, rgb: [f32; 3], local: [f32; 3], uv: [f32; 2], fx: &Effects, sat_fx: &Effects) -> [f32; 3] {
        let mut c = rgb;
        if fx.wb_temp != 0.0 || fx.wb_tint != 0.0 {
            c = fx.grade_reference(c);
        }
        c = fx.apply_reference(c);

        if self.dehaze != 0.0 {
            let (dark, air) = (local[1], local[2].max(1e-3));
            let amount = self.dehaze.clamp(-100.0, 100.0) / 100.0;
            if amount > 0.0 {
                // The atmospheric model: I = J·t + A·(1 − t). Solve for J.
                let t = (1.0 - 0.95 * amount * (dark / air).min(1.0)).max(0.15);
                c = c.map(|v| (v - air) / t + air);
            } else {
                c = c.map(|v| lerp(v, air, -amount * 0.4));
            }
        }

        let l = luma(c).clamp(0.0, 1.0);
        let mut shift = self.tone_shift(l);
        if self.clarity != 0.0 {
            // Local contrast, strongest in the mid-tones: blacks and whites
            // must not halo.
            let mid = 4.0 * l * (1.0 - l);
            shift += (l - local[0]) * mid * self.clarity.clamp(-100.0, 100.0) / 100.0 * 1.2;
        }
        c = c.map(|v| v + shift);

        if self.has_bands() {
            let hsl = rgb_to_hsl(c.map(|v| v.clamp(0.0, 1.0)));
            let (dh, ds, dl) = self.band_push(hsl[0]);
            // Greys carry no colour, so a colour band leaves them alone.
            let weight = hsl[1] * 4.0 * hsl[2] * (1.0 - hsl[2]);
            c = hsl_to_rgb([hsl[0] + dh, hsl[1] * ds, hsl[2] + dl * weight]);
        }

        if self.vibrance != 0.0 {
            // Saturation that spares what is already saturated.
            let max = c[0].max(c[1]).max(c[2]);
            let min = c[0].min(c[1]).min(c[2]);
            let gain = 1.0 + self.vibrance.clamp(-100.0, 100.0) / 100.0 * (1.0 - (max - min).clamp(0.0, 1.0));
            let y = luma(c);
            c = c.map(|v| y + (v - y) * gain);
        }
        c = sat_fx.apply_reference(c);

        if !self.curve.is_identity() {
            c = [self.curve.apply(0, c[0]), self.curve.apply(1, c[1]), self.curve.apply(2, c[2])];
        }

        if self.vignette != 0.0 {
            // Radius² normalised so the corners are 1 on any aspect ratio.
            let (x, y) = (uv[0] * 2.0 - 1.0, uv[1] * 2.0 - 1.0);
            let r2 = (x * x + y * y) * 0.5;
            let gain = 1.0 + self.vignette.clamp(-100.0, 100.0) / 100.0 * 0.8 * smooth(0.15, 1.0, r2);
            c = c.map(|v| v * gain);
        }
        c.map(|v| v.clamp(0.0, 1.0))
    }
}

/// A blurred measurement of the picture on a fixed grid: what clarity and
/// dehaze read. Fractions of the frame on purpose — see `LOCAL_GRID`.
struct LocalMap {
    w: usize,
    h: usize,
    luma: Vec<f32>,
    dark: Vec<f32>,
    air: f32,
}

/// Three box passes along both axes: gaussian enough, O(1) per pixel.
fn blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let line = |get: &dyn Fn(usize) -> f32, n: usize, put: &mut dyn FnMut(usize, f32)| {
        let span = (2 * r + 1) as f32;
        let at = |i: isize| get(i.clamp(0, n as isize - 1) as usize);
        let mut sum: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for i in 0..n {
            put(i, sum / span);
            sum += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
        }
    };
    let mut a = src.to_vec();
    let mut b = vec![0.0; a.len()];
    for _ in 0..3 {
        for y in 0..h {
            line(&|x| a[y * w + x], w, &mut |x, v| b[y * w + x] = v);
        }
        for x in 0..w {
            line(&|y| b[y * w + x], h, &mut |y, v| a[y * w + x] = v);
        }
    }
    a
}

impl LocalMap {
    fn measure(rgba: &[u8], w: usize, h: usize) -> Self {
        let long = w.max(h);
        let scale = LOCAL_GRID.min(long) as f64 / long as f64;
        let gw = ((w as f64 * scale).round() as usize).max(1);
        let gh = ((h as f64 * scale).round() as usize).max(1);
        let mut l = vec![0.0f32; gw * gh];
        let mut d = vec![0.0f32; gw * gh];
        let mut n = vec![0u32; gw * gh];
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            let (x, y) = (i % w, i / w);
            let g = (y * gh / h) * gw + x * gw / w;
            let c = [px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0];
            l[g] += luma(c);
            d[g] += c[0].min(c[1]).min(c[2]);
            n[g] += 1;
        }
        for g in 0..l.len() {
            let c = n[g].max(1) as f32;
            l[g] /= c;
            d[g] /= c;
        }
        let glong = gw.max(gh);
        let luma = blur(&l, gw, gh, ((glong as f32 * 0.02).round() as usize).max(1));
        let dark = blur(&d, gw, gh, ((glong as f32 * 0.03).round() as usize).max(1));
        // The haze's own brightness: the brightest of the unblurred areas.
        let air = l.iter().fold(0.0f32, |a, b| a.max(*b)).clamp(0.1, 1.0);
        Self { w: gw, h: gh, luma, dark, air }
    }

    fn sample(&self, uv: [f32; 2]) -> [f32; 3] {
        let (fx, fy) = (uv[0] * self.w as f32 - 0.5, uv[1] * self.h as f32 - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let ix = |x: f32| (x.max(0.0) as usize).min(self.w - 1);
        let iy = |y: f32| (y.max(0.0) as usize).min(self.h - 1);
        let read = |m: &[f32]| {
            let at = |x: f32, y: f32| m[iy(y) * self.w + ix(x)];
            lerp(
                lerp(at(x0, y0), at(x0 + 1.0, y0), tx),
                lerp(at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0), tx),
                ty,
            )
        };
        [read(&self.luma), read(&self.dark), self.air]
    }
}

/// Straight-alpha RGBA8 pixels. The alpha channel passes through untouched.
#[derive(Clone)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Pixels {
    /// Shrink to fit `box_w`×`box_h` — never enlarge. The one resampler, for
    /// the preview and the export alike.
    fn fitted(&self, box_w: u32, box_h: u32) -> Pixels {
        let (w, h) = (self.width, self.height);
        let scale = (box_w as f64 / w as f64).min(box_h as f64 / h as f64);
        if scale >= 1.0 {
            return self.clone();
        }
        let (nw, nh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
        let src = image::RgbaImage::from_raw(w, h, self.data.clone()).expect("pixels match their size");
        Pixels { width: nw, height: nh, data: image::imageops::thumbnail(&src, nw, nh).into_raw() }
    }
}

/// Apply the settings to pixels of any size.
pub fn develop(src: &Pixels, adj: &Adjust) -> Vec<u8> {
    if adj.is_identity() {
        return src.data.clone();
    }
    let (w, h) = (src.width as usize, src.height as usize);
    let local = (adj.clarity != 0.0 || adj.dehaze != 0.0).then(|| LocalMap::measure(&src.data, w, h));
    let fx = adj.base_fx();
    let sat_fx = Effects {
        saturation: 1.0 + adj.saturation.clamp(-100.0, 100.0) / 100.0,
        ..Default::default()
    };
    let mut out = src.data.clone();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let band = (h.div_ceil(threads)).max(1) * w * 4;
    std::thread::scope(|s| {
        for (b, rows) in out.chunks_mut(band).enumerate() {
            let (local, fx, sat_fx) = (&local, &fx, &sat_fx);
            s.spawn(move || {
                let first = b * band / (w * 4);
                for (i, px) in rows.chunks_exact_mut(4).enumerate() {
                    let (x, y) = (i % w, first + i / w);
                    let uv = [(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32];
                    let sampled = local.as_ref().map(|m| m.sample(uv)).unwrap_or_default();
                    let c = adj.pixel(
                        [px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0],
                        sampled,
                        uv,
                        fx,
                        sat_fx,
                    );
                    for k in 0..3 {
                        px[k] = (c[k] * 255.0).round() as u8;
                    }
                }
            });
        }
    });
    out
}

/// THE renderer: fit to the box, then develop. Preview and export are both
/// this call, so at the same size they are the same bytes.
pub fn render(original: &Pixels, adj: &Adjust, box_w: u32, box_h: u32) -> Pixels {
    let fitted = original.fitted(box_w, box_h);
    let data = develop(&fitted, adj);
    Pixels { width: fitted.width, height: fitted.height, data }
}

/// Decode a still, upright (phone photos store sideways pixels and an EXIF
/// orientation tag).
pub fn load(path: &str) -> Result<Pixels> {
    let doc = crate::media::ImageDoc::open(path)?;
    Ok(Pixels { width: doc.width, height: doc.height, data: doc.data })
}

// ── The document ─────────────────────────────────────────────────────────

/// What a `.reel` carries for a photo. `Project.photo` is `None` for edits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotoEdit {
    pub source: String,
    #[serde(default)]
    pub adjust: Adjust,
}

/// Where a photo's settings live.
pub fn sidecar(path: &str) -> String {
    format!("{path}.reel")
}

/// The settings saved for this photo, or identity.
pub fn load_settings(path: &str) -> Adjust {
    let doc = sidecar(path);
    if !Path::new(&doc).exists() {
        return Adjust::default();
    }
    match Project::load(&doc) {
        Ok(p) => p.photo.map(|e| e.adjust).unwrap_or_default(),
        Err(e) => {
            log::warn!("could not read {doc}: {e}");
            Adjust::default()
        }
    }
}

pub fn settings_json(path: &str, adjust: &Adjust) -> Result<String> {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Absolute, so the document finds its photo from any directory.
    let source = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    let mut project = Project::default();
    project.name = name;
    project.photo = Some(PhotoEdit { source, adjust: *adjust });
    Ok(serde_json::to_string_pretty(&project)?)
}

pub fn save_settings(path: &str, adjust: &Adjust) -> Result<()> {
    write_atomic(&sidecar(path), &settings_json(path, adjust)?)
}

/// The photo a `.reel` document points at: beside the document if it moved
/// with its picture, else where it was saved from.
pub fn source_of(doc_path: &str, edit: &PhotoEdit) -> String {
    match doc_path.strip_suffix(".reel") {
        Some(beside) if Path::new(beside).exists() => beside.to_string(),
        _ => edit.source.clone(),
    }
}

// ── Copy/paste and presets ───────────────────────────────────────────────
// Files, not process state: a double-click starts a new Reel, and copying
// from one photo to the next has to survive that.

fn config_dir() -> PathBuf {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    var("XDG_CONFIG_HOME")
        .or_else(|| var("HOME").map(|h| h.join(".config")))
        .or_else(|| var("APPDATA"))
        .unwrap_or_else(std::env::temp_dir)
        .join("reel")
}

fn clipboard_path() -> PathBuf {
    config_dir().join("photo-clipboard.json")
}

fn presets_path() -> PathBuf {
    config_dir().join("photo-presets.json")
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&path.to_string_lossy(), &serde_json::to_string_pretty(value)?)
}

pub fn copy_settings(adjust: &Adjust) -> Result<()> {
    write_json(&clipboard_path(), adjust)
}

pub fn pasted_settings() -> Option<Adjust> {
    serde_json::from_str(&std::fs::read_to_string(clipboard_path()).ok()?).ok()
}

pub fn read_presets_at(path: &Path) -> BTreeMap<String, Adjust> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn read_presets() -> BTreeMap<String, Adjust> {
    read_presets_at(&presets_path())
}

pub fn save_preset_at(path: &Path, name: &str, adjust: &Adjust) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a preset needs a name");
    }
    let mut all = read_presets_at(path);
    all.insert(name.to_string(), *adjust);
    write_json(path, &all)
}

pub fn save_preset(name: &str, adjust: &Adjust) -> Result<()> {
    save_preset_at(&presets_path(), name, adjust)
}

pub fn delete_preset_at(path: &Path, name: &str) -> Result<bool> {
    let mut all = read_presets_at(path);
    let found = all.remove(name).is_some();
    if found {
        write_json(path, &all)?;
    }
    Ok(found)
}

pub fn delete_preset(name: &str) -> Result<bool> {
    delete_preset_at(&presets_path(), name)
}

// ── Export ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Jpeg, Format::Png, Format::Webp];

    pub fn from_path(path: &str) -> Result<Self> {
        match Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
            Some("jpg" | "jpeg") => Ok(Self::Jpeg),
            Some("png") => Ok(Self::Png),
            Some("webp") => Ok(Self::Webp),
            _ => bail!("{path}: a photo exports as .jpg, .png or .webp"),
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
        }
    }
}

/// Size and quality of one export. `width`/`height` are a box the picture
/// fits inside (0 = no limit); nothing is ever enlarged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOpts {
    /// 1..100. PNG is lossless and ignores it; WebP at 100 is lossless.
    pub quality: u8,
    pub width: u32,
    pub height: u32,
}

impl Default for ExportOpts {
    fn default() -> Self {
        Self { quality: 92, width: 0, height: 0 }
    }
}

/// JPEG has no alpha: transparency lands on white, like every photo tool.
fn flattened(p: &Pixels) -> image::RgbImage {
    let mut rgb = Vec::with_capacity(p.data.len() / 4 * 3);
    for px in p.data.chunks_exact(4) {
        let a = px[3] as u32;
        for k in 0..3 {
            rgb.push(((px[k] as u32 * a + 255 * (255 - a) + 127) / 255) as u8);
        }
    }
    image::RgbImage::from_raw(p.width, p.height, rgb).expect("rgb matches its size")
}

fn encode(p: &Pixels, format: Format, quality: u8, out: &Path) -> Result<()> {
    let q = quality.clamp(1, 100);
    match format {
        Format::Jpeg => {
            let file = std::io::BufWriter::new(std::fs::File::create(out)?);
            image::codecs::jpeg::JpegEncoder::new_with_quality(file, q).encode_image(&flattened(p))?;
        }
        Format::Png => {
            image::RgbaImage::from_raw(p.width, p.height, p.data.clone())
                .ok_or_else(|| anyhow!("image buffer size mismatch"))?
                .save_with_format(out, image::ImageFormat::Png)?;
        }
        Format::Webp => {
            if !encode_webp_ffmpeg(p, q, out) {
                // No libwebp in this ffmpeg: lossless is still honest output.
                log::warn!("ffmpeg has no libwebp; writing lossless WebP");
                image::RgbaImage::from_raw(p.width, p.height, p.data.clone())
                    .ok_or_else(|| anyhow!("image buffer size mismatch"))?
                    .save_with_format(out, image::ImageFormat::WebP)?;
            }
        }
    }
    Ok(())
}

/// The image crate only writes lossless WebP; quality goes through ffmpeg.
fn encode_webp_ffmpeg(p: &Pixels, quality: u8, out: &Path) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let size = format!("{}x{}", p.width, p.height);
    let q = quality.to_string();
    let lossless = if quality >= 100 { "1" } else { "0" };
    let child = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &size, "-i", "-"])
        .args(["-frames:v", "1", "-c:v", "libwebp", "-lossless", lossless, "-quality", &q, "-f", "webp"])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    let wrote = child.stdin.take().is_some_and(|mut s| s.write_all(&p.data).is_ok());
    child.wait().map(|s| s.success()).unwrap_or(false) && wrote
}

/// Render the photo into `out`. The original stays exactly as it was, and
/// a half-written export never appears under the final name.
pub fn export(source: &str, adjust: &Adjust, opts: ExportOpts, out: &str) -> Result<Pixels> {
    let format = Format::from_path(out)?;
    let same = |a: &str, b: &str| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    };
    if same(source, out) {
        bail!("{out} is the original — Reel never overwrites it; choose another name");
    }
    let original = load(source)?;
    let limit = |v: u32| if v == 0 { u32::MAX } else { v };
    let done = render(&original, adjust, limit(opts.width), limit(opts.height));
    let tmp = format!("{out}.reel-tmp");
    let written = encode(&done, format, opts.quality, Path::new(&tmp))
        .and_then(|()| std::fs::rename(&tmp, out).with_context(|| format!("could not write {out}")));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    Ok(done)
}

/// Default name for an export: beside the original, never over it.
pub fn default_output(source: &str, format: Format) -> String {
    let p = Path::new(source);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "photo".into());
    p.with_file_name(format!("{stem}-edit.{}", format.ext())).to_string_lossy().into_owned()
}

// ── The live state behind the Adjust panel ───────────────────────────────

/// One photo open in the player. `base` is the original at preview size and
/// is never written to; everything on screen derives from it.
pub struct PhotoState {
    pub path: String,
    /// The original's real size (the panel only ever holds the preview).
    pub full: (u32, u32),
    pub base: Arc<Pixels>,
    pub adjust: Adjust,
    /// What `adjust` looks like, at preview size.
    pub developed: Vec<u8>,
    /// Before/after: `Some(f)` shows the original left of `f` (0..1).
    pub compare: Option<f32>,
    pub panel_open: bool,
    /// Which colour band the mixer is showing.
    pub band: usize,
    pub preset_name: String,
    pub export_open: bool,
    pub export_format: Format,
    pub export_opts: ExportOpts,
    pub export_path: String,
    /// What the last export did, for the dialog.
    pub export_note: String,
    /// Named looks, read at open and after each change.
    pub presets: BTreeMap<String, Adjust>,
    export_job: Option<crossbeam_channel::Receiver<Result<String, String>>>,
    /// The split the texture currently shows; `None` forces a re-upload.
    pub shown: Option<f32>,
    stale: bool,
    job: Option<crossbeam_channel::Receiver<(Adjust, Vec<u8>)>>,
    dirty_since: Option<std::time::Instant>,
    /// Settings already on disk, so identity never litters a sidecar.
    has_sidecar: bool,
}

impl PhotoState {
    pub fn new(path: &str, original: Pixels) -> Self {
        let adjust = load_settings(path);
        let full = (original.width, original.height);
        let base = original.fitted(PREVIEW_EDGE, PREVIEW_EDGE);
        let developed = develop(&base, &adjust);
        Self {
            path: path.to_string(),
            full,
            base: Arc::new(base),
            adjust,
            developed,
            compare: None,
            panel_open: true,
            band: 0,
            preset_name: String::new(),
            export_open: false,
            export_format: Format::Jpeg,
            export_opts: ExportOpts::default(),
            export_path: default_output(path, Format::Jpeg),
            export_note: String::new(),
            presets: read_presets(),
            export_job: None,
            shown: None,
            stale: false,
            job: None,
            dirty_since: None,
            has_sidecar: Path::new(&sidecar(path)).exists(),
        }
    }

    /// Every change to the settings comes through here.
    pub fn set(&mut self, adjust: Adjust) {
        if adjust != self.adjust {
            self.adjust = adjust;
            self.stale = true;
            self.dirty_since = Some(std::time::Instant::now());
        }
    }

    /// Render the export on a worker; the full-size original is decoded
    /// there, so the window never waits for it.
    pub fn start_export(&mut self) {
        if self.export_job.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let (src, adjust, opts, out) = (self.path.clone(), self.adjust, self.export_opts, self.export_path.clone());
        std::thread::spawn(move || {
            let done = export(&src, &adjust, opts, &out)
                .map(|p| format!("Saved {out} ({}×{})", p.width, p.height))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(done);
        });
        self.export_note = "Exporting…".into();
        self.export_job = Some(rx);
    }

    pub fn exporting(&self) -> bool {
        self.export_job.is_some()
    }

    /// Latest wins: one render at a time, and a slider drag that outruns it
    /// just renders the newest value next.
    pub fn poll(&mut self) {
        if let Some(Ok(done)) = self.export_job.as_ref().map(|rx| rx.try_recv()) {
            self.export_job = None;
            self.export_note = done.unwrap_or_else(|e| e);
        }
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok((rendered, pixels)) => {
                    self.job = None;
                    self.developed = pixels;
                    self.shown = None;
                    self.stale |= rendered != self.adjust;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.job = None,
            }
        }
        if self.stale && self.job.is_none() {
            self.stale = false;
            let (tx, rx) = crossbeam_channel::bounded(1);
            let (base, adjust) = (self.base.clone(), self.adjust);
            std::thread::spawn(move || {
                let _ = tx.send((adjust, develop(&base, &adjust)));
            });
            self.job = Some(rx);
        }
    }

    /// Anything still owed? Keeps the Wait-driven loop polling until the
    /// render lands and the debounce has written the settings.
    pub fn busy(&self) -> bool {
        self.stale || self.job.is_some() || self.dirty_since.is_some() || self.export_job.is_some()
    }

    /// Autosave, same manners as a project: ~700 ms after the last change,
    /// on a worker, atomically. An untouched photo never gets a document.
    pub fn poll_save(&mut self) -> Option<std::thread::JoinHandle<Result<()>>> {
        let since = self.dirty_since?;
        if since.elapsed() < std::time::Duration::from_millis(700) {
            return None;
        }
        self.save_now()
    }

    /// Write owed settings immediately (leaving the photo, quitting).
    pub fn save_now(&mut self) -> Option<std::thread::JoinHandle<Result<()>>> {
        self.dirty_since.take()?;
        if self.adjust.is_identity() && !self.has_sidecar {
            return None;
        }
        self.has_sidecar = true;
        let (path, adjust) = (self.path.clone(), self.adjust);
        Some(std::thread::spawn(move || save_settings(&path, &adjust)))
    }

    /// The bytes for the texture: `developed`, with the original left of the
    /// split. `split` of 0 is the plain result.
    pub fn composite(&self, split: f32) -> Vec<u8> {
        let mut out = self.developed.clone();
        let w = self.base.width as usize;
        let cut = ((split.clamp(0.0, 1.0) * w as f32).round() as usize).min(w) * 4;
        if cut > 0 {
            for (dst, src) in out.chunks_exact_mut(w * 4).zip(self.base.data.chunks_exact(w * 4)) {
                dst[..cut].copy_from_slice(&src[..cut]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A photo-like test card: a luminance sweep across, hue sweep down.
    fn card(w: u32, h: u32) -> Pixels {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let l = (x as f32 + 0.5) / w as f32;
                let hue = (y as f32 + 0.5) / h as f32 * 360.0;
                let c = hsl_to_rgb([hue, 0.6, 0.15 + l * 0.7]);
                data.extend([(c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, 255]);
            }
        }
        Pixels { width: w, height: h, data }
    }

    fn one(name: &str, value: f32) -> Adjust {
        let mut a = Adjust::default();
        *a.scalar_mut(name).unwrap() = value;
        a
    }

    /// Every operator, with something for each to chew on.
    fn every_operator() -> Vec<(String, Adjust)> {
        let mut all: Vec<(String, Adjust)> = SCALARS
            .iter()
            .map(|(n, _, hi)| (n.to_string(), one(n, hi * 0.6)))
            .collect();
        for (b, (name, _)) in BANDS.iter().enumerate() {
            let mut a = Adjust::default();
            a.hsl[b] = Band { hue: 40.0, sat: -50.0, lum: 30.0 };
            all.push((format!("hsl-{name}"), a));
        }
        let mut a = Adjust::default();
        a.curve.master = [0.05, 0.2, 0.5, 0.8, 0.95];
        all.push(("curve".into(), a));
        all
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("reel-photo-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_png(p: &Pixels, path: &Path) {
        image::RgbaImage::from_raw(p.width, p.height, p.data.clone()).unwrap().save(path).unwrap();
    }

    fn worst_delta(a: &[u8], b: &[u8]) -> i32 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
    }

    fn at(p: &Pixels, data: &[u8], x: u32, y: u32) -> [i32; 3] {
        let i = ((y * p.width + x) * 4) as usize;
        [data[i] as i32, data[i + 1] as i32, data[i + 2] as i32]
    }

    #[test]
    fn identity_changes_nothing() {
        let c = card(64, 48);
        assert_eq!(develop(&c, &Adjust::default()), c.data);
        let zero_effect = Adjust { exposure: 0.0, ..Default::default() };
        assert!(zero_effect.is_identity());
    }

    /// The contract of the whole feature: for every operator, what the panel
    /// shows is what the file holds. Same size → the same bytes through PNG;
    /// a preview smaller than the export → the same picture, resampled.
    #[test]
    fn every_operator_exports_exactly_what_the_preview_shows() {
        let dir = scratch("parity");
        let src = dir.join("src.png");
        let original = card(800, 600);
        write_png(&original, &src);
        let src_s = src.to_string_lossy().into_owned();
        for (name, adj) in every_operator() {
            let state_preview = {
                let mut st = PhotoState::new(&src_s, load(&src_s).unwrap());
                st.set(adj);
                st.poll();
                while st.job.is_some() {
                    st.poll();
                }
                st.composite(0.0)
            };
            let moved = worst_delta(&state_preview, &original.data);
            assert!(moved > 2, "{name} did nothing to the card");

            let png = dir.join(format!("{name}.png"));
            let png_s = png.to_string_lossy().into_owned();
            export(&src_s, &adj, ExportOpts::default(), &png_s).unwrap();
            let got = image::open(&png).unwrap().to_rgba8().into_raw();
            assert_eq!(worst_delta(&got, &state_preview), 0, "{name}: PNG differs from the preview");

            let lossless = dir.join(format!("{name}.webp"));
            let lossless_s = lossless.to_string_lossy().into_owned();
            export(&src_s, &adj, ExportOpts { quality: 100, ..Default::default() }, &lossless_s).unwrap();
            let got = image::open(&lossless).unwrap().to_rgba8().into_raw();
            assert_eq!(worst_delta(&got, &state_preview), 0, "{name}: lossless WebP differs");

            // A preview of 320 is the downscaled render of the same file.
            let small = render(&original, &adj, 320, 320);
            let big = render(&original, &adj, u32::MAX, u32::MAX);
            let shrunk = big.fitted(320, 320);
            let (mut sum, n) = (0i64, small.data.len() as i64);
            for (a, b) in small.data.iter().zip(&shrunk.data) {
                sum += (*a as i64 - *b as i64).abs();
            }
            assert!(sum / n <= 2, "{name}: preview scale changes the look (mean Δ{})", sum / n);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lossy_formats_stay_close_to_the_preview() {
        let dir = scratch("lossy");
        let src = dir.join("src.png");
        let original = card(400, 300);
        write_png(&original, &src);
        let adj = Adjust { exposure: 0.4, clarity: 30.0, vibrance: 25.0, ..Default::default() };
        let want = render(&original, &adj, u32::MAX, u32::MAX);
        for ext in ["jpg", "webp"] {
            let out = dir.join(format!("x.{ext}"));
            let out_s = out.to_string_lossy().into_owned();
            export(&src.to_string_lossy(), &adj, ExportOpts { quality: 95, ..Default::default() }, &out_s).unwrap();
            let got = image::open(&out).unwrap().to_rgba8();
            let sum: i64 = got.as_raw().iter().zip(&want.data).map(|(a, b)| (*a as i64 - *b as i64).abs()).sum();
            let mean = sum / got.as_raw().len() as i64;
            assert!(mean <= 3, "{ext}: mean Δ{mean}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quality_and_resize_reach_the_file() {
        let dir = scratch("size");
        let src = dir.join("src.png");
        write_png(&card(800, 600), &src);
        let s = src.to_string_lossy().into_owned();
        let bytes = |q: u8, ext: &str| {
            let out = dir.join(format!("q{q}.{ext}")).to_string_lossy().into_owned();
            export(&s, &Adjust::default(), ExportOpts { quality: q, ..Default::default() }, &out).unwrap();
            std::fs::metadata(&out).unwrap().len()
        };
        assert!(bytes(20, "jpg") < bytes(95, "jpg"));
        assert!(bytes(20, "webp") < bytes(95, "webp"));

        let out = dir.join("small.jpg").to_string_lossy().into_owned();
        export(&s, &Adjust::default(), ExportOpts { width: 400, ..Default::default() }, &out).unwrap();
        assert_eq!(image::image_dimensions(&out).unwrap(), (400, 300));
        // The box limits, it never enlarges.
        export(&s, &Adjust::default(), ExportOpts { width: 4000, height: 150, ..Default::default() }, &out).unwrap();
        assert_eq!(image::image_dimensions(&out).unwrap(), (200, 150));
        export(&s, &Adjust::default(), ExportOpts { width: 4000, ..Default::default() }, &out).unwrap();
        assert_eq!(image::image_dimensions(&out).unwrap(), (800, 600));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transparency_survives_png_and_flattens_on_white_in_jpeg() {
        let dir = scratch("alpha");
        let src = dir.join("src.png");
        let mut c = card(64, 64);
        for px in c.data.chunks_exact_mut(4) {
            px[3] = 0;
        }
        write_png(&c, &src);
        let s = src.to_string_lossy().into_owned();
        let adj = one("exposure", 1.0);
        let png = dir.join("o.png").to_string_lossy().into_owned();
        export(&s, &adj, ExportOpts::default(), &png).unwrap();
        assert!(image::open(&png).unwrap().to_rgba8().pixels().all(|p| p.0[3] == 0));
        let jpg = dir.join("o.jpg").to_string_lossy().into_owned();
        export(&s, &adj, ExportOpts::default(), &jpg).unwrap();
        let white = image::open(&jpg).unwrap().to_rgb8();
        assert!(white.pixels().all(|p| p.0.iter().all(|v| *v > 250)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_original_is_never_touched_and_settings_reopen() {
        let dir = scratch("doc");
        let src = dir.join("photo.png");
        write_png(&card(120, 90), &src);
        let s = src.to_string_lossy().into_owned();
        let before = std::fs::read(&src).unwrap();

        // Only opening leaves no trace.
        let mut st = PhotoState::new(&s, load(&s).unwrap());
        st.dirty_since = Some(std::time::Instant::now());
        if let Some(h) = st.save_now() {
            h.join().unwrap().unwrap();
        }
        assert!(!Path::new(&sidecar(&s)).exists(), "an untouched photo must leave no document");

        let adj = Adjust { exposure: 0.7, vignette: -40.0, ..Default::default() };
        st.set(adj);
        st.save_now().unwrap().join().unwrap().unwrap();

        // Reopening restores it, and the document is a real project.
        let again = PhotoState::new(&s, load(&s).unwrap());
        assert_eq!(again.adjust, adj);
        assert_eq!(again.developed, develop(&again.base, &adj), "reopens looking as it was left");
        let project = Project::load(&sidecar(&s)).unwrap();
        assert_eq!(project.photo.as_ref().unwrap().adjust, adj);
        assert!(project.export_segments().is_empty());

        let out = dir.join("out.jpg").to_string_lossy().into_owned();
        export(&s, &adj, ExportOpts::default(), &out).unwrap();
        assert!(export(&s, &adj, ExportOpts::default(), &s).is_err(), "refuses to overwrite the original");
        assert_eq!(std::fs::read(&src).unwrap(), before, "the original's bytes changed");

        // Reset all: back to the untouched picture, and it persists.
        let mut st = again;
        st.set(Adjust::default());
        st.save_now().unwrap().join().unwrap().unwrap();
        assert!(load_settings(&s).is_identity());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn before_after_splits_original_from_result() {
        let dir = scratch("split");
        let src = dir.join("photo.png");
        write_png(&card(100, 40), &src);
        let s = src.to_string_lossy().into_owned();
        let mut st = PhotoState::new(&s, load(&s).unwrap());
        st.developed = develop(&st.base, &one("exposure", 1.0));
        let shown = st.composite(0.5);
        let p = &*st.base;
        assert_eq!(at(p, &shown, 10, 20), at(p, &p.data, 10, 20), "left of the split is the original");
        assert_eq!(at(p, &shown, 90, 20), at(p, &st.developed, 90, 20), "right of it the result");
        assert_eq!(st.composite(0.0), st.developed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presets_save_list_and_delete() {
        let dir = scratch("presets");
        let file = dir.join("nested/presets.json");
        let look = Adjust { contrast: 20.0, vibrance: 15.0, ..Default::default() };
        save_preset_at(&file, " Golden hour ", &look).unwrap();
        save_preset_at(&file, "Punchy", &one("clarity", 40.0)).unwrap();
        assert!(save_preset_at(&file, "  ", &look).is_err());
        let all = read_presets_at(&file);
        assert_eq!(all.keys().map(String::as_str).collect::<Vec<_>>(), ["Golden hour", "Punchy"]);
        assert_eq!(all["Golden hour"], look);
        assert!(delete_preset_at(&file, "Punchy").unwrap());
        assert!(!delete_preset_at(&file, "Punchy").unwrap());
        assert_eq!(read_presets_at(&file).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Mid grey for the single-pixel direction checks.
    fn probe(adj: &Adjust, rgb: [f32; 3], local: [f32; 3], uv: [f32; 2]) -> [f32; 3] {
        let sat = Effects { saturation: 1.0 + adj.saturation / 100.0, ..Default::default() };
        adj.pixel(rgb, local, uv, &adj.base_fx(), &sat)
    }

    #[test]
    fn each_operator_moves_the_picture_the_way_it_says() {
        let mid = [0.5, 0.5, 0.5];
        let flat = [0.5, 0.0, 1.0];
        let c = [0.5, 0.5];
        let y = |a: &Adjust, v: f32| probe(a, [v, v, v], flat, c)[0];
        // One stop of light brightens, zero does not.
        assert!(y(&one("exposure", 1.0), 0.4) > 0.5);
        assert!(y(&one("contrast", 60.0), 0.8) > 0.85 && y(&one("contrast", 60.0), 0.2) < 0.15);
        // Zones: each slider moves its own tones and leaves the far end.
        assert!(y(&one("highlights", -80.0), 0.75) < 0.7 && (y(&one("highlights", -80.0), 0.2) - 0.2).abs() < 1e-3);
        assert!(y(&one("shadows", 80.0), 0.25) > 0.3 && (y(&one("shadows", 80.0), 0.8) - 0.8).abs() < 1e-3);
        assert!(y(&one("whites", -80.0), 0.95) < 0.85 && (y(&one("whites", -80.0), 0.3) - 0.3).abs() < 1e-3);
        assert!(y(&one("blacks", 80.0), 0.05) > 0.15 && (y(&one("blacks", 80.0), 0.7) - 0.7).abs() < 1e-3);

        let warm = probe(&one("temperature", 60.0), mid, flat, c);
        assert!(warm[0] > warm[1] && warm[1] > warm[2]);
        let magenta = probe(&one("tint", 60.0), mid, flat, c);
        assert!(magenta[1] < magenta[0]);

        // Vibrance lifts a muted colour more than a loud one; saturation doesn't care.
        let chroma = |v: [f32; 3]| v[0] - v[2];
        let (muted, loud) = ([0.55, 0.5, 0.45], [0.9, 0.5, 0.1]);
        let vib = one("vibrance", 60.0);
        let lift = |a: &Adjust, v| chroma(probe(a, v, flat, c)) / chroma(v);
        assert!(lift(&vib, muted) > lift(&vib, loud) + 0.3);
        assert!(lift(&one("saturation", -50.0), muted) < 0.6);

        // Clarity pushes a pixel away from its neighbourhood.
        let clar = one("clarity", 80.0);
        assert!(probe(&clar, [0.6; 3], [0.4, 0.0, 1.0], c)[0] > 0.6);
        assert!(probe(&clar, [0.4; 3], [0.6, 0.0, 1.0], c)[0] < 0.4);
        assert_eq!(probe(&clar, mid, flat, c), mid, "flat areas have nothing to enhance");

        // Dehaze deepens a veiled pixel; negative adds the veil back.
        let veil = [0.5, 0.45, 1.0];
        assert!(probe(&one("dehaze", 70.0), [0.5, 0.5, 0.6], veil, c)[0] < 0.5);
        assert!(probe(&one("dehaze", -70.0), [0.2; 3], veil, c)[0] > 0.2);

        // The vignette touches corners, never the centre.
        let dark = one("vignette", -80.0);
        assert_eq!(probe(&dark, mid, flat, c), mid);
        assert!(probe(&dark, mid, flat, [0.0, 0.0])[0] < 0.2);
        assert!(probe(&one("vignette", 80.0), mid, flat, [1.0, 1.0])[0] > 0.8);
    }

    #[test]
    fn the_tone_sliders_never_invert_the_ramp() {
        for (name, _, hi) in &SCALARS[2..6] {
            for sign in [1.0, -1.0] {
                let a = one(name, hi * sign);
                let mut last = -1.0f32;
                for i in 0..=255 {
                    let v = i as f32 / 255.0;
                    let y = probe(&a, [v; 3], [0.5, 0.0, 1.0], [0.5, 0.5])[0];
                    assert!(y >= last - 1e-3, "{name} {sign} inverted at {v}: {y} < {last}");
                    last = y;
                }
            }
        }
    }

    #[test]
    fn colour_bands_reach_their_colour_and_spare_the_rest() {
        let (flat, c) = ([0.5, 0.0, 1.0], [0.5, 0.5]);
        for (b, (name, centre)) in BANDS.iter().enumerate() {
            let mut a = Adjust::default();
            a.hsl[b].sat = -100.0;
            let at_centre = hsl_to_rgb([*centre, 0.7, 0.5]);
            let opposite = hsl_to_rgb([centre + 180.0, 0.7, 0.5]);
            let done = probe(&a, at_centre, flat, c);
            assert!(done[0].max(done[1]).max(done[2]) - done[0].min(done[1]).min(done[2]) < 0.02, "{name}");
            let spared = probe(&a, opposite, flat, c);
            for k in 0..3 {
                assert!((spared[k] - opposite[k]).abs() < 0.01, "{name} touched its opposite");
            }
            // Grey has no colour to push.
            a.hsl[b] = Band { hue: 100.0, sat: 100.0, lum: 100.0 };
            assert!((probe(&a, [0.5; 3], flat, c)[0] - 0.5).abs() < 1e-3, "{name} moved grey");
        }
        // Hue pushes turn the colour; luminance moves the lightness.
        let mut a = Adjust::default();
        a.hsl[5].hue = 100.0;
        let pushed = rgb_to_hsl(probe(&a, hsl_to_rgb([240.0, 0.7, 0.5]), flat, c));
        assert!((pushed[0] - 270.0).abs() < 1.0, "{pushed:?}");
        let mut a = Adjust::default();
        a.hsl[3].lum = 100.0;
        assert!(rgb_to_hsl(probe(&a, hsl_to_rgb([120.0, 0.7, 0.5]), flat, c))[2] > 0.7);
    }

    #[test]
    fn local_measurement_is_blind_to_resolution() {
        let (a, b) = (card(2000, 1500), card(1000, 750));
        let (ma, mb) = (LocalMap::measure(&a.data, 2000, 1500), LocalMap::measure(&b.data, 1000, 750));
        for uv in [[0.2, 0.3], [0.5, 0.5], [0.81, 0.77]] {
            let (x, y) = (ma.sample(uv), mb.sample(uv));
            for k in 0..3 {
                assert!((x[k] - y[k]).abs() < 0.01, "{uv:?}: {x:?} vs {y:?}");
            }
        }
    }

    #[test]
    fn old_documents_and_partial_settings_load() {
        let a: Adjust = serde_json::from_str(r#"{"contrast": 12}"#).unwrap();
        assert_eq!(a, Adjust { contrast: 12.0, ..Default::default() });
        let p: Project = serde_json::from_str(
            r#"{"name":"x","fps":30,"width":10,"height":10,"tracks":[]}"#,
        ).unwrap();
        assert!(p.photo.is_none());
    }
}

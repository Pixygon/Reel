//! 3D LUTs (.cube) — the film-look and log-conversion workhorse.
//!
//! A .cube file is a lattice of output colours indexed by input colour. Reel
//! samples it trilinearly on the GPU in BOTH pipelines (preview and frame
//! server), applied on sRGB-encoded values BEFORE the exposure/contrast/
//! saturation trims — the conventional order: conform the look first, trim
//! after. The no-GPU fallback maps to ffmpeg's `lut3d` filter.
//!
//! Parsed lattices are cached per (path, mtime) so scrubbing a LUT-graded
//! timeline never re-reads the file; textures are uploaded per device by
//! the pipelines that draw with them.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// A parsed lattice: `size³` RGB triples, red fastest — exactly the .cube
/// layout, which is also exactly wgpu's 3D-texture layout (x fastest).
pub struct Lut {
    pub size: u32,
    /// RGBA f32 (alpha 1.0), ready for a `Rgba32Float` 3D texture.
    pub data: Vec<f32>,
}

pub fn parse_cube(text: &str) -> Result<Lut> {
    let mut size = 0u32;
    let mut min = [0.0f32; 3];
    let mut max = [1.0f32; 3];
    let mut rows: Vec<[f32; 3]> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        match it.next() {
            Some("TITLE") | Some("LUT_1D_SIZE") if line.starts_with("LUT_1D") => {
                bail!("1D LUTs aren't supported (yet) — this needs a LUT_3D_SIZE cube")
            }
            Some("TITLE") => {}
            Some("LUT_3D_SIZE") => {
                size = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| anyhow!("bad LUT_3D_SIZE"))?;
            }
            Some("DOMAIN_MIN") => {
                for m in &mut min {
                    *m = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                }
            }
            Some("DOMAIN_MAX") => {
                for m in &mut max {
                    *m = it.next().and_then(|v| v.parse().ok()).unwrap_or(1.0);
                }
            }
            Some(first) => {
                // A data row: three floats.
                let r: f32 = first.parse().map_err(|_| anyhow!("bad LUT row: {line}"))?;
                let g: f32 = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| anyhow!("bad LUT row: {line}"))?;
                let b: f32 = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| anyhow!("bad LUT row: {line}"))?;
                rows.push([r, g, b]);
            }
            None => {}
        }
    }
    if size < 2 {
        bail!("no LUT_3D_SIZE in the file");
    }
    let expect = (size * size * size) as usize;
    if rows.len() != expect {
        bail!("LUT has {} rows, its size says {expect}", rows.len());
    }
    // Normalise a non-unit domain into 0..1 while building the RGBA buffer.
    let mut data = Vec::with_capacity(expect * 4);
    for [r, g, b] in rows {
        let n = |v: f32, lo: f32, hi: f32| {
            if (hi - lo).abs() > 1e-9 {
                (v - lo) / (hi - lo)
            } else {
                v
            }
        };
        data.push(n(r, min[0], max[0]));
        data.push(n(g, min[1], max[1]));
        data.push(n(b, min[2], max[2]));
        data.push(1.0);
    }
    Ok(Lut { size, data })
}

/// The identity lattice — bound whenever a draw has no LUT, so the shader
/// can sample unconditionally.
pub fn identity() -> Lut {
    let size = 2;
    let mut data = Vec::with_capacity(2 * 2 * 2 * 4);
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                data.extend_from_slice(&[r as f32, g as f32, b as f32, 1.0]);
            }
        }
    }
    Lut { size, data }
}

/// Process-wide parsed-LUT cache, keyed by path + mtime.
pub fn load(path: &str) -> Result<Arc<Lut>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (u64, Arc<Lut>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some((stamp, lut)) = cache.lock().unwrap().get(path) {
        if *stamp == mtime {
            return Ok(lut.clone());
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("could not read the LUT {path}: {e}"))?;
    let lut = Arc::new(parse_cube(&text)?);
    cache.lock().unwrap().insert(path.to_string(), (mtime, lut.clone()));
    Ok(lut)
}

/// f32 → f16 bits, for the filterable Rgba16Float 3D texture (Rgba32Float
/// isn't filterable without an extra device feature).
fn f32_to_f16_bits(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let frac = bits & 0x007f_ffff;
    if exp == 0xff {
        return sign | 0x7c00; // inf/nan → inf
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        return sign; // flush tiny to zero — LUT values live near 0..1
    }
    sign | ((e as u16) << 10) | ((frac >> 13) as u16)
}

/// Upload a lattice as a filterable 3D texture on `device`.
pub fn to_texture(lut: &Lut, device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let half: Vec<u16> = lut.data.iter().map(|v| f32_to_f16_bits(*v)).collect();
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("reel-lut"),
        size: wgpu::Extent3d {
            width: lut.size,
            height: lut.size,
            depth_or_array_layers: lut.size,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&half),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(lut.size * 8),
            rows_per_image: Some(lut.size),
        },
        wgpu::Extent3d {
            width: lut.size,
            height: lut.size,
            depth_or_array_layers: lut.size,
        },
    );
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

/// CPU reference application — trilinear, exactly what the shader computes.
/// The parity test drives the GPU against this.
#[cfg_attr(not(test), allow(dead_code))]
pub fn apply_reference(lut: &Lut, rgb: [f32; 3]) -> [f32; 3] {
    let n = lut.size as f32;
    let at = |x: u32, y: u32, z: u32, c: usize| -> f32 {
        let i = ((z * lut.size + y) * lut.size + x) as usize * 4 + c;
        lut.data[i]
    };
    let mut out = [0.0f32; 3];
    let pos: Vec<f32> = rgb.iter().map(|v| v.clamp(0.0, 1.0) * (n - 1.0)).collect();
    let base: Vec<u32> = pos.iter().map(|p| (p.floor() as u32).min(lut.size - 2)).collect();
    let frac: Vec<f32> = pos.iter().zip(&base).map(|(p, b)| p - *b as f32).collect();
    for (c, o) in out.iter_mut().enumerate() {
        let mut acc = 0.0;
        for dz in 0..2u32 {
            for dy in 0..2u32 {
                for dx in 0..2u32 {
                    let w = (if dx == 1 { frac[0] } else { 1.0 - frac[0] })
                        * (if dy == 1 { frac[1] } else { 1.0 - frac[1] })
                        * (if dz == 1 { frac[2] } else { 1.0 - frac[2] });
                    acc += w * at(base[0] + dx, base[1] + dy, base[2] + dz, c);
                }
            }
        }
        *o = acc;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(size: u32, f: impl Fn(f32, f32, f32) -> [f32; 3]) -> String {
        let mut s = format!("TITLE \"t\"\nLUT_3D_SIZE {size}\n");
        let n = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let o = f(r as f32 / n, g as f32 / n, b as f32 / n);
                    s.push_str(&format!("{} {} {}\n", o[0], o[1], o[2]));
                }
            }
        }
        s
    }

    #[test]
    fn parses_and_applies_the_lattice_it_was_given() {
        // Identity: every input comes back unchanged.
        let id = parse_cube(&cube(4, |r, g, b| [r, g, b])).unwrap();
        for probe in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.3, 0.6, 0.9], [0.5, 0.5, 0.5]] {
            let out = apply_reference(&id, probe);
            for c in 0..3 {
                assert!((out[c] - probe[c]).abs() < 1e-5, "{probe:?} → {out:?}");
            }
        }
        // Channel swap: r→g→b→r, exactly.
        let swap = parse_cube(&cube(3, |r, g, b| [b, r, g])).unwrap();
        let out = apply_reference(&swap, [1.0, 0.5, 0.0]);
        assert!((out[0] - 0.0).abs() < 1e-5);
        assert!((out[1] - 1.0).abs() < 1e-5);
        assert!((out[2] - 0.5).abs() < 1e-5);

        // Malformed files are refused, not guessed at.
        assert!(parse_cube("LUT_3D_SIZE 3\n0 0 0\n").is_err(), "wrong row count");
        assert!(parse_cube("0 0 0\n1 1 1\n").is_err(), "no size");
    }

    #[test]
    fn a_non_unit_domain_is_normalised() {
        let mut text = String::from("LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!("{} {} {}\n", r * 2, g * 2, b * 2));
                }
            }
        }
        let lut = parse_cube(&text).unwrap();
        let out = apply_reference(&lut, [1.0, 1.0, 1.0]);
        assert!(out.iter().all(|v| (v - 1.0).abs() < 1e-5), "domain not normalised: {out:?}");
    }
}

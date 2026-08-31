//! WGSL effect plugins — community effects as single files.
//!
//! A plugin is one `.wgsl` file that defines
//!
//! ```wgsl
//! //! name: Vignette
//! //! param: strength 0..1 = 0.6
//! //! param: radius   0..2 = 0.7
//! fn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32> { … }
//! ```
//!
//! The function runs in BOTH pipelines (preview and frame server) on
//! sRGB-ENCODED values — the same convention as every built-in effect —
//! after the grade lattice and before the trims. `uv` is the sampled
//! position (0..1), `p` carries up to four user parameters (declared in
//! the header, sliders in the UI). Files live in `~/.config/reel/effects`
//! and hot-reload: the cache key includes the mtime, so saving the file
//! re-compiles the pipeline on the next frame.
//!
//! A plugin that fails to compile falls back to the built-in pipeline and
//! logs — a broken community file must never take the render down.

use anyhow::{anyhow, Result};
use std::sync::{Arc, Mutex, OnceLock};

/// One declared parameter: a labelled slider.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDecl {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

pub struct Plugin {
    pub name: String,
    pub params: Vec<ParamDecl>,
    /// The full WGSL source (header comments included — WGSL ignores them).
    pub source: String,
    /// Cache key: path + mtime, so an edited file is a different plugin.
    pub key: u64,
}

/// Where user plugins live.
pub fn plugin_dir() -> std::path::PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        })
        .join("reel/effects")
}

/// First-run seeding: put the bundled example effects where the picker
/// and the docs point, so the feature is discoverable without a download.
/// Never overwrites — the user's edits win.
pub fn seed_examples() {
    let dir = plugin_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, src) in [
        ("vignette.wgsl", include_str!("../examples/effects/vignette.wgsl")),
        ("posterize.wgsl", include_str!("../examples/effects/posterize.wgsl")),
        ("invert.wgsl", include_str!("../examples/effects/invert.wgsl")),
    ] {
        let p = dir.join(name);
        if !p.exists() {
            let _ = std::fs::write(&p, src);
        }
    }
}

/// Parse the header + validate the contract. Pure; unit-tested.
pub fn parse(source: &str) -> Result<(String, Vec<ParamDecl>)> {
    if !source.contains("fn plugin(") {
        return Err(anyhow!(
            "a Reel effect must define `fn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32>`"
        ));
    }
    let mut name = String::from("Effect");
    let mut params = Vec::new();
    for line in source.lines() {
        let Some(rest) = line.trim().strip_prefix("//!") else { continue };
        let rest = rest.trim();
        if let Some(n) = rest.strip_prefix("name:") {
            name = n.trim().to_string();
        } else if let Some(p) = rest.strip_prefix("param:") {
            // "strength 0..1 = 0.6"
            let mut it = p.split_whitespace();
            let pname = it.next().unwrap_or("param").to_string();
            let range = it.next().unwrap_or("0..1");
            let (lo, hi) = range
                .split_once("..")
                .and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))
                .unwrap_or((0.0, 1.0));
            let default = it
                .skip_while(|t| *t == "=")
                .next()
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or((lo + hi) * 0.5);
            if params.len() < 4 {
                params.push(ParamDecl { name: pname, min: lo, max: hi.max(lo + 1e-6), default });
            }
        }
    }
    Ok((name, params))
}

/// Load a plugin, cached by path + mtime (hot reload for free).
pub fn load(path: &str) -> Result<Arc<Plugin>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, (u64, Arc<Plugin>)>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some((stamp, p)) = cache.lock().unwrap().get(path) {
        if *stamp == mtime {
            return Ok(p.clone());
        }
    }
    let source = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("could not read the effect {path}: {e}"))?;
    let (name, params) = parse(&source)?;
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.as_bytes().iter().chain(mtime.to_le_bytes().iter()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let plugin = Arc::new(Plugin { name, params, source, key: h });
    cache.lock().unwrap().insert(path.to_string(), (mtime, plugin.clone()));
    Ok(plugin)
}

/// The identity plugin line in the base shaders — replaced by the active
/// plugin's source when one is bound.
pub const SLOT: &str =
    "fn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32> { return rgb; }";

/// Splice a plugin into a base shader source.
pub fn splice(base: &str, plugin: &Plugin) -> String {
    base.replace(SLOT, &plugin.source)
}

/// Build a pipeline variant for `plugin` against a base shader, catching
/// validation errors so a broken file degrades to the identity pipeline
/// instead of panicking the process.
#[allow(clippy::too_many_arguments)]
pub fn build_variant(
    device: &wgpu::Device,
    base_source: &str,
    plugin: &Plugin,
    label: &str,
    layout: &wgpu::PipelineLayout,
    target: wgpu::TextureFormat,
    blend: wgpu::BlendState,
) -> Option<wgpu::RenderPipeline> {
    let source = splice(base_source, plugin);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });
    match pollster::block_on(device.pop_error_scope()) {
        None => Some(pipeline),
        Some(e) => {
            log::warn!("effect plugin '{}' failed to compile — using the built-in pipeline: {e}", plugin.name);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_parse_and_contracts_are_enforced() {
        let src = "//! name: Vignette\n//! param: strength 0..1 = 0.6\n//! param: radius 0.2..2 = 0.7\nfn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32> { return rgb; }\n";
        let (name, params) = parse(src).expect("parse");
        assert_eq!(name, "Vignette");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "strength");
        assert!((params[0].default - 0.6).abs() < 1e-6);
        assert!((params[1].min - 0.2).abs() < 1e-6 && (params[1].max - 2.0).abs() < 1e-6);

        // No plugin fn = refused with a message that teaches the contract.
        let err = parse("fn frag() {}").unwrap_err().to_string();
        assert!(err.contains("fn plugin("), "{err}");

        // Splicing replaces exactly the identity slot.
        let base = format!("HEADER\n{SLOT}\nFOOTER");
        let plugin = Plugin { name: name.clone(), params, source: "fn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32> { return vec3<f32>(1.0) - rgb; }".into(), key: 1 };
        let out = splice(&base, &plugin);
        assert!(out.contains("vec3<f32>(1.0) - rgb"));
        assert!(!out.contains("return rgb; }"), "identity slot must be gone");
    }
}

# Effect plugins

A Reel effect is **one WGSL file**. Drop it in `~/.config/reel/effects/`
(or anywhere — the picker takes any path), apply it to a clip, and it runs
**identically in the live preview and the render** — same code, same GPU.

```wgsl
//! name: Vignette
//! param: strength 0..1 = 0.6
//! param: radius 0.2..1.5 = 0.75
fn plugin(rgb: vec3<f32>, uv: vec2<f32>, p: vec4<f32>) -> vec3<f32> {
    let d = distance(uv, vec2<f32>(0.5, 0.5)) / max(p.y, 0.01);
    return rgb * (1.0 - smoothstep(0.6, 1.2, d) * p.x);
}
```

The contract:

- `rgb` is **sRGB-encoded** (0..1) — the same convention every built-in
  effect uses. Return sRGB-encoded.
- `uv` is the sampled position, 0..1 across the picture.
- `p` carries up to four user parameters. Declare them with
  `//! param: <name> <min>..<max> = <default>` and they become labelled
  sliders in the clip panel (and `--plugin-params` on the CLI). Each
  slider is keyframable like any other clip property — key it at the
  playhead and the value animates through the same engine in the preview
  and the render.
- The plugin runs **after the grade** (LUT/levels/curves) and **before the
  trims** (exposure/contrast/saturation).

Apply one:

```bash
reel effects cut.reel --clip 100 --plugin vignette.wgsl
reel effects cut.reel --clip 100 --plugin-params 0.8,0.6
```

Editing the file **hot-reloads**: save it and the next frame recompiles.
A plugin that fails to compile logs the error and the clip renders with
the built-in look — a broken file never takes the render down.

The graph fallback (no-GPU rendering, `REEL_RENDER=graph`) cannot run WGSL
and warns that plugins are dropped there.

Examples to start from live in `examples/effects/` in the repo.

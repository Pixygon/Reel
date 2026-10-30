
- PICTURE STAGE: `src/picture.wgsl` is ONE text spliced in front of both
  video.wgsl and compose.wgsl (`VIDEO_WGSL`/`COMPOSE_WGSL`) — blur, split,
  glitch, rotation and tile build are shared by construction, and
  `Effects::picture_uniforms` fills the three vec4s for both. Per-frame state
  (shake window, glitch phase, build progress) comes from `Effects::at_time`,
  called inside every `animated()`. Shake is zoom + pan, so no new shader.
  `engine/picture_tests.rs` draws both real pipelines and compares pixels; with
  no adapter it SKIPS, so a sandbox without /dev/dri proves nothing. Run it
  there on Mesa's software GL with a temporary `Backends::from_env()` and
  `WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 EGL_PLATFORM=surfaceless`.
- Plates (`plates.rs`) are real cached PNGs; `Clip.plate` is the document and
  `Project::load` rebuilds the file. Variants (`edit/variants.rs`) resolve into
  a plain Project at render time, so no renderer knows they exist; a base plate
  is regenerated at the variant size, an overlay plate keeps its shape.
- Overlay layers honour source alpha (`use_src_alpha: true`): `format=rgba`
  gives opaque video 255, so PNG alpha survives. `SegmentReader` seeks a still
  to 0 — `-ss` past its one frame decodes nothing. The live inset preview is
  still opaque for stills (mpv's padding byte).

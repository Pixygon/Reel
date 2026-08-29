# Reel

A cross-platform **media player, editor and capture tool** — video, audio and
images through one door. Linux-first, native.

The bar is deliberately high: **better than VLC to play a file, at the level of
Premiere to edit one.** This repo is the running foundation that aims there —
not the finished tool, but real, honest software you can build and run today.

## What works

- **Plays video — through libmpv when present.** Open a file (Open… dialog,
  drag-and-drop, or `reel <path>`) and it plays immediately, aspect-fit.
  With libmpv (the hot path, auto-detected at runtime): hardware
  decode, correct colour conversion, **audio with real A/V sync**, subtitles,
  and **frame-exact seek + live scrubbing**. Without it, the v0.1
  ffmpeg-subprocess decoder still works everywhere (keyframe seek, video only).
- **A real player's controls.** Play/pause, frame step, jump ±5 s/±60 s,
  volume/mute, 0.25–4× speed, loop, fullscreen — all on mpv/VLC-style
  keyboard shortcuts (Space, ←/→, ,/., ↑/↓, M, L, F, [ ], E for editor).
- **Plays audio and shows images too — one door for all media.** Audio files
  play through the same transport, with **audio visualizers rendered by the
  playback engine itself** — musical spectrum bars, scrolling spectrogram,
  vectorscope, waveform (V cycles; cover art shows when embedded). Images
  open instantly through the same GPU path — ultrawide screenshots, 8K
  stills, **and SVG, rasterized crisply via resvg**. Everything lands on the
  editor timeline (video/stills on V1, audio on A1).
- **An editor that finishes the job.** Trim by dragging clip edges, split at
  the playhead, move with snapping, undo/redo, set in/out markers, drop in
  more source files, give clips a **look** (exposure/contrast/saturation and
  fades, previewed live and rendered identically) — then **export the edit
  itself**, or hit a **one-click destination**: YouTube, TikTok, Reels,
  Instagram, Facebook, X. Mixed resolutions and
  codecs are normalised automatically, and the render uses your **GPU
  encoder** (NVENC/VideoToolbox) when there is one.
- **Captions in one button — and they never leave your machine.** Press
  **✦ Generate captions** and Reel transcribes the speech locally, places
  the lines on the timeline, previews them exactly where they will burn in,
  and burns them into the export. There is nothing to install and no account
  to make: the speech engine and model are fetched automatically the first
  time (~85 MB, once), then it works offline, on a plane, for free, with no
  per-minute billing and nothing uploaded. Cues follow your edit — a line
  spanning a cut appears in both halves, and speech you trimmed away
  captions nowhere.
- **4K edits like 720p.** Heavy sources get an automatic background editing
  proxy the preview scrubs instead — while exports, waveforms, thumbnails
  and captions always use the original. Nothing to configure, nothing to
  relink.
- **HDR footage just looks right.** Phone clips in HLG or HDR10 (PQ) are
  detected and tone-mapped to SDR through libplacebo at the industry's
  203-nit reference — no washed-out grays, no murky darks, nothing to
  configure.
- **Reel's own render engine.** Timeline exports are drawn by Reel's GPU
  compositor frame by frame — ffmpeg only encodes — so what the preview
  shows is what renders, by construction. The old filter-graph renderer
  remains as the automatic no-GPU fallback.
- **Keyframe animation, with a curve editor.** Animate exposure, contrast,
  saturation, zoom/pan, PiP placement — and **speed itself** (ramps: the
  clip's slot stays put while playback accelerates through the footage,
  audio tempo following). Set keys at the playhead or with `reel keyframe`,
  drag them on the curve, scrub to watch it play; the render evaluates the
  same curves per frame. Linear, hold and eased interpolation.
- **A second video track — picture-in-picture.** Drop a clip on the overlay
  track; the inset **plays live in the preview**, and you drag it where you
  want it. Position and size are fractions of the frame, so what you place
  is what renders at any resolution.
- **Speed.** Any clip, 0.25× to 4×, with the audio pitched to match. The
  clip's slot on the timeline resizes to suit, so the rest of the cut stays
  where you put it. (Constant speed per clip — *ramps* that accelerate
  through a shot aren't in yet.)
- **Waveforms on every clip.** The audio is drawn right on the timeline, so
  you cut on a word instead of hunting for it. Peaks are decoded in the
  background and cached per source, so splitting, trimming, moving or
  duplicating a clip never recomputes them.
- **The edits you make constantly.** Copy, paste and duplicate (Ctrl+C/V/D —
  pasting *inserts*, so it can never silently eat footage you already
  placed), plus markers (Ctrl+M) you can jump between with Ctrl+←/→.
- **Titles you place by dragging.** Add text, drag it where you want it on
  the picture, set size and colour. Position is stored as a fraction of the
  frame, so a title composed on the preview lands in exactly the same place
  in a 4K export.
- **Green screen, built in.** Check "Green screen" on a clip, pick the
  colour, and the key previews live — in the PiP inset too — and renders
  identically: chroma-weighted matte, soft edge, automatic despill.
- **Roll, slip and slide.** The trims professionals actually cut with:
  Ctrl-drag an edge to move a cut without moving the timeline, Alt-drag to
  choose what plays without moving when, Ctrl+Alt to slide between
  neighbours. Also `reel roll/slip/slide` for scripts.
- **Scopes, chapters, stills.** A live histogram and waveform while you
  grade; markers become real MP4 chapters on export; and one button saves
  the composed frame under the playhead as a PNG.
- **Grade through LUTs.** Load any .cube 3D LUT per clip — sampled on the
  GPU, previewed exactly as rendered, applied before your trims, and never
  colouring the letterbox bars.
- **Stabilization that provably works.** One checkbox (or
  `reel stabilize`): two-pass camera-shake smoothing, analysis cached, and
  a test that measures the shake energy actually halving.
- **A real transition library.** Crossfade, dip-to-black, wipes and slides —
  picked per cut, previewed live with the same geometry the render uses.
- **Delivered at the platform's loudness.** Social presets normalize the
  finished mix to −14 LUFS (music and ducking included), so your upload
  doesn't get squashed or boosted by the platform.
- **Multicam sync without clap sticks.** `reel align` lines two takes up by
  their audio alone.
- **One click removes the dead air.** ✂ Tighten (or `reel tighten`) finds
  every silence in the edit, cuts it with breathing room around your words,
  and closes the timeline up — the podcast jump-cut pass, undoable.
- **You hear the whole edit.** Editor playback mixes the timeline live —
  every clip's audio with its gain and fades, and the music bed ducking
  under speech in real time — through a native PipeWire stream on Linux.
  What was once export-only is now what your ears get while you cut.
- **Sound that behaves.** A volume trim per clip, plus a music bed under the
  whole edit that **ducks under speech automatically** — no volume curves to
  draw, no keyframes to place. Verified by measuring the music's own
  frequency band in a real render, not by eye.
- **Convert without editing — the HandBrake seam.** Hit **⬇ Export** on
  anything open: video → H.264/H.265/AV1/VP9 with quality presets (or custom
  CRF), downscale, audio bitrate/copy, instant lossless MKV remux — or
  **extract the audio** to MP3/M4A/Opus/FLAC/WAV. Audio sources convert
  between those formats; images convert to PNG/JPEG/WebP with resize. Live
  progress + cancel; runs on the system ffmpeg.
- **Opens from your file manager — and sets itself up.** Reel registers an
  "Open with" entry on first launch and asks (once) whether to become the
  default for Video / Music / Images — checkboxes, your pick, changeable
  under ⚙ → Default apps. The Arch package pulls in mpv + ffmpeg as
  dependencies and installs the desktop entry system-wide; the site's
  installer offers the codecs on other distros. The app is meant to be
  entered by double-clicking media, not launched bare.
- **A player-shaped player.** No toolbars over your media: the video fills
  the window, the seek bar runs edge to edge, the transport sits centered
  beneath it, and the whole control strip fades away (cursor included) after
  a couple of idle seconds during playback. Transparent images render
  properly (premultiplied alpha) over a viewer checkerboard.
- **Capture the screen — from the system tray.** Screenshot (full/region/
  window) and screen recording live in Reel's tray icon, reachable even with
  the window buried — and the result opens right in Reel, ready to trim,
  convert or export. Recording
  needs **no external tools**: Reel speaks xdg-desktop-portal + PipeWire
  directly (the same door OBS uses), so the system's own picker chooses
  screen/window/region, system audio is captured when a monitor source
  exists, and the portal's restore token skips the dialog after first
  approval. Native desktop tools (spectacle, grim, gpu-screen-recorder, …)
  are used opportunistically where they're better; ffmpeg gdigrab covers
  Windows — and if ffmpeg itself is missing, Reel downloads a private
  static build on first launch.
- **Projects are files.** Ctrl+S saves the cut as a `.reel` document;
  double-clicking one reopens the edit with its media loaded at the playhead.
- **Native Pixygon stack.** winit + wgpu + egui — the same stack as Infinite —
  themed in the Pixygon master-brand voice (void grounds, signal cyan, ember).
- **One binary, three platforms.** Builds to Linux + Windows via `pearl build`
  (macOS once the SDK is seeded), same as the rest of the estate's native apps.

## Drive it from the command line

Reel is two programs in one binary. `reel <file>` opens the player; the rest
runs **headless** — no window, no display — so a script, a CI job or an agent
can edit video with it:

```bash
reel new cut.reel --size 1080x1920 --fps 30
reel add cut.reel a.mp4 --in 2 --duration 5     # 5s of a.mp4, from 0:02
reel add cut.reel b.mp4
reel captions cut.reel                          # transcribed on this machine
reel title add cut.reel --text "Hello" --at 0 --duration 3
reel music set cut.reel bed.mp3 --gain-db -14   # ducks under the speech
reel render cut.reel out.mp4 --preset tiktok
```

Every command takes `--json` (one object on stdout, logs on stderr, non-zero
exit on failure), and `reel commands --json` describes the whole interface —
generated from the same table that parses the arguments, so it can't go stale.

See [AGENTS.md](AGENTS.md) to get going in a minute, or
[docs/CLI.md](docs/CLI.md) for the full reference.

## Build & run

```bash
cargo run --release -- /path/to/video.mp4     # open a file directly
cargo run --release                            # or open from the UI
cargo test                                     # decode-pipeline tests
```

Requires a GPU with Vulkan/Metal/DX12. **libmpv** (`mpv` package) is strongly
recommended — it is the real playback engine when found (dlopen'd at runtime,
never linked). A system **ffmpeg** on `PATH` is the universal fallback and is
what `cargo test`'s decode-pipeline tests exercise. `REEL_BACKEND=ffmpeg`
forces the fallback.

## Architecture

```
src/
├── main.rs         winit 0.30 app + run loop; clears the swapchain, egui draws over it
├── gpu.rs          wgpu context + VideoTexture (RGBA frame → GPU)
├── egui_backend.rs egui-wgpu integration (egui draws the whole UI incl. the video)
├── video/
│   ├── player.rs   the stable playback API (play/pause/seek/update) over two backends
│   ├── mpv.rs      libmpv backend (dlopen'd): hw decode, A/V sync, audio, exact seek
│   └── decoder.rs  fallback: ffmpeg subprocess → raw RGBA frames over a bounded channel
├── media.rs        media kinds + instant still-image documents (image crate, resvg for SVG)
├── capture.rs      screenshots + recording dispatch (modes, tool tiers)
├── portal.rs       built-in Linux capture: xdg-desktop-portal + PipeWire → ffmpeg encode
├── edit/           the NLE model — Project / Track / Clip (serde-serializable → a .reel doc)
├── effects.rs      per-clip look: ONE formula shared by the preview shader and the render
├── video_pass.rs   Reel's wgpu pipeline for the picture (+ video.wgsl) — the compositing seam
├── export.rs       convert/export engine + social presets (ffmpeg, live progress, cancel)
├── ui.rs           player transport, editor timeline, export dialog, shortcuts (all egui)
└── theme.rs        Pixygon design tokens → egui visuals
```

### Why this stack

Every serious NLE (Resolve, Premiere, Final Cut) is native, not a web wrapper —
because beating VLC on playback needs a **zero-copy GPU video surface**, which a
web renderer can't give you. wgpu+egui gives us that surface and a fast UI in one
language, and slots straight into `pearl build`'s cross-platform pipeline.

Playback now runs on **libmpv** where available — hardware decode
(VA-API/D3D11VA, copy-back for now), mpv's colour conversion, audio out with
real A/V sync, subtitles, frame-exact seek — behind the same `Player` API the
v0.1 subprocess decoder sits under, which remains the universal fallback.
Current seam: mpv's software render target (mpv decodes and converts, Reel
uploads RGBA to wgpu). The [ROADMAP](ROADMAP.md)'s next step keeps the frame
on the GPU end-to-end (render API GL/Vulkan interop, libplacebo-class output).

## Status

A capable daily driver for playing, converting, capturing — and editing:
multi-track cutting with ripple tools, effects, crossfades, captions,
titles, music with ducking, PiP, per-clip speed, waveforms and thumbnails,
plus a complete headless CLI. The frontier is the engine rework — a GPU
compositor with live multi-stream preview and frame-server rendering — that
unlocks keyframes, ramps and everything beyond. [ROADMAP.md](ROADMAP.md) is
the full, ordered map to the stated bar: replace Premiere, make a DAW
unnecessary for video work, rival Photoshop for what creators actually do.

**Download & docs: [reel.pixygon.io](https://reel.pixygon.io)** — one-line
Linux install, Arch repo, Windows zip.

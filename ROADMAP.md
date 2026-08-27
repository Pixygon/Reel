# Reel — road to the bar

Two targets, stated plainly: **playback better than VLC**, **editing at the
level of Premiere**. Neither is a v0.1 claim; this is the sequence that gets
there. Linux is the reference platform; Windows/macOS follow each milestone.

## Where v0.1 stands

Plays and scrubs video (ffmpeg-subprocess decode → wgpu texture → egui), and
renders the NLE timeline from a real Project/Track/Clip model. Honest
foundation, narrow feature set.

## Milestone 1 — playback that earns "better than VLC"

The subprocess decoder is the v0.1 crutch; the performance bar needs the frame
never leaving the GPU.

- [ ] Replace subprocess decode with **libmpv** (render API) or **libav + libplacebo**
      as the hot path — behind the existing `video::Player` API, so the UI is untouched.
- [ ] **Hardware decode** (VA-API on Linux, D3D11VA/VideoToolbox elsewhere) → GPU
      frames with zero CPU copy.
- [ ] **libplacebo-class rendering**: correct colour management, HDR tone-mapping,
      high-bit-depth, debanding — the things that actually make it look better than VLC.
- [ ] Frame-accurate seek (decode from keyframe, step to exact frame), gapless A/V sync.
- [ ] Audio out (device + passthrough), subtitle rendering, track selection.

## Milestone 2 — an editor you'd actually cut in

- [ ] Trim handles on clips; drag to move; snapping; ripple/roll.
- [ ] Multi-track compositing on the GPU (blend, opacity, transform).
- [ ] Playhead scrub renders the **composited timeline**, not just one source clip.
- [ ] Cut/copy/paste, undo/redo (command history over the serde model).
- [ ] Save/open `.reel` project documents (the model is already serde-ready).

## Milestone 3 — finish & polish

- [ ] **Export/encode** via libav (H.264/H.265/AV1, ProRes), with a render queue.
- [ ] Effects/transitions (GPU shaders), a basic colour panel, audio levels/mixer.
- [ ] Native file dialogs (rfd), drag-and-drop import, thumbnails/waveforms.
- [ ] Proxy workflow for heavy media; background conform.

## Milestone 4 — the Pixygon seam

- [ ] Publish exports straight to Bunny CDN / a pearl's media pipeline.
- [ ] Optional PixygonAPI sign-in; project sync.
- [ ] `pearl build` release cadence (Linux + Windows now, macOS once the SDK is seeded).

## Non-negotiables

- The frame stays on the GPU. Every architecture choice defers to that.
- One binary per platform, built here → CDN. No web runtime in the hot path.
- The `Player` API stays stable as the decode backend is swapped underneath it.

# Reel — the refinement roadmap (1.x)

**1.0 shipped 2026-08-29.** The player, the engine (GPU compositor +
frame-server renderer), the editor (trim family, keyframes, ramps,
transitions, multi-select, tighten), the grade (LUTs, curves, power
windows, chroma key, scopes, stabilization), the sound (live PipeWire
mixer, music with ducking, mixer strip, loudness delivery), captions and
titles (local, editable, resolution-exact), delivery (presets,
publish-everywhere, chapters, stills), a 31-verb CLI whose manual is its
parser, proxies, HDR tone-mapping, multicam sync — MIT, public, tested
against real output throughout. `docs/AUDIT-1.0.md` records the 1.0 audit;
this document is what comes next.

The constitution from the first roadmap still governs: local-first
forever; the preview never lies; never lose work; measured, not guessed;
CLI parity; tests exercise the real thing; honest docs. The budgets
stand (and were re-measured at 1.0: cold open 60 ms/745 ms; 1800 clips in
64 ms; kitchen-sink export at 1.7x realtime).

Ordering inside each theme is priority. Debts from the audit come first —
refinement starts by paying what we owe.

---

## A. One truth of time

The deepest 1.x item. The editor timeline keeps gaps and uncollapsed
transition overlaps; render time collapses both. Today the scrubber can
read `10.00 / 8.00` at the end of a transitioned edit.

- [x] The timebase module (`edit_spans` / `timeline_to_edit` /
      `edit_to_timeline`): totals equal `render_duration` to the bit, gaps
      collapse, the overlap belongs to the incoming clip, round trips
      tested. (The static map is honestly two-sheeted around transitions —
      a's tail and b's head are sequential on the timeline but simultaneous
      in the edit; what is monotone is the PLAYBACK PATH.)
- [x] The scrubber and the time readout speak edit time; seeks map back
      through the inverse. `10.00 / 8.00` is gone.
- [x] Playback resumes the incoming clip PAST a transition's head (it
      already played, blended over the outgoing tail) — edit time flows
      continuously through a fade, tested as path continuity.
- [x] The transport speed control shows the USER's rate again; the active
      clip's own rate multiplies underneath and shows as a badge.
- [ ] Markers/captions/titles authored against edit time survive
      re-timing edits upstream of them (anchor to clips where possible).

## B. Preview honesty debts

- [ ] `render_still` composes transitions (a mid-fade frame export shows
      the blend) and burns captions/titles like the full render.
- [ ] The graph fallback warns about EVERYTHING it drops (masks, LUTs,
      curves, chroma key, chapters) — one generalized capability check,
      not per-feature warnings.
- [ ] `captions --source` says plainly when zero cues mapped, and why.
- [ ] Pitch-preserving preview of speed-changed audio (a small
      time-stretcher in the mixer; the render already uses atempo).
- [ ] A proxy badge in the preview — you should know when you are looking
      at the 720p editing copy.
- [ ] Stabilization preview: at minimum, a one-click "render this clip's
      window and play it back" loop so the smoothing can be judged without
      a full export.

## C. Performance refinements

- [ ] PiP pool players render at inset size (`set_display_size`), not
      source size — a quarter-frame inset should cost a quarter-frame
      decode.
- [ ] Double-buffered readback in the frame server (overlap GPU-to-CPU
      copy with the next composite; today each frame round-trips
      serially).
- [ ] Grade-lattice and LUT texture caches evict (session-bounded LRU).
- [ ] Waveform/thumbnail/PCM caches persist across sessions (disk cache
      beside proxies) so a reopened project is instantly dressed.
- [ ] A `reel bench` verb: cold open, scrub latency, export speed measured
      on this machine, printed as a table — the public-benchmark seed.

## D. Grading depth

- [ ] Levels (black/white points + gamma) and white balance (temp/tint) —
      same lattice bake, so they stay one texture sample.
- [ ] HSL qualifiers (select by hue/sat/luma range, then push) — the
      secondary-correction half of a color page.
- [ ] Bezier handles on the keyframe curve editor; the full-width timeline
      curve lane.
- [ ] **Auto tracking for power windows**: template-match a region across
      frames, write mask-x/y keyframes (the mask is already animatable —
      tracking is a keyframe generator).
- [ ] Vectorscope and RGB parade in the scopes panel.
- [ ] Grade copy/paste between clips; a project-level "look" applied to
      many clips at once.

## E. Audio depth

- [ ] Pan per track/clip (live + export).
- [ ] Meters on the mixer strips; a LUFS meter on the master.
- [ ] Parametric EQ and a compressor as insert effects — measured tests,
      like the ducker.
- [ ] The repair set: noise reduction, de-hum, de-click ("Fix voice").
- [ ] Filler-word removal driven by word-level caption timestamps.
- [ ] Beat detection → beat-snapped cut points.
- [ ] Voice recording straight into A1, with punch-in.

## F. Editorial workflow

- [ ] **Media pool**: bins, thumbnails, metadata, search; relink for moved
      files; offline placeholders.
- [ ] **Source monitor**: preview any pool item, set in/out, three-point
      edit into the timeline.
- [ ] **Multicam cutting**: the sync half shipped (`reel align`); the
      cutting half is a live angle viewer with number-key switching.
- [ ] Compound clips (nest a sequence); adjustment layers.
- [ ] Drag-lasso multi-select; track targeting for paste.
- [ ] A keyboard-map overview (press `?`) — the shortcuts have outgrown
      the hint text.

## G. Delivery refinements

- [ ] Per-output caption burn-in toggles and filename templating in
      publish-everywhere.
- [ ] YouTube chapter text export (the markers already become MP4 atoms).
- [ ] `reel render --watch` (hot-render a project on change).
- [ ] HDR-to-HDR passthrough export for the codecs that carry it.

## H. Platform

- [ ] **macOS**: the build (VideoToolbox probing exists; ScreenCaptureKit;
      notarized dmg via the self-hosted runner).
- [ ] Windows capture backend (Windows.Graphics.Capture + WASAPI).
- [ ] Flatpak and AppImage; Windows file associations + installer.
- [ ] Lift the mixer's Linux gate once the cross-build story for a
      Windows audio backend is settled.
- [ ] Wayland file-drop the moment winit lands it (upstream).

## I. 1.x hygiene

- [ ] Reconcile the Pixygon changelog server's version (0.40.0) with the
      shipped 1.0.0 before the next `pearl ship`.
- [ ] Sponsors button live once the org toggle is flipped; donation link
      on the site.
- [ ] CONTRIBUTING.md + labeled starter issues; CI running the full suite
      (Xvfb visual checks included) on PRs.
- [ ] Crash recovery (restore exact editor state) and named project
      snapshots — "never lose work" still has these two IOUs.
- [ ] Localization scaffolding; AccessKit wiring; UI scale setting.

---

The grand arc beyond 1.x — image editing that rivals Photoshop, the full
DAW ambition, plugins, `reel serve`/MCP — lives on in the original phased
plan (git history of this file, and the phases 5–7 sections it carried).
Nothing there is abandoned; 1.x is the season of making what shipped
*excellent* before widening again. When reality disagrees with this
document, reality wins — and this document gets edited.

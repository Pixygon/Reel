# Reel from the command line

Reel is two programs in one binary. `reel <file>` opens the player; every
command below runs headless, with no window and no display — which is what
makes Reel drivable by a script, a CI job, or an agent.

A `.reel` project is plain JSON: a list of clips, plus titles, captions, a
music bed and markers. The whole workflow is therefore *make a project, do
things to it, render it* — and each of those is one command.

## The short version

```bash
reel new cut.reel --size 1920x1080 --fps 30
reel add cut.reel intro.mp4 --in 2 --duration 5   # 5s of intro.mp4, from 0:02
reel add cut.reel main.mp4
reel captions cut.reel                            # transcribed on this machine
reel title add cut.reel --text "Hello" --at 0 --duration 3
reel music set cut.reel bed.mp3 --gain-db -14     # ducks under speech
reel render cut.reel out.mp4 --preset tiktok
```

## Rules that make it safe to automate

- **Every command takes `--json`.** You get one object on stdout and nothing
  else; progress and logs go to stderr. Failures are also JSON
  (`{"ok": false, "error": "..."}`) and always exit non-zero.
- **Ids, not indexes.** `reel inspect` gives every clip a stable `id`. Pass
  that to `trim`, `move`, `remove`, `gain`, `effects` and `transition`. Ids
  survive edits elsewhere in the timeline; positions don't.
- **Unknown flags are errors.** A typo is refused rather than ignored, so you
  never silently render something other than what you asked for.
- **Nothing opens a window.** A mistyped command exits 2 with a message
  instead of launching a GUI.
- **`reel commands --json`** is this document in machine-readable form,
  generated from the same table that parses the arguments — so it cannot go
  out of date. Read it at runtime rather than hard-coding this page.

## Times, positions and sizes

- All times are **seconds** (floats).
- Clip times come in two flavours: `start` is where the clip sits on the
  **timeline**; `in_point` is where it begins inside the **source file**.
  `--at` sets the former, `--in` the latter.
- Title positions and sizes are **fractions of the frame** (`--x 0.5` is the
  centre, `--size 0.09` is nine percent of the frame height). This is why a
  title composed at 720p lands identically in a 4K render.

## Commands

### `reel serve`

Long-lived JSON-RPC session on stdio: every verb, no process-per-command. One message per line


### `reel mcp`

Model Context Protocol server on stdio — agents drive Reel as native tools


### `reel schema`

The .reel document's JSON Schema — generated from the live types, versioned with the app

| Flag | Value | Meaning |
| --- | --- | --- |
| `--json` | — | Print the result as JSON |

### `reel info MEDIA`

Duration, frame size and rate of a media file

### `reel new PROJECT`

Create an empty .reel project

| Flag | Value | Meaning |
| --- | --- | --- |
| `--name` | `TEXT` | Project name |
| `--size` | `WxH` | Frame size, e.g. 1920x1080 |
| `--fps` | `N` | Frame rate |

### `reel inspect PROJECT`

The whole project — clips with their ids, titles, captions, music, markers

### `reel add PROJECT MEDIA`

Append a piece of media to the timeline

| Flag | Value | Meaning |
| --- | --- | --- |
| `--at` | `SECONDS` | Timeline position (default: after the last clip) |
| `--in` | `SECONDS` | Start point inside the source (default 0) |
| `--duration` | `SECONDS` | How much of the source to use (default: all of it) |
| `--track` | `KIND` | video, overlay (picture-in-picture) or audio |

### `reel split PROJECT`

Cut every clip that crosses a point in the timeline

| Flag | Value | Meaning |
| --- | --- | --- |
| `--at` | `SECONDS` | Where to cut |

### `reel trim PROJECT`

Change a clip's source window or position

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id (from `reel inspect`) |
| `--in` | `SECONDS` | New start point inside the source |
| `--duration` | `SECONDS` | New length |
| `--start` | `SECONDS` | New timeline position |

### `reel move PROJECT`

Move a clip along the timeline

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id |
| `--to` | `SECONDS` | New timeline position |

### `reel roll PROJECT`

Roll a cut: one clip grows, the other shrinks, the timeline stays put

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | The cut between this clip and its left neighbour moves |
| `--by` | `SECONDS` | Positive = the neighbour grows; total length never changes |

### `reel slip PROJECT`

Slip a clip: change WHAT plays without moving WHEN

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id |
| `--by` | `SECONDS` | Shift the clip's window through its source |

### `reel slide PROJECT`

Slide a clip between its neighbours; the combined span is unchanged

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id (must touch neighbours on both sides) |
| `--by` | `SECONDS` | Move the clip; neighbours absorb the motion |

### `reel remove PROJECT`

Delete a clip

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id |
| `--ripple` | — | Close the gap left behind |

### `reel gap PROJECT`

Close every gap between clips

### `reel gain PROJECT`

Set a clip's audio level

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id |
| `--db` | `DECIBELS` | Level change, e.g. -6 or 3 |

### `reel audio PROJECT`

A clip's audio processing: pan, EQ, compressor, repair — live mix and export alike

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id (omit with --track for track-level pan) |
| `--track` | `NAME` | Track name (V1/A1/V2) for --pan at track level |
| `--pan` | `-1..1` | Stereo balance: -1 left, 0 centre, 1 right |
| `--eq-low` | `DB` | Low shelf at 120 Hz |
| `--eq-mid` | `DB` | Peaking bell (see --eq-mid-freq) |
| `--eq-mid-freq` | `HZ` | The bell's centre (default 1000) |
| `--eq-high` | `DB` | High shelf at 8 kHz |
| `--comp` | — | Compressor on (threshold/ratio below) |
| `--comp-thresh` | `DB` | Threshold, dBFS (default -18) |
| `--comp-ratio` | `N` | Ratio N:1 (default 3) |
| `--comp-off` | — | Compressor off |
| `--deess` | `0..1` | De-esser intensity (render-time; tames harsh S sounds) |
| `--fade-curve` | `SHAPE` | Audio fade shape: linear, smooth (qsin) or exp |
| `--fix` | — | Fix voice on export: rumble/hum off, noise down, clicks patched |
| `--fix-off` | — | Stop fixing |
| `--reset` | — | Back to untouched audio |
| `--json` | — | Print the result as JSON |

### `reel effects PROJECT`

Colour, fades and reframing for one clip

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id |
| `--exposure` | `N` | 1.0 = unchanged |
| `--contrast` | `N` | 1.0 = unchanged |
| `--saturation` | `N` | 1.0 = unchanged |
| `--fade-in` | `SECONDS` | Fade up from black |
| `--fade-out` | `SECONDS` | Fade down to black |
| `--zoom` | `N` | 1.0 = whole frame; used for reframing |
| `--pan-x` | `N` | -1..1, where the zoom sits |
| `--pan-y` | `N` | -1..1 |
| `--lut` | `FILE.cube` | Grade through a 3D LUT (applied before the trims) |
| `--mask` | `SHAPE` | Limit the grade to a window: ellipse, rect, or off |
| `--mask-x` | `0..1` | Window centre across the frame |
| `--mask-y` | `0..1` | Window centre down the frame |
| `--mask-w` | `0..1` | Half-width of the window |
| `--mask-h` | `0..1` | Half-height of the window |
| `--mask-feather` | `0..1` | Soft edge width (default 0.05) |
| `--mask-invert` | — | Grade outside the window instead |
| `--lut-off` | — | Remove the LUT |
| `--black` | `0..0.5` | Levels input black point (0 = unchanged) |
| `--white` | `0.5..1.5` | Levels input white point (1 = unchanged) |
| `--gamma` | `N` | Levels mid gamma; >1 brightens (1 = unchanged) |
| `--temp` | `-1..1` | White balance: + warms, - cools |
| `--tint` | `-1..1` | White balance: + magenta, - green |
| `--hsl-hue` | `DEG` | HSL qualifier: window centre hue 0..360 (creates the qualifier) |
| `--hsl-width` | `DEG` | Hue window half-width (default 40) |
| `--hsl-push-hue` | `DEG` | Hue shift inside the window |
| `--hsl-push-sat` | `N` | Saturation multiplier inside the window |
| `--hsl-push-lum` | `N` | Lightness multiplier inside the window |
| `--hsl-off` | — | Remove the qualifier |
| `--rotate` | `DEG` | Quarter turns: 0, 90, 180 or 270 (clockwise) |
| `--flip-h` | — | Mirror left-right (toggle) |
| `--flip-v` | — | Mirror top-bottom (toggle; both = 180° rotation) |
| `--plugin` | `FILE.wgsl` | An effect plugin (WGSL) — runs in preview AND render; see docs/PLUGINS.md |
| `--plugin-params` | `A,B,C,D` | The plugin's parameter values (defaults from its header) |
| `--plugin-off` | — | Remove the plugin |
| `--raw-filter` | `CHAIN` | EXPERT: raw ffmpeg video filters spliced into this clip's decode (render + frame; live preview can't show it) |
| `--raw-filter-off` | — | Remove the raw filter |
| `--like` | `CLIP` | Copy another clip's grade (colour only, not fades/reframe) |
| `--like-all` | — | With --like: stamp that grade on EVERY video clip |
| `--key-color` | `RRGGBB` | Chroma key: knock this colour out (e.g. 00b140) |
| `--key-similarity` | `0..1` | How far from the key colour still counts (default 0.3) |
| `--key-softness` | `0..1` | Soft edge width beyond similarity (default 0.1) |
| `--key-off` | — | Stop keying |
| `--reset` | — | Back to no effects |
| `--json` | — | Print the result as JSON |

### `reel keyframe PROJECT`

Animate a parameter over time — evaluated per frame at render

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id (from `reel inspect`) |
| `--param` | `NAME` | exposure, contrast, saturation, zoom, pan-x, pan-y, opacity, pip-x, pip-y, pip-scale |
| `--at` | `SECONDS` | TIMELINE time of the keyframe |
| `--value` | `N` | The value at that moment |
| `--interp` | `MODE` | linear (default), hold or ease |
| `--remove` | — | Remove the keyframe nearest --at instead |
| `--list` | — | Show every keyframe on the clip |

### `reel pip PROJECT`

Place a picture-in-picture overlay in the frame

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | An overlay clip's id |
| `--x` | `0..1` | Centre of the inset across the frame |
| `--y` | `0..1` | Centre of the inset down the frame |
| `--scale` | `0..1` | Inset width as a fraction of the frame |

### `reel speed PROJECT`

Change how fast a clip plays

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id |
| `--rate` | `N` | 2 = twice as fast, 0.5 = half. Audio follows. |
| `--keep-length` | — | Keep the timeline slot; use more or less source |

### `reel curves PROJECT`

Tone curves — an S-curve in one flag, baked into the clip's grade

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id |
| `--channel` | `NAME` | master, r, g or b |
| `--points` | `Y0,Y1,Y2,Y3,Y4` | Outputs at inputs 0,¼,½,¾,1 (identity: 0,0.25,0.5,0.75,1) |
| `--reset` | — | Back to identity on every channel |

### `reel transition PROJECT`

Crossfade from the previous clip into this one

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id — the fade runs INTO this clip |
| `--seconds` | `SECONDS` | Transition length (0 = hard cut) |
| `--kind` | `NAME` | fade, dip, wipe-left/right/up/down, slide-left/right |

### `reel title ACTION PROJECT`

ACTION is add, list or remove — text placed on the picture, optionally animated

| Flag | Value | Meaning |
| --- | --- | --- |
| `--text` | `TEXT` | The words |
| `--at` | `SECONDS` | When it appears |
| `--duration` | `SECONDS` | How long it stays |
| `--x` | `0..1` | Horizontal centre, as a fraction of the frame |
| `--y` | `0..1` | Vertical centre, as a fraction of the frame |
| `--size` | `0..1` | Text height as a fraction of the frame |
| `--color` | `RRGGBB` | Hex colour, e.g. ffcc00 |
| `--no-bold` | — | Regular weight |
| `--no-outline` | — | No dark outline |
| `--preset` | `NAME` | A title preset (style + motion) from ~/.config/reel/titles — name or path |
| `--fade-in` | `SECONDS` | Fade (and slide, if set) up over this long |
| `--fade-out` | `SECONDS` | Fade away over this long |
| `--slide` | `EDGE` | Slide in from left, right, top or bottom (needs --fade-in) |
| `--index` | `N` | Which title (for remove) |
| `--json` | — | Print the result as JSON |

### `reel music ACTION PROJECT AUDIO?`

ACTION is set or clear — a music bed under the whole edit

| Flag | Value | Meaning |
| --- | --- | --- |
| `--gain-db` | `DECIBELS` | Level (default -12) |
| `--no-duck` | — | Don't pull the music down under speech |
| `--fade` | `SECONDS` | Fade in/out (default 1) |
| `--fit` | — | Time-stretch the track to end exactly with the edit (pitch-preserved at render) |
| `--json` | — | Print the result as JSON |

### `reel marker PROJECT`

Flag a position in the timeline (named markers become chapters)

| Flag | Value | Meaning |
| --- | --- | --- |
| `--at` | `SECONDS` | Where to put it |
| `--label` | `TEXT` | Name it — named markers become named chapters |
| `--remove` | — | Take it away instead |
| `--list` | — | Show the markers |
| `--json` | — | Print the result as JSON |

### `reel track PROJECT`

Follow the subject under the clip's power window and keyframe the window onto its path

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id (needs a power window — see effects --mask) |
| `--json` | — | Print the result as JSON |

### `reel chapters PROJECT`

The markers as YouTube-ready chapter text (00:00 first, MM:SS titles)

| Flag | Value | Meaning |
| --- | --- | --- |
| `--out` | `FILE.txt` | Also write the list to a file |
| `--json` | — | Print the result as JSON |

### `reel stabilize PROJECT`

Smooth a clip's camera shake at render time (two-pass, cached)

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id |
| `--off` | — | Stop stabilising this clip |

### `reel align PROJECT`

Sync one clip to another by their AUDIO — multicam without clap sticks

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | The clip to move |
| `--to` | `ID` | The clip to sync against |
| `--window` | `SECONDS` | Largest offset to search (default 90) |

### `reel snapshot PROJECT`

Named project snapshots — freeze the edit now, roll back later. Never lose work

| Flag | Value | Meaning |
| --- | --- | --- |
| `--name` | `TEXT` | What to call this state (default: the timestamp) |
| `--list` | — | Show saved snapshots |
| `--restore` | `FILE` | Restore this snapshot (the current state is snapshotted first) |
| `--json` | — | Print the result as JSON |

### `reel adjust PROJECT`

Add an adjustment layer — a span whose colour grade applies to everything beneath it (set the grade with `reel effects --clip`)

| Flag | Value | Meaning |
| --- | --- | --- |
| `--at` | `SECONDS` | Where the layer starts |
| `--duration` | `SECONDS` | How long it lasts (default 4) |
| `--json` | — | Print the result as JSON |

### `reel pool PROJECT`

The media pool: gather, bin and list this project's media (offline files are flagged)

| Flag | Value | Meaning |
| --- | --- | --- |
| `--add` | `FILE` | Gather a file into the pool |
| `--bin` | `NAME` | With --add (or --file): which bin it goes in |
| `--file` | `FILE` | Re-bin an item already in the pool |
| `--remove` | `FILE` | Take an item out of the pool |
| `--json` | — | Print the result as JSON |

### `reel relink PROJECT`

Repoint moved media everywhere: clips, pool, music, angles. Directories relink recursively

| Flag | Value | Meaning |
| --- | --- | --- |
| `--from` | `PATH` | The old file or directory |
| `--to` | `PATH` | Where it lives now |
| `--json` | — | Print the result as JSON |

### `reel multicam PROJECT`

Multicam: register synced angles, then cut between them (keys 1-9 in the editor do this live)

| Flag | Value | Meaning |
| --- | --- | --- |
| `--add` | `FILE` | Register an angle |
| `--offset` | `SECONDS` | Timeline time where the angle's t=0 falls (with --add) |
| `--align` | — | With --add: find the offset by syncing audio against the first V1 clip |
| `--cut` | `SECONDS` | Cut to --angle at this timeline time |
| `--angle` | `N` | Angle index for --cut (0-based) |
| `--clear` | — | Forget every angle |
| `--json` | — | Print the result as JSON |

### `reel roomtone PROJECT`

Sample the quietest breath of the footage and loop it under the whole edit — cuts never drop to digital black

| Flag | Value | Meaning |
| --- | --- | --- |
| `--gain-db` | `DB` | Level trim for the bed (default 0 = as sampled) |
| `--off` | — | Remove the room tone |
| `--json` | — | Print the result as JSON |

### `reel beats TARGET`

Find the beats and drop a marker on each — cuts can snap to the music. TARGET is a .reel or a media file

| Flag | Value | Meaning |
| --- | --- | --- |
| `--source` | `FILE` | Detect in this file (default: the project's music bed) |
| `--every` | `N` | Keep every Nth beat (default 1) |
| `--replace` | — | Clear existing markers first |
| `--json` | — | Print the result as JSON |

### `reel fillers PROJECT`

Transcribe word-by-word and cut the ums and uhs out of the edit

| Flag | Value | Meaning |
| --- | --- | --- |
| `--words` | `LIST` | Comma-separated fillers (default um,uh,uhm,er,erm,hmm) |
| `--pad` | `SECONDS` | Extra trimmed around each word (default 0.04) |
| `--model` | `NAME` | tiny, base (default) or small |
| `--dry-run` | — | List what would be cut without cutting |
| `--quiet` | — | Don't print progress |
| `--json` | — | Print the result as JSON |

### `reel tighten PROJECT`

Cut the silent air out of the edit and close up — the podcast jump-cut

| Flag | Value | Meaning |
| --- | --- | --- |
| `--threshold` | `0..1` | Quiet = below this fraction of the source's own peak (default 0.06) |
| `--min-gap` | `SECONDS` | Only cut silences at least this long (default 0.6) |
| `--pad` | `SECONDS` | Breathing room kept on each side of a cut (default 0.15) |

### `reel captions TARGET`

Transcribe speech locally. TARGET is a .reel project or a media file

| Flag | Value | Meaning |
| --- | --- | --- |
| `--model` | `NAME` | tiny, base or small (default base) |
| `--size` | `N` | Caption size (default 20) |
| `--srt` | `FILE` | Also write the captions to this .srt |
| `--source` | `MEDIA` | Transcribe this instead of the project's first clip |
| `--quiet` | — | Don't print progress |

### `reel bench MEDIA`

Measure this machine: probe, first frame, scrub latency, export speed on MEDIA

| Flag | Value | Meaning |
| --- | --- | --- |
| `--seconds` | `N` | How much of the file the export leg renders (default 5) |

### `reel frame TARGET`

Export one frame as PNG. TARGET is a .reel (rendered with effects, overlays, animation) or a media file

| Flag | Value | Meaning |
| --- | --- | --- |
| `--at` | `SECONDS` | Which moment (default 0) |
| `--out` | `FILE.png` | Where to write the PNG (default beside the target) |
| `--overwrite` | — | Replace the output if it exists |

### `reel render PROJECT OUTPUT`

Render the edit — captions, titles and music included

| Flag | Value | Meaning |
| --- | --- | --- |
| `--preset` | `NAME` | A social preset: see `reel presets` |
| `--hdr-passthrough` | — | Keep the source's HDR (PQ/HLG) and encode 10-bit — h265/av1/vp9, source exports only |
| `--codec` | `NAME` | h264, h265, av1, vp9, remux, mp3, m4a, opus, flac, wav, png, jpeg, webp |
| `--quality` | `NAME` | high, balanced, small, or a CRF number |
| `--resolution` | `HEIGHT` | source, 2160, 1080, 720, 480 |
| `--fit` | `MODE` | letterbox, crop or blur (how a mismatched aspect is filled) |
| `--audio` | `MODE` | copy (pass the source audio through) or encode |
| `--loudness` | `LUFS` | Deliver audio at this integrated loudness (e.g. -14); presets set it automatically |
| `--no-hardware` | — | Force the software encoder |
| `--overwrite` | — | Replace the output file if it exists |
| `--watch` | — | Keep going: re-render whenever the project file changes (Ctrl+C stops) |
| `--quiet` | — | Don't print progress |
| `--json` | — | Print the result as JSON |

### `reel convert MEDIA OUTPUT`

Transcode one file, no project needed

| Flag | Value | Meaning |
| --- | --- | --- |
| `--preset` | `NAME` | A social preset: see `reel presets` |
| `--codec` | `NAME` | h264, h265, av1, vp9, remux, mp3, m4a, opus, flac, wav, png, jpeg, webp |
| `--quality` | `NAME` | high, balanced, small, or a CRF number |
| `--resolution` | `HEIGHT` | source, 2160, 1080, 720, 480 |
| `--fit` | `MODE` | letterbox, crop or blur (how a mismatched aspect is filled) |
| `--audio` | `MODE` | copy (pass the source audio through) or encode |
| `--loudness` | `LUFS` | Deliver audio at this integrated loudness (e.g. -14); presets set it automatically |
| `--no-hardware` | — | Force the software encoder |
| `--overwrite` | — | Replace the output file if it exists |
| `--quiet` | — | Don't print progress |

## Capture

Reel is also the screen recorder, and every choice the app offers through a
picker is a flag here — which is what makes capture reachable from a script
or an agent. Files land in `~/Pictures/Reel` and `~/Videos/Reel` unless you
name one.

`reel devices` first: it reports the monitors, cameras and audio sources by
the exact names the other two commands accept, plus which backend will
actually run. A capability this machine lacks is reported as a missing tool,
never silently substituted.

```bash
reel devices --json                                  # what can I capture?
reel screenshot shot.png --area 0,0,1280x720         # exact pixels, no picker
reel screenshot --delay 3                            # time to open a menu
reel record clip.mp4 --duration 10 --audio both      # blocks, returns the file
reel record --area 100,80,1920x1080 --fps 60         # starts, returns at once
reel record --stop                                   # …and finishes it
reel record --duration 20 --streamer                 # screen + camera → .reel
```

Recording without `--duration` returns immediately and keeps running in the
background, so a later `reel record --stop` — from a different process, a
different session, a different agent — finishes it and hands back the file.
`reel record --status` says whether one is running and for how long.

### `reel screenshot OUT`

Take a screenshot. Default is the whole desktop, saved under ~/Pictures/Reel

| Flag | Value | Meaning |
| --- | --- | --- |
| `--area` | `X,Y,WxH` | Grab exactly this rectangle — no picker, no user |
| `--region` | — | Drag-select a rectangle (waits for a person) |
| `--window` | — | The active window |
| `--display` | `NAME` | A monitor by name — see `reel devices` |
| `--delay` | `SECONDS` | Wait before grabbing (menus, hover states) |

### `reel record OUT`

Record the screen or a camera. Starts in the background; `--stop` finishes it

| Flag | Value | Meaning |
| --- | --- | --- |
| `--duration` | `SECONDS` | Record exactly this long, then return the file |
| `--stop` | — | Finish the recording already running and return its file |
| `--status` | — | Is a recording running? For how long, into what? |
| `--area` | `X,Y,WxH` | Record exactly this rectangle of the screen |
| `--display` | `NAME` | A monitor by name — see `reel devices` |
| `--fps` | `N` | Frame rate (default 30) |
| `--audio` | `MODE` | none, system, mic or both (default system) |
| `--no-cursor` | — | Leave the mouse pointer out |
| `--webcam` | — | Record a camera instead of the screen |
| `--device` | `PATH` | Which camera (default: the first one that answers) |
| `--streamer` | — | Record screen AND camera, then build the PiP project — needs --duration |
| `--project` | `FILE.reel` | Append the finished recording to this project as a clip |

### `reel devices`

What this machine can capture: monitors, cameras, audio sources, and the backends behind them

### `reel presets`

The one-click destinations (YouTube, TikTok, Reels…)

### `reel commands`

Every command, argument and flag — the machine-readable manual

## A worked example

Cut two shots together with a crossfade, colour the first, caption it, put a
title on it and render it for Instagram — start to finish, no window:

```bash
reel new promo.reel --size 1080x1350 --fps 30
reel add promo.reel shot-a.mp4 --duration 4
reel add promo.reel shot-b.mp4 --duration 3

# Ids are stable; read them back rather than guessing.
IDS=$(reel inspect promo.reel --json | jq -r '.clips[].id')
set -- $IDS

reel transition promo.reel --clip $2 --seconds 0.5
reel effects promo.reel --clip $1 --saturation 1.2 --fade-in 0.5
reel captions promo.reel --model base
reel title add promo.reel --text "New drop" --at 0.2 --duration 2.5 \
     --y 0.15 --size 0.11 --color ffcc00
reel music set promo.reel bed.mp3 --gain-db -14

reel render promo.reel promo.mp4 --preset "Instagram feed" --json
```

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | The command ran and failed (bad arguments, missing clip, render error) |
| 2 | Not a command at all — a typo, or a file that doesn't exist |

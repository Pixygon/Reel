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
| `--reset` | — | Back to no effects |

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

### `reel transition PROJECT`

Crossfade from the previous clip into this one

| Flag | Value | Meaning |
| --- | --- | --- |
| `--clip` | `ID` | Clip id — the fade runs INTO this clip |
| `--seconds` | `SECONDS` | Crossfade length (0 = hard cut) |

### `reel title ACTION PROJECT`

ACTION is add, list or remove — text placed on the picture

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
| `--index` | `N` | Which title (for remove) |

### `reel music ACTION PROJECT AUDIO?`

ACTION is set or clear — a music bed under the whole edit

| Flag | Value | Meaning |
| --- | --- | --- |
| `--gain-db` | `DECIBELS` | Level (default -12) |
| `--no-duck` | — | Don't pull the music down under speech |
| `--fade` | `SECONDS` | Fade in/out (default 1) |

### `reel marker PROJECT`

Flag a position in the timeline

| Flag | Value | Meaning |
| --- | --- | --- |
| `--at` | `SECONDS` | Where to put it |
| `--remove` | — | Take it away instead |
| `--list` | — | Show the markers |

### `reel captions TARGET`

Transcribe speech locally. TARGET is a .reel project or a media file

| Flag | Value | Meaning |
| --- | --- | --- |
| `--model` | `NAME` | tiny, base or small (default base) |
| `--size` | `N` | Caption size (default 20) |
| `--srt` | `FILE` | Also write the captions to this .srt |
| `--source` | `MEDIA` | Transcribe this instead of the project's first clip |
| `--quiet` | — | Don't print progress |

### `reel render PROJECT OUTPUT`

Render the edit — captions, titles and music included

| Flag | Value | Meaning |
| --- | --- | --- |
| `--preset` | `NAME` | A social preset: see `reel presets` |
| `--codec` | `NAME` | h264, h265, av1, vp9, remux, mp3, m4a, opus, flac, wav, png, jpeg, webp |
| `--quality` | `NAME` | high, balanced, small, or a CRF number |
| `--resolution` | `HEIGHT` | source, 2160, 1080, 720, 480 |
| `--fit` | `MODE` | letterbox, crop or blur (how a mismatched aspect is filled) |
| `--audio` | `MODE` | copy (pass the source audio through) or encode |
| `--no-hardware` | — | Force the software encoder |
| `--overwrite` | — | Replace the output file if it exists |
| `--quiet` | — | Don't print progress |

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
| `--no-hardware` | — | Force the software encoder |
| `--overwrite` | — | Replace the output file if it exists |
| `--quiet` | — | Don't print progress |

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

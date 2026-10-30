**What `MEDIA` can be.** Everything goes through the same verb:

| Media | Default length | Notes |
|---|---|---|
| Video or audio file | All of it, from `--in` | |
| Still image (PNG, JPEG, WebP, SVG…) | 5 seconds | `--duration` sets it. No sound of its own; the clips around it keep theirs |
| `color:RRGGBB` | 5 seconds | A generated solid plate, sized to the frame. The project stores the colour, not the file, so it survives a move to another machine |
| `color:transparent` | 5 seconds | A blank canvas: black on V1, nothing at all on the overlay track |
| A `.reel` project | Its whole length | **Nesting** (a compound clip): rendered flat beside itself, and re-rendered before `render` if the nested edit changed. A project can't nest itself |

**Overlay track.** There is one overlay track (`V2`) and no fixed limit on it:
clips may overlap freely and draw in the order they were added, later on top.
PNG **alpha is kept** in the render, so a logo or a dark-disc avatar needs no
`--key-color`. One honest gap: the live inset preview shows a still opaque
(the render and `reel frame` are right).

**Audio track.** Clips on `A1` may overlap; each keeps its own gain and they
are summed without normalising, in the preview and the render alike.

### `reel slot ACTION PROJECT NAME?`

Named template slots, so an episode swaps its captures and card text by name
and never needs `title remove`/`add --index`. `ACTION` is `set`, `list`, `bind`
or `unbind`.

| Flag | Value | Meaning |
|---|---|---|
| `--media` | `FILE` | set: the new media (file, .reel or color:RRGGBB) |
| `--text` | `TEXT` | set: the new text |
| `--clip` | `ID` | bind: the clip this slot fills |
| `--title` | `INDEX` | bind: the title this slot fills |
| `--json` | — | Print the result as JSON |

```bash
reel add ep.reel capture.mp4 --slot capture        # or bind later: reel slot bind ep.reel capture --clip 100
reel title add ep.reel --text "Episode 1" --slot card
reel slot set ep.reel capture --media ep2.mp4
reel slot set ep.reel card --text "Episode 2"
reel slot list ep.reel --json
```

A name can fill several places; `set` reaches them all. A swapped clip keeps
its place, length, effects and pip. It restarts at the new media's beginning,
and is shortened only if the new media can't fill it (`shortened` in the
result says so).

### `reel variant ACTION PROJECT NAME?`

Other frame shapes of ONE project. `ACTION` is `add`, `list`, `set` or `remove`.

| Flag | Value | Meaning |
|---|---|---|
| `--size` | `WxH` | add: the frame, e.g. 1080x1920 |
| `--fit` | `MODE` | add/set: letterbox, crop or blur (default blur when the aspect changes) |
| `--clip` | `ID` | set: an overlay clip to place by hand in this variant |
| `--title` | `INDEX` | set: a title to place by hand in this variant |
| `--x` | `0..1` | set: horizontal centre |
| `--y` | `0..1` | set: vertical centre |
| `--scale` | `0..1` | set: inset width, as a fraction of the frame |
| `--text-size` | `0..1` | set: title height, as a fraction of the frame |
| `--json` | — | Print the result as JSON |

```bash
reel variant add ep.reel 9x16 --size 1080x1920
reel render ep.reel ep-9x16.mp4 --variant 9x16
reel variant set ep.reel 9x16 --title 0 --y 0.2    # this one title, by hand
```

Nothing is hand-placed by default. Insets and titles are scaled from the main
layout, then kept inside the frame's safe area (portrait keeps clear of the
platform's caption block and side buttons). Titles grow when there's room and
shrink when they wouldn't fit. Overrides win, and keyframed pips follow.


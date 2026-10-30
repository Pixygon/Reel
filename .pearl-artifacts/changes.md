## [Unreleased]

### Added
- Picture effects, one shader stage shared by preview and render: `--blur`, `--rgb-split`, `--glitch`, `--shake`/`--shake-hz` and free `--rotate-deg`. A shake on an adjustment clip moves the whole frame.
- Logo build: `reel effects --build tiles N --build-seconds S` assembles a picture from tiles.
- Plates: `reel add P color:RRGGBB --duration N`, and `color:transparent` for a blank canvas.
- Template slots: `reel slot set|list|bind|unbind`, plus `--slot` on `add` and `title add`.
- Aspect variants: `reel variant add|list|set|remove` and `render`/`frame --variant`, laid out from the main edit inside the safe area, with per-variant overrides.

### Changed
- `reel add` gives a still 5 seconds by default instead of refusing it.
- A still or plate in the cut no longer drops the audio of the clips around it.

### Fixed
- PNG alpha on the overlay track now survives the render.
- `reel frame` no longer loses a still that isn't at the very start of the timeline.


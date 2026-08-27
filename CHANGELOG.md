# Changelog

All notable changes to **Reel**. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this file is
materialized from the Pixygon Changelog API — edit there, not here.

## [0.12.0] — 2026-08-27

### Added
- Status messages now appear as a brief toast notification at the top of the window instead of a permanent bottom status bar. _(781f2dd)_
- A new ☰ REEL menu consolidates Open, Default apps, website link, and Quit in one place. _(781f2dd)_

### Changed
- The player now has no top bar or tab bar at all — media fills the entire window. Playback controls (seek bar, transport, ☰ menu) appear as a bottom overlay that fades away, along with the cursor, after a couple of idle seconds during playback, then reappear on any input or when paused. **(BREAKING)** _(781f2dd)_
- Screenshot and screen recording have moved into Reel's system tray icon, so they're reachable even when Reel's window is minimized or buried. The in-app ☰ menu only shows capture options as a fallback when no system tray is available. **(BREAKING)** _(781f2dd)_

### Fixed
- Transparent images now render with correct colors instead of a dark or fringed halo around transparent areas, and are shown over a checkerboard backdrop like a proper image viewer. _(781f2dd)_


## [0.11.0] — 2026-08-27

### Added
- Reel now integrates with your Linux desktop: it installs itself in the "Open with" menu and can be set as the default player for video, music, and images with a couple of clicks. A one-time banner offers to set this up, and it's always reachable afterwards from ⚙ → Default apps.
- When Reel opens with nothing loaded, it now shows a friendly empty state with a big "Open a file…" button and a hint about dragging files in or double-clicking them in your file manager.
- Added a ⚙ menu with quick access to default-app settings and the Reel website.

### Changed
- Removed the inline "paste a path" text field and the Player/Editor tab switcher from the top bar; the editor is now reached via the "✂ Edit" button and you return to the player with a new "▶ Done" button. **(BREAKING)**

### Fixed
- Fixed a bug on Linux where the "Open…" file picker and screenshot/screen-recording dialogs would only work the first time and then silently stop responding.


## [0.10.0] — 2026-08-27

### Added
- The file picker now supports audio and image files in addition to video, with a new combined "Media" filter alongside separate Video, Audio, and Images filters.

### Fixed
- Opening a file on Linux via the file picker is now more reliable, and if the dialog fails to open for any reason, you'll see a helpful message suggesting you drop a file or paste a path instead.


## [0.9.0] — 2026-08-27

### Added
- Added command-line --help/-h and --version/-V flags, so running Reel from a terminal now prints usage, keyboard shortcuts, and version info instead of just opening a window.


## [0.8.0] — 2026-08-27

### Added
- Screen recording is now built in on Linux — Reel talks directly to xdg-desktop-portal and PipeWire (the same mechanism OBS uses), so no external capture tool is required. The system's own picker lets you choose the screen, a window, or a region, system audio is captured when available, and your choice is remembered after the first approval.
- Screenshots now support Full screen, Region, and Window modes via a new Shot menu, with the system's interactive portal dialog as a tool-free fallback on Linux.
- Audio playback now includes built-in visualizers — spectrum bars, scrolling spectrogram, vectorscope, and waveform — rendered directly by the playback engine. Press V to cycle, or pick one from the transport bar.
- SVG images now open instantly alongside other image formats, rasterized crisply, and can be exported to PNG/JPEG/WebP.
- If ffmpeg isn't found on your system (mainly on Windows), Reel now downloads a private static build automatically on first launch so exporting and playback keep working.

### Changed
- The Record button now shows a starting state while the screen/window picker is open, since starting a recording may briefly wait on your selection.


## [0.7.0] — 2026-08-27

### Added
- Reel now plays audio files, not just video — through the same transport controls. Embedded cover art displays when present, otherwise a simple ♪ card is shown.
- Images (PNG, JPEG, WebP, BMP, TIFF, ICO and more) now open instantly in Reel through the same viewer used for video — no separate image viewer needed. Oversized images, like ultrawide screenshots or 8K stills, are automatically downscaled to fit your GPU's texture limits.
- New screen capture tools right in the toolbar: 📷 Shot takes a screenshot, and ⏺ Record captures screen video (with system audio where supported). Captures open immediately in Reel, ready to trim, convert, or export.
- Export now supports audio extraction from video (to MP3, M4A, Opus, FLAC, or WAV), audio-to-audio conversion between those formats, and image conversion to PNG, JPEG, or WebP with resizing — all through the same Export dialog used for video.

### Changed
- Opening any media — video, audio, or an image — now drops it onto the editor timeline automatically: video and stills land on the video track, audio on the audio track.
- The Export dialog now only shows options relevant to what's open (e.g. quality/CRF controls are hidden for lossless or audio-only formats), reducing clutter and confusion.


## [0.6.0] — 2026-08-27

### Added
- You can now open videos with a native Open… file picker or by dragging a file straight onto the window, alongside the existing path field and command-line launch.
- Added a full set of player controls: frame stepping, ±5s/±60s jumps, volume/mute, 0.25–4× playback speed, loop, and fullscreen — all with mpv/VLC-style keyboard shortcuts (Space, arrows, comma/period, M, L, F, brackets, E for editor).
- Added an Export/Convert dialog right in the player — encode to H.264, H.265, AV1 or VP9 with quality presets or custom CRF, downscale resolution, choose audio bitrate or copy-through, or do an instant lossless MKV remux. Shows live progress and speed, and can be cancelled mid-encode.

### Improved
- Seeking is now smoother: dragging the seek bar or clicking/dragging the timeline scrubs live with frame-exact precision on the libmpv backend, and time labels now show hours for longer videos.


## [0.5.0] — 2026-08-27

### Added
- Reel now plays video through libmpv when it's installed, unlocking hardware-accelerated decode, correct colour conversion, audio playback with real audio/video sync, subtitle support, and frame-exact seeking. The status bar shows which backend (mpv or ffmpeg) is active.

### Changed
- If libmpv isn't available, Reel automatically falls back to the original ffmpeg-subprocess decoder (video only, keyframe-accurate seek), so playback still works everywhere. You can force this fallback with the REEL_BACKEND=ffmpeg environment variable.
- Opening a video file now starts playback immediately instead of loading it paused.

### Improved
- Playing a video to the end and pressing play again now replays it from the start, VLC-style, instead of staying stuck at the end.
- Redraws are now paced more precisely, keeping the UI responsive during playback and briefly after opening or seeking a file, without spinning the CPU when idle.



# Changelog

All notable changes to **Reel**. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this file is
materialized from the Pixygon Changelog API — edit there, not here.

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



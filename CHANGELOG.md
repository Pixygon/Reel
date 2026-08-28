# Changelog

All notable changes to **Reel**. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this file is
materialized from the Pixygon Changelog API — edit there, not here.

## [0.23.0] — 2026-08-28

### Added
- Added one-button local auto-captions. Click ✦ Generate captions and Reel transcribes your speech entirely on your machine — no upload, no account, no per-minute billing. The speech engine and model are fetched automatically the first time (about 85MB, once), then captioning works fully offline. _(fe249ea)_
- Captions now appear on the timeline and preview exactly where they'll burn into your final export, so what you see is what you get. Cues follow your edits — a line spanning a cut appears in both halves, and speech you trimmed away won't show a caption at all. _(fe249ea)_
- You can choose between three caption engine sizes (Fast, Balanced, Accurate) to trade off speed versus transcription accuracy, and adjust caption text size directly from the media panel. _(fe249ea)_
- Added Redo captions and Remove buttons so you can easily regenerate or clear captions from an edit. _(fe249ea)_

### Improved
- Ripple delete (Shift+Delete) and ripple trim to playhead (Q/W) now work linked across tracks, and J-K-L shuttle supports true reverse playback. _(fe249ea)_

### Fixed
- Fixed a bug where captions rendered far too large on high-resolution exports (e.g. ~3.7× too big on 4K), causing the exported captions to not match the preview. _(fe249ea)_


## [0.22.0] — 2026-08-28

### Added
- Added J-K-L shuttle controls: L steps up forward playback speed, J shuttles backwards (including true reverse playback), and K stops — the classic editor's shortcut for scrubbing through footage. _(39fdbb0)_
- Added ripple delete (Shift+Delete) — removing a clip now closes the gap behind it automatically, keeping video and linked audio in sync. _(39fdbb0)_
- Added ripple trim shortcuts Q and W to trim a clip's head or tail back to the playhead and instantly close up the resulting gap. _(39fdbb0)_

### Changed
- The L key now requires Shift to toggle looping, since plain L is used for shuttle-forward; Ctrl/Cmd+K is now an alternate shortcut for splitting a clip. **(BREAKING)** _(39fdbb0)_
- Updated the on-screen editor shortcut hints to reflect the new J K L shuttle, ripple delete, and ripple trim controls. _(39fdbb0)_


## [0.21.1] — 2026-08-28

### Added
- Added J-K-L shuttle controls: L steps up forward playback speed, J shuttles backwards (including true reverse playback), and K stops — the classic editor's shortcut for scrubbing through footage. _(39fdbb0)_
- Added ripple delete (Shift+Delete) — removing a clip now closes the gap behind it automatically, keeping video and linked audio in sync. _(39fdbb0)_
- Added ripple trim shortcuts Q and W to trim a clip's head or tail back to the playhead and instantly close up the resulting gap. _(39fdbb0)_

### Changed
- The L key now requires Shift to toggle looping, since plain L is used for shuttle-forward; Ctrl/Cmd+K is now an alternate shortcut for splitting a clip. **(BREAKING)** _(39fdbb0)_
- Updated the on-screen editor shortcut hints to reflect the new J K L shuttle, ripple delete, and ripple trim controls. _(39fdbb0)_


## [0.21.0] — 2026-08-28

### Changed
- The timeline panel now slides smoothly in and out when switching between Player and Editor modes instead of snapping into place. _(3bf31fb)_

### Improved
- Redesigned the editor's transport controls with larger, rounder play/pause and frame-step buttons, centered under the preview for easier clicking. _(3bf31fb)_
- Redesigned the export destination picker with larger, clearer preset cards (three per row) showing the name and a short description, plus a dedicated 'Custom settings' option instead of a small button row. _(3bf31fb)_
- The Start Export button is now bigger, filled, and rounded for better visibility, and the export window is slightly wider to fit the new preset cards. _(3bf31fb)_

### Fixed
- Fixed the editor toolbar layout so the right-hand tools no longer overflow and overlap the playback transport when the window is narrow. _(3bf31fb)_


## [0.20.0] — 2026-08-28

### Added
- Projects now save themselves automatically shortly after you stop editing — no more remembering to hit Save, and no risk of losing work. The timeline status now shows "saving…" / "saved" instead of a manual Save button. **(BREAKING)** _(11a61ba)_
- Right-click a clip (or empty timeline space) to close gaps instantly — close the gap before one clip, or sweep every gap on every track closed in one go. _(11a61ba)_
- Right-click a clip for a quick menu to delete it, in addition to trimming and dragging. _(11a61ba)_

### Changed
- In the editor, the seek bar and time display now scrub and show the position across your whole edited timeline (not just the currently loaded source clip), giving a true preview of the final cut as you work. _(11a61ba)_
- Saved project files are now written atomically (via a temp file plus rename), so a crash or power loss mid-save can no longer corrupt your project file. _(11a61ba)_

### Improved
- The media/inspector side panel now resizes smoothly and stays at the width you drag it to, and its contents scroll instead of forcing the panel wider than you want. _(11a61ba)_
- The editor layout was reorganized so the timeline always spans the full window width and the side panel no longer resizes it when opening or closing. _(11a61ba)_


## [0.19.0] — 2026-08-28

### Added
- Added a render queue: line up exports for multiple platforms with different settings, hit "Queue", and keep going — Reel renders them one after another while you walk away. The export dialog shows the currently rendering job, what's waiting, and a result for each finished export, with options to cancel everything or clear finished jobs.
- Added clip crossfades: set a crossfade duration on any clip to smoothly transition from the one before it. The timeline shows a visual wedge marking the overlap, and the exported video's duration correctly reflects the shortened, overlapped edit.
- Added reframing (zoom and pan) to clip effects, making it easy to punch into a landscape shot and pan the visible window so it fills a vertical or square frame without blurred bars on the sides. Adjustments are visible live in the preview and match exactly what's rendered on export.


## [0.18.0] — 2026-08-27

### Added
- You can now adjust exposure, contrast, and saturation per clip, plus add fade-in and fade-out — with live sliders in the clip inspector under a new "Look" section.
- The editor preview now shows your colour adjustments and fades exactly as they'll appear in the exported video, so what you see while editing matches the final render.

### Improved
- Timeline exports now apply each clip's colour and fade settings when rendering the final video, keeping the exported look consistent with the editor.


## [0.17.0] — 2026-08-27

### Added
- New one-click export destinations in the export window — pick "TikTok", "Reels / Shorts", "Instagram feed", "Square", "YouTube", "YouTube 4K", "Facebook" or "X / Twitter" and Reel automatically sets the right frame size, codec and quality for that platform. _(8c7a300)_
- Presets that change the video's aspect ratio (e.g. landscape to vertical) let you choose how the picture fits the new frame: letterboxed with bars, cropped to fill, or filled with a blurred backdrop — the same treatment TikTok and Reels use for landscape clips. _(8c7a300)_
- Exporting with a preset now suggests a filename like "myvideo-tiktok.mp4" so cuts for different platforms sit side by side without overwriting each other. _(8c7a300)_

### Improved
- Manual export settings are unaffected — choosing "Custom" keeps full control over resolution, codec and quality as before, with a per-fit option now available for exports with a fixed target frame. _(8c7a300)_


## [0.16.0] — 2026-08-27

### Added
- Laid groundwork in the new video pipeline for future compositing features like tinting, fades, and multi-track blending. _(8c64756)_

### Improved
- Video is now drawn through Reel's own GPU render pipeline instead of a generic texture blit, removing a per-pixel CPU alpha-fixup pass on every frame for smoother, more efficient playback. _(8c64756)_
- mpv now renders video at the exact on-screen size instead of always decoding at full source resolution, cutting rendering, copying and upload work dramatically when a large video is displayed at a smaller size (e.g. up to ~9x less work at 1280px display for a 4K source). _(8c64756)_


## [0.15.0] — 2026-08-27

### Added
- You can now build multi-source timelines: opening a file while editing an existing project adds it as a new clip instead of replacing the edit, and preview playback smoothly switches between source files as the playhead crosses clip boundaries. _(796a292)_
- Set an in/out export range on the timeline (I / O keys, or the new [ / ] toolbar buttons) to export just a portion of your edit. The selected range is shown dimmed on the timeline, and the export window offers a dedicated "Range" export option; Shift+I or Shift+O clears the markers. _(796a292)_
- Timeline exports can now mix clips from sources with different resolutions, frame rates, and codecs — everything is automatically normalised (scaled, letterboxed, and matched in frame rate and audio format) to the project's frame so the final render plays back cleanly. _(796a292)_
- Exporting can now use your GPU's hardware video encoder (NVIDIA NVENC or Apple VideoToolbox) for much faster renders, with automatic detection and a fallback to the software encoder when hardware isn't available or supported for the chosen codec. A new checkbox in the export window lets you toggle hardware encoding on or off. _(796a292)_

### Fixed
- Timeline exports no longer force an extra scaling pass after concatenation; the target resolution is now applied consistently while assembling each clip, avoiding quality loss on multi-clip exports. _(796a292)_


## [0.14.0] — 2026-08-27

### Added
- You can now export your edited timeline directly, not just the original source file. When a project has a cut on the timeline, the export dialog offers a choice between exporting the source or exporting the edit — trimmed clips are stitched together into a single rendered video. _(301d253)_
- Timeline exports get a smart default output name (e.g. "project-cut.mp4"), automatically avoiding overwriting existing files by adding a number suffix. _(301d253)_

### Changed
- When exporting the edited timeline, only standard video codecs (H.264, H.265, AV1, VP9) are offered, since a rendered cut is always a video file. _(301d253)_

### Improved
- The export dialog now shows how many clips and how many seconds are in your edit before you export it, and automatically carries over audio from your clips when rendering a timeline. _(301d253)_


## [0.13.2] — 2026-08-27

### Added
- Reel now has a real timeline editor: zoom and pan with the scroll wheel, click to select clips, drag clip bodies to move them and drag clip edges to trim, with snapping to nearby clip edges and the playhead. _(8094d7d)_
- Split (S) and delete (Del) clips directly on the timeline, plus full undo/redo (Ctrl+Z / Ctrl+Shift+Z) for all editing actions. _(8094d7d)_
- Save your edit as a .reel project file (Ctrl+S) and reopen it later — Reel reloads the timeline and its source media exactly where you left off. _(8094d7d)_
- The file open dialog now accepts .reel project files alongside media files. _(8094d7d)_

### Improved
- Playback in the editor now follows the sequence of clips on the timeline, automatically jumping to the next clip and skipping gaps instead of just playing the raw source file. _(8094d7d)_
- The timeline ruler now adapts its tick spacing to the current zoom level and shows readable time labels, and the editor toolbar gained zoom, split, delete, undo/redo, and save controls with a live unsaved-changes indicator. _(8094d7d)_
- Selecting a clip in the media/clips list now shows its position, length, and source in-point details in the side panel. _(8094d7d)_


## [0.13.1] — 2026-08-27

### Improved
- Opening a video or audio file no longer freezes the window — Reel now loads media on a background thread and shows an "Opening…" status while it works.
- App startup and cold-opening a file is noticeably faster: GPU backend selection skips unnecessary probing, and video decoding starts in software mode immediately, upgrading to hardware decoding a second later once playback is smoothly rolling.


## [0.13.0] — 2026-08-27

### Changed
- Updated the frame-step back/forward icons in the player controls for a cleaner look.

### Fixed
- Fixed the playback control overlay (seek bar, volume slider, buttons) rendering with an inflated, invisible layout in certain window sizes — controls now stay correctly bounded to the window.
- The volume slider no longer stretches to the full width of the window; it now displays at a normal, compact size.


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



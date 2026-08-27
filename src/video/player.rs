//! Playback state machine the UI talks to: play/pause/seek, one current frame.
//! Two backends live underneath, invisible above this API (the roadmap's
//! non-negotiable): **libmpv** when present (hardware decode, real A/V sync,
//! audio, frame-exact seek) and the v0.1 **ffmpeg-subprocess** decoder as the
//! universal fallback. `REEL_BACKEND=ffmpeg` forces the fallback.

use super::decoder::{self, DecodeHandle, Frame, VideoInfo};
use super::mpv::{self, MpvPlayer};
use crate::media::MediaKind;
use anyhow::Result;
use std::time::{Duration, Instant};

/// Audio visualizers — lavfi filter graphs mpv renders as the video track.
/// The gnarly stuff: musical spectrum, scrolling spectrogram, vectorscope,
/// waveform. All in the Pixygon palette where the filter allows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visualizer {
    Off,
    Cqt,
    Spectrum,
    Scope,
    Waves,
}

impl Visualizer {
    pub const ALL: [Visualizer; 5] =
        [Visualizer::Off, Visualizer::Cqt, Visualizer::Spectrum, Visualizer::Scope, Visualizer::Waves];

    pub fn label(self) -> &'static str {
        match self {
            Visualizer::Off => "Art / off",
            Visualizer::Cqt => "Spectrum bars",
            Visualizer::Spectrum => "Spectrogram",
            Visualizer::Scope => "Vectorscope",
            Visualizer::Waves => "Waveform",
        }
    }

    pub fn next(self) -> Self {
        let all = Self::ALL;
        let i = all.iter().position(|v| *v == self).unwrap_or(0);
        all[(i + 1) % all.len()]
    }

    fn graph(self) -> Option<(&'static str, (u32, u32))> {
        match self {
            Visualizer::Off => None,
            Visualizer::Cqt => Some((
                "[aid1]asplit[ao][a];[a]showcqt=s=1280x720:count=2:bar_g=2:sono_g=4[vo]",
                (1280, 720),
            )),
            Visualizer::Spectrum => Some((
                "[aid1]asplit[ao][a];[a]showspectrum=s=1280x720:mode=combined:color=fiery:scale=cbrt:slide=scroll[vo]",
                (1280, 720),
            )),
            Visualizer::Scope => Some((
                "[aid1]asplit[ao][a];[a]avectorscope=s=720x720:draw=line:scale=cbrt:zoom=1.5:rc=34:gc=211:bc=238[vo]",
                (720, 720),
            )),
            Visualizer::Waves => Some((
                "[aid1]asplit[ao][a];[a]showwaves=s=1280x720:mode=cline:colors=0x22D3EE|0xF43F5E:scale=sqrt[vo]",
                (1280, 720),
            )),
        }
    }
}

enum Backend {
    Mpv(MpvPlayer),
    /// ffmpeg subprocess: decode handle + wall-clock anchor (instant, media
    /// time at that instant) that paces the frame pull.
    Subprocess {
        decode: Option<DecodeHandle>,
        anchor: Option<(Instant, f64)>,
    },
}

pub struct Player {
    pub path: String,
    pub info: VideoInfo,
    /// Video, or Audio (pure audio and audio-with-cover-art both count —
    /// cover art still renders as `current` frames).
    pub kind: MediaKind,
    backend: Backend,
    pub playing: bool,
    /// Playback position in seconds.
    pub position: f64,
    /// The most recently decoded frame ready to show (RGBA8).
    pub current: Option<Frame>,
    /// True once playback has run out at end of file.
    pub ended: bool,
    /// 0–130 (100 = source level; mpv can amplify). No effect on the
    /// video-only subprocess fallback.
    pub volume: f64,
    pub muted: bool,
    /// Playback rate, 1.0 = realtime.
    pub speed: f64,
    pub looping: bool,
    /// Active audio visualizer (mpv backend, audio media).
    pub visualizer: Visualizer,
    dirty: bool,
    /// Redraws are requested until this instant even while paused, so frames
    /// that land asynchronously (open, seek) reach the screen.
    active_until: Instant,
}

impl Player {
    pub fn open(path: &str) -> Result<Self> {
        let (info, kind, backend) = match mpv::lib().map(|lib| MpvPlayer::open(lib, path)) {
            Some(Ok(p)) => {
                let info = p.info.clone();
                let kind = if p.has_video && !p.albumart { MediaKind::Video } else { MediaKind::Audio };
                (info, kind, Backend::Mpv(p))
            }
            Some(Err(e)) => {
                log::warn!("libmpv open failed ({e}); falling back to ffmpeg subprocess");
                Self::open_subprocess(path)?
            }
            None => Self::open_subprocess(path)?,
        };
        let mut player = Self {
            path: path.to_string(),
            info,
            kind,
            backend,
            playing: false,
            position: 0.0,
            current: None,
            ended: false,
            volume: 100.0,
            muted: false,
            speed: 1.0,
            looping: false,
            visualizer: Visualizer::Off,
            dirty: true,
            active_until: Instant::now() + Duration::from_millis(500),
        };
        // Pure audio (no cover art) gets a visualizer by default — a player
        // should never be a blank rectangle.
        if player.kind == MediaKind::Audio && player.current.is_none() {
            if let Backend::Mpv(m) = &player.backend {
                if !m.has_video {
                    player.set_visualizer(Visualizer::Cqt);
                }
            }
        }
        Ok(player)
    }

    fn open_subprocess(path: &str) -> Result<(VideoInfo, MediaKind, Backend)> {
        // The subprocess pipeline is video-only; audio files need libmpv.
        let info = decoder::probe(path)?;
        let decode = decoder::spawn(path, 0.0, &info)?;
        Ok((info, MediaKind::Video, Backend::Subprocess { decode: Some(decode), anchor: None }))
    }

    /// Which decode backend is live — for the status line / logs.
    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            Backend::Mpv(_) => "mpv",
            Backend::Subprocess { .. } => "ffmpeg",
        }
    }

    pub fn toggle_play(&mut self) {
        if self.ended && !self.playing {
            self.seek(0.0); // replay from the top, VLC-style
        }
        self.playing = !self.playing;
        match &mut self.backend {
            Backend::Mpv(m) => m.set_pause(!self.playing),
            Backend::Subprocess { anchor, .. } => *anchor = None, // re-anchor on next update
        }
        self.touch();
    }

    pub fn seek(&mut self, secs: f64) {
        let target = secs.clamp(0.0, self.info.duration.max(0.0));
        self.position = target;
        self.ended = false;
        match &mut self.backend {
            Backend::Mpv(m) => m.seek(target),
            Backend::Subprocess { decode, anchor } => {
                *anchor = None;
                // Restart decode from the seek point (drops the old handle → stops it).
                if let Ok(d) = decoder::spawn(&self.path, target, &self.info) {
                    *decode = Some(d);
                }
            }
        }
        self.dirty = true;
        self.touch();
    }

    /// Seek relative to the current position (shortcut keys, jog wheel).
    pub fn seek_by(&mut self, delta: f64) {
        self.seek(self.position + delta);
    }

    /// Step exactly one frame; pauses playback (that's what stepping is for).
    pub fn frame_step(&mut self, forward: bool) {
        self.playing = false;
        match &mut self.backend {
            Backend::Mpv(m) => {
                m.frame_step(forward); // mpv pauses itself as part of the step
            }
            Backend::Subprocess { .. } => {
                let dt = 1.0 / self.info.fps.max(1.0);
                self.seek(self.position + if forward { dt } else { -dt });
            }
        }
        self.touch();
    }

    pub fn set_volume(&mut self, vol: f64) {
        self.volume = vol.clamp(0.0, 130.0);
        if let Backend::Mpv(m) = &mut self.backend {
            m.set_volume(self.volume);
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if let Backend::Mpv(m) = &mut self.backend {
            m.set_muted(muted);
        }
    }

    pub fn set_speed(&mut self, speed: f64) {
        let speed = speed.clamp(0.25, 4.0);
        match &mut self.backend {
            Backend::Mpv(m) => m.set_speed(speed),
            Backend::Subprocess { anchor, .. } => *anchor = None, // re-anchor at the new rate
        }
        self.speed = speed;
    }

    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
        if let Backend::Mpv(m) = &mut self.backend {
            m.set_looping(looping);
        }
    }

    /// Whether this backend produces sound at all (the subprocess fallback is
    /// video-only, so its volume controls would lie).
    pub fn has_audio(&self) -> bool {
        matches!(self.backend, Backend::Mpv(_))
    }

    /// Visualizers apply to audio media on the mpv backend.
    pub fn supports_visualizer(&self) -> bool {
        self.kind == MediaKind::Audio && matches!(self.backend, Backend::Mpv(_))
    }

    pub fn set_visualizer(&mut self, v: Visualizer) {
        let Backend::Mpv(m) = &mut self.backend else { return };
        m.set_visualizer(v.graph());
        // The old frame is the wrong size/content now; drop it.
        self.current = None;
        self.visualizer = v;
        self.dirty = true;
        self.touch();
    }

    /// Whether seeking is cheap enough to fire on every pointer move while
    /// scrubbing (mpv coalesces seeks; the subprocess respawns ffmpeg).
    pub fn cheap_seek(&self) -> bool {
        matches!(self.backend, Backend::Mpv(_))
    }

    /// Whether a fresh frame was produced since the last `take_dirty`.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Should the app keep requesting redraws? True while playing, and for a
    /// short grace window after open/seek/toggle so async frames get shown.
    pub fn wants_redraw(&self) -> bool {
        self.playing || Instant::now() < self.active_until
    }

    fn touch(&mut self) {
        self.active_until = Instant::now() + Duration::from_millis(500);
    }

    /// Advance playback. Call once per UI frame.
    pub fn update(&mut self) {
        match &mut self.backend {
            Backend::Mpv(_) => self.update_mpv(),
            Backend::Subprocess { .. } => self.update_subprocess(),
        }
        // Loop for the fallback backend (mpv loops internally via loop-file).
        if self.ended && self.looping {
            self.seek(0.0);
            self.ended = false;
            if !self.playing {
                self.toggle_play();
            }
        }
    }

    fn update_mpv(&mut self) {
        let Backend::Mpv(m) = &mut self.backend else { return };
        if m.update(&mut self.current) {
            if let Some(f) = &self.current {
                self.position = f.pts;
            }
            self.dirty = true;
            self.active_until = Instant::now() + Duration::from_millis(500);
        } else if self.playing {
            self.position = m.position();
        }
        if m.eof_reached() {
            if self.playing {
                m.set_pause(true);
            }
            self.playing = false;
            self.ended = true;
        }
    }

    fn update_subprocess(&mut self) {
        let Backend::Subprocess { decode, anchor } = &mut self.backend else { return };
        let Some(decode) = decode else { return };

        // Anchor wall-clock to media time when (re)starting playback.
        let target = if self.playing {
            match anchor {
                Some((inst, base)) => *base + inst.elapsed().as_secs_f64() * self.speed,
                None => {
                    *anchor = Some((Instant::now(), self.position));
                    self.position
                }
            }
        } else {
            // Paused: still show the first decoded frame if we don't have one.
            self.position
        };

        let mut newest: Option<Frame> = None;
        // Drain frames whose pts <= target (or just one if paused/no frame yet).
        loop {
            match decode.rx.try_recv() {
                Ok(frame) => {
                    let show_now = frame.pts <= target + 1e-6;
                    let had_none = self.current.is_none() && newest.is_none();
                    let take = show_now || had_none || !self.playing;
                    if take {
                        self.position = frame.pts;
                        newest = Some(frame);
                        if !self.playing {
                            break; // one frame is enough when paused
                        }
                    } else {
                        // This frame is in the future; stash it as newest and stop.
                        self.position = self.position.max(frame.pts - 1.0 / self.info.fps);
                        newest = Some(frame);
                        break;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    if self.playing && self.current.is_some() {
                        self.ended = true;
                        self.playing = false;
                    }
                    break;
                }
            }
        }

        if let Some(f) = newest {
            self.current = Some(f);
            self.dirty = true;
        }

        if self.info.duration > 0.0 && self.position >= self.info.duration {
            self.ended = true;
            self.playing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audio files are first-class: open one through the same Player and it
    /// plays (position advances on mpv's clock, no video frames required).
    #[test]
    fn audio_file_opens_and_plays() {
        if mpv::lib().is_none() {
            eprintln!("libmpv not installed — skipping audio playback test");
            return;
        }
        // Generate a 2 s tone on the fly; keeps binary fixtures out of git.
        let path = std::env::temp_dir().join(format!("reel-audio-test-{}.m4a", std::process::id()));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                   "-c:a", "aac", &path.to_string_lossy()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "ffmpeg could not generate the audio fixture");

        let mut p = Player::open(&path.to_string_lossy()).expect("open audio");
        assert_eq!(p.kind, MediaKind::Audio);
        assert!(p.info.duration > 1.5 && p.info.duration < 2.5, "≈2s, got {}", p.info.duration);
        assert!(p.has_audio());

        p.toggle_play();
        let deadline = Instant::now() + Duration::from_secs(10);
        while p.position < 0.3 && Instant::now() < deadline {
            p.update();
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(p.position >= 0.3, "audio position should advance, got {}", p.position);

        // A pure-audio file auto-enables a visualizer, whose frames flow
        // through the normal frame path at the graph's size.
        assert_eq!(p.visualizer, Visualizer::Cqt);
        let deadline = Instant::now() + Duration::from_secs(10);
        while p.current.is_none() && Instant::now() < deadline {
            p.update();
            std::thread::sleep(Duration::from_millis(30));
        }
        let frame = p.current.as_ref().expect("visualizer should render frames");
        assert_eq!((frame.width, frame.height), (1280, 720));

        // Switching visualizers re-routes the graph without falling over.
        p.set_visualizer(Visualizer::Waves);
        let deadline = Instant::now() + Duration::from_secs(10);
        while p.current.is_none() && Instant::now() < deadline {
            p.update();
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(p.current.is_some(), "waveform visualizer should render");
        let _ = std::fs::remove_file(&path);
    }
}

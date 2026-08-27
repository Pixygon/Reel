//! Playback state machine the UI talks to: play/pause/seek, one current frame.
//! Two backends live underneath, invisible above this API (the roadmap's
//! non-negotiable): **libmpv** when present (hardware decode, real A/V sync,
//! audio, frame-exact seek) and the v0.1 **ffmpeg-subprocess** decoder as the
//! universal fallback. `REEL_BACKEND=ffmpeg` forces the fallback.

use super::decoder::{self, DecodeHandle, Frame, VideoInfo};
use super::mpv::{self, MpvPlayer};
use anyhow::Result;
use std::time::{Duration, Instant};

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
    backend: Backend,
    pub playing: bool,
    /// Playback position in seconds.
    pub position: f64,
    /// The most recently decoded frame ready to show (RGBA8).
    pub current: Option<Frame>,
    /// True once playback has run out at end of file.
    pub ended: bool,
    dirty: bool,
    /// Redraws are requested until this instant even while paused, so frames
    /// that land asynchronously (open, seek) reach the screen.
    active_until: Instant,
}

impl Player {
    pub fn open(path: &str) -> Result<Self> {
        let (info, backend) = match mpv::lib().map(|lib| MpvPlayer::open(lib, path)) {
            Some(Ok(p)) => {
                let info = p.info.clone();
                (info, Backend::Mpv(p))
            }
            Some(Err(e)) => {
                log::warn!("libmpv open failed ({e}); falling back to ffmpeg subprocess");
                Self::open_subprocess(path)?
            }
            None => Self::open_subprocess(path)?,
        };
        Ok(Self {
            path: path.to_string(),
            info,
            backend,
            playing: false,
            position: 0.0,
            current: None,
            ended: false,
            dirty: true,
            active_until: Instant::now() + Duration::from_millis(500),
        })
    }

    fn open_subprocess(path: &str) -> Result<(VideoInfo, Backend)> {
        let info = decoder::probe(path)?;
        let decode = decoder::spawn(path, 0.0, &info)?;
        Ok((info, Backend::Subprocess { decode: Some(decode), anchor: None }))
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
                Some((inst, base)) => *base + inst.elapsed().as_secs_f64(),
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

//! Playback state machine over the decoder: play/pause/seek, wall-clock paced
//! frame pull, with the most-recent decoded frame kept for display.

use super::decoder::{self, DecodeHandle, Frame, VideoInfo};
use anyhow::Result;
use std::time::Instant;

pub struct Player {
    pub path: String,
    pub info: VideoInfo,
    decode: Option<DecodeHandle>,
    pub playing: bool,
    /// Playback position in seconds.
    pub position: f64,
    /// The most recently decoded frame ready to show (RGBA8).
    pub current: Option<Frame>,
    /// True once the decoder has run dry at end of file.
    pub ended: bool,
    /// Wall-clock anchor: (instant, media-time-at-that-instant).
    anchor: Option<(Instant, f64)>,
    dirty: bool,
}

impl Player {
    pub fn open(path: &str) -> Result<Self> {
        let info = decoder::probe(path)?;
        let decode = decoder::spawn(path, 0.0, &info)?;
        Ok(Self {
            path: path.to_string(),
            info,
            decode: Some(decode),
            playing: false,
            position: 0.0,
            current: None,
            ended: false,
            anchor: None,
            dirty: true,
        })
    }

    pub fn toggle_play(&mut self) {
        self.playing = !self.playing;
        self.anchor = None; // re-anchor on next update
    }

    pub fn seek(&mut self, secs: f64) {
        let target = secs.clamp(0.0, self.info.duration.max(0.0));
        self.position = target;
        self.ended = false;
        self.anchor = None;
        // Restart decode from the seek point (drops the old handle → stops it).
        if let Ok(d) = decoder::spawn(&self.path, target, &self.info) {
            self.decode = Some(d);
        }
        self.dirty = true;
    }

    /// Whether a fresh frame was produced since the last `take_dirty`.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Advance playback. Call once per UI frame. Pulls decoded frames up to the
    /// current wall-clock target and keeps the latest for display.
    pub fn update(&mut self) {
        let Some(decode) = &self.decode else { return };

        // Anchor wall-clock to media time when (re)starting playback.
        let target = if self.playing {
            match self.anchor {
                Some((inst, base)) => base + inst.elapsed().as_secs_f64(),
                None => {
                    self.anchor = Some((Instant::now(), self.position));
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

//! The editing data model — an NLE project of tracks and clips on a timeline.
//! v0.1 defines the model and renders it (see ui::timeline); trimming, ripple,
//! effects and export are on the roadmap. Kept serde-serializable so a project
//! is a saveable `.reel` document from the start.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TrackKind {
    Video,
    Audio,
}

/// One clip placed on a track: a window `[in_point, in_point+duration)` of a
/// source media file, positioned at `start` on the timeline. All times seconds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub id: u64,
    pub name: String,
    pub source: String, // media file path
    pub start: f64,     // timeline position
    pub in_point: f64,  // offset into the source
    pub duration: f64,
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub muted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub tracks: Vec<Track>,
    #[serde(skip)]
    next_id: u64,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "Untitled".into(),
            fps: 30.0,
            width: 1920,
            height: 1080,
            tracks: vec![
                Track { id: 1, name: "V1".into(), kind: TrackKind::Video, clips: vec![], muted: false },
                Track { id: 2, name: "A1".into(), kind: TrackKind::Audio, clips: vec![], muted: false },
            ],
            next_id: 100,
        }
    }
}

impl Project {
    /// Total timeline length = the furthest clip end across all tracks.
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.end())
            .fold(0.0, f64::max)
    }

    /// Append a clip to the end of the first video track (v0.1 "add to timeline").
    pub fn append_video(&mut self, name: &str, source: &str, duration: f64) {
        self.append(TrackKind::Video, name, source, duration);
    }

    /// Append a clip to the end of the first audio track.
    pub fn append_audio(&mut self, name: &str, source: &str, duration: f64) {
        self.append(TrackKind::Audio, name, source, duration);
    }

    fn append(&mut self, kind: TrackKind, name: &str, source: &str, duration: f64) {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(track) = self.tracks.iter_mut().find(|t| t.kind == kind) {
            let start = track.clips.iter().map(|c| c.end()).fold(0.0, f64::max);
            track.clips.push(Clip {
                id,
                name: name.into(),
                source: source.into(),
                start,
                in_point: 0.0,
                duration,
            });
        }
    }
}

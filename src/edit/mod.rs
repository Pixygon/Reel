//! The editing data model — an NLE project of tracks and clips on a timeline.
//! v0.1 defines the model and renders it (see ui::timeline); trimming, ripple,
//! effects and export are on the roadmap. Kept serde-serializable so a project
//! is a saveable `.reel` document from the start.

use crate::effects::Effects;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    /// Colour adjustments and fades for this clip. Defaults to identity, and
    /// is `serde(default)` so `.reel` files written before effects still load.
    #[serde(default)]
    pub effects: Effects,
    /// Seconds of crossfade FROM the previous clip into this one. The two
    /// clips overlap by this much, so the edit gets shorter — exactly what
    /// ffmpeg's `xfade` does at render time.
    #[serde(default)]
    pub transition_in: f64,
    /// Level change for this clip's audio, in decibels. 0 is untouched.
    #[serde(default)]
    pub gain_db: f32,
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }
}

/// Write a document without ever leaving a half-written file behind: write a
/// sibling temp file, then rename over the target (rename is atomic on the
/// same filesystem). Autosave runs constantly, so a crash mid-write must not
/// be able to destroy someone's project.
pub fn write_atomic(path: &str, contents: &str) -> anyhow::Result<()> {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// How long the flattened edit actually renders to: crossfades overlap their
/// two clips, so each one shortens the result by its own length. The export
/// dialog and the progress bar both use this, so the duration a user sees is
/// the duration they get.
pub fn render_duration(segments: &[Segment]) -> f64 {
    let total: f64 = segments.iter().map(|s| s.duration).sum();
    let overlap: f64 = segments
        .iter()
        .enumerate()
        .map(|(i, s)| if i == 0 { 0.0 } else { s.transition_in.min(segments[i - 1].duration).min(s.duration) })
        .sum();
    (total - overlap).max(0.0)
}

/// One piece of the flattened edit, ready to render: a window of a source
/// file plus the effects that piece carries.
#[derive(Clone, Debug)]
pub struct Segment {
    pub source: String,
    pub in_point: f64,
    pub duration: f64,
    pub effects: Effects,
    /// Crossfade from the previous segment, in seconds (0 = hard cut).
    pub transition_in: f64,
    /// Level change for this clip's own audio, in decibels.
    pub gain_db: f32,
}

/// A music bed laid under the edit.
///
/// Kept deliberately small: one track, a level, and ducking. Music under
/// speech is the thing editors actually do, and the thing they most often
/// get wrong by hand — so Reel does the level-riding itself rather than
/// making you draw a volume curve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Music {
    pub source: String,
    /// Where it starts on the timeline, in seconds.
    #[serde(default)]
    pub start: f64,
    /// Level, in decibels.
    pub gain_db: f32,
    /// Pull the music down automatically whenever the edit's own audio
    /// speaks over it.
    pub duck: bool,
    /// Fade the bed in and out, in seconds (0 = hard start/stop).
    #[serde(default)]
    pub fade: f64,
}

impl Default for Music {
    fn default() -> Self {
        Self { source: String::new(), start: 0.0, gain_db: -12.0, duck: true, fade: 1.0 }
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
    /// Captions for the edit, in TIMELINE time. Generated locally; burned in
    /// at export. `serde(default)` so older documents still load.
    #[serde(default)]
    pub captions: Vec<crate::captions::Cue>,
    #[serde(default = "default_caption_size")]
    pub caption_size: u32,
    /// Text placed on the picture by hand, in TIMELINE time.
    #[serde(default)]
    pub titles: Vec<crate::titles::Title>,
    /// An optional music bed under the whole edit.
    #[serde(default)]
    pub music: Option<Music>,
    /// Timeline positions flagged by the user. Part of the document: a
    /// marker you dropped yesterday should still be there today.
    #[serde(default)]
    pub markers: Vec<f64>,
    #[serde(skip)]
    next_id: u64,
}

fn default_caption_size() -> u32 {
    20
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
            captions: Vec::new(),
            caption_size: default_caption_size(),
            titles: Vec::new(),
            music: None,
            markers: Vec::new(),
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
                effects: Effects::default(),
                transition_in: 0.0,
                gain_db: 0.0,
            });
        }
    }

    /// The caption showing at timeline time `t`, if any.
    pub fn caption_at(&self, t: f64) -> Option<&crate::captions::Cue> {
        self.captions.iter().find(|c| c.start <= t && t < c.end)
    }

    pub fn clip(&self, id: u64) -> Option<&Clip> {
        self.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: u64) -> Option<&mut Clip> {
        self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == id)
    }

    /// The clip on the given kind of track under timeline time `t`.
    pub fn clip_at(&self, kind: TrackKind, t: f64) -> Option<&Clip> {
        self.tracks
            .iter()
            .filter(|tr| tr.kind == kind)
            .flat_map(|tr| tr.clips.iter())
            .find(|c| c.start <= t && t < c.end())
    }

    /// The clip that ends at or before `t` on this track — the one a
    /// crossfade would come from.
    pub fn clip_before(&self, kind: TrackKind, t: f64) -> Option<&Clip> {
        self.tracks
            .iter()
            .filter(|tr| tr.kind == kind)
            .flat_map(|tr| tr.clips.iter())
            .filter(|c| c.end() <= t + 1e-6)
            .max_by(|a, b| a.end().total_cmp(&b.end()))
    }

    /// The next clip (by start) on the given track kind strictly after `t`.
    pub fn clip_after(&self, kind: TrackKind, t: f64) -> Option<&Clip> {
        self.tracks
            .iter()
            .filter(|tr| tr.kind == kind)
            .flat_map(|tr| tr.clips.iter())
            .filter(|c| c.start > t + 1e-9)
            .min_by(|a, b| a.start.total_cmp(&b.start))
    }

    /// Split every clip containing timeline time `t` into two at `t`.
    /// Returns how many clips were split.
    pub fn split_at(&mut self, t: f64) -> usize {
        const MIN: f64 = 0.05;
        let mut split = 0;
        let mut new_id = self.next_id;
        for track in &mut self.tracks {
            let mut additions = Vec::new();
            for clip in &mut track.clips {
                if clip.start + MIN < t && t < clip.end() - MIN {
                    let cut = t - clip.start; // offset into the clip
                    let mut right = clip.clone();
                    right.id = new_id;
                    new_id += 1;
                    right.start = t;
                    right.in_point = clip.in_point + cut;
                    right.duration = clip.duration - cut;
                    clip.duration = cut;
                    additions.push(right);
                    split += 1;
                }
            }
            track.clips.extend(additions);
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
        self.next_id = new_id;
        split
    }

    /// Slide `id` left until it touches the clip before it, closing the gap.
    /// Returns the seconds removed. Clips after it are NOT moved — use
    /// `close_all_gaps` for a ripple.
    pub fn close_gap_before(&mut self, id: u64) -> f64 {
        let Some(clip) = self.clip(id) else { return 0.0 };
        let (kind, start) = (
            self.tracks
                .iter()
                .find(|t| t.clips.iter().any(|c| c.id == id))
                .map(|t| t.kind.clone())
                .unwrap_or(TrackKind::Video),
            clip.start,
        );
        let prev_end = self
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.id != id && c.end() <= start + 1e-6)
            .map(|c| c.end())
            .fold(0.0f64, f64::max);
        let gap = start - prev_end;
        if gap <= 1e-6 {
            return 0.0;
        }
        if let Some(c) = self.clip_mut(id) {
            c.start -= gap;
        }
        gap
    }

    /// Butt every clip on every track up against its predecessor: no gaps
    /// anywhere, order preserved. Returns the total seconds removed.
    pub fn close_all_gaps(&mut self) -> f64 {
        let mut removed = 0.0;
        for track in &mut self.tracks {
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
            let mut cursor = 0.0f64;
            for clip in &mut track.clips {
                let gap = clip.start - cursor;
                if gap > 1e-6 {
                    clip.start -= gap;
                    removed += gap;
                }
                cursor = clip.end();
            }
        }
        removed
    }

    /// Shift every clip that starts at or after `from` left by `amount`, on
    /// EVERY track — so video and audio stay in sync through a ripple.
    fn ripple_from(&mut self, from: f64, amount: f64) {
        if amount <= 0.0 {
            return;
        }
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if clip.start >= from - 1e-6 {
                    clip.start = (clip.start - amount).max(0.0);
                }
            }
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
    }

    /// Remove a clip and close the hole behind it — Shift+Delete in every
    /// NLE. Clips on OTHER tracks lying inside the same span go too (video
    /// and audio arrive as linked pairs, so removing a shot must remove its
    /// sound); rippling one track while leaving another would overlap clips.
    /// Returns the seconds removed from the timeline.
    pub fn ripple_delete(&mut self, id: u64) -> f64 {
        let Some(clip) = self.clip(id) else { return 0.0 };
        let (start, end, duration) = (clip.start, clip.end(), clip.duration);
        for track in &mut self.tracks {
            track
                .clips
                .retain(|c| c.id != id && !(c.start >= start - 1e-6 && c.end() <= end + 1e-6));
        }
        self.ripple_from(end - 1e-9, duration);
        duration
    }

    /// Trim the clip under `t` back to the playhead and close the gap —
    /// Q (trim the head) and W (trim the tail) in Premiere's keymap.
    /// Returns the seconds removed.
    pub fn ripple_trim_to_playhead(&mut self, t: f64, head: bool) -> f64 {
        const MIN: f64 = 0.05;
        // Take the video track's clip as the reference for how much to remove,
        // then ripple every track by that amount so nothing drifts.
        let Some(clip) = self.clip_at(TrackKind::Video, t) else { return 0.0 };
        let (id, start, end) = (clip.id, clip.start, clip.end());
        let removed = if head { t - start } else { end - t };
        if removed <= MIN || (end - start) - removed < MIN {
            return 0.0;
        }
        // Apply the same trim to every track's clip under the playhead, so
        // picture and sound stay locked together.
        let ids: Vec<u64> = self
            .tracks
            .iter()
            .flat_map(|tr| tr.clips.iter())
            .filter(|c| c.start <= t && t < c.end())
            .map(|c| c.id)
            .collect();
        let _ = id;
        for cid in ids {
            if let Some(c) = self.clip_mut(cid) {
                if head {
                    let d = (t - c.start).min(c.duration - MIN).max(0.0);
                    c.in_point += d;
                    c.duration -= d;
                    c.start = t;
                } else {
                    let d = (c.end() - t).min(c.duration - MIN).max(0.0);
                    c.duration -= d;
                }
            }
        }
        if head {
            self.ripple_from(t - 1e-9, removed);
        } else {
            self.ripple_from(end - 1e-9, removed);
        }
        removed
    }

    /// Paste `clip` onto the track it came from at timeline position `at`,
    /// making room by pushing everything from that point along.
    ///
    /// Insert rather than overwrite: pasting should never silently eat
    /// footage you already placed. Rippling keeps the rest of the cut
    /// intact — the same behaviour as pasting a word into a sentence.
    pub fn paste_clip(&mut self, clip: &Clip, at: f64, kind: TrackKind) -> u64 {
        let at = at.max(0.0);
        // An insert edit, the way every NLE does it: split whatever straddles
        // the insertion point first, so no clip is left half-covered, then
        // open a gap on EVERY track. Shifting only the pasted clip's own
        // track would slide the audio out of sync with the picture.
        self.split_at(at);
        for track in &mut self.tracks {
            for c in &mut track.clips {
                if c.start >= at - 1e-6 {
                    c.start += clip.duration;
                }
            }
        }

        let id = self.next_id;
        self.next_id += 1;
        let mut placed = clip.clone();
        placed.id = id;
        placed.start = at;
        // A crossfade describes a join with a particular neighbour; carrying
        // it to a new position would fade into whatever happens to be there.
        placed.transition_in = 0.0;
        if let Some(track) = self.tracks.iter_mut().find(|t| t.kind == kind) {
            track.clips.push(placed);
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
        id
    }

    /// Copy a clip and drop the copy straight after the original.
    pub fn duplicate_clip(&mut self, id: u64) -> Option<u64> {
        let (clip, kind) = self.tracks.iter().find_map(|t| {
            t.clips.iter().find(|c| c.id == id).map(|c| (c.clone(), t.kind))
        })?;
        Some(self.paste_clip(&clip, clip.end(), kind))
    }

    /// The clip with this id, and which kind of track it lives on.
    pub fn clip_with_kind(&self, id: u64) -> Option<(Clip, TrackKind)> {
        self.tracks.iter().find_map(|t| {
            t.clips.iter().find(|c| c.id == id).map(|c| (c.clone(), t.kind))
        })
    }

    pub fn delete_clip(&mut self, id: u64) -> bool {
        for track in &mut self.tracks {
            let before = track.clips.len();
            track.clips.retain(|c| c.id != id);
            if track.clips.len() != before {
                return true;
            }
        }
        false
    }

    /// Legal range for a clip's `start` when moving: between its neighbours
    /// on the same track (overlaps are not allowed).
    pub fn move_range(&self, id: u64) -> (f64, f64) {
        for track in &self.tracks {
            if let Some(clip) = track.clips.iter().find(|c| c.id == id) {
                let mut lo = 0.0f64;
                let mut hi = f64::INFINITY;
                for other in &track.clips {
                    if other.id == id {
                        continue;
                    }
                    if other.end() <= clip.start + 1e-9 {
                        lo = lo.max(other.end());
                    } else if other.start + 1e-9 >= clip.end() {
                        hi = hi.min(other.start - clip.duration);
                    }
                }
                return (lo, hi.max(lo));
            }
        }
        (0.0, f64::INFINITY)
    }

    /// Every interesting time to snap against: clip edges (excluding `skip`)
    /// and timeline zero.
    pub fn snap_targets(&self, skip: Option<u64>) -> Vec<f64> {
        let mut v = vec![0.0];
        for track in &self.tracks {
            for c in &track.clips {
                if Some(c.id) == skip {
                    continue;
                }
                v.push(c.start);
                v.push(c.end());
            }
        }
        v
    }

    /// Map a source-media position to timeline time via a clip of `source`,
    /// preferring the clip whose source window contains `pos`.
    pub fn source_to_timeline(&self, source: &str, pos: f64) -> Option<f64> {
        self.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.source == source)
            .find(|c| c.in_point <= pos && pos <= c.in_point + c.duration)
            .map(|c| c.start + (pos - c.in_point))
    }

    /// Map a window of SOURCE time onto every place it appears on the
    /// timeline, clipped to the clip that carries it.
    ///
    /// Captions are generated against the original recording, but the edit
    /// may have trimmed it, split it, reordered it, or used the same moment
    /// twice — so a source window can land in zero, one, or several places,
    /// and must never spill past the cut it belongs to.
    pub fn map_source_window(&self, source: &str, a: f64, b: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for track in self.tracks.iter().filter(|t| t.kind == TrackKind::Video) {
            for c in &track.clips {
                if c.source != source {
                    continue;
                }
                let (cs, ce) = (c.in_point, c.in_point + c.duration);
                let (lo, hi) = (a.max(cs), b.min(ce));
                if hi <= lo {
                    continue;
                }
                out.push((c.start + (lo - cs), c.start + (hi - cs)));
            }
        }
        out.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// The edit flattened for export: V1 clips in timeline order as
    /// (source, in_point, duration). Gaps are collapsed — exactly how editor
    /// playback sequences the cut.
    pub fn export_segments(&self) -> Vec<Segment> {
        self.export_segments_range(None, None)
    }

    /// Like `export_segments`, restricted to the timeline window
    /// [`range_in`, `range_out`] — clips are cut at the boundaries so an
    /// in/out range exports exactly what the markers enclose.
    pub fn export_segments_range(
        &self,
        range_in: Option<f64>,
        range_out: Option<f64>,
    ) -> Vec<Segment> {
        let lo = range_in.unwrap_or(f64::NEG_INFINITY);
        let hi = range_out.unwrap_or(f64::INFINITY);
        let mut clips: Vec<&Clip> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| t.clips.iter())
            .collect();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        clips
            .into_iter()
            .filter_map(|c| {
                let start = c.start.max(lo);
                let end = c.end().min(hi);
                if end - start <= 0.01 {
                    return None; // outside the range (or a sliver)
                }
                let head = start - c.start; // trimmed off the clip's front
                Some(Segment {
                    source: c.source.clone(),
                    in_point: c.in_point + head,
                    duration: end - start,
                    effects: c.effects,
                    // A clip cut into by the range marker loses its
                    // transition — there's no longer a clip to fade from.
                    transition_in: if head > 0.01 { 0.0 } else { c.transition_in },
                    gain_db: c.gain_db,
                })
            })
            .collect()
    }

    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        write_atomic(path, &serde_json::to_string_pretty(self)?)
    }

    pub fn load(path: &str) -> anyhow::Result<Self> {
        let mut p: Project = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        // next_id is serde(skip); re-seed above every stored id.
        p.next_id = p
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.id)
            .max()
            .unwrap_or(99)
            + 1;
        Ok(p)
    }
}

/// Per-session editor state: zoom/scroll, selection, drag-in-progress, the
/// timeline playhead (timeline seconds — NOT source seconds), and undo/redo
/// as whole-model snapshots (the model is small; snapshots are simple and
/// unbreakable).
pub struct EditorState {
    pub px_per_s: f32,
    pub scroll_x: f32,
    pub selected: Option<u64>,
    pub drag: Option<Drag>,
    /// Timeline position of the playhead, in seconds.
    pub playhead: f64,
    /// Clip currently feeding the preview during editor playback.
    pub active_clip: Option<u64>,
    /// Export range markers, in timeline seconds (I / O keys).
    pub range_in: Option<f64>,
    pub range_out: Option<f64>,
    pub dirty: bool,
    pub project_path: Option<String>,
    /// The clip whose effect sliders are mid-drag — one undo step per gesture.
    pub fx_gesture: Option<u64>,
    /// Index of the title being edited, if any — it shows a box in the
    /// preview and is the one you can drag around.
    pub selected_title: Option<usize>,
    /// The copied clip, and which kind of track it came from.
    pub clipboard: Option<(Clip, TrackKind)>,
    /// When the project last changed — autosave waits for a quiet moment.
    pub changed_at: std::time::Instant,
    /// Have we told the user where the project is being saved? (Once only.)
    pub announced_path: bool,
    undo: Vec<Project>,
    redo: Vec<Project>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Drag {
    Move { id: u64, grab: f64 },
    TrimL { id: u64 },
    TrimR { id: u64 },
    Playhead,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            px_per_s: 60.0,
            scroll_x: 0.0,
            selected: None,
            drag: None,
            playhead: 0.0,
            active_clip: None,
            range_in: None,
            range_out: None,
            dirty: false,
            project_path: None,
            fx_gesture: None,
            selected_title: None,
            clipboard: None,
            changed_at: std::time::Instant::now(),
            announced_path: false,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

impl EditorState {
    /// Snapshot before a mutating operation.
    pub fn push_undo(&mut self, project: &Project) {
        self.undo.push(project.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.mark_changed();
    }

    /// Something about the project changed — autosave will pick it up.
    pub fn mark_changed(&mut self) {
        self.dirty = true;
        self.changed_at = std::time::Instant::now();
    }

    pub fn undo(&mut self, project: &mut Project) -> bool {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(project, prev));
            self.selected = None;
            self.mark_changed();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self, project: &mut Project) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(project, next));
            self.selected = None;
            self.mark_changed();
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Snap `t` to the nearest target within `tolerance` seconds.
    /// Returns (possibly snapped t, the target hit).
    pub fn snap(t: f64, targets: &[f64], tolerance: f64) -> (f64, Option<f64>) {
        let mut best: Option<f64> = None;
        for &target in targets {
            let d = (t - target).abs();
            if d <= tolerance && best.map_or(true, |b| d < (t - b).abs()) {
                best = Some(target);
            }
        }
        (best.unwrap_or(t), best)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_clip_project() -> Project {
        let mut p = Project::default();
        p.append_video("a", "/tmp/a.mp4", 10.0);
        p.append_audio("a", "/tmp/a.mp4", 10.0);
        p
    }

    #[test]
    fn split_divides_both_tracks_and_preserves_source_mapping() {
        let mut p = one_clip_project();
        assert_eq!(p.split_at(4.0), 2);
        let v: Vec<_> = p.tracks[0].clips.iter().collect();
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].start, v[0].duration, v[0].in_point), (0.0, 4.0, 0.0));
        assert_eq!((v[1].start, v[1].duration, v[1].in_point), (4.0, 6.0, 4.0));
        // Splitting outside any clip does nothing.
        assert_eq!(p.split_at(20.0), 0);
    }

    #[test]
    fn move_range_respects_neighbours() {
        let mut p = one_clip_project();
        p.split_at(4.0);
        let right_id = p.tracks[0].clips[1].id;
        // Move the right piece later, leaving a gap.
        p.clip_mut(right_id).unwrap().start = 7.0;
        let left_id = p.tracks[0].clips[0].id;
        let (lo, hi) = p.move_range(left_id);
        assert_eq!(lo, 0.0);
        assert!((hi - 3.0).abs() < 1e-9, "left clip (4s) may start at most at 3.0, got {hi}");
    }

    #[test]
    fn snapping_picks_nearest_within_tolerance() {
        let targets = [0.0, 4.0, 10.0];
        assert_eq!(EditorState::snap(3.9, &targets, 0.2), (4.0, Some(4.0)));
        assert_eq!(EditorState::snap(5.0, &targets, 0.2), (5.0, None));
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut p = one_clip_project();
        let mut ed = EditorState::default();
        ed.push_undo(&p);
        p.split_at(5.0);
        assert_eq!(p.tracks[0].clips.len(), 2);
        assert!(ed.undo(&mut p));
        assert_eq!(p.tracks[0].clips.len(), 1);
        assert!(ed.redo(&mut p));
        assert_eq!(p.tracks[0].clips.len(), 2);
    }

    #[test]
    fn atomic_write_replaces_cleanly_and_leaves_no_temp() {
        let path = std::env::temp_dir().join(format!("reel-atomic-{}.reel", std::process::id()));
        let p = path.to_string_lossy().into_owned();
        write_atomic(&p, "first").expect("write");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "first");
        // Overwriting an existing document must succeed and replace it whole.
        write_atomic(&p, "second, longer contents").expect("overwrite");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "second, longer contents");
        assert!(!std::path::Path::new(&format!("{p}.tmp")).exists(), "temp file left behind");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ripple_delete_closes_the_hole_on_every_track() {
        let mut p = one_clip_project(); // 10s on V1 and A1
        p.split_at(4.0);
        p.split_at(7.0); // → [0,4) [4,7) [7,10) on both tracks
        let mid = p.tracks[0].clips[1].id;
        let before = p.duration();
        assert_eq!(p.tracks[0].clips.len(), 3);

        let removed = p.ripple_delete(mid);
        assert!((removed - 3.0).abs() < 1e-9, "removed the middle clip's 3s");
        assert_eq!(p.tracks[0].clips.len(), 2);
        // The third clip slid left into the hole: no gap, and the edit is 3s shorter.
        assert!((p.tracks[0].clips[1].start - 4.0).abs() < 1e-9);
        assert!((p.duration() - (before - 3.0)).abs() < 1e-9);
        // The linked audio clip went with it, and what remains stays in sync
        // with the picture — no overlaps left behind.
        assert_eq!(p.tracks[1].clips.len(), 2, "the sound of that shot went too");
        assert!((p.tracks[1].clips[1].start - 4.0).abs() < 1e-9);
        for track in &p.tracks {
            for pair in track.clips.windows(2) {
                assert!(pair[0].end() <= pair[1].start + 1e-6, "clips must never overlap");
            }
        }
    }

    #[test]
    fn ripple_trim_pulls_the_edit_to_the_playhead() {
        // Trim the head: Q at 2s inside a clip starting at 0.
        let mut p = one_clip_project();
        p.split_at(5.0);
        let removed = p.ripple_trim_to_playhead(2.0, true);
        assert!((removed - 2.0).abs() < 1e-9);
        let first = &p.tracks[0].clips[0];
        assert!((first.start - 0.0).abs() < 1e-9, "clip slid back to the start");
        assert!((first.in_point - 2.0).abs() < 1e-9, "and kept its source offset");
        assert!((first.duration - 3.0).abs() < 1e-9);
        assert!((p.duration() - 8.0).abs() < 1e-9, "edit is 2s shorter");

        // Trim the tail: W at 1s inside a 3s clip removes its last 2s.
        let mut p = one_clip_project();
        p.split_at(3.0);
        let removed = p.ripple_trim_to_playhead(1.0, false);
        assert!((removed - 2.0).abs() < 1e-9);
        assert!((p.tracks[0].clips[0].duration - 1.0).abs() < 1e-9);
        assert!((p.tracks[0].clips[1].start - 1.0).abs() < 1e-9, "the next clip closed up");

        // Refuses to leave a sliver, or to trim outside a clip.
        let mut p = one_clip_project();
        assert_eq!(p.ripple_trim_to_playhead(0.01, true), 0.0);
        assert_eq!(p.ripple_trim_to_playhead(99.0, true), 0.0);
    }

    #[test]
    fn gaps_close_before_a_clip_and_across_the_timeline() {
        let mut p = one_clip_project(); // 10s clip at 0 on V1 and A1
        p.split_at(4.0);
        let right = p.tracks[0].clips[1].id;
        p.clip_mut(right).unwrap().start = 7.0; // leave a 3s hole

        // Closing the gap before that clip moves only it.
        assert!((p.close_gap_before(right) - 3.0).abs() < 1e-9);
        assert!((p.clip(right).unwrap().start - 4.0).abs() < 1e-9);
        assert_eq!(p.close_gap_before(right), 0.0, "already butted up");

        // A hole on each track, closed in one sweep.
        p.clip_mut(right).unwrap().start = 9.0;
        let a_id = p.tracks[1].clips[0].id;
        p.clip_mut(a_id).unwrap().start = 2.0;
        let removed = p.close_all_gaps();
        assert!(removed > 6.0, "should have removed both holes, got {removed}");
        assert_eq!(p.clip(a_id).unwrap().start, 0.0);
        assert!((p.clip(right).unwrap().start - 4.0).abs() < 1e-9);
    }

    #[test]
    fn export_range_cuts_clips_at_the_markers() {
        let mut p = one_clip_project(); // 10s clip at 0
        p.split_at(4.0); // → [0,4) and [4,10)
        // Range 2–6 must yield two segments totalling 4s, with source
        // in-points shifted to match where the markers landed.
        let segs = p.export_segments_range(Some(2.0), Some(6.0));
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[0].in_point, segs[0].duration), (2.0, 2.0));
        assert_eq!((segs[1].in_point, segs[1].duration), (4.0, 2.0));
        let total: f64 = segs.iter().map(|s| s.duration).sum();
        assert!((total - 4.0).abs() < 1e-9);
        // A range beyond every clip exports nothing.
        assert!(p.export_segments_range(Some(50.0), None).is_empty());
        // No markers = the whole edit.
        assert_eq!(p.export_segments().len(), 2);
    }

    #[test]
    fn project_saves_and_loads() {
        let mut p = one_clip_project();
        p.split_at(3.0);
        let path = std::env::temp_dir().join(format!("reel-proj-test-{}.reel", std::process::id()));
        p.save(&path.to_string_lossy()).expect("save");
        let loaded = Project::load(&path.to_string_lossy()).expect("load");
        assert_eq!(loaded.tracks[0].clips.len(), 2);
        // next_id re-seeded above the stored max — appends must not collide.
        let max_id = loaded.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.id).max().unwrap();
        let mut loaded2 = loaded;
        loaded2.append_video("b", "/tmp/b.mp4", 1.0);
        let new_max = loaded2.tracks[0].clips.iter().map(|c| c.id).max().unwrap();
        assert!(new_max > max_id);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn source_timeline_mapping_follows_trims() {
        let mut p = one_clip_project();
        p.split_at(4.0);
        // Delete the left piece; right piece starts at 4.0 with in_point 4.0.
        let left_id = p.tracks[0].clips[0].id;
        p.delete_clip(left_id);
        assert_eq!(p.source_to_timeline("/tmp/a.mp4", 5.0), Some(5.0));
        assert_eq!(p.source_to_timeline("/tmp/a.mp4", 2.0), None); // trimmed away
    }

    /// Pasting inserts: it makes room instead of overwriting whatever was
    /// already there. Losing footage to a stray Ctrl+V is the kind of thing
    /// people never forgive an editor for.
    #[test]
    fn pasting_makes_room_instead_of_overwriting() {
        let mut p = one_clip_project();
        p.split_at(4.0); // 0..4 and 4..10
        let first = p.tracks[0].clips[0].clone();

        let id = p.paste_clip(&first, 4.0, TrackKind::Video);
        let clips = &p.tracks[0].clips;
        assert_eq!(clips.len(), 3, "paste should add a clip, not replace one");

        // Nothing overlaps, and the total length grew by exactly the paste.
        for w in clips.windows(2) {
            assert!(
                w[1].start >= w[0].end() - 1e-6,
                "paste left clips overlapping: {:?} then {:?}",
                (w[0].start, w[0].end()),
                (w[1].start, w[1].end())
            );
        }
        let end = clips.iter().map(|c| c.end()).fold(0.0, f64::max);
        assert!((end - 14.0).abs() < 1e-6, "expected 10s + a 4s paste, got {end}");

        // The pasted clip is a real copy with its own id, at the playhead.
        let pasted = clips.iter().find(|c| c.id == id).unwrap();
        assert_ne!(pasted.id, first.id);
        assert!((pasted.start - 4.0).abs() < 1e-6);
        assert_eq!(pasted.source, first.source);
        assert!((pasted.in_point - first.in_point).abs() < 1e-6);
    }

    /// Pasting into the MIDDLE of a clip has to split it, not shove it aside
    /// and leave a hole where it used to be.
    #[test]
    fn pasting_mid_clip_splits_it_rather_than_leaving_a_hole() {
        let mut p = one_clip_project();
        let src = p.tracks[0].clips[0].clone(); // 0..10
        p.paste_clip(&src, 3.0, TrackKind::Video);

        let mut clips = p.tracks[0].clips.clone();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        // 0..3, the 10s paste at 3..13, then the old tail at 13..20.
        assert_eq!(clips.len(), 3);
        for w in clips.windows(2) {
            let gap = w[1].start - w[0].end();
            assert!(gap.abs() < 1e-6, "paste left a {gap:.3}s hole in the timeline");
        }
        let end = clips.last().unwrap().end();
        assert!((end - 20.0).abs() < 1e-6, "expected 10s + a 10s paste, got {end}");
    }

    #[test]
    fn duplicate_lands_immediately_after_the_original() {
        let mut p = one_clip_project();
        let id = p.tracks[0].clips[0].id;
        let end = p.tracks[0].clips[0].end();
        let copy = p.duplicate_clip(id).expect("duplicate");
        let c = p.tracks[0].clips.iter().find(|c| c.id == copy).unwrap();
        assert!((c.start - end).abs() < 1e-6, "copy should butt against the original");
        assert_eq!(p.tracks[0].clips.len(), 2);
    }

    /// Captions are written against the original recording, so mapping them
    /// into the edit has to survive the three things editors actually do:
    /// cut a line in half, delete a chunk, and reuse a moment twice.
    #[test]
    fn caption_windows_survive_cuts_gaps_and_reuse() {
        let mut p = one_clip_project();
        // Source is 0..10 at timeline 0..10. Cut at 4 and pull the right
        // half later, leaving a gap: 0..4 stays, 4..10 moves to 6..12.
        p.split_at(4.0);
        p.tracks[0].clips[1].start = 6.0;

        // A cue spanning the cut (3.5..4.5 in source) must land in BOTH
        // pieces, clipped at the join — never running over the gap.
        let spans = p.map_source_window("/tmp/a.mp4", 3.5, 4.5);
        assert_eq!(spans.len(), 2, "a cue across a cut belongs to both pieces");
        assert!((spans[0].0 - 3.5).abs() < 1e-6 && (spans[0].1 - 4.0).abs() < 1e-6);
        assert!((spans[1].0 - 6.0).abs() < 1e-6 && (spans[1].1 - 6.5).abs() < 1e-6);

        // Reuse: duplicate the tail, and the same words caption both copies.
        let clip = p.tracks[0].clips[1].clone();
        let mut copy = clip.clone();
        copy.id = p.next_id;
        p.next_id += 1;
        copy.start = 20.0;
        p.tracks[0].clips.push(copy);
        assert_eq!(
            p.map_source_window("/tmp/a.mp4", 5.0, 5.5).len(),
            2,
            "a moment used twice captions twice"
        );

        // A window that was trimmed away captions nowhere.
        let mut q = one_clip_project();
        q.split_at(4.0);
        let left = q.tracks[0].clips[0].id;
        q.delete_clip(left);
        assert!(q.map_source_window("/tmp/a.mp4", 1.0, 2.0).is_empty());
    }
}

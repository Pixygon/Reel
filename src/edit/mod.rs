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

    /// The edit flattened for export: V1 clips in timeline order as
    /// (source, in_point, duration). Gaps are collapsed — exactly how editor
    /// playback sequences the cut.
    pub fn export_segments(&self) -> Vec<(String, f64, f64)> {
        self.export_segments_range(None, None)
    }

    /// Like `export_segments`, restricted to the timeline window
    /// [`range_in`, `range_out`] — clips are cut at the boundaries so an
    /// in/out range exports exactly what the markers enclose.
    pub fn export_segments_range(
        &self,
        range_in: Option<f64>,
        range_out: Option<f64>,
    ) -> Vec<(String, f64, f64)> {
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
                Some((c.source.clone(), c.in_point + head, end - start))
            })
            .collect()
    }

    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
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
        self.dirty = true;
    }

    pub fn undo(&mut self, project: &mut Project) -> bool {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(project, prev));
            self.selected = None;
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self, project: &mut Project) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(project, next));
            self.selected = None;
            self.dirty = true;
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
    fn export_range_cuts_clips_at_the_markers() {
        let mut p = one_clip_project(); // 10s clip at 0
        p.split_at(4.0); // → [0,4) and [4,10)
        // Range 2–6 must yield two segments totalling 4s, with source
        // in-points shifted to match where the markers landed.
        let segs = p.export_segments_range(Some(2.0), Some(6.0));
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[0].1, segs[0].2), (2.0, 2.0)); // in_point 2, 2s long
        assert_eq!((segs[1].1, segs[1].2), (4.0, 2.0)); // in_point 4, 2s long
        let total: f64 = segs.iter().map(|s| s.2).sum();
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
}

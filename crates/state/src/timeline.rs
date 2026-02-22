use crate::clip::ClipId;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(Uuid);

impl TrackId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TrackId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimelineClipId(Uuid);

impl TimelineClipId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TimelineClipId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone)]
pub struct TimelineClip {
    pub id: TimelineClipId,
    pub source_id: ClipId,
    pub track_id: TrackId,
    pub timeline_start: f64,
    pub duration: f64,
    pub source_in: f64,
    pub source_out: f64,
    pub linked_to: Option<TimelineClipId>,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<TimelineClip>,
}

impl Track {
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            id: TrackId::new(),
            name: name.into(),
            kind,
            clips: Vec::new(),
        }
    }

    pub fn resolve_overlaps(&mut self, new_start: f64, new_end: f64) {
        let mut i = 0;
        let mut splits: Vec<TimelineClip> = Vec::new();

        while i < self.clips.len() {
            let clip = &self.clips[i];
            let clip_start = clip.timeline_start;
            let clip_end = clip.timeline_start + clip.duration;

            if clip_end <= new_start || clip_start >= new_end {
                i += 1;
                continue;
            }

            if clip_start >= new_start && clip_end <= new_end {
                self.clips.remove(i);
                continue;
            }

            if clip_start < new_start && clip_end > new_end {
                let left_duration = new_start - clip_start;
                let right_duration = clip_end - new_end;
                let right_in = clip.source_in + (new_end - clip_start);

                let right = TimelineClip {
                    id: TimelineClipId::new(),
                    source_id: clip.source_id,
                    track_id: clip.track_id,
                    timeline_start: new_end,
                    duration: right_duration,
                    source_in: right_in,
                    source_out: clip.source_out,
                    linked_to: None,
                };
                splits.push(right);

                let clip = &mut self.clips[i];
                clip.duration = left_duration;
                clip.source_out = clip.source_in + left_duration;
                i += 1;
                continue;
            }

            if clip_start < new_start {
                let clip = &mut self.clips[i];
                let trimmed = clip_end - new_start;
                clip.duration -= trimmed;
                clip.source_out -= trimmed;
                i += 1;
                continue;
            }

            let trim_amount = new_end - clip_start;
            let clip = &mut self.clips[i];
            clip.source_in += trim_amount;
            clip.timeline_start = new_end;
            clip.duration -= trim_amount;
            i += 1;
        }

        self.clips.extend(splits);
    }
}

pub struct PlayheadHit {
    pub track_id: TrackId,
    pub clip: TimelineClip,
    pub source_time: f64,
}

#[derive(Debug, Clone)]
pub struct Timeline {
    pub video_tracks: Vec<Track>,
    pub audio_tracks: Vec<Track>,
}

impl Timeline {
    pub fn new() -> Self {
        Self {
            video_tracks: vec![
                Track::new("V1", TrackKind::Video),
                Track::new("V2", TrackKind::Video),
                Track::new("V3", TrackKind::Video),
            ],
            audio_tracks: vec![
                Track::new("A1", TrackKind::Audio),
                Track::new("A2", TrackKind::Audio),
                Track::new("A3", TrackKind::Audio),
            ],
        }
    }

    pub fn all_tracks(&self) -> impl Iterator<Item = &Track> {
        self.video_tracks.iter().chain(self.audio_tracks.iter())
    }

    pub fn all_tracks_mut(&mut self) -> impl Iterator<Item = &mut Track> {
        self.video_tracks
            .iter_mut()
            .chain(self.audio_tracks.iter_mut())
    }

    pub fn track_by_id(&self, id: TrackId) -> Option<&Track> {
        self.all_tracks().find(|t| t.id == id)
    }

    pub fn track_by_id_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.all_tracks_mut().find(|t| t.id == id)
    }

    pub fn track_index_and_kind(&self, id: TrackId) -> Option<(TrackKind, usize)> {
        for (i, t) in self.video_tracks.iter().enumerate() {
            if t.id == id {
                return Some((TrackKind::Video, i));
            }
        }
        for (i, t) in self.audio_tracks.iter().enumerate() {
            if t.id == id {
                return Some((TrackKind::Audio, i));
            }
        }
        None
    }

    pub fn track_kind_for_clip(&self, id: TimelineClipId) -> Option<TrackKind> {
        self.find_clip(id).map(|(track, _, _)| track.kind)
    }

    pub fn find_clip(&self, id: TimelineClipId) -> Option<(&Track, usize, &TimelineClip)> {
        for track in self.all_tracks() {
            for (i, clip) in track.clips.iter().enumerate() {
                if clip.id == id {
                    return Some((track, i, clip));
                }
            }
        }
        None
    }

    pub fn find_clip_track_mut(&mut self, id: TimelineClipId) -> Option<(&mut Track, usize)> {
        for track in self
            .video_tracks
            .iter_mut()
            .chain(self.audio_tracks.iter_mut())
        {
            let idx = track.clips.iter().position(|c| c.id == id);
            if let Some(i) = idx {
                return Some((track, i));
            }
        }
        None
    }

    pub fn clip_at_time(&self, time: f64) -> Option<PlayheadHit> {
        for track in self.all_tracks() {
            for tc in &track.clips {
                if time >= tc.timeline_start && time < tc.timeline_start + tc.duration {
                    let source_time = tc.source_in + (time - tc.timeline_start);
                    return Some(PlayheadHit {
                        track_id: track.id,
                        clip: tc.clone(),
                        source_time,
                    });
                }
            }
        }
        None
    }

    pub fn audio_clip_at_time(&self, time: f64) -> Option<PlayheadHit> {
        for track in &self.audio_tracks {
            for tc in &track.clips {
                if time >= tc.timeline_start && time < tc.timeline_start + tc.duration {
                    let source_time = tc.source_in + (time - tc.timeline_start);
                    return Some(PlayheadHit {
                        track_id: track.id,
                        clip: tc.clone(),
                        source_time,
                    });
                }
            }
        }
        None
    }

    pub fn timeline_duration(&self) -> f64 {
        let mut max_end: f64 = 0.0;
        for track in self.all_tracks() {
            for tc in &track.clips {
                let end = tc.timeline_start + tc.duration;
                if end > max_end {
                    max_end = end;
                }
            }
        }
        max_end
    }

    pub fn track_count(&self) -> usize {
        self.video_tracks.len() + self.audio_tracks.len()
    }

    pub fn pair_count(&self) -> usize {
        self.video_tracks.len()
    }

    pub fn add_track_pair(&mut self) {
        let n = self.video_tracks.len() + 1;
        self.video_tracks
            .push(Track::new(format!("V{n}"), TrackKind::Video));
        self.audio_tracks
            .push(Track::new(format!("A{n}"), TrackKind::Audio));
    }

    pub fn remove_track_pair(&mut self, idx: usize) {
        if self.pair_count() <= 1 {
            return;
        }
        if idx < self.video_tracks.len() {
            self.video_tracks.remove(idx);
        }
        if idx < self.audio_tracks.len() {
            self.audio_tracks.remove(idx);
        }
    }

    fn move_clip_on_track_core(
        &mut self,
        track_id: TrackId,
        clip_id: TimelineClipId,
        new_position: f64,
    ) {
        let Some(track) = self.track_by_id_mut(track_id) else {
            return;
        };
        let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) else {
            return;
        };
        let clip = track.clips.remove(idx);
        let new_pos = new_position.max(0.0);
        track.resolve_overlaps(new_pos, new_pos + clip.duration);
        let mut moved = clip;
        moved.timeline_start = new_pos;
        track.clips.push(moved);
    }

    pub fn move_clip_on_track(
        &mut self,
        track_id: TrackId,
        clip_id: TimelineClipId,
        new_position: f64,
    ) {
        self.move_clip_on_track_core(track_id, clip_id, new_position);
        let linked = self.find_clip(clip_id).and_then(|(_, _, c)| c.linked_to);
        if let Some(linked_id) = linked {
            let linked_track = self.find_clip(linked_id).map(|(t, _, _)| t.id);
            if let Some(lt_id) = linked_track {
                self.move_clip_on_track_core(lt_id, linked_id, new_position);
            }
        }
    }

    fn move_clip_across_tracks_core(
        &mut self,
        clip_id: TimelineClipId,
        dst_track_id: TrackId,
        new_position: f64,
    ) {
        let mut clip = None;
        for track in self
            .video_tracks
            .iter_mut()
            .chain(self.audio_tracks.iter_mut())
        {
            if let Some(idx) = track.clips.iter().position(|c| c.id == clip_id) {
                clip = Some(track.clips.remove(idx));
                break;
            }
        }
        let Some(mut clip) = clip else {
            return;
        };
        let new_pos = new_position.max(0.0);
        let Some(dst) = self.track_by_id_mut(dst_track_id) else {
            return;
        };
        dst.resolve_overlaps(new_pos, new_pos + clip.duration);
        clip.timeline_start = new_pos;
        clip.track_id = dst_track_id;
        dst.clips.push(clip);
    }

    pub fn move_clip_across_tracks(
        &mut self,
        clip_id: TimelineClipId,
        dst_track_id: TrackId,
        new_position: f64,
    ) {
        self.move_clip_across_tracks_core(clip_id, dst_track_id, new_position);
        let linked = self.find_clip(clip_id).and_then(|(_, _, c)| c.linked_to);
        if let Some(linked_id) = linked {
            let linked_track = self.find_clip(linked_id).map(|(t, _, _)| t.id);
            if let Some(lt_id) = linked_track {
                self.move_clip_on_track_core(lt_id, linked_id, new_position);
            }
        }
    }

    pub fn paired_track_id(&self, track_id: TrackId) -> Option<TrackId> {
        for (i, t) in self.video_tracks.iter().enumerate() {
            if t.id == track_id {
                return self.audio_tracks.get(i).map(|t| t.id);
            }
        }
        for (i, t) in self.audio_tracks.iter().enumerate() {
            if t.id == track_id {
                return self.video_tracks.get(i).map(|t| t.id);
            }
        }
        None
    }

    pub fn add_clip_to_track(
        &mut self,
        source_id: ClipId,
        track_id: TrackId,
        position_seconds: f64,
        duration: f64,
    ) -> TimelineClipId {
        let id = TimelineClipId::new();
        let Some(track) = self.track_by_id_mut(track_id) else {
            return id;
        };

        let pos = position_seconds.max(0.0);
        track.resolve_overlaps(pos, pos + duration);

        track.clips.push(TimelineClip {
            id,
            source_id,
            track_id,
            timeline_start: pos,
            duration,
            source_in: 0.0,
            source_out: duration,
            linked_to: None,
        });
        id
    }

    pub fn link_clips(&mut self, a: TimelineClipId, b: TimelineClipId) {
        if let Some((track, idx)) = self.find_clip_track_mut(a) {
            track.clips[idx].linked_to = Some(b);
        }
        if let Some((track, idx)) = self.find_clip_track_mut(b) {
            track.clips[idx].linked_to = Some(a);
        }
    }

    pub fn sync_linked_clip(&mut self, clip_id: TimelineClipId) {
        let Some((_, _, clip)) = self.find_clip(clip_id) else {
            return;
        };
        let Some(linked_id) = clip.linked_to else {
            return;
        };
        let start = clip.timeline_start;
        let dur = clip.duration;
        let src_in = clip.source_in;
        let src_out = clip.source_out;

        if let Some((track, idx)) = self.find_clip_track_mut(linked_id) {
            let lc = &mut track.clips[idx];
            lc.timeline_start = start;
            lc.duration = dur;
            lc.source_in = src_in;
            lc.source_out = src_out;
        }
    }

    pub fn sync_linked_clip_after_trim(&mut self, clip_id: TimelineClipId) {
        let Some((_, _, clip)) = self.find_clip(clip_id) else {
            return;
        };
        let Some(linked_id) = clip.linked_to else {
            return;
        };
        let start = clip.timeline_start;
        let dur = clip.duration;
        let src_in = clip.source_in;
        let src_out = clip.source_out;

        let mut removed_clip = None;
        let mut linked_track_id = None;
        for track in self
            .video_tracks
            .iter_mut()
            .chain(self.audio_tracks.iter_mut())
        {
            if let Some(idx) = track.clips.iter().position(|c| c.id == linked_id) {
                removed_clip = Some(track.clips.remove(idx));
                linked_track_id = Some(track.id);
                break;
            }
        }

        let Some(mut lc) = removed_clip else {
            return;
        };
        let Some(lt_id) = linked_track_id else {
            return;
        };

        lc.timeline_start = start;
        lc.duration = dur;
        lc.source_in = src_in;
        lc.source_out = src_out;

        let Some(track) = self.track_by_id_mut(lt_id) else {
            return;
        };
        track.resolve_overlaps(start, start + dur);
        track.clips.push(lc);
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

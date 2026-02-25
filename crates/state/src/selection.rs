use std::collections::HashSet;

use crate::clip::ClipId;
use crate::timeline::TimelineClipId;

#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub selected_clips: HashSet<ClipId>,
    pub last_selected_clip: Option<ClipId>,
    pub hovered_clip: Option<ClipId>,
    pub selected_scrub_t: Option<f32>,
    pub selected_timeline_clips: HashSet<TimelineClipId>,
}

impl Selection {
    pub fn is_clip_selected(&self, id: ClipId) -> bool {
        self.selected_clips.contains(&id)
    }

    pub fn is_timeline_clip_selected(&self, id: TimelineClipId) -> bool {
        self.selected_timeline_clips.contains(&id)
    }

    pub fn select_single_timeline_clip(&mut self, id: TimelineClipId) {
        self.selected_timeline_clips.clear();
        self.selected_timeline_clips.insert(id);
    }

    pub fn toggle_timeline_clip(&mut self, id: TimelineClipId) {
        if self.selected_timeline_clips.contains(&id) {
            self.selected_timeline_clips.remove(&id);
        } else {
            self.selected_timeline_clips.insert(id);
        }
    }

    pub fn toggle_timeline_clip_to_match(&mut self, id: TimelineClipId, match_id: TimelineClipId) {
        if self.selected_timeline_clips.contains(&match_id) {
            self.selected_timeline_clips.insert(id);
        } else {
            self.selected_timeline_clips.remove(&id);
        }
    }

    pub fn clear_timeline_clips(&mut self) {
        self.selected_timeline_clips.clear();
    }

    pub fn primary_timeline_clip(&self) -> Option<TimelineClipId> {
        self.selected_timeline_clips.iter().next().copied()
    }

    pub fn select_single(&mut self, id: ClipId) {
        self.selected_clips.clear();
        self.selected_clips.insert(id);
        self.last_selected_clip = Some(id);
    }

    pub fn toggle_clip(&mut self, id: ClipId) {
        if self.selected_clips.contains(&id) {
            self.selected_clips.remove(&id);
            if self.last_selected_clip == Some(id) {
                self.last_selected_clip = self.selected_clips.iter().next().copied();
            }
        } else {
            self.selected_clips.insert(id);
            self.last_selected_clip = Some(id);
        }
    }

    pub fn select_range(&mut self, anchor: Option<ClipId>, target: ClipId, ordered: &[ClipId]) {
        let anchor = match anchor {
            Some(a) => a,
            None => {
                self.select_single(target);
                return;
            }
        };

        let anchor_pos = ordered.iter().position(|id| *id == anchor);
        let target_pos = ordered.iter().position(|id| *id == target);

        match (anchor_pos, target_pos) {
            (Some(a), Some(t)) => {
                let (start, end) = if a <= t { (a, t) } else { (t, a) };
                self.selected_clips.clear();
                for id in &ordered[start..=end] {
                    self.selected_clips.insert(*id);
                }
                self.last_selected_clip = Some(target);
            }
            _ => {
                self.select_single(target);
            }
        }
    }

    pub fn clear_clips(&mut self) {
        self.selected_clips.clear();
        self.last_selected_clip = None;
    }

    pub fn primary_clip(&self) -> Option<ClipId> {
        self.last_selected_clip
    }
}

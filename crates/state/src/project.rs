use std::collections::{HashMap, HashSet};

use crate::clip::{Clip, ClipId};
use crate::playback::Playback;
use crate::selection::Selection;
use crate::tag::Tag;
use crate::timeline::{self, TimelineClip, Track};

pub struct ProjectState {
    pub clips: HashMap<ClipId, Clip>,
    pub clip_order: Vec<ClipId>,
    pub starred: HashSet<ClipId>,
    pub clip_tags: HashMap<ClipId, u32>,
    pub tracks: Vec<Track>,
    pub playback: Playback,
}

impl Default for ProjectState {
    fn default() -> Self {
        Self {
            clips: HashMap::new(),
            clip_order: Vec::new(),
            starred: HashSet::new(),
            clip_tags: HashMap::new(),
            tracks: timeline::default_tracks(),
            playback: Playback::default(),
        }
    }
}

impl ProjectState {
    pub fn add_clip(&mut self, clip: Clip) {
        let id = clip.id;
        self.clip_order.push(id);
        self.clips.insert(id, clip);
        self.clip_tags.entry(id).or_insert(0);
    }

    pub fn toggle_star(&mut self, id: ClipId) {
        if !self.starred.remove(&id) {
            self.starred.insert(id);
        }
    }

    pub fn clip_tag_mask(&self, id: ClipId) -> u32 {
        self.clip_tags.get(&id).copied().unwrap_or(0)
    }

    pub fn toggle_tag(&mut self, id: ClipId, tag: Tag) {
        let entry = self.clip_tags.entry(id).or_insert(0);
        *entry ^= tag.bit();
    }

    pub fn toggle_tag_filter(&mut self, ui: &mut UiState, tag: Tag) {
        ui.tag_filter_mask ^= tag.bit();
    }

    pub fn add_clip_to_track(
        &mut self,
        clip_id: ClipId,
        track_index: usize,
        position_seconds: f64,
    ) {
        let Some(track) = self.tracks.get_mut(track_index) else {
            return;
        };

        let duration = self
            .clips
            .get(&clip_id)
            .and_then(|c| c.duration)
            .unwrap_or(3.0)
            .max(0.1);

        track.clips.push(TimelineClip {
            clip_id,
            position: position_seconds.max(0.0),
            duration,
            in_point: 0.0,
            out_point: duration,
        });
    }
}

pub struct UiState {
    pub search_query: String,
    pub starred_only: bool,
    pub tag_filter_mask: u32,
    pub selection: Selection,
    pub hovered_scrub_t: Option<f32>,
    pub hover_active_clip: Option<ClipId>,
    pub hover_started_at: Option<f64>,
    pub visible_clips: Vec<ClipId>,
    pub show_fps: bool,
    pub fps: f32,
    pub timeline_scrubbing: Option<f64>,
    pub show_browser: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            search_query: String::new(),
            starred_only: false,
            tag_filter_mask: 0,
            selection: Selection::default(),
            hovered_scrub_t: None,
            hover_active_clip: None,
            hover_started_at: None,
            visible_clips: Vec::new(),
            show_fps: false,
            fps: 0.0,
            timeline_scrubbing: None,
            show_browser: true,
        }
    }
}

#[derive(Default)]
pub struct AppState {
    pub project: ProjectState,
    pub ui: UiState,
}

impl AppState {
    pub fn filtered_clips(&self) -> Vec<ClipId> {
        let query = self.ui.search_query.to_lowercase();
        let tokens: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
        self.project
            .clip_order
            .iter()
            .filter(|id| {
                if self.ui.starred_only && !self.project.starred.contains(id) {
                    return false;
                }
                if self.ui.tag_filter_mask != 0 {
                    let clip_mask = self.project.clip_tags.get(*id).copied().unwrap_or(0);
                    if (clip_mask & self.ui.tag_filter_mask) == 0 {
                        return false;
                    }
                }
                if tokens.is_empty() {
                    return true;
                }
                let Some(clip) = self.project.clips.get(id) else {
                    return false;
                };

                tokens.iter().all(|t| clip.search_haystack.contains(t))
            })
            .copied()
            .collect()
    }
}

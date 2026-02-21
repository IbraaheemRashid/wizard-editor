use std::collections::{HashMap, HashSet};

use crate::clip::{Clip, ClipId};
use crate::playback::Playback;
use crate::selection::Selection;
use crate::tag::Tag;
use crate::timeline::{self, TimelineClip, Track};

pub struct AppState {
    pub clips: HashMap<ClipId, Clip>,
    pub clip_order: Vec<ClipId>,
    pub starred: HashSet<ClipId>,
    pub clip_tags: HashMap<ClipId, u32>,
    pub search_query: String,
    pub starred_only: bool,
    pub tag_filter_mask: u32,
    pub tracks: Vec<Track>,
    pub playback: Playback,
    pub selection: Selection,
    pub hovered_scrub_t: Option<f32>,
    pub hover_active_clip: Option<ClipId>,
    pub hover_started_at: Option<f64>,
    pub show_fps: bool,
    pub fps: f32,
    pub pending_thumbnails: HashSet<ClipId>,
    pub thumbnails: HashMap<ClipId, egui::TextureHandle>,
    pub preview_frames: HashMap<ClipId, Vec<egui::TextureHandle>>,
    pub preview_requested: HashSet<ClipId>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            clips: HashMap::new(),
            clip_order: Vec::new(),
            starred: HashSet::new(),
            clip_tags: HashMap::new(),
            search_query: String::new(),
            starred_only: false,
            tag_filter_mask: 0,
            tracks: timeline::default_tracks(),
            playback: Playback::default(),
            selection: Selection::default(),
            hovered_scrub_t: None,
            hover_active_clip: None,
            hover_started_at: None,
            show_fps: false,
            fps: 0.0,
            pending_thumbnails: HashSet::new(),
            thumbnails: HashMap::new(),
            preview_frames: HashMap::new(),
            preview_requested: HashSet::new(),
        }
    }
}

impl AppState {
    pub fn add_clip(&mut self, clip: Clip) {
        let id = clip.id;
        self.clip_order.push(id);
        self.clips.insert(id, clip);
        self.clip_tags.entry(id).or_insert(0);
        self.pending_thumbnails.insert(id);
    }

    pub fn filtered_clips(&self) -> Vec<ClipId> {
        let query = self.search_query.to_lowercase();
        let tokens: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
        self.clip_order
            .iter()
            .filter(|id| {
                if self.starred_only && !self.starred.contains(id) {
                    return false;
                }
                if self.tag_filter_mask != 0 {
                    let clip_mask = self.clip_tags.get(*id).copied().unwrap_or(0);
                    if (clip_mask & self.tag_filter_mask) == 0 {
                        return false;
                    }
                }
                if tokens.is_empty() {
                    return true;
                }
                let Some(clip) = self.clips.get(id) else {
                    return false;
                };

                let mut haystack = String::new();
                haystack.push_str(&clip.filename.to_lowercase());

                if let Some(codec) = &clip.codec {
                    haystack.push(' ');
                    haystack.push_str(&codec.to_lowercase());
                }

                if let Some((w, h)) = clip.resolution {
                    haystack.push(' ');
                    haystack.push_str(&format!("{w}x{h}"));
                }

                if let Some(dur) = clip.duration {
                    let dur_i = dur.round().max(0.0) as i64;
                    let m = dur_i / 60;
                    let s = dur_i % 60;
                    haystack.push(' ');
                    haystack.push_str(&format!("{m}:{s:02}"));
                    haystack.push(' ');
                    haystack.push_str(&dur_i.to_string());
                }

                let clip_mask = self.clip_tag_mask(**id);
                for tag in Tag::ALL {
                    if (clip_mask & tag.bit()) != 0 {
                        haystack.push(' ');
                        haystack.push_str(&tag.label().to_lowercase());
                    }
                }

                tokens.iter().all(|t| haystack.contains(t))
            })
            .copied()
            .collect()
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

    pub fn toggle_tag_filter(&mut self, tag: Tag) {
        self.tag_filter_mask ^= tag.bit();
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

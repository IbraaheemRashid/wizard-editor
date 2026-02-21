use std::collections::{HashMap, HashSet};

use crate::clip::{Clip, ClipId};
use crate::playback::Playback;
use crate::selection::Selection;
use crate::tag::Tag;
use crate::timeline::{self, TimelineClip, Track, TrackId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    ImportOrder,
    Name,
    Duration,
    Resolution,
    FileType,
}

impl SortMode {
    pub const ALL: &'static [SortMode] = &[
        SortMode::ImportOrder,
        SortMode::Name,
        SortMode::Duration,
        SortMode::Resolution,
        SortMode::FileType,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortMode::ImportOrder => "Import Order",
            SortMode::Name => "Name",
            SortMode::Duration => "Duration",
            SortMode::Resolution => "Resolution",
            SortMode::FileType => "File Type",
        }
    }
}

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

    pub fn move_clip_on_track(&mut self, track_index: usize, clip_idx: usize, new_position: f64) {
        let Some(track) = self.tracks.get_mut(track_index) else {
            return;
        };
        if clip_idx < track.clips.len() {
            track.clips[clip_idx].position = new_position.max(0.0);
        }
    }

    pub fn move_clip_across_tracks(
        &mut self,
        src_track: usize,
        clip_idx: usize,
        dst_track: usize,
        new_position: f64,
    ) {
        if src_track >= self.tracks.len() || dst_track >= self.tracks.len() {
            return;
        }
        if clip_idx >= self.tracks[src_track].clips.len() {
            return;
        }
        let mut clip = self.tracks[src_track].clips.remove(clip_idx);
        clip.position = new_position.max(0.0);
        self.tracks[dst_track].clips.push(clip);
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
    pub sort_mode: SortMode,
    pub sort_ascending: bool,
    pub renaming_clip: Option<ClipId>,
    pub rename_buffer: String,
    pub timeline_zoom: f32,
    pub timeline_scroll_offset: f32,
    pub timeline_dragging_clip: Option<(TrackId, usize)>,
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
            sort_mode: SortMode::ImportOrder,
            sort_ascending: true,
            renaming_clip: None,
            rename_buffer: String::new(),
            timeline_zoom: 100.0,
            timeline_scroll_offset: 0.0,
            timeline_dragging_clip: None,
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
        let mut result: Vec<ClipId> = self
            .project
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
            .collect();

        let clips = &self.project.clips;
        let ascending = self.ui.sort_ascending;
        match self.ui.sort_mode {
            SortMode::ImportOrder => {
                if !ascending {
                    result.reverse();
                }
            }
            SortMode::Name => {
                result.sort_by(|a, b| {
                    let na = clips.get(a).map(|c| c.display_name()).unwrap_or("");
                    let nb = clips.get(b).map(|c| c.display_name()).unwrap_or("");
                    let ord = na.to_lowercase().cmp(&nb.to_lowercase());
                    if ascending {
                        ord
                    } else {
                        ord.reverse()
                    }
                });
            }
            SortMode::Duration => {
                result.sort_by(|a, b| {
                    let da = clips.get(a).and_then(|c| c.duration).unwrap_or(0.0);
                    let db = clips.get(b).and_then(|c| c.duration).unwrap_or(0.0);
                    let ord = da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal);
                    if ascending {
                        ord
                    } else {
                        ord.reverse()
                    }
                });
            }
            SortMode::Resolution => {
                result.sort_by(|a, b| {
                    let ra = clips
                        .get(a)
                        .and_then(|c| c.resolution)
                        .map(|(w, h)| w * h)
                        .unwrap_or(0);
                    let rb = clips
                        .get(b)
                        .and_then(|c| c.resolution)
                        .map(|(w, h)| w * h)
                        .unwrap_or(0);
                    let ord = ra.cmp(&rb);
                    if ascending {
                        ord
                    } else {
                        ord.reverse()
                    }
                });
            }
            SortMode::FileType => {
                result.sort_by(|a, b| {
                    let ea = clips.get(a).map(|c| c.extension()).unwrap_or("");
                    let eb = clips.get(b).map(|c| c.extension()).unwrap_or("");
                    let ord = ea.to_lowercase().cmp(&eb.to_lowercase());
                    if ascending {
                        ord
                    } else {
                        ord.reverse()
                    }
                });
            }
        }

        result
    }
}

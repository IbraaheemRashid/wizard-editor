use std::collections::{HashMap, HashSet};

use crate::clip::{Clip, ClipId};
use crate::playback::Playback;
use crate::selection::Selection;
use crate::tag::Tag;
use crate::timeline::{Timeline, TimelineClipId, TrackId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimEdge {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct TrimState {
    pub clip_id: TimelineClipId,
    pub edge: TrimEdge,
    pub original_position: f64,
    pub original_duration: f64,
    pub original_in_point: f64,
    pub original_out_point: f64,
}

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

#[derive(Default)]
pub struct ProjectState {
    pub clips: HashMap<ClipId, Clip>,
    pub clip_order: Vec<ClipId>,
    pub starred: HashSet<ClipId>,
    pub clip_tags: HashMap<ClipId, u32>,
    pub timeline: Timeline,
    pub playback: Playback,
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

    pub fn add_clip_to_track(
        &mut self,
        source_id: ClipId,
        track_id: TrackId,
        position_seconds: f64,
    ) {
        let clip = self.clips.get(&source_id);
        let duration = clip.and_then(|c| c.duration).unwrap_or(3.0).max(0.1);
        let audio_only = clip.map(|c| c.audio_only).unwrap_or(false);

        let track_kind = self.timeline.track_index_and_kind(track_id);

        if audio_only {
            let audio_track_id = match track_kind {
                Some((crate::timeline::TrackKind::Audio, _)) => track_id,
                Some((crate::timeline::TrackKind::Video, _)) => {
                    self.timeline
                        .paired_track_id(track_id)
                        .unwrap_or(track_id)
                }
                None => track_id,
            };
            self.timeline
                .add_clip_to_track(source_id, audio_track_id, position_seconds, duration);
            return;
        }

        let paired = self.timeline.paired_track_id(track_id);

        let primary_id =
            self.timeline
                .add_clip_to_track(source_id, track_id, position_seconds, duration);

        if let Some(paired_track) = paired {
            let linked_id = self.timeline.add_clip_to_track(
                source_id,
                paired_track,
                position_seconds,
                duration,
            );
            self.timeline.link_clips(primary_id, linked_id);
        }
    }
}

pub struct BrowserUiState {
    pub search_query: String,
    pub starred_only: bool,
    pub tag_filter_mask: u32,
    pub sort_mode: SortMode,
    pub sort_ascending: bool,
    pub renaming_clip: Option<ClipId>,
    pub rename_buffer: String,
    pub visible_clips: Vec<ClipId>,
    pub hover_active_clip: Option<ClipId>,
    pub hover_started_at: Option<f64>,
    pub hovered_scrub_t: Option<f32>,
}

impl Default for BrowserUiState {
    fn default() -> Self {
        Self {
            search_query: String::new(),
            starred_only: false,
            tag_filter_mask: 0,
            sort_mode: SortMode::ImportOrder,
            sort_ascending: true,
            renaming_clip: None,
            rename_buffer: String::new(),
            visible_clips: Vec::new(),
            hover_active_clip: None,
            hover_started_at: None,
            hovered_scrub_t: None,
        }
    }
}

pub struct TimelineUiState {
    pub zoom: f32,
    pub scroll_offset: f32,
    pub vertical_scroll_offset: f32,
    pub scrubbing: Option<f64>,
    pub dragging_clip: Option<TimelineClipId>,
    pub drag_grab_offset: Option<f64>,
    pub trimming_clip: Option<TrimState>,
}

impl Default for TimelineUiState {
    fn default() -> Self {
        Self {
            zoom: 100.0,
            scroll_offset: 0.0,
            vertical_scroll_offset: 0.0,
            scrubbing: None,
            dragging_clip: None,
            drag_grab_offset: None,
            trimming_clip: None,
        }
    }
}

#[derive(Default)]
pub struct DebugUiState {
    pub show_fps: bool,
    pub ui_fps: f32,
    pub video_fps: f32,
}

#[derive(Default)]
pub struct UiState {
    pub browser: BrowserUiState,
    pub timeline: TimelineUiState,
    pub debug: DebugUiState,
    pub selection: Selection,
}

#[derive(Default)]
pub struct AppState {
    pub project: ProjectState,
    pub ui: UiState,
}

impl AppState {
    pub fn filtered_clips(&self) -> Vec<ClipId> {
        let query = self.ui.browser.search_query.to_lowercase();
        let tokens: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
        let mut result: Vec<ClipId> = self
            .project
            .clip_order
            .iter()
            .filter(|id| {
                if self.ui.browser.starred_only && !self.project.starred.contains(id) {
                    return false;
                }
                if self.ui.browser.tag_filter_mask != 0 {
                    let clip_mask = self.project.clip_tags.get(*id).copied().unwrap_or(0);
                    if (clip_mask & self.ui.browser.tag_filter_mask) == 0 {
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
        let ascending = self.ui.browser.sort_ascending;
        match self.ui.browser.sort_mode {
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

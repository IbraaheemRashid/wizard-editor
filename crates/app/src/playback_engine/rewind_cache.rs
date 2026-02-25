use std::collections::VecDeque;

use wizard_state::clip::ClipId;
use wizard_state::timeline::TimelineClipId;

use crate::constants::{REWIND_CACHE_MAX_BYTES, REWIND_CACHE_MAX_FRAMES};

#[allow(dead_code)]
pub struct RewindCacheEntry {
    pub timeline_pos: f64,
    pub source_pts: f64,
    pub clip_id: ClipId,
    pub timeline_clip_id: TimelineClipId,
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
}

impl RewindCacheEntry {
    fn byte_size(&self) -> usize {
        self.rgba_data.len()
    }
}

pub struct RewindCache {
    entries: VecDeque<RewindCacheEntry>,
    total_bytes: usize,
}

impl RewindCache {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(REWIND_CACHE_MAX_FRAMES),
            total_bytes: 0,
        }
    }

    pub fn push(&mut self, entry: RewindCacheEntry) {
        let entry_bytes = entry.byte_size();
        self.entries.push_back(entry);
        self.total_bytes += entry_bytes;

        while self.entries.len() > REWIND_CACHE_MAX_FRAMES
            || self.total_bytes > REWIND_CACHE_MAX_BYTES
        {
            if let Some(evicted) = self.entries.pop_front() {
                self.total_bytes -= evicted.byte_size();
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn best_entry_for_playhead(&self, playhead: f64) -> Option<&RewindCacheEntry> {
        let mut best: Option<&RewindCacheEntry> = None;
        for entry in self.entries.iter().rev() {
            if entry.timeline_pos <= playhead + 0.001 {
                best = Some(entry);
                break;
            }
        }
        best
    }

    pub fn trim_above(&mut self, playhead: f64) {
        while let Some(back) = self.entries.back() {
            if back.timeline_pos > playhead + 0.002 {
                let removed = self.entries.pop_back().unwrap();
                self.total_bytes -= removed.byte_size();
            } else {
                break;
            }
        }
    }

    pub fn last_timeline_clip_id(&self) -> Option<TimelineClipId> {
        self.entries.back().map(|e| e.timeline_clip_id)
    }
}

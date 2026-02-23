use std::collections::{HashMap, HashSet};

use wizard_state::clip::ClipId;

pub struct ScrubCacheEntry {
    pub frames: Vec<egui::TextureHandle>,
    pub pts: Vec<f64>,
}

impl ScrubCacheEntry {
    pub fn frame_at_time(&self, source_time: f64) -> Option<&egui::TextureHandle> {
        if self.pts.is_empty() {
            return None;
        }
        let idx = match self.pts.binary_search_by(|p| {
            p.partial_cmp(&source_time)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) if i >= self.pts.len() => self.pts.len() - 1,
            Err(i) => {
                if (self.pts[i] - source_time).abs() < (self.pts[i - 1] - source_time).abs() {
                    i
                } else {
                    i - 1
                }
            }
        };
        self.frames.get(idx)
    }
}

#[derive(Default)]
pub struct TextureCache {
    pub thumbnails: HashMap<ClipId, egui::TextureHandle>,
    pub preview_frames: HashMap<ClipId, Vec<egui::TextureHandle>>,
    pub pending_thumbnails: HashSet<ClipId>,
    pub preview_requested: HashSet<ClipId>,
    pub waveform_peaks: HashMap<ClipId, Vec<(f32, f32)>>,
    pub playback_texture: Option<egui::TextureHandle>,
    pub scrub_frames: HashMap<ClipId, ScrubCacheEntry>,
    pub scrub_requested: HashSet<ClipId>,
}

impl TextureCache {
    pub fn update_playback_texture(
        &mut self,
        ctx: &egui::Context,
        width: usize,
        height: usize,
        rgba_data: &[u8],
    ) {
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], rgba_data);
        if let Some(ref mut handle) = self.playback_texture {
            let [tw, th] = handle.size();
            if tw == width && th == height {
                handle.set(image, egui::TextureOptions::LINEAR);
                return;
            }
        }
        let texture = ctx.load_texture("playback_frame", image, egui::TextureOptions::LINEAR);
        self.playback_texture = Some(texture);
    }
}

impl wizard_ui::TextureLookup for TextureCache {
    fn thumbnail(&self, id: &ClipId) -> Option<&egui::TextureHandle> {
        self.thumbnails.get(id)
    }

    fn preview_frames(&self, id: &ClipId) -> Option<&Vec<egui::TextureHandle>> {
        self.preview_frames.get(id)
    }

    fn is_pending(&self, id: &ClipId) -> bool {
        self.pending_thumbnails.contains(id)
    }

    fn is_preview_loading(&self, id: &ClipId) -> bool {
        self.preview_requested.contains(id)
    }

    fn waveform_peaks(&self, id: &ClipId) -> Option<&Vec<(f32, f32)>> {
        self.waveform_peaks.get(id)
    }

    fn playback_frame(&self) -> Option<&egui::TextureHandle> {
        self.playback_texture.as_ref()
    }

    fn scrub_frame_at_time(&self, id: &ClipId, source_time: f64) -> Option<&egui::TextureHandle> {
        self.scrub_frames
            .get(id)
            .and_then(|entry| entry.frame_at_time(source_time))
    }
}

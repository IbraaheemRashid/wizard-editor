use std::collections::{HashMap, HashSet};

use wizard_state::clip::ClipId;

#[derive(Default)]
pub struct TextureCache {
    pub thumbnails: HashMap<ClipId, egui::TextureHandle>,
    pub preview_frames: HashMap<ClipId, Vec<egui::TextureHandle>>,
    pub pending_thumbnails: HashSet<ClipId>,
    pub preview_requested: HashSet<ClipId>,
    pub waveform_peaks: HashMap<ClipId, Vec<(f32, f32)>>,
    pub playback_texture: Option<egui::TextureHandle>,
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
}

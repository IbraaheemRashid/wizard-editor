pub mod browser;
pub mod constants;
pub mod preview;
pub mod theme;
pub mod timeline;
pub mod waveform_gpu;

use wizard_state::clip::ClipId;

pub trait TextureLookup {
    fn thumbnail(&self, id: &ClipId) -> Option<&egui::TextureHandle>;
    fn preview_frames(&self, id: &ClipId) -> Option<&Vec<egui::TextureHandle>>;
    fn is_pending(&self, id: &ClipId) -> bool;
    fn waveform_peaks(&self, id: &ClipId) -> Option<&Vec<(f32, f32)>>;
    fn playback_frame(&self) -> Option<&egui::TextureHandle>;
}

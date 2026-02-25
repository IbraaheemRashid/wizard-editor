pub mod browser;
pub mod constants;
// FOURTH PANEL
pub mod inspector;
pub mod preview;
pub mod theme;
pub mod timeline;
pub mod waveform_gpu;

use wizard_state::clip::ClipId;

pub trait TextureLookup {
    fn thumbnail(&self, id: &ClipId) -> Option<&egui::TextureHandle>;
    fn preview_frames(&self, id: &ClipId) -> Option<&Vec<egui::TextureHandle>>;
    fn is_pending(&self, id: &ClipId) -> bool;
    fn is_preview_loading(&self, id: &ClipId) -> bool;
    fn waveform_peaks(&self, id: &ClipId) -> Option<&Vec<(f32, f32)>>;
    fn playback_frame(&self) -> Option<&egui::TextureHandle>;
    fn scrub_frame_at_time(&self, id: &ClipId, source_time: f64) -> Option<&egui::TextureHandle>;
}

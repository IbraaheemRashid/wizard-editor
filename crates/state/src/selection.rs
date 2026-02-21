use crate::clip::ClipId;

#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub selected_clip: Option<ClipId>,
    pub hovered_clip: Option<ClipId>,
    pub selected_scrub_t: Option<f32>,
}

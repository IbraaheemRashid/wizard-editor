use crate::clip::ClipId;
use crate::timeline::TimelineClipId;

#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub selected_clip: Option<ClipId>,
    pub hovered_clip: Option<ClipId>,
    pub selected_scrub_t: Option<f32>,
    pub selected_timeline_clip: Option<TimelineClipId>,
}

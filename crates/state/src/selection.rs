use crate::clip::ClipId;

#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub selected_clip: Option<ClipId>,
    pub hovered_clip: Option<ClipId>,
}

use crate::clip::ClipId;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(Uuid);

impl TrackId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TrackId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone)]
pub struct TimelineClip {
    pub clip_id: ClipId,
    pub position: f64,
    pub duration: f64,
    pub in_point: f64,
    pub out_point: f64,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<TimelineClip>,
}

impl Track {
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            id: TrackId::new(),
            name: name.into(),
            kind,
            clips: Vec::new(),
        }
    }
}

pub fn default_tracks() -> Vec<Track> {
    vec![
        Track::new("V1", TrackKind::Video),
        Track::new("V2", TrackKind::Video),
        Track::new("A1", TrackKind::Audio),
    ]
}

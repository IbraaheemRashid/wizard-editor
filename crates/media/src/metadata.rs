use std::path::Path;

use crate::decoder::probe_metadata;

pub struct MediaMetadata {
    pub duration: Option<f64>,
    pub resolution: Option<(u32, u32)>,
    pub codec: Option<String>,
    pub has_video: bool,
}

pub fn extract_metadata(path: &Path) -> MediaMetadata {
    match probe_metadata(path) {
        Some(result) => MediaMetadata {
            duration: result.duration,
            has_video: result.resolution.is_some(),
            resolution: result.resolution,
            codec: result.codec,
        },
        None => MediaMetadata {
            duration: None,
            resolution: None,
            codec: None,
            has_video: false,
        },
    }
}

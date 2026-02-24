use std::path::Path;

use gst_pbutils::prelude::DiscovererStreamInfoExt;
use gstreamer as gst;
use gstreamer_pbutils as gst_pbutils;

use crate::gst_pipeline::init_once;

pub struct MediaMetadata {
    pub duration: Option<f64>,
    pub resolution: Option<(u32, u32)>,
    pub codec: Option<String>,
    pub has_video: bool,
}

pub fn extract_metadata(path: &Path) -> MediaMetadata {
    init_once();

    let uri = match url_from_path(path) {
        Some(u) => u,
        None => {
            return MediaMetadata {
                duration: None,
                resolution: None,
                codec: None,
                has_video: false,
            };
        }
    };

    let discoverer = match gst_pbutils::Discoverer::new(gst::ClockTime::from_seconds(10)) {
        Ok(d) => d,
        Err(_) => {
            return MediaMetadata {
                duration: None,
                resolution: None,
                codec: None,
                has_video: false,
            };
        }
    };

    let info = match discoverer.discover_uri(&uri) {
        Ok(i) => i,
        Err(_) => {
            return MediaMetadata {
                duration: None,
                resolution: None,
                codec: None,
                has_video: false,
            };
        }
    };

    let duration = info
        .duration()
        .map(|d| d.nseconds() as f64 / 1_000_000_000.0);

    let mut resolution = None;
    let mut codec = None;
    let mut has_video = false;

    if let Some(stream) = info.video_streams().into_iter().next() {
        has_video = true;
        let w = stream.width();
        let h = stream.height();
        if w > 0 && h > 0 {
            resolution = Some((w, h));
        }
        if let Some(caps) = DiscovererStreamInfoExt::caps(&stream) {
            if let Some(structure) = caps.structure(0) {
                codec = Some(structure.name().as_str().to_string());
            }
        }
    }

    MediaMetadata {
        duration,
        resolution,
        codec,
        has_video,
    }
}

fn url_from_path(path: &Path) -> Option<String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    Some(format!("file://{}", abs.display()))
}

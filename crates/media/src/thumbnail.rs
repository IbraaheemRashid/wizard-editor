use std::path::Path;

use crate::decoder::VideoDecoder;

const THUMB_WIDTH: u32 = 480;
const THUMB_HEIGHT: u32 = 270;

pub fn extract_thumbnail(path: &Path) -> Option<image::RgbaImage> {
    let mut decoder = VideoDecoder::open(path).ok()?;
    decoder
        .seek_and_decode(0.0, THUMB_WIDTH, THUMB_HEIGHT)
        .or_else(|| decoder.seek_and_decode(0.04, THUMB_WIDTH, THUMB_HEIGHT))
        .or_else(|| decoder.seek_and_decode(0.25, THUMB_WIDTH, THUMB_HEIGHT))
        .or_else(|| decoder.seek_and_decode(1.0, THUMB_WIDTH, THUMB_HEIGHT))
}

pub fn extract_preview_frames(path: &Path, count: usize) -> Vec<image::RgbaImage> {
    let mut decoder = match VideoDecoder::open(path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let duration = match decoder.duration_seconds() {
        Some(d) if d > 0.0 => d,
        _ => return Vec::new(),
    };

    if count <= 1 {
        return decoder
            .seek_and_decode(0.0, THUMB_WIDTH, THUMB_HEIGHT)
            .into_iter()
            .collect();
    }

    let times: Vec<f64> = (0..count)
        .map(|i| i as f64 * duration / count as f64)
        .collect();

    decoder.decode_frames_at_times(&times, THUMB_WIDTH, THUMB_HEIGHT)
}

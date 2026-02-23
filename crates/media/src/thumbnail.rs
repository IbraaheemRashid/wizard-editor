use std::path::Path;

use crate::decoder::VideoDecoder;

const THUMB_WIDTH: u32 = 480;
const THUMB_HEIGHT: u32 = 270;

const PREVIEW_WIDTH: u32 = 240;
const PREVIEW_HEIGHT: u32 = 135;

fn is_mostly_black(img: &image::RgbaImage) -> bool {
    let total = img.width() as usize * img.height() as usize;
    if total == 0 {
        return true;
    }
    let pixels = img.as_raw();
    let step = (total / 200).max(1);
    let mut dark_count = 0usize;
    let mut checked = 0usize;
    for i in (0..total).step_by(step) {
        let offset = i * 4;
        if offset + 3 >= pixels.len() {
            break;
        }
        let r = pixels[offset] as u32;
        let g = pixels[offset + 1] as u32;
        let b = pixels[offset + 2] as u32;
        if r + g + b < 30 {
            dark_count += 1;
        }
        checked += 1;
    }
    checked > 0 && dark_count * 100 / checked > 90
}

pub fn extract_thumbnail(path: &Path) -> Option<image::RgbaImage> {
    let mut decoder = VideoDecoder::open(path).ok()?;
    let times = [0.5, 1.0, 2.0, 0.0, 0.04, 0.25, 5.0];
    for &t in &times {
        if let Some(img) = decoder.seek_and_decode(t, THUMB_WIDTH, THUMB_HEIGHT) {
            if !is_mostly_black(&img) {
                return Some(img);
            }
        }
    }
    decoder.seek_and_decode(1.0, THUMB_WIDTH, THUMB_HEIGHT)
}

pub fn extract_frames_streaming(
    path: &Path,
    count: usize,
    width: u32,
    height: u32,
    sender: &std::sync::mpsc::Sender<(usize, f64, image::RgbaImage)>,
) {
    let mut decoder = match VideoDecoder::open(path) {
        Ok(d) => d,
        Err(_) => return,
    };

    let duration = match decoder.duration_seconds() {
        Some(d) if d > 0.0 => d,
        _ => return,
    };

    let actual_count = count.max(1);
    let targets: Vec<f64> = (0..actual_count)
        .map(|i| i as f64 * duration / actual_count as f64)
        .collect();

    decoder.seek(0.0);

    let mut next_idx = 0;
    loop {
        if next_idx >= actual_count {
            break;
        }

        let Some((img, pts)) = decoder.decode_next_frame_with_pts(width, height) else {
            break;
        };

        if pts + 0.001 >= targets[next_idx] {
            if sender.send((next_idx, pts, img.clone())).is_err() {
                return;
            }
            next_idx += 1;

            while next_idx < actual_count && pts + 0.001 >= targets[next_idx] {
                if sender.send((next_idx, pts, img.clone())).is_err() {
                    return;
                }
                next_idx += 1;
            }
        }
    }
}

pub fn extract_preview_frames_streaming(
    path: &Path,
    count: usize,
    sender: &std::sync::mpsc::Sender<(usize, image::RgbaImage)>,
) {
    let (pts_tx, pts_rx) = std::sync::mpsc::channel();
    extract_frames_streaming(path, count, PREVIEW_WIDTH, PREVIEW_HEIGHT, &pts_tx);
    drop(pts_tx);
    while let Ok((index, _pts, image)) = pts_rx.recv() {
        if sender.send((index, image)).is_err() {
            return;
        }
    }
}

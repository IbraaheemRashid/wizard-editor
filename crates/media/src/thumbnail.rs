use std::path::Path;
use std::process::Command;

const THUMB_WIDTH: u32 = 480;
const THUMB_HEIGHT: u32 = 270;
const EXPECTED_BYTES: usize = (THUMB_WIDTH * THUMB_HEIGHT * 4) as usize;

pub fn extract_thumbnail(path: &Path) -> Option<image::RgbaImage> {
    try_extract_at(path, "1").or_else(|| try_extract_at(path, "0"))
}

pub fn extract_preview_frames(path: &Path, count: usize) -> Vec<image::RgbaImage> {
    let duration = probe_duration(path);
    let duration = match duration {
        Some(d) if d > 0.0 => d,
        _ => return Vec::new(),
    };

    if count <= 1 {
        return try_extract_at(path, "0").into_iter().collect();
    }

    let fps = (count as f64 / duration).clamp(0.5, 30.0);
    let vf = format!(
        "fps={fps},scale={THUMB_WIDTH}:{THUMB_HEIGHT}:force_original_aspect_ratio=decrease,pad={THUMB_WIDTH}:{THUMB_HEIGHT}:(ow-iw)/2:(oh-ih)/2"
    );

    let output = Command::new("ffmpeg")
        .args(["-v", "quiet"])
        .args(["-i"])
        .arg(path)
        .args(["-vf", &vf])
        .args(["-frames:v", &count.to_string()])
        .args(["-f", "rawvideo"])
        .args(["-pix_fmt", "rgba"])
        .args(["pipe:1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let mut frames = Vec::new();
    for chunk in output.stdout.chunks_exact(EXPECTED_BYTES) {
        if let Some(img) = image::RgbaImage::from_raw(THUMB_WIDTH, THUMB_HEIGHT, chunk.to_vec()) {
            frames.push(img);
        }
    }
    frames
}

fn probe_duration(path: &Path) -> Option<f64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "compact=print_section=0:nokey=1",
            "-show_entries",
            "format=duration",
        ])
        .arg(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<f64>().ok()
}

fn try_extract_at(path: &Path, seek_seconds: &str) -> Option<image::RgbaImage> {
    let output = Command::new("ffmpeg")
        .args([
            "-ss",
            seek_seconds,
            "-i",
        ])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={THUMB_WIDTH}:{THUMB_HEIGHT}:force_original_aspect_ratio=decrease,pad={THUMB_WIDTH}:{THUMB_HEIGHT}:(ow-iw)/2:(oh-ih)/2"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.len() != EXPECTED_BYTES {
        return None;
    }

    image::RgbaImage::from_raw(THUMB_WIDTH, THUMB_HEIGHT, output.stdout)
}

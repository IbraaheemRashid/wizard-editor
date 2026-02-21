use std::path::Path;
use std::process::Command;

pub fn decode_pcm_snippet_f32_mono(
    path: &Path,
    start_seconds: f64,
    duration_seconds: f64,
    sample_rate_hz: u32,
) -> Vec<f32> {
    let start_seconds = start_seconds.max(0.0);
    let duration_seconds = duration_seconds.max(0.0);
    if duration_seconds <= 0.0 {
        return Vec::new();
    }

    let start_arg = format!("{start_seconds:.3}");
    let dur_arg = format!("{duration_seconds:.3}");
    let sr_arg = format!("{sample_rate_hz}");

    let output = Command::new("ffmpeg")
        .args(["-v", "quiet"])
        .args(["-ss", &start_arg])
        .args(["-i"])
        .arg(path)
        .args(["-t", &dur_arg])
        .args(["-vn"])
        .args(["-ac", "1"])
        .args(["-ar", &sr_arg])
        .args(["-f", "f32le"])
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

    let bytes = output.stdout;
    if bytes.len() < 4 {
        return Vec::new();
    }

    let mut samples = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    samples
}

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

pub fn extract_waveform_peaks(path: &Path, num_peaks: usize) -> Vec<(f32, f32)> {
    if num_peaks == 0 {
        return Vec::new();
    }

    let output = Command::new("ffmpeg")
        .args(["-v", "quiet"])
        .args(["-i"])
        .arg(path)
        .args(["-vn"])
        .args(["-ac", "1"])
        .args(["-ar", "8000"])
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

    let total_samples = bytes.len() / 4;
    let samples_per_peak = (total_samples / num_peaks).max(1);
    let mut peaks = Vec::with_capacity(num_peaks);

    for chunk_start in (0..total_samples).step_by(samples_per_peak) {
        let chunk_end = (chunk_start + samples_per_peak).min(total_samples);
        let mut min_val: f32 = 0.0;
        let mut max_val: f32 = 0.0;
        for i in chunk_start..chunk_end {
            let byte_offset = i * 4;
            if byte_offset + 4 > bytes.len() {
                break;
            }
            let sample = f32::from_le_bytes([
                bytes[byte_offset],
                bytes[byte_offset + 1],
                bytes[byte_offset + 2],
                bytes[byte_offset + 3],
            ]);
            min_val = min_val.min(sample);
            max_val = max_val.max(sample);
        }
        peaks.push((min_val, max_val));
        if peaks.len() >= num_peaks {
            break;
        }
    }

    peaks
}

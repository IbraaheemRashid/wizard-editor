use std::path::Path;

use crate::decoder::AudioDecoder;

pub fn extract_waveform_peaks(path: &Path, num_peaks: usize) -> Vec<(f32, f32)> {
    if num_peaks == 0 {
        return Vec::new();
    }

    let mut decoder = match AudioDecoder::open(path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let (samples, _sample_rate) = decoder.decode_all_mono_f32();
    if samples.is_empty() {
        return Vec::new();
    }

    let total_samples = samples.len();
    let samples_per_peak = (total_samples / num_peaks).max(1);
    let mut peaks = Vec::with_capacity(num_peaks);

    for chunk_start in (0..total_samples).step_by(samples_per_peak) {
        let chunk_end = (chunk_start + samples_per_peak).min(total_samples);
        let mut min_val: f32 = 0.0;
        let mut max_val: f32 = 0.0;
        for &sample in &samples[chunk_start..chunk_end] {
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

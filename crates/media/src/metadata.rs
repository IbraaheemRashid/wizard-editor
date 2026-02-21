use std::path::Path;
use std::process::Command;

use serde::Deserialize;

pub struct MediaMetadata {
    pub duration: Option<f64>,
    pub resolution: Option<(u32, u32)>,
    pub codec: Option<String>,
}

#[derive(Deserialize)]
struct FfprobeOutput {
    format: Option<FfprobeFormat>,
    streams: Option<Vec<FfprobeStream>>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

pub fn extract_metadata(path: &Path) -> MediaMetadata {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => {
            return MediaMetadata {
                duration: None,
                resolution: None,
                codec: None,
            }
        }
    };

    let parsed: FfprobeOutput = match serde_json::from_slice(&output.stdout) {
        Ok(p) => p,
        Err(_) => {
            return MediaMetadata {
                duration: None,
                resolution: None,
                codec: None,
            }
        }
    };

    let duration = parsed
        .format
        .and_then(|f| f.duration)
        .and_then(|d| d.parse::<f64>().ok());

    let video_stream = parsed.streams.and_then(|streams| {
        streams
            .into_iter()
            .find(|s| s.codec_type.as_deref() == Some("video"))
    });

    let resolution = video_stream
        .as_ref()
        .and_then(|s| match (s.width, s.height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        });

    let codec = video_stream.and_then(|s| s.codec_name);

    MediaMetadata {
        duration,
        resolution,
        codec,
    }
}

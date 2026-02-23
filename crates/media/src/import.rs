use std::path::{Path, PathBuf};

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "avi", "mkv", "webm", "m4v", "mxf", "ts", "mts", "m2ts", "flv", "wmv", "mpg",
    "mpeg", "vob", "3gp", "3g2", "ogv", "f4v", "divx", "asf", "rm", "rmvb", "dv", "r3d", "braw",
];

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "aac", "flac", "ogg", "m4a", "wma", "aiff", "aif", "opus", "alac",
];

pub fn is_media_extension(ext: &str) -> bool {
    let lower = ext.to_lowercase();
    VIDEO_EXTENSIONS.contains(&lower.as_str()) || AUDIO_EXTENSIONS.contains(&lower.as_str())
}

pub fn scan_folder(path: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    if is_media_extension(ext) {
                        results.push(p);
                    }
                }
            }
        }
    }
    results.sort();
    results
}

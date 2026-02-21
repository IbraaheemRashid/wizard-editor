use std::path::{Path, PathBuf};

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "avi", "mkv", "webm", "m4v", "mxf", "ts", "mts", "m2ts", "flv", "wmv", "mpg",
    "mpeg", "vob", "3gp", "3g2", "ogv", "f4v", "divx", "asf", "rm", "rmvb", "dv", "r3d", "braw",
];

pub fn scan_folder(path: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    if VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                        results.push(p);
                    }
                }
            }
        }
    }
    results.sort();
    results
}

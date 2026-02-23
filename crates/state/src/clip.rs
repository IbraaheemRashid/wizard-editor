use std::path::PathBuf;
use uuid::Uuid;

use crate::tag::Tag;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClipId(Uuid);

impl ClipId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ClipId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct Clip {
    pub id: ClipId,
    pub path: PathBuf,
    pub filename: String,
    pub display_name: Option<String>,
    pub duration: Option<f64>,
    pub resolution: Option<(u32, u32)>,
    pub codec: Option<String>,
    pub audio_only: bool,
    pub search_haystack: String,
}

impl Clip {
    pub fn from_path(path: PathBuf) -> Self {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mut search_haystack = filename.to_lowercase();
        if !ext.is_empty() {
            search_haystack.push(' ');
            search_haystack.push('.');
            search_haystack.push_str(&ext.to_lowercase());
        }
        Self {
            id: ClipId::new(),
            path,
            filename,
            display_name: None,
            duration: None,
            resolution: None,
            codec: None,
            audio_only: false,
            search_haystack,
        }
    }

    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.filename)
    }

    pub fn extension(&self) -> &str {
        self.path.extension().and_then(|e| e.to_str()).unwrap_or("")
    }

    pub fn rebuild_search_haystack(&mut self, tag_mask: u32) {
        let mut haystack = self.filename.to_lowercase();

        if let Some(name) = &self.display_name {
            haystack.push(' ');
            haystack.push_str(&name.to_lowercase());
        }

        let ext = self.extension();
        if !ext.is_empty() {
            haystack.push(' ');
            haystack.push('.');
            haystack.push_str(&ext.to_lowercase());
        }

        if let Some(codec) = &self.codec {
            haystack.push(' ');
            haystack.push_str(&codec.to_lowercase());
        }

        if let Some((w, h)) = self.resolution {
            haystack.push(' ');
            haystack.push_str(&format!("{w}x{h}"));
        }

        if let Some(dur) = self.duration {
            let dur_i = dur.round().max(0.0) as i64;
            let m = dur_i / 60;
            let s = dur_i % 60;
            haystack.push(' ');
            haystack.push_str(&format!("{m}:{s:02}"));
            haystack.push(' ');
            haystack.push_str(&dur_i.to_string());
        }

        for tag in Tag::ALL {
            if (tag_mask & tag.bit()) != 0 {
                haystack.push(' ');
                haystack.push_str(&tag.label().to_lowercase());
            }
        }

        self.search_haystack = haystack;
    }
}

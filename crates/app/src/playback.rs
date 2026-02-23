use std::collections::HashSet;

use crate::workers::preview_worker::PreviewRequest;
use crate::workers::scrub_cache_worker::ScrubCacheRequest;
use crate::EditorApp;

impl EditorApp {
    pub fn enqueue_visible_previews(&mut self) {
        const PREFETCH_PER_FRAME: usize = 2;
        let mut remaining = PREFETCH_PER_FRAME;

        if let Some(clip_id) = self.state.ui.selection.hovered_clip {
            let _ = self.enqueue_preview_request(clip_id, true);
        }

        if let Some(clip_id) = self.state.ui.selection.primary_clip() {
            let _ = self.enqueue_preview_request(clip_id, true);
        }

        let visible: Vec<wizard_state::clip::ClipId> = self.state.ui.browser.visible_clips.clone();
        for clip_id in visible {
            if remaining == 0 {
                break;
            }
            if self.enqueue_preview_request(clip_id, false) {
                remaining -= 1;
            }
        }
    }

    fn enqueue_preview_request(
        &mut self,
        clip_id: wizard_state::clip::ClipId,
        priority: bool,
    ) -> bool {
        if self.textures.preview_frames.contains_key(&clip_id) {
            return false;
        }
        if self.textures.preview_requested.contains(&clip_id) {
            return false;
        }
        let Some(clip) = self.state.project.clips.get(&clip_id) else {
            return false;
        };

        self.textures.preview_requested.insert(clip_id);
        let _ = self.preview.req_tx.send(PreviewRequest::Enqueue {
            clip_id,
            path: clip.path.clone(),
            priority,
        });
        true
    }

    pub fn enqueue_scrub_cache_for_timeline_clips(&mut self) {
        let mut on_timeline: HashSet<wizard_state::clip::ClipId> = HashSet::new();
        for track in &self.state.project.timeline.video_tracks {
            for clip in &track.clips {
                on_timeline.insert(clip.source_id);
            }
        }

        for &source_id in &on_timeline {
            if self.textures.scrub_frames.contains_key(&source_id) {
                continue;
            }
            if self.textures.scrub_requested.contains(&source_id) {
                continue;
            }
            let Some(clip) = self.state.project.clips.get(&source_id) else {
                continue;
            };
            if clip.audio_only {
                continue;
            }
            self.textures.scrub_requested.insert(source_id);
            let _ = self.scrub_cache.req_tx.send(ScrubCacheRequest::Extract {
                clip_id: source_id,
                path: clip.path.clone(),
            });
        }

        let stale: Vec<wizard_state::clip::ClipId> = self
            .textures
            .scrub_frames
            .keys()
            .filter(|id| !on_timeline.contains(id))
            .copied()
            .collect();
        for id in stale {
            self.textures.scrub_frames.remove(&id);
            self.textures.scrub_requested.remove(&id);
            let _ = self
                .scrub_cache
                .req_tx
                .send(ScrubCacheRequest::Invalidate { clip_id: id });
        }
    }
}

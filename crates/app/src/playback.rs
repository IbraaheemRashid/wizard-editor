use wizard_state::playback::PlaybackState;

use crate::constants::*;
use crate::workers::audio_worker::AudioPreviewRequest;
use crate::workers::preview_worker::PreviewRequest;
use crate::workers::video_decode_worker::{
    VideoDecodeRequest, PLAYBACK_DECODE_HEIGHT, PLAYBACK_DECODE_WIDTH,
};
use crate::EditorApp;

impl EditorApp {
    pub fn is_playing(&self) -> bool {
        matches!(
            self.state.project.playback.state,
            PlaybackState::Playing | PlaybackState::PlayingReverse
        )
    }

    pub fn path_has_no_audio(&self, path: &std::path::Path) -> bool {
        self.no_audio_paths
            .lock()
            .map(|set| set.contains(path))
            .unwrap_or(false)
    }

    pub fn reset_audio_sources(&mut self) {
        self.mixer.clear();
        if let Some(ref output) = self.audio_output {
            output.clear_buffer();
        }
    }

    pub fn handle_playback_stop_transition(&mut self) {
        let _ = self.audio.req_tx.send(AudioPreviewRequest::Stop);
        self.last_hover_audio_request = None;
        self.last_scrub_audio_request = None;
        self.last_video_decode_request = None;
        self.last_decoded_frame = None;
        self.shadow = None;
        self.reset_audio_sources();
    }

    pub fn handle_playback_state_transition(
        &mut self,
        previous: PlaybackState,
        current: PlaybackState,
    ) {
        let direction_switched = matches!(
            (previous, current),
            (PlaybackState::Playing, PlaybackState::PlayingReverse)
                | (PlaybackState::PlayingReverse, PlaybackState::Playing)
        );
        if direction_switched {
            self.forward = None;
            self.reverse = None;
            self.shadow = None;
            self.handle_playback_stop_transition();
        }
    }

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

    pub fn update_hover_audio(&mut self) {
        if self.audio_output.is_none() {
            return;
        }

        let is_playing = self.is_playing();

        let Some(clip_id) = self.state.ui.selection.hovered_clip else {
            if !is_playing
                && self.state.ui.timeline.scrubbing.is_none()
                && self.last_hover_audio_request.take().is_some()
            {
                let _ = self.audio.req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let Some(t_norm) = self.state.ui.browser.hovered_scrub_t else {
            if !is_playing
                && self.state.ui.timeline.scrubbing.is_none()
                && self.last_hover_audio_request.take().is_some()
            {
                let _ = self.audio.req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let has_preview_frames = self
            .textures
            .preview_frames
            .get(&clip_id)
            .is_some_and(|f| !f.is_empty());
        if !has_preview_frames {
            return;
        }

        let Some(clip) = self.state.project.clips.get(&clip_id) else {
            return;
        };
        if self.path_has_no_audio(&clip.path) {
            return;
        }
        let Some(duration) = clip.duration else {
            return;
        };

        let time_seconds = (t_norm.clamp(0.0, 1.0) as f64 * duration).clamp(0.0, duration);
        let bucket = (time_seconds * HOVER_AUDIO_BUCKET_RATE).round() as i64;
        if self.last_hover_audio_request == Some((clip_id, bucket)) {
            return;
        }
        self.last_hover_audio_request = Some((clip_id, bucket));

        let _ = self.audio.req_tx.send(AudioPreviewRequest::Preview {
            path: clip.path.clone(),
            time_seconds,
            sample_rate_hz: self.audio_sample_rate,
        });
    }

    pub fn update_timeline_scrub_audio(&mut self) {
        if self.audio_output.is_none() {
            return;
        }
        let Some(time) = self.state.ui.timeline.scrubbing else {
            return;
        };
        if self.state.ui.browser.hovered_scrub_t.is_some() {
            return;
        }
        let Some(hit) = self.state.project.timeline.audio_clip_at_time(time) else {
            return;
        };
        let Some(clip) = self.state.project.clips.get(&hit.clip.source_id) else {
            return;
        };
        if self.path_has_no_audio(&clip.path) {
            return;
        }

        let bucket = (hit.source_time * SCRUB_AUDIO_BUCKET_RATE).round() as i64;
        if self.last_scrub_audio_request == Some((hit.clip.source_id, bucket)) {
            return;
        }
        self.last_scrub_audio_request = Some((hit.clip.source_id, bucket));

        let _ = self.audio.req_tx.send(AudioPreviewRequest::Preview {
            path: clip.path.clone(),
            time_seconds: hit.source_time,
            sample_rate_hz: self.audio_sample_rate,
        });
    }

    pub fn update_playback_frame(&mut self, now: f64) {
        let last_frame_time = self.last_pipeline_frame_time();
        let fwd_started_at = self.forward.as_ref().map(|f| f.started_at);
        let fwd_frame_delivered = self.pipeline_frame_delivered();

        let forward_frame_age = last_frame_time.map(|t| now - t).unwrap_or(f64::INFINITY);
        let forward_startup_age = fwd_started_at.map(|t| now - t).unwrap_or(f64::INFINITY);
        let forward_frame_gap_stalled = last_frame_time
            .is_some_and(|_| fwd_frame_delivered && forward_frame_age > PIPELINE_STALL_THRESHOLD_S);
        let forward_pipeline_stalled = self.forward.is_some()
            && self.state.project.playback.state == PlaybackState::Playing
            && (forward_frame_gap_stalled
                || (!fwd_frame_delivered && forward_startup_age > FORWARD_STARTUP_GRACE_S));
        let rev_last_frame_time = self.reverse.as_ref().and_then(|r| r.last_frame_time);
        let rev_started_at = self.reverse.as_ref().map(|r| r.started_at);
        let reverse_startup_age = rev_started_at.map(|t| now - t).unwrap_or(f64::INFINITY);
        let reverse_pipeline_stalled = self.reverse.is_some()
            && self.state.project.playback.state == PlaybackState::PlayingReverse
            && (rev_last_frame_time.is_some_and(|t| (now - t) > PIPELINE_STALL_THRESHOLD_S)
                || (rev_last_frame_time.is_none()
                    && reverse_startup_age > FORWARD_STARTUP_GRACE_S));
        if (self.forward.is_some() && !forward_pipeline_stalled)
            || (self.reverse.is_some() && !reverse_pipeline_stalled)
        {
            if self.forward.is_some()
                && self.state.project.playback.state == PlaybackState::Playing
                && !fwd_frame_delivered
                && forward_startup_age <= FORWARD_STARTUP_GRACE_S
            {
                return;
            }
            if self.reverse.is_some()
                && self.state.project.playback.state == PlaybackState::PlayingReverse
                && rev_last_frame_time.is_none()
                && reverse_startup_age <= FORWARD_STARTUP_GRACE_S
            {
                return;
            }
            return;
        }

        let playhead = self.state.project.playback.playhead;
        let is_active = self.state.project.playback.state != PlaybackState::Stopped;
        let is_scrubbing = self.state.ui.timeline.scrubbing.is_some();

        if !is_active && !is_scrubbing {
            return;
        }

        let time = if is_scrubbing {
            self.state.ui.timeline.scrubbing.unwrap_or(playhead)
        } else {
            playhead
        };

        if let Some(hit) = self.state.project.timeline.video_clip_at_time(time) {
            if let Some(clip) = self.state.project.clips.get(&hit.clip.source_id) {
                let bucket = (hit.source_time * VIDEO_DECODE_BUCKET_RATE).round() as i64;
                if self.last_video_decode_request == Some((hit.clip.source_id, bucket)) {
                    return;
                }

                let (tw, th) = if is_scrubbing {
                    (SCRUB_DECODE_WIDTH, SCRUB_DECODE_HEIGHT)
                } else {
                    (PLAYBACK_DECODE_WIDTH, PLAYBACK_DECODE_HEIGHT)
                };
                let _ = self.video_decode.req_tx.send(VideoDecodeRequest {
                    clip_id: hit.clip.source_id,
                    path: clip.path.clone(),
                    time_seconds: hit.source_time,
                    target_width: tw,
                    target_height: th,
                    max_decode_frames: if is_scrubbing {
                        SCRUB_MAX_DECODE_FRAMES
                    } else {
                        PLAYBACK_MAX_DECODE_FRAMES
                    },
                });
                self.last_video_decode_request = Some((hit.clip.source_id, bucket));
            }
        } else {
            self.last_video_decode_request = None;
        }
    }
}

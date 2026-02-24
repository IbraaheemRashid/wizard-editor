use wizard_state::project::AppState;

use crate::constants::*;
use crate::texture_cache::TextureCache;
use crate::workers;
use crate::workers::audio_worker::AudioPreviewRequest;
use crate::workers::video_decode_worker::VideoDecodeRequest;

use super::PlaybackEngine;

impl PlaybackEngine {
    pub fn update_hover_audio(&mut self, state: &AppState, textures: &TextureCache) {
        if self.audio_output.is_none() {
            return;
        }

        let is_playing = self.is_playing(state);

        let Some(clip_id) = state.ui.selection.hovered_clip else {
            if !is_playing
                && state.ui.timeline.scrubbing.is_none()
                && self.last_hover_audio_request.take().is_some()
            {
                let _ = self.audio.req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let Some(t_norm) = state.ui.browser.hovered_scrub_t else {
            if !is_playing
                && state.ui.timeline.scrubbing.is_none()
                && self.last_hover_audio_request.take().is_some()
            {
                let _ = self.audio.req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let has_preview_frames = textures
            .preview_frames
            .get(&clip_id)
            .is_some_and(|f| !f.is_empty());
        if !has_preview_frames {
            return;
        }

        let Some(clip) = state.project.clips.get(&clip_id) else {
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

    pub fn update_timeline_scrub_audio(&mut self, state: &AppState) {
        if self.audio_output.is_none() {
            return;
        }
        let Some(time) = state.ui.timeline.scrubbing else {
            return;
        };
        if state.ui.browser.hovered_scrub_t.is_some() {
            return;
        }
        let Some(hit) = state.project.timeline.audio_clip_at_time(time) else {
            return;
        };
        let Some(clip) = state.project.clips.get(&hit.clip.source_id) else {
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

    pub fn update_playback_frame(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        now: f64,
    ) {
        let fwd_stall = self.forward.as_ref().map(|f| f.stall_status(now));
        let rev_stall = self.reverse.as_ref().map(|r| r.stall_status(now));

        let forward_pipeline_stalled = self.forward.is_some()
            && state.project.playback.state == wizard_state::playback::PlaybackState::Playing
            && fwd_stall.is_some_and(|s| s.is_stalled());
        let reverse_pipeline_stalled = self.reverse.is_some()
            && state.project.playback.state
                == wizard_state::playback::PlaybackState::PlayingReverse
            && rev_stall.is_some_and(|s| s.is_stalled());

        if (self.forward.is_some() && !forward_pipeline_stalled)
            || (self.reverse.is_some() && !reverse_pipeline_stalled)
        {
            let fwd_starting = fwd_stall == Some(crate::pipeline::PipelineStatus::StartingUp)
                && state.project.playback.state == wizard_state::playback::PlaybackState::Playing;
            let rev_starting = rev_stall == Some(crate::pipeline::PipelineStatus::StartingUp)
                && state.project.playback.state
                    == wizard_state::playback::PlaybackState::PlayingReverse;
            if fwd_starting || rev_starting {
                return;
            }
            return;
        }

        let playhead = state.project.playback.playhead;
        let is_active =
            state.project.playback.state != wizard_state::playback::PlaybackState::Stopped;
        let is_scrubbing = state.ui.timeline.scrubbing.is_some();
        let playhead_changed_while_stopped =
            !is_active && (playhead - self.last_playhead_observed).abs() > f64::EPSILON;

        if !is_active && !is_scrubbing && !playhead_changed_while_stopped {
            return;
        }

        let time = if is_scrubbing {
            state.ui.timeline.scrubbing.unwrap_or(playhead)
        } else {
            playhead
        };

        if let Some(hit) = state.project.timeline.video_clip_at_time(time) {
            if is_scrubbing {
                if let Some(tex) = textures
                    .scrub_frames
                    .get(&hit.clip.source_id)
                    .and_then(|entry| entry.frame_at_time(hit.source_time))
                {
                    textures.playback_texture = Some(tex.clone());
                    return;
                }
            }

            if let Some(clip) = state.project.clips.get(&hit.clip.source_id) {
                let bucket = (hit.source_time * VIDEO_DECODE_BUCKET_RATE).round() as i64;
                if self.last_video_decode_request == Some((hit.clip.source_id, bucket)) {
                    return;
                }

                let (tw, th) = if is_scrubbing {
                    (SCRUB_DECODE_WIDTH, SCRUB_DECODE_HEIGHT)
                } else {
                    (
                        workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
                        workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
                    )
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

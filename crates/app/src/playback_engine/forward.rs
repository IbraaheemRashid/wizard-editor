use wizard_media::pipeline::DecodedFrame;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;
use wizard_state::timeline::TimelineClipId;

use crate::constants::*;
use crate::pipeline::{
    ForwardPipelineState, PendingPipeline, PendingShadowPipeline, ShadowAudioSourceRequest,
    ShadowPipelineState,
};
use crate::texture_cache::TextureCache;
use crate::workers;

use super::PlaybackEngine;

impl PlaybackEngine {
    pub fn manage_shadow_pipeline(&mut self, state: &mut AppState, now: f64) {
        self.poll_pending_shadow_pipeline(now);

        if state.project.playback.state != PlaybackState::Playing {
            return;
        }
        let fwd = match self.forward.as_ref() {
            Some(f) => f,
            None => return,
        };
        let current_timeline_clip = fwd.timeline_clip;

        if let Some(ref shadow) = self.shadow {
            if let Some(next_hit) = state
                .project
                .timeline
                .next_clip_after(current_timeline_clip)
            {
                if shadow.timeline_clip == next_hit.clip.id {
                    return;
                }
            }
        }
        if let Some(ref shadow) = self.pending_shadow {
            if let Some(next_hit) = state
                .project
                .timeline
                .next_clip_after(current_timeline_clip)
            {
                if shadow.timeline_clip == next_hit.clip.id {
                    return;
                }
            }
        }

        let time_remaining = state
            .project
            .timeline
            .time_remaining_in_clip(current_timeline_clip, state.project.playback.playhead);
        let Some(remaining) = time_remaining else {
            return;
        };

        let speed = state.project.playback.speed;
        if remaining > SHADOW_LOOKAHEAD_S / speed {
            return;
        }

        let Some(next_hit) = state
            .project
            .timeline
            .next_clip_after(current_timeline_clip)
        else {
            return;
        };

        let next_clip_id = next_hit.clip.source_id;
        let next_timeline_clip_id = next_hit.clip.id;
        let Some(clip) = state.project.clips.get(&next_clip_id) else {
            return;
        };
        let path = clip.path.clone();

        self.shadow = None;
        self.pending_shadow = None;

        let next_time = state
            .project
            .timeline
            .find_clip(current_timeline_clip)
            .map(|(_, _, tc)| tc.timeline_start + tc.duration)
            .unwrap_or(state.project.playback.playhead);

        let mut audio_requests = Vec::new();
        let audio_hits = state.project.timeline.audio_clips_at_time(next_time);
        for hit in audio_hits {
            let Some(aclip) = state.project.clips.get(&hit.clip.source_id) else {
                continue;
            };
            if self.path_has_no_audio(&aclip.path) {
                continue;
            }
            audio_requests.push(ShadowAudioSourceRequest {
                path: aclip.path.clone(),
                source_time: hit.source_time,
            });
        }

        self.pending_shadow = Some(PendingShadowPipeline::spawn(
            &path,
            next_hit.source_time,
            workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
            workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
            self.audio_sample_rate,
            self.audio_channels,
            speed,
            next_clip_id,
            next_timeline_clip_id,
            audio_requests,
            now,
        ));
    }

    pub fn poll_pending_shadow_pipeline(&mut self, _now: f64) {
        let pending = match self.pending_shadow.as_ref() {
            Some(p) => p,
            None => return,
        };

        let result = match pending.try_recv() {
            Some(r) => r,
            None => return,
        };

        let pending = self.pending_shadow.take().expect("checked above");

        if let Ok(build) = result {
            self.shadow = Some(ShadowPipelineState {
                handle: build.handle,
                clip: pending.clip,
                timeline_clip: pending.timeline_clip,
                first_frame_ready: false,
                buffered_frame: None,
                audio_sources: build.audio_sources,
            });
        }
    }

    pub fn promote_shadow_pipeline(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        next_time: f64,
        next_hit: &wizard_state::timeline::PlayheadHit,
        now: f64,
        ctx: &egui::Context,
    ) -> bool {
        let next_timeline_clip_id = next_hit.clip.id;

        let shadow_matches = self
            .shadow
            .as_ref()
            .is_some_and(|s| s.timeline_clip == next_timeline_clip_id);

        if !shadow_matches {
            return false;
        }

        let shadow = self.shadow.take().expect("shadow checked above");

        let _ = shadow.handle.begin_playing();

        let mut fwd = ForwardPipelineState {
            handle: shadow.handle,
            clip: shadow.clip,
            timeline_clip: shadow.timeline_clip,
            pts_offset: None,
            speed: state.project.playback.speed,
            frame_delivered: shadow.buffered_frame.is_some(),
            activated: true,
            started_at: now,
            last_frame_time: if shadow.buffered_frame.is_some() {
                Some(now)
            } else {
                None
            },
            age: 0,
        };

        state.project.playback.playhead = next_time;

        if let Some(frame) = shadow.buffered_frame {
            textures.update_playback_texture(
                ctx,
                frame.width as usize,
                frame.height as usize,
                &frame.rgba_data,
            );
            self.last_decoded_frame = Some((frame.pts_seconds, "fwd"));
            fwd.frame_delivered = true;
            fwd.last_frame_time = Some(now);
        } else {
            self.show_scrub_cache_bridge_frame(
                textures,
                next_hit.clip.source_id,
                next_hit.source_time,
            );
            self.last_video_decode_request = None;
        }

        self.forward = Some(fwd);

        if !shadow.audio_sources.is_empty() {
            for (ref audio_handle, _) in &shadow.audio_sources {
                let _ = audio_handle.begin_playing();
            }
            self.mixer.replace_sources(shadow.audio_sources);
            if let Some(ref output) = self.audio_output {
                output.clear_buffer();
            }
        } else {
            self.reset_audio_sources();
            self.start_audio_sources(state);
        }

        true
    }

    pub fn poll_pending_pipeline(&mut self, now: f64) {
        let pending = match self.pending_forward.as_ref() {
            Some(p) => p,
            None => return,
        };

        let result = match pending.try_recv() {
            Some(r) => r,
            None => return,
        };

        let pending = self.pending_forward.take().expect("checked above");

        if let Ok(handle) = result {
            self.runtime_log_frames = 0;
            self.forward = Some(ForwardPipelineState {
                handle,
                clip: pending.clip,
                timeline_clip: pending.timeline_clip,
                pts_offset: None,
                speed: pending.speed,
                frame_delivered: false,
                activated: false,
                started_at: pending.started_at,
                last_frame_time: None,
                age: 0,
            });
            self.try_activate_pipeline(now);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_pipeline(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        timeline_clip_id: TimelineClipId,
        clip_id: ClipId,
        path: &std::path::Path,
        source_time: f64,
        now: f64,
    ) {
        self.reset_audio_sources();

        let speed = state.project.playback.speed;

        self.pending_forward = Some(PendingPipeline::spawn(
            path,
            source_time,
            workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
            workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
            self.audio_sample_rate,
            self.audio_channels,
            speed,
            clip_id,
            timeline_clip_id,
            now,
        ));

        self.show_scrub_cache_bridge_frame(textures, clip_id, source_time);
        self.last_video_decode_request = None;

        self.start_audio_sources(state);

        if let Some((_, _, tc)) = state.project.timeline.find_clip(timeline_clip_id) {
            let remaining = (tc.timeline_start + tc.duration) - state.project.playback.playhead;
            if remaining < SHADOW_LOOKAHEAD_S / state.project.playback.speed {
                self.manage_shadow_pipeline(state, now);
            }
        }
    }

    pub fn apply_pipeline_frame(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        ctx: &egui::Context,
        frame: &DecodedFrame,
        now: f64,
    ) -> bool {
        self.last_decoded_frame = Some((frame.pts_seconds, "fwd"));
        textures.update_playback_texture(
            ctx,
            frame.width as usize,
            frame.height as usize,
            &frame.rgba_data,
        );

        let active_clip = self.forward.as_ref().and_then(|fwd| {
            state
                .project
                .timeline
                .find_clip(fwd.timeline_clip)
                .map(|(_, _, tc)| (tc.timeline_start, tc.duration, tc.source_in, tc.source_out))
        });
        if let Some((timeline_start, duration, source_in, source_out)) = active_clip {
            let fwd = self.forward.as_mut().expect("forward checked above");
            let expected_source_at_playhead =
                source_in + (state.project.playback.playhead - timeline_start).max(0.0);
            let pts_offset = if let Some(offset) = fwd.pts_offset {
                offset
            } else {
                let offset = frame.pts_seconds - expected_source_at_playhead;
                fwd.pts_offset = Some(offset);
                offset
            };
            let mapped_source_pts = frame.pts_seconds - pts_offset;

            if mapped_source_pts >= source_out {
                let playhead_near_end =
                    state.project.playback.playhead >= (timeline_start + duration - 0.016);
                if playhead_near_end && fwd.age >= 2 {
                    let next_time = timeline_start + duration;
                    state.project.playback.playhead = next_time;
                    self.forward = None;

                    if let Some(next_hit) = state.project.timeline.video_clip_at_time(next_time) {
                        let next_hit_clone = next_hit.clone();
                        if self.promote_shadow_pipeline(
                            state,
                            textures,
                            next_time,
                            &next_hit_clone,
                            now,
                            ctx,
                        ) {
                            return false;
                        }

                        let next_clip_id = next_hit.clip.source_id;
                        let next_timeline_clip_id = next_hit.clip.id;
                        if let Some(clip) = state.project.clips.get(&next_clip_id) {
                            let path = clip.path.clone();
                            self.start_pipeline(
                                state,
                                textures,
                                next_timeline_clip_id,
                                next_clip_id,
                                &path,
                                next_hit.source_time,
                                now,
                            );
                        }
                    } else {
                        self.reset_audio_sources();
                    }
                    return false;
                }
                if let Some(ref mut fwd) = self.forward {
                    fwd.last_frame_time = Some(now);
                }
                return true;
            }

            if mapped_source_pts >= source_in && mapped_source_pts < source_out {
                if !fwd.frame_delivered {
                    let new_playhead = timeline_start + (mapped_source_pts - source_in);
                    state.project.playback.playhead = new_playhead;
                }
                fwd.frame_delivered = true;
            }
        } else if let Some(timeline_pos) =
            self.find_timeline_hit_for_source_pts(state, frame.pts_seconds)
        {
            if let Some(ref mut fwd) = self.forward {
                fwd.frame_delivered = true;
            }
            state.project.playback.playhead = timeline_pos;
        }
        if let Some(ref mut fwd) = self.forward {
            fwd.last_frame_time = Some(now);
        }

        self.update_video_fps(state, now);
        true
    }

    pub(super) fn pick_best_frame_for_playhead(
        &self,
        state: &AppState,
        frames: &[DecodedFrame],
    ) -> usize {
        let playhead = state.project.playback.playhead;
        let pts_offset = self.forward.as_ref().and_then(|f| f.pts_offset);
        let mut best_idx = 0;
        for (i, frame) in frames.iter().enumerate() {
            let mapped = match pts_offset {
                Some(off) => frame.pts_seconds - off,
                None => frame.pts_seconds,
            };
            let timeline_pos = self
                .forward
                .as_ref()
                .and_then(|fwd| {
                    state
                        .project
                        .timeline
                        .find_clip(fwd.timeline_clip)
                        .map(|(_, _, tc)| tc.timeline_start + (mapped - tc.source_in))
                })
                .unwrap_or(mapped);
            if timeline_pos <= playhead + 0.05 {
                best_idx = i;
            } else {
                break;
            }
        }
        best_idx
    }

    pub fn find_timeline_hit_for_source_pts(&self, state: &AppState, pts: f64) -> Option<f64> {
        let fwd = self.forward.as_ref()?;
        let (_, _, tc) = state.project.timeline.find_clip(fwd.timeline_clip)?;
        if pts >= tc.source_in && pts < tc.source_out {
            return Some(tc.timeline_start + (pts - tc.source_in));
        }
        None
    }
}

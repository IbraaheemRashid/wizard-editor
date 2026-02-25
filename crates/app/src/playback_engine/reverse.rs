use std::path::Path;

use wizard_media::pipeline::DecodedFrame;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;
use wizard_state::timeline::TimelineClipId;

use crate::constants::*;
use crate::pipeline::{PendingReversePipeline, ReversePipelineState, ReverseShadowState};
use crate::texture_cache::TextureCache;
use crate::workers;

use super::PlaybackEngine;

fn reverse_boundary_threshold_s(path: &Path) -> f64 {
    let is_low_fps_mpeg = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .is_some_and(|ext| matches!(ext.as_str(), "mpg" | "mpeg"));
    if is_low_fps_mpeg {
        0.4
    } else {
        0.12
    }
}

impl PlaybackEngine {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn manage_reverse_pipeline(
        &mut self,
        state: &AppState,
        textures: &mut TextureCache,
        timeline_clip_id: TimelineClipId,
        clip_id: ClipId,
        path: &Path,
        source_time: f64,
        now: f64,
    ) {
        let speed = state.project.playback.speed;

        if let Some(ref mut rev) = self.reverse {
            if (speed - rev.speed).abs() > 0.01 {
                rev.handle.update_speed(speed);
                rev.speed = speed;
            }
        }

        let pending_matches = self
            .pending_reverse
            .as_ref()
            .is_some_and(|p| p.timeline_clip == timeline_clip_id && p.clip.0 == clip_id);

        let needs_new = !pending_matches
            && match self.reverse.as_ref() {
                None => true,
                Some(rev) => {
                    let startup_timeout_s = REVERSE_STARTUP_TIMEOUT_S.max(1.5);
                    let startup_timed_out =
                        rev.last_frame_time.is_none() && now - rev.started_at > startup_timeout_s;
                    let long_stalled = rev
                        .last_frame_time
                        .is_some_and(|t| now - t > FRAME_GAP_LONG_STALL_S);
                    let reverse_is_stuck = startup_timed_out || long_stalled;
                    rev.timeline_clip != timeline_clip_id
                        || rev.clip.0 != clip_id
                        || rev.clip.1 != *path
                        || reverse_is_stuck
                }
            };

        if needs_new {
            let too_close_to_start = self.reverse.is_none()
                && state
                    .project
                    .timeline
                    .find_clip(timeline_clip_id)
                    .is_some_and(|(_, _, tc)| {
                        let distance_into_clip = source_time - tc.source_in;
                        distance_into_clip < reverse_boundary_threshold_s(path)
                    });
            if too_close_to_start {
                return;
            }
            self.reverse = None;
            self.pending_reverse = None;
            self.pending_reverse = Some(PendingReversePipeline::spawn(
                path,
                source_time,
                workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
                workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
                speed,
                clip_id,
                timeline_clip_id,
                now,
            ));
            self.show_scrub_cache_bridge_frame(textures, clip_id, source_time);
        }
    }

    pub fn poll_pending_reverse_pipeline(&mut self, now: f64) {
        let pending = match self.pending_reverse.as_ref() {
            Some(p) => p,
            None => return,
        };

        let result = match pending.try_recv() {
            Some(r) => r,
            None => return,
        };

        let pending = self.pending_reverse.take().expect("checked above");
        if let Ok(handle) = result {
            self.reverse = Some(ReversePipelineState {
                handle,
                clip: pending.clip,
                timeline_clip: pending.timeline_clip,
                pts_offset: None,
                speed: pending.speed,
                activated: false,
                started_at: pending.started_at,
                last_frame_time: None,
            });
            self.try_activate_pipeline(now);
        }
    }

    pub fn apply_reverse_pipeline_frame(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        ctx: &egui::Context,
        frame: &DecodedFrame,
        now: f64,
    ) -> bool {
        let previous_rev_pts = self
            .last_decoded_frame
            .and_then(|(pts, source)| (source == "rev").then_some(pts));
        self.last_decoded_frame = Some((frame.pts_seconds, "rev"));
        textures.update_playback_texture(
            ctx,
            frame.width as usize,
            frame.height as usize,
            &frame.rgba_data,
        );

        if let Some(ref mut rev) = self.reverse {
            rev.last_frame_time = Some(now);
            if let Some((_, _, tc)) = state.project.timeline.find_clip(rev.timeline_clip) {
                let expected_source_at_playhead =
                    tc.source_in + (state.project.playback.playhead - tc.timeline_start).max(0.0);
                let pts_offset = if let Some(offset) = rev.pts_offset {
                    offset
                } else {
                    let offset = frame.pts_seconds - expected_source_at_playhead;
                    rev.pts_offset = Some(offset);
                    offset
                };
                let mapped_source_pts = frame.pts_seconds - pts_offset;

                if mapped_source_pts >= tc.source_in && mapped_source_pts < tc.source_out {
                    let timeline_pos = tc.timeline_start + (mapped_source_pts - tc.source_in);
                    let boundary_threshold = reverse_boundary_threshold_s(&rev.clip.1);
                    let distance_to_clip_start = (mapped_source_pts - tc.source_in).max(0.0);
                    let should_apply = previous_rev_pts.is_none()
                        || timeline_pos <= state.project.playback.playhead;
                    if should_apply {
                        state.project.playback.playhead = timeline_pos;
                    }
                    if distance_to_clip_start <= boundary_threshold && previous_rev_pts.is_some() {
                        let from_timeline_clip = rev.timeline_clip;
                        let prev_time = (tc.timeline_start - 0.001).max(0.0);
                        state.project.playback.playhead = prev_time;
                        self.reverse = None;
                        self.reset_audio_sources();

                        if prev_time <= 0.0 {
                            state.project.playback.playhead = 0.0;
                            state.project.playback.state = PlaybackState::Stopped;
                        } else if let Some(prev_hit) = state
                            .project
                            .timeline
                            .previous_clip_before(from_timeline_clip)
                        {
                            let prev_timeline_clip_id = prev_hit.clip.id;
                            if self.promote_reverse_shadow(
                                state,
                                textures,
                                prev_timeline_clip_id,
                                now,
                                ctx,
                            ) {
                                self.try_activate_pipeline(now);
                            } else {
                                let prev_clip_id = prev_hit.clip.source_id;
                                if let Some(clip) = state.project.clips.get(&prev_clip_id) {
                                    let path = clip.path.clone();
                                    let speed = state.project.playback.speed;
                                    self.pending_reverse = Some(PendingReversePipeline::spawn(
                                        &path,
                                        prev_hit.source_time,
                                        workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
                                        workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
                                        speed,
                                        prev_clip_id,
                                        prev_timeline_clip_id,
                                        now,
                                    ));
                                    self.show_scrub_cache_bridge_frame(
                                        textures,
                                        prev_clip_id,
                                        prev_hit.source_time,
                                    );
                                }
                            }
                        }
                        return false;
                    }
                } else if mapped_source_pts < tc.source_in {
                    let from_timeline_clip = rev.timeline_clip;
                    let prev_time = (tc.timeline_start - 0.001).max(0.0);
                    state.project.playback.playhead = prev_time;
                    self.reverse = None;
                    self.reset_audio_sources();

                    if prev_time <= 0.0 {
                        state.project.playback.playhead = 0.0;
                        state.project.playback.state = PlaybackState::Stopped;
                    } else if let Some(prev_hit) = state
                        .project
                        .timeline
                        .previous_clip_before(from_timeline_clip)
                    {
                        let prev_timeline_clip_id = prev_hit.clip.id;
                        if self.promote_reverse_shadow(
                            state,
                            textures,
                            prev_timeline_clip_id,
                            now,
                            ctx,
                        ) {
                            self.try_activate_pipeline(now);
                        } else {
                            let prev_clip_id = prev_hit.clip.source_id;
                            if let Some(clip) = state.project.clips.get(&prev_clip_id) {
                                let path = clip.path.clone();
                                let speed = state.project.playback.speed;
                                self.pending_reverse = Some(PendingReversePipeline::spawn(
                                    &path,
                                    prev_hit.source_time,
                                    workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
                                    workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
                                    speed,
                                    prev_clip_id,
                                    prev_timeline_clip_id,
                                    now,
                                ));
                                self.show_scrub_cache_bridge_frame(
                                    textures,
                                    prev_clip_id,
                                    prev_hit.source_time,
                                );
                            }
                        }
                    }
                    return false;
                }
            }
        }

        self.update_video_fps(state, now);
        true
    }

    pub(super) fn pick_best_reverse_frame_for_playhead(
        &self,
        state: &AppState,
        frames: &[DecodedFrame],
    ) -> usize {
        let playhead = state.project.playback.playhead;
        let pts_offset = self.reverse.as_ref().and_then(|r| r.pts_offset);
        let mut best_idx = frames.len() - 1;
        for (i, frame) in frames.iter().enumerate() {
            let mapped = match pts_offset {
                Some(off) => frame.pts_seconds - off,
                None => frame.pts_seconds,
            };
            let timeline_pos = self
                .reverse
                .as_ref()
                .and_then(|rev| {
                    state
                        .project
                        .timeline
                        .find_clip(rev.timeline_clip)
                        .map(|(_, _, tc)| tc.timeline_start + (mapped - tc.source_in))
                })
                .unwrap_or(mapped);
            if timeline_pos >= playhead - 0.05 {
                best_idx = i;
            } else {
                break;
            }
        }
        best_idx
    }

    pub fn poll_pending_reverse_shadow(&mut self, _now: f64) {
        let pending = match self.pending_reverse_shadow.as_ref() {
            Some(p) => p,
            None => return,
        };

        let result = match pending.try_recv() {
            Some(r) => r,
            None => return,
        };

        let pending = self.pending_reverse_shadow.take().expect("checked above");

        if let Ok(handle) = result {
            self.reverse_shadow = Some(ReverseShadowState {
                handle,
                clip: pending.clip,
                timeline_clip: pending.timeline_clip,
                first_frame_ready: false,
                buffered_frame: None,
            });
        }
    }

    pub fn poll_reverse_shadow_frame(&mut self) {
        if let Some(ref mut shadow) = self.reverse_shadow {
            if shadow.first_frame_ready {
                return;
            }
            if let Some(frame) = shadow.handle.try_recv_frame() {
                shadow.first_frame_ready = true;
                shadow.buffered_frame = Some(frame);
            }
        }
    }

    pub fn manage_reverse_shadow_pipeline(&mut self, state: &mut AppState, now: f64) {
        self.poll_pending_reverse_shadow(now);

        if state.project.playback.state != PlaybackState::PlayingReverse {
            return;
        }
        let rev = match self.reverse.as_ref() {
            Some(r) => r,
            None => return,
        };
        let current_timeline_clip = rev.timeline_clip;

        if let Some(ref shadow) = self.reverse_shadow {
            if let Some(prev_hit) = state
                .project
                .timeline
                .previous_clip_before(current_timeline_clip)
            {
                if shadow.timeline_clip == prev_hit.clip.id {
                    return;
                }
            }
        }
        if let Some(ref shadow) = self.pending_reverse_shadow {
            if let Some(prev_hit) = state
                .project
                .timeline
                .previous_clip_before(current_timeline_clip)
            {
                if shadow.timeline_clip == prev_hit.clip.id {
                    return;
                }
            }
        }

        let (_, _, tc) = match state.project.timeline.find_clip(current_timeline_clip) {
            Some(found) => found,
            None => return,
        };
        let time_elapsed = (state.project.playback.playhead - tc.timeline_start).max(0.0);

        let speed = state.project.playback.speed;
        if time_elapsed > SHADOW_LOOKAHEAD_S / speed {
            return;
        }

        let Some(prev_hit) = state
            .project
            .timeline
            .previous_clip_before(current_timeline_clip)
        else {
            return;
        };

        let prev_clip_id = prev_hit.clip.source_id;
        let prev_timeline_clip_id = prev_hit.clip.id;
        let Some(clip) = state.project.clips.get(&prev_clip_id) else {
            return;
        };
        let path = clip.path.clone();

        self.reverse_shadow = None;
        self.pending_reverse_shadow = None;

        self.pending_reverse_shadow = Some(PendingReversePipeline::spawn(
            &path,
            prev_hit.source_time,
            workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
            workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
            speed,
            prev_clip_id,
            prev_timeline_clip_id,
            now,
        ));
    }

    pub fn manage_reverse_shadow_for_stopped(&mut self, state: &mut AppState, now: f64) {
        let playhead = state.project.playback.playhead;

        let target_hit = state.project.timeline.video_clip_at_time(playhead);

        let Some(hit) = target_hit else {
            return;
        };

        if self
            .reverse_shadow
            .as_ref()
            .is_some_and(|s| s.timeline_clip == hit.clip.id)
        {
            return;
        }
        if self
            .pending_reverse_shadow
            .as_ref()
            .is_some_and(|s| s.timeline_clip == hit.clip.id)
        {
            return;
        }

        let clip_id = hit.clip.source_id;
        let timeline_clip_id = hit.clip.id;
        let Some(clip) = state.project.clips.get(&clip_id) else {
            return;
        };
        let path = clip.path.clone();
        let speed = state.project.playback.speed;

        self.reverse_shadow = None;
        self.pending_reverse_shadow = None;

        self.pending_reverse_shadow = Some(PendingReversePipeline::spawn(
            &path,
            hit.source_time,
            workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
            workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
            speed,
            clip_id,
            timeline_clip_id,
            now,
        ));
    }

    pub fn promote_reverse_shadow(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        timeline_clip_id: TimelineClipId,
        now: f64,
        ctx: &egui::Context,
    ) -> bool {
        let shadow_matches = self
            .reverse_shadow
            .as_ref()
            .is_some_and(|s| s.timeline_clip == timeline_clip_id);

        if !shadow_matches {
            return false;
        }

        let shadow = self.reverse_shadow.take().expect("checked above");

        let mut rev = ReversePipelineState {
            handle: shadow.handle,
            clip: shadow.clip,
            timeline_clip: shadow.timeline_clip,
            pts_offset: None,
            speed: state.project.playback.speed,
            activated: shadow.buffered_frame.is_some(),
            started_at: now,
            last_frame_time: if shadow.buffered_frame.is_some() {
                Some(now)
            } else {
                None
            },
        };

        if let Some(frame) = shadow.buffered_frame {
            textures.update_playback_texture(
                ctx,
                frame.width as usize,
                frame.height as usize,
                &frame.rgba_data,
            );
            self.last_decoded_frame = Some((frame.pts_seconds, "rev"));
            rev.last_frame_time = Some(now);
        }

        self.reverse = Some(rev);
        true
    }
}

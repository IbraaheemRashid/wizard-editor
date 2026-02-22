use std::path::{Path, PathBuf};

use wizard_media::pipeline::{
    AudioOnlyHandle, DecodedFrame, PipelineHandle, ReversePipelineHandle,
};
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::timeline::TimelineClipId;

use crate::constants::*;
use crate::workers;
use crate::EditorApp;

pub struct ForwardPipelineState {
    pub handle: PipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub pts_offset: Option<f64>,
    pub speed: f64,
    pub frame_delivered: bool,
    pub started_at: f64,
    pub last_frame_time: Option<f64>,
    pub audio_only_handle: Option<AudioOnlyHandle>,
}

pub struct ShadowPipelineState {
    pub handle: PipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
}

pub struct ReversePipelineState {
    pub handle: ReversePipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub pts_offset: Option<f64>,
    pub speed: f64,
}

impl EditorApp {
    pub fn manage_shadow_pipeline(&mut self, _now: f64) {
        if self.state.project.playback.state != PlaybackState::Playing {
            return;
        }
        let fwd = match self.forward.as_ref() {
            Some(f) => f,
            None => return,
        };
        let current_timeline_clip = fwd.timeline_clip;

        if let Some(ref shadow) = self.shadow {
            if let Some(next_hit) = self
                .state
                .project
                .timeline
                .next_clip_after(current_timeline_clip)
            {
                if shadow.timeline_clip == next_hit.clip.id {
                    return;
                }
            }
        }

        let time_remaining = self
            .state
            .project
            .timeline
            .time_remaining_in_clip(current_timeline_clip, self.state.project.playback.playhead);
        let Some(remaining) = time_remaining else {
            return;
        };

        let speed = self.state.project.playback.speed;
        if remaining > SHADOW_LOOKAHEAD_S / speed {
            return;
        }

        let Some(next_hit) = self
            .state
            .project
            .timeline
            .next_clip_after(current_timeline_clip)
        else {
            return;
        };

        let next_clip_id = next_hit.clip.source_id;
        let next_timeline_clip_id = next_hit.clip.id;
        let Some(clip) = self.state.project.clips.get(&next_clip_id) else {
            return;
        };
        let path = clip.path.clone();

        self.shadow = None;

        match PipelineHandle::start(
            &path,
            next_hit.source_time,
            workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
            workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
            None,
            self.audio_sample_rate,
            self.audio_channels,
            speed,
        ) {
            Ok(handle) => {
                self.shadow = Some(ShadowPipelineState {
                    handle,
                    clip: (next_clip_id, path),
                    timeline_clip: next_timeline_clip_id,
                });
            }
            Err(e) => {
                eprintln!("Failed to start shadow pipeline: {e}");
            }
        }
    }

    pub fn promote_shadow_pipeline(
        &mut self,
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
        shadow.handle.reset_clock();

        self.reset_audio_queue();

        let next_clip_id = next_hit.clip.source_id;
        let mut fwd = ForwardPipelineState {
            handle: shadow.handle,
            clip: shadow.clip,
            timeline_clip: shadow.timeline_clip,
            pts_offset: None,
            speed: self.state.project.playback.speed,
            frame_delivered: false,
            started_at: now,
            last_frame_time: None,
            audio_only_handle: None,
        };

        self.state.project.playback.playhead = next_time;

        while let Some(frame) = fwd.handle.try_recv_frame() {
            let texture = ctx.load_texture(
                "playback_frame",
                egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba_data,
                ),
                egui::TextureOptions::LINEAR,
            );
            self.textures.playback_texture = Some(texture);
            self.last_decoded_frame = Some((frame.pts_seconds, "fwd"));
            fwd.frame_delivered = true;
            fwd.last_frame_time = Some(now);
        }

        self.forward = Some(fwd);

        if let Some(clip) = self.state.project.clips.get(&next_clip_id) {
            let path = clip.path.clone();
            if !self.path_has_no_audio(&path) {
                let speed = self.state.project.playback.speed;
                match AudioOnlyHandle::start(
                    &path,
                    next_hit.source_time,
                    self.audio_producer.clone(),
                    self.audio_sample_rate,
                    self.audio_channels,
                    speed,
                ) {
                    Ok(handle) => {
                        if let Some(ref mut f) = self.forward {
                            f.audio_only_handle = Some(handle);
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to start audio-only handle: {e}");
                    }
                }
            }
        }

        true
    }

    pub fn manage_pipeline(&mut self, now: f64, _ctx: &egui::Context) {
        let is_forward = self.state.project.playback.state == PlaybackState::Playing;
        let is_reverse = self.state.project.playback.state == PlaybackState::PlayingReverse;
        let is_playing = self.is_playing();
        let is_scrubbing = self.state.ui.timeline.scrubbing.is_some();

        if !is_forward && self.forward.is_some() {
            self.forward = None;
            self.shadow = None;
        }

        if !is_reverse && self.reverse.is_some() {
            self.reverse = None;
        }

        if !is_playing {
            self.was_scrubbing = is_scrubbing;
            return;
        }

        let playhead = if let Some(scrub_time) = self.state.ui.timeline.scrubbing {
            scrub_time
        } else {
            self.state.project.playback.playhead
        };

        let hit = self.state.project.timeline.clip_at_time(playhead);
        let Some(hit) = hit else {
            let had_pipeline = self.forward.is_some() || self.reverse.is_some();
            self.forward = None;
            self.reverse = None;
            self.shadow = None;
            if had_pipeline {
                self.reset_audio_queue();
            }
            self.was_scrubbing = is_scrubbing;
            return;
        };

        let timeline_clip_id = hit.clip.id;
        let clip_id = hit.clip.source_id;
        let Some(clip) = self.state.project.clips.get(&clip_id) else {
            self.was_scrubbing = is_scrubbing;
            return;
        };
        let path = clip.path.clone();

        let scrub_just_released = self.was_scrubbing && !is_scrubbing;

        if is_scrubbing {
            self.forward = None;
            self.reverse = None;
            self.shadow = None;
            self.reset_audio_queue();
            self.was_scrubbing = true;
            return;
        }

        if is_forward {
            let speed = self.state.project.playback.speed;
            let fwd_speed = self.forward.as_ref().map(|f| f.speed).unwrap_or(speed);
            let speed_changed = self.forward.is_some() && (speed - fwd_speed).abs() > 0.01;

            let needs_new_pipeline = match self.forward.as_ref() {
                None => true,
                Some(fwd) => {
                    fwd.timeline_clip != timeline_clip_id
                        || fwd.clip.0 != clip_id
                        || fwd.clip.1 != path
                }
            };

            if speed_changed && !needs_new_pipeline {
                if let Some(ref fwd) = self.forward {
                    fwd.handle.update_speed(speed);
                }
                self.forward = None;
                self.shadow = None;
                self.reset_audio_queue();
                self.start_pipeline(timeline_clip_id, clip_id, &path, hit.source_time, now);
                if let Some(ref mut fwd) = self.forward {
                    fwd.speed = speed;
                }
            }

            let last_frame_time = self.last_pipeline_frame_time();
            let has_stale_pipeline =
                last_frame_time.is_some_and(|t| (now - t) > STALE_PIPELINE_THRESHOLD_S);
            if self.forward.is_some() && has_stale_pipeline && !needs_new_pipeline {
                self.forward = None;
                self.shadow = None;
                self.reset_audio_queue();
                self.start_pipeline(timeline_clip_id, clip_id, &path, hit.source_time, now);
                self.was_scrubbing = is_scrubbing;
                return;
            }

            if scrub_just_released || needs_new_pipeline {
                self.forward = None;
                self.shadow = None;
                if scrub_just_released {
                    self.reset_audio_queue();
                }
                self.start_pipeline(timeline_clip_id, clip_id, &path, hit.source_time, now);
                if scrub_just_released {
                    self.state.project.playback.playhead = playhead;
                }
            }
        }

        if is_reverse {
            self.manage_reverse_pipeline(timeline_clip_id, clip_id, &path, hit.source_time);
        }

        self.was_scrubbing = is_scrubbing;
    }

    fn manage_reverse_pipeline(
        &mut self,
        timeline_clip_id: TimelineClipId,
        clip_id: ClipId,
        path: &Path,
        source_time: f64,
    ) {
        let speed = self.state.project.playback.speed;

        if let Some(ref rev) = self.reverse {
            if speed != rev.speed {
                rev.handle.update_speed(speed);
            }
        }

        let needs_new = if let Some(ref rev) = self.reverse {
            rev.timeline_clip != timeline_clip_id || rev.clip.0 != clip_id || rev.clip.1 != *path
        } else {
            true
        };

        if needs_new {
            self.reverse = None;
            match ReversePipelineHandle::start(
                path,
                source_time,
                speed,
                workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
                workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
            ) {
                Ok(handle) => {
                    self.reverse = Some(ReversePipelineState {
                        handle,
                        clip: (clip_id, path.to_path_buf()),
                        timeline_clip: timeline_clip_id,
                        pts_offset: None,
                        speed,
                    });
                }
                Err(e) => {
                    eprintln!("Failed to start reverse pipeline: {e}");
                }
            }
        }
    }

    pub fn start_pipeline(
        &mut self,
        timeline_clip_id: TimelineClipId,
        clip_id: ClipId,
        path: &Path,
        source_time: f64,
        now: f64,
    ) {
        self.reset_audio_queue();

        let speed = self.state.project.playback.speed;
        let has_audio = !self.path_has_no_audio(path);
        let audio_prod = if has_audio {
            Some(self.audio_producer.clone())
        } else {
            None
        };

        match PipelineHandle::start(
            path,
            source_time,
            workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
            workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
            audio_prod,
            self.audio_sample_rate,
            self.audio_channels,
            speed,
        ) {
            Ok(handle) => {
                self.forward = Some(ForwardPipelineState {
                    handle,
                    clip: (clip_id, path.to_path_buf()),
                    timeline_clip: timeline_clip_id,
                    pts_offset: None,
                    speed,
                    frame_delivered: false,
                    started_at: now,
                    last_frame_time: None,
                    audio_only_handle: None,
                });
            }
            Err(e) => {
                eprintln!("Failed to start pipeline: {e}");
            }
        }
    }

    pub fn apply_pipeline_frame(
        &mut self,
        ctx: &egui::Context,
        frame: &DecodedFrame,
        now: f64,
    ) -> bool {
        self.last_decoded_frame = Some((frame.pts_seconds, "fwd"));
        let texture = ctx.load_texture(
            "playback_frame",
            egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba_data,
            ),
            egui::TextureOptions::LINEAR,
        );
        self.textures.playback_texture = Some(texture);

        let active_clip = self.forward.as_ref().and_then(|fwd| {
            self.state
                .project
                .timeline
                .find_clip(fwd.timeline_clip)
                .map(|(_, _, tc)| (tc.timeline_start, tc.duration, tc.source_in, tc.source_out))
        });
        if let Some((timeline_start, duration, source_in, source_out)) = active_clip {
            let fwd = self.forward.as_mut().expect("forward checked above");
            let expected_source_at_playhead =
                source_in + (self.state.project.playback.playhead - timeline_start).max(0.0);
            let pts_offset = if let Some(offset) = fwd.pts_offset {
                offset
            } else {
                let offset = frame.pts_seconds - expected_source_at_playhead;
                fwd.pts_offset = Some(offset);
                offset
            };
            let mapped_source_pts = frame.pts_seconds - pts_offset;

            if mapped_source_pts >= source_out {
                let next_time = timeline_start + duration;
                self.state.project.playback.playhead = next_time;
                self.forward = None;

                if let Some(next_hit) = self.state.project.timeline.clip_at_time(next_time) {
                    let next_hit_clone = next_hit.clone();
                    if self.promote_shadow_pipeline(next_time, &next_hit_clone, now, ctx) {
                        return false;
                    }

                    let next_clip_id = next_hit.clip.source_id;
                    let next_timeline_clip_id = next_hit.clip.id;
                    if let Some(clip) = self.state.project.clips.get(&next_clip_id) {
                        let path = clip.path.clone();
                        self.start_pipeline(
                            next_timeline_clip_id,
                            next_clip_id,
                            &path,
                            next_hit.source_time,
                            now,
                        );
                    }
                } else {
                    self.reset_audio_queue();
                }
                return false;
            }

            if mapped_source_pts >= source_in && mapped_source_pts < source_out {
                let new_playhead = timeline_start + (mapped_source_pts - source_in);
                if new_playhead >= self.state.project.playback.playhead || !fwd.frame_delivered {
                    self.state.project.playback.playhead = new_playhead;
                }
                fwd.frame_delivered = true;
            }
        } else if let Some(timeline_pos) = self.find_timeline_hit_for_source_pts(frame.pts_seconds)
        {
            if let Some(ref mut fwd) = self.forward {
                fwd.frame_delivered = true;
            }
            self.state.project.playback.playhead = timeline_pos;
        }
        if let Some(ref mut fwd) = self.forward {
            fwd.last_frame_time = Some(now);
        }

        self.update_video_fps(now);
        true
    }

    pub fn apply_reverse_pipeline_frame(
        &mut self,
        ctx: &egui::Context,
        frame: &DecodedFrame,
        now: f64,
    ) -> bool {
        self.last_decoded_frame = Some((frame.pts_seconds, "rev"));
        let texture = ctx.load_texture(
            "playback_frame",
            egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba_data,
            ),
            egui::TextureOptions::LINEAR,
        );
        self.textures.playback_texture = Some(texture);

        if let Some(ref mut rev) = self.reverse {
            if let Some((_, _, tc)) = self.state.project.timeline.find_clip(rev.timeline_clip) {
                let expected_source_at_playhead = tc.source_in
                    + (self.state.project.playback.playhead - tc.timeline_start).max(0.0);
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
                    self.state.project.playback.playhead = timeline_pos;
                } else if mapped_source_pts < tc.source_in {
                    let prev_time = (tc.timeline_start - 0.001).max(0.0);
                    self.state.project.playback.playhead = prev_time;
                    self.reverse = None;
                    self.reset_audio_queue();

                    if prev_time > 0.0 {
                        if let Some(prev_hit) = self.state.project.timeline.clip_at_time(prev_time)
                        {
                            let prev_clip_id = prev_hit.clip.source_id;
                            let prev_timeline_clip_id = prev_hit.clip.id;
                            if let Some(clip) = self.state.project.clips.get(&prev_clip_id) {
                                let path = clip.path.clone();
                                let speed = self.state.project.playback.speed;
                                if let Ok(handle) = ReversePipelineHandle::start(
                                    &path,
                                    prev_hit.source_time,
                                    speed,
                                    workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
                                    workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
                                ) {
                                    self.reverse = Some(ReversePipelineState {
                                        handle,
                                        clip: (prev_clip_id, path),
                                        timeline_clip: prev_timeline_clip_id,
                                        pts_offset: None,
                                        speed,
                                    });
                                }
                            }
                        }
                    }
                    return false;
                }
            }
        }

        self.update_video_fps(now);
        true
    }

    pub fn find_timeline_hit_for_source_pts(&self, pts: f64) -> Option<f64> {
        let fwd = self.forward.as_ref()?;
        let (_, _, tc) = self.state.project.timeline.find_clip(fwd.timeline_clip)?;
        if pts >= tc.source_in && pts < tc.source_out {
            return Some(tc.timeline_start + (pts - tc.source_in));
        }
        None
    }

    pub fn last_pipeline_frame_time(&self) -> Option<f64> {
        self.forward.as_ref().and_then(|f| f.last_frame_time)
    }

    pub fn pipeline_frame_delivered(&self) -> bool {
        self.forward
            .as_ref()
            .map(|f| f.frame_delivered)
            .unwrap_or(false)
    }

    fn update_video_fps(&mut self, now: f64) {
        if self.video_fps_window_start.is_none() {
            self.video_fps_window_start = Some(now);
            self.video_fps_window_frames = 0;
        }
        self.video_fps_window_frames += 1;

        if let Some(start) = self.video_fps_window_start {
            let elapsed = (now - start).max(0.0);
            if elapsed >= FPS_WINDOW_S {
                let inst_video_fps = (self.video_fps_window_frames as f64 / elapsed) as f32;
                if self.state.ui.debug.video_fps <= 0.0 {
                    self.state.ui.debug.video_fps = inst_video_fps;
                } else {
                    self.state.ui.debug.video_fps =
                        self.state.ui.debug.video_fps * 0.8 + inst_video_fps * 0.2;
                }
                self.video_fps_window_start = Some(now);
                self.video_fps_window_frames = 0;
            }
        }
    }
}

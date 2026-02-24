use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wizard_audio::output::{AudioOutput, AudioProducer};
use wizard_media::gst_pipeline::{GstAudioOnlyHandle, GstReversePipelineHandle};
use wizard_media::pipeline::DecodedFrame;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;
use wizard_state::timeline::TimelineClipId;

use crate::audio_mixer::AudioMixer;
use crate::constants::*;
use crate::pipeline::{
    ForwardPipelineState, PendingPipeline, PendingShadowPipeline, PipelineStatus,
    ReversePipelineState, ShadowAudioSourceRequest, ShadowPipelineState,
};
use crate::texture_cache::TextureCache;
use crate::workers;
use crate::workers::audio_worker::{AudioPreviewRequest, AudioWorkerChannels};
use crate::workers::video_decode_worker::{VideoDecodeRequest, VideoDecodeWorkerChannels};

pub struct PlaybackEngine {
    pub forward: Option<ForwardPipelineState>,
    pub pending_forward: Option<PendingPipeline>,
    pub shadow: Option<ShadowPipelineState>,
    pub pending_shadow: Option<PendingShadowPipeline>,
    pub reverse: Option<ReversePipelineState>,

    pub audio_output: Option<AudioOutput>,
    pub audio_producer: Arc<Mutex<AudioProducer>>,
    pub mixer: AudioMixer,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
    pub no_audio_paths: Arc<Mutex<HashSet<PathBuf>>>,

    pub video_decode: VideoDecodeWorkerChannels,
    pub audio: AudioWorkerChannels,

    pub last_video_decode_request: Option<(ClipId, i64)>,
    pub last_hover_audio_request: Option<(ClipId, i64)>,
    pub last_scrub_audio_request: Option<(ClipId, i64)>,
    pub was_scrubbing: bool,
    pub last_is_playing: bool,
    pub last_playback_state: PlaybackState,
    pub last_decoded_frame: Option<(f64, &'static str)>,
    pub last_playhead_observed: f64,
    pub video_fps_window_start: Option<f64>,
    pub video_fps_window_frames: u32,
    pub runtime_log_frames: u32,
}

impl PlaybackEngine {
    pub fn new(
        audio_output: Option<AudioOutput>,
        audio_producer: Arc<Mutex<AudioProducer>>,
        audio_sample_rate: u32,
        audio_channels: u16,
        no_audio_paths: Arc<Mutex<HashSet<PathBuf>>>,
    ) -> Self {
        let mixer = AudioMixer::new(audio_producer.clone());
        let video_decode = workers::video_decode_worker::spawn_video_decode_worker();
        let audio = workers::audio_worker::spawn_audio_worker(no_audio_paths.clone());

        Self {
            forward: None,
            pending_forward: None,
            shadow: None,
            pending_shadow: None,
            reverse: None,
            audio_output,
            audio_producer,
            mixer,
            audio_sample_rate,
            audio_channels,
            no_audio_paths,
            video_decode,
            audio,
            last_video_decode_request: None,
            last_hover_audio_request: None,
            last_scrub_audio_request: None,
            was_scrubbing: false,
            last_is_playing: false,
            last_playback_state: PlaybackState::Stopped,
            last_decoded_frame: None,
            last_playhead_observed: 0.0,
            video_fps_window_start: None,
            video_fps_window_frames: 0,
            runtime_log_frames: 0,
        }
    }

    pub fn is_playing(&self, state: &AppState) -> bool {
        matches!(
            state.project.playback.state,
            PlaybackState::Playing | PlaybackState::PlayingReverse
        )
    }

    pub fn path_has_no_audio(&self, path: &Path) -> bool {
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

    pub fn last_pipeline_frame_time(&self) -> Option<f64> {
        self.forward.as_ref().and_then(|f| f.last_frame_time)
    }

    pub fn pipeline_frame_delivered(&self) -> bool {
        self.forward
            .as_ref()
            .map(|f| f.frame_delivered)
            .unwrap_or(false)
    }

    pub fn handle_playback_stop_transition(&mut self) {
        let _ = self.audio.req_tx.send(AudioPreviewRequest::Stop);
        self.last_hover_audio_request = None;
        self.last_scrub_audio_request = None;
        self.last_video_decode_request = None;
        self.last_decoded_frame = None;
        self.shadow = None;
        self.pending_shadow = None;
        self.pending_forward = None;
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
            self.pending_forward = None;
            self.reverse = None;
            self.shadow = None;
            self.pending_shadow = None;
            self.handle_playback_stop_transition();
        }
    }

    pub fn poll_shadow_frame(&mut self) {
        if let Some(ref mut shadow) = self.shadow {
            if shadow.first_frame_ready {
                return;
            }
            if let Some(frame) = shadow.handle.try_recv_frame() {
                shadow.first_frame_ready = true;
                shadow.buffered_frame = Some(frame);
            }
        }
    }

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

        eprintln!(
            "[SHADOW] spawn queued clip={next_timeline_clip_id:?} audio_sources={}",
            audio_requests.len()
        );
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

    pub fn poll_pending_shadow_pipeline(&mut self, now: f64) {
        let pending = match self.pending_shadow.as_ref() {
            Some(p) => p,
            None => return,
        };

        let result = match pending.try_recv() {
            Some(r) => r,
            None => return,
        };

        let pending = self.pending_shadow.take().expect("checked above");
        let wait_ms = (now - pending.started_at) * 1000.0;

        match result {
            Ok(build) => {
                eprintln!(
                    "[SHADOW] pipeline arrived after {wait_ms:.1}ms, audio_sources={}",
                    build.audio_sources.len()
                );
                self.shadow = Some(ShadowPipelineState {
                    handle: build.handle,
                    clip: pending.clip,
                    timeline_clip: pending.timeline_clip,
                    first_frame_ready: false,
                    buffered_frame: None,
                    audio_sources: build.audio_sources,
                });
            }
            Err(e) => {
                eprintln!("Failed to start shadow pipeline: {e}");
            }
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

    pub fn try_activate_pipeline(&mut self) {
        if let Some(ref mut fwd) = self.forward {
            if !fwd.activated && fwd.handle.is_first_frame_ready() {
                fwd.activated = true;
                eprintln!("[ACTIVATE] forward pipeline -> Playing");
                let _ = fwd.handle.begin_playing();
            }
        }
        if let Some(ref mut rev) = self.reverse {
            if !rev.activated && rev.handle.is_first_frame_ready() {
                rev.activated = true;
                let _ = rev.handle.begin_playing();
            }
        }
    }

    pub fn manage_pipeline(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        now: f64,
        _ctx: &egui::Context,
    ) {
        self.try_activate_pipeline();

        let is_forward = state.project.playback.state == PlaybackState::Playing;
        let is_reverse = state.project.playback.state == PlaybackState::PlayingReverse;
        let is_playing = self.is_playing(state);
        let is_scrubbing = state.ui.timeline.scrubbing.is_some();

        if !is_forward && self.forward.is_some() {
            self.forward = None;
            self.shadow = None;
            self.pending_shadow = None;
        }

        if !is_reverse && self.reverse.is_some() {
            self.reverse = None;
        }

        if !is_playing {
            self.was_scrubbing = is_scrubbing;
            return;
        }

        let playhead = if let Some(scrub_time) = state.ui.timeline.scrubbing {
            scrub_time
        } else {
            state.project.playback.playhead
        };

        let hit = state.project.timeline.video_clip_at_time(playhead);
        let Some(hit) = hit else {
            let had_pipeline = self.forward.is_some() || self.reverse.is_some();
            self.forward = None;
            self.pending_forward = None;
            self.reverse = None;
            self.shadow = None;
            self.pending_shadow = None;
            textures.playback_texture = None;
            self.last_decoded_frame = None;
            if had_pipeline {
                self.reset_audio_sources();
            }
            if is_forward && !is_scrubbing {
                let has_audio = state.project.timeline.has_unmuted_audio_at_time(playhead);
                if has_audio && self.mixer.source_count() == 0 {
                    self.start_audio_sources(state);
                } else if !has_audio && self.mixer.source_count() > 0 {
                    self.mixer.clear();
                    self.reset_audio_sources();
                }
            }
            self.was_scrubbing = is_scrubbing;
            return;
        };

        let timeline_clip_id = hit.clip.id;
        let clip_id = hit.clip.source_id;
        let Some(clip) = state.project.clips.get(&clip_id) else {
            self.was_scrubbing = is_scrubbing;
            return;
        };
        let path = clip.path.clone();

        let scrub_just_released = self.was_scrubbing && !is_scrubbing;

        if is_scrubbing {
            self.forward = None;
            self.pending_forward = None;
            self.reverse = None;
            self.shadow = None;
            self.pending_shadow = None;
            self.reset_audio_sources();
            self.was_scrubbing = true;
            return;
        }

        if is_forward {
            if let Some(ref mut fwd) = self.forward {
                fwd.age = fwd.age.saturating_add(1);
            }

            let speed = state.project.playback.speed;
            let fwd_speed = self.forward.as_ref().map(|f| f.speed).unwrap_or(speed);
            let speed_changed = self.forward.is_some() && (speed - fwd_speed).abs() > 0.01;

            let pending_matches = self
                .pending_forward
                .as_ref()
                .is_some_and(|p| p.timeline_clip == timeline_clip_id && p.clip.0 == clip_id);

            let needs_new_pipeline = match self.forward.as_ref() {
                None => !pending_matches,
                Some(fwd) => {
                    fwd.age >= 2
                        && (fwd.timeline_clip != timeline_clip_id
                            || fwd.clip.0 != clip_id
                            || fwd.clip.1 != path)
                }
            };

            if speed_changed && !needs_new_pipeline {
                if let Some(ref mut fwd) = self.forward {
                    fwd.handle.update_speed(speed);
                    fwd.speed = speed;
                }
                self.reset_audio_sources();
                self.start_audio_sources(state);
            }

            let last_frame_time = self.last_pipeline_frame_time();
            let has_stale_pipeline =
                last_frame_time.is_some_and(|t| (now - t) > STALE_PIPELINE_THRESHOLD_S);
            if self.forward.is_some() && has_stale_pipeline && !needs_new_pipeline {
                eprintln!(
                    "[MANAGE] STALE pipeline restart! last_frame_time={:?} now={now:.3} gap={:.3}s playhead={:.3}",
                    last_frame_time,
                    last_frame_time.map(|t| now - t).unwrap_or(0.0),
                    state.project.playback.playhead
                );
                self.forward = None;
                self.pending_forward = None;
                self.shadow = None;
                self.pending_shadow = None;
                self.reset_audio_sources();
                self.start_pipeline(
                    state,
                    textures,
                    timeline_clip_id,
                    clip_id,
                    &path,
                    hit.source_time,
                    now,
                );
                self.was_scrubbing = is_scrubbing;
                return;
            }

            if scrub_just_released || needs_new_pipeline {
                eprintln!(
                    "[MANAGE] pipeline restart: scrub_released={scrub_just_released} needs_new={needs_new_pipeline} playhead={:.3}",
                    state.project.playback.playhead
                );
                self.forward = None;
                self.pending_forward = None;
                self.shadow = None;
                self.pending_shadow = None;
                if scrub_just_released {
                    self.reset_audio_sources();
                }
                self.start_pipeline(
                    state,
                    textures,
                    timeline_clip_id,
                    clip_id,
                    &path,
                    hit.source_time,
                    now,
                );
                if scrub_just_released {
                    state.project.playback.playhead = playhead;
                }
            }
        }

        if is_reverse {
            self.manage_reverse_pipeline(
                state,
                textures,
                timeline_clip_id,
                clip_id,
                &path,
                hit.source_time,
                now,
            );
        }

        self.was_scrubbing = is_scrubbing;
    }

    #[allow(clippy::too_many_arguments)]
    fn manage_reverse_pipeline(
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
            match GstReversePipelineHandle::start(
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
                        activated: false,
                        started_at: now,
                        last_frame_time: None,
                    });
                    for _ in 0..50 {
                        if self
                            .reverse
                            .as_ref()
                            .is_some_and(|r| r.handle.is_first_frame_ready())
                        {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_micros(100));
                    }
                    self.try_activate_pipeline();
                    self.show_scrub_cache_bridge_frame(textures, clip_id, source_time);
                }
                Err(e) => {
                    eprintln!("Failed to start reverse pipeline: {e}");
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_pipeline(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        timeline_clip_id: TimelineClipId,
        clip_id: ClipId,
        path: &Path,
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
        let wait_ms = (now - pending.started_at) * 1000.0;

        match result {
            Ok(handle) => {
                let first_ready = handle.is_first_frame_ready();
                eprintln!(
                    "[PENDING] pipeline arrived after {wait_ms:.1}ms, first_frame_ready={first_ready}"
                );
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
                self.try_activate_pipeline();
            }
            Err(e) => {
                eprintln!("Failed to start pipeline: {e}");
            }
        }
    }

    pub fn start_audio_sources(&mut self, state: &AppState) {
        self.mixer.clear();

        let playhead = state.project.playback.playhead;
        let speed = state.project.playback.speed;
        let hits = state.project.timeline.audio_clips_at_time(playhead);

        for hit in hits {
            let Some(clip) = state.project.clips.get(&hit.clip.source_id) else {
                continue;
            };
            let path = clip.path.clone();
            if self.path_has_no_audio(&path) {
                continue;
            }

            let (producer, consumer) = AudioMixer::create_source_producer();
            let source_producer = Arc::new(Mutex::new(producer));

            match GstAudioOnlyHandle::start(
                &path,
                hit.source_time,
                source_producer,
                self.audio_sample_rate,
                self.audio_channels,
                speed,
            ) {
                Ok(handle) => {
                    let _ = handle.begin_playing();
                    self.mixer.add_source(handle, consumer);
                }
                Err(e) => {
                    eprintln!("Failed to start audio source: {e}");
                }
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
                let playhead_near_end = state.project.playback.playhead >= (timeline_start + duration - 0.016);
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
                    if previous_rev_pts.is_none() || timeline_pos <= state.project.playback.playhead
                    {
                        state.project.playback.playhead = timeline_pos;
                    }
                } else if mapped_source_pts < tc.source_in {
                    let prev_time = (tc.timeline_start - 0.001).max(0.0);
                    state.project.playback.playhead = prev_time;
                    self.reverse = None;
                    self.reset_audio_sources();

                    if prev_time > 0.0 {
                        if let Some(prev_hit) = state.project.timeline.video_clip_at_time(prev_time)
                        {
                            let prev_clip_id = prev_hit.clip.source_id;
                            let prev_timeline_clip_id = prev_hit.clip.id;
                            if let Some(clip) = state.project.clips.get(&prev_clip_id) {
                                let path = clip.path.clone();
                                let speed = state.project.playback.speed;
                                if let Ok(handle) = GstReversePipelineHandle::start(
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
                                        activated: false,
                                        started_at: now,
                                        last_frame_time: None,
                                    });
                                    self.show_scrub_cache_bridge_frame(
                                        textures,
                                        prev_clip_id,
                                        prev_hit.source_time,
                                    );
                                }
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

    fn show_scrub_cache_bridge_frame(
        &self,
        textures: &mut TextureCache,
        source_id: ClipId,
        source_time: f64,
    ) {
        if let Some(tex) = textures
            .scrub_frames
            .get(&source_id)
            .and_then(|entry| entry.frame_at_time(source_time))
        {
            textures.playback_texture = Some(tex.clone());
        }
    }

    pub fn find_timeline_hit_for_source_pts(&self, state: &AppState, pts: f64) -> Option<f64> {
        let fwd = self.forward.as_ref()?;
        let (_, _, tc) = state.project.timeline.find_clip(fwd.timeline_clip)?;
        if pts >= tc.source_in && pts < tc.source_out {
            return Some(tc.timeline_start + (pts - tc.source_in));
        }
        None
    }

    fn pick_best_frame_for_playhead(
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

    fn pick_best_reverse_frame_for_playhead(
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

    fn update_video_fps(&mut self, state: &mut AppState, now: f64) {
        if self.video_fps_window_start.is_none() {
            self.video_fps_window_start = Some(now);
            self.video_fps_window_frames = 0;
        }
        self.video_fps_window_frames += 1;

        if let Some(start) = self.video_fps_window_start {
            let elapsed = (now - start).max(0.0);
            if elapsed >= FPS_WINDOW_S {
                let inst_video_fps = (self.video_fps_window_frames as f64 / elapsed) as f32;
                if state.ui.debug.video_fps <= 0.0 {
                    state.ui.debug.video_fps = inst_video_fps;
                } else {
                    state.ui.debug.video_fps =
                        state.ui.debug.video_fps * 0.8 + inst_video_fps * 0.2;
                }
                self.video_fps_window_start = Some(now);
                self.video_fps_window_frames = 0;
            }
        }
    }

    pub fn poll_pipeline_frames(
        &mut self,
        state: &mut AppState,
        textures: &mut TextureCache,
        ctx: &egui::Context,
        now: f64,
    ) -> bool {
        let mut received = false;

        let fwd_status = self.forward.as_ref().map(|f| f.status(now));
        let fwd_frame_delivered = self.pipeline_frame_delivered();
        let rev_status = self.reverse.as_ref().map(|r| r.status(now));

        while let Ok(result) = self.video_decode.result_rx.try_recv() {
            let forward_pipeline_stalled = self.forward.is_some()
                && state.project.playback.state == PlaybackState::Playing
                && fwd_status.is_some_and(|s| s.is_stalled());
            let forward_long_stall = self.forward.is_some()
                && state.project.playback.state == PlaybackState::Playing
                && fwd_status == Some(PipelineStatus::LongStall);
            let reverse_pipeline_stalled = self.reverse.is_some()
                && state.project.playback.state == PlaybackState::PlayingReverse
                && rev_status.is_some_and(|s| s.is_stalled());
            let reverse_long_stall = self.reverse.is_some()
                && state.project.playback.state == PlaybackState::PlayingReverse
                && rev_status == Some(PipelineStatus::LongStall);
            let current_source = self.last_decoded_frame.map(|(_, s)| s);
            let forward_awaiting_first = self.forward.is_some()
                && state.project.playback.state == PlaybackState::Playing
                && !fwd_frame_delivered;
            let reverse_awaiting_first = self.reverse.is_some()
                && state.project.playback.state == PlaybackState::PlayingReverse
                && self
                    .reverse
                    .as_ref()
                    .and_then(|r| r.last_frame_time)
                    .is_none();
            let should_apply_fallback_texture = (state.project.playback.state
                == PlaybackState::Playing
                && (self.forward.is_none() || forward_pipeline_stalled))
                || (state.project.playback.state == PlaybackState::PlayingReverse
                    && (self.reverse.is_none() || reverse_pipeline_stalled))
                || state.project.playback.state == PlaybackState::Stopped
                || state.ui.timeline.scrubbing.is_some();
            let should_preserve_pipeline_texture =
                (current_source == Some("fwd") && !forward_long_stall && !forward_awaiting_first)
                    || (current_source == Some("rev")
                        && !reverse_long_stall
                        && !reverse_awaiting_first);
            if !should_apply_fallback_texture {
                continue;
            }
            if should_preserve_pipeline_texture {
                continue;
            }
            let img = result.image.as_ref();
            textures.update_playback_texture(
                ctx,
                img.width() as usize,
                img.height() as usize,
                img.as_raw(),
            );
            received = true;
        }

        self.poll_shadow_frame();

        let mut pipeline_frames: Vec<DecodedFrame> = Vec::new();
        if let Some(ref fwd) = self.forward {
            while let Some(frame) = fwd.handle.try_recv_frame() {
                pipeline_frames.push(frame);
            }
        }
        if !pipeline_frames.is_empty() {
            received = true;
            let best_idx = self.pick_best_frame_for_playhead(state, &pipeline_frames);
            let frame = &pipeline_frames[best_idx];
            self.runtime_log_frames += 1;
            self.apply_pipeline_frame(state, textures, ctx, frame, now);
        }
        if let Some(ref fwd) = self.forward {
            for mut frame in pipeline_frames {
                let buf = std::mem::take(&mut frame.rgba_data);
                if !buf.is_empty() {
                    fwd.handle.return_buffer(buf);
                }
            }
        }

        let mut reverse_frames: Vec<DecodedFrame> = Vec::new();
        if let Some(ref rev) = self.reverse {
            while let Some(frame) = rev.handle.try_recv_frame() {
                reverse_frames.push(frame);
            }
        }
        if !reverse_frames.is_empty() {
            received = true;
            let best_idx = self.pick_best_reverse_frame_for_playhead(state, &reverse_frames);
            let frame = &reverse_frames[best_idx];
            self.apply_reverse_pipeline_frame(state, textures, ctx, frame, now);
        }

        let mut last_snippet: Option<crate::workers::audio_worker::AudioSnippet> = None;
        while let Ok(snippet) = self.audio.snippet_rx.try_recv() {
            last_snippet = Some(snippet);
        }
        if let Some(snippet) = last_snippet {
            if self.is_playing(state) {
            } else {
                self.reset_audio_sources();
                if let Ok(mut producer) = self.audio_producer.lock() {
                    let ch = self.audio_channels;
                    wizard_audio::output::enqueue_samples(&mut producer, &snippet.samples_mono, ch);
                }
            }
        }

        self.mixer.mix_tick();

        received
    }

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
            && state.project.playback.state == PlaybackState::Playing
            && fwd_stall.is_some_and(|s| s.is_stalled());
        let reverse_pipeline_stalled = self.reverse.is_some()
            && state.project.playback.state == PlaybackState::PlayingReverse
            && rev_stall.is_some_and(|s| s.is_stalled());

        if (self.forward.is_some() && !forward_pipeline_stalled)
            || (self.reverse.is_some() && !reverse_pipeline_stalled)
        {
            let fwd_starting = fwd_stall == Some(PipelineStatus::StartingUp)
                && state.project.playback.state == PlaybackState::Playing;
            let rev_starting = rev_stall == Some(PipelineStatus::StartingUp)
                && state.project.playback.state == PlaybackState::PlayingReverse;
            if fwd_starting || rev_starting {
                return;
            }
            return;
        }

        let playhead = state.project.playback.playhead;
        let is_active = state.project.playback.state != PlaybackState::Stopped;
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

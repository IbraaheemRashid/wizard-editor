mod forward;
pub mod rewind_cache;
mod reverse;
mod scrub;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wizard_audio::output::{AudioOutput, AudioProducer};
use wizard_media::gst_pipeline::GstAudioOnlyHandle;
use wizard_media::pipeline::DecodedFrame;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

use crate::audio_mixer::AudioMixer;
use crate::constants::*;
use crate::pipeline::{
    ForwardPipelineState, PendingPipeline, PendingReversePipeline, PendingShadowPipeline,
    PipelineStatus, ReversePipelineState, ReverseShadowState, ShadowPipelineState,
};
use crate::texture_cache::TextureCache;
use crate::workers;
use crate::workers::audio_worker::{AudioPreviewRequest, AudioWorkerChannels};
use crate::workers::video_decode_worker::VideoDecodeWorkerChannels;
use rewind_cache::RewindCache;

pub struct PlaybackEngine {
    pub forward: Option<ForwardPipelineState>,
    pub pending_forward: Option<PendingPipeline>,
    pub shadow: Option<ShadowPipelineState>,
    pub pending_shadow: Option<PendingShadowPipeline>,
    pub reverse: Option<ReversePipelineState>,
    pub pending_reverse: Option<PendingReversePipeline>,
    pub reverse_shadow: Option<ReverseShadowState>,
    pub pending_reverse_shadow: Option<PendingReversePipeline>,

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
    pub rewind_cache: RewindCache,
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
            pending_reverse: None,
            reverse_shadow: None,
            pending_reverse_shadow: None,
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
            rewind_cache: RewindCache::new(),
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
        self.reverse_shadow = None;
        self.pending_reverse_shadow = None;
        self.pending_forward = None;
        self.pending_reverse = None;
        self.rewind_cache.clear();
        self.reset_audio_sources();
    }

    pub fn handle_playback_state_transition(
        &mut self,
        previous: PlaybackState,
        current: PlaybackState,
    ) {
        let forward_to_reverse = matches!(
            (previous, current),
            (PlaybackState::Playing, PlaybackState::PlayingReverse)
        );
        let reverse_to_forward = matches!(
            (previous, current),
            (PlaybackState::PlayingReverse, PlaybackState::Playing)
        );
        if forward_to_reverse || reverse_to_forward {
            self.forward = None;
            self.pending_forward = None;
            self.reverse = None;
            self.pending_reverse = None;
            self.shadow = None;
            self.pending_shadow = None;
            self.reverse_shadow = None;
            self.pending_reverse_shadow = None;
            if reverse_to_forward {
                self.rewind_cache.clear();
            }
            let _ = self.audio.req_tx.send(AudioPreviewRequest::Stop);
            self.last_hover_audio_request = None;
            self.last_scrub_audio_request = None;
            self.last_video_decode_request = None;
            self.last_decoded_frame = None;
            self.reset_audio_sources();
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

    pub fn try_activate_pipeline(&mut self, now: f64) {
        if let Some(ref mut fwd) = self.forward {
            if !fwd.activated && fwd.handle.is_first_frame_ready() {
                fwd.activated = true;
                let _ = fwd.handle.begin_playing();
            }
        }
        if let Some(ref mut rev) = self.reverse {
            if !rev.activated {
                let ready = rev.handle.is_first_frame_ready();
                let force_activate =
                    !ready && (now - rev.started_at) >= REVERSE_FORCE_PLAYING_AFTER_S;
                if ready || force_activate {
                    rev.activated = true;
                    let _ = rev.handle.begin_playing();
                }
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
        self.try_activate_pipeline(now);

        let is_forward = state.project.playback.state == PlaybackState::Playing;
        let is_reverse = state.project.playback.state == PlaybackState::PlayingReverse;
        let is_playing = self.is_playing(state);
        let is_scrubbing = state.ui.timeline.scrubbing.is_some();

        if !is_forward && self.forward.is_some() {
            self.forward = None;
            self.shadow = None;
            self.pending_shadow = None;
        }

        if !is_reverse
            && (self.reverse.is_some()
                || self.pending_reverse.is_some()
                || self.reverse_shadow.is_some()
                || self.pending_reverse_shadow.is_some())
        {
            self.reverse = None;
            self.pending_reverse = None;
            self.reverse_shadow = None;
            self.pending_reverse_shadow = None;
        }

        if !is_playing {
            self.poll_pending_shadow_pipeline(now);
            self.poll_shadow_frame();
            self.poll_pending_reverse_shadow(now);
            self.poll_reverse_shadow_frame();
            if !is_scrubbing {
                self.manage_shadow_for_stopped(state, now);
                self.manage_reverse_shadow_for_stopped(state, now);
            }
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
                self.manage_shadow_pipeline(state, now);
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
            self.rewind_cache.clear();
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
                let shadow_promoted = !scrub_just_released
                    && self.forward.is_none()
                    && self
                        .shadow
                        .as_ref()
                        .is_some_and(|s| s.timeline_clip == timeline_clip_id)
                    && self.promote_shadow_pipeline(
                        state, textures, playhead, &hit, now, _ctx,
                    );

                if !shadow_promoted {
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
        }

        if is_reverse {
            if self.reverse.is_none()
                && self.pending_reverse.is_none()
                && self.promote_reverse_shadow(
                    state,
                    textures,
                    timeline_clip_id,
                    now,
                    _ctx,
                )
            {
                self.try_activate_pipeline(now);
            } else {
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
        }

        self.was_scrubbing = is_scrubbing;
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

            if let Ok(handle) = GstAudioOnlyHandle::start(
                &path,
                hit.source_time,
                source_producer,
                self.audio_sample_rate,
                self.audio_channels,
                speed,
            ) {
                let _ = handle.begin_playing();
                self.mixer.add_source(handle, consumer);
            }
        }
    }

    pub(crate) fn show_scrub_cache_bridge_frame(
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
            let cache_is_active = state.project.playback.state == PlaybackState::PlayingReverse
                && !self.rewind_cache.is_empty();
            let should_preserve_pipeline_texture =
                (current_source == Some("fwd") && !forward_long_stall && !forward_awaiting_first)
                    || (current_source == Some("rev")
                        && !reverse_long_stall
                        && !reverse_awaiting_first)
                    || (current_source == Some("cache") && cache_is_active);
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

        let cache_active = state.project.playback.state == PlaybackState::PlayingReverse
            && !self.rewind_cache.is_empty();

        if cache_active {
            let mut pipeline_delivered = false;
            if let Some(ref rev) = self.reverse {
                while let Some(_) = rev.handle.try_recv_frame() {
                    pipeline_delivered = true;
                }
            }
            if pipeline_delivered {
                let playhead = state.project.playback.playhead;
                self.rewind_cache.clear();
                self.reverse = None;
                self.pending_reverse = None;

                if let Some(hit) = state.project.timeline.video_clip_at_time(playhead) {
                    let clip_id = hit.clip.source_id;
                    let timeline_clip_id = hit.clip.id;
                    if let Some(clip) = state.project.clips.get(&clip_id) {
                        let path = clip.path.clone();
                        let speed = state.project.playback.speed;
                        self.pending_reverse =
                            Some(crate::pipeline::PendingReversePipeline::spawn(
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
                }
            } else {
                let playhead = state.project.playback.playhead;
                self.rewind_cache.trim_above(playhead);
                if let Some(entry) = self.rewind_cache.best_entry_for_playhead(playhead) {
                    textures.update_playback_texture(
                        ctx,
                        entry.width as usize,
                        entry.height as usize,
                        &entry.rgba_data,
                    );
                    self.last_decoded_frame = Some((entry.source_pts, "cache"));
                    received = true;
                }
            }
        } else {
            let mut reverse_frames: Vec<DecodedFrame> = Vec::new();
            if let Some(ref rev) = self.reverse {
                while let Some(frame) = rev.handle.try_recv_frame() {
                    reverse_frames.push(frame);
                }
            }
            if !reverse_frames.is_empty() {
                received = true;
                let best_idx =
                    self.pick_best_reverse_frame_for_playhead(state, &reverse_frames);
                let frame = &reverse_frames[best_idx];
                self.apply_reverse_pipeline_frame(state, textures, ctx, frame, now);
            }
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
}

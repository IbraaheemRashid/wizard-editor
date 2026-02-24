use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use wizard_audio::output::{AudioOutput, AudioProducer};
use wizard_media::gst_pipeline::GstAudioOnlyHandle;
use wizard_media::pipeline::DecodedFrame;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;
use wizard_state::timeline::TimelineClipId;

use crate::audio_mixer::AudioMixer;
use crate::constants::*;
use crate::debug_log;
use crate::pipeline::{
    ForwardPipelineState, PendingPipeline, PendingReversePipeline, PendingShadowPipeline,
    PipelineStatus, ReversePipelineHandle as ReverseHandleKind, ReversePipelineState,
    ShadowAudioSourceRequest, ShadowPipelineState,
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
    pub pending_reverse: Option<PendingReversePipeline>,

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

static REVERSE_NO_FRAME_LOG_MS: AtomicU64 = AtomicU64::new(0);
static REVERSE_MAP_LOG_MS: AtomicU64 = AtomicU64::new(0);
static REVERSE_BOUNDARY_LOG_MS: AtomicU64 = AtomicU64::new(0);
const CPU_REVERSE_STARTUP_TIMEOUT_S: f64 = 1.5;

fn should_emit_throttled(last_ms: &AtomicU64, interval_ms: u64) -> bool {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let prev = last_ms.load(Ordering::Relaxed);
    if now_ms.saturating_sub(prev) < interval_ms {
        return false;
    }
    last_ms.store(now_ms, Ordering::Relaxed);
    true
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
        self.pending_reverse = None;
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
            self.pending_reverse = None;
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
        let _ = now - pending.started_at;

        match result {
            Ok(build) => {
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
                let force_activate = !ready && (now - rev.started_at) >= REVERSE_FORCE_PLAYING_AFTER_S;
                eprintln!(
                    "[REVERSE] try_activate: first_frame_ready={ready} force_activate={force_activate} activated={}",
                    rev.activated
                );
                if ready || force_activate {
                    rev.activated = true;
                    let result = rev.handle.begin_playing();
                    eprintln!("[REVERSE] begin_playing result: {result:?}");
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

        if !is_reverse && (self.reverse.is_some() || self.pending_reverse.is_some()) {
            self.reverse = None;
            self.pending_reverse = None;
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

        let mut reverse_is_stuck = false;
        let needs_new = !pending_matches
            && match self.reverse.as_ref() {
                None => true,
                Some(rev) => {
                    let startup_timeout_s = match rev.handle {
                        ReverseHandleKind::Cpu(_) => CPU_REVERSE_STARTUP_TIMEOUT_S,
                        ReverseHandleKind::Gst(_) => REVERSE_STARTUP_TIMEOUT_S,
                    };
                    reverse_is_stuck = rev.last_frame_time.is_none()
                        && now - rev.started_at > startup_timeout_s;
                    rev.timeline_clip != timeline_clip_id
                        || rev.clip.0 != clip_id
                        || rev.clip.1 != *path
                        || reverse_is_stuck
                }
            };

        eprintln!("[REVERSE] manage: needs_new={needs_new} pending_matches={pending_matches} has_reverse={} clip={clip_id:?}", self.reverse.is_some());
        if needs_new {
            // #region agent log
            debug_log::emit(
                "H4",
                "crates/app/src/playback_engine.rs:manage_reverse_pipeline",
                "reverse pipeline requested",
                &format!(
                    "needsNew={} pendingMatches={} hasReverse={} reverseIsStuck={} clipId={:?} timelineClipId={:?} sourceTime={:.3} speed={:.3}",
                    needs_new,
                    pending_matches,
                    self.reverse.is_some(),
                    reverse_is_stuck,
                    clip_id,
                    timeline_clip_id,
                    source_time,
                    speed
                ),
            );
            // #endregion
            self.reverse = None;
            self.pending_reverse = None;
            eprintln!("[REVERSE] spawning new pipeline for clip {clip_id:?} at source_time={source_time:.3}");
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
        let _ = now - pending.started_at;

        match result {
            Ok(handle) => {
                let _ = handle.is_first_frame_ready();
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
            Err(e) => {
                eprintln!("Failed to start pipeline: {e}");
            }
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
        let wait_ms = (now - pending.started_at) * 1000.0;

        match result {
            Ok(handle) => {
                eprintln!("[REVERSE] pipeline arrived after {wait_ms:.1}ms");
                // #region agent log
                debug_log::emit(
                    "H2",
                    "crates/app/src/playback_engine.rs:poll_pending_reverse_pipeline",
                    "reverse pipeline ready",
                    &format!(
                        "waitMs={:.1} clipId={:?} timelineClipId={:?} speed={:.3}",
                        wait_ms,
                        pending.clip.0,
                        pending.timeline_clip,
                        pending.speed
                    ),
                );
                // #endregion
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
            Err(e) => {
                eprintln!("Failed to start reverse pipeline: {e}");
                // #region agent log
                debug_log::emit(
                    "H2",
                    "crates/app/src/playback_engine.rs:poll_pending_reverse_pipeline",
                    "reverse pipeline failed to start",
                    &format!("waitMs={:.1} error={}", wait_ms, e),
                );
                // #endregion
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
                    eprintln!("[REVERSE] first frame: pts={:.3} expected={:.3} offset={:.3} src_in={:.3} src_out={:.3}", frame.pts_seconds, expected_source_at_playhead, offset, tc.source_in, tc.source_out);
                    rev.pts_offset = Some(offset);
                    offset
                };
                let mapped_source_pts = frame.pts_seconds - pts_offset;

                if mapped_source_pts >= tc.source_in && mapped_source_pts < tc.source_out {
                    let timeline_pos = tc.timeline_start + (mapped_source_pts - tc.source_in);
                    eprintln!("[REVERSE] playhead -> {timeline_pos:.3} (mapped_pts={mapped_source_pts:.3})");
                    let distance_to_clip_start = (mapped_source_pts - tc.source_in).max(0.0);
                    if distance_to_clip_start <= 0.12
                        && should_emit_throttled(&REVERSE_BOUNDARY_LOG_MS, 150)
                    {
                        // #region agent log
                        debug_log::emit(
                            "H6",
                            "crates/app/src/playback_engine.rs:apply_reverse_pipeline_frame",
                            "reverse near clip start but still in-range",
                            &format!(
                                "mappedSourcePts={:.3} sourceIn={:.3} sourceOut={:.3} distanceToStart={:.3} timelinePos={:.3} playhead={:.3} clip={:?}",
                                mapped_source_pts,
                                tc.source_in,
                                tc.source_out,
                                distance_to_clip_start,
                                timeline_pos,
                                state.project.playback.playhead,
                                rev.timeline_clip
                            ),
                        );
                        // #endregion
                    }
                    let should_apply =
                        previous_rev_pts.is_none() || timeline_pos <= state.project.playback.playhead;
                    if should_emit_throttled(&REVERSE_MAP_LOG_MS, 250) {
                        // #region agent log
                        debug_log::emit(
                            "H3",
                            "crates/app/src/playback_engine.rs:apply_reverse_pipeline_frame",
                            "reverse frame mapped to timeline",
                            &format!(
                                "framePts={:.3} mappedSourcePts={:.3} timelinePos={:.3} currentPlayhead={:.3} ptsOffset={:.3} hadPreviousRevPts={} shouldApply={}",
                                frame.pts_seconds,
                                mapped_source_pts,
                                timeline_pos,
                                state.project.playback.playhead,
                                pts_offset,
                                previous_rev_pts.is_some(),
                                should_apply
                            ),
                        );
                        // #endregion
                    }
                    if should_apply {
                        state.project.playback.playhead = timeline_pos;
                    }
                    if distance_to_clip_start <= 0.12 && previous_rev_pts.is_some() {
                        let from_timeline_clip = rev.timeline_clip;
                        let prev_time = (tc.timeline_start - 0.001).max(0.0);
                        state.project.playback.playhead = prev_time;
                        self.reverse = None;
                        self.reset_audio_sources();

                        // #region agent log
                        debug_log::emit(
                            "H7",
                            "crates/app/src/playback_engine.rs:apply_reverse_pipeline_frame",
                            "reverse transitioned from near-start in-range frame",
                            &format!(
                                "fromTimelineClip={:?} distanceToStart={:.3} prevTime={:.3}",
                                from_timeline_clip, distance_to_clip_start, prev_time
                            ),
                        );
                        // #endregion

                        if prev_time > 0.0 {
                            if let Some(prev_hit) = state
                                .project
                                .timeline
                                .previous_clip_before(from_timeline_clip)
                            {
                                let prev_clip_id = prev_hit.clip.source_id;
                                let prev_timeline_clip_id = prev_hit.clip.id;
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
                            } else {
                                // #region agent log
                                debug_log::emit(
                                    "H7",
                                    "crates/app/src/playback_engine.rs:apply_reverse_pipeline_frame",
                                    "reverse near-start transition failed: no previous clip",
                                    &format!(
                                        "fromTimelineClip={:?} distanceToStart={:.3} prevTime={:.3}",
                                        from_timeline_clip, distance_to_clip_start, prev_time
                                    ),
                                );
                                // #endregion
                            }
                        }
                        return false;
                    }
                } else if mapped_source_pts < tc.source_in {
                    eprintln!("[REVERSE] mapped_pts={mapped_source_pts:.3} < source_in={:.3} — clip transition", tc.source_in);
                    let from_timeline_clip = rev.timeline_clip;
                    // #region agent log
                    debug_log::emit(
                        "H3",
                        "crates/app/src/playback_engine.rs:apply_reverse_pipeline_frame",
                        "reverse frame below source range, transitioning to previous clip",
                        &format!(
                            "framePts={:.3} mappedSourcePts={:.3} sourceIn={:.3} timelineStart={:.3}",
                            frame.pts_seconds,
                            mapped_source_pts,
                            tc.source_in,
                            tc.timeline_start
                        ),
                    );
                    // #endregion
                    let prev_time = (tc.timeline_start - 0.001).max(0.0);
                    state.project.playback.playhead = prev_time;
                    self.reverse = None;
                    self.reset_audio_sources();

                    if prev_time > 0.0 {
                        if let Some(prev_hit) = state
                            .project
                            .timeline
                            .previous_clip_before(from_timeline_clip)
                        {
                            let prev_clip_id = prev_hit.clip.source_id;
                            let prev_timeline_clip_id = prev_hit.clip.id;
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
                                // #region agent log
                                debug_log::emit(
                                    "H7",
                                    "crates/app/src/playback_engine.rs:apply_reverse_pipeline_frame",
                                    "reverse transitioned to previous clip",
                                    &format!(
                                        "fromTimelineClip={:?} prevTime={:.3} toTimelineClip={:?} toSourceTime={:.3}",
                                        from_timeline_clip,
                                        prev_time,
                                        prev_timeline_clip_id,
                                        prev_hit.source_time
                                    ),
                                );
                                // #endregion
                            }
                        } else {
                            // #region agent log
                            debug_log::emit(
                                "H7",
                                "crates/app/src/playback_engine.rs:apply_reverse_pipeline_frame",
                                "reverse transition failed: no previous hit at boundary time",
                                &format!(
                                    "fromTimelineClip={:?} prevTime={:.3}",
                                    from_timeline_clip,
                                    prev_time
                                ),
                            );
                            // #endregion
                        }
                    } else {
                        // #region agent log
                        debug_log::emit(
                            "H7",
                            "crates/app/src/playback_engine.rs:apply_reverse_pipeline_frame",
                            "reverse reached absolute timeline start",
                            &format!(
                                "fromTimelineClip={:?} prevTime={:.3}",
                                from_timeline_clip,
                                prev_time
                            ),
                        );
                        // #endregion
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
            if reverse_frames.is_empty() && rev.activated {
                eprintln!("[REVERSE] activated but no frames this tick (last_frame_time={:?})", rev.last_frame_time);
                if should_emit_throttled(&REVERSE_NO_FRAME_LOG_MS, 500) {
                    // #region agent log
                    debug_log::emit(
                        "H2",
                        "crates/app/src/playback_engine.rs:poll_pipeline_frames",
                        "reverse activated but delivered no frames",
                        &format!(
                            "lastFrameTime={:?} startedAt={:.3} status={:?} isActivated={}",
                            rev.last_frame_time,
                            rev.started_at,
                            rev.status(now),
                            rev.activated
                        ),
                    );
                    // #endregion
                }
            }
        }
        if !reverse_frames.is_empty() {
            eprintln!("[REVERSE] {} frame(s) received, pts={:.3}", reverse_frames.len(), reverse_frames[0].pts_seconds);
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

mod workers;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ringbuf::traits::Producer;

use wizard_audio::output::{AudioOutput, AudioProducer};
use wizard_media::metadata::MediaMetadata;
use wizard_media::pipeline::{DecodedFrame, PipelineHandle, ReversePipelineHandle};
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;
use wizard_state::timeline::TimelineClipId;

use workers::audio_worker::{AudioPreviewRequest, AudioWorkerChannels};
use workers::preview_worker::{PreviewRequest, PreviewWorkerChannels};
use workers::video_decode_worker::VideoDecodeWorkerChannels;

#[derive(Default)]
pub struct TextureCache {
    pub thumbnails: HashMap<ClipId, egui::TextureHandle>,
    pub preview_frames: HashMap<ClipId, Vec<egui::TextureHandle>>,
    pub pending_thumbnails: HashSet<ClipId>,
    pub preview_requested: HashSet<ClipId>,
    pub waveform_peaks: HashMap<ClipId, Vec<(f32, f32)>>,
    pub playback_texture: Option<egui::TextureHandle>,
}

impl wizard_ui::TextureLookup for TextureCache {
    fn thumbnail(&self, id: &ClipId) -> Option<&egui::TextureHandle> {
        self.thumbnails.get(id)
    }

    fn preview_frames(&self, id: &ClipId) -> Option<&Vec<egui::TextureHandle>> {
        self.preview_frames.get(id)
    }

    fn is_pending(&self, id: &ClipId) -> bool {
        self.pending_thumbnails.contains(id)
    }

    fn is_preview_loading(&self, id: &ClipId) -> bool {
        self.preview_requested.contains(id)
    }

    fn waveform_peaks(&self, id: &ClipId) -> Option<&Vec<(f32, f32)>> {
        self.waveform_peaks.get(id)
    }

    fn playback_frame(&self) -> Option<&egui::TextureHandle> {
        self.playback_texture.as_ref()
    }
}

pub struct EditorApp {
    state: AppState,
    textures: TextureCache,
    last_frame_time: Option<f64>,
    thumb_tx: mpsc::Sender<(ClipId, image::RgbaImage)>,
    thumb_rx: mpsc::Receiver<(ClipId, image::RgbaImage)>,
    meta_tx: mpsc::Sender<(ClipId, MediaMetadata)>,
    meta_rx: mpsc::Receiver<(ClipId, MediaMetadata)>,
    preview: PreviewWorkerChannels,
    audio_output: Option<AudioOutput>,
    audio_producer: Arc<Mutex<AudioProducer>>,
    audio_sample_rate: u32,
    audio_channels: u16,
    audio: AudioWorkerChannels,
    last_hover_audio_request: Option<(ClipId, i64)>,
    last_scrub_audio_request: Option<(ClipId, i64)>,
    last_video_decode_request: Option<(ClipId, i64)>,
    waveform_tx: mpsc::Sender<(ClipId, Vec<(f32, f32)>)>,
    waveform_rx: mpsc::Receiver<(ClipId, Vec<(f32, f32)>)>,
    video_decode: VideoDecodeWorkerChannels,
    pipeline: Option<PipelineHandle>,
    pipeline_clip: Option<(ClipId, PathBuf)>,
    pipeline_timeline_clip: Option<TimelineClipId>,
    pipeline_source_time: Option<f64>,
    reverse_pipeline: Option<ReversePipelineHandle>,
    reverse_pipeline_clip: Option<(ClipId, PathBuf)>,
    reverse_pipeline_timeline_clip: Option<TimelineClipId>,
    reverse_pipeline_speed: f64,
    pipeline_speed: f64,
    was_scrubbing: bool,
    video_fps_window_start: Option<f64>,
    video_fps_window_frames: u32,
    no_audio_paths: Arc<Mutex<HashSet<std::path::PathBuf>>>,
    folder_watcher: Option<RecommendedWatcher>,
    watch_rx: mpsc::Receiver<PathBuf>,
    watch_tx: mpsc::Sender<PathBuf>,
    known_paths: HashSet<PathBuf>,
    last_is_playing: bool,
    last_playback_state: PlaybackState,
    last_pipeline_frame_time: Option<f64>,
    last_decoded_frame: Option<(f64, &'static str)>,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        wizard_ui::theme::apply_theme(&cc.egui_ctx);

        if let Some(render_state) = &cc.wgpu_render_state {
            let renderer = wizard_ui::waveform_gpu::WaveformRenderer::new(
                &render_state.device,
                render_state.target_format,
            );
            render_state
                .renderer
                .write()
                .callback_resources
                .insert(renderer);
            cc.egui_ctx
                .data_mut(|d| d.insert_temp(egui::Id::new("gpu_waveforms"), true));
        }

        let (thumb_tx, thumb_rx) = mpsc::channel();
        let (meta_tx, meta_rx) = mpsc::channel();
        let (waveform_tx, waveform_rx) = mpsc::channel::<(ClipId, Vec<(f32, f32)>)>();

        let preview = workers::preview_worker::spawn_preview_worker();
        let video_decode = workers::video_decode_worker::spawn_video_decode_worker();

        let (audio_output, audio_producer, audio_sample_rate, audio_channels) =
            match AudioOutput::new() {
                Ok((output, producer)) => {
                    let sr = output.sample_rate_hz();
                    let ch = output.channels();
                    (Some(output), producer, sr, ch)
                }
                Err(_) => {
                    let rb = ringbuf::HeapRb::<f32>::new(4096);
                    let (producer, _consumer) = ringbuf::traits::Split::split(rb);
                    (None, producer, 48000, 2)
                }
            };

        let no_audio_paths: Arc<Mutex<HashSet<std::path::PathBuf>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let audio = workers::audio_worker::spawn_audio_worker(no_audio_paths.clone());
        let (watch_tx, watch_rx) = mpsc::channel::<PathBuf>();

        Self {
            state: AppState::default(),
            textures: TextureCache::default(),
            last_frame_time: None,
            thumb_tx,
            thumb_rx,
            meta_tx,
            meta_rx,
            preview,
            audio_output,
            audio_producer: Arc::new(Mutex::new(audio_producer)),
            audio_sample_rate,
            audio_channels,
            audio,
            last_hover_audio_request: None,
            last_scrub_audio_request: None,
            last_video_decode_request: None,
            waveform_tx,
            waveform_rx,
            video_decode,
            pipeline: None,
            pipeline_clip: None,
            pipeline_timeline_clip: None,
            pipeline_source_time: None,
            reverse_pipeline: None,
            reverse_pipeline_clip: None,
            reverse_pipeline_timeline_clip: None,
            reverse_pipeline_speed: 1.0,
            pipeline_speed: 1.0,
            was_scrubbing: false,
            video_fps_window_start: None,
            video_fps_window_frames: 0,
            no_audio_paths,
            folder_watcher: None,
            watch_rx,
            watch_tx,
            known_paths: HashSet::new(),
            last_is_playing: false,
            last_playback_state: PlaybackState::Stopped,
            last_pipeline_frame_time: None,
            last_decoded_frame: None,
        }
    }

    fn poll_background_tasks(&mut self, ctx: &egui::Context) {
        let mut received = false;

        while let Ok((id, img)) = self.thumb_rx.try_recv() {
            let texture = ctx.load_texture(
                format!("thumb_{:?}", id),
                egui::ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    img.as_raw(),
                ),
                egui::TextureOptions::LINEAR,
            );
            self.textures.thumbnails.insert(id, texture);
            self.textures.pending_thumbnails.remove(&id);
            received = true;
        }

        while let Ok((id, meta)) = self.meta_rx.try_recv() {
            let tag_mask = self.state.project.clip_tag_mask(id);
            if let Some(clip) = self.state.project.clips.get_mut(&id) {
                clip.duration = meta.duration;
                clip.resolution = meta.resolution;
                clip.codec = meta.codec;
                clip.rebuild_search_haystack(tag_mask);
            }
            received = true;
        }

        while let Ok(pf) = self.preview.result_rx.try_recv() {
            let texture = ctx.load_texture(
                format!("preview_{:?}_{}", pf.clip_id, pf.index),
                egui::ColorImage::from_rgba_unmultiplied(
                    [pf.image.width() as usize, pf.image.height() as usize],
                    pf.image.as_raw(),
                ),
                egui::TextureOptions::LINEAR,
            );
            let frames = self
                .textures
                .preview_frames
                .entry(pf.clip_id)
                .or_insert_with(|| Vec::with_capacity(pf.total));
            if frames.len() <= pf.index {
                frames.resize_with(pf.index + 1, || {
                    ctx.load_texture(
                        "placeholder",
                        egui::ColorImage::new([1, 1], egui::Color32::TRANSPARENT),
                        Default::default(),
                    )
                });
            }
            frames[pf.index] = texture;
            received = true;
        }

        while let Ok((id, peaks)) = self.waveform_rx.try_recv() {
            self.textures.waveform_peaks.insert(id, peaks);
            received = true;
        }

        let now = ctx.input(|i| i.time);

        let mut pipeline_frames: Vec<DecodedFrame> = Vec::new();
        if let Some(ref pipeline) = self.pipeline {
            while let Some(frame) = pipeline.try_recv_frame() {
                pipeline_frames.push(frame);
            }
        }
        for frame in &pipeline_frames {
            received = true;
            if !self.apply_pipeline_frame(ctx, frame, now) {
                break;
            }
        }

        let mut reverse_frames: Vec<DecodedFrame> = Vec::new();
        if let Some(ref rp) = self.reverse_pipeline {
            while let Some(frame) = rp.try_recv_frame() {
                reverse_frames.push(frame);
            }
        }
        for frame in &reverse_frames {
            received = true;
            if !self.apply_reverse_pipeline_frame(ctx, frame, now) {
                break;
            }
        }

        while let Ok((_clip_id, _time, img)) = self.video_decode.result_rx.try_recv() {
            let texture = ctx.load_texture(
                "playback_frame",
                egui::ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    img.as_raw(),
                ),
                egui::TextureOptions::LINEAR,
            );
            self.textures.playback_texture = Some(texture);
            received = true;
        }

        while let Ok(snippet) = self.audio.snippet_rx.try_recv() {
            if let Ok(mut producer) = self.audio_producer.lock() {
                if !self.is_playing() {
                    producer.push_slice(&[]);
                }
                let ch = self.audio_channels;
                wizard_audio::output::enqueue_samples(&mut producer, &snippet.samples_mono, ch);
            }
        }

        if received {
            ctx.request_repaint();
        }
    }

    fn apply_pipeline_frame(&mut self, ctx: &egui::Context, frame: &DecodedFrame, now: f64) -> bool {
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

        let active_clip = self
            .pipeline_timeline_clip
            .and_then(|timeline_clip_id| {
                self.state
                    .project
                    .timeline
                    .find_clip(timeline_clip_id)
                    .map(|(_, _, tc)| (tc.timeline_start, tc.duration, tc.source_in, tc.source_out))
            });
        if let Some((timeline_start, duration, source_in, source_out)) = active_clip {
            if frame.pts_seconds >= source_in && frame.pts_seconds < source_out {
                let new_playhead = timeline_start + (frame.pts_seconds - source_in);
                if new_playhead >= self.state.project.playback.playhead {
                    self.state.project.playback.playhead = new_playhead;
                }
            } else if frame.pts_seconds >= source_out {
                let next_time = timeline_start + duration;
                self.state.project.playback.playhead = next_time;
                self.pipeline = None;
                self.pipeline_clip = None;
                self.pipeline_timeline_clip = None;
                self.pipeline_source_time = None;
                self.last_pipeline_frame_time = None;
                self.reset_audio_queue();

                if let Some(next_hit) = self.state.project.timeline.clip_at_time(next_time) {
                    let next_clip_id = next_hit.clip.source_id;
                    let next_timeline_clip_id = next_hit.clip.id;
                    if let Some(clip) = self.state.project.clips.get(&next_clip_id) {
                        let path = clip.path.clone();
                        self.start_pipeline(
                            next_timeline_clip_id,
                            next_clip_id,
                            &path,
                            next_hit.source_time,
                        );
                    }
                }
                return false;
            }
        } else if let Some(timeline_pos) = self.find_timeline_hit_for_source_pts(frame.pts_seconds)
        {
            if timeline_pos >= self.state.project.playback.playhead {
                self.state.project.playback.playhead = timeline_pos;
            }
        }
        self.last_pipeline_frame_time = Some(now);

        if self.video_fps_window_start.is_none() {
            self.video_fps_window_start = Some(now);
            self.video_fps_window_frames = 0;
        }
        self.video_fps_window_frames += 1;

        if let Some(start) = self.video_fps_window_start {
            let elapsed = (now - start).max(0.0);
            if elapsed >= 0.25 {
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
        true
    }

    fn apply_reverse_pipeline_frame(
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

        if let Some(timeline_clip_id) = self.reverse_pipeline_timeline_clip {
            if let Some((_, _, tc)) = self.state.project.timeline.find_clip(timeline_clip_id) {
                if frame.pts_seconds >= tc.source_in && frame.pts_seconds < tc.source_out {
                    let timeline_pos = tc.timeline_start + (frame.pts_seconds - tc.source_in);
                    if timeline_pos <= self.state.project.playback.playhead {
                        self.state.project.playback.playhead = timeline_pos;
                    }
                } else if frame.pts_seconds < tc.source_in {
                    let prev_time = (tc.timeline_start - 0.001).max(0.0);
                    self.state.project.playback.playhead = prev_time;
                    self.reverse_pipeline = None;
                    self.reverse_pipeline_clip = None;
                    self.reverse_pipeline_timeline_clip = None;
                    self.last_pipeline_frame_time = None;
                    self.reset_audio_queue();

                    if prev_time > 0.0 {
                        if let Some(prev_hit) =
                            self.state.project.timeline.clip_at_time(prev_time)
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
                                    self.reverse_pipeline = Some(handle);
                                    self.reverse_pipeline_clip =
                                        Some((prev_clip_id, path));
                                    self.reverse_pipeline_timeline_clip =
                                        Some(prev_timeline_clip_id);
                                    self.reverse_pipeline_speed = speed;
                                }
                            }
                        }
                    }
                    return false;
                }
            }
        }
        self.last_pipeline_frame_time = Some(now);

        if self.video_fps_window_start.is_none() {
            self.video_fps_window_start = Some(now);
            self.video_fps_window_frames = 0;
        }
        self.video_fps_window_frames += 1;

        if let Some(start) = self.video_fps_window_start {
            let elapsed = (now - start).max(0.0);
            if elapsed >= 0.25 {
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
        true
    }

    fn find_timeline_hit_for_source_pts(&self, pts: f64) -> Option<f64> {
        let timeline_clip_id = self.pipeline_timeline_clip?;
        let (_, _, tc) = self.state.project.timeline.find_clip(timeline_clip_id)?;
        if pts >= tc.source_in && pts < tc.source_out {
            return Some(tc.timeline_start + (pts - tc.source_in));
        }
        None
    }

    fn manage_pipeline(&mut self, now: f64) {
        let is_forward = self.state.project.playback.state == PlaybackState::Playing;
        let is_reverse = self.state.project.playback.state == PlaybackState::PlayingReverse;
        let is_playing = self.is_playing();
        let is_scrubbing = self.state.ui.timeline.scrubbing.is_some();

        if !is_forward && self.pipeline.is_some() {
            self.pipeline = None;
            self.pipeline_clip = None;
            self.pipeline_timeline_clip = None;
            self.pipeline_source_time = None;
            self.last_pipeline_frame_time = None;
        }

        if !is_reverse && self.reverse_pipeline.is_some() {
            self.reverse_pipeline = None;
            self.reverse_pipeline_clip = None;
            self.reverse_pipeline_timeline_clip = None;
            self.last_pipeline_frame_time = None;
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
            let had_pipeline = self.pipeline.is_some() || self.reverse_pipeline.is_some();
            self.pipeline = None;
            self.pipeline_clip = None;
            self.pipeline_timeline_clip = None;
            self.pipeline_source_time = None;
            self.reverse_pipeline = None;
            self.reverse_pipeline_clip = None;
            self.reverse_pipeline_timeline_clip = None;
            self.last_pipeline_frame_time = None;
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
            self.pipeline = None;
            self.pipeline_clip = None;
            self.pipeline_timeline_clip = None;
            self.pipeline_source_time = None;
            self.reverse_pipeline = None;
            self.reverse_pipeline_clip = None;
            self.reverse_pipeline_timeline_clip = None;
            self.last_pipeline_frame_time = None;
            self.reset_audio_queue();
            self.was_scrubbing = true;
            return;
        }

        if is_forward {
            let speed = self.state.project.playback.speed;
            let speed_changed =
                self.pipeline.is_some() && (speed - self.pipeline_speed).abs() > 0.01;

            let needs_new_pipeline = if self.pipeline.is_none() {
                true
            } else {
                match (self.pipeline_timeline_clip, &self.pipeline_clip) {
                    (Some(current_timeline_id), Some((current_id, current_path))) => {
                        current_timeline_id != timeline_clip_id
                            || *current_id != clip_id
                            || *current_path != path
                    }
                    (None, _) => true,
                    _ => true,
                }
            };

            if speed_changed && !needs_new_pipeline {
                if let Some(ref p) = self.pipeline {
                    p.update_speed(speed);
                }
                let audio_muted_before = (self.pipeline_speed - 1.0).abs() > 0.01;
                let audio_muted_after = (speed - 1.0).abs() > 0.01;
                if audio_muted_before != audio_muted_after {
                    self.pipeline = None;
                    self.pipeline_clip = None;
                    self.pipeline_timeline_clip = None;
                    self.pipeline_source_time = None;
                    self.last_pipeline_frame_time = None;
                    self.reset_audio_queue();
                    self.start_pipeline(timeline_clip_id, clip_id, &path, hit.source_time);
                }
                self.pipeline_speed = speed;
            }

            let has_stale_pipeline = self
                .last_pipeline_frame_time
                .is_some_and(|t| (now - t) > 0.75);
            if self.pipeline.is_some() && has_stale_pipeline && !needs_new_pipeline {
                self.pipeline = None;
                self.pipeline_clip = None;
                self.pipeline_timeline_clip = None;
                self.pipeline_source_time = None;
                self.last_pipeline_frame_time = None;
                self.reset_audio_queue();
                self.start_pipeline(timeline_clip_id, clip_id, &path, hit.source_time);
                self.was_scrubbing = is_scrubbing;
                return;
            }

            if scrub_just_released || needs_new_pipeline {
                self.pipeline = None;
                self.pipeline_clip = None;
                self.pipeline_timeline_clip = None;
                self.pipeline_source_time = None;
                self.last_pipeline_frame_time = None;
                if scrub_just_released {
                    self.reset_audio_queue();
                }
                self.start_pipeline(timeline_clip_id, clip_id, &path, hit.source_time);
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

        if speed != self.reverse_pipeline_speed {
            if let Some(ref rp) = self.reverse_pipeline {
                rp.update_speed(speed);
            }
            self.reverse_pipeline_speed = speed;
        }

        let needs_new = if self.reverse_pipeline.is_none() {
            true
        } else {
            match (
                self.reverse_pipeline_timeline_clip,
                &self.reverse_pipeline_clip,
            ) {
                (Some(current_timeline_id), Some((current_id, current_path))) => {
                    current_timeline_id != timeline_clip_id
                        || *current_id != clip_id
                        || *current_path != *path
                }
                (None, _) => true,
                _ => true,
            }
        };

        if needs_new {
            self.reverse_pipeline = None;
            self.reverse_pipeline_clip = None;
            self.reverse_pipeline_timeline_clip = None;
            self.last_pipeline_frame_time = None;
            match ReversePipelineHandle::start(
                path,
                source_time,
                speed,
                workers::video_decode_worker::PLAYBACK_DECODE_WIDTH,
                workers::video_decode_worker::PLAYBACK_DECODE_HEIGHT,
            ) {
                Ok(handle) => {
                    self.reverse_pipeline = Some(handle);
                    self.reverse_pipeline_clip = Some((clip_id, path.to_path_buf()));
                    self.reverse_pipeline_timeline_clip = Some(timeline_clip_id);
                    self.reverse_pipeline_speed = speed;
                }
                Err(e) => {
                    eprintln!("Failed to start reverse pipeline: {e}");
                }
            }
        }
    }

    fn start_pipeline(
        &mut self,
        timeline_clip_id: TimelineClipId,
        clip_id: ClipId,
        path: &Path,
        source_time: f64,
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
                self.pipeline = Some(handle);
                self.pipeline_clip = Some((clip_id, path.to_path_buf()));
                self.pipeline_timeline_clip = Some(timeline_clip_id);
                self.pipeline_source_time = Some(source_time);
                self.pipeline_speed = speed;
                self.last_pipeline_frame_time = None;
            }
            Err(e) => {
                eprintln!("Failed to start pipeline: {e}");
            }
        }
    }

    fn enqueue_visible_previews(&mut self) {
        const PREFETCH_PER_FRAME: usize = 2;
        let mut remaining = PREFETCH_PER_FRAME;

        if let Some(clip_id) = self.state.ui.selection.hovered_clip {
            let _ = self.enqueue_preview_request(clip_id, true);
        }

        if let Some(clip_id) = self.state.ui.selection.selected_clip {
            let _ = self.enqueue_preview_request(clip_id, true);
        }

        let visible: Vec<ClipId> = self.state.ui.browser.visible_clips.clone();
        for clip_id in visible {
            if remaining == 0 {
                break;
            }
            if self.enqueue_preview_request(clip_id, false) {
                remaining -= 1;
            }
        }
    }

    fn enqueue_preview_request(&mut self, clip_id: ClipId, priority: bool) -> bool {
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

    fn is_playing(&self) -> bool {
        matches!(
            self.state.project.playback.state,
            PlaybackState::Playing | PlaybackState::PlayingReverse
        )
    }

    fn path_has_no_audio(&self, path: &std::path::Path) -> bool {
        self.no_audio_paths
            .lock()
            .map(|set| set.contains(path))
            .unwrap_or(false)
    }

    fn update_hover_audio(&mut self) {
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
        let bucket = (time_seconds * 10.0).round() as i64;
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

    fn update_timeline_scrub_audio(&mut self) {
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

        let bucket = (hit.source_time * 10.0).round() as i64;
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

    fn update_playback_frame(&mut self) {
        if self.pipeline.is_some() || self.reverse_pipeline.is_some() {
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

        if let Some(hit) = self.state.project.timeline.clip_at_time(time) {
            if let Some(clip) = self.state.project.clips.get(&hit.clip.source_id) {
                let bucket = (hit.source_time * 60.0).round() as i64;
                if self.last_video_decode_request == Some((hit.clip.source_id, bucket)) {
                    return;
                }

                let clip_changed = self
                    .last_video_decode_request
                    .is_none_or(|(id, _)| id != hit.clip.source_id);
                if clip_changed {
                    if let Some(thumb) = self.textures.thumbnails.get(&hit.clip.source_id) {
                        self.textures.playback_texture = Some(thumb.clone());
                    }
                }

                let _ = self.video_decode.req_tx.send((
                    hit.clip.source_id,
                    clip.path.clone(),
                    hit.source_time,
                ));
                self.last_video_decode_request = Some((hit.clip.source_id, bucket));
            }
        } else {
            self.last_video_decode_request = None;
        }
    }

    fn reset_audio_queue(&mut self) {
        if self.audio_output.is_none() {
            return;
        }
        if let Ok((output, producer)) = AudioOutput::new() {
            self.audio_sample_rate = output.sample_rate_hz();
            self.audio_channels = output.channels();
            self.audio_output = Some(output);
            self.audio_producer = Arc::new(Mutex::new(producer));
        }
    }

    fn handle_playback_stop_transition(&mut self) {
        let _ = self.audio.req_tx.send(AudioPreviewRequest::Stop);
        self.last_hover_audio_request = None;
        self.last_scrub_audio_request = None;
        self.last_video_decode_request = None;
        self.last_decoded_frame = None;
        self.reset_audio_queue();
    }

    fn handle_playback_state_transition(
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
            self.pipeline = None;
            self.pipeline_clip = None;
            self.pipeline_timeline_clip = None;
            self.pipeline_source_time = None;
            self.reverse_pipeline = None;
            self.reverse_pipeline_clip = None;
            self.reverse_pipeline_timeline_clip = None;
            self.handle_playback_stop_transition();
        }
    }

    fn import_file(&mut self, p: PathBuf) {
        if self.known_paths.contains(&p) {
            return;
        }
        self.known_paths.insert(p.clone());

        let clip = wizard_state::clip::Clip::from_path(p.clone());
        let clip_id = clip.id;
        self.state.project.add_clip(clip);
        self.textures.pending_thumbnails.insert(clip_id);

        let ttx = self.thumb_tx.clone();
        let mtx = self.meta_tx.clone();
        let wtx = self.waveform_tx.clone();
        std::thread::spawn(move || {
            let meta = wizard_media::metadata::extract_metadata(&p);
            let _ = mtx.send((clip_id, meta));

            if let Some(img) = wizard_media::thumbnail::extract_thumbnail(&p) {
                let _ = ttx.send((clip_id, img));
            }

            let peaks = wizard_media::audio::extract_waveform_peaks(&p, 512);
            if !peaks.is_empty() {
                let _ = wtx.send((clip_id, peaks));
            }
        });
    }

    fn import_folder(&mut self, path: PathBuf) {
        let files = wizard_media::import::scan_folder(&path);
        for p in files {
            self.import_file(p);
        }

        let tx = self.watch_tx.clone();
        let watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                let Ok(event) = res else { return };
                use notify::EventKind;
                if !matches!(event.kind, EventKind::Create(_)) {
                    return;
                }
                for p in event.paths {
                    if p.is_file() {
                        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                            if wizard_media::import::VIDEO_EXTENSIONS
                                .contains(&ext.to_lowercase().as_str())
                            {
                                let _ = tx.send(p);
                            }
                        }
                    }
                }
            });
        if let Ok(mut w) = watcher {
            let _ = w.watch(&path, RecursiveMode::Recursive);
            self.folder_watcher = Some(w);
        }
    }

    fn poll_folder_watcher(&mut self) {
        while let Ok(path) = self.watch_rx.try_recv() {
            self.import_file(path);
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let was_playing = self.last_is_playing;
        let previous_playback_state = self.last_playback_state;
        let now = ctx.input(|i| i.time);
        if let Some(last) = self.last_frame_time {
            let dt = now - last;
            let duration = self.state.project.timeline.timeline_duration();
            let pipeline_driving = self.pipeline.is_some() || self.reverse_pipeline.is_some();
            let should_use_clock_advance =
                !pipeline_driving && self.state.project.playback.state != PlaybackState::Stopped;
            if should_use_clock_advance {
                self.state.project.playback.advance(dt, duration);
            }
            if dt > 0.0 {
                let inst_fps = (1.0 / dt) as f32;
                if self.state.ui.debug.ui_fps <= 0.0 {
                    self.state.ui.debug.ui_fps = inst_fps;
                } else {
                    self.state.ui.debug.ui_fps = self.state.ui.debug.ui_fps * 0.9 + inst_fps * 0.1;
                }
            }
        }
        self.last_frame_time = Some(now);

        workers::keyboard::handle_keyboard(ctx, &mut self.state);
        self.handle_playback_state_transition(
            previous_playback_state,
            self.state.project.playback.state,
        );

        self.manage_pipeline(now);
        self.poll_background_tasks(ctx);
        self.poll_folder_watcher();

        egui::TopBottomPanel::top("top_panel")
            .exact_height(28.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.state.ui.debug.show_fps, "FPS");
                    if self.state.ui.debug.show_fps {
                        ui.label(format!(
                            "UI {:.1} | Video {:.1}",
                            self.state.ui.debug.ui_fps, self.state.ui.debug.video_fps
                        ));
                    }
                });
            });

        if self.state.ui.browser.show_browser {
            let mut action = wizard_ui::browser::BrowserAction::None;
            egui::SidePanel::left("browser_panel")
                .width_range(200.0..=1200.0)
                .default_width(425.0)
                .show(ctx, |ui| {
                    action = wizard_ui::browser::browser_panel(ui, &mut self.state, &self.textures);
                });
            match action {
                wizard_ui::browser::BrowserAction::None => {}
                wizard_ui::browser::BrowserAction::Collapse => {
                    self.state.ui.browser.show_browser = false;
                }
                wizard_ui::browser::BrowserAction::ImportFolder(path) => {
                    self.import_folder(path);
                }
            }
        } else {
            egui::SidePanel::left("browser_panel_collapsed")
                .exact_width(32.0)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(4.0);
                        if ui
                            .button("\u{25B6}")
                            .on_hover_text("Show browser")
                            .clicked()
                        {
                            self.state.ui.browser.show_browser = true;
                        }
                    });
                });
        }

        self.enqueue_visible_previews();
        self.update_hover_audio();
        self.update_timeline_scrub_audio();
        self.update_playback_frame();

        egui::TopBottomPanel::bottom("timeline_panel")
            .resizable(true)
            .height_range(40.0..=800.0)
            .default_height(465.0)
            .show(ctx, |ui| {
                wizard_ui::timeline::timeline_panel(ui, &mut self.state, &self.textures);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            wizard_ui::preview::preview_panel(ui, &mut self.state, &self.textures);
        });

        if self.state.ui.debug.show_fps {
            egui::Area::new(egui::Id::new("fps_overlay"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
                .show(ctx, |ui| {
                    ui.label(format!(
                        "UI {:.1} fps | Video {:.1} fps",
                        self.state.ui.debug.ui_fps, self.state.ui.debug.video_fps
                    ));
                    if let Some((pts, direction)) = self.last_decoded_frame {
                        ui.label(format!("Decode {direction} pts: {pts:.3}"));
                    } else {
                        ui.label("Decode pts: -");
                    }

                    let active_timeline_clip = self
                        .pipeline_timeline_clip
                        .or(self.reverse_pipeline_timeline_clip);
                    if let Some(timeline_clip_id) = active_timeline_clip {
                        if let Some((_, _, tc)) = self.state.project.timeline.find_clip(timeline_clip_id)
                        {
                            ui.label(format!(
                                "Clip {:?} src [{:.3}..{:.3}]",
                                timeline_clip_id, tc.source_in, tc.source_out
                            ));
                        } else {
                            ui.label("Clip: stale");
                        }
                    } else {
                        ui.label("Clip: none");
                    }
                });
        }

        let is_playing = self.is_playing();
        if was_playing && !is_playing {
            self.handle_playback_stop_transition();
        }
        self.last_is_playing = is_playing;
        self.last_playback_state = self.state.project.playback.state;

        if self.state.project.playback.state != PlaybackState::Stopped {
            ctx.request_repaint();
        }
    }
}

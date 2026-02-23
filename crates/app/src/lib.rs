mod audio_mixer;
mod channel_polling;
mod constants;
mod import;
mod pipeline;
mod playback;
pub mod texture_cache;
pub mod workers;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use notify::RecommendedWatcher;
use wizard_audio::output::{AudioOutput, AudioProducer};
use wizard_media::metadata::MediaMetadata;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

use audio_mixer::AudioMixer;
use pipeline::{ForwardPipelineState, ReversePipelineState, ShadowPipelineState};
use texture_cache::TextureCache;
use workers::audio_worker::AudioWorkerChannels;
use workers::preview_worker::PreviewWorkerChannels;
use workers::video_decode_worker::VideoDecodeWorkerChannels;

use crate::constants::*;

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
    mixer: AudioMixer,
    audio: AudioWorkerChannels,
    last_hover_audio_request: Option<(ClipId, i64)>,
    last_scrub_audio_request: Option<(ClipId, i64)>,
    last_video_decode_request: Option<(ClipId, i64)>,
    waveform_tx: mpsc::Sender<(ClipId, Vec<(f32, f32)>)>,
    waveform_rx: mpsc::Receiver<(ClipId, Vec<(f32, f32)>)>,
    video_decode: VideoDecodeWorkerChannels,
    forward: Option<ForwardPipelineState>,
    shadow: Option<ShadowPipelineState>,
    reverse: Option<ReversePipelineState>,
    was_scrubbing: bool,
    video_fps_window_start: Option<f64>,
    video_fps_window_frames: u32,
    no_audio_paths: Arc<Mutex<HashSet<PathBuf>>>,
    folder_watcher: Option<RecommendedWatcher>,
    watch_rx: mpsc::Receiver<PathBuf>,
    watch_tx: mpsc::Sender<PathBuf>,
    known_paths: HashSet<PathBuf>,
    last_is_playing: bool,
    last_playback_state: PlaybackState,
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

        let no_audio_paths: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let audio = workers::audio_worker::spawn_audio_worker(no_audio_paths.clone());
        let (watch_tx, watch_rx) = mpsc::channel::<PathBuf>();

        let audio_producer = Arc::new(Mutex::new(audio_producer));
        let mixer = AudioMixer::new(audio_producer.clone());

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
            audio_producer,
            audio_sample_rate,
            audio_channels,
            mixer,
            audio,
            last_hover_audio_request: None,
            last_scrub_audio_request: None,
            last_video_decode_request: None,
            waveform_tx,
            waveform_rx,
            video_decode,
            forward: None,
            shadow: None,
            reverse: None,
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
            last_decoded_frame: None,
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
            let last_frame_time = self.last_pipeline_frame_time();
            let fwd_frame_delivered = self.pipeline_frame_delivered();
            let fwd_started_at = self.forward.as_ref().map(|f| f.started_at);
            let rev_last_frame_time = self.reverse.as_ref().and_then(|r| r.last_frame_time);
            let rev_started_at = self.reverse.as_ref().map(|r| r.started_at);
            let recent_pipeline_frame =
                last_frame_time.is_some_and(|t| (now - t) <= PIPELINE_STALL_THRESHOLD_S);
            let pipeline_delivering = (self.forward.is_some() || self.reverse.is_some())
                && fwd_frame_delivered
                && recent_pipeline_frame;
            let reverse_startup_hold = self.state.project.playback.state
                == PlaybackState::PlayingReverse
                && self.reverse.is_some()
                && rev_last_frame_time.is_none()
                && rev_started_at.is_some_and(|t| (now - t) <= FORWARD_STARTUP_GRACE_S);
            let is_playing_forward = self.state.project.playback.state == PlaybackState::Playing;
            let is_playing_reverse =
                self.state.project.playback.state == PlaybackState::PlayingReverse;
            let should_use_clock_advance =
                (!pipeline_delivering || is_playing_forward || is_playing_reverse)
                    && !reverse_startup_hold
                    && self.state.project.playback.state != PlaybackState::Stopped;
            if should_use_clock_advance {
                self.state.project.playback.advance(dt, duration);
            }
            if DEBUG_PLAYBACK
                && is_playing_forward
                && !fwd_frame_delivered
                && self.forward.is_some()
            {
                let elapsed_ms = fwd_started_at.map(|t| (now - t) * 1000.0).unwrap_or(0.0);
                eprintln!("[DBG] clock advancing before first frame, elapsed={elapsed_ms:.1}ms");
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

        self.manage_pipeline(now, ctx);
        self.manage_shadow_pipeline(now);
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

        let mut action = wizard_ui::browser::BrowserAction::None;
        egui::SidePanel::left("browser_panel")
            .width_range(200.0..=1200.0)
            .default_width(425.0)
            .show(ctx, |ui| {
                action = wizard_ui::browser::browser_panel(ui, &mut self.state, &self.textures);
            });
        match action {
            wizard_ui::browser::BrowserAction::None => {}
            wizard_ui::browser::BrowserAction::ImportFolder(path) => {
                self.import_folder(path);
            }
        }

        self.enqueue_visible_previews();
        self.update_hover_audio();
        self.update_timeline_scrub_audio();
        self.update_playback_frame(now);

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
                        .forward
                        .as_ref()
                        .map(|f| f.timeline_clip)
                        .or(self.reverse.as_ref().map(|r| r.timeline_clip));
                    if let Some(timeline_clip_id) = active_timeline_clip {
                        if let Some((_, _, tc)) =
                            self.state.project.timeline.find_clip(timeline_clip_id)
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

                    if self.shadow.is_some() {
                        ui.label("Shadow: active");
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

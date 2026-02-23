mod audio_mixer;
mod channel_polling;
mod constants;
mod debug_log;
mod import;
pub mod pipeline;
mod playback;
mod playback_engine;
pub mod texture_cache;
pub mod workers;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use notify::RecommendedWatcher;
use wizard_audio::output::AudioOutput;
use wizard_media::metadata::MediaMetadata;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

use pipeline::PipelineStatus;
use playback_engine::PlaybackEngine;
use texture_cache::TextureCache;
use workers::preview_worker::PreviewWorkerChannels;
use workers::scrub_cache_worker::ScrubCacheWorkerChannels;

use crate::constants::*;

static CLOCK_ADVANCE_LOG_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static PLAYHEAD_JUMP_LOG_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub struct EditorApp {
    state: AppState,
    textures: TextureCache,
    playback: PlaybackEngine,

    thumb_tx: mpsc::Sender<(ClipId, image::RgbaImage)>,
    thumb_rx: mpsc::Receiver<(ClipId, image::RgbaImage)>,
    meta_tx: mpsc::Sender<(ClipId, MediaMetadata)>,
    meta_rx: mpsc::Receiver<(ClipId, MediaMetadata)>,
    preview: PreviewWorkerChannels,
    scrub_cache: ScrubCacheWorkerChannels,
    waveform_tx: mpsc::Sender<(ClipId, Vec<(f32, f32)>)>,
    waveform_rx: mpsc::Receiver<(ClipId, Vec<(f32, f32)>)>,

    folder_watcher: Option<RecommendedWatcher>,
    watch_rx: mpsc::Receiver<PathBuf>,
    watch_tx: mpsc::Sender<PathBuf>,
    known_paths: HashSet<PathBuf>,

    last_frame_time: Option<f64>,
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
        let scrub_cache = workers::scrub_cache_worker::spawn_scrub_cache_worker();

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
        let (watch_tx, watch_rx) = mpsc::channel::<PathBuf>();

        let audio_producer = Arc::new(Mutex::new(audio_producer));

        let playback = PlaybackEngine::new(
            audio_output,
            audio_producer,
            audio_sample_rate,
            audio_channels,
            no_audio_paths,
        );

        Self {
            state: AppState::default(),
            textures: TextureCache::default(),
            playback,
            thumb_tx,
            thumb_rx,
            meta_tx,
            meta_rx,
            preview,
            scrub_cache,
            waveform_tx,
            waveform_rx,
            folder_watcher: None,
            watch_rx,
            watch_tx,
            known_paths: HashSet::new(),
            last_frame_time: None,
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let was_playing = self.playback.last_is_playing;
        let previous_playback_state = self.playback.last_playback_state;
        let now = ctx.input(|i| i.time);
        if let Some(last) = self.last_frame_time {
            let dt = now - last;
            let duration = self.state.project.timeline.timeline_duration();
            let fwd_frame_delivered = self.playback.pipeline_frame_delivered();
            let fwd_started_at = self.playback.forward.as_ref().map(|f| f.started_at);
            let fwd_status = self.playback.forward.as_ref().map(|f| f.stall_status(now));
            let rev_status = self.playback.reverse.as_ref().map(|r| r.stall_status(now));
            let pipeline_delivering = (self.playback.forward.is_some()
                || self.playback.reverse.is_some())
                && fwd_frame_delivered
                && fwd_status == Some(PipelineStatus::Delivering);
            let reverse_startup_hold = self.state.project.playback.state
                == PlaybackState::PlayingReverse
                && rev_status == Some(PipelineStatus::StartingUp);
            let is_playing_forward = self.state.project.playback.state == PlaybackState::Playing;
            let is_playing_reverse =
                self.state.project.playback.state == PlaybackState::PlayingReverse;
            let forward_startup_hold = is_playing_forward
                && self.playback.forward.is_some()
                && !fwd_frame_delivered
                && fwd_started_at.is_some_and(|t| now - t <= FORWARD_STARTUP_LONG_GRACE_S);
            let should_use_clock_advance =
                (!pipeline_delivering || is_playing_forward || is_playing_reverse)
                    && !reverse_startup_hold
                    && !forward_startup_hold
                    && self.state.project.playback.state != PlaybackState::Stopped;
            if should_use_clock_advance {
                self.state.project.playback.advance(dt, duration);
            }
            if DEBUG_PLAYBACK
                && is_playing_forward
                && !fwd_frame_delivered
                && self.playback.forward.is_some()
            {
                let elapsed_ms = fwd_started_at.map(|t| (now - t) * 1000.0).unwrap_or(0.0);
                eprintln!("[DBG] clock advancing before first frame, elapsed={elapsed_ms:.1}ms");

                if CLOCK_ADVANCE_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 10 {
                    crate::debug_log::emit(
                        "H5",
                        "crates/app/src/lib.rs:update",
                        "clock advanced before first pipeline frame",
                        serde_json::json!({
                            "elapsedMs": elapsed_ms,
                            "dt": dt,
                            "playhead": self.state.project.playback.playhead,
                            "pipelineDelivering": pipeline_delivering,
                            "forwardPresent": self.playback.forward.is_some()
                        }),
                    );
                }
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
        self.playback.handle_playback_state_transition(
            previous_playback_state,
            self.state.project.playback.state,
        );

        self.playback
            .manage_pipeline(&mut self.state, &mut self.textures, now, ctx);
        self.playback.manage_shadow_pipeline(&mut self.state, now);
        self.poll_import_tasks(ctx);
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
        self.enqueue_scrub_cache_for_timeline_clips();
        self.playback
            .update_hover_audio(&self.state, &self.textures);
        self.playback.update_timeline_scrub_audio(&self.state);
        self.playback
            .update_playback_frame(&mut self.state, &mut self.textures, now);

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
                    if let Some((pts, direction)) = self.playback.last_decoded_frame {
                        ui.label(format!("Decode {direction} pts: {pts:.3}"));
                    } else {
                        ui.label("Decode pts: -");
                    }

                    let active_timeline_clip = self
                        .playback
                        .forward
                        .as_ref()
                        .map(|f| f.timeline_clip)
                        .or(self.playback.reverse.as_ref().map(|r| r.timeline_clip));
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

                    if self.playback.shadow.is_some() {
                        ui.label("Shadow: active");
                    }
                });
        }

        let is_playing = self.playback.is_playing(&self.state);
        if was_playing && !is_playing {
            self.playback.handle_playback_stop_transition();
        }

        let current_playhead = self.state.project.playback.playhead;
        if self.state.project.playback.state == PlaybackState::Playing {
            let rewind = self.playback.last_playhead_observed - current_playhead;
            if rewind > 0.05
                && PLAYHEAD_JUMP_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 30
            {
                crate::debug_log::emit(
                    "H8",
                    "crates/app/src/lib.rs:update",
                    "playhead moved backward during forward playback",
                    serde_json::json!({
                        "previousPlayhead": self.playback.last_playhead_observed,
                        "currentPlayhead": current_playhead,
                        "rewindSeconds": rewind,
                        "scrubbing": self.state.ui.timeline.scrubbing,
                        "forwardPresent": self.playback.forward.is_some(),
                        "reversePresent": self.playback.reverse.is_some(),
                        "lastDecodedFrame": self.playback.last_decoded_frame.map(|(pts, src)| serde_json::json!({"pts": pts, "source": src})).unwrap_or(serde_json::Value::Null)
                    }),
                );
            }
        }
        self.playback.last_playhead_observed = current_playhead;
        self.playback.last_is_playing = is_playing;
        self.playback.last_playback_state = self.state.project.playback.state;

        if let Some(received) =
            Some(
                self.playback
                    .poll_pipeline_frames(&mut self.state, &mut self.textures, ctx, now),
            )
        {
            if received {
                ctx.request_repaint();
            }
        }

        if self.state.project.playback.state != PlaybackState::Stopped {
            ctx.request_repaint();
        }
    }
}

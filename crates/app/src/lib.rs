use std::sync::mpsc;
use std::time::{Duration, Instant};

use wizard_audio::output::AudioOutput;
use wizard_media::metadata::MediaMetadata;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

enum AudioPreviewRequest {
    Stop,
    Preview {
        clip_id: ClipId,
        path: std::path::PathBuf,
        time_seconds: f64,
        sample_rate_hz: u32,
    },
}

struct AudioSnippet {
    clip_id: ClipId,
    samples_mono: Vec<f32>,
}

pub struct EditorApp {
    state: AppState,
    last_frame_time: Option<f64>,
    thumb_tx: mpsc::Sender<(ClipId, image::RgbaImage)>,
    thumb_rx: mpsc::Receiver<(ClipId, image::RgbaImage)>,
    meta_tx: mpsc::Sender<(ClipId, MediaMetadata)>,
    meta_rx: mpsc::Receiver<(ClipId, MediaMetadata)>,
    preview_tx: mpsc::Sender<(ClipId, Vec<image::RgbaImage>)>,
    preview_rx: mpsc::Receiver<(ClipId, Vec<image::RgbaImage>)>,
    audio_output: Option<AudioOutput>,
    audio_req_tx: mpsc::Sender<AudioPreviewRequest>,
    audio_snippet_rx: mpsc::Receiver<AudioSnippet>,
    last_audio_request: Option<(ClipId, i64)>,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        wizard_ui::theme::apply_theme(&cc.egui_ctx);
        let (thumb_tx, thumb_rx) = mpsc::channel();
        let (meta_tx, meta_rx) = mpsc::channel();
        let (preview_tx, preview_rx) = mpsc::channel();

        let audio_output = AudioOutput::new().ok();
        let (audio_req_tx, audio_req_rx) = mpsc::channel();
        let (audio_snippet_tx, audio_snippet_rx) = mpsc::channel();

        std::thread::spawn(move || {
            let mut last_decode_started_at = Instant::now() - Duration::from_secs(1);
            loop {
                let Ok(mut req) = audio_req_rx.recv() else {
                    return;
                };
                while let Ok(next) = audio_req_rx.try_recv() {
                    req = next;
                }

                match req {
                    AudioPreviewRequest::Stop => {}
                    AudioPreviewRequest::Preview {
                        clip_id,
                        path,
                        time_seconds,
                        sample_rate_hz,
                    } => {
                        let min_interval = Duration::from_millis(80);
                        if last_decode_started_at.elapsed() < min_interval {
                            continue;
                        }
                        last_decode_started_at = Instant::now();

                        let snippet_duration = 0.35;
                        let start_seconds = (time_seconds - 0.15).max(0.0);
                        let samples_mono = wizard_media::audio::decode_pcm_snippet_f32_mono(
                            &path,
                            start_seconds,
                            snippet_duration,
                            sample_rate_hz,
                        );
                        let _ = audio_snippet_tx.send(AudioSnippet {
                            clip_id,
                            samples_mono,
                        });
                    }
                }
            }
        });

        Self {
            state: AppState::default(),
            last_frame_time: None,
            thumb_tx,
            thumb_rx,
            meta_tx,
            meta_rx,
            preview_tx,
            preview_rx,
            audio_output,
            audio_req_tx,
            audio_snippet_rx,
            last_audio_request: None,
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
            self.state.thumbnails.insert(id, texture);
            self.state.pending_thumbnails.remove(&id);
            received = true;
        }

        while let Ok((id, meta)) = self.meta_rx.try_recv() {
            if let Some(clip) = self.state.clips.get_mut(&id) {
                clip.duration = meta.duration;
                clip.resolution = meta.resolution;
                clip.codec = meta.codec;
            }
            received = true;
        }

        while let Ok((id, images)) = self.preview_rx.try_recv() {
            let textures: Vec<egui::TextureHandle> = images
                .iter()
                .enumerate()
                .map(|(i, img)| {
                    ctx.load_texture(
                        format!("preview_{:?}_{}", id, i),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [img.width() as usize, img.height() as usize],
                            img.as_raw(),
                        ),
                        egui::TextureOptions::LINEAR,
                    )
                })
                .collect();
            self.state.preview_frames.insert(id, textures);
            received = true;
        }

        while let Ok(snippet) = self.audio_snippet_rx.try_recv() {
            if self.state.selection.hovered_clip == Some(snippet.clip_id) {
                if let Some(out) = &self.audio_output {
                    out.clear();
                    out.enqueue_mono_samples(&snippet.samples_mono);
                }
            }
        }

        if received {
            ctx.request_repaint();
        }
    }

    fn update_hover_audio(&mut self) {
        let Some(out) = &self.audio_output else {
            return;
        };

        let Some(clip_id) = self.state.selection.hovered_clip else {
            if self.last_audio_request.take().is_some() {
                out.clear();
                let _ = self.audio_req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let Some(t_norm) = self.state.hovered_scrub_t else {
            if self.last_audio_request.take().is_some() {
                out.clear();
                let _ = self.audio_req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let Some(clip) = self.state.clips.get(&clip_id) else {
            return;
        };
        let Some(duration) = clip.duration else {
            return;
        };

        let time_seconds = (t_norm.clamp(0.0, 1.0) as f64 * duration).clamp(0.0, duration);
        let bucket = (time_seconds * 10.0).round() as i64;
        if self.last_audio_request == Some((clip_id, bucket)) {
            return;
        }
        self.last_audio_request = Some((clip_id, bucket));

        let _ = self.audio_req_tx.send(AudioPreviewRequest::Preview {
            clip_id,
            path: clip.path.clone(),
            time_seconds,
            sample_rate_hz: out.sample_rate_hz(),
        });
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);
        if let Some(last) = self.last_frame_time {
            let dt = now - last;
            self.state.playback.advance(dt);
            if dt > 0.0 {
                let inst_fps = (1.0 / dt) as f32;
                if self.state.fps <= 0.0 {
                    self.state.fps = inst_fps;
                } else {
                    self.state.fps = self.state.fps * 0.9 + inst_fps * 0.1;
                }
            }
        }
        self.last_frame_time = Some(now);

        self.poll_background_tasks(ctx);
        handle_keyboard(ctx, &mut self.state);

        let thumb_tx = self.thumb_tx.clone();
        let meta_tx = self.meta_tx.clone();
        let preview_tx = self.preview_tx.clone();

        egui::TopBottomPanel::top("top_panel")
            .exact_height(28.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.state.show_fps, "FPS");
                    if self.state.show_fps {
                        ui.label(format!("{:.1}", self.state.fps));
                    }
                });
            });

        egui::SidePanel::left("browser_panel")
            .width_range(48.0..=1200.0)
            .default_width(300.0)
            .show(ctx, |ui| {
                wizard_ui::browser::browser_panel(
                    ui,
                    &mut self.state,
                    &thumb_tx,
                    &meta_tx,
                    &preview_tx,
                );
            });

        self.update_hover_audio();

        egui::TopBottomPanel::bottom("timeline_panel")
            .resizable(true)
            .height_range(120.0..=400.0) // pick a max that makes sense
            .default_height(220.0)
            .show(ctx, |ui| {
                wizard_ui::timeline::timeline_panel(ui, &mut self.state);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            wizard_ui::preview::preview_panel(ui, &self.state);
        });

        if self.state.show_fps {
            egui::Area::new(egui::Id::new("fps_overlay"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
                .show(ctx, |ui| {
                    ui.label(format!("{:.1} fps", self.state.fps));
                });
        }

        if self.state.playback.state != PlaybackState::Stopped {
            ctx.request_repaint();
        }
    }
}

fn handle_keyboard(ctx: &egui::Context, state: &mut AppState) {
    ctx.input(|i| {
        if i.key_pressed(egui::Key::L) {
            state.playback.state = PlaybackState::Playing;
        }
        if i.key_pressed(egui::Key::J) {
            state.playback.play_reverse();
        }
        if i.key_pressed(egui::Key::K) {
            state.playback.stop();
        }
        if i.key_pressed(egui::Key::Space) {
            state.playback.toggle_play();
        }
    });
}

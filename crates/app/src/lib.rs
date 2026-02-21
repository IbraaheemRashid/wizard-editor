use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wizard_audio::output::AudioOutput;
use wizard_media::metadata::MediaMetadata;
use wizard_state::clip::ClipId;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

enum PreviewRequest {
    Enqueue {
        clip_id: ClipId,
        path: std::path::PathBuf,
        priority: bool,
    },
}

fn apply_preview_req(
    req: PreviewRequest,
    queue: &mut std::collections::VecDeque<(ClipId, std::path::PathBuf)>,
    queued: &mut std::collections::HashSet<ClipId>,
) {
    match req {
        PreviewRequest::Enqueue {
            clip_id,
            path,
            priority,
        } => {
            if queued.contains(&clip_id) {
                return;
            }
            queued.insert(clip_id);
            if priority {
                queue.push_front((clip_id, path));
            } else {
                queue.push_back((clip_id, path));
            }
        }
    }
}

enum AudioPreviewRequest {
    Stop,
    Preview {
        path: std::path::PathBuf,
        time_seconds: f64,
        sample_rate_hz: u32,
    },
}

struct AudioSnippet {
    samples_mono: Vec<f32>,
}

#[derive(Default)]
pub struct TextureCache {
    pub thumbnails: HashMap<ClipId, egui::TextureHandle>,
    pub preview_frames: HashMap<ClipId, Vec<egui::TextureHandle>>,
    pub pending_thumbnails: HashSet<ClipId>,
    pub preview_requested: HashSet<ClipId>,
    pub waveforms: HashMap<ClipId, Vec<(f32, f32)>>,
}

impl wizard_ui::browser::TextureLookup for TextureCache {
    fn thumbnail(&self, id: &ClipId) -> Option<&egui::TextureHandle> {
        self.thumbnails.get(id)
    }

    fn preview_frames(&self, id: &ClipId) -> Option<&Vec<egui::TextureHandle>> {
        self.preview_frames.get(id)
    }

    fn is_pending(&self, id: &ClipId) -> bool {
        self.pending_thumbnails.contains(id)
    }

    fn waveform_peaks(&self, id: &ClipId) -> Option<&Vec<(f32, f32)>> {
        self.waveforms.get(id)
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
    preview_rx: mpsc::Receiver<(ClipId, Vec<image::RgbaImage>)>,
    preview_req_tx: mpsc::Sender<PreviewRequest>,
    audio_output: Option<AudioOutput>,
    audio_req_tx: mpsc::Sender<AudioPreviewRequest>,
    audio_snippet_rx: mpsc::Receiver<AudioSnippet>,
    last_audio_request: Option<(ClipId, i64)>,
    waveform_tx: mpsc::Sender<(ClipId, Vec<(f32, f32)>)>,
    waveform_rx: mpsc::Receiver<(ClipId, Vec<(f32, f32)>)>,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        wizard_ui::theme::apply_theme(&cc.egui_ctx);
        let (thumb_tx, thumb_rx) = mpsc::channel();
        let (meta_tx, meta_rx) = mpsc::channel();
        let (preview_result_tx, preview_rx) = mpsc::channel();
        let (preview_req_tx, preview_req_rx) = mpsc::channel();

        std::thread::spawn(move || {
            use std::collections::{HashSet, VecDeque};

            let mut queue: VecDeque<(ClipId, std::path::PathBuf)> = VecDeque::new();
            let mut queued: HashSet<ClipId> = HashSet::new();

            loop {
                let (clip_id, path) = if let Some(item) = queue.pop_front() {
                    queued.remove(&item.0);
                    item
                } else {
                    let Ok(req) = preview_req_rx.recv() else {
                        return;
                    };
                    apply_preview_req(req, &mut queue, &mut queued);
                    continue;
                };

                while let Ok(req) = preview_req_rx.try_recv() {
                    apply_preview_req(req, &mut queue, &mut queued);
                }

                let frames = wizard_media::thumbnail::extract_preview_frames(&path, 32);
                let _ = preview_result_tx.send((clip_id, frames));
            }
        });

        let (waveform_tx, waveform_rx) = mpsc::channel::<(ClipId, Vec<(f32, f32)>)>();

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
                        path,
                        time_seconds,
                        sample_rate_hz,
                        ..
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
                        let _ = audio_snippet_tx.send(AudioSnippet { samples_mono });
                    }
                }
            }
        });

        Self {
            state: AppState::default(),
            textures: TextureCache::default(),
            last_frame_time: None,
            thumb_tx,
            thumb_rx,
            meta_tx,
            meta_rx,
            preview_rx,
            preview_req_tx,
            audio_output,
            audio_req_tx,
            audio_snippet_rx,
            last_audio_request: None,
            waveform_tx,
            waveform_rx,
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

        while let Ok((id, images)) = self.preview_rx.try_recv() {
            if images.is_empty() {
                self.textures.preview_requested.remove(&id);
                continue;
            }
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
            self.textures.preview_frames.insert(id, textures);
            received = true;
        }

        while let Ok((id, peaks)) = self.waveform_rx.try_recv() {
            self.textures.waveforms.insert(id, peaks);
            received = true;
        }

        while let Ok(snippet) = self.audio_snippet_rx.try_recv() {
            if let Some(out) = &self.audio_output {
                out.clear();
                out.enqueue_mono_samples(&snippet.samples_mono);
            }
        }

        if received {
            ctx.request_repaint();
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

        let visible: Vec<ClipId> = self.state.ui.visible_clips.clone();
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
        let _ = self.preview_req_tx.send(PreviewRequest::Enqueue {
            clip_id,
            path: clip.path.clone(),
            priority,
        });
        true
    }

    fn update_hover_audio(&mut self) {
        let Some(out) = &self.audio_output else {
            return;
        };

        let Some(clip_id) = self.state.ui.selection.hovered_clip else {
            if self.state.ui.timeline_scrubbing.is_none()
                && self.last_audio_request.take().is_some()
            {
                out.clear();
                let _ = self.audio_req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let Some(t_norm) = self.state.ui.hovered_scrub_t else {
            if self.state.ui.timeline_scrubbing.is_none()
                && self.last_audio_request.take().is_some()
            {
                out.clear();
                let _ = self.audio_req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let Some(clip) = self.state.project.clips.get(&clip_id) else {
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
            path: clip.path.clone(),
            time_seconds,
            sample_rate_hz: out.sample_rate_hz(),
        });
    }

    fn update_timeline_scrub_audio(&mut self) {
        let Some(out) = &self.audio_output else {
            return;
        };

        let Some(time) = self.state.ui.timeline_scrubbing else {
            return;
        };

        if self.state.ui.selection.hovered_clip.is_some() {
            return;
        }

        let mut found_clip_id = None;
        let mut found_path = None;
        let mut found_time = 0.0;

        for track in &self.state.project.tracks {
            for tc in &track.clips {
                if time >= tc.position && time < tc.position + tc.duration {
                    let clip_time = tc.in_point + (time - tc.position);
                    if let Some(clip) = self.state.project.clips.get(&tc.clip_id) {
                        found_clip_id = Some(tc.clip_id);
                        found_path = Some(clip.path.clone());
                        found_time = clip_time;
                        break;
                    }
                }
            }
            if found_clip_id.is_some() {
                break;
            }
        }

        let Some(clip_id) = found_clip_id else {
            return;
        };
        let Some(path) = found_path else {
            return;
        };

        let bucket = (found_time * 10.0).round() as i64;
        if self.last_audio_request == Some((clip_id, bucket)) {
            return;
        }
        self.last_audio_request = Some((clip_id, bucket));

        let _ = self.audio_req_tx.send(AudioPreviewRequest::Preview {
            path,
            time_seconds: found_time,
            sample_rate_hz: out.sample_rate_hz(),
        });
    }

    fn import_folder(&mut self, path: std::path::PathBuf) {
        let files = wizard_media::import::scan_folder(&path);
        for p in files {
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
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);
        if let Some(last) = self.last_frame_time {
            let dt = now - last;
            self.state.project.playback.advance(dt);
            if dt > 0.0 {
                let inst_fps = (1.0 / dt) as f32;
                if self.state.ui.fps <= 0.0 {
                    self.state.ui.fps = inst_fps;
                } else {
                    self.state.ui.fps = self.state.ui.fps * 0.9 + inst_fps * 0.1;
                }
            }
        }
        self.last_frame_time = Some(now);

        self.poll_background_tasks(ctx);
        handle_keyboard(ctx, &mut self.state);

        egui::TopBottomPanel::top("top_panel")
            .exact_height(28.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.state.ui.show_fps, "FPS");
                    if self.state.ui.show_fps {
                        ui.label(format!("{:.1}", self.state.ui.fps));
                    }
                });
            });

        if self.state.ui.show_browser {
            let mut action = wizard_ui::browser::BrowserAction::None;
            egui::SidePanel::left("browser_panel")
                .width_range(200.0..=1200.0)
                .default_width(420.0)
                .show(ctx, |ui| {
                    action = wizard_ui::browser::browser_panel(ui, &mut self.state, &self.textures);
                });
            match action {
                wizard_ui::browser::BrowserAction::None => {}
                wizard_ui::browser::BrowserAction::Collapse => {
                    self.state.ui.show_browser = false;
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
                            self.state.ui.show_browser = true;
                        }
                    });
                });
        }

        self.enqueue_visible_previews();
        self.update_hover_audio();
        self.update_timeline_scrub_audio();

        egui::TopBottomPanel::bottom("timeline_panel")
            .resizable(true)
            .height_range(40.0..=800.0)
            .default_height(250.0)
            .show(ctx, |ui| {
                wizard_ui::timeline::timeline_panel(ui, &mut self.state, &self.textures);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            wizard_ui::preview::preview_panel(ui, &self.state, &self.textures);
        });

        if self.state.ui.show_fps {
            egui::Area::new(egui::Id::new("fps_overlay"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
                .show(ctx, |ui| {
                    ui.label(format!("{:.1} fps", self.state.ui.fps));
                });
        }

        if self.state.project.playback.state != PlaybackState::Stopped {
            ctx.request_repaint();
        }
    }
}

fn handle_keyboard(ctx: &egui::Context, state: &mut AppState) {
    ctx.input(|i| {
        if i.key_pressed(egui::Key::L) {
            state.project.playback.state = PlaybackState::Playing;
        }
        if i.key_pressed(egui::Key::J) {
            state.project.playback.play_reverse();
        }
        if i.key_pressed(egui::Key::K) {
            state.project.playback.stop();
        }
        if i.key_pressed(egui::Key::Space) {
            state.project.playback.toggle_play();
        }
    });
}

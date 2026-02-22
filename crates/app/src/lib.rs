use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::mpsc;

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
    StartStream {
        path: std::path::PathBuf,
        start_seconds: f64,
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
    pub playback_texture: Option<egui::TextureHandle>,
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
    preview_rx: mpsc::Receiver<(ClipId, Vec<image::RgbaImage>)>,
    preview_req_tx: mpsc::Sender<PreviewRequest>,
    audio_output: Option<AudioOutput>,
    audio_req_tx: mpsc::Sender<AudioPreviewRequest>,
    audio_snippet_rx: mpsc::Receiver<AudioSnippet>,
    last_audio_request: Option<(ClipId, i64)>,
    waveform_tx: mpsc::Sender<(ClipId, Vec<(f32, f32)>)>,
    waveform_rx: mpsc::Receiver<(ClipId, Vec<(f32, f32)>)>,
    playback_frame_req_tx: mpsc::Sender<(ClipId, std::path::PathBuf, f64)>,
    playback_frame_rx: mpsc::Receiver<(ClipId, f64, image::RgbaImage)>,
    last_playback_frame_request: Option<f64>,
    playback_audio_clip: Option<ClipId>,
    no_audio_paths: Arc<Mutex<HashSet<std::path::PathBuf>>>,
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

        let (playback_frame_req_tx, playback_frame_req_rx) =
            mpsc::channel::<(ClipId, std::path::PathBuf, f64)>();
        let (playback_frame_result_tx, playback_frame_rx) =
            mpsc::channel::<(ClipId, f64, image::RgbaImage)>();

        std::thread::spawn(move || {
            let mut cached_decoder: Option<(
                std::path::PathBuf,
                wizard_media::decoder::VideoDecoder,
            )> = None;
            loop {
                let Ok(mut req) = playback_frame_req_rx.recv() else {
                    return;
                };
                while let Ok(next) = playback_frame_req_rx.try_recv() {
                    req = next;
                }
                let (clip_id, path, time) = req;

                let needs_new = cached_decoder.as_ref().is_none_or(|(p, _)| p != &path);

                if needs_new {
                    cached_decoder = wizard_media::decoder::VideoDecoder::open(&path)
                        .ok()
                        .map(|d| (path.clone(), d));
                }

                if let Some((_, ref mut decoder)) = cached_decoder {
                    let can_sequential = decoder.last_decode_time().is_some_and(|last| {
                        let diff = time - last;
                        diff > 0.0 && diff < 0.5
                    });

                    let img = if can_sequential {
                        decoder.decode_next_frame(960, 540)
                    } else {
                        decoder.seek_and_decode(time, 960, 540)
                    };

                    if let Some(img) = img {
                        let _ = playback_frame_result_tx.send((clip_id, time, img));
                    }
                }
            }
        });

        let audio_output = AudioOutput::new().ok();
        let (audio_req_tx, audio_req_rx) = mpsc::channel();
        let (audio_snippet_tx, audio_snippet_rx) = mpsc::channel();

        let no_audio_paths: Arc<Mutex<HashSet<std::path::PathBuf>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let no_audio_paths_for_thread = no_audio_paths.clone();

        std::thread::spawn(move || {
            let mut cached_decoder: Option<(
                std::path::PathBuf,
                wizard_media::decoder::AudioDecoder,
            )> = None;

            let open_decoder = |path: &std::path::Path,
                                no_audio: &Arc<Mutex<HashSet<std::path::PathBuf>>>|
             -> Option<wizard_media::decoder::AudioDecoder> {
                match wizard_media::decoder::AudioDecoder::open(path) {
                    Ok(d) => Some(d),
                    Err(_err) => {
                        if let Ok(streams) = wizard_media::decoder::probe_streams(path) {
                            let has_audio_stream = streams.iter().any(|s| s.medium == "Audio");
                            if !has_audio_stream {
                                if let Ok(mut set) = no_audio.lock() {
                                    set.insert(path.to_path_buf());
                                }
                            }
                        }
                        None
                    }
                }
            };

            let ensure_decoder =
                |cached: &mut Option<(std::path::PathBuf, wizard_media::decoder::AudioDecoder)>,
                 path: &std::path::Path,
                 no_audio: &Arc<Mutex<HashSet<std::path::PathBuf>>>| {
                    let needs_new = cached.as_ref().is_none_or(|(p, _)| p != path);
                    if needs_new {
                        *cached = open_decoder(path, no_audio)
                            .map(|d| (path.to_path_buf(), d));
                    }
                };

            loop {
                let Ok(req) = audio_req_rx.recv() else {
                    return;
                };

                match req {
                    AudioPreviewRequest::Stop => {
                        while audio_req_rx.try_recv().is_ok() {}
                    }
                    AudioPreviewRequest::Preview {
                        path,
                        time_seconds,
                        sample_rate_hz,
                    } => {
                        if no_audio_paths_for_thread
                            .lock()
                            .map(|set| set.contains(&path))
                            .unwrap_or(false)
                        {
                            continue;
                        }

                        ensure_decoder(
                            &mut cached_decoder,
                            &path,
                            &no_audio_paths_for_thread,
                        );

                        if let Some((_, ref mut decoder)) = cached_decoder {
                            let samples = decoder.decode_range_mono_f32(
                                time_seconds.max(0.0),
                                0.5,
                                sample_rate_hz,
                            );
                            let _ = audio_snippet_tx.send(AudioSnippet {
                                samples_mono: samples,
                            });
                        }
                    }
                    AudioPreviewRequest::StartStream {
                        path,
                        start_seconds,
                        sample_rate_hz,
                    } => {
                        if no_audio_paths_for_thread
                            .lock()
                            .map(|set| set.contains(&path))
                            .unwrap_or(false)
                        {
                            continue;
                        }

                        ensure_decoder(
                            &mut cached_decoder,
                            &path,
                            &no_audio_paths_for_thread,
                        );

                        if let Some((_, ref mut decoder)) = cached_decoder {
                            decoder.seek_to(start_seconds);
                        }

                        let mut streaming_rate = sample_rate_hz;
                        loop {
                            let chunk = match cached_decoder {
                                Some((_, ref mut decoder)) => {
                                    decoder.decode_chunk_mono_f32(0.5, streaming_rate)
                                }
                                None => break,
                            };
                            if chunk.is_empty() {
                                break;
                            }
                            let _ = audio_snippet_tx.send(AudioSnippet {
                                samples_mono: chunk,
                            });

                            match audio_req_rx.try_recv() {
                                Ok(AudioPreviewRequest::Stop) => {
                                    while audio_req_rx.try_recv().is_ok() {}
                                    break;
                                }
                                Ok(AudioPreviewRequest::StartStream {
                                    path: new_path,
                                    start_seconds: new_start,
                                    sample_rate_hz: new_rate,
                                }) => {
                                    ensure_decoder(
                                        &mut cached_decoder,
                                        &new_path,
                                        &no_audio_paths_for_thread,
                                    );
                                    if let Some((_, ref mut dec)) = cached_decoder {
                                        dec.seek_to(new_start);
                                        streaming_rate = new_rate;
                                    } else {
                                        break;
                                    }
                                }
                                Ok(AudioPreviewRequest::Preview {
                                    path: p,
                                    time_seconds: t,
                                    sample_rate_hz: sr,
                                }) => {
                                    ensure_decoder(
                                        &mut cached_decoder,
                                        &p,
                                        &no_audio_paths_for_thread,
                                    );
                                    if let Some((_, ref mut dec)) = cached_decoder {
                                        let samples =
                                            dec.decode_range_mono_f32(t.max(0.0), 0.5, sr);
                                        let _ = audio_snippet_tx.send(AudioSnippet {
                                            samples_mono: samples,
                                        });
                                    }
                                    break;
                                }
                                Err(_) => {}
                            }
                        }
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
            playback_frame_req_tx,
            playback_frame_rx,
            last_playback_frame_request: None,
            playback_audio_clip: None,
            no_audio_paths,
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

        while let Ok((_clip_id, _time, img)) = self.playback_frame_rx.try_recv() {
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

        while let Ok(snippet) = self.audio_snippet_rx.try_recv() {
            if let Some(out) = &self.audio_output {
                let is_playing = self.state.project.playback.state == PlaybackState::Playing;
                if !is_playing {
                    out.clear();
                }
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

        let is_playing = self.state.project.playback.state == PlaybackState::Playing;

        let Some(clip_id) = self.state.ui.selection.hovered_clip else {
            if !is_playing
                && self.state.ui.timeline_scrubbing.is_none()
                && self.last_audio_request.take().is_some()
            {
                out.clear();
                let _ = self.audio_req_tx.send(AudioPreviewRequest::Stop);
            }
            return;
        };

        let Some(t_norm) = self.state.ui.hovered_scrub_t else {
            if !is_playing
                && self.state.ui.timeline_scrubbing.is_none()
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
        if self
            .no_audio_paths
            .lock()
            .map(|set| set.contains(&clip.path))
            .unwrap_or(false)
        {
            return;
        }
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

        if let Some(hit) = self.state.project.timeline.audio_clip_at_time(time) {
            if let Some(clip) = self.state.project.clips.get(&hit.clip.source_id) {
                found_clip_id = Some(hit.clip.source_id);
                found_path = Some(clip.path.clone());
                found_time = hit.source_time;
            }
        }

        let Some(clip_id) = found_clip_id else {
            return;
        };
        let Some(path) = found_path else {
            return;
        };
        if self
            .no_audio_paths
            .lock()
            .map(|set| set.contains(&path))
            .unwrap_or(false)
        {
            return;
        }

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

    fn update_playback_audio(&mut self) {
        let Some(out) = &self.audio_output else {
            return;
        };

        if self.state.project.playback.state != PlaybackState::Playing {
            if self.playback_audio_clip.is_some() {
                out.clear();
                let _ = self.audio_req_tx.send(AudioPreviewRequest::Stop);
                self.playback_audio_clip = None;
            }
            return;
        }

        if self.state.ui.selection.hovered_clip.is_some() {
            return;
        }

        let playhead = self.state.project.playback.playhead;

        let hit = self.state.project.timeline.audio_clip_at_time(playhead);
        let Some(hit) = hit else {
            if self.playback_audio_clip.is_some() {
                out.clear();
                let _ = self.audio_req_tx.send(AudioPreviewRequest::Stop);
                self.playback_audio_clip = None;
            }
            return;
        };

        let clip_id = hit.clip.source_id;
        let source_time = hit.source_time;

        let Some(clip) = self.state.project.clips.get(&clip_id) else {
            return;
        };
        if self
            .no_audio_paths
            .lock()
            .map(|set| set.contains(&clip.path))
            .unwrap_or(false)
        {
            return;
        }

        if self.playback_audio_clip != Some(clip_id) {
            out.clear();
            self.playback_audio_clip = Some(clip_id);
            let _ = self.audio_req_tx.send(AudioPreviewRequest::StartStream {
                path: clip.path.clone(),
                start_seconds: source_time,
                sample_rate_hz: out.sample_rate_hz(),
            });
        }
    }

    fn update_playback_frame(&mut self) {
        let playhead = self.state.project.playback.playhead;
        let is_playing = self.state.project.playback.state != PlaybackState::Stopped;
        let is_scrubbing = self.state.ui.timeline_scrubbing.is_some();

        if !is_playing && !is_scrubbing {
            return;
        }

        let threshold = 0.02;
        if let Some(last) = self.last_playback_frame_request {
            if (playhead - last).abs() < threshold {
                return;
            }
        }

        let time = if is_scrubbing {
            self.state.ui.timeline_scrubbing.unwrap_or(playhead)
        } else {
            playhead
        };

        if let Some(hit) = self.state.project.timeline.clip_at_time(time) {
            if let Some(clip) = self.state.project.clips.get(&hit.clip.source_id) {
                self.last_playback_frame_request = Some(playhead);
                let _ = self.playback_frame_req_tx.send((
                    hit.clip.source_id,
                    clip.path.clone(),
                    hit.source_time,
                ));
            }
        }
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
            let duration = self.state.project.timeline.timeline_duration();
            self.state.project.playback.advance(dt, duration);
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
        self.update_playback_audio();
        self.update_playback_frame();

        egui::TopBottomPanel::bottom("timeline_panel")
            .resizable(true)
            .height_range(40.0..=800.0)
            .default_height(250.0)
            .show(ctx, |ui| {
                wizard_ui::timeline::timeline_panel(ui, &mut self.state, &self.textures);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            wizard_ui::preview::preview_panel(ui, &mut self.state, &self.textures);
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

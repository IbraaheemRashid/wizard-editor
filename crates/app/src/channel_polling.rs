use wizard_media::pipeline::DecodedFrame;
use wizard_state::playback::PlaybackState;

use crate::constants::*;
use crate::EditorApp;

static FIRST_FRAME_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl EditorApp {
    pub fn poll_background_tasks(&mut self, ctx: &egui::Context) {
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
                clip.audio_only = !meta.has_video;
                clip.rebuild_search_haystack(tag_mask);
                if !meta.has_video {
                    self.textures.pending_thumbnails.remove(&id);
                }
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

        let fwd_last_frame_time = self.forward.as_ref().and_then(|f| f.last_frame_time);
        let fwd_frame_delivered = self
            .forward
            .as_ref()
            .map(|f| f.frame_delivered)
            .unwrap_or(false);
        let fwd_started_at = self.forward.as_ref().map(|f| f.started_at);
        let rev_last_frame_time = self.reverse.as_ref().and_then(|r| r.last_frame_time);
        let rev_started_at = self.reverse.as_ref().map(|r| r.started_at);

        while let Ok(result) = self.video_decode.result_rx.try_recv() {
            let forward_frame_age = fwd_last_frame_time
                .map(|t| now - t)
                .unwrap_or(f64::INFINITY);
            let forward_startup_age = fwd_started_at.map(|t| now - t).unwrap_or(f64::INFINITY);
            let forward_frame_gap_stalled = fwd_last_frame_time
                .is_some_and(|_| fwd_frame_delivered && forward_frame_age > FRAME_GAP_STALL_S);
            let forward_frame_gap_long_stall = fwd_last_frame_time
                .is_some_and(|_| fwd_frame_delivered && forward_frame_age > FRAME_GAP_LONG_STALL_S);
            let forward_pipeline_stalled = self.forward.is_some()
                && self.state.project.playback.state == PlaybackState::Playing
                && (forward_frame_gap_stalled
                    || (!fwd_frame_delivered && forward_startup_age > FORWARD_STARTUP_GRACE_S));
            let forward_long_stall = self.forward.is_some()
                && self.state.project.playback.state == PlaybackState::Playing
                && (forward_frame_gap_long_stall
                    || (!fwd_frame_delivered
                        && forward_startup_age > FORWARD_STARTUP_LONG_GRACE_S));
            let reverse_startup_age = rev_started_at.map(|t| now - t).unwrap_or(f64::INFINITY);
            let reverse_pipeline_stalled = self.reverse.is_some()
                && self.state.project.playback.state == PlaybackState::PlayingReverse
                && (rev_last_frame_time.is_some_and(|t| (now - t) > FRAME_GAP_STALL_S)
                    || (rev_last_frame_time.is_none()
                        && reverse_startup_age > FORWARD_STARTUP_GRACE_S));
            let reverse_long_stall = self.reverse.is_some()
                && self.state.project.playback.state == PlaybackState::PlayingReverse
                && (rev_last_frame_time.is_some_and(|t| (now - t) > FRAME_GAP_LONG_STALL_S)
                    || (rev_last_frame_time.is_none()
                        && reverse_startup_age > FORWARD_STARTUP_LONG_GRACE_S));
            let current_source = self.last_decoded_frame.map(|(_, s)| s);
            let forward_awaiting_first = self.forward.is_some()
                && self.state.project.playback.state == PlaybackState::Playing
                && !fwd_frame_delivered;
            let reverse_awaiting_first = self.reverse.is_some()
                && self.state.project.playback.state == PlaybackState::PlayingReverse
                && rev_last_frame_time.is_none();
            let should_apply_fallback_texture = (self.state.project.playback.state
                == PlaybackState::Playing
                && (self.forward.is_none()
                    || forward_pipeline_stalled
                    || forward_awaiting_first))
                || (self.state.project.playback.state == PlaybackState::PlayingReverse
                    && (self.reverse.is_none()
                        || reverse_pipeline_stalled
                        || reverse_awaiting_first))
                || self.state.project.playback.state == PlaybackState::Stopped
                || self.state.ui.timeline.scrubbing.is_some();
            let should_preserve_pipeline_texture = (current_source == Some("fwd")
                && !forward_long_stall
                && !forward_awaiting_first)
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
            self.textures.update_playback_texture(
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
        if DEBUG_PLAYBACK && !pipeline_frames.is_empty() && !fwd_frame_delivered {
            let elapsed_ms = fwd_started_at.map(|t| (now - t) * 1000.0).unwrap_or(0.0);
            let pts = pipeline_frames[0].pts_seconds;
            eprintln!("[DBG] first pipeline frame arrived, latency={elapsed_ms:.1}ms, pts={pts:.3}");
            FIRST_FRAME_LOGGED.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        for frame in &pipeline_frames {
            received = true;
            if !self.apply_pipeline_frame(ctx, frame, now) {
                break;
            }
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
        for frame in &reverse_frames {
            received = true;
            if !self.apply_reverse_pipeline_frame(ctx, frame, now) {
                break;
            }
        }

        let mut last_snippet: Option<crate::workers::audio_worker::AudioSnippet> = None;
        while let Ok(snippet) = self.audio.snippet_rx.try_recv() {
            last_snippet = Some(snippet);
        }
        if let Some(snippet) = last_snippet {
            if !self.is_playing() {
                self.reset_audio_sources();
            }
            if let Ok(mut producer) = self.audio_producer.lock() {
                let ch = self.audio_channels;
                wizard_audio::output::enqueue_samples(&mut producer, &snippet.samples_mono, ch);
            }
        }

        self.mixer.mix_tick();

        if received {
            ctx.request_repaint();
        }
    }
}

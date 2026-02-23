use crate::EditorApp;

impl EditorApp {
    pub fn poll_import_tasks(&mut self, ctx: &egui::Context) {
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

        while let Ok(sf) = self.scrub_cache.result_rx.try_recv() {
            let texture = ctx.load_texture(
                format!("scrub_{:?}_{}", sf.clip_id, sf.index),
                egui::ColorImage::from_rgba_unmultiplied(
                    [sf.image.width() as usize, sf.image.height() as usize],
                    sf.image.as_raw(),
                ),
                egui::TextureOptions::LINEAR,
            );
            let entry = self
                .textures
                .scrub_frames
                .entry(sf.clip_id)
                .or_insert_with(|| crate::texture_cache::ScrubCacheEntry {
                    frames: Vec::with_capacity(sf.total),
                    pts: Vec::with_capacity(sf.total),
                });
            if entry.frames.len() <= sf.index {
                let needed = sf.index + 1 - entry.frames.len();
                for _ in 0..needed {
                    entry.frames.push(ctx.load_texture(
                        "scrub_placeholder",
                        egui::ColorImage::new([1, 1], egui::Color32::TRANSPARENT),
                        Default::default(),
                    ));
                    entry.pts.push(0.0);
                }
            }
            entry.frames[sf.index] = texture;
            entry.pts[sf.index] = sf.pts_seconds;
            received = true;
        }

        while let Ok((id, peaks)) = self.waveform_rx.try_recv() {
            self.textures.waveform_peaks.insert(id, peaks);
            received = true;
        }

        if received {
            ctx.request_repaint();
        }
    }
}

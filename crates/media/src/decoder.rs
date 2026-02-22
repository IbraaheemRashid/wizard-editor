use std::path::Path;

use ffmpeg::format::Pixel;
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{self, flag::Flags as ScaleFlags};
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg_the_third as ffmpeg;

#[derive(Debug, Clone)]
pub struct StreamProbe {
    pub index: usize,
    pub medium: String,
    pub codec: String,
}

pub fn probe_streams(path: &Path) -> Result<Vec<StreamProbe>, ffmpeg::Error> {
    init_once();
    let format_ctx = ffmpeg::format::input(path)?;
    let mut out = Vec::new();
    for s in format_ctx.streams() {
        let params = s.parameters();
        out.push(StreamProbe {
            index: s.index(),
            medium: format!("{:?}", params.medium()),
            codec: params.id().name().to_string(),
        });
    }
    Ok(out)
}

pub struct VideoDecoder {
    format_ctx: ffmpeg::format::context::Input,
    video_stream_index: usize,
    decoder: ffmpeg::decoder::Video,
    scaler: Option<ScalerState>,
    last_decode_ts: Option<f64>,
    time_base: f64,
}

struct ScalerState {
    ctx: scaling::Context,
    src_w: u32,
    src_h: u32,
    src_fmt: Pixel,
    out_width: u32,
    out_height: u32,
}

pub fn init_once() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg::init().expect("failed to initialize ffmpeg");
        unsafe {
            ffmpeg::ffi::av_log_set_level(ffmpeg::ffi::AV_LOG_FATAL);
        }
    });
}

impl VideoDecoder {
    pub fn open(path: &Path) -> Result<Self, ffmpeg::Error> {
        init_once();

        let format_ctx = ffmpeg::format::input(path)?;

        let video_stream = format_ctx
            .streams()
            .best(Type::Video)
            .ok_or(ffmpeg::Error::StreamNotFound)?;

        let video_stream_index = video_stream.index();
        let tb = video_stream.time_base();
        let time_base = tb.numerator() as f64 / tb.denominator() as f64;

        let codec_ctx =
            ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?;
        let decoder = codec_ctx.decoder().video()?;

        Ok(Self {
            format_ctx,
            video_stream_index,
            decoder,
            scaler: None,
            last_decode_ts: None,
            time_base,
        })
    }

    pub fn duration_seconds(&self) -> Option<f64> {
        let dur = self.format_ctx.duration();
        if dur <= 0 {
            return None;
        }
        Some(dur as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE))
    }

    pub fn resolution(&self) -> (u32, u32) {
        (self.decoder.width(), self.decoder.height())
    }

    pub fn codec_name(&self) -> String {
        self.decoder.id().name().to_string()
    }

    pub fn seek_and_decode(
        &mut self,
        time_seconds: f64,
        target_width: u32,
        target_height: u32,
    ) -> Option<image::RgbaImage> {
        let target_time = time_seconds.max(0.0);
        let ts = (target_time * 1_000_000.0) as i64;
        let _ = self.format_ctx.seek(ts, ..);
        self.decoder.flush();
        self.last_decode_ts = None;

        let mut best_before_target: Option<image::RgbaImage> = None;
        let mut last_pts = f64::NEG_INFINITY;
        let mut stagnant_pts_frames = 0_u32;
        for _ in 0..180 {
            let Some((img, pts)) = self.decode_next_video_frame_inner(target_width, target_height)
            else {
                break;
            };
            if (pts - last_pts).abs() < 1e-6 {
                stagnant_pts_frames += 1;
            } else {
                stagnant_pts_frames = 0;
                last_pts = pts;
            }

            // Some MPEG streams expose unreliable/non-advancing PTS around seeks.
            // Bail out early instead of burning many frames and stalling scrub.
            if stagnant_pts_frames >= 4 {
                return Some(img);
            }

            if pts + 1e-6 >= target_time {
                return Some(img);
            }
            best_before_target = Some(img);
        }
        best_before_target
    }

    pub fn decode_next_frame(
        &mut self,
        target_width: u32,
        target_height: u32,
    ) -> Option<image::RgbaImage> {
        self.decode_next_video_frame_inner(target_width, target_height)
            .map(|(img, _)| img)
    }

    pub fn last_decode_time(&self) -> Option<f64> {
        self.last_decode_ts
    }

    fn record_pts(&mut self, frame: &VideoFrame) {
        if let Some(pts) = frame.pts() {
            self.last_decode_ts = Some(pts as f64 * self.time_base);
        }
    }

    fn decode_next_video_frame_inner(
        &mut self,
        target_width: u32,
        target_height: u32,
    ) -> Option<(image::RgbaImage, f64)> {
        let mut decoded_frame = VideoFrame::empty();
        let mut attempts = 0;

        loop {
            if attempts > 5000 {
                return None;
            }
            attempts += 1;

            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.format_ctx) {
                Ok(_) => {}
                Err(ffmpeg::Error::Eof) => {
                    let _ = self.decoder.send_eof();
                    return match self.decoder.receive_frame(&mut decoded_frame) {
                        Ok(_) => {
                            self.record_pts(&decoded_frame);
                            let pts = self.last_decode_ts.unwrap_or(0.0);
                            self.convert_frame(&decoded_frame, target_width, target_height)
                                .map(|img| (img, pts))
                        }
                        Err(_) => None,
                    };
                }
                Err(_) => return None,
            }

            if packet.stream() != self.video_stream_index {
                continue;
            }

            if self.decoder.send_packet(&packet).is_err() {
                continue;
            }

            match self.decoder.receive_frame(&mut decoded_frame) {
                Ok(_) => {
                    self.record_pts(&decoded_frame);
                    let pts = self.last_decode_ts.unwrap_or(0.0);
                    return self
                        .convert_frame(&decoded_frame, target_width, target_height)
                        .map(|img| (img, pts));
                }
                Err(_) => continue,
            }
        }
    }

    pub fn decode_gop_range(
        &mut self,
        start_seconds: f64,
        end_seconds: f64,
        target_width: u32,
        target_height: u32,
    ) -> Vec<(f64, Vec<u8>, u32, u32)> {
        let ts = (start_seconds.max(0.0) * 1_000_000.0) as i64;
        let _ = self.format_ctx.seek(ts, ..);
        self.decoder.flush();
        self.last_decode_ts = None;

        let mut frames = Vec::new();
        let mut decoded_frame = VideoFrame::empty();
        let mut attempts = 0;

        loop {
            if attempts > 50000 {
                break;
            }
            attempts += 1;

            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.format_ctx) {
                Ok(_) => {}
                Err(ffmpeg::Error::Eof) => {
                    let _ = self.decoder.send_eof();
                    while self.decoder.receive_frame(&mut decoded_frame).is_ok() {
                        let pts = decoded_frame
                            .pts()
                            .map(|p| p as f64 * self.time_base)
                            .unwrap_or(0.0);
                        if pts >= end_seconds {
                            break;
                        }
                        if let Some(img) =
                            self.convert_frame(&decoded_frame, target_width, target_height)
                        {
                            frames.push((pts, img.into_raw(), target_width, target_height));
                        }
                    }
                    break;
                }
                Err(_) => break,
            }

            if packet.stream() != self.video_stream_index {
                continue;
            }

            if self.decoder.send_packet(&packet).is_err() {
                continue;
            }

            while self.decoder.receive_frame(&mut decoded_frame).is_ok() {
                let pts = decoded_frame
                    .pts()
                    .map(|p| p as f64 * self.time_base)
                    .unwrap_or(0.0);
                if pts >= end_seconds {
                    return frames;
                }
                if pts >= start_seconds {
                    if let Some(img) =
                        self.convert_frame(&decoded_frame, target_width, target_height)
                    {
                        frames.push((pts, img.into_raw(), target_width, target_height));
                    }
                }
            }
        }

        frames
    }

    pub fn decode_frames_at_times(
        &mut self,
        times: &[f64],
        target_width: u32,
        target_height: u32,
    ) -> Vec<image::RgbaImage> {
        let mut results = Vec::with_capacity(times.len());
        for &t in times {
            if let Some(img) = self.seek_and_decode(t, target_width, target_height) {
                results.push(img);
            }
        }
        results
    }

    fn ensure_scaler(
        &mut self,
        src_w: u32,
        src_h: u32,
        src_fmt: Pixel,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<(), ffmpeg::Error> {
        let needs_rebuild = match &self.scaler {
            Some(s) => {
                s.src_w != src_w
                    || s.src_h != src_h
                    || s.src_fmt != src_fmt
                    || s.out_width != dst_w
                    || s.out_height != dst_h
            }
            None => true,
        };

        if needs_rebuild {
            let ctx = scaling::Context::get(
                src_fmt,
                src_w,
                src_h,
                Pixel::RGBA,
                dst_w,
                dst_h,
                ScaleFlags::BILINEAR,
            )?;
            self.scaler = Some(ScalerState {
                ctx,
                src_w,
                src_h,
                src_fmt,
                out_width: dst_w,
                out_height: dst_h,
            });
        }

        Ok(())
    }

    fn convert_frame(
        &mut self,
        frame: &VideoFrame,
        target_width: u32,
        target_height: u32,
    ) -> Option<image::RgbaImage> {
        let src_w = frame.width();
        let src_h = frame.height();
        if src_w == 0 || src_h == 0 {
            return None;
        }

        let (dst_w, dst_h) = fit_dimensions(src_w, src_h, target_width, target_height);
        if dst_w == 0 || dst_h == 0 {
            return None;
        }

        self.ensure_scaler(src_w, src_h, frame.format(), dst_w, dst_h)
            .ok()?;

        let scaler = self.scaler.as_mut()?;
        let mut rgba_frame = VideoFrame::empty();
        scaler.ctx.run(frame, &mut rgba_frame).ok()?;

        let stride = rgba_frame.stride(0);
        let data = rgba_frame.data(0);

        let mut pixels = Vec::with_capacity((dst_w * dst_h * 4) as usize);
        for y in 0..dst_h as usize {
            let row_start = y * stride;
            let row_end = row_start + (dst_w as usize * 4);
            if row_end <= data.len() {
                pixels.extend_from_slice(&data[row_start..row_end]);
            }
        }

        if pixels.len() != (dst_w * dst_h * 4) as usize {
            return None;
        }

        if target_width != dst_w || target_height != dst_h {
            let mut padded = vec![0u8; (target_width * target_height * 4) as usize];
            let x_offset = ((target_width - dst_w) / 2) as usize;
            let y_offset = ((target_height - dst_h) / 2) as usize;
            for y in 0..dst_h as usize {
                let src_start = y * dst_w as usize * 4;
                let src_end = src_start + dst_w as usize * 4;
                let dst_start = ((y_offset + y) * target_width as usize + x_offset) * 4;
                let dst_end = dst_start + dst_w as usize * 4;
                if src_end <= pixels.len() && dst_end <= padded.len() {
                    padded[dst_start..dst_end].copy_from_slice(&pixels[src_start..src_end]);
                }
            }
            image::RgbaImage::from_raw(target_width, target_height, padded)
        } else {
            image::RgbaImage::from_raw(dst_w, dst_h, pixels)
        }
    }
}

pub fn fit_dimensions(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (max_w, max_h);
    }
    let scale_w = max_w as f64 / src_w as f64;
    let scale_h = max_h as f64 / src_h as f64;
    let scale = scale_w.min(scale_h);
    let w = ((src_w as f64 * scale).round() as u32).max(2) & !1;
    let h = ((src_h as f64 * scale).round() as u32).max(2) & !1;
    (w.min(max_w), h.min(max_h))
}

pub struct AudioDecoder {
    format_ctx: ffmpeg::format::context::Input,
    audio_stream_index: usize,
    decoder: ffmpeg::decoder::Audio,
    last_decode_end: Option<f64>,
}

impl AudioDecoder {
    pub fn open(path: &Path) -> Result<Self, ffmpeg::Error> {
        init_once();

        let format_ctx = ffmpeg::format::input(path)?;

        let audio_stream = format_ctx
            .streams()
            .best(Type::Audio)
            .ok_or(ffmpeg::Error::StreamNotFound)?;

        let audio_stream_index = audio_stream.index();
        let codec_ctx =
            ffmpeg::codec::context::Context::from_parameters(audio_stream.parameters())?;
        let decoder = codec_ctx.decoder().audio()?;

        Ok(Self {
            format_ctx,
            audio_stream_index,
            decoder,
            last_decode_end: None,
        })
    }

    fn channel_count(&self) -> usize {
        self.decoder.ch_layout().channels() as usize
    }

    pub fn decode_range_mono_f32(
        &mut self,
        start_seconds: f64,
        duration_seconds: f64,
        target_sample_rate: u32,
    ) -> Vec<f32> {
        let start_seconds = start_seconds.max(0.0);
        let duration_seconds = duration_seconds.max(0.0);
        if duration_seconds <= 0.0 {
            return Vec::new();
        }

        let is_sequential = self
            .last_decode_end
            .is_some_and(|end| start_seconds >= end && start_seconds - end < 1.0);

        let skip_samples = if is_sequential {
            let gap = start_seconds - self.last_decode_end.unwrap_or(0.0);
            let src_rate = self.decoder.rate();
            (gap * src_rate as f64) as usize
        } else {
            let ts = (start_seconds * 1_000_000.0) as i64;
            let _ = self.format_ctx.seek(ts, ..);
            self.decoder.flush();
            0
        };

        let end_seconds = start_seconds + duration_seconds;
        let src_rate = self.decoder.rate();
        let total_samples_needed =
            ((duration_seconds + skip_samples as f64 / src_rate as f64) * src_rate as f64) as usize;
        let mut output = Vec::with_capacity(total_samples_needed);

        let channels = self.channel_count();

        let mut decoded = ffmpeg::util::frame::Audio::empty();

        'outer: loop {
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.format_ctx) {
                Ok(_) => {}
                Err(_) => break,
            }

            if packet.stream() != self.audio_stream_index {
                continue;
            }

            if self.decoder.send_packet(&packet).is_err() {
                continue;
            }

            while self.decoder.receive_frame(&mut decoded).is_ok() {
                extract_mono_samples(&decoded, channels, &mut output);

                let effective_len = output.len().saturating_sub(skip_samples);
                if effective_len as f64 / src_rate as f64 + start_seconds >= end_seconds {
                    break 'outer;
                }
            }
        }

        if skip_samples > 0 && output.len() > skip_samples {
            output.drain(..skip_samples);
        } else if skip_samples > 0 {
            output.clear();
        }

        let actual_decoded = output.len() as f64 / src_rate as f64;
        self.last_decode_end = Some(start_seconds + actual_decoded);

        if src_rate != target_sample_rate && !output.is_empty() {
            resample_linear(&output, src_rate, target_sample_rate)
        } else {
            output
        }
    }

    pub fn seek_to(&mut self, start_seconds: f64) {
        let ts = (start_seconds.max(0.0) * 1_000_000.0) as i64;
        let _ = self.format_ctx.seek(ts, ..);
        self.decoder.flush();
        self.last_decode_end = Some(start_seconds);
    }

    pub fn decode_chunk_mono_f32(
        &mut self,
        duration_seconds: f64,
        target_sample_rate: u32,
    ) -> Vec<f32> {
        if duration_seconds <= 0.0 {
            return Vec::new();
        }

        let src_rate = self.decoder.rate();
        let samples_needed = (duration_seconds * src_rate as f64) as usize;
        let channels = self.channel_count();
        let mut output = Vec::with_capacity(samples_needed);

        let mut decoded = ffmpeg::util::frame::Audio::empty();

        'outer: loop {
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.format_ctx) {
                Ok(_) => {}
                Err(_) => break,
            }

            if packet.stream() != self.audio_stream_index {
                continue;
            }

            if self.decoder.send_packet(&packet).is_err() {
                continue;
            }

            while self.decoder.receive_frame(&mut decoded).is_ok() {
                extract_mono_samples(&decoded, channels, &mut output);
                if output.len() >= samples_needed {
                    break 'outer;
                }
            }
        }

        let actual_decoded = output.len() as f64 / src_rate as f64;
        let prev_end = self.last_decode_end.unwrap_or(0.0);
        self.last_decode_end = Some(prev_end + actual_decoded);

        if src_rate != target_sample_rate && !output.is_empty() {
            resample_linear(&output, src_rate, target_sample_rate)
        } else {
            output
        }
    }

    pub fn decode_all_mono_f32(&mut self) -> (Vec<f32>, u32) {
        let _ = self.format_ctx.seek(0, ..);
        self.decoder.flush();
        self.last_decode_end = None;

        let src_rate = self.decoder.rate();
        let channels = self.channel_count();
        let mut output = Vec::new();

        let mut decoded = ffmpeg::util::frame::Audio::empty();

        loop {
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.format_ctx) {
                Ok(_) => {}
                Err(_) => break,
            }

            if packet.stream() != self.audio_stream_index {
                continue;
            }

            if self.decoder.send_packet(&packet).is_err() {
                continue;
            }

            while self.decoder.receive_frame(&mut decoded).is_ok() {
                extract_mono_samples(&decoded, channels, &mut output);
            }
        }

        let _ = self.decoder.send_eof();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            extract_mono_samples(&decoded, channels, &mut output);
        }

        (output, src_rate)
    }
}

fn extract_mono_samples(
    frame: &ffmpeg::util::frame::Audio,
    channels: usize,
    output: &mut Vec<f32>,
) {
    let sample_count = frame.samples();
    let is_planar = !frame.is_packed();
    let format = frame.format();

    use ffmpeg::format::Sample;

    match (format, is_planar) {
        (Sample::F32(_), false) => {
            let data = frame.data(0);
            for i in 0..sample_count {
                let mut sum: f32 = 0.0;
                for ch in 0..channels {
                    let offset = (i * channels + ch) * 4;
                    if offset + 4 <= data.len() {
                        sum += f32::from_le_bytes([
                            data[offset],
                            data[offset + 1],
                            data[offset + 2],
                            data[offset + 3],
                        ]);
                    }
                }
                output.push(sum / channels.max(1) as f32);
            }
        }
        (Sample::F32(_), true) => {
            for i in 0..sample_count {
                let mut sum: f32 = 0.0;
                for ch in 0..channels {
                    if ch < frame.planes() {
                        let plane = frame.data(ch);
                        let offset = i * 4;
                        if offset + 4 <= plane.len() {
                            sum += f32::from_le_bytes([
                                plane[offset],
                                plane[offset + 1],
                                plane[offset + 2],
                                plane[offset + 3],
                            ]);
                        }
                    }
                }
                output.push(sum / channels.max(1) as f32);
            }
        }
        (Sample::I16(_), false) => {
            let data = frame.data(0);
            for i in 0..sample_count {
                let mut sum: f32 = 0.0;
                for ch in 0..channels {
                    let offset = (i * channels + ch) * 2;
                    if offset + 2 <= data.len() {
                        let sample = i16::from_le_bytes([data[offset], data[offset + 1]]);
                        sum += sample as f32 / 32768.0;
                    }
                }
                output.push(sum / channels.max(1) as f32);
            }
        }
        (Sample::I16(_), true) => {
            for i in 0..sample_count {
                let mut sum: f32 = 0.0;
                for ch in 0..channels {
                    if ch < frame.planes() {
                        let plane = frame.data(ch);
                        let offset = i * 2;
                        if offset + 2 <= plane.len() {
                            let sample = i16::from_le_bytes([plane[offset], plane[offset + 1]]);
                            sum += sample as f32 / 32768.0;
                        }
                    }
                }
                output.push(sum / channels.max(1) as f32);
            }
        }
        _ => {
            for _ in 0..sample_count {
                output.push(0.0);
            }
        }
    }
}

fn resample_linear(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if input.is_empty() || src_rate == 0 || dst_rate == 0 {
        return Vec::new();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = (input.len() as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        output.push(a + (b - a) * frac);
    }
    output
}

pub struct ProbeResult {
    pub duration: Option<f64>,
    pub resolution: Option<(u32, u32)>,
    pub codec: Option<String>,
}

pub fn probe_metadata(path: &Path) -> Option<ProbeResult> {
    init_once();

    let format_ctx = ffmpeg::format::input(path).ok()?;

    let duration = {
        let d = format_ctx.duration();
        if d > 0 {
            Some(d as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE))
        } else {
            None
        }
    };

    let video_stream = format_ctx.streams().best(Type::Video);

    let resolution = video_stream.as_ref().and_then(|s| {
        let codec_ctx = ffmpeg::codec::context::Context::from_parameters(s.parameters()).ok()?;
        let dec = codec_ctx.decoder().video().ok()?;
        Some((dec.width(), dec.height()))
    });

    let codec = video_stream.map(|s| s.parameters().id().name().to_string());

    Some(ProbeResult {
        duration,
        resolution,
        codec,
    })
}

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use ffmpeg::format::Pixel;
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{self, flag::Flags as ScaleFlags};
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg_the_third as ffmpeg;
use ringbuf::traits::Producer;

use crate::decoder::{fit_dimensions, init_once};

pub type AudioProducer = ringbuf::HeapProd<f32>;

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub pts_seconds: f64,
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
}

struct StreamClock {
    start_time: Instant,
    start_pts: Option<f64>,
    speed: f64,
}

impl StreamClock {
    fn new(speed: f64) -> Self {
        Self {
            start_time: Instant::now(),
            start_pts: None,
            speed: speed.max(0.01),
        }
    }

    fn reset(&mut self, pts: f64) {
        self.start_time = Instant::now();
        self.start_pts = Some(pts);
    }

    fn set_speed(&mut self, new_speed: f64) {
        if let Some(start_pts) = self.start_pts {
            let elapsed = self.start_time.elapsed().as_secs_f64();
            let current_pts = start_pts + elapsed * self.speed;
            self.start_pts = Some(current_pts);
            self.start_time = Instant::now();
        }
        self.speed = new_speed.max(0.01);
    }

    fn delay(&mut self, pts_seconds: f64) -> std::time::Duration {
        let start_pts = *self.start_pts.get_or_insert(pts_seconds);
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let target = (pts_seconds - start_pts) / self.speed;
        let diff = target - elapsed;
        if diff > 0.001 {
            std::time::Duration::from_secs_f64(diff)
        } else {
            std::time::Duration::ZERO
        }
    }
}

pub struct PipelineHandle {
    frame_rx: Option<mpsc::Receiver<DecodedFrame>>,
    stop_tx: Option<mpsc::Sender<()>>,
    buf_return_tx: mpsc::Sender<Vec<u8>>,
    video_speed_tx: Option<mpsc::Sender<f64>>,
    audio_speed_tx: Option<mpsc::Sender<f64>>,
    _demuxer_handle: Option<JoinHandle<()>>,
    _video_handle: Option<JoinHandle<()>>,
    _audio_handle: Option<JoinHandle<()>>,
}

impl PipelineHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        path: &Path,
        start_time_seconds: f64,
        target_w: u32,
        target_h: u32,
        audio_producer: Option<Arc<Mutex<AudioProducer>>>,
        output_sample_rate: u32,
        output_channels: u16,
        speed: f64,
    ) -> Result<Self, String> {
        init_once();

        let format_ctx =
            ffmpeg::format::input(path).map_err(|e| format!("Failed to open {path:?}: {e}"))?;

        let video_stream = format_ctx.streams().best(Type::Video);
        let audio_stream = format_ctx.streams().best(Type::Audio);

        let video_stream_index = video_stream.as_ref().map(|s| s.index());
        let audio_stream_index = if audio_producer.is_some() {
            audio_stream.as_ref().map(|s| s.index())
        } else {
            None
        };

        let has_video = video_stream.is_some();
        let has_audio = audio_stream.is_some();

        let video_time_base = video_stream.as_ref().map(|s| {
            let tb = s.time_base();
            tb.numerator() as f64 / tb.denominator() as f64
        });
        let audio_time_base = audio_stream.as_ref().map(|s| {
            let tb = s.time_base();
            tb.numerator() as f64 / tb.denominator() as f64
        });

        drop(format_ctx);

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (video_packet_tx, video_packet_rx) = mpsc::sync_channel::<ffmpeg::Packet>(128);
        let (audio_packet_tx, audio_packet_rx) = mpsc::sync_channel::<ffmpeg::Packet>(128);
        let (frame_tx, frame_rx) = mpsc::sync_channel::<DecodedFrame>(16);
        let (buf_return_tx, buf_return_rx) = mpsc::channel::<Vec<u8>>();
        let (video_speed_tx, video_speed_rx) = mpsc::channel::<f64>();
        let (audio_speed_tx, audio_speed_rx) = mpsc::channel::<f64>();

        let path_owned = path.to_path_buf();
        let demuxer_handle = spawn_demuxer(
            path_owned.clone(),
            start_time_seconds,
            video_stream_index,
            audio_stream_index,
            video_packet_tx,
            audio_packet_tx,
            stop_rx,
        );

        let video_handle = if let (true, Some(tb)) = (has_video, video_time_base) {
            Some(spawn_video_decoder(
                video_packet_rx,
                frame_tx,
                buf_return_rx,
                path_owned.clone(),
                tb,
                start_time_seconds,
                target_w,
                target_h,
                speed,
                video_speed_rx,
            ))
        } else {
            drop(video_packet_rx);
            None
        };

        let audio_handle = if let (Some(producer), true, Some(audio_tb)) =
            (audio_producer, has_audio, audio_time_base)
        {
            Some(spawn_audio_decoder(
                audio_packet_rx,
                producer,
                path_owned,
                start_time_seconds,
                audio_tb,
                output_sample_rate,
                output_channels,
                speed,
                audio_speed_rx,
            ))
        } else {
            drop(audio_packet_rx);
            None
        };

        Ok(Self {
            frame_rx: Some(frame_rx),
            stop_tx: Some(stop_tx),
            buf_return_tx,
            video_speed_tx: Some(video_speed_tx),
            audio_speed_tx: Some(audio_speed_tx),
            _demuxer_handle: Some(demuxer_handle),
            _video_handle: video_handle,
            _audio_handle: audio_handle,
        })
    }

    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.as_ref().and_then(|rx| rx.try_recv().ok())
    }

    pub fn return_buffer(&self, buf: Vec<u8>) {
        let _ = self.buf_return_tx.send(buf);
    }

    pub fn update_speed(&self, speed: f64) {
        if let Some(ref tx) = self.video_speed_tx {
            let _ = tx.send(speed);
        }
        if let Some(ref tx) = self.audio_speed_tx {
            let _ = tx.send(speed);
        }
    }

    pub fn stop(mut self) {
        self.signal_stop();
    }

    fn signal_stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.frame_rx.take();
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

fn spawn_demuxer(
    path: PathBuf,
    start_time_seconds: f64,
    video_stream_index: Option<usize>,
    audio_stream_index: Option<usize>,
    video_packet_tx: mpsc::SyncSender<ffmpeg::Packet>,
    audio_packet_tx: mpsc::SyncSender<ffmpeg::Packet>,
    stop_rx: mpsc::Receiver<()>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("pipeline-demuxer".into())
        .spawn(move || {
            let Ok(mut format_ctx) = ffmpeg::format::input(&path) else {
                return;
            };

            if start_time_seconds > 0.01 {
                let ts = (start_time_seconds * 1_000_000.0) as i64;
                let _ = format_ctx.seek(ts, ..);
            }

            loop {
                match stop_rx.try_recv() {
                    Ok(_) | Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {}
                }

                let mut packet = ffmpeg::Packet::empty();
                match packet.read(&mut format_ctx) {
                    Ok(_) => {}
                    Err(ffmpeg::Error::Eof) => return,
                    Err(_) => return,
                }

                let stream_idx = packet.stream();

                if video_stream_index == Some(stream_idx) {
                    if video_packet_tx.send(packet).is_err() {
                        return;
                    }
                } else if audio_stream_index == Some(stream_idx)
                    && audio_packet_tx.send(packet).is_err()
                {
                    return;
                }
            }
        })
        .expect("failed to spawn demuxer thread")
}

#[allow(clippy::too_many_arguments)]
fn spawn_video_decoder(
    packet_rx: mpsc::Receiver<ffmpeg::Packet>,
    frame_tx: mpsc::SyncSender<DecodedFrame>,
    buf_return_rx: mpsc::Receiver<Vec<u8>>,
    path: PathBuf,
    time_base: f64,
    skip_before: f64,
    target_w: u32,
    target_h: u32,
    speed: f64,
    speed_rx: mpsc::Receiver<f64>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("pipeline-video".into())
        .spawn(move || {
            let Ok(format_ctx) = ffmpeg::format::input(&path) else {
                return;
            };
            let Some(video_stream) = format_ctx.streams().best(Type::Video) else {
                return;
            };
            let Ok(codec_ctx) =
                ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())
            else {
                return;
            };
            drop(format_ctx);
            let Ok(mut decoder) = codec_ctx.decoder().video() else {
                return;
            };

            let mut scaler: Option<(scaling::Context, u32, u32, Pixel, u32, u32)> = None;
            let mut decoded_frame = VideoFrame::empty();
            let mut skipping = skip_before > 0.01;
            let mut buf_pool: Vec<Vec<u8>> = Vec::with_capacity(8);
            let mut clock = StreamClock::new(speed);

            let reclaim_buffers = |pool: &mut Vec<Vec<u8>>, rx: &mpsc::Receiver<Vec<u8>>| {
                while let Ok(buf) = rx.try_recv() {
                    if pool.len() < 8 {
                        pool.push(buf);
                    }
                }
            };

            let take_buffer =
                |pool: &mut Vec<Vec<u8>>| -> Vec<u8> { pool.pop().unwrap_or_default() };

            let mut process_frame =
                |frame: &VideoFrame,
                 scaler: &mut Option<(scaling::Context, u32, u32, Pixel, u32, u32)>,
                 skipping: &mut bool,
                 buf_pool: &mut Vec<Vec<u8>>|
                 -> bool {
                    let pts = frame.pts().map(|p| p as f64 * time_base).unwrap_or(0.0);

                    if *skipping {
                        if pts < skip_before - 0.02 {
                            return true;
                        }
                        *skipping = false;
                        clock.reset(pts);
                    }

                    while let Ok(s) = speed_rx.try_recv() {
                        clock.set_speed(s);
                    }

                    reclaim_buffers(buf_pool, &buf_return_rx);
                    let mut buf = take_buffer(buf_pool);

                    if let Some((w, h)) =
                        convert_video_frame(frame, scaler, target_w, target_h, &mut buf)
                    {
                        let delay = clock.delay(pts);
                        if !delay.is_zero() {
                            std::thread::sleep(delay);
                        }
                        let ok = frame_tx
                            .send(DecodedFrame {
                                pts_seconds: pts,
                                width: w,
                                height: h,
                                rgba_data: buf,
                            })
                            .is_ok();
                        ok
                    } else {
                        if buf_pool.len() < 8 {
                            buf_pool.push(buf);
                        }
                        true
                    }
                };

            loop {
                let Ok(packet) = packet_rx.recv() else {
                    let _ = decoder.send_eof();
                    while decoder.receive_frame(&mut decoded_frame).is_ok() {
                        if !process_frame(
                            &decoded_frame,
                            &mut scaler,
                            &mut skipping,
                            &mut buf_pool,
                        ) {
                            return;
                        }
                    }
                    return;
                };

                if decoder.send_packet(&packet).is_err() {
                    continue;
                }

                while decoder.receive_frame(&mut decoded_frame).is_ok() {
                    if !process_frame(
                        &decoded_frame,
                        &mut scaler,
                        &mut skipping,
                        &mut buf_pool,
                    ) {
                        return;
                    }
                }
            }
        })
        .expect("failed to spawn video decoder thread")
}

fn convert_video_frame(
    frame: &VideoFrame,
    scaler: &mut Option<(scaling::Context, u32, u32, Pixel, u32, u32)>,
    target_w: u32,
    target_h: u32,
    reuse_buf: &mut Vec<u8>,
) -> Option<(u32, u32)> {
    let src_w = frame.width();
    let src_h = frame.height();
    if src_w == 0 || src_h == 0 {
        return None;
    }

    let (dst_w, dst_h) = fit_dimensions(src_w, src_h, target_w, target_h);
    if dst_w == 0 || dst_h == 0 {
        return None;
    }

    let needs_rebuild = match scaler {
        Some((_, sw, sh, sf, dw, dh)) => {
            *sw != src_w || *sh != src_h || *sf != frame.format() || *dw != dst_w || *dh != dst_h
        }
        None => true,
    };

    if needs_rebuild {
        let ctx = scaling::Context::get(
            frame.format(),
            src_w,
            src_h,
            Pixel::RGBA,
            dst_w,
            dst_h,
            ScaleFlags::BILINEAR,
        )
        .ok()?;
        *scaler = Some((ctx, src_w, src_h, frame.format(), dst_w, dst_h));
    }

    let (ref mut ctx, _, _, _, _, _) = scaler.as_mut()?;
    let mut rgba_frame = VideoFrame::empty();
    ctx.run(frame, &mut rgba_frame).ok()?;

    let stride = rgba_frame.stride(0);
    let data = rgba_frame.data(0);
    let expected = (target_w * target_h * 4) as usize;

    reuse_buf.clear();
    if reuse_buf.capacity() < expected {
        reuse_buf.reserve(expected - reuse_buf.capacity());
    }

    if target_w != dst_w || target_h != dst_h {
        reuse_buf.resize(expected, 0u8);
        let x_offset = ((target_w - dst_w) / 2) as usize;
        let y_offset = ((target_h - dst_h) / 2) as usize;
        for y in 0..dst_h as usize {
            let src_start = y * stride;
            let src_end = src_start + (dst_w as usize * 4);
            let dst_start = ((y_offset + y) * target_w as usize + x_offset) * 4;
            let dst_end = dst_start + dst_w as usize * 4;
            if src_end <= data.len() && dst_end <= reuse_buf.len() {
                reuse_buf[dst_start..dst_end].copy_from_slice(&data[src_start..src_end]);
            }
        }
    } else {
        for y in 0..dst_h as usize {
            let row_start = y * stride;
            let row_end = row_start + (dst_w as usize * 4);
            if row_end <= data.len() {
                reuse_buf.extend_from_slice(&data[row_start..row_end]);
            }
        }
    }

    if reuse_buf.len() != expected {
        return None;
    }

    Some((target_w, target_h))
}

const GOP_WINDOW: f64 = 4.0;
static MEDIA_AUDIO_PUSH_LOG_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn emit_media_debug_log(
    hypothesis_id: &str,
    location: &str,
    message: &str,
    data: serde_json::Value,
) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let payload = serde_json::json!({
        "id": format!("log_{}_media", timestamp),
        "timestamp": timestamp,
        "location": location,
        "message": message,
        "data": data,
        "runId": "initial",
        "hypothesisId": hypothesis_id
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/Users/irashid/personal/wizard-editor/.cursor/debug.log")
    {
        let _ = std::io::Write::write_all(&mut file, payload.to_string().as_bytes());
        let _ = std::io::Write::write_all(&mut file, b"\n");
    }
}

pub struct ReversePipelineHandle {
    frame_rx: mpsc::Receiver<DecodedFrame>,
    stop_tx: Option<mpsc::Sender<()>>,
    speed_tx: mpsc::Sender<f64>,
    _decode_handle: Option<JoinHandle<()>>,
    _pacer_handle: Option<JoinHandle<()>>,
}

impl ReversePipelineHandle {
    pub fn start(
        path: &Path,
        start_time: f64,
        speed: f64,
        target_w: u32,
        target_h: u32,
    ) -> Result<Self, String> {
        init_once();

        let path_owned = path.to_path_buf();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (speed_tx, speed_rx) = mpsc::channel::<f64>();
        let (intermediate_tx, intermediate_rx) = mpsc::sync_channel::<DecodedFrame>(8);
        let (frame_tx, frame_rx) = mpsc::sync_channel::<DecodedFrame>(4);

        let decode_handle = std::thread::Builder::new()
            .name("reverse-decode".into())
            .spawn(move || {
                let Ok(mut decoder) = crate::decoder::VideoDecoder::open(&path_owned) else {
                    return;
                };

                let mut current_end = start_time;

                while current_end > 0.0 {
                    if let Ok(_) | Err(mpsc::TryRecvError::Disconnected) = stop_rx.try_recv() {
                        return;
                    }

                    let gop_start = (current_end - GOP_WINDOW).max(0.0);
                    let mut frames =
                        decoder.decode_gop_range(gop_start, current_end, target_w, target_h);
                    frames.reverse();

                    for (pts, rgba_data, w, h) in frames {
                        if let Ok(_) | Err(mpsc::TryRecvError::Disconnected) = stop_rx.try_recv() {
                            return;
                        }
                        if intermediate_tx
                            .send(DecodedFrame {
                                pts_seconds: pts,
                                width: w,
                                height: h,
                                rgba_data,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }

                    current_end = gop_start;
                    if gop_start <= 0.0 {
                        break;
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn reverse decode thread: {e}"))?;

        let initial_speed = speed;
        let pacer_handle = std::thread::Builder::new()
            .name("reverse-pacer".into())
            .spawn(move || {
                let mut clock = StreamClock::new(initial_speed);
                let mut last_pts: Option<f64> = None;
                let mut gop_base_pts: f64 = 0.0;

                while let Ok(frame) = intermediate_rx.recv() {
                    while let Ok(s) = speed_rx.try_recv() {
                        clock.set_speed(s);
                    }

                    let is_gop_boundary = match last_pts {
                        Some(prev) => frame.pts_seconds > prev + 0.001,
                        None => true,
                    };

                    if is_gop_boundary {
                        clock.reset(0.0);
                        gop_base_pts = frame.pts_seconds;
                    }

                    let distance = (gop_base_pts - frame.pts_seconds).abs();
                    let delay = clock.delay(distance);
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }

                    last_pts = Some(frame.pts_seconds);

                    if frame_tx.send(frame).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn reverse pacer thread: {e}"))?;

        Ok(Self {
            frame_rx,
            stop_tx: Some(stop_tx),
            speed_tx,
            _decode_handle: Some(decode_handle),
            _pacer_handle: Some(pacer_handle),
        })
    }

    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    pub fn update_speed(&self, speed: f64) {
        let _ = self.speed_tx.send(speed);
    }

    fn signal_stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for ReversePipelineHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_audio_decoder(
    packet_rx: mpsc::Receiver<ffmpeg::Packet>,
    audio_producer: Arc<Mutex<AudioProducer>>,
    path: PathBuf,
    skip_before: f64,
    time_base: f64,
    output_sample_rate: u32,
    output_channels: u16,
    speed: f64,
    speed_rx: mpsc::Receiver<f64>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("pipeline-audio".into())
        .spawn(move || {
            let Ok(format_ctx) = ffmpeg::format::input(&path) else {
                return;
            };
            let Some(audio_stream) = format_ctx.streams().best(Type::Audio) else {
                return;
            };
            let Ok(codec_ctx) =
                ffmpeg::codec::context::Context::from_parameters(audio_stream.parameters())
            else {
                return;
            };
            drop(format_ctx);
            let Ok(mut decoder) = codec_ctx.decoder().audio() else {
                return;
            };

            let src_format = decoder.format();
            let src_rate = decoder.rate();
            let (src_channels, src_mask) = {
                let src_channel_layout = decoder.ch_layout();
                (src_channel_layout.channels(), src_channel_layout.mask())
            };
            let src_layout_for_resampler = match src_mask.and_then(ffmpeg::ChannelLayout::from_mask)
            {
                Some(layout) => layout,
                None => ffmpeg::ChannelLayout::default_for_channels(src_channels),
            };

            let dst_format = ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed);
            let dst_rate = output_sample_rate;
            let dst_layout = ffmpeg::ChannelLayout::MONO;

            let resampler = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ffmpeg::software::resampling::Context::get2(
                    src_format,
                    src_layout_for_resampler.clone(),
                    src_rate,
                    dst_format,
                    dst_layout,
                    dst_rate,
                )
            }));
            let resampler = match resampler {
                Ok(r) => r,
                Err(_) => {
                    return;
                }
            };

            let muted = speed < 0.99;

            let Ok(mut resampler) = resampler else {
                if muted {
                    loop {
                        if packet_rx.recv().is_err() {
                            return;
                        }
                    }
                }
                let mut decoded = ffmpeg::util::frame::Audio::empty();
                loop {
                    let Ok(packet) = packet_rx.recv() else {
                        return;
                    };
                    let _ = decoder.send_packet(&packet);
                    while decoder.receive_frame(&mut decoded).is_ok() {
                        push_audio_samples_manual(&decoded, &audio_producer, output_channels);
                    }
                }
            };

            if muted {
                loop {
                    if packet_rx.recv().is_err() {
                        return;
                    }
                }
            }

            let mut decoded = ffmpeg::util::frame::Audio::empty();
            let mut resampled = ffmpeg::util::frame::Audio::empty();
            let mut clock = StreamClock::new(speed);
            let mut skipping = skip_before > 0.01;
            let mut estimated_next_pts: Option<f64> = None;

            loop {
                let Ok(packet) = packet_rx.recv() else {
                    let _ = decoder.send_eof();
                    while decoder.receive_frame(&mut decoded).is_ok() {
                        let _ = resampler.run(&decoded, &mut resampled);
                        push_resampled_f32(&resampled, &audio_producer, output_channels);
                    }
                    if let Ok(Some(_delay)) = resampler.flush(&mut resampled) {
                        push_resampled_f32(&resampled, &audio_producer, output_channels);
                    }
                    return;
                };

                if decoder.send_packet(&packet).is_err() {
                    continue;
                }

                while decoder.receive_frame(&mut decoded).is_ok() {
                    let frame_pts = decoded
                        .pts()
                        .map(|p| p as f64 * time_base)
                        .or(estimated_next_pts)
                        .unwrap_or(0.0);
                    let frame_duration = if decoder.rate() > 0 {
                        decoded.samples() as f64 / decoder.rate() as f64
                    } else {
                        0.0
                    };
                    estimated_next_pts = Some(frame_pts + frame_duration);

                    if skipping {
                        if frame_pts + frame_duration < skip_before - 0.02 {
                            continue;
                        }
                        skipping = false;
                        clock.reset(frame_pts);
                    }

                    while let Ok(s) = speed_rx.try_recv() {
                        clock.set_speed(s);
                    }

                    let delay = clock.delay(frame_pts);
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }

                    if decoded.ch_layout().mask().is_none() {
                        decoded.set_ch_layout(src_layout_for_resampler.clone());
                    }

                    let _ = resampler.run(&decoded, &mut resampled);
                    push_resampled_f32(&resampled, &audio_producer, output_channels);
                }
            }
        })
        .expect("failed to spawn audio decoder thread")
}

pub struct AudioOnlyHandle {
    stop_tx: Option<mpsc::Sender<()>>,
    _demuxer: Option<JoinHandle<()>>,
    _decoder: Option<JoinHandle<()>>,
}

impl AudioOnlyHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        path: &Path,
        start_time: f64,
        audio_producer: Arc<Mutex<AudioProducer>>,
        sample_rate: u32,
        channels: u16,
        speed: f64,
    ) -> Result<Self, String> {
        init_once();

        let format_ctx =
            ffmpeg::format::input(path).map_err(|e| format!("Failed to open {path:?}: {e}"))?;

        let audio_stream = format_ctx
            .streams()
            .best(Type::Audio)
            .ok_or_else(|| "No audio stream found".to_string())?;

        let audio_stream_index = audio_stream.index();
        let audio_time_base = {
            let tb = audio_stream.time_base();
            tb.numerator() as f64 / tb.denominator() as f64
        };

        drop(format_ctx);

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (audio_packet_tx, audio_packet_rx) = mpsc::sync_channel::<ffmpeg::Packet>(128);

        let path_owned = path.to_path_buf();
        let path_for_decoder = path_owned.clone();

        let demuxer = std::thread::Builder::new()
            .name("audio-only-demuxer".into())
            .spawn(move || {
                let Ok(mut format_ctx) = ffmpeg::format::input(&path_owned) else {
                    return;
                };

                if start_time > 0.01 {
                    let ts = (start_time * 1_000_000.0) as i64;
                    let _ = format_ctx.seek(ts, ..);
                }

                loop {
                    match stop_rx.try_recv() {
                        Ok(_) | Err(mpsc::TryRecvError::Disconnected) => return,
                        Err(mpsc::TryRecvError::Empty) => {}
                    }

                    let mut packet = ffmpeg::Packet::empty();
                    match packet.read(&mut format_ctx) {
                        Ok(_) => {}
                        Err(ffmpeg::Error::Eof) => return,
                        Err(_) => return,
                    }

                    if packet.stream() == audio_stream_index
                        && audio_packet_tx.send(packet).is_err()
                    {
                        return;
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn audio-only demuxer: {e}"))?;

        let (_audio_speed_tx, audio_speed_rx) = mpsc::channel::<f64>();
        let decoder_handle = spawn_audio_decoder(
            audio_packet_rx,
            audio_producer,
            path_for_decoder,
            start_time,
            audio_time_base,
            sample_rate,
            channels,
            speed,
            audio_speed_rx,
        );

        Ok(Self {
            stop_tx: Some(stop_tx),
            _demuxer: Some(demuxer),
            _decoder: Some(decoder_handle),
        })
    }

    fn signal_stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for AudioOnlyHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

fn push_resampled_f32(
    frame: &ffmpeg::util::frame::Audio,
    producer: &Arc<Mutex<AudioProducer>>,
    output_channels: u16,
) -> usize {
    let samples = frame.samples();
    if samples == 0 {
        return 0;
    }

    let data = frame.data(0);
    let ch = output_channels as usize;

    let mut buf = Vec::with_capacity(samples * ch);
    for i in 0..samples {
        let offset = i * 4;
        if offset + 4 > data.len() {
            break;
        }
        let sample = f32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        for _ in 0..ch {
            buf.push(sample);
        }
    }

    if let Ok(mut guard) = producer.lock() {
        let pushed = guard.push_slice(&buf);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let last = MEDIA_AUDIO_PUSH_LOG_MS.load(std::sync::atomic::Ordering::Relaxed);
        if now_ms.saturating_sub(last) > 500 {
            MEDIA_AUDIO_PUSH_LOG_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
            // #region agent log
            emit_media_debug_log(
                "H9",
                "crates/media/src/pipeline.rs:push_resampled_f32",
                "audio decoder pushed samples to source ring",
                serde_json::json!({
                    "frameSamples": samples,
                    "bufferLen": buf.len(),
                    "pushed": pushed,
                    "outputChannels": output_channels
                }),
            );
            // #endregion
        }
        if pushed == 0 && !buf.is_empty() {
            // #region agent log
            emit_media_debug_log(
                "H10",
                "crates/media/src/pipeline.rs:push_resampled_f32",
                "audio decoder push dropped all samples",
                serde_json::json!({
                    "frameSamples": samples,
                    "bufferLen": buf.len(),
                    "outputChannels": output_channels
                }),
            );
            // #endregion
        }
        return pushed;
    }

    // #region agent log
    emit_media_debug_log(
        "H10",
        "crates/media/src/pipeline.rs:push_resampled_f32",
        "audio decoder failed to lock source ring producer",
        serde_json::json!({
            "frameSamples": samples,
            "bufferLen": buf.len(),
            "outputChannels": output_channels
        }),
    );
    // #endregion
    0
}

fn push_audio_samples_manual(
    frame: &ffmpeg::util::frame::Audio,
    producer: &Arc<Mutex<AudioProducer>>,
    output_channels: u16,
) {
    let sample_count = frame.samples();
    let is_planar = !frame.is_packed();
    let format = frame.format();
    let channels = frame.ch_layout().channels() as usize;
    let ch_out = output_channels as usize;

    use ffmpeg::format::Sample;

    let mut buf = Vec::with_capacity(sample_count * ch_out);

    for i in 0..sample_count {
        let mut mono: f32 = 0.0;
        match (format, is_planar) {
            (Sample::F32(_), false) => {
                let data = frame.data(0);
                for ch in 0..channels {
                    let offset = (i * channels + ch) * 4;
                    if offset + 4 <= data.len() {
                        mono += f32::from_le_bytes([
                            data[offset],
                            data[offset + 1],
                            data[offset + 2],
                            data[offset + 3],
                        ]);
                    }
                }
                mono /= channels.max(1) as f32;
            }
            (Sample::F32(_), true) => {
                for ch in 0..channels {
                    if ch < frame.planes() {
                        let plane = frame.data(ch);
                        let offset = i * 4;
                        if offset + 4 <= plane.len() {
                            mono += f32::from_le_bytes([
                                plane[offset],
                                plane[offset + 1],
                                plane[offset + 2],
                                plane[offset + 3],
                            ]);
                        }
                    }
                }
                mono /= channels.max(1) as f32;
            }
            (Sample::I16(_), false) => {
                let data = frame.data(0);
                for ch in 0..channels {
                    let offset = (i * channels + ch) * 2;
                    if offset + 2 <= data.len() {
                        let sample = i16::from_le_bytes([data[offset], data[offset + 1]]);
                        mono += sample as f32 / 32768.0;
                    }
                }
                mono /= channels.max(1) as f32;
            }
            (Sample::I16(_), true) => {
                for ch in 0..channels {
                    if ch < frame.planes() {
                        let plane = frame.data(ch);
                        let offset = i * 2;
                        if offset + 2 <= plane.len() {
                            let sample = i16::from_le_bytes([plane[offset], plane[offset + 1]]);
                            mono += sample as f32 / 32768.0;
                        }
                    }
                }
                mono /= channels.max(1) as f32;
            }
            _ => {}
        }

        for _ in 0..ch_out {
            buf.push(mono);
        }
    }

    if let Ok(mut guard) = producer.lock() {
        let _ = guard.push_slice(&buf);
    }
}

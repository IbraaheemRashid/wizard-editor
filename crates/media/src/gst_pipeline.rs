use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use ringbuf::traits::Producer;

use crate::pipeline::{AudioProducer, DecodedFrame};

fn init_once() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().expect("Failed to initialize GStreamer");
    });
}

pub fn prewarm_file(path: &Path) {
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("file-prewarm".into())
        .spawn(move || {
            prewarm_file_sync(&path);
            prewarm_gst_pipeline(&path);
        })
        .ok();
}

fn prewarm_file_sync(path: &Path) {
    use std::io::Read as _;
    let t0 = Instant::now();
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        let mut total = 0usize;
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if total >= 16 * 1024 * 1024 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = t0.elapsed().as_secs_f64() * 1000.0;
    }
}

fn prewarm_gst_pipeline(path: &Path) {
    init_once();
    let t0 = Instant::now();
    let pipeline = gst::Pipeline::new();
    let Ok(filesrc) = gst::ElementFactory::make("filesrc")
        .property(
            "location",
            path.to_str().unwrap_or_default(),
        )
        .build()
    else {
        return;
    };
    let Ok(decodebin) = make_element("decodebin") else {
        return;
    };
    let Ok(fakesink) = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
    else {
        return;
    };

    if pipeline.add_many([&filesrc, &decodebin, &fakesink]).is_err() {
        return;
    }
    if gst::Element::link_many([&filesrc, &decodebin]).is_err() {
        return;
    }

    let fakesink_weak = fakesink.downgrade();
    decodebin.connect_pad_added(move |_dbin, src_pad| {
        let caps = match src_pad.current_caps() {
            Some(c) => c,
            None => src_pad.query_caps(None),
        };
        let Some(structure) = caps.structure(0) else {
            return;
        };
        if structure.name().as_str().starts_with("video/") {
            if let Some(sink) = fakesink_weak.upgrade() {
                let sink_pad = sink.static_pad("sink").expect("fakesink has sink");
                if !sink_pad.is_linked() {
                    let _ = src_pad.link(&sink_pad);
                }
            }
        }
    });

    let _ = pipeline.set_state(gst::State::Paused);
    if let Some(bus) = pipeline.bus() {
        let timeout = gst::ClockTime::from_seconds(3);
        let _ = wait_for_async_done(&bus, timeout);
    }
    let _ = pipeline.set_state(gst::State::Null);

    let _ = t0.elapsed().as_secs_f64() * 1000.0;
}

fn wait_for_async_done(bus: &gst::Bus, timeout: gst::ClockTime) -> Result<(), String> {
    loop {
        let Some(msg) = bus.timed_pop(timeout) else {
            return Ok(());
        };
        match msg.view() {
            gst::MessageView::AsyncDone(_) => return Ok(()),
            gst::MessageView::Error(err) => {
                return Err(format!("{}", err.error()));
            }
            _ => {}
        }
    }
}

fn build_video_caps(target_w: u32, target_h: u32) -> gst::Caps {
    gst_video::VideoCapsBuilder::new()
        .format(gst_video::VideoFormat::Rgba)
        .width(target_w as i32)
        .height(target_h as i32)
        .build()
}

fn build_audio_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("channels", 1i32)
        .field("layout", "interleaved")
        .build()
}

fn make_element(factory_name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory_name)
        .build()
        .map_err(|e| format!("Failed to create {factory_name}: {e}"))
}

fn connect_decodebin_video_only(decodebin: &gst::Element, videoconvert: &gst::Element) {
    let videoconvert_weak = videoconvert.downgrade();
    decodebin.connect_pad_added(move |_dbin, src_pad| {
        let caps = match src_pad.current_caps() {
            Some(c) => c,
            None => src_pad.query_caps(None),
        };
        let Some(structure) = caps.structure(0) else {
            return;
        };
        if structure.name().as_str().starts_with("video/") {
            if let Some(vc) = videoconvert_weak.upgrade() {
                let sink_pad = vc.static_pad("sink").expect("videoconvert has sink");
                if !sink_pad.is_linked() {
                    let _ = src_pad.link(&sink_pad);
                }
            }
        }
    });
}

fn connect_decodebin_audio_only(decodebin: &gst::Element, audioconvert: &gst::Element) {
    let audioconvert_weak = audioconvert.downgrade();
    decodebin.connect_pad_added(move |_dbin, src_pad| {
        let caps = match src_pad.current_caps() {
            Some(c) => c,
            None => src_pad.query_caps(None),
        };
        let Some(structure) = caps.structure(0) else {
            return;
        };
        if structure.name().as_str().starts_with("audio/") {
            if let Some(aconv) = audioconvert_weak.upgrade() {
                let sink_pad = aconv.static_pad("sink").expect("audioconvert has sink");
                if !sink_pad.is_linked() {
                    let _ = src_pad.link(&sink_pad);
                }
            }
        }
    });
}

fn connect_decodebin_video_and_audio(
    decodebin: &gst::Element,
    videoconvert: &gst::Element,
    audioconvert: &gst::Element,
) {
    let videoconvert_weak = videoconvert.downgrade();
    let audioconvert_weak = audioconvert.downgrade();
    decodebin.connect_pad_added(move |_dbin, src_pad| {
        let caps = match src_pad.current_caps() {
            Some(c) => c,
            None => src_pad.query_caps(None),
        };
        let Some(structure) = caps.structure(0) else {
            return;
        };
        let name = structure.name().as_str();

        if name.starts_with("video/") {
            if let Some(vc) = videoconvert_weak.upgrade() {
                let sink_pad = vc.static_pad("sink").expect("videoconvert has sink");
                if !sink_pad.is_linked() {
                    let _ = src_pad.link(&sink_pad);
                }
            }
        } else if name.starts_with("audio/") {
            if let Some(aconv) = audioconvert_weak.upgrade() {
                let sink_pad = aconv.static_pad("sink").expect("audioconvert has sink");
                if !sink_pad.is_linked() {
                    let _ = src_pad.link(&sink_pad);
                }
            }
        }
    });
}

fn preroll_and_seek(
    pipeline: &gst::Pipeline,
    start_time_seconds: f64,
    speed: f64,
) -> Result<(), String> {
    let t0 = Instant::now();
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|e| format!("Failed to set Paused: {e}"))?;

    let bus = pipeline.bus().ok_or("No bus")?;
    let timeout = gst::ClockTime::from_seconds(5);
    wait_for_async_done(&bus, timeout).map_err(|e| format!("Preroll error: {e}"))?;
    let paused_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let mut seek_ms = 0.0;
    if start_time_seconds > 0.01 {
        let t_seek = Instant::now();
        let seek_pos = gst::ClockTime::from_nseconds((start_time_seconds * 1_000_000_000.0) as u64);
        pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, seek_pos)
            .map_err(|e| format!("Seek failed: {e}"))?;
        wait_for_async_done(&bus, timeout).map_err(|e| format!("Seek error: {e}"))?;
        seek_ms = t_seek.elapsed().as_secs_f64() * 1000.0;
    }

    let mut speed_ms = 0.0;
    if (speed - 1.0).abs() > 0.01 {
        let t_speed = Instant::now();
        let cur_pos: Option<gst::ClockTime> = pipeline.query_position();
        let pos = cur_pos.unwrap_or(gst::ClockTime::ZERO);
        let _ = pipeline.seek(
            speed,
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::SeekType::Set,
            pos,
            gst::SeekType::End,
            gst::ClockTime::ZERO,
        );
        let _ = wait_for_async_done(&bus, timeout);
        speed_ms = t_speed.elapsed().as_secs_f64() * 1000.0;
    }

    let _ = (paused_ms, seek_ms, speed_ms, start_time_seconds);

    Ok(())
}

fn push_audio_from_buffer(data: &[u8], ch: u16, producer: &Arc<Mutex<AudioProducer>>) {
    let sample_count = data.len() / 4;
    let ch_out = ch as usize;
    let mut buf = Vec::with_capacity(sample_count * ch_out);

    for i in 0..sample_count {
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
        for _ in 0..ch_out {
            buf.push(sample);
        }
    }

    if let Ok(mut guard) = producer.lock() {
        guard.push_slice(&buf);
    }
}

pub struct GstPipelineHandle {
    frame_rx: mpsc::Receiver<DecodedFrame>,
    buf_return_tx: mpsc::Sender<Vec<u8>>,
    stop_tx: Option<mpsc::Sender<()>>,
    pipeline: gst::Pipeline,
    first_frame_ready: Arc<AtomicBool>,
    _bridge_handle: Option<JoinHandle<()>>,
    _audio_bridge_handle: Option<JoinHandle<()>>,
}

impl GstPipelineHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        path: &Path,
        start_time_seconds: f64,
        target_w: u32,
        target_h: u32,
        audio_producer: Option<Arc<Mutex<AudioProducer>>>,
        _output_sample_rate: u32,
        output_channels: u16,
        speed: f64,
    ) -> Result<Self, String> {
        let t_total = Instant::now();
        prewarm_file_sync(path);
        init_once();

        let pipeline = gst::Pipeline::new();

        let filesrc = gst::ElementFactory::make("filesrc")
            .property(
                "location",
                path.to_str()
                    .ok_or_else(|| "Invalid path encoding".to_string())?,
            )
            .build()
            .map_err(|e| format!("Failed to create filesrc: {e}"))?;

        let decodebin = make_element("decodebin")?;

        pipeline
            .add_many([&filesrc, &decodebin])
            .map_err(|e| format!("Failed to add elements: {e}"))?;
        gst::Element::link_many([&filesrc, &decodebin])
            .map_err(|e| format!("Failed to link: {e}"))?;

        let videoconvert = make_element("videoconvert")?;
        let videoscale = make_element("videoscale")?;
        let video_caps = build_video_caps(target_w, target_h);

        let video_appsink = gst_app::AppSink::builder()
            .caps(&video_caps)
            .max_buffers(4)
            .drop(true)
            .sync(true)
            .build();

        pipeline
            .add_many([
                &videoconvert,
                &videoscale,
                video_appsink.upcast_ref::<gst::Element>(),
            ])
            .map_err(|e| format!("Failed to add video elements: {e}"))?;
        gst::Element::link_many([
            &videoconvert,
            &videoscale,
            video_appsink.upcast_ref::<gst::Element>(),
        ])
        .map_err(|e| format!("Failed to link video chain: {e}"))?;

        let mut audio_appsink: Option<gst_app::AppSink> = None;
        let mut audioconvert: Option<gst::Element> = None;

        if audio_producer.is_some() {
            let aconv = make_element("audioconvert")?;
            let aresample = make_element("audioresample")?;
            let audio_caps = build_audio_caps();

            let asink = gst_app::AppSink::builder()
                .caps(&audio_caps)
                .max_buffers(64)
                .drop(false)
                .sync(true)
                .build();

            pipeline
                .add_many([&aconv, &aresample, asink.upcast_ref::<gst::Element>()])
                .map_err(|e| format!("Failed to add audio elements: {e}"))?;
            gst::Element::link_many([&aconv, &aresample, asink.upcast_ref::<gst::Element>()])
                .map_err(|e| format!("Failed to link audio chain: {e}"))?;

            audioconvert = Some(aconv);
            audio_appsink = Some(asink);
        }

        if let Some(ref aconv) = audioconvert {
            connect_decodebin_video_and_audio(&decodebin, &videoconvert, aconv);
        } else {
            connect_decodebin_video_only(&decodebin, &videoconvert);
        }

        preroll_and_seek(&pipeline, start_time_seconds, speed)?;

        let (frame_tx, frame_rx) = mpsc::sync_channel::<DecodedFrame>(16);
        let (buf_return_tx, buf_return_rx) = mpsc::channel::<Vec<u8>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let first_frame_ready = Arc::new(AtomicBool::new(false));

        let bridge_handle = {
            let video_sink = video_appsink;
            let tw = target_w;
            let th = target_h;
            let ffr = first_frame_ready.clone();
            std::thread::Builder::new()
                .name("gst-video-bridge".into())
                .spawn(move || {
                    let mut buf_pool: Vec<Vec<u8>> = Vec::with_capacity(8);
                    let expected_size = (tw as usize) * (th as usize) * 4;

                    if let Ok(preroll_sample) = video_sink.pull_preroll() {
                        if let Some(buffer) = preroll_sample.buffer() {
                            let pts_seconds = buffer
                                .pts()
                                .map(|p| p.nseconds() as f64 / 1_000_000_000.0)
                                .unwrap_or(0.0);
                            if let Ok(map) = buffer.map_readable() {
                                let data = map.as_slice();
                                let mut rgba_data = Vec::with_capacity(expected_size);
                                if data.len() >= expected_size {
                                    rgba_data.extend_from_slice(&data[..expected_size]);
                                } else {
                                    rgba_data.extend_from_slice(data);
                                    rgba_data.resize(expected_size, 0);
                                }
                                let _ = frame_tx.send(DecodedFrame {
                                    pts_seconds,
                                    width: tw,
                                    height: th,
                                    rgba_data,
                                });
                            }
                        }
                    }
                    ffr.store(true, Ordering::Release);

                    loop {
                        if stop_rx.try_recv().is_ok() {
                            return;
                        }

                        while let Ok(buf) = buf_return_rx.try_recv() {
                            if buf_pool.len() < 8 {
                                buf_pool.push(buf);
                            }
                        }

                        let sample =
                            match video_sink.try_pull_sample(gst::ClockTime::from_mseconds(8)) {
                                Some(s) => s,
                                None => {
                                    if video_sink.is_eos() {
                                        return;
                                    }
                                    continue;
                                }
                            };

                        let Some(buffer) = sample.buffer() else {
                            continue;
                        };

                        let pts_seconds = buffer
                            .pts()
                            .map(|p| p.nseconds() as f64 / 1_000_000_000.0)
                            .unwrap_or(0.0);

                        let Ok(map) = buffer.map_readable() else {
                            continue;
                        };

                        let data = map.as_slice();
                        let mut rgba_data = buf_pool.pop().unwrap_or_default();
                        rgba_data.clear();
                        if rgba_data.capacity() < expected_size {
                            rgba_data.reserve(expected_size - rgba_data.capacity());
                        }

                        if data.len() >= expected_size {
                            rgba_data.extend_from_slice(&data[..expected_size]);
                        } else {
                            rgba_data.extend_from_slice(data);
                            rgba_data.resize(expected_size, 0);
                        }

                        if frame_tx
                            .send(DecodedFrame {
                                pts_seconds,
                                width: tw,
                                height: th,
                                rgba_data,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                })
                .expect("failed to spawn gst video bridge thread")
        };

        let audio_bridge_handle =
            if let (Some(asink), Some(producer)) = (audio_appsink, audio_producer) {
                let muted = speed < 0.99;
                let ch = output_channels;
                Some(
                    std::thread::Builder::new()
                        .name("gst-audio-bridge".into())
                        .spawn(move || loop {
                            let sample = match asink.try_pull_sample(gst::ClockTime::from_mseconds(
                                if muted { 50 } else { 8 },
                            )) {
                                Some(s) => s,
                                None => {
                                    if asink.is_eos() {
                                        return;
                                    }
                                    continue;
                                }
                            };

                            if muted {
                                continue;
                            }

                            let Some(buffer) = sample.buffer() else {
                                continue;
                            };
                            let Ok(map) = buffer.map_readable() else {
                                continue;
                            };

                            push_audio_from_buffer(map.as_slice(), ch, &producer);
                        })
                        .expect("failed to spawn gst audio bridge thread"),
                )
            } else {
                None
            };

        let _ = t_total.elapsed().as_secs_f64() * 1000.0;

        Ok(Self {
            frame_rx,
            buf_return_tx,
            stop_tx: Some(stop_tx),
            pipeline,
            first_frame_ready,
            _bridge_handle: Some(bridge_handle),
            _audio_bridge_handle: audio_bridge_handle,
        })
    }

    pub fn is_first_frame_ready(&self) -> bool {
        self.first_frame_ready.load(Ordering::Acquire)
    }

    pub fn begin_playing(&self) -> Result<(), String> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("Failed to set Playing: {e}"))?;
        Ok(())
    }

    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    pub fn return_buffer(&self, buf: Vec<u8>) {
        let _ = self.buf_return_tx.send(buf);
    }

    pub fn update_speed(&self, speed: f64) {
        let cur_pos: Option<gst::ClockTime> = self.pipeline.query_position();
        let pos = cur_pos.unwrap_or(gst::ClockTime::ZERO);
        let _ = self.pipeline.seek(
            speed,
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::SeekType::Set,
            pos,
            gst::SeekType::End,
            gst::ClockTime::ZERO,
        );
    }

    pub fn stop(mut self) {
        self.signal_stop();
    }

    fn signal_stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for GstPipelineHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

pub struct GstReversePipelineHandle {
    frame_rx: mpsc::Receiver<DecodedFrame>,
    stop_tx: Option<mpsc::Sender<()>>,
    pipeline: gst::Pipeline,
    first_frame_ready: Arc<AtomicBool>,
    _bridge_handle: Option<JoinHandle<()>>,
}

impl GstReversePipelineHandle {
    pub fn start(
        path: &Path,
        start_time: f64,
        speed: f64,
        target_w: u32,
        target_h: u32,
    ) -> Result<Self, String> {
        prewarm_file_sync(path);
        init_once();

        let pipeline = gst::Pipeline::new();

        let filesrc = gst::ElementFactory::make("filesrc")
            .property(
                "location",
                path.to_str()
                    .ok_or_else(|| "Invalid path encoding".to_string())?,
            )
            .build()
            .map_err(|e| format!("Failed to create filesrc: {e}"))?;

        let decodebin = make_element("decodebin")?;

        pipeline
            .add_many([&filesrc, &decodebin])
            .map_err(|e| format!("Failed to add elements: {e}"))?;
        gst::Element::link_many([&filesrc, &decodebin])
            .map_err(|e| format!("Failed to link: {e}"))?;

        let videoconvert = make_element("videoconvert")?;
        let videoscale = make_element("videoscale")?;
        let video_caps = build_video_caps(target_w, target_h);

        let video_appsink = gst_app::AppSink::builder()
            .caps(&video_caps)
            .max_buffers(8)
            .drop(true)
            .sync(false)
            .build();

        pipeline
            .add_many([
                &videoconvert,
                &videoscale,
                video_appsink.upcast_ref::<gst::Element>(),
            ])
            .map_err(|e| format!("Failed to add video elements: {e}"))?;
        gst::Element::link_many([
            &videoconvert,
            &videoscale,
            video_appsink.upcast_ref::<gst::Element>(),
        ])
        .map_err(|e| format!("Failed to link video chain: {e}"))?;

        connect_decodebin_video_only(&decodebin, &videoconvert);

        pipeline
            .set_state(gst::State::Paused)
            .map_err(|e| format!("Failed to set Paused: {e}"))?;

        let bus = pipeline.bus().ok_or("No bus")?;
        let timeout = gst::ClockTime::from_seconds(5);
        wait_for_async_done(&bus, timeout).map_err(|e| format!("Preroll error: {e}"))?;

        let seek_pos = if start_time > 0.01 {
            gst::ClockTime::from_nseconds((start_time * 1_000_000_000.0) as u64)
        } else {
            gst::ClockTime::ZERO
        };

        pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                seek_pos,
            )
            .map_err(|e| format!("Reverse position seek failed: {e}"))?;
        wait_for_async_done(&bus, timeout)
            .map_err(|e| format!("Reverse position seek error: {e}"))?;

        let rate = -(speed.max(0.01));
        pipeline
            .seek(
                rate,
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::ACCURATE,
                gst::SeekType::Set,
                gst::ClockTime::ZERO,
                gst::SeekType::Set,
                seek_pos,
            )
            .map_err(|e| format!("Reverse segment seek failed: {e}"))?;
        wait_for_async_done(&bus, timeout)
            .map_err(|e| format!("Reverse segment seek error: {e}"))?;

        let (frame_tx, frame_rx) = mpsc::sync_channel::<DecodedFrame>(4);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let first_frame_ready = Arc::new(AtomicBool::new(false));

        let bridge_handle = {
            let video_sink = video_appsink;
            let tw = target_w;
            let th = target_h;
            let ffr = first_frame_ready.clone();
            std::thread::Builder::new()
                .name("gst-reverse-bridge".into())
                .spawn(move || {
                    let expected_size = (tw as usize) * (th as usize) * 4;
                    let mut has_signaled_first_frame = false;

                    if let Ok(preroll_sample) = video_sink.pull_preroll() {
                        if let Some(buffer) = preroll_sample.buffer() {
                            let pts_seconds = buffer
                                .pts()
                                .map(|p| p.nseconds() as f64 / 1_000_000_000.0)
                                .unwrap_or(0.0);
                            if let Ok(map) = buffer.map_readable() {
                                let data = map.as_slice();
                                let mut rgba_data = Vec::with_capacity(expected_size);
                                if data.len() >= expected_size {
                                    rgba_data.extend_from_slice(&data[..expected_size]);
                                } else {
                                    rgba_data.extend_from_slice(data);
                                    rgba_data.resize(expected_size, 0);
                                }
                                if frame_tx
                                    .send(DecodedFrame {
                                        pts_seconds,
                                        width: tw,
                                        height: th,
                                        rgba_data,
                                    })
                                    .is_ok()
                                {
                                    ffr.store(true, Ordering::Release);
                                    has_signaled_first_frame = true;
                                }
                            }
                        }
                    }

                    loop {
                        if stop_rx.try_recv().is_ok() {
                            return;
                        }

                        let sample =
                            match video_sink.try_pull_sample(gst::ClockTime::from_mseconds(8)) {
                                Some(s) => s,
                                None => {
                                    if video_sink.is_eos() {
                                        return;
                                    }
                                    continue;
                                }
                            };

                        let Some(buffer) = sample.buffer() else {
                            continue;
                        };

                        let pts_seconds = buffer
                            .pts()
                            .map(|p| p.nseconds() as f64 / 1_000_000_000.0)
                            .unwrap_or(0.0);

                        let Ok(map) = buffer.map_readable() else {
                            continue;
                        };

                        let data = map.as_slice();
                        let mut rgba_data = Vec::with_capacity(expected_size);
                        if data.len() >= expected_size {
                            rgba_data.extend_from_slice(&data[..expected_size]);
                        } else {
                            rgba_data.extend_from_slice(data);
                            rgba_data.resize(expected_size, 0);
                        }

                        if frame_tx
                            .send(DecodedFrame {
                                pts_seconds,
                                width: tw,
                                height: th,
                                rgba_data,
                            })
                            .is_err()
                        {
                            return;
                        }
                        if !has_signaled_first_frame {
                            ffr.store(true, Ordering::Release);
                            has_signaled_first_frame = true;
                        }
                    }
                })
                .expect("failed to spawn gst reverse bridge thread")
        };

        Ok(Self {
            frame_rx,
            stop_tx: Some(stop_tx),
            pipeline,
            first_frame_ready,
            _bridge_handle: Some(bridge_handle),
        })
    }

    pub fn is_first_frame_ready(&self) -> bool {
        self.first_frame_ready.load(Ordering::Acquire)
    }

    pub fn begin_playing(&self) -> Result<(), String> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("Failed to set Playing: {e}"))?;
        Ok(())
    }

    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    pub fn update_speed(&self, speed: f64) {
        let cur_pos: Option<gst::ClockTime> = self.pipeline.query_position();
        let pos = cur_pos.unwrap_or(gst::ClockTime::ZERO);
        let rate = -(speed.max(0.01));
        let _ = self.pipeline.seek(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::ZERO,
            gst::SeekType::Set,
            pos,
        );
    }

    fn signal_stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for GstReversePipelineHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

pub struct GstAudioOnlyHandle {
    stop_tx: Option<mpsc::Sender<()>>,
    pipeline: gst::Pipeline,
    first_frame_ready: Arc<AtomicBool>,
    _bridge_handle: Option<JoinHandle<()>>,
}

impl GstAudioOnlyHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        path: &Path,
        start_time: f64,
        audio_producer: Arc<Mutex<AudioProducer>>,
        _sample_rate: u32,
        channels: u16,
        speed: f64,
    ) -> Result<Self, String> {
        prewarm_file_sync(path);
        init_once();

        let pipeline = gst::Pipeline::new();

        let filesrc = gst::ElementFactory::make("filesrc")
            .property(
                "location",
                path.to_str()
                    .ok_or_else(|| "Invalid path encoding".to_string())?,
            )
            .build()
            .map_err(|e| format!("Failed to create filesrc: {e}"))?;

        let decodebin = make_element("decodebin")?;

        pipeline
            .add_many([&filesrc, &decodebin])
            .map_err(|e| format!("Failed to add elements: {e}"))?;
        gst::Element::link_many([&filesrc, &decodebin])
            .map_err(|e| format!("Failed to link: {e}"))?;

        let audioconvert = make_element("audioconvert")?;
        let audioresample = make_element("audioresample")?;
        let audio_caps = build_audio_caps();

        let audio_appsink = gst_app::AppSink::builder()
            .caps(&audio_caps)
            .max_buffers(64)
            .drop(false)
            .sync(true)
            .build();

        pipeline
            .add_many([
                &audioconvert,
                &audioresample,
                audio_appsink.upcast_ref::<gst::Element>(),
            ])
            .map_err(|e| format!("Failed to add audio elements: {e}"))?;
        gst::Element::link_many([
            &audioconvert,
            &audioresample,
            audio_appsink.upcast_ref::<gst::Element>(),
        ])
        .map_err(|e| format!("Failed to link audio chain: {e}"))?;

        connect_decodebin_audio_only(&decodebin, &audioconvert);

        preroll_and_seek(&pipeline, start_time, speed)?;

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let first_frame_ready = Arc::new(AtomicBool::new(false));
        let muted = speed < 0.99;
        let ch = channels;

        let bridge_handle = {
            let ffr = first_frame_ready.clone();
            std::thread::Builder::new()
                .name("gst-audio-only-bridge".into())
                .spawn(move || {
                    let _ = audio_appsink.pull_preroll();
                    ffr.store(true, Ordering::Release);

                    loop {
                        if stop_rx.try_recv().is_ok() {
                            return;
                        }

                        let sample = match audio_appsink.try_pull_sample(
                            gst::ClockTime::from_mseconds(if muted { 50 } else { 8 }),
                        ) {
                            Some(s) => s,
                            None => {
                                if audio_appsink.is_eos() {
                                    return;
                                }
                                continue;
                            }
                        };

                        if muted {
                            continue;
                        }

                        let Some(buffer) = sample.buffer() else {
                            continue;
                        };
                        let Ok(map) = buffer.map_readable() else {
                            continue;
                        };

                        push_audio_from_buffer(map.as_slice(), ch, &audio_producer);
                    }
                })
                .expect("failed to spawn gst audio-only bridge thread")
        };

        Ok(Self {
            stop_tx: Some(stop_tx),
            pipeline,
            first_frame_ready,
            _bridge_handle: Some(bridge_handle),
        })
    }

    pub fn is_first_frame_ready(&self) -> bool {
        self.first_frame_ready.load(Ordering::Acquire)
    }

    pub fn begin_playing(&self) -> Result<(), String> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("Failed to set Playing: {e}"))?;
        Ok(())
    }

    fn signal_stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for GstAudioOnlyHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

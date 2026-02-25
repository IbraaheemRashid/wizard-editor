use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::gst_init::*;
use crate::pipeline::{AudioProducer, DecodedFrame};

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
        path: &std::path::Path,
        start_time_seconds: f64,
        target_w: u32,
        target_h: u32,
        audio_producer: Option<Arc<Mutex<AudioProducer>>>,
        _output_sample_rate: u32,
        output_channels: u16,
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
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
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

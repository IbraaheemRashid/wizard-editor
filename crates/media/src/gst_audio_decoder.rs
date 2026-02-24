use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_pbutils;

use crate::gst_init::*;
use crate::pipeline::AudioProducer;

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

pub struct GstAudioDecoder {
    pipeline: gst::Pipeline,
    appsink: gst_app::AppSink,
    last_decode_end: Option<f64>,
}

impl GstAudioDecoder {
    pub fn open(path: &Path) -> Result<Self, String> {
        init_once();

        let pipeline = gst::Pipeline::new();

        let filesrc = gst::ElementFactory::make("filesrc")
            .property("location", path.to_str().unwrap_or_default())
            .build()
            .map_err(|e| format!("Failed to create filesrc: {e}"))?;

        let decodebin = make_element("decodebin")?;
        let audioconvert = make_element("audioconvert")?;
        let audioresample = make_element("audioresample")?;

        let caps = gst::Caps::builder("audio/x-raw")
            .field("format", "F32LE")
            .field("channels", 1i32)
            .field("layout", "interleaved")
            .build();

        let appsink = gst_app::AppSink::builder().caps(&caps).sync(false).build();

        pipeline
            .add_many([
                &filesrc,
                &decodebin,
                &audioconvert,
                &audioresample,
                appsink.upcast_ref::<gst::Element>(),
            ])
            .map_err(|e| format!("Failed to add elements: {e}"))?;

        gst::Element::link_many([&filesrc, &decodebin])
            .map_err(|e| format!("Failed to link: {e}"))?;
        gst::Element::link_many([
            &audioconvert,
            &audioresample,
            appsink.upcast_ref::<gst::Element>(),
        ])
        .map_err(|e| format!("Failed to link audio chain: {e}"))?;

        connect_decodebin_audio_only(&decodebin, &audioconvert);

        if let Err(e) = pipeline.set_state(gst::State::Paused) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(format!("Failed to set Paused: {e}"));
        }

        let bus = match pipeline.bus() {
            Some(b) => b,
            None => {
                let _ = pipeline.set_state(gst::State::Null);
                return Err("No bus".to_string());
            }
        };
        let timeout = gst::ClockTime::from_seconds(10);
        if let Err(e) = wait_for_async_done(&bus, timeout) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(format!("Preroll error: {e}"));
        }

        Ok(Self {
            pipeline,
            appsink,
            last_decode_end: None,
        })
    }

    pub fn has_audio_stream(path: &Path) -> bool {
        init_once();

        let uri = {
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else if let Ok(cwd) = std::env::current_dir() {
                cwd.join(path)
            } else {
                return false;
            };
            format!("file://{}", abs.display())
        };

        let discoverer = match gstreamer_pbutils::Discoverer::new(gst::ClockTime::from_seconds(5)) {
            Ok(d) => d,
            Err(_) => return false,
        };

        let info = match discoverer.discover_uri(&uri) {
            Ok(i) => i,
            Err(_) => return false,
        };

        !info.audio_streams().is_empty()
    }

    pub fn decode_range_mono_f32(
        &mut self,
        start_seconds: f64,
        duration_seconds: f64,
        _target_sample_rate: u32,
    ) -> Vec<f32> {
        let start_seconds = start_seconds.max(0.0);
        if duration_seconds <= 0.0 {
            return Vec::new();
        }

        let seek_pos = gst::ClockTime::from_nseconds((start_seconds * 1_000_000_000.0) as u64);
        let _ = self
            .pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, seek_pos);
        if let Some(bus) = self.pipeline.bus() {
            let _ = wait_for_async_done(&bus, gst::ClockTime::from_seconds(5));
        }

        let _ = self.pipeline.set_state(gst::State::Playing);

        let end_seconds = start_seconds + duration_seconds;
        let mut samples = Vec::new();

        loop {
            let sample = match self
                .appsink
                .try_pull_sample(gst::ClockTime::from_seconds(3))
            {
                Some(s) => s,
                None => break,
            };

            let Some(buffer) = sample.buffer() else {
                continue;
            };

            let pts = buffer
                .pts()
                .map(|p| p.nseconds() as f64 / 1_000_000_000.0)
                .unwrap_or(0.0);

            let buf_duration = buffer
                .duration()
                .map(|d| d.nseconds() as f64 / 1_000_000_000.0)
                .unwrap_or(0.0);

            if let Ok(map) = buffer.map_readable() {
                let data = map.as_slice();
                for chunk in data.chunks_exact(4) {
                    samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }

            if pts + buf_duration >= end_seconds {
                break;
            }
        }

        let _ = self.pipeline.set_state(gst::State::Paused);

        self.last_decode_end = Some(start_seconds + duration_seconds);
        samples
    }
}

impl Drop for GstAudioDecoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(2));
    }
}

use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::gst_pipeline::init_once;

pub fn extract_waveform_peaks(path: &Path, num_peaks: usize) -> Vec<(f32, f32)> {
    if num_peaks == 0 {
        return Vec::new();
    }

    let samples = decode_all_audio_mono(path);
    if samples.is_empty() {
        return Vec::new();
    }

    let total_samples = samples.len();
    let samples_per_peak = (total_samples / num_peaks).max(1);
    let mut peaks = Vec::with_capacity(num_peaks);

    for chunk_start in (0..total_samples).step_by(samples_per_peak) {
        let chunk_end = (chunk_start + samples_per_peak).min(total_samples);
        let mut min_val: f32 = 0.0;
        let mut max_val: f32 = 0.0;
        for &sample in &samples[chunk_start..chunk_end] {
            min_val = min_val.min(sample);
            max_val = max_val.max(sample);
        }
        peaks.push((min_val, max_val));
        if peaks.len() >= num_peaks {
            break;
        }
    }

    peaks
}

fn decode_all_audio_mono(path: &Path) -> Vec<f32> {
    init_once();

    let pipeline = gst::Pipeline::new();

    let filesrc = match gst::ElementFactory::make("filesrc")
        .property("location", path.to_str().unwrap_or_default())
        .build()
    {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let decodebin = match gst::ElementFactory::make("decodebin").build() {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let audioconvert = match gst::ElementFactory::make("audioconvert").build() {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let audioresample = match gst::ElementFactory::make("audioresample").build() {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("channels", 1i32)
        .field("layout", "interleaved")
        .field("rate", 44100i32)
        .build();

    let appsink = gst_app::AppSink::builder().caps(&caps).sync(false).build();

    if pipeline
        .add_many([
            &filesrc,
            &decodebin,
            &audioconvert,
            &audioresample,
            appsink.upcast_ref::<gst::Element>(),
        ])
        .is_err()
    {
        return Vec::new();
    }

    if gst::Element::link_many([&filesrc, &decodebin]).is_err() {
        return Vec::new();
    }

    if gst::Element::link_many([
        &audioconvert,
        &audioresample,
        appsink.upcast_ref::<gst::Element>(),
    ])
    .is_err()
    {
        return Vec::new();
    }

    let audioconvert_weak = audioconvert.downgrade();
    decodebin.connect_pad_added(move |_dbin, src_pad| {
        let pad_caps = match src_pad.current_caps() {
            Some(c) => c,
            None => src_pad.query_caps(None),
        };
        let Some(structure) = pad_caps.structure(0) else {
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

    if pipeline.set_state(gst::State::Playing).is_err() {
        let _ = pipeline.set_state(gst::State::Null);
        return Vec::new();
    }

    let mut all_samples = Vec::new();

    loop {
        match appsink.try_pull_sample(gst::ClockTime::from_seconds(5)) {
            Some(sample) => {
                if let Some(buffer) = sample.buffer() {
                    if let Ok(map) = buffer.map_readable() {
                        let data = map.as_slice();
                        for chunk in data.chunks_exact(4) {
                            all_samples
                                .push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                        }
                    }
                }
            }
            None => {
                if appsink.is_eos() {
                    break;
                }
                break;
            }
        }
    }

    let _ = pipeline.set_state(gst::State::Null);
    all_samples
}

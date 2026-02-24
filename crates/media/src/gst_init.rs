use std::path::Path;
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use ringbuf::traits::Producer;

use crate::pipeline::AudioProducer;

pub fn init_once() {
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

pub(crate) fn prewarm_file_sync(path: &Path) {
    use std::io::Read as _;
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
    }
}

fn prewarm_gst_pipeline(path: &Path) {
    init_once();
    let pipeline = gst::Pipeline::new();
    let Ok(filesrc) = gst::ElementFactory::make("filesrc")
        .property("location", path.to_str().unwrap_or_default())
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

    if pipeline
        .add_many([&filesrc, &decodebin, &fakesink])
        .is_err()
    {
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
}

pub(crate) fn wait_for_async_done(bus: &gst::Bus, timeout: gst::ClockTime) -> Result<(), String> {
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

pub(crate) fn build_video_caps(target_w: u32, target_h: u32) -> gst::Caps {
    use gstreamer_video as gst_video;
    gst_video::VideoCapsBuilder::new()
        .format(gst_video::VideoFormat::Rgba)
        .width(target_w as i32)
        .height(target_h as i32)
        .build()
}

pub(crate) fn build_audio_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("channels", 1i32)
        .field("layout", "interleaved")
        .build()
}

pub(crate) fn make_element(factory_name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory_name)
        .build()
        .map_err(|e| format!("Failed to create {factory_name}: {e}"))
}

pub(crate) fn connect_decodebin_video_only(decodebin: &gst::Element, videoconvert: &gst::Element) {
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

pub(crate) fn connect_decodebin_audio_only(decodebin: &gst::Element, audioconvert: &gst::Element) {
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

pub(crate) fn connect_decodebin_video_and_audio(
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

pub(crate) fn preroll_and_seek(
    pipeline: &gst::Pipeline,
    start_time_seconds: f64,
    speed: f64,
) -> Result<(), String> {
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|e| format!("Failed to set Paused: {e}"))?;

    let bus = pipeline.bus().ok_or("No bus")?;
    let timeout = gst::ClockTime::from_seconds(5);
    wait_for_async_done(&bus, timeout).map_err(|e| format!("Preroll error: {e}"))?;

    if start_time_seconds > 0.01 {
        let seek_pos = gst::ClockTime::from_nseconds((start_time_seconds * 1_000_000_000.0) as u64);
        pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, seek_pos)
            .map_err(|e| format!("Seek failed: {e}"))?;
        wait_for_async_done(&bus, timeout).map_err(|e| format!("Seek error: {e}"))?;
    }

    if (speed - 1.0).abs() > 0.01 {
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
    }

    Ok(())
}

pub(crate) fn push_audio_from_buffer(data: &[u8], ch: u16, producer: &Arc<Mutex<AudioProducer>>) {
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

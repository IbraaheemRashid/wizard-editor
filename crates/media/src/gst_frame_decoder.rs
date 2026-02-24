use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::gst_init::*;

pub struct GstFrameDecoder {
    pipeline: gst::Pipeline,
    appsink: gst_app::AppSink,
    target_w: u32,
    target_h: u32,
    last_decode_ts: Option<f64>,
    duration_secs: Option<f64>,
    is_playing: bool,
}

impl GstFrameDecoder {
    pub fn open(path: &Path, target_w: u32, target_h: u32) -> Result<Self, String> {
        init_once();

        let pipeline = gst::Pipeline::new();

        let filesrc = gst::ElementFactory::make("filesrc")
            .property("location", path.to_str().unwrap_or_default())
            .build()
            .map_err(|e| format!("Failed to create filesrc: {e}"))?;

        let decodebin = make_element("decodebin")?;
        let videoconvert = make_element("videoconvert")?;
        let videoscale = make_element("videoscale")?;

        let video_caps = build_video_caps(target_w, target_h);
        let appsink = gst_app::AppSink::builder()
            .caps(&video_caps)
            .sync(false)
            .build();

        pipeline
            .add_many([
                &filesrc,
                &decodebin,
                &videoconvert,
                &videoscale,
                appsink.upcast_ref::<gst::Element>(),
            ])
            .map_err(|e| format!("Failed to add elements: {e}"))?;

        gst::Element::link_many([&filesrc, &decodebin])
            .map_err(|e| format!("Failed to link filesrc->decodebin: {e}"))?;
        gst::Element::link_many([
            &videoconvert,
            &videoscale,
            appsink.upcast_ref::<gst::Element>(),
        ])
        .map_err(|e| format!("Failed to link video chain: {e}"))?;

        connect_decodebin_video_only(&decodebin, &videoconvert);

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

        let duration_secs: Option<f64> = pipeline
            .query_duration::<gst::ClockTime>()
            .map(|d| d.nseconds() as f64 / 1_000_000_000.0);

        Ok(Self {
            pipeline,
            appsink,
            target_w,
            target_h,
            last_decode_ts: None,
            duration_secs,
            is_playing: false,
        })
    }

    pub fn duration_seconds(&self) -> Option<f64> {
        self.duration_secs
    }

    pub fn last_decode_time(&self) -> Option<f64> {
        self.last_decode_ts
    }

    fn ensure_playing(&mut self) {
        if !self.is_playing {
            let _ = self.pipeline.set_state(gst::State::Playing);
            self.is_playing = true;
        }
    }

    pub fn seek_and_decode(&mut self, time_seconds: f64) -> Option<image::RgbaImage> {
        self.ensure_playing();

        let seek_pos =
            gst::ClockTime::from_nseconds((time_seconds.max(0.0) * 1_000_000_000.0) as u64);
        if self
            .pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, seek_pos)
            .is_err()
        {
            return None;
        }

        self.last_decode_ts = None;
        self.pull_next_frame()
    }

    pub fn decode_next_frame(&mut self) -> Option<image::RgbaImage> {
        self.ensure_playing();
        self.pull_next_frame()
    }

    pub fn decode_next_frame_with_pts(&mut self) -> Option<(image::RgbaImage, f64)> {
        self.ensure_playing();

        let expected_size = (self.target_w as usize) * (self.target_h as usize) * 4;

        let sample = self
            .appsink
            .try_pull_sample(gst::ClockTime::from_seconds(5))?;
        let buffer = sample.buffer()?;
        let pts_seconds = buffer
            .pts()
            .map(|p| p.nseconds() as f64 / 1_000_000_000.0)
            .unwrap_or(0.0);

        let map = buffer.map_readable().ok()?;
        let data = map.as_slice();

        let mut rgba = Vec::with_capacity(expected_size);
        if data.len() >= expected_size {
            rgba.extend_from_slice(&data[..expected_size]);
        } else {
            rgba.extend_from_slice(data);
            rgba.resize(expected_size, 0);
        }

        self.last_decode_ts = Some(pts_seconds);

        image::RgbaImage::from_raw(self.target_w, self.target_h, rgba).map(|img| (img, pts_seconds))
    }

    pub fn seek(&mut self, time_seconds: f64) {
        self.ensure_playing();

        let seek_pos =
            gst::ClockTime::from_nseconds((time_seconds.max(0.0) * 1_000_000_000.0) as u64);
        let _ = self
            .pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, seek_pos);
        self.last_decode_ts = None;
    }

    fn pull_next_frame(&mut self) -> Option<image::RgbaImage> {
        self.decode_next_frame_with_pts().map(|(img, _)| img)
    }
}

impl Drop for GstFrameDecoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(2));
    }
}

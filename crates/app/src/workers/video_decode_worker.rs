use std::sync::mpsc;

use wizard_state::clip::ClipId;

pub const PLAYBACK_DECODE_WIDTH: u32 = 960;
pub const PLAYBACK_DECODE_HEIGHT: u32 = 540;

pub struct VideoDecodeWorkerChannels {
    pub req_tx: mpsc::Sender<(ClipId, std::path::PathBuf, f64)>,
    pub result_rx: mpsc::Receiver<(ClipId, f64, image::RgbaImage)>,
}

pub fn spawn_video_decode_worker() -> VideoDecodeWorkerChannels {
    let (req_tx, req_rx) = mpsc::channel::<(ClipId, std::path::PathBuf, f64)>();
    let (result_tx, result_rx) = mpsc::channel::<(ClipId, f64, image::RgbaImage)>();

    std::thread::spawn(move || {
        let mut cached_decoder: Option<(std::path::PathBuf, wizard_media::decoder::VideoDecoder)> =
            None;
        let mut last_emitted: Option<(ClipId, i64)> = None;
        loop {
            let Ok(mut req) = req_rx.recv() else {
                return;
            };
            while let Ok(next) = req_rx.try_recv() {
                req = next;
            }
            let (clip_id, path, time) = req;

            let needs_new = cached_decoder.as_ref().is_none_or(|(p, _)| p != &path);

            if needs_new {
                cached_decoder = wizard_media::decoder::VideoDecoder::open(&path)
                    .ok()
                    .map(|d| (path.clone(), d));
            }

            if let Some((_, ref mut decoder)) = cached_decoder {
                let bucket = (time * 60.0).round() as i64;
                if last_emitted == Some((clip_id, bucket)) {
                    continue;
                }

                let can_sequential = decoder.last_decode_time().is_some_and(|last| {
                    let diff = time - last;
                    diff > 0.0 && diff < 0.2
                });

                let img = if can_sequential {
                    decoder.decode_next_frame(PLAYBACK_DECODE_WIDTH, PLAYBACK_DECODE_HEIGHT)
                } else {
                    decoder.seek_and_decode(time, PLAYBACK_DECODE_WIDTH, PLAYBACK_DECODE_HEIGHT)
                };

                if let Some(img) = img {
                    let _ = result_tx.send((clip_id, time, img));
                    last_emitted = Some((clip_id, bucket));
                }
            }
        }
    });

    VideoDecodeWorkerChannels { req_tx, result_rx }
}

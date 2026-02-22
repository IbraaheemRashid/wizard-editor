use std::path::PathBuf;
use std::sync::mpsc;

use wizard_state::clip::ClipId;

pub const PLAYBACK_DECODE_WIDTH: u32 = 1920;
pub const PLAYBACK_DECODE_HEIGHT: u32 = 1080;

pub struct VideoDecodeRequest {
    pub clip_id: ClipId,
    pub path: PathBuf,
    pub time_seconds: f64,
}

pub struct VideoDecodeResult {
    pub clip_id: ClipId,
    pub time_seconds: f64,
    pub image: image::RgbaImage,
}

pub struct VideoDecodeWorkerChannels {
    pub req_tx: mpsc::Sender<VideoDecodeRequest>,
    pub result_rx: mpsc::Receiver<VideoDecodeResult>,
}

pub fn spawn_video_decode_worker() -> VideoDecodeWorkerChannels {
    let (req_tx, req_rx) = mpsc::channel::<VideoDecodeRequest>();
    let (result_tx, result_rx) = mpsc::channel::<VideoDecodeResult>();

    std::thread::spawn(move || {
        let mut cached_decoder: Option<(PathBuf, wizard_media::decoder::VideoDecoder)> = None;
        let mut last_emitted: Option<(ClipId, i64)> = None;
        loop {
            let Ok(mut req) = req_rx.recv() else {
                return;
            };
            while let Ok(next) = req_rx.try_recv() {
                req = next;
            }

            let needs_new = cached_decoder.as_ref().is_none_or(|(p, _)| p != &req.path);

            if needs_new {
                cached_decoder = wizard_media::decoder::VideoDecoder::open(&req.path)
                    .ok()
                    .map(|d| (req.path.clone(), d));
            }

            if let Some((_, ref mut decoder)) = cached_decoder {
                let bucket = (req.time_seconds * 60.0).round() as i64;
                if last_emitted == Some((req.clip_id, bucket)) {
                    continue;
                }

                let can_sequential = decoder.last_decode_time().is_some_and(|last| {
                    let diff = req.time_seconds - last;
                    diff > 0.0 && diff < 0.2
                });

                let img = if can_sequential {
                    decoder.decode_next_frame(PLAYBACK_DECODE_WIDTH, PLAYBACK_DECODE_HEIGHT)
                } else {
                    decoder.seek_and_decode(
                        req.time_seconds,
                        PLAYBACK_DECODE_WIDTH,
                        PLAYBACK_DECODE_HEIGHT,
                    )
                };

                if let Some(img) = img {
                    let _ = result_tx.send(VideoDecodeResult {
                        clip_id: req.clip_id,
                        time_seconds: req.time_seconds,
                        image: img,
                    });
                    last_emitted = Some((req.clip_id, bucket));
                }
            }
        }
    });

    VideoDecodeWorkerChannels { req_tx, result_rx }
}

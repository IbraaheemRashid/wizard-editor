use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use wizard_media::gst_pipeline::GstFrameDecoder;
use wizard_state::clip::ClipId;

pub const PLAYBACK_DECODE_WIDTH: u32 = 1920;
pub const PLAYBACK_DECODE_HEIGHT: u32 = 1080;

const FRAME_CACHE_CAPACITY: usize = 64;
const DECODER_LRU_CAPACITY: usize = 4;

pub struct VideoDecodeRequest {
    pub clip_id: ClipId,
    pub path: PathBuf,
    pub time_seconds: f64,
    pub target_width: u32,
    pub target_height: u32,
    pub max_decode_frames: u32,
}

pub struct VideoDecodeResult {
    pub clip_id: ClipId,
    pub time_seconds: f64,
    pub image: Arc<image::RgbaImage>,
}

pub struct VideoDecodeWorkerChannels {
    pub req_tx: mpsc::Sender<VideoDecodeRequest>,
    pub result_rx: mpsc::Receiver<VideoDecodeResult>,
}

struct FrameCacheEntry {
    image: Arc<image::RgbaImage>,
    order: u64,
}

pub fn spawn_video_decode_worker() -> VideoDecodeWorkerChannels {
    let (req_tx, req_rx) = mpsc::channel::<VideoDecodeRequest>();
    let (result_tx, result_rx) = mpsc::channel::<VideoDecodeResult>();

    std::thread::spawn(move || {
        let mut decoder_lru: VecDeque<(PathBuf, GstFrameDecoder)> =
            VecDeque::with_capacity(DECODER_LRU_CAPACITY);
        let mut last_emitted: Option<(ClipId, i64)> = None;
        let mut frame_cache: HashMap<(ClipId, i64), FrameCacheEntry> = HashMap::new();
        let mut cache_order: u64 = 0;

        loop {
            let Ok(mut req) = req_rx.recv() else {
                return;
            };
            while let Ok(next) = req_rx.try_recv() {
                req = next;
            }

            let lru_idx = decoder_lru.iter().position(|(p, _)| p == &req.path);
            let decoder_idx = if let Some(idx) = lru_idx {
                if idx != 0 {
                    let entry = decoder_lru.remove(idx).expect("index valid");
                    decoder_lru.push_front(entry);
                }
                0
            } else {
                match GstFrameDecoder::open(&req.path, req.target_width, req.target_height) {
                    Ok(d) => {
                        if decoder_lru.len() >= DECODER_LRU_CAPACITY {
                            decoder_lru.pop_back();
                        }
                        decoder_lru.push_front((req.path.clone(), d));
                        0
                    }
                    Err(_) => continue,
                }
            };

            let (_, ref mut decoder) = decoder_lru[decoder_idx];

            let bucket = (req.time_seconds * 60.0).round() as i64;
            if last_emitted == Some((req.clip_id, bucket)) {
                continue;
            }

            let cache_key = (req.clip_id, bucket);
            if let Some(entry) = frame_cache.get(&cache_key) {
                let _ = result_tx.send(VideoDecodeResult {
                    clip_id: req.clip_id,
                    time_seconds: req.time_seconds,
                    image: Arc::clone(&entry.image),
                });
                last_emitted = Some((req.clip_id, bucket));
                continue;
            }

            let can_sequential = decoder.last_decode_time().is_some_and(|last| {
                let diff = req.time_seconds - last;
                diff > 0.0 && diff < 0.2
            });

            let img = if can_sequential {
                decoder.decode_next_frame()
            } else {
                decoder.seek_and_decode(req.time_seconds)
            };

            if let Some(img) = img {
                let img = Arc::new(img);

                if frame_cache.len() >= FRAME_CACHE_CAPACITY {
                    if let Some(oldest_key) = frame_cache
                        .iter()
                        .min_by_key(|(_, v)| v.order)
                        .map(|(k, _)| *k)
                    {
                        frame_cache.remove(&oldest_key);
                    }
                }
                cache_order += 1;
                frame_cache.insert(
                    cache_key,
                    FrameCacheEntry {
                        image: Arc::clone(&img),
                        order: cache_order,
                    },
                );

                let _ = result_tx.send(VideoDecodeResult {
                    clip_id: req.clip_id,
                    time_seconds: req.time_seconds,
                    image: img,
                });
                last_emitted = Some((req.clip_id, bucket));
            }
        }
    });

    VideoDecodeWorkerChannels { req_tx, result_rx }
}

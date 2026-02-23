use std::collections::{HashSet, VecDeque};
use std::sync::{mpsc, Arc, Mutex};

use wizard_state::clip::ClipId;

use crate::constants::{
    SCRUB_CACHE_FPS, SCRUB_CACHE_HEIGHT, SCRUB_CACHE_MAX_FRAMES, SCRUB_CACHE_WIDTH,
    SCRUB_CACHE_WORKER_COUNT,
};

pub enum ScrubCacheRequest {
    Extract {
        clip_id: ClipId,
        path: std::path::PathBuf,
    },
    Invalidate {
        clip_id: ClipId,
    },
}

pub struct ScrubCacheFrame {
    pub clip_id: ClipId,
    pub index: usize,
    pub total: usize,
    pub pts_seconds: f64,
    pub image: image::RgbaImage,
}

pub struct ScrubCacheWorkerChannels {
    pub req_tx: mpsc::Sender<ScrubCacheRequest>,
    pub result_rx: mpsc::Receiver<ScrubCacheFrame>,
}

fn apply_scrub_req(
    req: ScrubCacheRequest,
    queue: &mut VecDeque<(ClipId, std::path::PathBuf)>,
    queued: &mut HashSet<ClipId>,
) {
    match req {
        ScrubCacheRequest::Extract { clip_id, path } => {
            if queued.contains(&clip_id) {
                return;
            }
            queued.insert(clip_id);
            queue.push_back((clip_id, path));
        }
        ScrubCacheRequest::Invalidate { clip_id } => {
            queue.retain(|(id, _)| *id != clip_id);
            queued.remove(&clip_id);
        }
    }
}

pub fn spawn_scrub_cache_worker() -> ScrubCacheWorkerChannels {
    let (req_tx, req_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let (work_tx, work_rx) = mpsc::channel::<(ClipId, std::path::PathBuf)>();
    let work_rx = Arc::new(Mutex::new(work_rx));

    std::thread::spawn(move || {
        let mut queue: VecDeque<(ClipId, std::path::PathBuf)> = VecDeque::new();
        let mut queued: HashSet<ClipId> = HashSet::new();

        loop {
            let item = if let Some(item) = queue.pop_front() {
                queued.remove(&item.0);
                item
            } else {
                let Ok(req) = req_rx.recv() else {
                    return;
                };
                apply_scrub_req(req, &mut queue, &mut queued);
                continue;
            };

            while let Ok(req) = req_rx.try_recv() {
                apply_scrub_req(req, &mut queue, &mut queued);
            }

            if work_tx.send(item).is_err() {
                return;
            }
        }
    });

    for _ in 0..SCRUB_CACHE_WORKER_COUNT {
        let work_rx = Arc::clone(&work_rx);
        let result_tx = result_tx.clone();
        std::thread::spawn(move || loop {
            let (clip_id, path) = {
                let rx = work_rx.lock().expect("work_rx lock poisoned");
                match rx.recv() {
                    Ok(item) => item,
                    Err(_) => return,
                }
            };

            let duration = {
                let decoder = match wizard_media::decoder::VideoDecoder::open(&path) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                match decoder.duration_seconds() {
                    Some(d) if d > 0.0 => d,
                    _ => continue,
                }
            };

            let frame_count = ((duration * SCRUB_CACHE_FPS).ceil() as usize).clamp(1, SCRUB_CACHE_MAX_FRAMES);

            let (frame_tx, frame_rx) = mpsc::channel();
            wizard_media::thumbnail::extract_frames_streaming(
                &path,
                frame_count,
                SCRUB_CACHE_WIDTH,
                SCRUB_CACHE_HEIGHT,
                &frame_tx,
            );
            drop(frame_tx);

            while let Ok((index, pts, image)) = frame_rx.recv() {
                if result_tx
                    .send(ScrubCacheFrame {
                        clip_id,
                        index,
                        total: frame_count,
                        pts_seconds: pts,
                        image,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
    }
    drop(result_tx);

    ScrubCacheWorkerChannels { req_tx, result_rx }
}

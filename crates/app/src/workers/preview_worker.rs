use std::collections::{HashSet, VecDeque};
use std::sync::mpsc;

use wizard_state::clip::ClipId;

const PREVIEW_FRAME_COUNT: usize = 16;

pub enum PreviewRequest {
    Enqueue {
        clip_id: ClipId,
        path: std::path::PathBuf,
        priority: bool,
    },
}

pub struct PreviewFrame {
    pub clip_id: ClipId,
    pub index: usize,
    pub total: usize,
    pub image: image::RgbaImage,
}

fn apply_preview_req(
    req: PreviewRequest,
    queue: &mut VecDeque<(ClipId, std::path::PathBuf)>,
    queued: &mut HashSet<ClipId>,
) {
    match req {
        PreviewRequest::Enqueue {
            clip_id,
            path,
            priority,
        } => {
            if queued.contains(&clip_id) {
                return;
            }
            queued.insert(clip_id);
            if priority {
                queue.push_front((clip_id, path));
            } else {
                queue.push_back((clip_id, path));
            }
        }
    }
}

pub struct PreviewWorkerChannels {
    pub req_tx: mpsc::Sender<PreviewRequest>,
    pub result_rx: mpsc::Receiver<PreviewFrame>,
}

pub fn spawn_preview_worker() -> PreviewWorkerChannels {
    let (req_tx, req_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let mut queue: VecDeque<(ClipId, std::path::PathBuf)> = VecDeque::new();
        let mut queued: HashSet<ClipId> = HashSet::new();

        loop {
            let (clip_id, path) = if let Some(item) = queue.pop_front() {
                queued.remove(&item.0);
                item
            } else {
                let Ok(req) = req_rx.recv() else {
                    return;
                };
                apply_preview_req(req, &mut queue, &mut queued);
                continue;
            };

            while let Ok(req) = req_rx.try_recv() {
                apply_preview_req(req, &mut queue, &mut queued);
            }

            let (frame_tx, frame_rx) = mpsc::channel();
            wizard_media::thumbnail::extract_preview_frames_streaming(
                &path,
                PREVIEW_FRAME_COUNT,
                &frame_tx,
            );
            drop(frame_tx);

            while let Ok((index, image)) = frame_rx.recv() {
                if result_tx
                    .send(PreviewFrame {
                        clip_id,
                        index,
                        total: PREVIEW_FRAME_COUNT,
                        image,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
    });

    PreviewWorkerChannels { req_tx, result_rx }
}

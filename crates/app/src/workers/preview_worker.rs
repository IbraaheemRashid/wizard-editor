use std::collections::{HashSet, VecDeque};
use std::sync::mpsc;

use wizard_state::clip::ClipId;

pub enum PreviewRequest {
    Enqueue {
        clip_id: ClipId,
        path: std::path::PathBuf,
        priority: bool,
    },
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
    pub result_rx: mpsc::Receiver<(ClipId, Vec<image::RgbaImage>)>,
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

            let frames = wizard_media::thumbnail::extract_preview_frames(&path, 32);
            let _ = result_tx.send((clip_id, frames));
        }
    });

    PreviewWorkerChannels { req_tx, result_rx }
}

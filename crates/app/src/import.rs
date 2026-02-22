use std::path::PathBuf;

use notify::{RecursiveMode, Watcher};

use crate::EditorApp;

impl EditorApp {
    pub fn import_file(&mut self, p: PathBuf) {
        if self.known_paths.contains(&p) {
            return;
        }
        self.known_paths.insert(p.clone());

        let clip = wizard_state::clip::Clip::from_path(p.clone());
        let clip_id = clip.id;
        self.state.project.add_clip(clip);
        self.textures.pending_thumbnails.insert(clip_id);

        let ttx = self.thumb_tx.clone();
        let mtx = self.meta_tx.clone();
        let wtx = self.waveform_tx.clone();
        std::thread::spawn(move || {
            let meta = wizard_media::metadata::extract_metadata(&p);
            let _ = mtx.send((clip_id, meta));

            if let Some(img) = wizard_media::thumbnail::extract_thumbnail(&p) {
                let _ = ttx.send((clip_id, img));
            }

            let peaks = wizard_media::audio::extract_waveform_peaks(&p, 512);
            if !peaks.is_empty() {
                let _ = wtx.send((clip_id, peaks));
            }
        });
    }

    pub fn import_folder(&mut self, path: PathBuf) {
        let files = wizard_media::import::scan_folder(&path);
        for p in files {
            self.import_file(p);
        }

        let tx = self.watch_tx.clone();
        let watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                let Ok(event) = res else { return };
                use notify::EventKind;
                if !matches!(event.kind, EventKind::Create(_)) {
                    return;
                }
                for p in event.paths {
                    if p.is_file() {
                        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                            if wizard_media::import::VIDEO_EXTENSIONS
                                .contains(&ext.to_lowercase().as_str())
                            {
                                let _ = tx.send(p);
                            }
                        }
                    }
                }
            });
        if let Ok(mut w) = watcher {
            let _ = w.watch(&path, RecursiveMode::Recursive);
            self.folder_watcher = Some(w);
        }
    }

    pub fn poll_folder_watcher(&mut self) {
        while let Ok(path) = self.watch_rx.try_recv() {
            self.import_file(path);
        }
    }
}

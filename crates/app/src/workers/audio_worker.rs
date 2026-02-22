use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

pub enum AudioPreviewRequest {
    Stop,
    Preview {
        path: PathBuf,
        time_seconds: f64,
        sample_rate_hz: u32,
    },
}

pub struct AudioSnippet {
    pub samples_mono: Vec<f32>,
}

pub struct AudioWorkerChannels {
    pub req_tx: mpsc::Sender<AudioPreviewRequest>,
    pub snippet_rx: mpsc::Receiver<AudioSnippet>,
}

pub fn spawn_audio_worker(no_audio_paths: Arc<Mutex<HashSet<PathBuf>>>) -> AudioWorkerChannels {
    let (req_tx, req_rx) = mpsc::channel();
    let (snippet_tx, snippet_rx) = mpsc::sync_channel(8);

    std::thread::spawn(move || {
        let mut cached_decoder: Option<(PathBuf, wizard_media::decoder::AudioDecoder)> = None;

        let open_decoder = |path: &std::path::Path,
                            no_audio: &Arc<Mutex<HashSet<PathBuf>>>|
         -> Option<wizard_media::decoder::AudioDecoder> {
            match wizard_media::decoder::AudioDecoder::open(path) {
                Ok(d) => Some(d),
                Err(_err) => {
                    if let Ok(streams) = wizard_media::decoder::probe_streams(path) {
                        let has_audio_stream = streams.iter().any(|s| s.medium == "Audio");
                        if !has_audio_stream {
                            if let Ok(mut set) = no_audio.lock() {
                                set.insert(path.to_path_buf());
                            }
                        }
                    }
                    None
                }
            }
        };

        let ensure_decoder =
            |cached: &mut Option<(PathBuf, wizard_media::decoder::AudioDecoder)>,
             path: &std::path::Path,
             no_audio: &Arc<Mutex<HashSet<PathBuf>>>| {
                let needs_new = cached.as_ref().is_none_or(|(p, _)| p != path);
                if needs_new {
                    *cached = open_decoder(path, no_audio).map(|d| (path.to_path_buf(), d));
                }
            };

        loop {
            let Ok(req) = req_rx.recv() else {
                return;
            };

            match req {
                AudioPreviewRequest::Stop => while req_rx.try_recv().is_ok() {},
                AudioPreviewRequest::Preview {
                    path,
                    time_seconds,
                    sample_rate_hz,
                } => {
                    if no_audio_paths
                        .lock()
                        .map(|set| set.contains(&path))
                        .unwrap_or(false)
                    {
                        continue;
                    }

                    ensure_decoder(&mut cached_decoder, &path, &no_audio_paths);

                    if let Some((_, ref mut decoder)) = cached_decoder {
                        let samples = decoder.decode_range_mono_f32(
                            time_seconds.max(0.0),
                            0.5,
                            sample_rate_hz,
                        );
                        let _ = snippet_tx.send(AudioSnippet {
                            samples_mono: samples,
                        });
                    }
                }
            }
        }
    });

    AudioWorkerChannels { req_tx, snippet_rx }
}

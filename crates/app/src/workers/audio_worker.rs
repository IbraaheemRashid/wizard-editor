use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use wizard_media::gst_pipeline::GstAudioDecoder;

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
        let mut cached_decoder: Option<(PathBuf, GstAudioDecoder)> = None;

        let open_decoder = |path: &std::path::Path,
                            no_audio: &Arc<Mutex<HashSet<PathBuf>>>|
         -> Option<GstAudioDecoder> {
            match GstAudioDecoder::open(path) {
                Ok(d) => Some(d),
                Err(_) => {
                    if !GstAudioDecoder::has_audio_stream(path) {
                        if let Ok(mut set) = no_audio.lock() {
                            set.insert(path.to_path_buf());
                        }
                    }
                    None
                }
            }
        };

        let ensure_decoder =
            |cached: &mut Option<(PathBuf, GstAudioDecoder)>,
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
                        let mut samples = decoder.decode_range_mono_f32(
                            time_seconds.max(0.0),
                            1.0,
                            sample_rate_hz,
                        );
                        apply_fade(&mut samples, sample_rate_hz);
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

fn apply_fade(samples: &mut [f32], sample_rate: u32) {
    let fade_samples = ((sample_rate as f32 * 0.01) as usize).max(1);
    let len = samples.len();
    if len == 0 {
        return;
    }
    let fade_in = fade_samples.min(len / 2);
    let fade_out = fade_samples.min(len / 2);
    for (i, sample) in samples[..fade_in].iter_mut().enumerate() {
        *sample *= i as f32 / fade_in as f32;
    }
    for i in 0..fade_out {
        samples[len - 1 - i] *= i as f32 / fade_out as f32;
    }
}

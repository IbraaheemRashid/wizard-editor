use std::sync::{Arc, Mutex};

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use wizard_audio::output::{AudioConsumer, AudioProducer};
use wizard_media::pipeline::AudioOnlyHandle;

struct AudioSource {
    consumer: AudioConsumer,
    _handle: AudioOnlyHandle,
}

pub struct AudioMixer {
    pub output: Arc<Mutex<AudioProducer>>,
    sources: Vec<AudioSource>,
    mix_buf: Vec<f32>,
}

const SOURCE_RING_SIZE: usize = 65536;
const MIX_BUF_MAX: usize = 4096;
static LAST_MIXER_UNDERFLOW_LOG_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

impl AudioMixer {
    pub fn new(output: Arc<Mutex<AudioProducer>>) -> Self {
        Self {
            output,
            sources: Vec::new(),
            mix_buf: vec![0.0f32; MIX_BUF_MAX],
        }
    }

    pub fn create_source_producer() -> (AudioProducer, AudioConsumer) {
        let rb = HeapRb::<f32>::new(SOURCE_RING_SIZE);
        rb.split()
    }

    pub fn add_source(&mut self, handle: AudioOnlyHandle, consumer: AudioConsumer) {
        self.sources.push(AudioSource {
            consumer,
            _handle: handle,
        });
    }

    pub fn mix_tick(&mut self) {
        if self.sources.is_empty() {
            return;
        }

        let max_available = self
            .sources
            .iter()
            .map(|s| s.consumer.occupied_len())
            .max()
            .unwrap_or(0);

        if max_available == 0 {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let last = LAST_MIXER_UNDERFLOW_LOG_MS.load(std::sync::atomic::Ordering::Relaxed);
            if now_ms.saturating_sub(last) > 500 {
                LAST_MIXER_UNDERFLOW_LOG_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
                // #region agent log
                crate::debug_log::emit(
                    "H4",
                    "crates/app/src/audio_mixer.rs:mix_tick",
                    "audio mixer underflow detected",
                    serde_json::json!({
                        "sourceCount": self.sources.len(),
                        "maxAvailableSamples": max_available
                    }),
                );
                // #endregion
            }
            return;
        }

        let mix_len = max_available.min(MIX_BUF_MAX);
        let buf = &mut self.mix_buf[..mix_len];
        buf.fill(0.0);

        for source in &mut self.sources {
            let avail = source.consumer.occupied_len();
            for slot in buf.iter_mut().take(avail.min(mix_len)) {
                if let Some(sample) = source.consumer.try_pop() {
                    *slot += sample;
                }
            }
        }

        for sample in buf.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }

        if let Ok(mut producer) = self.output.lock() {
            producer.push_slice(buf);
        }
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub fn clear(&mut self) {
        self.sources.clear();
    }

    pub fn replace_sources(&mut self, new_sources: Vec<(AudioOnlyHandle, AudioConsumer)>) {
        self.sources.clear();
        for (handle, consumer) in new_sources {
            self.sources.push(AudioSource {
                consumer,
                _handle: handle,
            });
        }
    }
}

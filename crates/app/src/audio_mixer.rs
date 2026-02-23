use std::sync::{Arc, Mutex};

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use wizard_audio::output::{AudioConsumer, AudioProducer};
use wizard_media::pipeline::AudioOnlyHandle;

fn append_debug_log(location: &str, message: &str, hypothesis_id: &str, data_json: String) {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let id = format!("log_{timestamp}_{hypothesis_id}");
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/Users/irashid/personal/wizard-editor/.cursor/debug.log")
    {
        let _ = writeln!(
            file,
            "{{\"id\":\"{}\",\"timestamp\":{},\"location\":\"{}\",\"message\":\"{}\",\"data\":{},\"runId\":\"audio-resampler-debug\",\"hypothesisId\":\"{}\"}}",
            id, timestamp, location, message, data_json, hypothesis_id
        );
    }
}

struct AudioSource {
    consumer: AudioConsumer,
    _handle: AudioOnlyHandle,
}

pub struct AudioMixer {
    pub output: Arc<Mutex<AudioProducer>>,
    sources: Vec<AudioSource>,
    mix_buf: Vec<f32>,
    logged_no_input_with_sources: bool,
    logged_output_backpressure: bool,
}

const SOURCE_RING_SIZE: usize = 16384;

impl AudioMixer {
    pub fn new(output: Arc<Mutex<AudioProducer>>) -> Self {
        Self {
            output,
            sources: Vec::new(),
            mix_buf: Vec::with_capacity(4096),
            logged_no_input_with_sources: false,
            logged_output_backpressure: false,
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
            if !self.logged_no_input_with_sources {
                // #region agent log
                append_debug_log(
                    "crates/app/src/audio_mixer.rs:mix_tick",
                    "mixer has sources but no available input samples",
                    "H11",
                    format!("{{\"sourceCount\":{}}}", self.sources.len()),
                );
                // #endregion
                self.logged_no_input_with_sources = true;
            }
            return;
        }

        self.mix_buf.clear();
        self.mix_buf.resize(max_available, 0.0f32);

        for source in &mut self.sources {
            let avail = source.consumer.occupied_len();
            for i in 0..avail.min(max_available) {
                if let Some(sample) = source.consumer.try_pop() {
                    self.mix_buf[i] += sample;
                }
            }
        }

        for sample in &mut self.mix_buf {
            *sample = sample.clamp(-1.0, 1.0);
        }

        if let Ok(mut producer) = self.output.lock() {
            let pushed = producer.push_slice(&self.mix_buf);
            if !self.logged_output_backpressure && pushed < self.mix_buf.len() {
                // #region agent log
                append_debug_log(
                    "crates/app/src/audio_mixer.rs:mix_tick",
                    "output producer backpressure dropped mixed samples",
                    "H12",
                    format!(
                        "{{\"sourceCount\":{},\"maxAvailable\":{},\"mixLen\":{},\"pushed\":{}}}",
                        self.sources.len(),
                        max_available,
                        self.mix_buf.len(),
                        pushed
                    ),
                );
                // #endregion
                self.logged_output_backpressure = true;
            }
        }
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub fn clear(&mut self) {
        self.sources.clear();
        self.logged_no_input_with_sources = false;
        self.logged_output_backpressure = false;
    }
}

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat};

#[derive(Clone)]
pub struct AudioOutput {
    state: Arc<Mutex<AudioOutputState>>,
    _stream: Arc<cpal::Stream>,
}

struct AudioOutputState {
    samples_mono: VecDeque<f32>,
    sample_rate_hz: u32,
    channels: usize,
}

impl AudioOutput {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "No default output device".to_string())?;

        let supported = device
            .default_output_config()
            .map_err(|e| format!("Failed to get default output config: {e}"))?;

        let sample_rate_hz = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let state = Arc::new(Mutex::new(AudioOutputState {
            samples_mono: VecDeque::new(),
            sample_rate_hz,
            channels: channels.max(1),
        }));

        let err_fn = |err| {
            eprintln!("audio stream error: {err}");
        };

        let stream = match sample_format {
            SampleFormat::F32 => build_stream::<f32>(device, &config, state.clone(), err_fn)?,
            SampleFormat::I16 => build_stream::<i16>(device, &config, state.clone(), err_fn)?,
            SampleFormat::U16 => build_stream::<u16>(device, &config, state.clone(), err_fn)?,
            other => return Err(format!("Unsupported sample format: {other}")),
        };

        stream
            .play()
            .map_err(|e| format!("Failed to start audio stream: {e}"))?;

        Ok(Self {
            state,
            _stream: Arc::new(stream),
        })
    }

    pub fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.samples_mono.clear();
        }
    }

    pub fn enqueue_mono_samples(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            state.samples_mono.extend(samples.iter().copied());
        }
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.state
            .lock()
            .map(|s| s.sample_rate_hz)
            .unwrap_or(48_000)
    }

    pub fn channels(&self) -> usize {
        self.state.lock().map(|s| s.channels).unwrap_or(2)
    }
}

fn build_stream<T>(
    device: cpal::Device,
    config: &cpal::StreamConfig,
    state: Arc<Mutex<AudioOutputState>>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: Sample + FromSample<f32> + cpal::SizedSample,
{
    let channels = config.channels as usize;
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut guard = match state.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };

                let mut i = 0;
                while i < data.len() {
                    let sample = guard.samples_mono.pop_front().unwrap_or(0.0);
                    for c in 0..channels {
                        if i + c < data.len() {
                            data[i + c] = T::from_sample(sample);
                        }
                    }
                    i += channels;
                }
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("Failed to build output stream: {e}"))
}

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

pub type AudioProducer = ringbuf::HeapProd<f32>;
pub type AudioConsumer = ringbuf::HeapCons<f32>;

pub struct AudioOutput {
    _stream: cpal::Stream,
    sample_rate_hz: u32,
    channels: u16,
    consumer_slot: Arc<Mutex<AudioConsumer>>,
}

impl AudioOutput {
    pub fn new() -> Result<(Self, AudioProducer), String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "No default output device".to_string())?;

        let supported = device
            .default_output_config()
            .map_err(|e| format!("Failed to get default output config: {e}"))?;

        let sample_rate_hz = supported.sample_rate().0;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels;

        let rb = HeapRb::<f32>::new(sample_rate_hz as usize / 4);
        let (producer, consumer) = rb.split();

        let consumer_slot = Arc::new(Mutex::new(consumer));

        let err_fn = |err| {
            eprintln!("audio stream error: {err}");
        };

        let stream = match sample_format {
            SampleFormat::F32 => {
                build_stream::<f32>(device, &config, consumer_slot.clone(), err_fn)?
            }
            SampleFormat::I16 => {
                build_stream::<i16>(device, &config, consumer_slot.clone(), err_fn)?
            }
            SampleFormat::U16 => {
                build_stream::<u16>(device, &config, consumer_slot.clone(), err_fn)?
            }
            other => return Err(format!("Unsupported sample format: {other}")),
        };

        stream
            .play()
            .map_err(|e| format!("Failed to start audio stream: {e}"))?;

        Ok((
            Self {
                _stream: stream,
                sample_rate_hz,
                channels,
                consumer_slot,
            },
            producer,
        ))
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn swap_buffer(&self) -> AudioProducer {
        let rb = HeapRb::<f32>::new(self.sample_rate_hz as usize / 4);
        let (producer, consumer) = rb.split();
        if let Ok(mut slot) = self.consumer_slot.lock() {
            *slot = consumer;
        }
        producer
    }

    pub fn swap_consumer(&self, consumer: AudioConsumer) {
        if let Ok(mut slot) = self.consumer_slot.lock() {
            *slot = consumer;
        }
    }
}

pub fn enqueue_samples(producer: &mut AudioProducer, samples: &[f32], channels: u16) {
    if samples.is_empty() {
        return;
    }
    let ch = channels as usize;
    if ch <= 1 {
        let _ = producer.push_slice(samples);
    } else {
        for &s in samples {
            for _ in 0..ch {
                let _ = producer.try_push(s);
            }
        }
    }
}

fn build_stream<T>(
    device: cpal::Device,
    config: &cpal::StreamConfig,
    consumer_slot: Arc<Mutex<AudioConsumer>>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: Sample + FromSample<f32> + cpal::SizedSample,
{
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                if let Ok(mut consumer) = consumer_slot.try_lock() {
                    for sample in data.iter_mut() {
                        let s = consumer.try_pop().unwrap_or(0.0);
                        *sample = T::from_sample(s);
                    }
                } else {
                    for sample in data.iter_mut() {
                        *sample = T::from_sample(0.0);
                    }
                }
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("Failed to build output stream: {e}"))
}

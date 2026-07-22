//! Default-microphone capture for the protocol spike.
//!
//! Audio stays in callback buffers and the in-memory channel only. The code
//! never creates a WAV file and never logs samples or transcript text.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use tokio::sync::mpsc::UnboundedSender;

use crate::spike::error::SpikeError;

const TARGET_RATE: u32 = 16_000;

pub struct Recorder {
    stream: cpal::Stream,
    stream_failed: Arc<AtomicBool>,
}

impl Recorder {
    pub fn start_default(tx: UnboundedSender<Vec<u8>>) -> Result<Self, SpikeError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(SpikeError::MicrophoneUnavailable)?;
        let supported = device
            .default_input_config()
            .map_err(|_| SpikeError::MicrophoneUnavailable)?;
        let config: StreamConfig = supported.config();
        let converter = Arc::new(Mutex::new(PcmConverter::new(
            config.channels as usize,
            config.sample_rate.0,
        )));
        let stream_failed = Arc::new(AtomicBool::new(false));

        let stream = match supported.sample_format() {
            SampleFormat::F32 => build_stream_f32(
                &device,
                &config,
                Arc::clone(&converter),
                tx,
                Arc::clone(&stream_failed),
            ),
            SampleFormat::I16 => build_stream_i16(
                &device,
                &config,
                Arc::clone(&converter),
                tx,
                Arc::clone(&stream_failed),
            ),
            SampleFormat::U16 => {
                build_stream_u16(&device, &config, converter, tx, Arc::clone(&stream_failed))
            }
            _ => return Err(SpikeError::MicrophoneUnavailable),
        }
        .map_err(|_| SpikeError::MicrophoneUnavailable)?;

        stream.play().map_err(|_| SpikeError::MicrophoneFailed)?;
        Ok(Self {
            stream,
            stream_failed,
        })
    }

    pub fn stop(self) -> Result<(), SpikeError> {
        let _ = self.stream.pause();
        if self.stream_failed.load(Ordering::Acquire) {
            return Err(SpikeError::MicrophoneFailed);
        }
        Ok(())
    }
}

fn build_stream_f32(
    device: &cpal::Device,
    config: &StreamConfig,
    converter: Arc<Mutex<PcmConverter>>,
    tx: UnboundedSender<Vec<u8>>,
    stream_failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    device.build_input_stream(
        config,
        move |samples: &[f32], _| forward(samples, &converter, &tx),
        move |_| stream_failed.store(true, Ordering::Release),
        None,
    )
}

fn build_stream_i16(
    device: &cpal::Device,
    config: &StreamConfig,
    converter: Arc<Mutex<PcmConverter>>,
    tx: UnboundedSender<Vec<u8>>,
    stream_failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    device.build_input_stream(
        config,
        move |samples: &[i16], _| {
            let normalized: Vec<f32> = samples
                .iter()
                .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
                .collect();
            forward(&normalized, &converter, &tx);
        },
        move |_| stream_failed.store(true, Ordering::Release),
        None,
    )
}

fn build_stream_u16(
    device: &cpal::Device,
    config: &StreamConfig,
    converter: Arc<Mutex<PcmConverter>>,
    tx: UnboundedSender<Vec<u8>>,
    stream_failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    device.build_input_stream(
        config,
        move |samples: &[u16], _| {
            let normalized: Vec<f32> = samples
                .iter()
                .map(|sample| (f32::from(*sample) / f32::from(u16::MAX)) * 2.0 - 1.0)
                .collect();
            forward(&normalized, &converter, &tx);
        },
        move |_| stream_failed.store(true, Ordering::Release),
        None,
    )
}

fn forward(samples: &[f32], converter: &Arc<Mutex<PcmConverter>>, tx: &UnboundedSender<Vec<u8>>) {
    let pcm = converter
        .lock()
        .expect("audio converter lock poisoned")
        .convert(samples);
    if !pcm.is_empty() {
        let _ = tx.send(pcm);
    }
}

struct PcmConverter {
    channels: usize,
    input_rate: u32,
    phase: f64,
    previous: f32,
}

impl PcmConverter {
    fn new(channels: usize, input_rate: u32) -> Self {
        Self {
            channels: channels.max(1),
            input_rate,
            phase: 0.0,
            previous: 0.0,
        }
    }

    fn convert(&mut self, interleaved: &[f32]) -> Vec<u8> {
        let mono = downmix(interleaved, self.channels);
        let samples = self.resample(&mono);
        let mut pcm = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            pcm.extend_from_slice(&value.to_le_bytes());
        }
        pcm
    }

    fn resample(&mut self, samples: &[f32]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }
        if self.input_rate == TARGET_RATE {
            self.previous = *samples.last().unwrap_or(&self.previous);
            return samples.to_vec();
        }

        let step = f64::from(self.input_rate) / f64::from(TARGET_RATE);
        let mut phase = self.phase;
        let mut output = Vec::with_capacity(((samples.len() as f64) / step).ceil() as usize + 1);
        while phase < samples.len() as f64 {
            let index = phase.floor() as usize;
            let fraction = (phase - index as f64) as f32;
            let a = if index == 0 {
                self.previous
            } else {
                samples[index - 1]
            };
            let b = samples.get(index).copied().unwrap_or(self.previous);
            output.push(a + (b - a) * fraction);
            phase += step;
        }
        self.phase = (phase - samples.len() as f64).max(0.0);
        self.previous = *samples.last().unwrap_or(&self.previous);
        output
    }
}

fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_stereo_to_16khz_i16_pcm() {
        let mut converter = PcmConverter::new(2, TARGET_RATE);
        let pcm = converter.convert(&[-1.0, 1.0, 0.5, 0.5]);
        let values: Vec<i16> = pcm
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        assert_eq!(values, vec![0, 16_384]);
    }
}

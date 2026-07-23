//! Default-microphone capture for the protocol spike.
//!
//! Audio stays in callback buffers and a bounded in-memory channel only. The
//! code never creates a WAV file and never logs samples or transcript text.

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use tokio::sync::{mpsc, watch};

use crate::spike::error::SpikeError;

const TARGET_RATE: u32 = 16_000;
const LOW_PASS_TAPS: usize = 63;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderFailure {
    Device,
    QueueOverflow,
}

impl RecorderFailure {
    pub fn into_error(self) -> SpikeError {
        match self {
            Self::Device => SpikeError::MicrophoneFailed,
            Self::QueueOverflow => SpikeError::AudioBackpressure,
        }
    }
}

pub struct Recorder {
    stream: cpal::Stream,
    failure_rx: watch::Receiver<Option<RecorderFailure>>,
}

impl Recorder {
    pub fn start_default(tx: mpsc::Sender<Vec<u8>>) -> Result<Self, SpikeError> {
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
        let (failure_tx, failure_rx) = watch::channel(None);

        let stream = match supported.sample_format() {
            SampleFormat::F32 => {
                build_stream_f32(&device, &config, Arc::clone(&converter), tx, failure_tx)
            }
            SampleFormat::I16 => {
                build_stream_i16(&device, &config, Arc::clone(&converter), tx, failure_tx)
            }
            SampleFormat::U16 => build_stream_u16(&device, &config, converter, tx, failure_tx),
            _ => return Err(SpikeError::MicrophoneUnavailable),
        }
        .map_err(|_| SpikeError::MicrophoneUnavailable)?;

        stream.play().map_err(|_| SpikeError::MicrophoneFailed)?;
        Ok(Self { stream, failure_rx })
    }

    pub fn failure_receiver(&self) -> watch::Receiver<Option<RecorderFailure>> {
        self.failure_rx.clone()
    }

    pub fn stop(self) -> Result<(), SpikeError> {
        let _ = self.stream.pause();
        match *self.failure_rx.borrow() {
            Some(failure) => Err(failure.into_error()),
            None => Ok(()),
        }
    }
}

fn build_stream_f32(
    device: &cpal::Device,
    config: &StreamConfig,
    converter: Arc<Mutex<PcmConverter>>,
    tx: mpsc::Sender<Vec<u8>>,
    failure_tx: watch::Sender<Option<RecorderFailure>>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let error_tx = failure_tx.clone();
    device.build_input_stream(
        config,
        move |samples: &[f32], _| forward(samples, &converter, &tx, &failure_tx),
        move |_| report_failure(&error_tx, RecorderFailure::Device),
        None,
    )
}

fn build_stream_i16(
    device: &cpal::Device,
    config: &StreamConfig,
    converter: Arc<Mutex<PcmConverter>>,
    tx: mpsc::Sender<Vec<u8>>,
    failure_tx: watch::Sender<Option<RecorderFailure>>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let error_tx = failure_tx.clone();
    device.build_input_stream(
        config,
        move |samples: &[i16], _| {
            let normalized: Vec<f32> = samples
                .iter()
                .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
                .collect();
            forward(&normalized, &converter, &tx, &failure_tx);
        },
        move |_| report_failure(&error_tx, RecorderFailure::Device),
        None,
    )
}

fn build_stream_u16(
    device: &cpal::Device,
    config: &StreamConfig,
    converter: Arc<Mutex<PcmConverter>>,
    tx: mpsc::Sender<Vec<u8>>,
    failure_tx: watch::Sender<Option<RecorderFailure>>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let error_tx = failure_tx.clone();
    device.build_input_stream(
        config,
        move |samples: &[u16], _| {
            let normalized: Vec<f32> = samples
                .iter()
                .map(|sample| (f32::from(*sample) / f32::from(u16::MAX)) * 2.0 - 1.0)
                .collect();
            forward(&normalized, &converter, &tx, &failure_tx);
        },
        move |_| report_failure(&error_tx, RecorderFailure::Device),
        None,
    )
}

fn forward(
    samples: &[f32],
    converter: &Arc<Mutex<PcmConverter>>,
    tx: &mpsc::Sender<Vec<u8>>,
    failure_tx: &watch::Sender<Option<RecorderFailure>>,
) {
    let pcm = converter
        .lock()
        .expect("audio converter lock poisoned")
        .convert(samples);
    if pcm.is_empty() {
        return;
    }
    match tx.try_send(pcm) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            report_failure(failure_tx, RecorderFailure::QueueOverflow)
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            report_failure(failure_tx, RecorderFailure::Device)
        }
    }
}

fn report_failure(failure_tx: &watch::Sender<Option<RecorderFailure>>, failure: RecorderFailure) {
    let _ = failure_tx.send(Some(failure));
}

struct PcmConverter {
    channels: usize,
    input_rate: u32,
    phase: f64,
    previous: f32,
    low_pass: Option<LowPassFilter>,
}

impl PcmConverter {
    fn new(channels: usize, input_rate: u32) -> Self {
        Self {
            channels: channels.max(1),
            input_rate,
            phase: 0.0,
            previous: 0.0,
            low_pass: (input_rate > TARGET_RATE)
                .then(|| LowPassFilter::for_downsampling(input_rate, TARGET_RATE)),
        }
    }

    fn convert(&mut self, interleaved: &[f32]) -> Vec<u8> {
        let mono = downmix(interleaved, self.channels);
        let filtered = match &mut self.low_pass {
            Some(filter) => filter.process(&mono),
            None => mono,
        };
        let samples = self.resample(&filtered);
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

struct LowPassFilter {
    coefficients: Vec<f32>,
    history: VecDeque<f32>,
}

impl LowPassFilter {
    fn for_downsampling(input_rate: u32, output_rate: u32) -> Self {
        let cutoff = (0.45 * f64::from(output_rate) / f64::from(input_rate)).min(0.45);
        let midpoint = (LOW_PASS_TAPS / 2) as isize;
        let mut coefficients: Vec<f32> = (0..LOW_PASS_TAPS)
            .map(|index| {
                let offset = index as isize - midpoint;
                let sinc = if offset == 0 {
                    2.0 * cutoff
                } else {
                    (2.0 * PI * cutoff * offset as f64).sin() / (PI * offset as f64)
                };
                let window =
                    0.54 - 0.46 * (2.0 * PI * index as f64 / (LOW_PASS_TAPS - 1) as f64).cos();
                (sinc * window) as f32
            })
            .collect();
        let scale = coefficients.iter().sum::<f32>();
        for coefficient in &mut coefficients {
            *coefficient /= scale;
        }
        Self {
            coefficients,
            history: VecDeque::with_capacity(LOW_PASS_TAPS),
        }
    }

    fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(samples.len());
        for sample in samples {
            if self.history.len() == LOW_PASS_TAPS {
                self.history.pop_front();
            }
            self.history.push_back(*sample);
            let filtered = self
                .history
                .iter()
                .rev()
                .zip(&self.coefficients)
                .map(|(value, coefficient)| value * coefficient)
                .sum();
            output.push(filtered);
        }
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

    #[tokio::test]
    async fn full_audio_queue_reports_backpressure() {
        let converter = Arc::new(Mutex::new(PcmConverter::new(1, TARGET_RATE)));
        let (tx, mut rx) = mpsc::channel(1);
        let (failure_tx, failure_rx) = watch::channel(None);

        forward(&[0.5], &converter, &tx, &failure_tx);
        forward(&[0.25], &converter, &tx, &failure_tx);

        assert!(rx.recv().await.is_some());
        assert_eq!(*failure_rx.borrow(), Some(RecorderFailure::QueueOverflow));
    }

    #[test]
    fn downsampling_filters_out_of_band_signal() {
        let low_band = sine_wave(1_000.0);
        let high_band = sine_wave(10_000.0);

        let low_rms = output_rms(PcmConverter::new(1, 48_000).convert(&low_band));
        let high_rms = output_rms(PcmConverter::new(1, 48_000).convert(&high_band));

        assert!(low_rms > 0.4, "speech-band signal should remain audible");
        assert!(high_rms < 0.08, "out-of-band signal should be attenuated");
    }

    fn sine_wave(frequency_hz: f64) -> Vec<f32> {
        (0..48_000)
            .map(|index| (2.0 * PI * frequency_hz * index as f64 / 48_000.0).sin() as f32)
            .collect()
    }

    fn output_rms(pcm: Vec<u8>) -> f64 {
        let samples: Vec<f64> = pcm
            .chunks_exact(2)
            .skip(1_000)
            .map(|bytes| f64::from(i16::from_le_bytes([bytes[0], bytes[1]])) / f64::from(i16::MAX))
            .collect();
        (samples.iter().map(|sample| sample * sample).sum::<f64>() / samples.len() as f64).sqrt()
    }
}

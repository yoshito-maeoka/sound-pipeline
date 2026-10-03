//! Audio I/O: file decoding, format conversion, WAV writing, and the cpal
//! output stream fed from a lock-free ring buffer.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Result};

/// Engine thread pushes interleaved samples, the cpal callback pops them.
pub type SampleProducer = ringbuf::HeapProd<f32>;
pub type SampleConsumer = ringbuf::HeapCons<f32>;

/// Interleaved PCM held in memory.
#[derive(Clone, Debug, Default)]
pub struct AudioData {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: usize,
}

impl AudioData {
    pub fn frames(&self) -> usize {
        if self.channels == 0 { 0 } else { self.samples.len() / self.channels }
    }
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 { 0.0 } else { self.frames() as f64 / self.sample_rate as f64 }
    }
}

/// Decode any format symphonia supports (wav, mp3, flac, ogg, m4a...).
pub fn decode_file(path: &Path) -> Result<AudioData> {
    bail!("decode_file not implemented: {}", path.display())
}

/// Resample (linear interpolation is fine) and remap channels
/// (mono→N duplicates, N→mono averages, otherwise take/duplicate).
pub fn convert(data: &AudioData, sample_rate: u32, channels: usize) -> AudioData {
    let _ = (sample_rate, channels);
    data.clone()
}

/// Write 32-bit float WAV.
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32, channels: usize) -> Result<()> {
    let _ = (samples, sample_rate, channels);
    bail!("write_wav not implemented: {}", path.display())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    pub sample_rate: u32,
    pub channels: usize,
}

pub fn list_output_devices() -> Vec<OutputDeviceInfo> {
    Vec::new()
}

/// A running output stream. Dropping it stops playback.
pub struct OutputStream {
    pub config: OutputConfig,
    pub device_name: String,
    // real impl holds the cpal::Stream here (it is !Send; keep on main thread)
}

/// Open `device_name` (None = default device), preferring `preferred_rate`
/// if supported (else the device default). The callback pops interleaved
/// samples from `consumer` in `config.channels` layout, writes silence on
/// underrun, and when `flush` is set drains the ring buffer and clears it.
pub fn open_output(
    device_name: Option<&str>,
    preferred_rate: u32,
    consumer: SampleConsumer,
    flush: Arc<AtomicBool>,
) -> Result<OutputStream> {
    let _ = (preferred_rate, consumer, flush);
    bail!("open_output not implemented ({device_name:?})")
}

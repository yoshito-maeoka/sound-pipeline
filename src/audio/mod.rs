//! Audio I/O: file decoding, format conversion, WAV writing, and the cpal
//! output stream fed from a lock-free ring buffer.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use ringbuf::traits::Consumer;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

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
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .with_context(|| format!("unrecognized audio format: {}", path.display()))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("no audio track in {}", path.display()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("no audio codec parameters in {}", path.display()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .context("unsupported audio codec")?;

    let mut out = AudioData::default();
    let mut scratch: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(e) => {
                if out.samples.is_empty() {
                    return Err(anyhow!(e).context("failed to read audio packets"));
                }
                break; // truncated file: keep what we have
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                if out.channels == 0 {
                    out.channels = buf.spec().channels().count();
                    out.sample_rate = buf.spec().rate();
                }
                scratch.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut scratch);
                out.samples.extend_from_slice(&scratch);
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(_) => break,
        }
    }
    if out.channels == 0 || out.samples.is_empty() {
        bail!("no audio decoded from {}", path.display());
    }
    // Guard against a stream that changed layout mid-file.
    let whole = out.samples.len() / out.channels * out.channels;
    out.samples.truncate(whole);
    Ok(out)
}

/// Resample (linear interpolation is fine) and remap channels
/// (mono→N duplicates, N→mono averages, otherwise take/duplicate).
pub fn convert(data: &AudioData, sample_rate: u32, channels: usize) -> AudioData {
    if data.channels == 0 || data.sample_rate == 0 || sample_rate == 0 || channels == 0 {
        return data.clone();
    }
    let remapped = remap_channels(&data.samples, data.channels, channels);
    let samples = if data.sample_rate == sample_rate {
        remapped
    } else {
        resample_linear(&remapped, channels, data.sample_rate, sample_rate)
    };
    AudioData { samples, sample_rate, channels }
}

fn remap_channels(src: &[f32], from: usize, to: usize) -> Vec<f32> {
    if from == to {
        return src.to_vec();
    }
    let frames = src.len() / from;
    let mut out = Vec::with_capacity(frames * to);
    for frame in src.chunks_exact(from) {
        if to == 1 {
            out.push(frame.iter().sum::<f32>() / from as f32);
        } else if from == 1 {
            out.extend(std::iter::repeat_n(frame[0], to));
        } else {
            out.extend((0..to).map(|c| frame[c % from]));
        }
    }
    out
}

fn resample_linear(src: &[f32], ch: usize, from: u32, to: u32) -> Vec<f32> {
    let frames = src.len() / ch;
    if frames == 0 {
        return Vec::new();
    }
    let out_frames = ((frames as f64) * to as f64 / from as f64).round() as usize;
    let step = from as f64 / to as f64;
    let mut out = Vec::with_capacity(out_frames * ch);
    for i in 0..out_frames {
        let pos = i as f64 * step;
        let i0 = (pos.floor() as usize).min(frames - 1);
        let i1 = (i0 + 1).min(frames - 1);
        let t = (pos - i0 as f64) as f32;
        for c in 0..ch {
            let a = src[i0 * ch + c];
            let b = src[i1 * ch + c];
            out.push(a + (b - a) * t);
        }
    }
    out
}

/// Write 32-bit float WAV.
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32, channels: usize) -> Result<()> {
    if channels == 0 {
        bail!("cannot write wav with 0 channels");
    }
    let spec = hound::WavSpec {
        channels: channels as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)
        .with_context(|| format!("cannot create {}", path.display()))?;
    for &s in samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
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

fn device_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|d| d.name().to_string())
}

pub fn list_output_devices() -> Vec<OutputDeviceInfo> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| device_name(&d));
    let Ok(devices) = host.output_devices() else { return Vec::new() };
    devices
        .filter_map(|d| device_name(&d))
        .map(|name| OutputDeviceInfo { is_default: default.as_deref() == Some(name.as_str()), name })
        .collect()
}

/// A running output stream. Dropping it stops playback.
pub struct OutputStream {
    pub config: OutputConfig,
    pub device_name: String,
    // cpal::Stream is !Send on some platforms; keep this on the main thread.
    _stream: cpal::Stream,
}

/// Shared sink for asynchronous device errors.
pub type ErrorSink = Arc<Mutex<Vec<String>>>;

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
    open_output_with_errors(device_name, preferred_rate, consumer, flush, Arc::new(Mutex::new(Vec::new())))
}

/// Like [`open_output`], but stream errors (e.g. device unplugged) are
/// appended to `errors`.
pub fn open_output_with_errors(
    device_name: Option<&str>,
    preferred_rate: u32,
    consumer: SampleConsumer,
    flush: Arc<AtomicBool>,
    errors: ErrorSink,
) -> Result<OutputStream> {
    let host = cpal::default_host();
    let device = match device_name {
        None => host.default_output_device().ok_or_else(|| anyhow!("no default output device"))?,
        Some(name) => host
            .output_devices()
            .context("cannot enumerate output devices")?
            .find(|d| self::device_name(d).as_deref() == Some(name))
            .ok_or_else(|| anyhow!("output device not found: {name}"))?,
    };
    let actual_name = self::device_name(&device).unwrap_or_else(|| "unknown".into());
    let supported = pick_config(&device, preferred_rate)?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let out_cfg = OutputConfig { sample_rate: config.sample_rate, channels: config.channels as usize };

    let err_fn = move |e: cpal::StreamError| {
        let msg = format!("audio output error: {e}");
        if let Ok(mut v) = errors.lock()
            && v.last() != Some(&msg)
        {
            v.push(msg);
        }
    };

    let stream = match format {
        SampleFormat::F32 => {
            let mut consumer = consumer;
            device.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    if flush.swap(false, Ordering::AcqRel) {
                        consumer.clear();
                        data.fill(0.0);
                        return;
                    }
                    let n = consumer.pop_slice(data);
                    data[n..].fill(0.0);
                },
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => build_converting::<i16>(&device, &config, consumer, flush, err_fn),
        SampleFormat::U16 => build_converting::<u16>(&device, &config, consumer, flush, err_fn),
        SampleFormat::I32 => build_converting::<i32>(&device, &config, consumer, flush, err_fn),
        SampleFormat::U32 => build_converting::<u32>(&device, &config, consumer, flush, err_fn),
        SampleFormat::I8 => build_converting::<i8>(&device, &config, consumer, flush, err_fn),
        SampleFormat::U8 => build_converting::<u8>(&device, &config, consumer, flush, err_fn),
        SampleFormat::F64 => build_converting::<f64>(&device, &config, consumer, flush, err_fn),
        other => bail!("unsupported output sample format {other:?}"),
    }
    .with_context(|| format!("cannot open output device {actual_name}"))?;
    stream.play().context("cannot start output stream")?;
    Ok(OutputStream { config: out_cfg, device_name: actual_name, _stream: stream })
}

fn build_converting<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut consumer: SampleConsumer,
    flush: Arc<AtomicBool>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            if flush.swap(false, Ordering::AcqRel) {
                consumer.clear();
                data.fill(T::from_sample(0.0f32));
                return;
            }
            for s in data.iter_mut() {
                *s = T::from_sample(consumer.try_pop().unwrap_or(0.0));
            }
        },
        err_fn,
        None,
    )
}

fn pick_config(device: &cpal::Device, preferred_rate: u32) -> Result<cpal::SupportedStreamConfig> {
    let default = device.default_output_config().context("no default output config")?;
    if default.sample_rate() == preferred_rate {
        return Ok(default);
    }
    if let Ok(ranges) = device.supported_output_configs() {
        let mut cands: Vec<_> = ranges
            .filter(|r| r.min_sample_rate() <= preferred_rate && preferred_rate <= r.max_sample_rate())
            .collect();
        cands.sort_by_key(|r| (r.channels() != default.channels(), r.sample_format() != SampleFormat::F32));
        if let Some(r) = cands.into_iter().next() {
            return Ok(r.with_sample_rate(preferred_rate));
        }
    }
    Ok(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("soundpipe-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn convert_mono_to_stereo_and_back() {
        let d = AudioData { samples: vec![0.1, 0.2, 0.3], sample_rate: 100, channels: 1 };
        let s = convert(&d, 100, 2);
        assert_eq!(s.channels, 2);
        assert_eq!(s.samples, vec![0.1, 0.1, 0.2, 0.2, 0.3, 0.3]);
        let m = convert(&s, 100, 1);
        assert_eq!(m.samples, d.samples);
    }

    #[test]
    fn convert_stereo_to_mono_averages() {
        let d = AudioData { samples: vec![1.0, 0.0, 0.5, 0.5], sample_rate: 100, channels: 2 };
        assert_eq!(convert(&d, 100, 1).samples, vec![0.5, 0.5]);
    }

    #[test]
    fn convert_multichannel() {
        let d = AudioData { samples: vec![1., 2., 3., 4., 5., 6.], sample_rate: 8, channels: 6 };
        assert_eq!(convert(&d, 8, 2).samples, vec![1., 2.]);
        let s = AudioData { samples: vec![1., 2.], sample_rate: 8, channels: 2 };
        assert_eq!(convert(&s, 8, 4).samples, vec![1., 2., 1., 2.]);
    }

    #[test]
    fn convert_resample_length_and_ramp() {
        let samples: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let d = AudioData { samples, sample_rate: 100, channels: 1 };
        let up = convert(&d, 200, 1);
        assert_eq!(up.frames(), 200);
        assert!((up.samples[1] - 0.5).abs() < 1e-5);
        assert!((up.samples[10] - 5.0).abs() < 1e-5);
        let down = convert(&d, 50, 1);
        assert_eq!(down.frames(), 50);
        assert!((down.samples[10] - 20.0).abs() < 1e-4);
        assert_eq!(down.sample_rate, 50);
    }

    #[test]
    fn convert_empty_is_safe() {
        let d = AudioData { samples: vec![], sample_rate: 44100, channels: 2 };
        assert_eq!(convert(&d, 48000, 2).frames(), 0);
    }

    #[test]
    fn wav_roundtrip() {
        let path = tmp("rt.wav");
        let samples: Vec<f32> = (0..2000).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect();
        write_wav(&path, &samples, 22050, 2).unwrap();
        let d = decode_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(d.sample_rate, 22050);
        assert_eq!(d.channels, 2);
        assert_eq!(d.samples.len(), samples.len());
        for (a, b) in d.samples.iter().zip(&samples) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn decode_int16_wav_mono() {
        let path = tmp("i16.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..800 {
            w.write_sample(((i as f32 * 0.1).sin() * 16000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
        let d = decode_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((d.sample_rate, d.channels, d.frames()), (8000, 1, 800));
        assert!(d.samples.iter().all(|s| s.abs() <= 1.0));
        assert!(d.samples.iter().any(|s| s.abs() > 0.3));
    }

    #[test]
    fn decode_missing_file_errors() {
        assert!(decode_file(Path::new("/nonexistent/nope.wav")).is_err());
    }

    #[test]
    fn decode_garbage_errors() {
        let path = tmp("garbage.wav");
        std::fs::write(&path, b"this is not audio at all, definitely not").unwrap();
        let r = decode_file(&path);
        let _ = std::fs::remove_file(&path);
        assert!(r.is_err());
    }

    #[test]
    fn list_devices_does_not_panic() {
        let _ = list_output_devices();
    }
}

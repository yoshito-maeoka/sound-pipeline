//! Playback engine: owns the source audio and the filter chain, runs DSP on
//! its own thread and feeds the output ring buffer.
//!
//! **Skeleton:** this stub keeps the chain mirror and a fake transport so
//! the TUI can be developed against it. The real implementation must keep
//! every public signature below.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;

use crate::audio::{AudioData, OutputConfig};
use crate::plugin::FilterDescriptor;

/// One filter slot as seen by the UI.
#[derive(Clone)]
pub struct ChainNode {
    pub desc: Arc<FilterDescriptor>,
    pub params: Vec<f32>,
    pub bypass: bool,
}

/// Levels for the UI, refreshed by the engine thread.
/// Peaks are linear (0.0..), decayed so they read nicely at ~30 fps.
#[derive(Clone, Debug, Default)]
pub struct Meters {
    pub source_peak: f32,
    /// Peak after each chain node (same length as the chain).
    pub node_peaks: Vec<f32>,
    /// `Filter::meter()` of each node.
    pub node_meters: Vec<Option<f32>>,
    pub output_peak: f32,
}

/// Progress of a background WAV render.
pub struct RenderJob {
    pub path: PathBuf,
    /// 0..=1000 (permille).
    pub progress: Arc<AtomicU32>,
    pub done: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<String>>>,
}

pub struct EngineHandle {
    source: Arc<AudioData>,
    chain: Vec<ChainNode>,
    playing: bool,
    looping: bool,
    base_pos: f64,
    started: Option<Instant>,
    device: Option<String>,
}

impl EngineHandle {
    /// Open the output device (None = default) and start the engine thread, paused.
    pub fn start(source: AudioData, device: Option<String>) -> Result<Self> {
        Ok(Self {
            source: Arc::new(source),
            chain: Vec::new(),
            playing: false,
            looping: true,
            base_pos: 0.0,
            started: None,
            device,
        })
    }

    pub fn chain(&self) -> &[ChainNode] {
        &self.chain
    }

    /// Insert a filter with default params at `index` (0 = right after the source).
    pub fn insert(&mut self, index: usize, desc: Arc<FilterDescriptor>) -> Result<()> {
        let params = desc.defaults();
        let index = index.min(self.chain.len());
        self.chain.insert(index, ChainNode { desc, params, bypass: false });
        Ok(())
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.chain.len() {
            self.chain.remove(index);
        }
    }

    pub fn move_node(&mut self, from: usize, to: usize) {
        if from < self.chain.len() && to < self.chain.len() {
            let n = self.chain.remove(from);
            self.chain.insert(to, n);
        }
    }

    /// Value is sanitized via the ParamSpec before use.
    pub fn set_param(&mut self, index: usize, param: usize, value: f32) {
        if let Some(n) = self.chain.get_mut(index) {
            if let Some(spec) = n.desc.params.get(param) {
                n.params[param] = spec.sanitize(value);
            }
        }
    }

    pub fn set_bypass(&mut self, index: usize, bypass: bool) {
        if let Some(n) = self.chain.get_mut(index) {
            n.bypass = bypass;
        }
    }

    pub fn play(&mut self) {
        if !self.playing {
            self.playing = true;
            self.started = Some(Instant::now());
        }
    }

    pub fn pause(&mut self) {
        self.base_pos = self.position_secs();
        self.playing = false;
        self.started = None;
    }

    pub fn toggle_play(&mut self) {
        if self.playing { self.pause() } else { self.play() }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    pub fn looping(&self) -> bool {
        self.looping
    }

    pub fn seek(&mut self, secs: f64) {
        self.base_pos = secs.clamp(0.0, self.duration_secs());
        if self.playing {
            self.started = Some(Instant::now());
        }
    }

    pub fn position_secs(&self) -> f64 {
        let d = self.duration_secs().max(1e-9);
        let p = self.base_pos + self.started.map_or(0.0, |s| s.elapsed().as_secs_f64());
        if self.looping { p % d } else { p.min(d) }
    }

    pub fn duration_secs(&self) -> f64 {
        self.source.duration_secs()
    }

    pub fn meters(&self) -> Meters {
        Meters {
            node_peaks: vec![0.0; self.chain.len()],
            node_meters: vec![None; self.chain.len()],
            ..Default::default()
        }
    }

    /// Switch output device; may change the sample rate (engine rebuilds
    /// filters at the new rate, keeping params).
    pub fn set_output_device(&mut self, name: Option<String>) -> Result<()> {
        self.device = name;
        Ok(())
    }

    pub fn output_device_name(&self) -> String {
        self.device.clone().unwrap_or_else(|| "Default output".into())
    }

    pub fn output_config(&self) -> OutputConfig {
        OutputConfig { sample_rate: self.source.sample_rate, channels: self.source.channels }
    }

    /// Render the whole source through a fresh copy of the current chain
    /// (offline, `realtime: false`) into a WAV file on a background thread.
    pub fn render_to_wav(&self, path: PathBuf) -> RenderJob {
        let job = RenderJob {
            path,
            progress: Arc::new(AtomicU32::new(0)),
            done: Arc::new(AtomicBool::new(false)),
            error: Arc::new(Mutex::new(Some("render not implemented".into()))),
        };
        job.done.store(true, Ordering::SeqCst);
        job
    }

    /// Most recent non-fatal engine error (plugin crash, device error), cleared on read.
    pub fn take_error(&mut self) -> Option<String> {
        None
    }
}

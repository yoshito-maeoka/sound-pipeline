//! Playback engine: owns the source audio and the filter chain, runs DSP on
//! its own thread and feeds the output ring buffer.
//!
//! The UI thread owns an [`EngineHandle`] holding a mirror of the chain
//! (for display) and the cpal stream. Filters are instantiated on the UI
//! thread and sent to the DSP thread over an mpsc channel; the DSP thread
//! renders small blocks of the (device-rate) source through the chain into
//! a ~4096 frame ring buffer that the cpal callback drains.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::HeapRb;

use crate::audio::{
    self, AudioData, ErrorSink, OutputConfig, OutputStream, SampleProducer,
};
use crate::plugin::{Filter, FilterContext, FilterDescriptor, Passthrough};

/// Target latency: the engine keeps at most this many frames in the ring.
const RING_FRAMES: usize = 4096;
/// Ring capacity is sized for up to this many channels.
const RING_MAX_CHANNELS: usize = 8;
/// Frames processed per DSP block.
const BLOCK_FRAMES: usize = 256;
/// Peak meters fall by half every this many seconds.
const METER_HALF_LIFE: f64 = 0.25;

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

// ---------------------------------------------------------------------------
// DSP thread
// ---------------------------------------------------------------------------

/// A live filter instance on the DSP thread.
struct Node {
    filter: Box<dyn Filter>,
    bypass: bool,
    name: String,
    /// Set when the filter panicked; it is skipped from then on.
    failed: bool,
}

enum Cmd {
    Insert { index: usize, node: Node },
    Remove(usize),
    Move { from: usize, to: usize },
    SetParam { index: usize, param: usize, value: f32 },
    SetBypass { index: usize, bypass: bool },
    Play,
    Pause,
    Seek(u64),
    SetLooping(bool),
    /// New output device: new ring, re-converted source, re-instantiated nodes.
    NewOutput {
        producer: SampleProducer,
        flush: Arc<AtomicBool>,
        source: Arc<AudioData>,
        nodes: Vec<Node>,
        pos: u64,
    },
    Shutdown,
}

/// State shared between the handle and the DSP thread.
struct Shared {
    /// Audible position in output-rate frames.
    position: AtomicU64,
    playing: AtomicBool,
    meters: Mutex<Meters>,
    errors: ErrorSink,
}

struct Core {
    shared: Arc<Shared>,
    producer: SampleProducer,
    flush: Arc<AtomicBool>,
    source: Arc<AudioData>,
    nodes: Vec<Node>,
    pos: usize,
    playing: bool,
    looping: bool,
    /// After a flush request, wait for the output callback to consume it
    /// before pushing fresh samples (otherwise they would be wiped too).
    await_flush: Option<Instant>,
    block: Vec<f32>,
    // meter state
    hold_src: f32,
    hold_nodes: Vec<f32>,
    hold_out: f32,
    node_meters: Vec<Option<f32>>,
    last_meter: Instant,
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

impl Core {
    fn new(
        shared: Arc<Shared>,
        producer: SampleProducer,
        flush: Arc<AtomicBool>,
        source: Arc<AudioData>,
        nodes: Vec<Node>,
        looping: bool,
    ) -> Self {
        let n = nodes.len();
        Self {
            shared,
            producer,
            flush,
            source,
            nodes,
            pos: 0,
            playing: false,
            looping,
            await_flush: None,
            block: Vec::with_capacity(BLOCK_FRAMES * RING_MAX_CHANNELS),
            hold_src: 0.0,
            hold_nodes: vec![0.0; n],
            hold_out: 0.0,
            node_meters: vec![None; n],
            last_meter: Instant::now(),
        }
    }

    fn ch(&self) -> usize {
        self.source.channels.max(1)
    }

    fn len_frames(&self) -> usize {
        self.source.frames()
    }

    fn report(&self, msg: String) {
        if let Ok(mut v) = self.shared.errors.lock()
            && v.last() != Some(&msg)
        {
            v.push(msg);
        }
    }

    fn set_playing(&mut self, p: bool) {
        self.playing = p;
        self.shared.playing.store(p, Ordering::Release);
    }

    /// Frame currently being heard (read position minus queued audio).
    fn audible_pos(&self) -> usize {
        let queued = self.producer.occupied_len() / self.ch();
        let len = self.len_frames();
        if queued <= self.pos {
            self.pos - queued
        } else if self.looping && len > 0 {
            (self.pos + len - queued % len) % len
        } else {
            0
        }
    }

    fn publish_position(&self) {
        let p = self.audible_pos().min(self.len_frames());
        self.shared.position.store(p as u64, Ordering::Release);
    }

    fn seek(&mut self, frame: usize) {
        self.pos = frame.min(self.len_frames());
        for n in &mut self.nodes {
            n.filter.reset();
        }
        self.flush.store(true, Ordering::Release);
        self.await_flush = Some(Instant::now());
        self.shared.position.store(self.pos as u64, Ordering::Release);
    }

    fn apply(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Insert { index, node } => {
                let i = index.min(self.nodes.len());
                self.nodes.insert(i, node);
                self.hold_nodes.insert(i, 0.0);
                self.node_meters.insert(i, None);
            }
            Cmd::Remove(i) => {
                if i < self.nodes.len() {
                    self.nodes.remove(i);
                    self.hold_nodes.remove(i);
                    self.node_meters.remove(i);
                }
            }
            Cmd::Move { from, to } => {
                let n = self.nodes.len();
                if from < n && to < n {
                    let node = self.nodes.remove(from);
                    self.nodes.insert(to, node);
                    let p = self.hold_nodes.remove(from);
                    self.hold_nodes.insert(to, p);
                    let m = self.node_meters.remove(from);
                    self.node_meters.insert(to, m);
                }
            }
            Cmd::SetParam { index, param, value } => {
                if let Some(n) = self.nodes.get_mut(index) {
                    n.filter.set_param(param, value);
                }
            }
            Cmd::SetBypass { index, bypass } => {
                if let Some(n) = self.nodes.get_mut(index) {
                    n.bypass = bypass;
                }
            }
            Cmd::Play => {
                if self.pos >= self.len_frames() && !self.looping {
                    self.seek(0);
                }
                self.set_playing(true);
            }
            Cmd::Pause => {
                if self.playing {
                    // Drop queued audio and rewind to what was actually heard.
                    let at = self.audible_pos();
                    self.set_playing(false);
                    self.seek(at);
                }
            }
            Cmd::Seek(f) => self.seek(f as usize),
            Cmd::SetLooping(l) => self.looping = l,
            Cmd::NewOutput { producer, flush, source, nodes, pos } => {
                self.producer = producer;
                self.flush = flush;
                self.source = source;
                let n = nodes.len();
                self.nodes = nodes;
                self.hold_nodes = vec![0.0; n];
                self.node_meters = vec![None; n];
                self.pos = (pos as usize).min(self.len_frames());
                self.await_flush = None;
                self.shared.position.store(self.pos as u64, Ordering::Release);
            }
            Cmd::Shutdown => return false,
        }
        true
    }

    fn decay_meters(&mut self) {
        let dt = self.last_meter.elapsed().as_secs_f64();
        self.last_meter = Instant::now();
        let f = 0.5f64.powf(dt / METER_HALF_LIFE) as f32;
        self.hold_src *= f;
        self.hold_out *= f;
        for p in &mut self.hold_nodes {
            *p *= f;
        }
    }

    fn publish_meters(&self) {
        if let Ok(mut m) = self.shared.meters.lock() {
            m.source_peak = self.hold_src;
            m.output_peak = self.hold_out;
            m.node_peaks.clone_from(&self.hold_nodes);
            m.node_meters.clone_from(&self.node_meters);
        }
    }

    /// Copy the next block from the source into `self.block`, wrapping when looping.
    fn read_block(&mut self) {
        let ch = self.ch();
        let want = BLOCK_FRAMES * ch;
        let len = self.len_frames();
        self.block.clear();
        while self.block.len() < want {
            if self.pos >= len {
                if self.looping && len > 0 {
                    self.pos = 0;
                } else {
                    break;
                }
            }
            let n = ((want - self.block.len()) / ch).min(len - self.pos);
            self.block.extend_from_slice(&self.source.samples[self.pos * ch..(self.pos + n) * ch]);
            self.pos += n;
        }
    }

    /// Render one block into the ring. Returns false if nothing was produced.
    fn produce(&mut self) -> bool {
        self.read_block();
        if self.block.is_empty() {
            self.set_playing(false);
            return false;
        }
        let mut block = std::mem::take(&mut self.block);
        let src_peak = peak(&block);
        let mut peaks = std::mem::take(&mut self.hold_nodes);
        let mut last = src_peak;
        let mut new_peaks = Vec::with_capacity(self.nodes.len());
        let mut crashed: Vec<String> = Vec::new();
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if !node.bypass && !node.failed {
                let r = catch_unwind(AssertUnwindSafe(|| node.filter.process(&mut block)));
                if r.is_err() {
                    node.failed = true;
                    crashed.push(format!("filter '{}' crashed and was bypassed", node.name));
                } else {
                    last = peak(&block);
                    if let Some(m) = self.node_meters.get_mut(i) {
                        *m = node.filter.meter();
                    }
                }
            }
            new_peaks.push(last);
        }
        for msg in crashed {
            self.report(msg);
        }
        for s in block.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        let out_peak = peak(&block);
        self.producer.push_slice(&block);

        self.decay_meters();
        self.hold_src = self.hold_src.max(src_peak);
        self.hold_out = self.hold_out.max(out_peak);
        for (h, n) in peaks.iter_mut().zip(new_peaks) {
            *h = h.max(n);
        }
        self.hold_nodes = peaks;
        self.publish_meters();
        self.block = block;

        if self.pos >= self.len_frames() && !self.looping {
            self.set_playing(false);
        }
        true
    }

    /// One scheduling step. Returns Some(did_work).
    fn step(&mut self) -> bool {
        if let Some(t) = self.await_flush {
            if self.flush.load(Ordering::Acquire) && t.elapsed() < Duration::from_millis(100) {
                return false;
            }
            self.await_flush = None;
        }
        let worked = if self.playing {
            let room = RING_FRAMES * self.ch();
            let queued = self.producer.occupied_len();
            if queued + BLOCK_FRAMES * self.ch() <= room.min(self.producer.capacity().get()) {
                self.produce()
            } else {
                false
            }
        } else {
            false
        };
        if !worked && self.last_meter.elapsed() >= Duration::from_millis(10) {
            self.decay_meters();
            self.publish_meters();
        }
        self.publish_position();
        worked
    }
}

fn run_core(mut core: Core, rx: Receiver<Cmd>) {
    loop {
        // Drain pending commands.
        loop {
            match rx.try_recv() {
                Ok(c) => {
                    if !core.apply(c) {
                        return;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        if core.step() {
            continue;
        }
        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(c) => {
                if !core.apply(c) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

// ---------------------------------------------------------------------------
// UI-side handle
// ---------------------------------------------------------------------------

pub struct EngineHandle {
    /// Source at its native rate/channels (used for offline render).
    original: Arc<AudioData>,
    /// Frames in the device-rate source.
    frames: u64,
    chain: Vec<ChainNode>,
    looping: bool,
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    stream: OutputStream,
}

fn new_ring(flush_on: bool) -> (SampleProducer, ringbuf::HeapCons<f32>, Arc<AtomicBool>) {
    let (p, c) = HeapRb::<f32>::new(RING_FRAMES * RING_MAX_CHANNELS).split();
    (p, c, Arc::new(AtomicBool::new(flush_on)))
}

fn make_node(desc: &FilterDescriptor, ctx: &FilterContext, params: &[f32], bypass: bool) -> Result<Node> {
    let filter = desc.instantiate(ctx, params)?;
    Ok(Node { filter, bypass, name: desc.name.clone(), failed: false })
}

impl EngineHandle {
    /// Open the output device (None = default) and start the engine thread, paused.
    pub fn start(source: AudioData, device: Option<String>) -> Result<Self> {
        if source.channels == 0 || source.sample_rate == 0 {
            return Err(anyhow!("source has no audio"));
        }
        let errors: ErrorSink = Arc::new(Mutex::new(Vec::new()));
        let (producer, consumer, flush) = new_ring(false);
        let stream = audio::open_output_with_errors(
            device.as_deref(),
            source.sample_rate,
            consumer,
            flush.clone(),
            errors.clone(),
        )?;
        let cfg = stream.config;
        let converted = Arc::new(audio::convert(&source, cfg.sample_rate, cfg.channels));
        let shared = Arc::new(Shared {
            position: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            meters: Mutex::new(Meters::default()),
            errors,
        });
        let frames = converted.frames() as u64;
        let core = Core::new(shared.clone(), producer, flush, converted, Vec::new(), true);
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("sp-engine".into())
            .spawn(move || run_core(core, rx))?;
        Ok(Self {
            original: Arc::new(source),
            frames,
            chain: Vec::new(),
            looping: true,
            tx,
            thread: Some(thread),
            shared,
            stream,
        })
    }

    fn ctx(&self) -> FilterContext {
        let c = self.stream.config;
        FilterContext { sample_rate: c.sample_rate as f32, channels: c.channels, realtime: true }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn chain(&self) -> &[ChainNode] {
        &self.chain
    }

    /// Insert a filter with default params at `index` (0 = right after the source).
    pub fn insert(&mut self, index: usize, desc: Arc<FilterDescriptor>) -> Result<()> {
        let params = desc.defaults();
        let index = index.min(self.chain.len());
        let node = make_node(&desc, &self.ctx(), &params, false)?;
        self.send(Cmd::Insert { index, node });
        self.chain.insert(index, ChainNode { desc, params, bypass: false });
        Ok(())
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.chain.len() {
            self.chain.remove(index);
            self.send(Cmd::Remove(index));
        }
    }

    pub fn move_node(&mut self, from: usize, to: usize) {
        if from < self.chain.len() && to < self.chain.len() {
            let n = self.chain.remove(from);
            self.chain.insert(to, n);
            self.send(Cmd::Move { from, to });
        }
    }

    /// Value is sanitized via the ParamSpec before use.
    pub fn set_param(&mut self, index: usize, param: usize, value: f32) {
        if let Some(n) = self.chain.get_mut(index)
            && let Some(spec) = n.desc.params.get(param)
        {
            let v = spec.sanitize(value);
            n.params[param] = v;
            self.send(Cmd::SetParam { index, param, value: v });
        }
    }

    pub fn set_bypass(&mut self, index: usize, bypass: bool) {
        if let Some(n) = self.chain.get_mut(index) {
            n.bypass = bypass;
            self.send(Cmd::SetBypass { index, bypass });
        }
    }

    pub fn play(&mut self) {
        self.shared.playing.store(true, Ordering::Release);
        self.send(Cmd::Play);
    }

    pub fn pause(&mut self) {
        self.shared.playing.store(false, Ordering::Release);
        self.send(Cmd::Pause);
    }

    pub fn toggle_play(&mut self) {
        if self.is_playing() { self.pause() } else { self.play() }
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
        self.send(Cmd::SetLooping(looping));
    }

    pub fn looping(&self) -> bool {
        self.looping
    }

    pub fn seek(&mut self, secs: f64) {
        let rate = self.stream.config.sample_rate as f64;
        let frame = ((secs.max(0.0) * rate) as u64).min(self.frames);
        self.shared.position.store(frame, Ordering::Release);
        self.send(Cmd::Seek(frame));
    }

    pub fn position_secs(&self) -> f64 {
        let rate = self.stream.config.sample_rate as f64;
        let p = self.shared.position.load(Ordering::Acquire) as f64 / rate;
        p.min(self.duration_secs())
    }

    pub fn duration_secs(&self) -> f64 {
        self.original.duration_secs()
    }

    pub fn meters(&self) -> Meters {
        let mut m = self.shared.meters.lock().map(|m| m.clone()).unwrap_or_default();
        m.node_peaks.resize(self.chain.len(), 0.0);
        m.node_meters.resize(self.chain.len(), None);
        m
    }

    /// Switch output device; may change the sample rate (engine rebuilds
    /// filters at the new rate, keeping params).
    pub fn set_output_device(&mut self, name: Option<String>) -> Result<()> {
        let (producer, consumer, flush) = new_ring(false);
        let new_stream = audio::open_output_with_errors(
            name.as_deref(),
            self.original.sample_rate,
            consumer,
            flush.clone(),
            self.shared.errors.clone(),
        )?;
        let cfg = new_stream.config;
        let old = self.stream.config;
        let source = Arc::new(audio::convert(&self.original, cfg.sample_rate, cfg.channels));
        let frames = source.frames() as u64;
        // Keep the play position (in seconds) across a rate change.
        let secs = self.shared.position.load(Ordering::Acquire) as f64 / old.sample_rate as f64;
        let pos = ((secs * cfg.sample_rate as f64) as u64).min(frames);
        let ctx = FilterContext {
            sample_rate: cfg.sample_rate as f32,
            channels: cfg.channels,
            realtime: true,
        };
        let mut nodes = Vec::with_capacity(self.chain.len());
        let mut problems = Vec::new();
        for cn in &self.chain {
            match make_node(&cn.desc, &ctx, &cn.params, cn.bypass) {
                Ok(n) => nodes.push(n),
                Err(e) => {
                    problems.push(format!("'{}' failed to restart on new device: {e:#}", cn.desc.name));
                    nodes.push(Node {
                        filter: Box::new(Passthrough),
                        bypass: true,
                        name: cn.desc.name.clone(),
                        failed: true,
                    });
                }
            }
        }
        self.send(Cmd::NewOutput { producer, flush, source, nodes, pos });
        self.frames = frames;
        self.shared.position.store(pos, Ordering::Release);
        self.stream = new_stream; // drops (stops) the old stream
        if let Ok(mut v) = self.shared.errors.lock() {
            v.extend(problems);
        }
        Ok(())
    }

    pub fn output_device_name(&self) -> String {
        self.stream.device_name.clone()
    }

    pub fn output_config(&self) -> OutputConfig {
        self.stream.config
    }

    /// Render the whole source through a fresh copy of the current chain
    /// (offline, `realtime: false`) into a WAV file on a background thread.
    pub fn render_to_wav(&self, path: PathBuf) -> RenderJob {
        let job = RenderJob {
            path: path.clone(),
            progress: Arc::new(AtomicU32::new(0)),
            done: Arc::new(AtomicBool::new(false)),
            error: Arc::new(Mutex::new(None)),
        };
        let source = self.original.clone();
        let nodes: Vec<(Arc<FilterDescriptor>, Vec<f32>)> = self
            .chain
            .iter()
            .filter(|n| !n.bypass)
            .map(|n| (n.desc.clone(), n.params.clone()))
            .collect();
        let (progress, done, error) = (job.progress.clone(), job.done.clone(), job.error.clone());
        let spawned = std::thread::Builder::new().name("sp-render".into()).spawn({
            let (progress, done, error) = (progress.clone(), done.clone(), error.clone());
            move || {
                let r = catch_unwind(AssertUnwindSafe(|| render_offline(&source, &nodes, &path, &progress)));
                let r = r.unwrap_or_else(|_| Err(anyhow!("a filter crashed during render")));
                if let Err(e) = r {
                    *error.lock().unwrap() = Some(format!("{e:#}"));
                }
                done.store(true, Ordering::SeqCst);
            }
        });
        if let Err(e) = spawned {
            *error.lock().unwrap() = Some(format!("cannot start render thread: {e}"));
            done.store(true, Ordering::SeqCst);
        }
        job
    }

    /// Most recent non-fatal engine error (plugin crash, device error), cleared on read.
    pub fn take_error(&mut self) -> Option<String> {
        let mut v = self.shared.errors.lock().ok()?;
        let last = v.last().cloned();
        v.clear();
        last
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Process `source` through fresh filter instances and write a WAV.
fn render_offline(
    source: &AudioData,
    nodes: &[(Arc<FilterDescriptor>, Vec<f32>)],
    path: &std::path::Path,
    progress: &AtomicU32,
) -> Result<()> {
    let ctx = FilterContext {
        sample_rate: source.sample_rate as f32,
        channels: source.channels,
        realtime: false,
    };
    let mut filters = Vec::with_capacity(nodes.len());
    for (desc, params) in nodes {
        filters.push(desc.instantiate(&ctx, params)?);
    }
    let mut out = source.samples.clone();
    let ch = source.channels.max(1);
    let total = out.len().max(1);
    let step = 4096 * ch;
    let mut done = 0usize;
    for chunk in out.chunks_mut(step) {
        for f in filters.iter_mut() {
            f.process(chunk);
        }
        for s in chunk.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        done += chunk.len();
        progress.store((done as u64 * 999 / total as u64) as u32, Ordering::Relaxed);
    }
    audio::write_wav(path, &out, source.sample_rate, source.channels)?;
    progress.store(1000, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Consumer;

    struct Gain(f32);
    impl Filter for Gain {
        fn set_param(&mut self, _i: usize, v: f32) {
            self.0 = v;
        }
        fn process(&mut self, buf: &mut [f32]) {
            buf.iter_mut().for_each(|s| *s *= self.0);
        }
    }

    struct Boom;
    impl Filter for Boom {
        fn set_param(&mut self, _i: usize, _v: f32) {}
        fn process(&mut self, _buf: &mut [f32]) {
            panic!("boom");
        }
    }

    fn node(f: Box<dyn Filter>) -> Node {
        Node { filter: f, bypass: false, name: "t".into(), failed: false }
    }

    fn core_with(samples: Vec<f32>, ch: usize, nodes: Vec<Node>) -> (Core, ringbuf::HeapCons<f32>) {
        let (p, c, flush) = new_ring(false);
        let shared = Arc::new(Shared {
            position: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            meters: Mutex::new(Meters::default()),
            errors: Arc::new(Mutex::new(Vec::new())),
        });
        let src = Arc::new(AudioData { samples, sample_rate: 48000, channels: ch });
        (Core::new(shared, p, flush, src, nodes, false), c)
    }

    fn drain(c: &mut ringbuf::HeapCons<f32>) -> Vec<f32> {
        let mut v = vec![0.0; c.occupied_len()];
        let n = c.pop_slice(&mut v);
        v.truncate(n);
        v
    }

    #[test]
    fn plays_through_chain_and_stops_at_end() {
        let samples: Vec<f32> = (0..1000).map(|_| 0.5).collect(); // 500 stereo frames
        let (mut core, mut cons) = core_with(samples, 2, vec![node(Box::new(Gain(0.5)))]);
        core.apply(Cmd::Play);
        while core.step() {}
        let out = drain(&mut cons);
        assert_eq!(out.len(), 1000);
        assert!(out.iter().all(|s| (*s - 0.25).abs() < 1e-6));
        assert!(!core.playing);
        assert!(!core.shared.playing.load(Ordering::Acquire));
        let m = core.shared.meters.lock().unwrap().clone();
        assert!((m.source_peak - 0.5).abs() < 1e-6);
        assert!((m.node_peaks[0] - 0.25).abs() < 1e-6);
        assert!((m.output_peak - 0.25).abs() < 1e-6);
    }

    #[test]
    fn ring_never_exceeds_target_latency() {
        let (mut core, mut cons) = core_with(vec![0.1; 2 * 48000], 2, vec![]);
        core.looping = true;
        core.apply(Cmd::Play);
        while core.step() {}
        assert!(cons.occupied_len() <= RING_FRAMES * 2);
        assert!(cons.occupied_len() >= (RING_FRAMES - BLOCK_FRAMES) * 2);
        drain(&mut cons);
    }

    #[test]
    fn looping_wraps() {
        let samples: Vec<f32> = (0..300).map(|i| i as f32).collect(); // 300 mono frames
        let (mut core, mut cons) = core_with(samples, 1, vec![]);
        core.apply(Cmd::SetLooping(true));
        core.apply(Cmd::Play);
        while core.step() {}
        let out = drain(&mut cons);
        assert!(out.len() >= 600);
        assert_eq!(out[300], 0.0);
        assert_eq!(out[301], 1.0);
        assert!(core.playing);
    }

    #[test]
    fn seek_sets_flush_and_position() {
        let (mut core, _cons) = core_with(vec![0.0; 2000], 2, vec![]);
        core.apply(Cmd::Seek(400));
        assert!(core.flush.load(Ordering::Acquire));
        assert_eq!(core.shared.position.load(Ordering::Acquire), 400);
        // Blocked on the flush until the callback consumes it.
        core.apply(Cmd::Play);
        assert!(!core.step());
        core.flush.store(false, Ordering::Release);
        assert!(core.step());
    }

    #[test]
    fn pause_rewinds_to_audible_position() {
        let (mut core, _cons) = core_with(vec![0.0; 2 * 48000], 2, vec![]);
        core.apply(Cmd::Play);
        while core.step() {}
        assert!(core.pos >= RING_FRAMES - BLOCK_FRAMES);
        core.apply(Cmd::Pause);
        assert_eq!(core.pos, 0); // nothing was consumed yet
        assert!(!core.playing);
    }

    #[test]
    fn param_bypass_and_panic_isolation() {
        let (mut core, mut cons) =
            core_with(vec![1.0; 512], 1, vec![node(Box::new(Boom)), node(Box::new(Gain(1.0)))]);
        core.apply(Cmd::SetParam { index: 1, param: 0, value: 0.5 });
        core.apply(Cmd::Play);
        while core.step() {}
        let out = drain(&mut cons);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6));
        assert!(core.nodes[0].failed);
        assert_eq!(core.shared.errors.lock().unwrap().len(), 1);

        core.apply(Cmd::SetBypass { index: 1, bypass: true });
        assert!(core.nodes[1].bypass);
    }

    #[test]
    fn insert_remove_move_keep_meter_vectors_aligned() {
        let (mut core, _cons) = core_with(vec![0.0; 100], 1, vec![]);
        core.apply(Cmd::Insert { index: 0, node: node(Box::new(Gain(1.0))) });
        core.apply(Cmd::Insert { index: 5, node: node(Box::new(Gain(2.0))) });
        core.apply(Cmd::Move { from: 0, to: 1 });
        assert_eq!(core.nodes.len(), 2);
        assert_eq!(core.hold_nodes.len(), 2);
        core.apply(Cmd::Remove(0));
        assert_eq!((core.nodes.len(), core.hold_nodes.len(), core.node_meters.len()), (1, 1, 1));
    }

    #[test]
    fn render_offline_applies_filters() {
        use crate::plugin::ParamSpec;
        let desc = Arc::new(FilterDescriptor::native(
            "g",
            "G",
            "Test",
            vec![ParamSpec::new("gain", "Gain", 0.0, 2.0, 1.0)],
            |_| Box::new(Gain(1.0)),
        ));
        let src = AudioData { samples: vec![0.8; 20000], sample_rate: 8000, channels: 2 };
        let path = std::env::temp_dir().join(format!("sp-render-{}.wav", std::process::id()));
        let progress = AtomicU32::new(0);
        render_offline(&src, &[(desc, vec![0.5])], &path, &progress).unwrap();
        let d = audio::decode_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(progress.load(Ordering::Relaxed), 1000);
        assert_eq!((d.sample_rate, d.channels, d.samples.len()), (8000, 2, 20000));
        assert!(d.samples.iter().all(|s| (*s - 0.4).abs() < 1e-6));
    }

    /// Needs a real output device; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_device_smoke() {
        let src = AudioData { samples: vec![0.0; 48000 * 2], sample_rate: 48000, channels: 2 };
        let mut e = EngineHandle::start(src, None).unwrap();
        e.play();
        std::thread::sleep(Duration::from_millis(300));
        assert!(e.position_secs() > 0.1);
        e.seek(0.5);
        e.pause();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!e.is_playing());
        assert!(e.take_error().is_none());
    }
}

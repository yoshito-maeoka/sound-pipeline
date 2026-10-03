//! The plugin framework: every filter — built-in, Rust plugin, or external
//! process plugin — is described by a [`FilterDescriptor`] and instantiated
//! as a `Box<dyn Filter>`.
//!
//! Audio contract (shared by all filters):
//! - samples are `f32`, nominal range -1.0..=1.0
//! - buffers are **interleaved** (`L R L R ...`), `buf.len()` is always a
//!   multiple of `FilterContext::channels`
//! - filters process in place and must never allocate or block in
//!   `process` when `FilterContext::realtime` is true

use std::sync::Arc;

use anyhow::Result;

/// Stream format + mode handed to a filter factory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterContext {
    pub sample_rate: f32,
    pub channels: usize,
    /// true inside the live TUI engine, false for offline render / CLI.
    pub realtime: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Linear,
    /// Logarithmic mapping for UI sliders (frequencies, times). `min` must be > 0.
    Log,
}

/// Describes one automatable parameter of a filter.
#[derive(Clone, Debug)]
pub struct ParamSpec {
    /// Short machine name, used on the CLI: `sp filter lowpass cutoff=800`.
    pub key: String,
    pub label: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Quantization step; 0.0 means continuous.
    pub step: f32,
    pub unit: String,
    pub scale: Scale,
    /// Optional display names for integer values `min..=max` (e.g. ["off", "on"]).
    pub choices: Vec<String>,
}

impl ParamSpec {
    pub fn new(key: &str, label: &str, min: f32, max: f32, default: f32) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            min,
            max,
            default,
            step: 0.0,
            unit: String::new(),
            scale: Scale::Linear,
            choices: Vec::new(),
        }
    }
    pub fn unit(mut self, unit: &str) -> Self {
        self.unit = unit.into();
        self
    }
    pub fn log(mut self) -> Self {
        self.scale = Scale::Log;
        self
    }
    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }
    /// Integer-valued switch with named choices; sets min/max/step accordingly.
    pub fn choices(mut self, names: &[&str]) -> Self {
        self.choices = names.iter().map(|s| s.to_string()).collect();
        self.min = 0.0;
        self.max = (names.len().max(1) - 1) as f32;
        self.step = 1.0;
        self
    }

    /// Clamp to range and apply `step` quantization.
    pub fn sanitize(&self, v: f32) -> f32 {
        let v = if v.is_finite() { v } else { self.default };
        let mut v = v.clamp(self.min, self.max);
        if self.step > 0.0 {
            v = self.min + ((v - self.min) / self.step).round() * self.step;
            v = v.clamp(self.min, self.max);
        }
        v
    }

    /// Value -> 0..=1 slider position.
    pub fn to_norm(&self, v: f32) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        let n = match self.scale {
            Scale::Log if self.min > 0.0 => (v.max(self.min) / self.min).ln() / (self.max / self.min).ln(),
            _ => (v - self.min) / (self.max - self.min),
        };
        n.clamp(0.0, 1.0)
    }

    /// 0..=1 slider position -> sanitized value.
    pub fn from_norm(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        let v = match self.scale {
            Scale::Log if self.min > 0.0 => self.min * (self.max / self.min).powf(n),
            _ => self.min + n * (self.max - self.min),
        };
        self.sanitize(v)
    }

    /// Human-readable value with unit, e.g. "2.50 kHz", "-12.0 dB", "on".
    pub fn format_value(&self, v: f32) -> String {
        if !self.choices.is_empty() {
            let i = (v - self.min).round().max(0.0) as usize;
            if let Some(c) = self.choices.get(i) {
                return c.clone();
            }
        }
        if self.unit == "Hz" && v.abs() >= 1000.0 {
            return format!("{:.2} kHz", v / 1000.0);
        }
        let num = if self.step >= 1.0 || v.abs() >= 100.0 {
            format!("{v:.0}")
        } else if v.abs() >= 10.0 {
            format!("{v:.1}")
        } else {
            format!("{v:.2}")
        };
        if self.unit.is_empty() {
            num
        } else {
            format!("{num} {}", self.unit)
        }
    }
}

/// A running filter instance. Lives on the engine thread.
pub trait Filter: Send {
    /// Set parameter `index` (index into `FilterDescriptor::params`).
    /// Values arrive already sanitized.
    fn set_param(&mut self, index: usize, value: f32);
    /// Process interleaved samples in place.
    fn process(&mut self, buf: &mut [f32]);
    /// Clear internal state (delay lines, envelopes) — called on seek.
    fn reset(&mut self) {}
    /// Optional live readout shown in the editor (e.g. compressor gain
    /// reduction in dB). Paired with `FilterDescriptor::meter_label`.
    fn meter(&self) -> Option<f32> {
        None
    }
}

/// Which editor UI the TUI shows for a filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorKind {
    /// One horizontal slider per parameter (default).
    Sliders,
    /// Graphic EQ: the first `EQ_BANDS.len()` params are band gains in dB,
    /// in the order of `EQ_BANDS`; any further params render as sliders.
    Equalizer,
}

/// Center frequencies of the graphic EQ bands (Hz).
pub const EQ_BANDS: [f32; 8] = [60.0, 150.0, 400.0, 1000.0, 2500.0, 6000.0, 12000.0, 16000.0];

pub type FilterFactory = Arc<dyn Fn(&FilterContext) -> Result<Box<dyn Filter>> + Send + Sync>;

/// Where a filter comes from.
#[derive(Clone)]
pub enum FilterKind {
    /// Compiled into the binary (built-in or a Rust plugin).
    Native,
    /// External process speaking raw f32 over stdin/stdout.
    Exec(Arc<crate::exec_plugin::ExecSpec>),
}

/// Static description + factory for one filter type.
#[derive(Clone)]
pub struct FilterDescriptor {
    /// Unique id, used on the CLI (`sp filter <id>`). lowercase, no spaces.
    pub id: String,
    pub name: String,
    /// Grouping in the "add filter" picker: Tone, Dynamics, Drive, Modulation, Time, Plugin...
    pub category: String,
    pub description: String,
    pub params: Vec<ParamSpec>,
    pub editor: EditorKind,
    pub meter_label: Option<String>,
    pub kind: FilterKind,
    pub factory: FilterFactory,
}

impl FilterDescriptor {
    /// Descriptor for a native (Rust) filter.
    pub fn native<F>(id: &str, name: &str, category: &str, params: Vec<ParamSpec>, factory: F) -> Self
    where
        F: Fn(&FilterContext) -> Box<dyn Filter> + Send + Sync + 'static,
    {
        Self {
            id: id.into(),
            name: name.into(),
            category: category.into(),
            description: String::new(),
            params,
            editor: EditorKind::Sliders,
            meter_label: None,
            kind: FilterKind::Native,
            factory: Arc::new(move |ctx| Ok(factory(ctx))),
        }
    }
    pub fn description(mut self, text: &str) -> Self {
        self.description = text.into();
        self
    }
    pub fn editor(mut self, editor: EditorKind) -> Self {
        self.editor = editor;
        self
    }
    pub fn meter(mut self, label: &str) -> Self {
        self.meter_label = Some(label.into());
        self
    }

    pub fn defaults(&self) -> Vec<f32> {
        self.params.iter().map(|p| p.default).collect()
    }

    /// Create an instance and apply `values` (one per param; missing → default).
    pub fn instantiate(&self, ctx: &FilterContext, values: &[f32]) -> Result<Box<dyn Filter>> {
        let mut f = (self.factory)(ctx)?;
        for (i, p) in self.params.iter().enumerate() {
            f.set_param(i, p.sanitize(values.get(i).copied().unwrap_or(p.default)));
        }
        Ok(f)
    }

    pub fn param_index(&self, key: &str) -> Option<usize> {
        self.params.iter().position(|p| p.key == key)
    }
}

/// All filters known to the app, in picker order.
#[derive(Clone, Default)]
pub struct Registry {
    filters: Vec<Arc<FilterDescriptor>>,
    /// Non-fatal problems found while loading plugins, shown in the TUI.
    pub warnings: Vec<String>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry with every built-in filter.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        crate::filters::register_builtins(&mut r);
        r
    }

    /// Add a filter; replaces an existing one with the same id.
    pub fn register(&mut self, desc: FilterDescriptor) {
        let desc = Arc::new(desc);
        match self.filters.iter_mut().find(|d| d.id == desc.id) {
            Some(slot) => *slot = desc,
            None => self.filters.push(desc),
        }
    }

    /// Discover external process plugins (see `exec_plugin`).
    pub fn load_exec_plugins(&mut self) {
        crate::exec_plugin::load_into(self);
    }

    pub fn get(&self, id: &str) -> Option<Arc<FilterDescriptor>> {
        self.filters.iter().find(|d| d.id == id).cloned()
    }

    pub fn all(&self) -> &[Arc<FilterDescriptor>] {
        &self.filters
    }
}

/// Filter that does nothing. Handy for stubs and tests.
pub struct Passthrough;

impl Filter for Passthrough {
    fn set_param(&mut self, _index: usize, _value: f32) {}
    fn process(&mut self, _buf: &mut [f32]) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_param_roundtrip() {
        let p = ParamSpec::new("cutoff", "Cutoff", 20.0, 20000.0, 1000.0).unit("Hz").log();
        for v in [20.0, 440.0, 1000.0, 20000.0] {
            assert!((p.from_norm(p.to_norm(v)) - v).abs() / v < 1e-3);
        }
        assert_eq!(p.format_value(2500.0), "2.50 kHz");
    }

    #[test]
    fn choices_quantize() {
        let p = ParamSpec::new("pp", "Ping-pong", 0.0, 0.0, 0.0).choices(&["off", "on"]);
        assert_eq!(p.sanitize(0.7), 1.0);
        assert_eq!(p.format_value(1.0), "on");
    }
}

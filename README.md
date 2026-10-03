# soundpipe

A terminal audio player that chains filters like Unix pipes. Load a file, stack
filters (EQ, compressor, delay, reverb, ...), tweak them live, and export the
chain as a shell pipeline. The binary is `sp`.

## Install

```sh
cargo install --path .
```

## TUI

```sh
sp song.mp3                 # open the TUI
sp song.mp3 --device NAME   # pick an output device (see `sp devices`)
```

Add filters from the picker, edit their parameters with sliders, bypass or
reorder them, and play/seek. Press `?` in the app for the key list.
Plugin loading problems are shown in the app and in `sp list`.

## Pipe commands

Every filter is also a plain Unix tool. The stream format between commands is
raw interleaved f32 little-endian, 44100 Hz, 2 channels by default (override
with `--rate` / `--channels`, which must match across the pipe).

```sh
sp list                                   # filters, ids, params (key, range, default, unit)
sp devices                                # output devices (* = default)
sp decode song.mp3 > song.f32             # any format -> raw f32le
sp decode song.mp3 | sp filter lowpass cutoff=800 | sp play
sp decode a.wav --rate 48000 --channels 1 | sp filter gain gain=-6 --rate 48000 --channels 1 | sp play --rate 48000 --channels 1
```

`sp filter <id> key=value ...` processes stdin to stdout in blocks of 1024
frames; unspecified params use their defaults. `sp play` waits until the audio
has finished playing. A closed pipe (`| head`) exits quietly.
Pressing the export key in the TUI prints the current chain as such a pipeline.

## Writing plugins

### 1. External process plugins (any language)

Drop a manifest `<name>.toml` in `$SP_PLUGIN_PATH` (colon-separated),
`./plugins`, or `~/.config/sp/plugins`:

```toml
id = "bitcrush"                # unique, lowercase, no spaces
name = "Bitcrush"
category = "Plugin"            # default "Plugin"
description = "Lo-fi effect"
command = ["python3", "bitcrush.py"]   # relative paths resolve against the manifest dir,
                                       # which is also the working directory

[[params]]
key = "bits"                   # used as `sp filter bitcrush bits=4`
label = "Bit depth"
min = 1
max = 16
default = 8
step = 1                       # 0 = continuous
unit = "bit"
scale = "linear"               # or "log" (min must be > 0)
# choices = ["sine", "square"] # named integer values 0..n-1 (min/max implied)
```

Protocol:

- stdin: raw interleaved f32le samples; stdout: the same number of processed
  samples, **flushed after each chunk you read**. Never output more or fewer
  samples than you read.
- Environment: `SP_SAMPLE_RATE`, `SP_CHANNELS`, and `SP_PARAM_<KEY>` (key
  uppercased, e.g. `SP_PARAM_BITS=8`) for each param.
- Parameters are read once at start-up; when the user changes one in the TUI the
  process is restarted (with ~150 ms debounce).
- stderr goes to `<tmpdir>/sp-plugin-<id>.log`, not the terminal.
- In the TUI, if the plugin cannot keep up (more than ~20 ms latency) the dry
  signal passes through; if it dies, the filter becomes a passthrough.
  Offline (`sp filter`, rendering) it waits for the output.

See `plugins/bitcrush.py` for a complete Python example. Minimal shell plugin
(`command = ["cat"]`) passes audio through unchanged.

### 2. Rust plugins

Build your own binary that registers filters on the registry, then runs the
normal `sp` CLI/TUI:

```rust
use soundpipe::{Filter, FilterDescriptor, ParamSpec, Registry};

struct Half;
impl Filter for Half {
    fn set_param(&mut self, _index: usize, _value: f32) {}
    fn process(&mut self, buf: &mut [f32]) { buf.iter_mut().for_each(|s| *s *= 0.5); }
}

fn main() {
    let mut reg = Registry::with_builtins();
    reg.register(FilterDescriptor::native("half", "Half", "Plugin",
        vec![ParamSpec::new("dummy", "Dummy", 0.0, 1.0, 0.0)], |_ctx| Box::new(Half)));
    soundpipe::run(reg).unwrap();
}
```

Buffers are interleaved `f32` (`buf.len()` is a multiple of
`ctx.channels`); `process` must not allocate or block when `ctx.realtime`.
`set_param(index, value)` indexes the descriptor's params. A full example with
a log-scaled slider and a choice parameter lives in `examples/custom_filter.rs`:

```sh
cargo run --example custom_filter -- list
cargo run --example custom_filter -- song.mp3
```

Because filters are shared by the whole CLI, a Rust plugin also works in
`sp filter` (via your own binary): `custom_filter filter tremolo rate=6`.

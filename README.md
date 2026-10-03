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

```
 song.mp3 ──▶ EQ ──▶ Compressor ──▶ Reverb ──▶ Speaker
```

The chain runs left to right, from the file to the speaker. Each box shows a
level meter, and you can change settings while the sound plays.

| Key | Action |
|---|---|
| `←` `→` | select a box |
| `a` | add a filter after the selected box |
| `d` / `Del` | delete the selected filter |
| `b` | bypass the selected filter |
| `<` `>` | move the selected filter left / right |
| `⏎` / `Tab` | open the editor for the selected filter (`Esc` goes back) |
| `space` | play / pause |
| `[` `]` | seek back / forward 5 s |
| `l` | loop on / off |
| `o` | output picker: choose a device, or render the chain to a WAV file |
| `x` | export the chain as a shell pipeline (copied to the clipboard on macOS) |
| `q` | quit |

The key list is always shown on the bottom line.

**Mouse:**
- Click an arrow `──▶` to add a filter at that spot.
- Click a box to select it, and click it again to edit it.
- Click the speaker to choose the output.
- Click or drag sliders and EQ bars. The scroll wheel adjusts the selected setting.
- Click the progress bar to seek.

**Editors:** most filters use one slider per setting. The Equalizer shows 8
vertical band bars (`←→` band, `↑↓` gain, `Tab` switches to the extra
sliders). The Compressor shows a live gain-reduction meter.

**Rendering to WAV** (from the `o` picker) writes `<file>.sp.wav` next to the
source. It keeps going after the song ends until the reverb/delay tail fades
out (below -90 dBFS for 0.5 s, at most 10 s extra).

Plugin loading problems are shown in the app and in `sp list`.

## Built-in filters

| Id | Name | Category | What it does |
|---|---|---|---|
| `eq` | Equalizer | Tone | 8-band graphic EQ (60 Hz – 16 kHz, ±12 dB) |
| `lowpass` | Low-pass | Tone | removes highs above the cutoff |
| `highpass` | High-pass | Tone | removes lows below the cutoff |
| `gain` | Gain | Tone | volume and stereo pan |
| `compressor` | Compressor | Dynamics | evens out loud and quiet parts, with gain-reduction meter |
| `distortion` | Distortion | Drive | soft, hard or fuzz clipping with a tone control |
| `chorus` | Chorus | Modulation | thickens the sound with a slowly moving delay |
| `flanger` | Flanger | Modulation | jet-like sweep with feedback |
| `delay` | Delay | Time | clean repeating delay |
| `echo` | Echo | Time | tape-style echo, darker repeats, optional ping-pong |
| `reverb` | Reverb | Time | Freeverb-style room reverb with predelay |

`sp list` prints every filter with its parameters (key, range, default, unit).

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
- In the TUI and when rendering, stderr goes to `<tmpdir>/sp-plugin-<id>.log`
  so it does not corrupt the screen. `sp filter` leaves stderr on the terminal.
- In the TUI, if the plugin cannot keep up (more than ~20 ms latency) the dry
  signal passes through; if it dies, the filter becomes a passthrough.
  Offline (`sp filter`, rendering) it waits for the output.

See `plugins/bitcrush.py` for a complete Python example. Plugins in `./plugins`
are only found when you run `sp` from that directory; copy the manifest and
script to `~/.config/sp/plugins` to use them everywhere. Minimal shell plugin
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

## Known issues

Tracked in [beads](https://github.com/steveyegge/beads) (`bd ready` lists them):

- `sp filter gain gain` (a setting without `=value`) shows the general usage
  text instead of a specific error.
- `sp decode --channels 9` is accepted without complaint.
- The CLI pipe commands (`sp filter` etc.) do not add an effect tail at the
  end of the stream; only Render to WAV does.

## Development

```sh
cargo test                    # unit + integration tests (bitcrush test needs python3)
cargo clippy --all-targets
bd ready                      # open work items
```

Layout: `src/plugin.rs` (plugin API), `src/filters/` (built-ins),
`src/exec_plugin.rs` (process plugins), `src/audio/` (decode, output, WAV),
`src/engine.rs` (real-time engine + render), `src/tui/`, `src/cli.rs`.

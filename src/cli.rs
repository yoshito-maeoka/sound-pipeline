//! Command line entry: `sp <file>` opens the TUI; subcommands expose the
//! same filters as plain Unix pipe tools.
//!
//!   sp <file> [--device NAME]              open the TUI
//!   sp list                                list filters and their params
//!   sp devices                             list output devices
//!   sp decode <file> [--rate R] [--channels C]   file -> raw f32le on stdout
//!   sp filter <id> [key=value ...] [--rate R] [--channels C]   stdin -> stdout
//!   sp play [--rate R] [--channels C] [--device NAME]          stdin -> speaker
//!
//! Raw stream format between commands: interleaved f32 little-endian,
//! default 44100 Hz, 2 channels.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ringbuf::HeapRb;
use ringbuf::traits::{Observer, Producer, Split};

use crate::audio::{self, AudioData};
use crate::engine::ChainNode;
use crate::plugin::{FilterContext, FilterKind, Registry};

pub const DEFAULT_RATE: u32 = 44100;
pub const DEFAULT_CHANNELS: usize = 2;
/// Frames per block in `sp filter`.
const BLOCK_FRAMES: usize = 1024;

const HELP: &str = "\
sp - chain audio filters like Unix pipes

USAGE
  sp <file> [--device NAME]                 open the TUI
  sp list                                   list filters and parameters
  sp devices                                list output devices
  sp decode <file> [--rate R] [--channels C]
                                            decode a file to raw f32le on stdout
  sp filter <id> [key=value ...] [--rate R] [--channels C]
                                            raw f32le stdin -> filter -> stdout
  sp play [--rate R] [--channels C] [--device NAME]
                                            raw f32le stdin -> speakers
  sp help                                   this text

Raw format: interleaved f32 little-endian, default 44100 Hz, 2 channels.
Example: sp decode song.mp3 | sp filter lowpass cutoff=800 | sp play
Plugins: $SP_PLUGIN_PATH, ./plugins, ~/.config/sp/plugins (see README).
";

/// A parsed command line.
#[derive(Debug, PartialEq)]
pub enum Command {
    Tui { file: PathBuf, device: Option<String> },
    List,
    Devices,
    Decode { file: PathBuf, rate: u32, channels: usize },
    Filter { id: String, params: Vec<(String, f32)>, rate: u32, channels: usize },
    Play { rate: u32, channels: usize, device: Option<String> },
    Help,
}

fn flag_value(args: &[String], i: &mut usize, name: &str) -> Result<String> {
    *i += 1;
    args.get(*i).cloned().ok_or_else(|| anyhow!("{name} needs a value"))
}

/// Parse the arguments after the program name.
pub fn parse_args(args: &[String]) -> Result<Command> {
    let Some(first) = args.first() else {
        return Ok(Command::Help);
    };
    let rest = &args[1..];
    let mut rate = DEFAULT_RATE;
    let mut channels = DEFAULT_CHANNELS;
    let mut device: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut kv: Vec<(String, f32)> = Vec::new();

    let sub = match first.as_str() {
        "help" | "--help" | "-h" => return Ok(Command::Help),
        "list" | "devices" | "decode" | "filter" | "play" => first.as_str(),
        _ => "tui",
    };
    let rest: &[String] = if sub == "tui" { args } else { rest };

    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        match a.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--rate" | "-r" if sub != "tui" => {
                let v = flag_value(rest, &mut i, "--rate")?;
                rate = v.parse().ok().filter(|r| *r > 0).ok_or_else(|| anyhow!("bad --rate '{v}'"))?;
            }
            "--channels" | "-c" if sub != "tui" => {
                let v = flag_value(rest, &mut i, "--channels")?;
                channels = v.parse().ok().filter(|c| *c > 0).ok_or_else(|| anyhow!("bad --channels '{v}'"))?;
            }
            "--device" | "-d" => device = Some(flag_value(rest, &mut i, "--device")?),
            s if s.starts_with("--") => bail!("unknown option '{s}'"),
            s if sub == "filter" && positional.len() == 1 && s.contains('=') => {
                let (k, v) = s.split_once('=').unwrap();
                let v: f32 = v.parse().map_err(|_| anyhow!("bad value for '{k}': '{v}'"))?;
                kv.push((k.to_string(), v));
            }
            _ => positional.push(a.clone()),
        }
        i += 1;
    }

    let need_none = |positional: &[String]| -> Result<()> {
        match positional.first() {
            Some(p) => bail!("unexpected argument '{p}'"),
            None => Ok(()),
        }
    };
    match sub {
        "list" => need_none(&positional).map(|_| Command::List),
        "devices" => need_none(&positional).map(|_| Command::Devices),
        "play" => need_none(&positional).map(|_| Command::Play { rate, channels, device }),
        "decode" => match positional.as_slice() {
            [f] => Ok(Command::Decode { file: f.into(), rate, channels }),
            _ => bail!("usage: sp decode <file> [--rate R] [--channels C]"),
        },
        "filter" => match positional.as_slice() {
            [id] => Ok(Command::Filter { id: id.clone(), params: kv, rate, channels }),
            _ => bail!("usage: sp filter <id> [key=value ...] [--rate R] [--channels C]"),
        },
        _ => match positional.as_slice() {
            [f] => Ok(Command::Tui { file: f.into(), device }),
            _ => bail!("usage: sp <file> [--device NAME]  (try `sp help`)"),
        },
    }
}

/// Parse `std::env::args()` and run. Loads exec plugins into `registry` first.
pub fn run(mut registry: Registry) -> Result<()> {
    registry.load_exec_plugins();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = parse_args(&args)?;
    let res = match cmd {
        Command::Help => {
            print!("{HELP}");
            Ok(())
        }
        Command::Tui { file, device } => crate::tui::run(registry, file, device),
        Command::List => cmd_list(&registry),
        Command::Devices => cmd_devices(),
        Command::Decode { file, rate, channels } => cmd_decode(&file, rate, channels),
        Command::Filter { id, params, rate, channels } => cmd_filter(&registry, &id, &params, rate, channels),
        Command::Play { rate, channels, device } => cmd_play(rate, channels, device.as_deref()),
    };
    match res {
        Err(e) if is_broken_pipe(&e) => Ok(()),
        other => other,
    }
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<io::Error>())
        .any(|io| io.kind() == io::ErrorKind::BrokenPipe)
}

fn cmd_list(reg: &Registry) -> Result<()> {
    let mut out = String::new();
    for d in reg.all() {
        let src = match d.kind {
            FilterKind::Native => "",
            FilterKind::Exec(_) => " [plugin process]",
        };
        out.push_str(&format!("{}  {} ({}){}\n", d.id, d.name, d.category, src));
        if !d.description.is_empty() {
            out.push_str(&format!("    {}\n", d.description));
        }
        for p in &d.params {
            let unit = if p.unit.is_empty() { String::new() } else { format!(" {}", p.unit) };
            let choices = if p.choices.is_empty() {
                String::new()
            } else {
                format!("  choices: {}", p.choices.join("/"))
            };
            out.push_str(&format!(
                "    {:<10} {}..{}  default {}{}{}\n",
                p.key,
                p.min,
                p.max,
                p.default,
                unit,
                choices
            ));
        }
    }
    for w in &reg.warnings {
        eprintln!("warning: {w}");
    }
    io::stdout().write_all(out.as_bytes())?;
    Ok(())
}

fn cmd_devices() -> Result<()> {
    let mut out = String::new();
    for d in audio::list_output_devices() {
        out.push_str(&format!("{} {}\n", if d.is_default { "*" } else { " " }, d.name));
    }
    io::stdout().write_all(out.as_bytes())?;
    Ok(())
}

fn f32s_to_bytes(samples: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

fn cmd_decode(file: &Path, rate: u32, channels: usize) -> Result<()> {
    let data = audio::decode_file(file).with_context(|| format!("decoding {}", file.display()))?;
    let data = audio::convert(&data, rate, channels);
    let mut out = io::stdout().lock();
    for chunk in data.samples.chunks(16384) {
        out.write_all(&f32s_to_bytes(chunk))?;
    }
    out.flush()?;
    Ok(())
}

/// Read until `buf` is full or EOF; returns bytes read.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Stream raw f32le frames from `input` in blocks, calling `f` for each
/// complete block of samples. A trailing partial frame is dropped.
fn for_each_block(input: &mut impl Read, channels: usize, mut f: impl FnMut(&mut [f32]) -> Result<()>) -> Result<()> {
    let frame_bytes = 4 * channels;
    let mut bytes = vec![0u8; BLOCK_FRAMES * frame_bytes];
    let mut samples = vec![0f32; BLOCK_FRAMES * channels];
    loop {
        let n = read_full(input, &mut bytes)?;
        let frames = n / frame_bytes;
        if frames == 0 {
            return Ok(());
        }
        let ns = frames * channels;
        for (s, c) in samples[..ns].iter_mut().zip(bytes.chunks_exact(4)) {
            *s = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        }
        f(&mut samples[..ns])?;
        if n < bytes.len() {
            return Ok(());
        }
    }
}

fn cmd_filter(reg: &Registry, id: &str, kv: &[(String, f32)], rate: u32, channels: usize) -> Result<()> {
    let desc = reg
        .get(id)
        .ok_or_else(|| anyhow!("unknown filter '{id}' (try `sp list`)"))?;
    let mut values = desc.defaults();
    for (k, v) in kv {
        let i = desc.param_index(k).ok_or_else(|| {
            let keys: Vec<&str> = desc.params.iter().map(|p| p.key.as_str()).collect();
            anyhow!("filter '{id}' has no parameter '{k}' (available: {})", keys.join(", "))
        })?;
        values[i] = desc.params[i].sanitize(*v);
    }
    let ctx = FilterContext { sample_rate: rate as f32, channels, realtime: false };

    if let FilterKind::Exec(spec) = &desc.kind {
        let mut cmd = spec.build_command(&ctx, &desc.params, &values);
        let status = cmd
            .status()
            .with_context(|| format!("cannot start plugin '{id}' ({})", spec.command[0]))?;
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
        return Ok(());
    }

    let mut filter = desc.instantiate(&ctx, &values)?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    for_each_block(&mut stdin, channels, |block| {
        filter.process(block);
        stdout.write_all(&f32s_to_bytes(block))?;
        stdout.flush()?;
        Ok(())
    })
}

fn cmd_play(rate: u32, channels: usize, device: Option<&str>) -> Result<()> {
    let rb = HeapRb::<f32>::new(192_000);
    let (mut prod, cons) = rb.split();
    let flush = Arc::new(AtomicBool::new(false));
    let stream = audio::open_output(device, rate, cons, flush)?;
    let out_rate = stream.config.sample_rate;
    let out_ch = stream.config.channels;
    let convert_needed = out_rate != rate || out_ch != channels;

    let mut push = |samples: &[f32]| {
        let mut off = 0;
        while off < samples.len() {
            off += prod.push_slice(&samples[off..]);
            if off < samples.len() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let mut stdin = io::stdin().lock();
    // Larger blocks when converting, to keep resampling boundary effects small.
    for_each_block(&mut stdin, channels, |block| {
        if convert_needed {
            let data = AudioData { samples: block.to_vec(), sample_rate: rate, channels };
            push(&audio::convert(&data, out_rate, out_ch).samples);
        } else {
            push(block);
        }
        Ok(())
    })?;
    // Wait for the ring to drain, then for the device buffer to play out.
    while prod.occupied_len() > 0 {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(300));
    drop(stream);
    Ok(())
}

/// Single-quote a string for POSIX shells.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Shell pipeline equivalent to the chain (bypassed nodes omitted), e.g.
/// `sp decode 'song.mp3' --rate 48000 --channels 2 | sp filter lowpass cutoff=800 q=0.71 --rate 48000 --channels 2 | sp play --rate 48000 --channels 2`
pub fn export_pipeline(file: &Path, chain: &[ChainNode], rate: u32, channels: usize) -> String {
    let fmt = format!("--rate {rate} --channels {channels}");
    let mut parts = vec![format!("sp decode {} {fmt}", shell_quote(&file.display().to_string()))];
    for node in chain.iter().filter(|n| !n.bypass) {
        let mut s = format!("sp filter {}", node.desc.id);
        for (i, p) in node.desc.params.iter().enumerate() {
            let v = node.params.get(i).copied().unwrap_or(p.default);
            s.push_str(&format!(" {}={}", p.key, v));
        }
        s.push(' ');
        s.push_str(&fmt);
        parts.push(s);
    }
    parts.push(format!("sp play {fmt}"));
    parts.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{FilterDescriptor, ParamSpec, Passthrough};

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_subcommands() {
        assert_eq!(parse_args(&a(&[])).unwrap(), Command::Help);
        assert_eq!(parse_args(&a(&["list"])).unwrap(), Command::List);
        assert_eq!(
            parse_args(&a(&["song.mp3", "--device", "X"])).unwrap(),
            Command::Tui { file: "song.mp3".into(), device: Some("X".into()) }
        );
        assert_eq!(
            parse_args(&a(&["decode", "a.wav", "--rate", "48000", "--channels", "1"])).unwrap(),
            Command::Decode { file: "a.wav".into(), rate: 48000, channels: 1 }
        );
        assert_eq!(
            parse_args(&a(&["filter", "gain", "gain=-6", "--rate", "22050"])).unwrap(),
            Command::Filter { id: "gain".into(), params: vec![("gain".into(), -6.0)], rate: 22050, channels: 2 }
        );
        assert_eq!(
            parse_args(&a(&["play", "--device", "Spk"])).unwrap(),
            Command::Play { rate: 44100, channels: 2, device: Some("Spk".into()) }
        );
    }

    #[test]
    fn rejects_bad_args() {
        assert!(parse_args(&a(&["decode"])).is_err());
        assert!(parse_args(&a(&["filter"])).is_err());
        assert!(parse_args(&a(&["filter", "x", "k=abc"])).is_err());
        assert!(parse_args(&a(&["play", "--rate"])).is_err());
        assert!(parse_args(&a(&["play", "--rate", "0"])).is_err());
        assert!(parse_args(&a(&["play", "--bogus"])).is_err());
    }

    #[test]
    fn exports_pipeline() {
        let d = Arc::new(FilterDescriptor::native(
            "lowpass",
            "Lowpass",
            "Tone",
            vec![
                ParamSpec::new("cutoff", "Cutoff", 20.0, 20000.0, 1000.0),
                ParamSpec::new("q", "Q", 0.1, 10.0, 0.71),
            ],
            |_| Box::new(Passthrough),
        ));
        let chain = vec![
            ChainNode { desc: d.clone(), params: vec![800.0, 0.71], bypass: false },
            ChainNode { desc: d, params: vec![1.0, 1.0], bypass: true },
        ];
        let s = export_pipeline(Path::new("it's a song.mp3"), &chain, 48000, 2);
        assert_eq!(
            s,
            "sp decode 'it'\\''s a song.mp3' --rate 48000 --channels 2 | \
             sp filter lowpass cutoff=800 q=0.71 --rate 48000 --channels 2 | \
             sp play --rate 48000 --channels 2"
        );
    }

    #[test]
    fn block_reader_drops_partial_frame() {
        let mut bytes = f32s_to_bytes(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        bytes.extend_from_slice(&[0, 0]);
        let mut got = Vec::new();
        for_each_block(&mut &bytes[..], 2, |b| {
            got.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        assert_eq!(got, vec![1.0, 2.0, 3.0, 4.0]);
    }
}

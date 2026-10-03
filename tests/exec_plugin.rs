use std::path::Path;
use std::process::Command;

use soundpipe::{FilterContext, FilterKind, Registry};

fn python_available() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

fn registry() -> Registry {
    let mut reg = Registry::new();
    soundpipe::exec_plugin::load_dir(&mut reg, &Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins"));
    assert!(reg.warnings.is_empty(), "{:?}", reg.warnings);
    reg
}

#[test]
fn bitcrush_manifest_loads() {
    let reg = registry();
    let d = reg.get("bitcrush").expect("bitcrush registered");
    assert!(matches!(d.kind, FilterKind::Exec(_)));
    assert_eq!(d.param_index("bits"), Some(0));
    assert_eq!(d.params[1].default, 4.0);
}

#[test]
fn bitcrush_processes_audio() {
    if !python_available() {
        eprintln!("python3 missing, skipping");
        return;
    }
    let reg = registry();
    let d = reg.get("bitcrush").unwrap();
    let ctx = FilterContext { sample_rate: 44100.0, channels: 2, realtime: false };
    // 2 bits, hold 1 -> only levels of 0.5 multiples (levels = 2)
    let mut f = d.instantiate(&ctx, &[2.0, 1.0]).unwrap();
    let mut buf: Vec<f32> = (0..4096).map(|i| ((i as f32) * 0.01).sin() * 0.9).collect();
    let orig = buf.clone();
    for chunk in buf.chunks_mut(1024) {
        f.process(chunk);
    }
    assert_ne!(buf, orig);
    assert!(buf.iter().all(|s| [-1.0, -0.5, 0.0, 0.5, 1.0].contains(s)), "unexpected level");
}

#[test]
fn bitcrush_downsample_holds_samples() {
    if !python_available() {
        return;
    }
    let reg = registry();
    let d = reg.get("bitcrush").unwrap();
    let ctx = FilterContext { sample_rate: 44100.0, channels: 1, realtime: false };
    let mut f = d.instantiate(&ctx, &[16.0, 4.0]).unwrap();
    let mut buf: Vec<f32> = (0..64).map(|i| i as f32 / 100.0).collect();
    f.process(&mut buf);
    for g in buf.chunks(4) {
        assert!(g.iter().all(|s| (*s - g[0]).abs() < 1e-6));
    }
}

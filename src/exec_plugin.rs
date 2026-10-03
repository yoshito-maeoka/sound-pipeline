//! External process plugins — the Unix-pipe way to write a custom filter in
//! any language.
//!
//! A plugin is a `<name>.toml` manifest in a plugin directory, describing a
//! command that reads interleaved f32le samples on stdin and writes the same
//! number of processed samples on stdout (flushing after every chunk).
//! Stream format and parameter values arrive as environment variables:
//! `SP_SAMPLE_RATE`, `SP_CHANNELS`, `SP_PARAM_<KEY>` (key uppercased).
//! Parameters are env vars, so changing one restarts the process.
//!
//! Plugin directories, in priority order: `$SP_PLUGIN_PATH` (colon
//! separated), `./plugins`, `~/.config/sp/plugins`.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use crate::plugin::{
    EditorKind, Filter, FilterContext, FilterDescriptor, FilterKind, ParamSpec, Registry, Scale,
};

/// Max time `process()` waits for plugin output in realtime mode.
const REALTIME_WAIT: Duration = Duration::from_millis(20);
/// Max time `process()` waits for plugin output offline.
const OFFLINE_WAIT: Duration = Duration::from_secs(5);
/// Delay between a parameter change and the process restart.
const RESTART_DEBOUNCE: Duration = Duration::from_millis(150);

/// Parsed manifest, shared by every instance of the plugin.
#[derive(Clone, Debug)]
pub struct ExecSpec {
    pub id: String,
    /// Directory of the manifest; the command runs with this as cwd.
    pub dir: PathBuf,
    pub command: Vec<String>,
    pub name: String,
    pub params: Vec<ParamSpec>,
}

/// Environment variable name for a parameter key.
pub fn param_env_name(key: &str) -> String {
    let k: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
        .collect();
    format!("SP_PARAM_{k}")
}

impl ExecSpec {
    /// Build the process command with SP_* env vars set for `ctx` and the
    /// given param values. Used by the in-app filter and by `sp filter`,
    /// which execs it directly with inherited stdio.
    pub fn build_command(&self, ctx: &FilterContext, params: &[ParamSpec], values: &[f32]) -> Command {
        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..]).current_dir(&self.dir);
        cmd.env("SP_SAMPLE_RATE", format!("{}", ctx.sample_rate.round() as u32));
        cmd.env("SP_CHANNELS", ctx.channels.to_string());
        for (i, p) in params.iter().enumerate() {
            let v = p.sanitize(values.get(i).copied().unwrap_or(p.default));
            cmd.env(param_env_name(&p.key), format!("{v}"));
        }
        cmd
    }
}

// ---------------------------------------------------------------- manifest

#[derive(Deserialize)]
struct Manifest {
    id: String,
    name: Option<String>,
    category: Option<String>,
    #[serde(default)]
    description: String,
    command: Vec<String>,
    #[serde(default)]
    params: Vec<ManifestParam>,
}

#[derive(Deserialize)]
struct ManifestParam {
    key: String,
    label: Option<String>,
    #[serde(default)]
    min: f32,
    #[serde(default = "one")]
    max: f32,
    default: Option<f32>,
    #[serde(default)]
    step: f32,
    #[serde(default)]
    unit: String,
    scale: Option<String>,
    #[serde(default)]
    choices: Vec<String>,
}

fn one() -> f32 {
    1.0
}

/// Parse manifest text; `dir` is the manifest directory.
pub fn parse_manifest(text: &str, dir: &Path) -> Result<FilterDescriptor> {
    let m: Manifest = toml::from_str(text).map_err(|e| anyhow!("invalid manifest: {}", e.message()))?;
    if m.id.is_empty() || m.id.chars().any(|c| c.is_whitespace() || c.is_uppercase()) {
        bail!("id must be non-empty, lowercase, without spaces");
    }
    if m.command.is_empty() || m.command[0].is_empty() {
        bail!("command must be a non-empty array");
    }
    let command: Vec<String> = m
        .command
        .iter()
        .map(|c| {
            let p = Path::new(c);
            if p.is_relative() && dir.join(p).exists() {
                dir.join(p).to_string_lossy().into_owned()
            } else {
                c.clone()
            }
        })
        .collect();

    let mut params: Vec<ParamSpec> = Vec::new();
    for mp in m.params {
        if mp.key.is_empty() {
            bail!("param with empty key");
        }
        if params.iter().any(|p| p.key == mp.key) {
            bail!("duplicate param key '{}'", mp.key);
        }
        let label = mp.label.clone().unwrap_or_else(|| mp.key.clone());
        let mut p = ParamSpec::new(&mp.key, &label, mp.min, mp.max, mp.default.unwrap_or(mp.min));
        p.unit = mp.unit;
        p.step = mp.step.max(0.0);
        if !mp.choices.is_empty() {
            let names: Vec<&str> = mp.choices.iter().map(String::as_str).collect();
            p = p.choices(&names);
            p.default = mp.default.unwrap_or(0.0);
        } else {
            if mp.max <= mp.min {
                bail!("param '{}': max must be greater than min", mp.key);
            }
            match mp.scale.as_deref() {
                None | Some("linear") => {}
                Some("log") => {
                    if mp.min <= 0.0 {
                        bail!("param '{}': log scale needs min > 0", mp.key);
                    }
                    p.scale = Scale::Log;
                }
                Some(other) => bail!("param '{}': unknown scale '{other}'", mp.key),
            }
        }
        p.default = p.sanitize(p.default);
        params.push(p);
    }

    let name = m.name.unwrap_or_else(|| m.id.clone());
    let spec = Arc::new(ExecSpec {
        id: m.id.clone(),
        dir: dir.to_path_buf(),
        command,
        name: name.clone(),
        params: params.clone(),
    });
    let fspec = spec.clone();
    Ok(FilterDescriptor {
        id: m.id,
        name,
        category: m.category.unwrap_or_else(|| "Plugin".into()),
        description: m.description,
        params,
        editor: EditorKind::Sliders,
        meter_label: None,
        kind: FilterKind::Exec(spec),
        factory: Arc::new(move |ctx| Ok(Box::new(ExecFilter::new(fspec.clone(), *ctx)) as Box<dyn Filter>)),
    })
}

/// Plugin search path, in priority order.
pub fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("SP_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&p).filter(|d| !d.as_os_str().is_empty()));
    }
    dirs.push(PathBuf::from("plugins"));
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/sp/plugins"));
    }
    let canon = |d: &Path| d.canonicalize().unwrap_or_else(|_| d.to_path_buf());
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if !out.iter().any(|o| canon(o) == canon(&d)) {
            out.push(d);
        }
    }
    out
}

/// Scan `plugin_dirs()` and register each manifest; problems go to `reg.warnings`.
pub fn load_into(reg: &mut Registry) {
    for dir in plugin_dirs() {
        load_dir(reg, &dir);
    }
}

/// Register every `*.toml` manifest in `dir`.
pub fn load_dir(reg: &mut Registry, dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    let dir_abs = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for path in files {
        let res = std::fs::read_to_string(&path)
            .map_err(|e| anyhow!("{e}"))
            .and_then(|t| parse_manifest(&t, &dir_abs));
        match res {
            Ok(desc) => match reg.get(&desc.id) {
                Some(existing) => {
                    let same = matches!(&existing.kind, FilterKind::Exec(s) if s.dir == dir_abs);
                    if !same {
                        reg.warnings.push(format!(
                            "plugin {}: id '{}' already registered, skipped",
                            path.display(),
                            desc.id
                        ));
                    }
                }
                None => reg.register(desc),
            },
            Err(e) => reg.warnings.push(format!("plugin {}: {e}", path.display())),
        }
    }
}

// ------------------------------------------------------------- ExecFilter

/// Output side shared with the reader thread.
struct Shared {
    out: Mutex<VecDeque<f32>>,
    cv: Condvar,
    dead: AtomicBool,
    /// Samples sent to the writer thread but not yet written to the pipe.
    inflight: AtomicUsize,
}

struct Running {
    child: Child,
    tx: Sender<Vec<f32>>,
    shared: Arc<Shared>,
    /// Output samples to drop when they eventually arrive (we already
    /// passed the dry signal for them after a stall).
    discard: usize,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A [`Filter`] backed by an external process.
pub struct ExecFilter {
    spec: Arc<ExecSpec>,
    ctx: FilterContext,
    values: Vec<f32>,
    run: Option<Running>,
    dirty_since: Option<Instant>,
    failed: bool,
    stall_logged: bool,
}

impl ExecFilter {
    pub fn new(spec: Arc<ExecSpec>, ctx: FilterContext) -> Self {
        let values = spec.params.iter().map(|p| p.default).collect();
        Self { spec, ctx, values, run: None, dirty_since: None, failed: false, stall_logged: false }
    }

    fn log_path(&self) -> PathBuf {
        std::env::temp_dir().join(format!("sp-plugin-{}.log", self.spec.id))
    }

    fn spawn(&mut self) -> Result<Running> {
        let mut cmd = self.spec.build_command(&self.ctx, &self.spec.params, &self.values);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        match File::create(self.log_path()) {
            Ok(f) => cmd.stderr(Stdio::from(f)),
            Err(_) => cmd.stderr(Stdio::null()),
        };
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("cannot start plugin '{}' ({}): {e}", self.spec.id, self.spec.command[0]))?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let shared = Arc::new(Shared {
            out: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            dead: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
        });
        let (tx, rx) = mpsc::channel::<Vec<f32>>();

        let sh = shared.clone();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            while let Ok(chunk) = rx.recv() {
                bytes.clear();
                for s in &chunk {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                let ok = stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_ok();
                sh.inflight.fetch_sub(chunk.len(), Ordering::SeqCst);
                if !ok {
                    break;
                }
            }
        });

        let sh = shared.clone();
        thread::spawn(move || {
            let mut raw = [0u8; 16384];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                match stdout.read(&mut raw) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        carry.extend_from_slice(&raw[..n]);
                        let whole = carry.len() / 4 * 4;
                        let mut q = sh.out.lock().unwrap();
                        for c in carry[..whole].chunks_exact(4) {
                            q.push_back(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                        }
                        drop(q);
                        carry.drain(..whole);
                        sh.cv.notify_all();
                    }
                }
            }
            sh.dead.store(true, Ordering::SeqCst);
            sh.cv.notify_all();
        });

        Ok(Running { child, tx, shared, discard: 0 })
    }

    fn restart(&mut self) {
        self.run = None; // Drop kills the child
        self.dirty_since = None;
        self.stall_logged = false;
        match self.spawn() {
            Ok(r) => {
                self.run = Some(r);
                self.failed = false;
            }
            Err(e) => {
                self.failed = true;
                log_line(&self.log_path(), &format!("sp: {e}"));
            }
        }
    }
}

impl Filter for ExecFilter {
    fn set_param(&mut self, index: usize, value: f32) {
        if let Some(v) = self.values.get_mut(index)
            && *v != value
        {
            *v = value;
            if self.run.is_some() || self.failed {
                self.dirty_since = Some(Instant::now());
            }
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        if buf.is_empty() {
            return;
        }
        if let Some(t) = self.dirty_since
            && (t.elapsed() >= RESTART_DEBOUNCE || !self.ctx.realtime)
        {
            self.restart();
        }
        if self.run.is_none() && !self.failed {
            self.restart();
        }
        let Some(run) = self.run.as_mut() else { return };
        if run.shared.dead.load(Ordering::SeqCst) && run.shared.out.lock().unwrap().len() < buf.len() {
            return; // plugin died: passthrough
        }

        // Don't let the pipe back up unboundedly if the plugin is slow.
        let cap = (self.ctx.sample_rate as usize).max(8000) * self.ctx.channels.max(1);
        if run.shared.inflight.load(Ordering::SeqCst) > cap {
            return;
        }
        run.shared.inflight.fetch_add(buf.len(), Ordering::SeqCst);
        if run.tx.send(buf.to_vec()).is_err() {
            run.shared.inflight.fetch_sub(buf.len(), Ordering::SeqCst);
            return;
        }

        let wait = if self.ctx.realtime { REALTIME_WAIT } else { OFFLINE_WAIT };
        let deadline = Instant::now() + wait;
        let mut q = run.shared.out.lock().unwrap();
        loop {
            if run.discard > 0 {
                let d = run.discard.min(q.len());
                q.drain(..d);
                run.discard -= d;
            }
            if run.discard == 0 && q.len() >= buf.len() {
                let n = buf.len();
                for (dst, src) in buf.iter_mut().zip(q.drain(..n)) {
                    *dst = src;
                }
                return;
            }
            if run.shared.dead.load(Ordering::SeqCst) {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            q = run.shared.cv.wait_timeout(q, deadline - now).unwrap().0;
        }
        // Stalled: keep the dry signal, drop this much output later.
        run.discard += buf.len();
        drop(q);
        if !self.stall_logged {
            self.stall_logged = true;
            log_line(&self.log_path(), &format!("sp: plugin '{}' stalled; passing dry signal", self.spec.id));
        }
    }

    fn reset(&mut self) {
        // Restart so the plugin's internal state is cleared.
        if self.run.is_some() {
            self.restart();
        }
    }
}

/// Append a line to the plugin log (never print: it would corrupt the TUI).
fn log_line(path: &Path, msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
id = "bitcrush"
name = "Bitcrush"
command = ["python3", "bitcrush.py"]

[[params]]
key = "bits"
label = "Bits"
min = 1
max = 16
default = 8
step = 1

[[params]]
key = "freq"
min = 20
max = 20000
default = 1000
scale = "log"
unit = "Hz"

[[params]]
key = "shape"
choices = ["sine", "square"]
"#;

    #[test]
    fn parses_manifest() {
        let d = parse_manifest(MANIFEST, Path::new("/nonexistent")).unwrap();
        assert_eq!(d.id, "bitcrush");
        assert_eq!(d.category, "Plugin");
        assert_eq!(d.params.len(), 3);
        assert_eq!(d.params[0].default, 8.0);
        assert_eq!(d.params[1].scale, Scale::Log);
        assert_eq!(d.params[2].max, 1.0);
        assert_eq!(d.params[2].choices.len(), 2);
        assert!(matches!(d.kind, FilterKind::Exec(_)));
    }

    #[test]
    fn rejects_bad_manifests() {
        let p = Path::new(".");
        assert!(parse_manifest("id = \"x\"\ncommand = []", p).is_err());
        assert!(parse_manifest("id = \"Bad Id\"\ncommand = [\"x\"]", p).is_err());
        assert!(parse_manifest("command = [\"x\"]", p).is_err());
        assert!(parse_manifest("id=\"x\"\ncommand=[\"x\"]\n[[params]]\nkey=\"a\"\nmin=0\nmax=1\nscale=\"log\"", p).is_err());
        assert!(parse_manifest("id=\"x\"\ncommand=[\"x\"]\n[[params]]\nkey=\"a\"\nmin=2\nmax=1", p).is_err());
    }

    #[test]
    fn env_names_and_command() {
        assert_eq!(param_env_name("cut-off"), "SP_PARAM_CUT_OFF");
        let d = parse_manifest(MANIFEST, Path::new("/nonexistent")).unwrap();
        let FilterKind::Exec(spec) = &d.kind else { panic!() };
        let ctx = FilterContext { sample_rate: 48000.0, channels: 2, realtime: false };
        let cmd = spec.build_command(&ctx, &d.params, &[4.0, 500.0, 1.0]);
        let envs: Vec<(String, String)> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.unwrap().to_string_lossy().into_owned()))
            .collect();
        assert!(envs.contains(&("SP_SAMPLE_RATE".into(), "48000".into())));
        assert!(envs.contains(&("SP_CHANNELS".into(), "2".into())));
        assert!(envs.contains(&("SP_PARAM_BITS".into(), "4".into())));
        assert!(envs.contains(&("SP_PARAM_SHAPE".into(), "1".into())));
    }

    #[test]
    fn load_dir_reports_warnings() {
        let dir = std::env::temp_dir().join(format!("sp-test-plugins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good.toml"), "id=\"good\"\ncommand=[\"cat\"]").unwrap();
        std::fs::write(dir.join("bad.toml"), "this is not toml ===").unwrap();
        let mut reg = Registry::new();
        load_dir(&mut reg, &dir);
        assert!(reg.get("good").is_some());
        assert_eq!(reg.warnings.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cat_plugin_roundtrips_offline() {
        let d = parse_manifest("id=\"cat\"\ncommand=[\"cat\"]", Path::new(".")).unwrap();
        let ctx = FilterContext { sample_rate: 44100.0, channels: 2, realtime: false };
        let mut f = d.instantiate(&ctx, &[]).unwrap();
        for _ in 0..3 {
            let mut buf: Vec<f32> = (0..2048).map(|i| i as f32 / 4096.0).collect();
            let orig = buf.clone();
            f.process(&mut buf);
            assert_eq!(buf, orig);
        }
    }
}

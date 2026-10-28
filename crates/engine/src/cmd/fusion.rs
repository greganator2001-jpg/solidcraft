//! Fission: run a Fusion 360 Python script (`def run(context):`, `import adsk.core, adsk.fusion`).
//! Fission ships a stand-in for Fusion's `adsk` package (`fusion/adsk` next to the program) that turns
//! the script's calls into Fission commands. The script runs in Python (the one in FissionCAD's
//! folder, else Python on the PATH) and asks Fission one command at a time, a JSON line each way:
//! `{"id", "cmd", "params"}` → `{"id", "ok", "result" | "error"}`; `{"op": "message", "text"}` is a
//! message box. Desktop only.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::CommandSpec;
use crate::schema::bambu as sch;
use crate::schema::{TypedContract, output_schema_for};

const RUN_CONTRACT: TypedContract<sch::FusionRunParams> = TypedContract::new();
const FFX_CONTRACT: TypedContract<sch::FfxOpenParams> = TypedContract::new();
use crate::params::{bad, num, str_};
use crate::{EngineError, Result, Session};

pub static COMMANDS: &[CommandSpec] = &[CommandSpec::new("fusion.run_script", "Run Fusion Script", crate::checked!("fusion.run_script", run_script_cmd))
    .contract(&RUN_CONTRACT)
    .output(output_schema_for::<sch::FusionRunResult>)
    .at("MANAGE", "ADD-INS")
    .icon("play")
    .params("path: a Fusion 360 script (.py with def run(context)); timeout?: seconds (default 300) → commands: how many steps it made, messages: its message boxes, log: what it printed"),
    CommandSpec::new("fusion.open_ffx", "Open from Fusion", crate::checked!("fusion.open_ffx", open_ffx))
    .contract(&FFX_CONTRACT)
    .output(output_schema_for::<sch::FfxOpenResult>)
    .at("MANAGE", "ADD-INS")
    .icon("import")
    .params("path: a .ffx (a Fusion design saved by the Fission Export script in Fusion) → design, bodies, parameters added, timeline: Fusion's step count, rebuilt: whether it came in as an editable timeline (else as Fusion's finished bodies), messages")];

/// Biggest .ffx read, and biggest part of one.
const MAX_FFX: u64 = 512 << 20;

/// Commands a script may call that only read the design: run again they change nothing.
const READ_ONLY: [&str; 8] = ["sketch.inspect", "model.faces", "model.edges", "inspect.measure", "timeline.inspect", "engine.describe", "model.threads", "document.inspect"];

/// What a script did.
#[derive(Clone, Debug, Default)]
pub struct ScriptRun {
    /// The commands that changed the design, in order (run them again on a copy to get the same design).
    pub journal: Vec<(String, Value)>,
    /// Its message boxes.
    pub messages: Vec<String>,
    /// What it printed.
    pub log: String,
}

/// The Python to run scripts with: `FISSION_PYTHON`, FissionCAD's own, else `py -3` / `python` / `python3`.
pub fn find_python() -> Option<Vec<String>> {
    let mut candidates: Vec<Vec<String>> = Vec::new();
    if let Some(p) = std::env::var_os("FISSION_PYTHON") {
        candidates.push(vec![p.to_string_lossy().into_owned()]);
    }
    if cfg!(windows) {
        candidates.push(vec![r"C:\My Cad\FissionCAD\.venv\Scripts\python.exe".into()]);
        candidates.push(vec!["py".into(), "-3".into()]);
    }
    candidates.push(vec!["python".into()]);
    candidates.push(vec!["python3".into()]);
    candidates.into_iter().find(|c| {
        let Some((prog, args)) = c.split_first() else { return false };
        let mut cmd = Command::new(prog);
        cmd.args(args).arg("--version").stdout(Stdio::null()).stderr(Stdio::null());
        hide_window(&mut cmd);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    })
}

#[cfg(windows)]
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}
#[cfg(not(windows))]
fn hide_window(_: &mut Command) {}

/// Fission's `adsk` stand-in: `FISSION_FUSION_SHIM`, else `fusion/` next to the program, else the
/// source tree's (`packaging/fission/fusion`, for development).
pub fn shim_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FISSION_FUSION_SHIM") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let near = exe.parent()?.join("fusion");
    if near.join("fission_bridge.py").is_file() {
        return Some(near);
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/fission/fusion");
    src.join("fission_bridge.py").is_file().then_some(src)
}

fn reply(id: &Value, r: &Result<Value>) -> String {
    match r {
        Ok(v) => json!({"id": id, "ok": true, "result": v}).to_string(),
        Err(e) => json!({"id": id, "ok": false, "error": e.to_string()}).to_string(),
    }
}

/// Run a Fusion script against `s`: every command it asks for runs on `s`, as it asks.
pub fn run_script(s: &mut Session, script: &Path, timeout: Duration) -> std::result::Result<ScriptRun, String> {
    run_script_with(s, script, timeout, &[])
}

/// [`run_script`] with these environment variables set for the script.
fn run_script_with(s: &mut Session, script: &Path, timeout: Duration, env: &[(&str, &Path)]) -> std::result::Result<ScriptRun, String> {
    if !script.is_file() {
        return Err(format!("{} isn't there", script.display()));
    }
    let python = find_python().ok_or("Python wasn't found: install Python 3 (python.org), or set FISSION_PYTHON to python.exe")?;
    let shim = shim_dir().ok_or("Fission's Fusion script support (the fusion folder next to Fission.exe) is missing")?;
    let (prog, args) = python.split_first().ok_or("no Python")?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .arg("-u")
        .arg(shim.join("fission_bridge.py"))
        .arg(script)
        .env("PYTHONPATH", &shim)
        .env("PYTHONIOENCODING", "utf-8")
        .envs(env.iter().map(|(k, v)| (*k, *v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start Python ({e})"))?;
    let (Some(mut stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        child.kill().ok();
        return Err("couldn't talk to Python".into());
    };
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let (etx, erx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
            if all.len() < 1 << 20 {
                all.push_str(&line);
                all.push('\n');
            }
        }
        etx.send(all).ok();
    });
    let mut run = ScriptRun::default();
    let was_sketch = s.active_sketch;
    let end = Instant::now() + timeout;
    let mut failure: Option<String> = None;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            failure = Some(format!("the script was still running after {} seconds, so it was stopped", timeout.as_secs()));
            break;
        }
        let line = match rx.recv_timeout(left) {
            Ok(l) => l,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            // not ours: something the script wrote past the bridge
            run.log.push_str(&line);
            run.log.push('\n');
            continue;
        };
        match msg.get("op").and_then(Value::as_str) {
            Some("message") => run.messages.push(msg.get("text").and_then(Value::as_str).unwrap_or("").chars().take(4000).collect()),
            Some("error") => failure = Some(msg.get("text").and_then(Value::as_str).unwrap_or("the script failed").chars().take(4000).collect()),
            Some("done") => break,
            _ => {
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                let r = match msg.get("cmd").and_then(Value::as_str) {
                    Some(c) if c.starts_with("fusion.") => Err(bad(c, "a script can't run scripts")),
                    Some(c) => {
                        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
                        let r = s.execute(c, &params);
                        if r.is_ok() && !READ_ONLY.contains(&c) {
                            run.journal.push((c.to_string(), params));
                        }
                        r
                    }
                    None => Err(EngineError::Other("a request without a command".into())),
                };
                if writeln!(stdin, "{}", reply(&id, &r)).and_then(|()| stdin.flush()).is_err() {
                    break;
                }
            }
        }
    }
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().ok();
    child.wait().ok();
    run.log.push_str(&erx.recv_timeout(Duration::from_secs(2)).unwrap_or_default());
    // A script that ends inside a sketch (Fission draws into the open one) leaves it finished,
    // as Fusion does.
    if failure.is_none() && was_sketch.is_none() && s.active_sketch.is_some() && s.execute("sketch.finish", &json!({})).is_ok() {
        run.journal.push(("sketch.finish".into(), json!({})));
    }
    match failure {
        // what it printed last helps find why
        Some(f) if !run.log.trim().is_empty() => {
            let lines: Vec<&str> = run.log.lines().collect();
            Err(format!("{f}\n{}", lines[lines.len().saturating_sub(12)..].join("\n")))
        }
        Some(f) => Err(f),
        None => Ok(run),
    }
}

fn run_script_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "fusion.run_script";
    let path = str_(p, "path").ok_or_else(|| bad(cmd, "give `path`, a Fusion 360 script (.py)"))?;
    let timeout = Duration::from_secs_f64(num(p, "timeout").unwrap_or(300.0).clamp(5.0, 3600.0));
    let run = run_script(s, Path::new(path), timeout).map_err(EngineError::Other)?;
    serde_json::to_value(sch::FusionRunResult { commands: run.journal.len(), messages: run.messages, log: run.log, ..Default::default() })
        .map_err(|e| EngineError::Other(e.to_string()))
}

fn ffx_part(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> std::result::Result<Vec<u8>, String> {
    use std::io::Read;
    let f = zip.by_name(name).map_err(|_| format!("the .ffx has no {name}: was it saved by the Fission Export script?"))?;
    if f.size() > MAX_FFX {
        return Err(format!("{name} in the .ffx is too big"));
    }
    let mut out = Vec::new();
    f.take(MAX_FFX).read_to_end(&mut out).map_err(|e| format!("couldn't read {name} ({e})"))?;
    Ok(out)
}

/// A user parameter's name Fission can take: letters, digits and `_`, not starting with a digit.
fn param_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_alphanumeric() || c == '_') && !n.starts_with(|c: char| c.is_ascii_digit())
}

/// Open a Fusion design saved with its timeline by the Fission Export script: rebuilt step by step
/// as a Fission timeline (sketches, features, your parameters) when every step comes out as in
/// Fusion, else Fusion's finished bodies with your parameters.
fn open_ffx(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "fusion.open_ffx";
    let path = str_(p, "path").ok_or_else(|| bad(cmd, "give `path`, a .ffx file"))?;
    let other = |e: String| EngineError::Other(e);
    let meta = std::fs::metadata(path).map_err(|e| other(format!("{path}: {e}")))?;
    if meta.len() > MAX_FFX {
        return Err(bad(cmd, "the .ffx is too big"));
    }
    let file = std::fs::File::open(path).map_err(|e| other(format!("{path}: {e}")))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| other(format!("{path} isn't a .ffx ({e})")))?;
    let timeline = ffx_part(&mut zip, "timeline.json").map_err(other)?;
    let doc: Value = serde_json::from_slice(&timeline).map_err(|e| other(format!("timeline.json: {e}")))?;
    if doc.get("format").and_then(Value::as_str) != Some("fissioncad-fusion-export") {
        return Err(bad(cmd, "this isn't a design saved by the Fission Export script"));
    }
    let design = doc.pointer("/design/name").and_then(Value::as_str).unwrap_or("Fusion design").chars().take(120).collect::<String>();
    let steps = doc.get("timeline").and_then(Value::as_array).map_or(0, |t| t.iter().filter(|r| r.get("type").and_then(Value::as_str) != Some("_snapshot")).count());
    // Unpacked next to nothing else: timeline.json and the STEP files (plain names only).
    let dir = std::env::temp_dir().join(format!("fission-ffx-{}-{}", std::process::id(), s.doc.features.len()));
    std::fs::create_dir_all(&dir).map_err(|e| other(e.to_string()))?;
    let names: Vec<String> = zip.file_names().filter_map(|n| n.ok().map(std::borrow::Cow::into_owned)).collect();
    for name in names.iter().filter(|n| !n.contains(['/', '\\']) && !n.starts_with('.') && n.len() < 200).take(500) {
        if let Ok(bytes) = ffx_part(&mut zip, name) {
            std::fs::write(dir.join(name), bytes).map_err(|e| other(e.to_string()))?;
        }
    }
    let result = open_ffx_in(s, &dir, &doc, &design, steps);
    std::fs::remove_dir_all(&dir).ok();
    result
}

fn open_ffx_in(s: &mut Session, dir: &Path, doc: &Value, design: &str, steps: usize) -> Result<Value> {
    let other = |e: String| EngineError::Other(e);
    let before: Vec<String> = s.doc.params.iter().map(|q| q.name.clone()).collect();
    // 1) the timeline, step by step
    let mark = s.checkpoint();
    let why = match shim_dir().map(|d| d.join("ffx_replay.py")) {
        Some(replay) if replay.is_file() => match run_script_with(s, &replay, Duration::from_secs(1800), &[("FISSION_FFX", dir)]) {
            Ok(run) => {
                s.drop_checkpoint(mark);
                let bodies = body_names(s);
                let parameters = s.doc.params.iter().map(|q| q.name.clone()).filter(|n| !before.contains(n)).collect();
                return serde_json::to_value(sch::FfxOpenResult { design: design.to_string(), bodies, parameters, timeline: steps, rebuilt: true, messages: run.messages, ..Default::default() })
                    .map_err(|e| other(e.to_string()));
            }
            Err(e) => e,
        },
        _ => "Fission's Fusion support (the fusion folder next to Fission.exe) is missing".to_string(),
    };
    s.restore_checkpoint(mark).ok();
    s.drop_checkpoint(mark);
    // 2) Fusion's finished bodies, and your parameters
    let mut parameters = Vec::new();
    for prm in doc.get("params").and_then(Value::as_array).into_iter().flatten().take(2000) {
        if prm.get("user").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let (Some(name), Some(expr)) = (prm.get("name").and_then(Value::as_str), prm.get("expr").and_then(Value::as_str)) else { continue };
        if !param_name(name) || s.doc.params.iter().any(|q| q.name == name) {
            continue;
        }
        let mut add = json!({"name": name, "expression": expr});
        if let Some(c) = prm.get("comment").and_then(Value::as_str).filter(|c| !c.is_empty()) {
            add["comment"] = json!(c.chars().take(500).collect::<String>());
        }
        if s.execute("parameters.add", &add).is_ok() {
            parameters.push(name.to_string());
        }
    }
    let step_path = dir.join("final.step");
    if !step_path.is_file() {
        return Err(other("the .ffx has no final.step: was it saved by the Fission Export script?".into()));
    }
    let r = s.execute("file.insert_step", &json!({"path": step_path.to_string_lossy(), "name": design}))?;
    let bodies = r.get("bodies").and_then(Value::as_array).map(|b| b.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let messages = vec![format!("The timeline couldn't be rebuilt step by step ({why}), so Fusion's finished bodies came in as they are.")];
    serde_json::to_value(sch::FfxOpenResult { design: design.to_string(), bodies, parameters, timeline: steps, rebuilt: false, messages, ..Default::default() })
        .map_err(|e| other(e.to_string()))
}

/// The design's bodies' names.
fn body_names(s: &mut Session) -> Vec<String> {
    s.execute("inspect.measure", &json!({}))
        .ok()
        .and_then(|r| r.get("bodies").and_then(Value::as_array).map(|b| b.iter().filter_map(|x| x.get("name").and_then(Value::as_str)).map(str::to_string).collect()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_ffx_and_script_paths_are_refused_plainly() {
        let mut s = Session::new();
        let dir = std::env::temp_dir().join(format!("fission-ffx-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let junk = dir.join("junk.ffx");
        std::fs::write(&junk, b"not a zip").ok();
        for p in [json!({}), json!({"path": 3}), json!({"path": "/no/such.ffx"}), json!({"path": junk.to_string_lossy()})] {
            assert!(open_ffx(&mut s, &p).is_err(), "{p}");
        }
        // a zip that isn't one of Fission Export's
        let other = dir.join("other.ffx");
        if let Ok(f) = std::fs::File::create(&other) {
            let mut z = zip::ZipWriter::new(f);
            if z.start_file("timeline.json", zip::write::SimpleFileOptions::default()).is_ok() {
                std::io::Write::write_all(&mut z, br#"{"format": "something else"}"#).ok();
            }
            z.finish().ok();
        }
        let e = open_ffx(&mut s, &json!({"path": other.to_string_lossy()})).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains("Fission Export"), "{e}");
        assert!(run_script(&mut s, Path::new("/no/such/script.py"), Duration::from_secs(5)).is_err());
        assert!(s.doc.features.is_empty());
        assert!(param_name("width") && param_name("_d2") && !param_name("2x") && !param_name("a b") && !param_name(""));
        std::fs::remove_dir_all(&dir).ok();
    }
}

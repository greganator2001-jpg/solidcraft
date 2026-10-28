//! Slicing with Bambu Studio's own slicer (its command line), so the result is exactly what Bambu Studio
//! makes: same presets, same G-code. The complete presets are written next to the model first, because the
//! command line only takes complete ones.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::Result;
use crate::model3mf::{self, Part};
use crate::profiles::Library;

/// A filament slot: its Bambu Studio preset, colour and any changed settings.
#[derive(Clone, Debug, Default)]
pub struct Filament {
    pub preset: String,
    /// '#rrggbb'
    pub color: String,
    pub overrides: Map<String, Value>,
}

/// What to slice with.
#[derive(Clone, Debug, Default)]
pub struct SliceJob {
    pub printer: String,
    pub process: String,
    /// Slot 1 is `filaments[0]`.
    pub filaments: Vec<Filament>,
    /// Process settings to change, e.g. {"sparse_infill_density": "20%"}.
    pub overrides: Map<String, Value>,
    /// "Textured PEI Plate", "Cool Plate" … ("" = the printer's default).
    pub bed_type: String,
    /// Let Bambu Studio arrange / auto-orient the parts.
    pub arrange: bool,
    pub orient: bool,
}

/// One filament the sliced plate uses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsedFilament {
    pub slot: u32,
    pub material: String,
    pub color: String,
    pub grams: f64,
    pub meters: f64,
}

/// A sliced plate.
#[derive(Clone, Debug, Default)]
pub struct SliceResult {
    /// The sliced `.gcode.3mf` (what goes to the printer).
    pub file: PathBuf,
    pub seconds: f64,
    pub grams: f64,
    pub filaments: Vec<UsedFilament>,
    /// e.g. it had to slice with safer settings.
    pub note: String,
}

impl SliceResult {
    pub fn time_text(&self) -> String {
        let s = self.seconds.max(0.0).round() as u64;
        let (h, m) = (s / 3600, (s % 3600) / 60);
        if h > 0 { format!("{h}h {m:02}m") } else { format!("{m}m {:02}s", s % 60) }
    }
}

/// Settings that make the slicer's crashes less likely (used for the second try after a crash).
const SAFE: [(&str, &str); 5] = [
    ("enable_arc_fitting", "0"),
    ("enable_wrapping_detection", "0"),
    ("wall_generator", "classic"),
    ("timelapse_type", "0"),
    ("enable_prime_tower", "0"),
];

/// Bambu Studio's command-line exit codes.
fn code_text(code: i64) -> &'static str {
    match code {
        -1 => "Bambu Studio couldn't set itself up to slice",
        -2 => "Bambu Studio didn't accept the slicing options",
        -3 => "Bambu Studio couldn't find the model or preset files",
        -5 => "Bambu Studio couldn't read a preset",
        -6 => "Bambu Studio couldn't read the model",
        -14 => "Bambu Studio ran out of memory",
        -17 => "the process preset doesn't fit this printer",
        -50 => "the plate is empty, or no part is fully on it",
        -51 => "some slicing settings aren't valid",
        -52 => "some parts hang over the edge of the plate",
        -61 => "a filament can't be printed on this plate type",
        -62 => "the filaments' temperatures are too far apart",
        -100 => "slicing failed",
        -104 => "the print goes outside the printable area",
        _ => "",
    }
}

/// Exit codes come as unsigned 32-bit on Windows.
fn signed(code: i64) -> i64 {
    if ((1 << 31)..(1 << 32)).contains(&code) { code - (1 << 32) } else { code }
}

/// A crash (access violation …) rather than one of Bambu Studio's own error codes.
fn crashed(code: Option<i64>) -> bool {
    code.map(signed).is_some_and(|c| c <= -1000 || matches!(c, 3 | 134 | 139 | -11 | -6))
}

/// The command line for one slice.
pub fn command(exe: &Path, settings: &[PathBuf], filaments: &[PathBuf], model: &Path, out_name: &str, job: &SliceJob, plain: bool) -> Vec<String> {
    let folder = model.parent().map(Path::to_path_buf).unwrap_or_default();
    let join = |v: &[PathBuf]| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(";");
    let mut cmd = vec![
        exe.display().to_string(),
        "--slice".into(),
        if plain { "0".into() } else { "1".into() },
        "--outputdir".into(),
        folder.display().to_string(),
        "--load-settings".into(),
        join(settings),
        "--load-filaments".into(),
        join(filaments),
        "--export-3mf".into(),
        out_name.into(),
    ];
    if !plain {
        cmd.extend(["--arrange".into(), if job.arrange { "1".into() } else { "0".into() }, "--ensure-on-bed".into()]);
        if job.orient {
            cmd.extend(["--orient".into(), "1".into()]);
        }
        if !job.bed_type.is_empty() {
            // the command line spells settings with dashes
            cmd.push(format!("--curr-bed-type={}", job.bed_type));
        }
    }
    cmd.push(model.display().to_string());
    cmd
}

/// Run the program, waiting at most `timeout`. Returns (exit code, output tail).
fn run(cmd: &[String], cwd: &Path, timeout: Duration) -> Result<(Option<i64>, String)> {
    let (prog, args) = cmd.split_first().ok_or("nothing to run")?;
    let mut c = Command::new(prog);
    c.args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    let mut child = c.spawn().map_err(|e| format!("couldn't start Bambu Studio ({e})"))?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let mut buf = Vec::new();
        if let Some(o) = out.as_mut() {
            o.read_to_end(&mut buf).ok();
        }
        if let Some(e) = err.as_mut() {
            e.read_to_end(&mut buf).ok();
        }
        s.push_str(&String::from_utf8_lossy(&buf));
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() > timeout => {
                child.kill().ok();
                child.wait().ok();
                return Err(format!("Bambu Studio took longer than {} minutes to slice", timeout.as_secs() / 60));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("lost track of Bambu Studio ({e})")),
        }
    };
    let log = reader.join().unwrap_or_default();
    let tail: String = log.chars().rev().take(12_000).collect::<Vec<_>>().into_iter().rev().collect();
    Ok((status.and_then(|s| s.code()).map(i64::from), tail))
}

fn last_error_line(log: &str) -> String {
    log.lines()
        .map(str::trim)
        .rev()
        .find(|l| ["error", "invalid", "unknown", "can not", "cannot", "failed"].iter().any(|w| l.to_lowercase().contains(w)))
        .map(|l| l.chars().take(300).collect())
        .unwrap_or_default()
}

fn log_line(text: &str) {
    // ~/Fission/bambu_slice.log: what was run and what Bambu Studio said (for bug reports)
    if let Some(dir) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let p = PathBuf::from(dir).join("Fission");
        if std::fs::create_dir_all(&p).is_ok()
            && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p.join("bambu_slice.log"))
        {
            use std::io::Write;
            writeln!(f, "{}", text.trim_end()).ok();
        }
    }
}

/// One slice attempt. Ok(Ok(result)) sliced; Ok(Err((message, code))) Bambu Studio said no or crashed.
fn once(
    exe: &Path,
    lib: &Library,
    job: &SliceJob,
    model: &Path,
    out_name: &str,
    extra: &Map<String, Value>,
    plain: bool,
    timeout: Duration,
) -> Result<std::result::Result<SliceResult, (String, Option<i64>)>> {
    let folder = model.parent().ok_or("the model has no folder")?.to_path_buf();
    for old in ["result.json", out_name] {
        std::fs::remove_file(folder.join(old)).ok();
    }
    let machine = folder.join("machine.json");
    lib.write_flat(&job.printer, &machine, &Map::new())?;
    let process = folder.join("process.json");
    let mut po = job.overrides.clone();
    po.extend(extra.clone());
    lib.write_flat(&job.process, &process, &po)?;
    if job.filaments.is_empty() {
        return Err("pick at least one filament".into());
    }
    let mut fils = Vec::new();
    for (i, f) in job.filaments.iter().enumerate() {
        let p = folder.join(format!("filament{}.json", i + 1));
        let mut o = f.overrides.clone();
        let col = if f.color.is_empty() { "#FFFFFF".to_string() } else { f.color.to_uppercase() };
        o.insert("filament_colour".into(), Value::from(vec![col]));
        lib.write_flat(&f.preset, &p, &o)?;
        fils.push(p);
    }
    let cmd = command(exe, &[machine, process], &fils, model, out_name, job, plain);
    log_line(&format!("\n=== {}", cmd.join(" ")));
    let (code, log) = run(&cmd, &folder, timeout)?;
    log_line(&format!("exit {:?}\n{log}", code.map(signed)));
    match read_result(&folder, out_name) {
        Ok(Some(r)) => Ok(Ok(r)),
        Ok(None) => {
            let c = code.map(signed);
            let mut msg = if crashed(code) {
                "Bambu Studio's slicer crashed".to_string()
            } else {
                let t = c.map(code_text).unwrap_or("");
                if t.is_empty() { format!("Bambu Studio's slicer stopped (code {})", c.unwrap_or(0)) } else { t.to_string() }
            };
            let detail = last_error_line(&log);
            if !detail.is_empty() {
                msg = format!("{msg} — {detail}");
            }
            Ok(Err((msg, code)))
        }
        Err(e) => Ok(Err((e, code))),
    }
}

/// Slice these parts (already placed on the plate) with Bambu Studio. Works in `folder` (made if needed).
/// If the slicer crashes it tries again with the plainest command and a few risky options off; if that
/// crashes too, a test cube tells whether it's this model or the setup.
pub fn slice(exe: &Path, lib: &Library, job: &SliceJob, parts: &[Part], folder: &Path, timeout: Duration) -> Result<SliceResult> {
    std::fs::create_dir_all(folder).map_err(|e| format!("couldn't make {} ({e})", folder.display()))?;
    let model = folder.join("model.3mf");
    std::fs::write(&model, model3mf::write(parts)?).map_err(|e| format!("couldn't write the model ({e})"))?;
    let out = "plate.gcode.3mf";
    let first = once(exe, lib, job, &model, out, &Map::new(), false, timeout)?;
    let (msg, code) = match first {
        Ok(r) => return Ok(r),
        Err(e) => e,
    };
    if !crashed(code) {
        return Err(msg);
    }
    let safe: Map<String, Value> = SAFE.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect();
    match once(exe, lib, job, &model, out, &safe, true, timeout)? {
        Ok(mut r) => {
            r.note = "Bambu Studio's slicer crashed with the full settings, so this was sliced with arc fitting, wrapping detection and the prime tower off.".into();
            Ok(r)
        }
        Err(_) => {
            let cube = folder.join("cube_test.3mf");
            let cube_ok = model3mf::write(&[model3mf::test_cube(lib.bed(&job.printer))])
                .and_then(|b| std::fs::write(&cube, b).map_err(|e| e.to_string()))
                .and_then(|_| once(exe, lib, job, &cube, "cube.gcode.3mf", &Map::new(), true, Duration::from_secs(300)))
                .map(|r| r.is_ok())
                .unwrap_or(false);
            Err(if cube_ok {
                "Bambu Studio's slicer crashes on this model (a test cube slices fine with the same settings). Try Send with Bambu Studio.".into()
            } else {
                "Bambu Studio's slicer crashes even on a simple test cube with these presets. Try Send with Bambu Studio.".into()
            })
        }
    }
}

/// The result of a slice in `folder`: Ok(None) when there's none (and no reason given).
pub fn read_result(folder: &Path, out_name: &str) -> Result<Option<SliceResult>> {
    let rj = folder.join("result.json");
    if let Ok(text) = std::fs::read_to_string(&rj)
        && let Ok(j) = serde_json::from_str::<Value>(&text)
    {
        let code = j.get("return_code").and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            let e = j.get("error_string").and_then(Value::as_str).unwrap_or("").trim().to_string();
            return Err(if e.is_empty() { format!("Bambu Studio's slicer failed (code {code})") } else { e });
        }
    }
    let out = folder.join(out_name);
    if !out.is_file() {
        return Ok(None);
    }
    read_sliced(&out, 1).map(Some)
}

fn zip_text(z: &mut zip::ZipArchive<std::fs::File>, name: &str, cap: u64) -> Option<String> {
    let f = z.by_name(name).ok()?;
    let mut s = String::new();
    f.take(cap).read_to_string(&mut s).ok()?;
    Some(s)
}

/// Print time, weight and filaments from a sliced `.gcode.3mf` (its `Metadata/slice_info.config`).
pub fn read_sliced(path: &Path, plate: u32) -> Result<SliceResult> {
    let f = std::fs::File::open(path).map_err(|e| format!("couldn't open the sliced file ({e})"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("couldn't read the sliced file ({e})"))?;
    if z.by_name(&format!("Metadata/plate_{plate}.gcode")).is_err() {
        return Err("the sliced file has no G-code in it".into());
    }
    let mut res = SliceResult { file: path.to_path_buf(), ..Default::default() };
    let Some(info) = zip_text(&mut z, "Metadata/slice_info.config", 8 << 20) else { return Ok(res) };
    parse_slice_info(&info, plate, &mut res);
    Ok(res)
}

/// Read `<plate><metadata key=… value=…/><filament id=… used_g=…/></plate>`.
pub fn parse_slice_info(xml: &str, plate: u32, res: &mut SliceResult) {
    use quick_xml::events::Event;
    let mut r = quick_xml::Reader::from_str(xml);
    let mut in_plate = false;
    let mut meta: Vec<(String, String)> = Vec::new();
    let mut fils: Vec<UsedFilament> = Vec::new();
    let attr = |e: &quick_xml::events::BytesStart, k: &[u8]| -> String {
        e.attributes()
            .filter_map(|a| a.ok())
            .find(|a| a.key.as_ref() == k)
            .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.to_string()))
            .unwrap_or_default()
    };
    let mut done = false;
    for _ in 0..1_000_000 {
        match r.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"plate" => {
                in_plate = true;
                meta.clear();
                fils.clear();
            }
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if in_plate => match e.name().as_ref() {
                b"metadata" => meta.push((attr(&e, b"key"), attr(&e, b"value"))),
                b"filament" => fils.push(UsedFilament {
                    slot: attr(&e, b"id").parse().unwrap_or(1),
                    material: attr(&e, b"type"),
                    color: attr(&e, b"color"),
                    grams: attr(&e, b"used_g").parse().unwrap_or(0.0),
                    meters: attr(&e, b"used_m").parse().unwrap_or(0.0),
                }),
                _ => {}
            },
            Ok(Event::End(e)) if e.name().as_ref() == b"plate" => {
                in_plate = false;
                let get = |k: &str| meta.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone()).unwrap_or_default();
                let idx = get("index");
                if idx.is_empty() || idx == plate.to_string() {
                    res.seconds = get("prediction").parse().unwrap_or(0.0);
                    res.grams = get("weight").parse().unwrap_or(0.0);
                    res.filaments = fils.clone();
                    done = true;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        if done {
            break;
        }
    }
}

/// The 0-based filament slots the plate uses (`Metadata/plate_N.json` `filament_ids`), else from the slice info.
pub fn used_slots(path: &Path, plate: u32) -> Vec<u32> {
    let from_json = std::fs::File::open(path)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
        .and_then(|mut z| zip_text(&mut z, &format!("Metadata/plate_{plate}.json"), 1 << 20))
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|j| j.get("filament_ids").and_then(Value::as_array).cloned())
        .map(|a| a.iter().filter_map(Value::as_u64).filter_map(|v| u32::try_from(v).ok()).collect::<Vec<_>>())
        .filter(|v| !v.is_empty());
    from_json.unwrap_or_else(|| read_sliced(path, plate).map(|r| r.filaments.iter().map(|f| f.slot.saturating_sub(1)).collect()).unwrap_or_default())
}

/// Open a file in the Bambu Studio app (not waiting for it).
pub fn open_in_studio(exe: &Path, file: &Path) -> Result<()> {
    let mut c = if cfg!(target_os = "macos") && exe.to_string_lossy().ends_with("/MacOS/BambuStudio") {
        let app = exe.parent().and_then(Path::parent).and_then(Path::parent).ok_or("can't find the Bambu Studio app")?;
        let mut c = Command::new("open");
        c.arg("-a").arg(app).arg(file);
        c
    } else {
        let mut c = Command::new(exe);
        c.arg(file);
        c
    };
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0000_0008); // detached
    }
    c.spawn().map(|_| ()).map_err(|e| format!("couldn't open Bambu Studio ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_slice_info_and_exit_codes() {
        let xml = r##"<?xml version="1.0"?><config><plate><metadata key="index" value="1"/><metadata key="prediction" value="3720"/><metadata key="weight" value="7.50"/><filament id="1" type="PLA" color="#FFFFFF" used_m="1.5" used_g="2.5"/><filament id="2" type="PLA" color="#C02A20" used_m="1.5" used_g="5.0"/></plate></config>"##;
        let mut r = SliceResult::default();
        parse_slice_info(xml, 1, &mut r);
        assert_eq!((r.seconds, r.grams, r.filaments.len()), (3720.0, 7.5, 2));
        assert_eq!(r.filaments[1], UsedFilament { slot: 2, material: "PLA".into(), color: "#C02A20".into(), grams: 5.0, meters: 1.5 });
        assert_eq!(r.time_text(), "1h 02m");
        assert_eq!(signed(4294967294), -2);
        assert!(crashed(Some(3221225477))); // access violation
        assert!(!crashed(Some(4294967294)));
        assert_eq!(code_text(signed(4294967294)), "Bambu Studio didn't accept the slicing options");
        // hostile slice info
        let mut r2 = SliceResult::default();
        parse_slice_info("<plate><metadata key=\"prediction\" value=\"x\"/><filament id=\"q\"", 1, &mut r2);
        assert_eq!(r2.seconds, 0.0);
    }

    #[test]
    fn command_line_like_bambu_studio_wants() {
        let job = SliceJob { bed_type: "Textured PEI Plate".into(), ..Default::default() };
        let cmd = command(
            Path::new("/x/bambu-studio"),
            &[PathBuf::from("/t/m.json"), PathBuf::from("/t/p.json")],
            &[PathBuf::from("/t/f1.json")],
            Path::new("/t/model.3mf"),
            "plate.gcode.3mf",
            &job,
            false,
        );
        assert_eq!(cmd[1..3], ["--slice".to_string(), "1".into()]);
        assert!(cmd.contains(&"/t/m.json;/t/p.json".to_string()));
        assert!(cmd.contains(&"--curr-bed-type=Textured PEI Plate".to_string()));
        assert_eq!(cmd.last().map(String::as_str), Some("/t/model.3mf"));
        let plain = command(Path::new("/x/b"), &[], &[], Path::new("/t/model.3mf"), "o", &job, true);
        assert!(!plain.iter().any(|a| a.starts_with("--curr-bed-type") || a == "--arrange"));
    }
}

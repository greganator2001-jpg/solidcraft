//! Bambu Studio's printer / process / filament presets, read from its install and its settings folder.
//!
//! Bambu Studio keeps each preset as a small JSON file that `inherits` a parent and `include`s templates.
//! Its command-line slicer only takes complete settings, so [`Library::flatten`] walks the chain and merges
//! it, the way Bambu Studio does when it loads a preset.
//!
//! Where the presets live (later ones win when two have the same name):
//! - `<install>/resources/profiles/BBL/{machine,process,filament}` — the ones that came with the install
//! - `<settings>/system/BBL/…` — the ones Bambu Studio updated online
//! - `<settings>/user/<id>/{machine,process,filament}` — your own (including `filament/base/…`)
//!
//! where `<settings>` is `%APPDATA%\BambuStudio` on Windows.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::{Result, pattern, studio};

pub const KINDS: [&str; 3] = ["machine", "process", "filament"];
/// Keys a template's `include` doesn't pass on.
const META: [&str; 8] = ["name", "type", "from", "instantiation", "inherits", "setting_id", "include", "description"];
/// Biggest preset file read.
const MAX_PRESET_BYTES: u64 = 4 << 20;
/// Most preset files read from one folder tree.
const MAX_FILES: usize = 40_000;

// ------------------------------------------------------------------ finding Bambu Studio
/// Where Bambu Studio's program usually is on this system.
pub fn studio_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        for env in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(env) {
                let base = PathBuf::from(base);
                out.push(base.join("Bambu Studio").join("bambu-studio.exe"));
                out.push(base.join("BambuStudio").join("bambu-studio.exe"));
                out.push(base.join("Programs").join("Bambu Studio").join("bambu-studio.exe"));
            }
        }
    } else if cfg!(target_os = "macos") {
        out.push(PathBuf::from("/Applications/BambuStudio.app/Contents/MacOS/BambuStudio"));
    } else {
        out.push(PathBuf::from("/usr/bin/bambu-studio"));
        if let Some(home) = std::env::var_os("HOME") {
            out.push(PathBuf::from(home).join("Applications").join("BambuStudio.AppImage"));
        }
    }
    out
}

/// Bambu Studio's program: `chosen` when it exists, else `FISSION_BAMBU_STUDIO`, else the usual install.
pub fn find_studio(chosen: Option<&Path>) -> Option<PathBuf> {
    let env = std::env::var_os("FISSION_BAMBU_STUDIO").map(PathBuf::from);
    chosen.map(Path::to_path_buf).into_iter().chain(env).chain(studio_candidates()).find(|p| p.is_file())
}

/// The install's resources folder (the one holding `profiles/BBL`).
pub fn resources_of(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let mut cands = vec![dir.join("resources")];
    if let Some(up) = dir.parent() {
        cands.push(up.join("Resources"));
        cands.push(up.join("resources"));
    }
    cands.into_iter().find(|c| c.join("profiles").join("BBL").is_dir())
}

/// Bambu Studio's settings folder (`%APPDATA%\BambuStudio`), when it exists.
pub fn settings_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FISSION_BAMBU_SETTINGS") {
        return Some(PathBuf::from(p)).filter(|p| p.is_dir());
    }
    let p = if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("BambuStudio"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/BambuStudio"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/BambuStudio"))
    };
    p.filter(|p| p.is_dir())
}

/// Where a preset came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    System,
    User,
}

/// Folders to read presets from, oldest first (later ones win): (folder, origin).
pub fn profile_roots(exe: Option<&Path>) -> Vec<(PathBuf, Origin)> {
    let mut roots = Vec::new();
    if let Some(res) = exe.and_then(resources_of) {
        roots.push((res.join("profiles").join("BBL"), Origin::System));
    }
    if let Some(p) = std::env::var_os("FISSION_BAMBU_PROFILES") {
        roots.push((PathBuf::from(p), Origin::System));
    }
    if let Some(sd) = settings_dir() {
        roots.push((sd.join("system").join("BBL"), Origin::System));
        if let Ok(rd) = std::fs::read_dir(sd.join("user")) {
            let mut users: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
            users.sort();
            roots.extend(users.into_iter().map(|u| (u, Origin::User)));
        }
    }
    roots.into_iter().filter(|(p, _)| p.is_dir()).collect()
}

// ------------------------------------------------------------------ the preset library
#[derive(Clone, Debug)]
struct Preset {
    kind: &'static str,
    origin: Origin,
    json: Map<String, Value>,
}

/// Every preset by name.
#[derive(Debug, Default)]
pub struct Library {
    items: HashMap<String, Preset>,
    /// printer model name ("Bambu Lab A1") -> its `machine_model` JSON
    models: HashMap<String, Map<String, Value>>,
    /// Bambu Studio's shown system filaments (its settings' `filaments` list), if any
    visible: Option<BTreeSet<String>>,
}

impl Library {
    /// Read every preset under these roots (oldest first).
    pub fn load(roots: &[(PathBuf, Origin)]) -> Self {
        let mut lib = Library::default();
        for (root, origin) in roots {
            for kind in KINDS {
                let mut files = Vec::new();
                collect_json(&root.join(kind), &mut files, 0);
                files.sort();
                for f in files {
                    let Some(j) = read_json(&f) else { continue };
                    let Some(name) = j.get("name").and_then(Value::as_str).map(str::to_string) else { continue };
                    if j.get("type").and_then(Value::as_str) == Some("machine_model") {
                        lib.models.insert(name, j);
                        continue;
                    }
                    lib.items.insert(name, Preset { kind, origin: *origin, json: j });
                }
            }
        }
        lib.visible = studio::read_conf(None)
            .get("filaments")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<BTreeSet<_>>())
            .filter(|s| !s.is_empty());
        lib
    }

    /// The library of this Bambu Studio install (and its settings folder).
    pub fn for_install(exe: Option<&Path>) -> Self {
        Self::load(&profile_roots(exe))
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Replace the list of system filaments Bambu Studio shows (tests; `None` shows them all).
    pub fn set_visible(&mut self, v: Option<BTreeSet<String>>) {
        self.visible = v;
    }

    /// The complete settings of a preset: its parents and templates merged in, marked as a stand-alone
    /// system preset so the command line takes it as is.
    pub fn flatten(&self, name: &str) -> Result<Map<String, Value>> {
        let mut seen = Vec::new();
        let mut flat = self.walk(name, &mut seen)?;
        flat.insert("name".into(), Value::from(name));
        flat.insert("from".into(), Value::from("system"));
        flat.insert("inherits".into(), Value::from(""));
        flat.insert("instantiation".into(), Value::from("true"));
        if !flat.contains_key("type")
            && let Some(p) = self.items.get(name)
        {
            flat.insert("type".into(), Value::from(p.kind));
        }
        Ok(flat)
    }

    fn walk(&self, name: &str, seen: &mut Vec<String>) -> Result<Map<String, Value>> {
        if seen.iter().any(|s| s == name) || seen.len() > 64 {
            return Err(format!("Bambu Studio preset '{name}' inherits itself"));
        }
        seen.push(name.to_string());
        let p = self.items.get(name).ok_or_else(|| format!("Bambu Studio preset '{name}' wasn't found"))?;
        let mut out = match p.json.get("inherits").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(parent) => self.walk(parent, seen)?,
            None => Map::new(),
        };
        if let Some(inc) = p.json.get("include").and_then(Value::as_array) {
            for t in inc.iter().filter_map(Value::as_str) {
                if let Some(tp) = self.items.get(t) {
                    for (k, v) in &tp.json {
                        if !META.contains(&k.as_str()) {
                            out.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
        for (k, v) in &p.json {
            if k != "include" {
                out.insert(k.clone(), v.clone());
            }
        }
        Ok(out)
    }

    fn usable(&self, name: &str) -> bool {
        self.items.get(name).is_some_and(|p| p.json.get("instantiation").map(value_text).map(|s| s.to_lowercase()) != Some("false".into()))
    }

    fn of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a String> + 'a {
        self.items.iter().filter(move |(n, p)| p.kind == kind && self.usable(n)).map(|(n, _)| n)
    }

    /// Printer presets you can pick ("Bambu Lab A1 0.4 nozzle"), yours included.
    pub fn printers(&self) -> Vec<String> {
        sorted(self.of_kind("machine").cloned())
    }

    /// The preset and the ones it inherits from: ["Bambu Lab A1 0.4 nozzle - Copy", "Bambu Lab A1 0.4 nozzle", …].
    pub fn lineage(&self, name: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut n = name.to_string();
        while !n.is_empty() && !out.contains(&n) && out.len() < 64 {
            out.push(n.clone());
            n = self.items.get(&n).and_then(|p| p.json.get("inherits")).and_then(Value::as_str).unwrap_or("").to_string();
        }
        out
    }

    /// Can this process or filament preset be used on this printer preset?
    pub fn compatible(&self, name: &str, printer: &str) -> bool {
        let Ok(flat) = self.flatten(name) else { return false };
        let cp = list_of(flat.get("compatible_printers"));
        if !cp.is_empty() {
            // a printer you copied in Bambu Studio ("… - Copy") takes the presets of the one it came from
            return self.lineage(printer).iter().any(|p| cp.contains(p));
        }
        let cond = flat.get("compatible_printers_condition").map(value_text).unwrap_or_default();
        if cond.trim().is_empty() {
            return true;
        }
        let model = self.flatten(printer).ok().and_then(|f| f.get("printer_model").map(value_text)).unwrap_or_default();
        // honour the printer_model=~/…/ parts; anything we can't read doesn't rule the preset out
        let mut rest = cond.as_str();
        while let Some(at) = rest.find("printer_model") {
            rest = &rest[at + "printer_model".len()..];
            let r = rest.trim_start();
            let negate = r.starts_with("!~");
            if !(r.starts_with("=~") || negate) {
                continue;
            }
            let r = r[2..].trim_start();
            let Some(body) = r.strip_prefix('/') else { continue };
            let Some(end) = body.find('/') else { continue };
            if let Some(hit) = pattern::search(&body[..end], &model)
                && hit == negate
            {
                return false;
            }
            rest = &body[end..];
        }
        true
    }

    pub fn is_user(&self, name: &str) -> bool {
        self.items.get(name).is_some_and(|p| p.origin == Origin::User)
    }

    /// Process presets for this printer, sorted.
    pub fn processes(&self, printer: &str) -> Vec<String> {
        sorted(self.of_kind("process").filter(|n| self.compatible(n, printer)).cloned())
    }

    /// Filament presets for this printer: your own first, then Bambu's system ones. With `studio_list`,
    /// the system ones are the filaments you show in Bambu Studio (all of them when it has no list).
    pub fn filaments(&self, printer: &str, studio_list: bool) -> Vec<String> {
        let ok: Vec<&String> = self.of_kind("filament").filter(|n| self.compatible(n, printer)).collect();
        let mut user: Vec<String> = ok.iter().filter(|n| self.is_user(n)).map(|n| n.to_string()).collect();
        let mut system: Vec<String> = ok.iter().filter(|n| !self.is_user(n)).map(|n| n.to_string()).collect();
        user.sort_by_key(|s| s.to_lowercase());
        system.sort_by_key(|s| s.to_lowercase());
        if studio_list && let Some(vis) = &self.visible {
            let kept: Vec<String> = system.iter().filter(|n| vis.contains(*n)).cloned().collect();
            if !kept.is_empty() {
                system = kept;
            }
        }
        user.extend(system);
        user
    }

    /// The preset's filament ID ('GFA00', or 'P…' for your own) — what an AMS tray reports.
    pub fn filament_id(&self, name: &str) -> String {
        self.flatten(name).ok().and_then(|f| f.get("filament_id").map(first_text)).unwrap_or_default()
    }

    /// One setting of a preset as text (a per-extruder list's first entry), "" when it has none.
    pub fn setting(&self, name: &str, key: &str) -> String {
        self.flatten(name).ok().and_then(|f| f.get(key).map(first_text)).unwrap_or_default()
    }

    /// The material ("PLA", "PETG" …), upper case.
    pub fn filament_type(&self, name: &str) -> String {
        self.flatten(name).ok().and_then(|f| f.get("filament_type").map(first_text)).unwrap_or_default().to_uppercase()
    }

    /// The filament preset with this filament ID that fits this printer — yours first.
    pub fn filament_for_id(&self, fid: &str, printer: &str) -> Option<String> {
        if fid.is_empty() {
            return None;
        }
        self.filaments(printer, false).into_iter().find(|n| self.filament_id(n) == fid)
    }

    /// The printer's default process (its `default_print_profile`), else a 0.20mm Standard one.
    pub fn default_process(&self, printer: &str) -> Option<String> {
        let procs = self.processes(printer);
        let d = self.flatten(printer).ok().and_then(|f| f.get("default_print_profile").map(value_text));
        if let Some(d) = d.filter(|d| procs.contains(d)) {
            return Some(d);
        }
        procs.iter().find(|p| p.starts_with("0.20mm Standard")).or(procs.first()).cloned()
    }

    /// A sensible filament preset of this material on this printer.
    pub fn default_filament(&self, printer: &str, material: &str) -> Option<String> {
        let fil = self.filaments(printer, true);
        let want = if material.is_empty() { "PLA".to_string() } else { material.to_uppercase() };
        let dflt = self.flatten(printer).ok().map(|f| list_of(f.get("default_filament_profile"))).unwrap_or_default();
        if let Some(d) = dflt.iter().find(|d| fil.contains(d) && self.filament_type(d) == want) {
            return Some(d.clone());
        }
        for pref in [format!("Bambu {want} Basic"), format!("Bambu {want}"), format!("Generic {want}")] {
            if let Some(h) = fil.iter().find(|f| f.starts_with(&format!("{pref} "))) {
                return Some(h.clone());
            }
        }
        fil.iter().find(|f| self.filament_type(f) == want).or_else(|| dflt.iter().find(|d| fil.contains(d))).or(fil.first()).cloned()
    }

    /// (x0, y0, x1, y1) of the printer's printable area in mm.
    pub fn bed(&self, printer: &str) -> (f64, f64, f64, f64) {
        let area = self.flatten(printer).ok().map(|f| list_of(f.get("printable_area"))).unwrap_or_default();
        let pts: Vec<(f64, f64)> = area
            .iter()
            .filter_map(|a| {
                let (x, y) = a.split_once('x')?;
                Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
            })
            .collect();
        if pts.is_empty() {
            return (0.0, 0.0, 256.0, 256.0);
        }
        let fold = |f: fn(f64, f64) -> f64, init: f64, g: fn(&(f64, f64)) -> f64| pts.iter().map(g).fold(init, f);
        (
            fold(f64::min, f64::INFINITY, |p| p.0),
            fold(f64::min, f64::INFINITY, |p| p.1),
            fold(f64::max, f64::NEG_INFINITY, |p| p.0),
            fold(f64::max, f64::NEG_INFINITY, |p| p.1),
        )
    }

    /// The printable height in mm.
    pub fn height(&self, printer: &str) -> f64 {
        self.flatten(printer).ok().and_then(|f| f.get("printable_height").map(value_text)).and_then(|s| s.parse().ok()).unwrap_or(250.0)
    }

    /// The printer model ("Bambu Lab A1").
    pub fn model_of(&self, printer: &str) -> String {
        self.flatten(printer).ok().and_then(|f| f.get("printer_model").map(value_text)).unwrap_or_default()
    }

    /// Bambu's model code ("N2S" for the A1, "O1S" for the H2S).
    pub fn model_id(&self, model: &str) -> String {
        self.models.get(model).and_then(|m| m.get("model_id")).map(value_text).unwrap_or_default()
    }

    /// The bed types the printer's model lists, else Bambu's four common ones.
    pub fn bed_types(&self) -> Vec<&'static str> {
        vec!["Textured PEI Plate", "Cool Plate", "Engineering Plate", "High Temp Plate", "Supertack Plate"]
    }

    /// Write a flattened preset with changes on top (`overrides`), for the command line.
    pub fn write_flat(&self, name: &str, path: &Path, overrides: &Map<String, Value>) -> Result<()> {
        let mut flat = self.flatten(name)?;
        for (k, v) in overrides {
            flat.insert(k.clone(), v.clone());
        }
        let text = serde_json::to_string_pretty(&Value::Object(flat)).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("couldn't write {} ({e})", path.display()))
    }

    /// A summary for the `bambu.presets` command.
    pub fn summary(&self, printer: &str) -> BTreeMap<&'static str, Value> {
        let mut m = BTreeMap::new();
        m.insert("printers", Value::from(self.printers()));
        if !printer.is_empty() {
            m.insert("processes", Value::from(self.processes(printer)));
            let fil = self.filaments(printer, true);
            let mine: Vec<&String> = fil.iter().filter(|f| self.is_user(f)).collect();
            m.insert("filaments", Value::from(fil.clone()));
            m.insert("my_filaments", Value::from(mine.into_iter().cloned().collect::<Vec<_>>()));
            m.insert("default_process", self.default_process(printer).map(Value::from).unwrap_or(Value::Null));
            let (x0, y0, x1, y1) = self.bed(printer);
            m.insert("bed", Value::from(vec![x0, y0, x1, y1]));
            m.insert("height", Value::from(self.height(printer)));
            let model = self.model_of(printer);
            m.insert("model_id", Value::from(self.model_id(&model)));
            m.insert("model", Value::from(model));
        }
        m
    }
}

fn sorted(it: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = it.collect();
    v.sort();
    v
}

/// A JSON value as text (a string's own text, a number as written).
pub fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A per-extruder list's first entry, or the value itself.
fn first_text(v: &Value) -> String {
    match v {
        Value::Array(a) => a.first().map(value_text).unwrap_or_default(),
        other => value_text(other),
    }
}

/// A list setting (a JSON list, or `a;b;c`).
fn list_of(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(value_text).filter(|s| !s.is_empty()).collect(),
        Some(Value::String(s)) => s.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 || out.len() >= MAX_FILES {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            collect_json(&p, out, depth + 1);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")) {
            out.push(p);
            if out.len() >= MAX_FILES {
                return;
            }
        }
    }
}

fn read_json(path: &Path) -> Option<Map<String, Value>> {
    if std::fs::metadata(path).ok()?.len() > MAX_PRESET_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(p: &Path, v: Value) {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).ok();
        }
        std::fs::write(p, v.to_string()).ok();
    }

    /// A small preset tree like Bambu Studio's: system A1/H2S printers, processes and filaments, and a
    /// user folder with own filaments and a printer copy.
    pub(crate) fn fake_tree(root: &Path) -> (PathBuf, PathBuf) {
        let sys = root.join("sys");
        let user = root.join("user").join("123456");
        write(
            &sys.join("machine/fdm_machine_common.json"),
            json!({"name": "fdm_machine_common", "instantiation": "false", "type": "machine", "printable_height": "250"}),
        );
        write(&sys.join("machine/Bambu Lab A1.json"), json!({"name": "Bambu Lab A1", "type": "machine_model", "model_id": "N2S"}));
        write(&sys.join("machine/Bambu Lab H2S.json"), json!({"name": "Bambu Lab H2S", "type": "machine_model", "model_id": "O1S"}));
        write(
            &sys.join("machine/a1.json"),
            json!({"name": "Bambu Lab A1 0.4 nozzle", "inherits": "fdm_machine_common", "printer_model": "Bambu Lab A1",
            "printable_area": ["0x0", "256x0", "256x256", "0x256"], "default_print_profile": "0.20mm Standard @BBL A1",
            "default_filament_profile": ["Bambu PLA Basic @BBL A1"], "include": ["A1 start"]}),
        );
        write(&sys.join("machine/a1start.json"), json!({"name": "A1 start", "instantiation": "false", "machine_start_gcode": "G28"}));
        write(
            &sys.join("machine/h2s.json"),
            json!({"name": "Bambu Lab H2S 0.4 nozzle", "inherits": "fdm_machine_common", "printer_model": "Bambu Lab H2S",
            "printable_area": ["0x0", "340x0", "340x320", "0x320"], "printable_height": "340"}),
        );
        write(&sys.join("process/common.json"), json!({"name": "fdm_process_common", "instantiation": "false", "layer_height": "0.2"}));
        write(
            &sys.join("process/a1.json"),
            json!({"name": "0.20mm Standard @BBL A1", "inherits": "fdm_process_common", "compatible_printers": ["Bambu Lab A1 0.4 nozzle"]}),
        );
        write(
            &sys.join("process/a1fine.json"),
            json!({"name": "0.12mm Fine @BBL A1", "inherits": "fdm_process_common", "layer_height": "0.12", "compatible_printers_condition": "printer_model=~/^Bambu Lab A1$/"}),
        );
        write(
            &sys.join("process/h2s.json"),
            json!({"name": "0.20mm Standard @BBL H2S", "inherits": "fdm_process_common", "compatible_printers": ["Bambu Lab H2S 0.4 nozzle"]}),
        );
        write(&sys.join("filament/common.json"), json!({"name": "fdm_filament_common", "instantiation": "false", "filament_type": ["PLA"]}));
        for (tag, printer) in [("A1", "Bambu Lab A1 0.4 nozzle"), ("H2S", "Bambu Lab H2S 0.4 nozzle")] {
            write(
                &sys.join(format!("filament/pla_{tag}.json")),
                json!({"name": format!("Bambu PLA Basic @BBL {tag}"), "inherits": "fdm_filament_common",
                "filament_id": "GFA00", "compatible_printers": [printer]}),
            );
            write(
                &sys.join(format!("filament/petg_{tag}.json")),
                json!({"name": format!("Bambu PETG Basic @BBL {tag}"), "inherits": "fdm_filament_common",
                "filament_type": ["PETG"], "filament_id": "GFG00", "compatible_printers": [printer]}),
            );
        }
        write(
            &user.join("filament/base/acme.json"),
            json!({"name": "ACME PLA Silk @Bambu Lab A1 0.4 nozzle", "inherits": "", "from": "User",
            "filament_id": "P1234567", "filament_type": ["PLA"], "compatible_printers": ["Bambu Lab A1 0.4 nozzle"], "nozzle_temperature": ["225"]}),
        );
        write(
            &user.join("filament/copy.json"),
            json!({"name": "Bambu PLA Basic @BBL A1 - Copy", "inherits": "Bambu PLA Basic @BBL A1", "from": "User",
            "compatible_printers": [], "nozzle_temperature": ["215"]}),
        );
        write(
            &user.join("machine/a1copy.json"),
            json!({"name": "Bambu Lab A1 0.4 nozzle - Copy", "inherits": "Bambu Lab A1 0.4 nozzle", "from": "User"}),
        );
        (sys, user)
    }

    #[test]
    fn library_like_bambu_studio() {
        let dir = std::env::temp_dir().join(format!("fission-profiles-{}", std::process::id()));
        let (sys, user) = fake_tree(&dir);
        let mut lib = Library::load(&[(sys, Origin::System), (user, Origin::User)]);
        lib.set_visible(None);
        let a1 = "Bambu Lab A1 0.4 nozzle";
        // flattening merges parents and templates and marks it stand-alone
        let f = lib.flatten(a1).unwrap_or_default();
        assert_eq!(f.get("machine_start_gcode"), Some(&json!("G28")));
        assert_eq!(f.get("printable_height"), Some(&json!("250")));
        assert_eq!(f.get("inherits"), Some(&json!("")));
        assert_eq!(lib.printers(), vec![a1.to_string(), "Bambu Lab A1 0.4 nozzle - Copy".into(), "Bambu Lab H2S 0.4 nozzle".into()]);
        // processes by list and by condition
        assert_eq!(lib.processes(a1), vec!["0.12mm Fine @BBL A1".to_string(), "0.20mm Standard @BBL A1".into()]);
        assert_eq!(lib.processes("Bambu Lab H2S 0.4 nozzle"), vec!["0.20mm Standard @BBL H2S".to_string()]);
        assert_eq!(lib.default_process(a1).as_deref(), Some("0.20mm Standard @BBL A1"));
        // your own filaments first; the H2S doesn't get the A1's
        let fil = lib.filaments(a1, true);
        assert_eq!(fil[..2], ["ACME PLA Silk @Bambu Lab A1 0.4 nozzle".to_string(), "Bambu PLA Basic @BBL A1 - Copy".into()]);
        assert!(!lib.filaments("Bambu Lab H2S 0.4 nozzle", true).iter().any(|f| f.starts_with("ACME")));
        // the printer copy gets the A1's filaments
        assert!(lib.filaments("Bambu Lab A1 0.4 nozzle - Copy", true).iter().any(|f| f.starts_with("ACME")));
        assert_eq!(lib.flatten("Bambu PLA Basic @BBL A1 - Copy").ok().and_then(|f| f.get("nozzle_temperature").cloned()), Some(json!(["215"])));
        // AMS trays report filament IDs
        assert_eq!(lib.filament_for_id("P1234567", a1).as_deref(), Some("ACME PLA Silk @Bambu Lab A1 0.4 nozzle"));
        assert_eq!(lib.filament_for_id("P1234567", "Bambu Lab H2S 0.4 nozzle"), None);
        assert_eq!(lib.default_filament(a1, "PETG").as_deref(), Some("Bambu PETG Basic @BBL A1"));
        assert_eq!(lib.bed("Bambu Lab H2S 0.4 nozzle"), (0.0, 0.0, 340.0, 320.0));
        assert_eq!(lib.height("Bambu Lab H2S 0.4 nozzle"), 340.0);
        assert_eq!(lib.model_id(&lib.model_of(a1)), "N2S");
        // Bambu Studio's shown-filament list narrows the system ones, never yours
        lib.set_visible(Some(["Bambu PETG Basic @BBL A1".to_string()].into_iter().collect()));
        assert_eq!(lib.filaments(a1, true)[2..], ["Bambu PETG Basic @BBL A1".to_string()]);
        // hostile: a preset that inherits itself
        write(&dir.join("bad/process/loop.json"), json!({"name": "Loop", "inherits": "Loop"}));
        let bad = Library::load(&[(dir.join("bad"), Origin::User)]);
        assert!(bad.flatten("Loop").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}

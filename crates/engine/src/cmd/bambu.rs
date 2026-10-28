//! Fission: printing on Bambu Lab printers with the Bambu Studio you already have. Its printer,
//! process and filament presets (your own first), slicing with its slicer, and sending the sliced
//! plate to a printer on your network (the printers, access codes and AMS contents Bambu Studio
//! knows). Desktop only: the browser build can't start programs or reach printers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};
use solidcraft_bambu::lan::{self, PrintOptions, Printer, PrinterAddr, Tray};
use solidcraft_bambu::model3mf::{self, Part};
use solidcraft_bambu::profiles::{self, Library};
use solidcraft_bambu::slice::{self, Filament, SliceJob, UsedFilament};
use solidcraft_bambu::studio;

use super::CommandSpec;
use crate::params::{bad, bool_, num, str_, string_list};
use crate::schema::bambu as sch;
use crate::schema::{TypedContract, output_schema_for};
use crate::{EngineError, Result, Session};

pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec::new("bambu.open_in_studio", "Send with Bambu Studio", crate::checked!("bambu.open_in_studio", open_in_studio))
        .contract(&OPEN_CONTRACT)
        .output(output_schema_for::<sch::OpenInStudioResult>)
        .icon("export")
        .noundo()
        .params("file? (a .3mf or sliced .gcode.3mf; default: the design's shown bodies, or `bodies`, written as a 3MF with their colours); bodies?: [names]; slots?: {body: filament slot 1..16} → file: what Bambu Studio was started with"),
    CommandSpec::new("bambu.presets", "Bambu Presets", crate::checked!("bambu.presets", presets))
        .contract(&PRESETS_CONTRACT)
        .output(output_schema_for::<sch::PresetsResult>)
        .icon("settings")
        .noundo()
        .params("printer? (a printer preset name, as bambu.presets lists; default: the first printer Bambu Studio knows) → studio: the Bambu Studio program or null, printers: [preset names], printer, processes, filaments (your own first, then the system ones you show in Bambu Studio), default_process, default_filament, bed_types, bed: [x0, y0, x1, y1], height"),
    CommandSpec::new("bambu.print", "Print Plate", crate::checked!("bambu.print", print))
        .contract(&PRINT_CONTRACT)
        .output(output_schema_for::<sch::PrintResult>)
        .icon("export")
        .noundo()
        .params("serial (from bambu.printers); file (a sliced .gcode.3mf, from bambu.slice); ip? (default: Bambu Studio's, else found on the network); mapping?: [tray number per filament slot: AMS unit*4+slot, 254 the external spool, -1 auto] (default: each slot from the tray with the same filament, else the same material and nearest colour); bed_leveling?, flow_cali?, nozzle_offset_cali?: auto|on|off (default: Bambu Studio's last choice for this model); timelapse?: bool; name? (shown on the printer) → uploaded, started, mapping: [{slot, tray, label}]"),
    CommandSpec::new("bambu.printers", "Bambu Printers", crate::checked!("bambu.printers", printers))
        .contract(&PRINTERS_CONTRACT)
        .output(output_schema_for::<sch::PrintersResult>)
        .icon("settings")
        .noundo()
        .params("discover?: bool (listen for the printers on the network to find IP addresses Bambu Studio keeps scrambled; default true); wait? (seconds, 1..10, default 4) → printers: [{serial, name, model, preset, ip, online, ams: [filament ids Bambu Studio last saw]}]"),
    CommandSpec::new("bambu.slice", "Slice Plate", crate::checked!("bambu.slice", slice_plate))
        .contract(&SLICE_CONTRACT)
        .output(output_schema_for::<sch::SliceResult>)
        .icon("export")
        .noundo()
        .params("printer? (preset, from bambu.presets; default as bambu.presets); process? (default: the printer's default); filaments?: [preset name or {preset, color?}] (slot 1 first; default: the printer's PLA in the bodies' colours); bodies?: [names] (default: the shown bodies); slots?: {body: slot 1..16} (default 1); bed_type?; arrange?: bool (default false: the bodies stay where they are, centred on the plate); orient?: bool; overrides?: {setting: value} (process settings, e.g. sparse_infill_density) → file (the sliced .gcode.3mf), seconds, time, grams, filaments: [{slot, material, color, grams, meters}], note"),
    CommandSpec::new("bambu.status", "Printer Status", crate::checked!("bambu.status", status))
        .contract(&STATUS_CONTRACT)
        .output(output_schema_for::<sch::StatusResult>)
        .icon("settings")
        .noundo()
        .params("serial; ip? → state, percent, minutes_left, layer, layers, job, nozzle, bed, error, summary, trays: [{tray, label, material, color, filament_id}]"),
];

const OPEN_CONTRACT: TypedContract<sch::OpenInStudioParams> = TypedContract::new();
const PRESETS_CONTRACT: TypedContract<sch::PresetsParams> = TypedContract::new();
const PRINT_CONTRACT: TypedContract<sch::PrintParams> = TypedContract::new();
const PRINTERS_CONTRACT: TypedContract<sch::PrintersParams> = TypedContract::new();
const SLICE_CONTRACT: TypedContract<sch::SliceParams> = TypedContract::new();
const STATUS_CONTRACT: TypedContract<sch::StatusParams> = TypedContract::new();

/// How long a slice may take.
const SLICE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const MAX_SLOTS: u32 = 16;

fn other(e: String) -> EngineError {
    EngineError::Other(e)
}

fn studio_exe(cmd: &str) -> Result<PathBuf> {
    profiles::find_studio(None).ok_or_else(|| bad(cmd, "Bambu Studio isn't installed (or set FISSION_BAMBU_STUDIO to its program)"))
}

fn library(exe: Option<&Path>, cmd: &str) -> Result<Library> {
    let lib = Library::for_install(exe);
    if lib.is_empty() {
        return Err(bad(cmd, "no Bambu Studio presets were found (is Bambu Studio installed and opened once?)"));
    }
    Ok(lib)
}

fn printer_preset(lib: &Library, p: &Value, cmd: &str) -> Result<String> {
    let all = lib.printers();
    match str_(p, "printer") {
        Some(n) if all.iter().any(|x| x == n) => Ok(n.to_string()),
        Some(n) => Err(bad(cmd, format!("no printer preset `{n}` (bambu.presets lists them)"))),
        None => {
            let conf = studio::read_conf(None);
            let known: Vec<String> = studio::printers(&conf).iter().map(|s| studio::default_preset(&s.model)).collect();
            known
                .into_iter()
                .find(|k| all.contains(k))
                .or_else(|| all.first().cloned())
                .ok_or_else(|| bad(cmd, "Bambu Studio has no printer presets"))
        }
    }
}

fn hex_color(c: Option<[f32; 3]>) -> String {
    match c {
        Some([r, g, b]) => {
            let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            format!("#{:02X}{:02X}{:02X}", q(r), q(g), q(b))
        }
        None => "#C8C8C8".into(),
    }
}

/// The bodies to print: the named ones, else those shown, each on its slot.
fn plate_parts(s: &Session, p: &Value, cmd: &str) -> Result<Vec<Part>> {
    let named = string_list(p, "bodies");
    let state = s.doc.painted(&s.world_state());
    let wanted: Vec<String> = if named.is_empty() {
        state.bodies.iter().map(|b| b.name.clone()).filter(|n| !s.visibility.hidden_bodies.contains(n)).collect()
    } else {
        named
    };
    if wanted.is_empty() {
        return Err(bad(cmd, "no bodies are shown to print"));
    }
    let slots = p.get("slots").and_then(Value::as_object);
    let meshes = solidcraft_io::print_meshes(&state, &wanted, solidcraft_io::MeshQuality::Standard)?;
    meshes
        .into_iter()
        .map(|m| {
            let filament = match slots.and_then(|o| o.get(&m.name)) {
                None | Some(Value::Null) => 1,
                Some(v) => match v.as_u64() {
                    Some(n) if (1..=u64::from(MAX_SLOTS)).contains(&n) => n as u32,
                    _ => return Err(bad(cmd, format!("the slot for `{}` must be 1..{MAX_SLOTS}", m.name))),
                },
            };
            Ok(Part { name: m.name, vertices: m.positions, triangles: m.triangles, color: hex_color(m.color), filament })
        })
        .collect()
}

fn safe_name(s: &str) -> String {
    let n: String = s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(60).collect();
    if n.trim_matches('_').is_empty() { "design".into() } else { n }
}

fn work_folder(s: &Session, cmd: &str) -> Result<PathBuf> {
    let base = solidcraft_bambu::home_folder().ok_or_else(|| bad(cmd, "no home folder to slice in"))?;
    Ok(base.join("plates").join(safe_name(&s.doc.name)))
}

fn presets(_: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bambu.presets";
    let exe = profiles::find_studio(None);
    let lib = library(exe.as_deref(), cmd)?;
    let printer = printer_preset(&lib, p, cmd)?;
    let (x0, y0, x1, y1) = lib.bed(&printer);
    reply(sch::PresetsResult {
        studio: exe.map(|e| e.to_string_lossy().into_owned()),
        printers: lib.printers(),
        processes: lib.processes(&printer),
        filaments: lib.filaments(&printer, true),
        default_process: lib.default_process(&printer),
        default_filament: lib.default_filament(&printer, "PLA"),
        bed_types: lib.bed_types().iter().map(|b| b.to_string()).collect(),
        bed: [x0, y0, x1, y1],
        height: lib.height(&printer),
        printer,
        ..Default::default()
    })
}

fn printers(_: &mut Session, p: &Value) -> Result<Value> {
    let conf = studio::read_conf(None);
    let mut list = studio::printers(&conf);
    let mut online: Vec<String> = Vec::new();
    if bool_(p, "discover").unwrap_or(true) && !list.is_empty() {
        let wait = num(p, "wait").unwrap_or(4.0).clamp(1.0, 10.0);
        let want: Vec<String> = list.iter().map(|x| x.serial.clone()).collect();
        for f in lan::discover(Duration::from_secs_f64(wait), &want) {
            if let Some(x) = list.iter_mut().find(|x| x.serial.eq_ignore_ascii_case(&f.serial)) {
                x.ip = f.ip;
                online.push(x.serial.clone());
            }
        }
    }
    let printers = list
        .iter()
        .map(|x| sch::PrinterInfo {
            serial: x.serial.clone(),
            name: x.name.clone(),
            model: x.model.clone(),
            preset: studio::default_preset(&x.model),
            ip: x.ip.clone(),
            online: online.contains(&x.serial),
            ams: studio::last_ams(&conf, &x.serial),
        })
        .collect();
    reply(sch::PrintersResult { printers, ..Default::default() })
}

fn filaments_of(lib: &Library, printer: &str, p: &Value, parts: &[Part], cmd: &str) -> Result<Vec<Filament>> {
    let used = parts.iter().map(|x| x.filament).max().unwrap_or(1);
    let colour_of = |slot: u32| parts.iter().find(|x| x.filament == slot).map(|x| x.color.clone()).unwrap_or_else(|| "#C8C8C8".into());
    let mut out = Vec::new();
    if let Some(list) = p.get("filaments").and_then(Value::as_array) {
        if list.len() > MAX_SLOTS as usize {
            return Err(bad(cmd, format!("at most {MAX_SLOTS} filaments")));
        }
        let known = lib.filaments(printer, false);
        for (i, f) in list.iter().enumerate() {
            let (preset, color) = match f {
                Value::String(s) => (s.clone(), None),
                Value::Object(o) => {
                    (o.get("preset").and_then(Value::as_str).unwrap_or("").to_string(), o.get("color").and_then(Value::as_str).map(str::to_string))
                }
                _ => return Err(bad(cmd, "each filament is a preset name or {preset, color}")),
            };
            if !known.contains(&preset) {
                return Err(bad(cmd, format!("`{preset}` isn't a filament preset for {printer} (bambu.presets lists them)")));
            }
            out.push(Filament { preset, color: color.unwrap_or_else(|| colour_of(i as u32 + 1)), overrides: Map::new() });
        }
    }
    let fallback = lib.default_filament(printer, "PLA").or_else(|| lib.filaments(printer, true).first().cloned());
    while (out.len() as u32) < used {
        let preset = out
            .last()
            .map(|f| f.preset.clone())
            .or_else(|| fallback.clone())
            .ok_or_else(|| bad(cmd, format!("no filament presets for {printer}")))?;
        out.push(Filament { preset, color: colour_of(out.len() as u32 + 1), overrides: Map::new() });
    }
    Ok(out)
}

fn used_info(f: &[UsedFilament]) -> Vec<sch::UsedFilamentInfo> {
    f.iter()
        .map(|u| sch::UsedFilamentInfo { slot: u.slot, material: u.material.clone(), color: u.color.clone(), grams: u.grams, meters: u.meters })
        .collect()
}

fn reply<T: serde::Serialize>(r: T) -> Result<Value> {
    serde_json::to_value(r).map_err(|e| other(format!("couldn't write the reply ({e})")))
}

fn slice_plate(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bambu.slice";
    let exe = studio_exe(cmd)?;
    let lib = library(Some(&exe), cmd)?;
    let printer = printer_preset(&lib, p, cmd)?;
    let process = match str_(p, "process") {
        Some(n) if lib.processes(&printer).iter().any(|x| x == n) => n.to_string(),
        Some(n) => return Err(bad(cmd, format!("`{n}` isn't a process preset for {printer}"))),
        None => lib.default_process(&printer).ok_or_else(|| bad(cmd, format!("no process presets for {printer}")))?,
    };
    let bed_type = match str_(p, "bed_type") {
        None => String::new(),
        Some(b) if lib.bed_types().contains(&b) => b.to_string(),
        Some(b) => return Err(bad(cmd, format!("unknown bed_type `{b}` ({})", lib.bed_types().join(", ")))),
    };
    let mut parts = plate_parts(s, p, cmd)?;
    model3mf::centre_on_bed(&mut parts, lib.bed(&printer));
    if let Some((lo, hi)) = model3mf::bounds_of(&parts) {
        let (x0, y0, x1, y1) = lib.bed(&printer);
        let fits = hi[0] - lo[0] <= x1 - x0 && hi[1] - lo[1] <= y1 - y0 && hi[2] - lo[2] <= lib.height(&printer);
        if !fits && !bool_(p, "arrange").unwrap_or(false) {
            return Err(bad(
                cmd,
                format!(
                    "the bodies ({:.0} x {:.0} x {:.0} mm) don't fit on the {printer} plate; try arrange: true",
                    hi[0] - lo[0],
                    hi[1] - lo[1],
                    hi[2] - lo[2]
                ),
            ));
        }
    }
    let filaments = filaments_of(&lib, &printer, p, &parts, cmd)?;
    let overrides = p.get("overrides").and_then(Value::as_object).cloned().unwrap_or_default();
    let job = SliceJob {
        printer,
        process,
        filaments,
        overrides,
        bed_type,
        arrange: bool_(p, "arrange").unwrap_or(false),
        orient: bool_(p, "orient").unwrap_or(false),
    };
    let folder = work_folder(s, cmd)?;
    let r = slice::slice(&exe, &lib, &job, &parts, &folder, SLICE_TIMEOUT).map_err(other)?;
    reply(sch::SliceResult {
        file: r.file.to_string_lossy().into_owned(),
        seconds: r.seconds,
        time: r.time_text(),
        grams: r.grams,
        filaments: used_info(&r.filaments),
        note: r.note,
        printer: job.printer,
        process: job.process,
        ..Default::default()
    })
}

fn open_in_studio(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bambu.open_in_studio";
    let exe = studio_exe(cmd)?;
    let file = match str_(p, "file") {
        Some(f) => {
            let f = PathBuf::from(f);
            if !f.is_file() {
                return Err(bad(cmd, format!("{} isn't there", f.display())));
            }
            f
        }
        None => {
            let parts = plate_parts(s, p, cmd)?;
            let folder = work_folder(s, cmd)?;
            std::fs::create_dir_all(&folder).map_err(|e| other(format!("couldn't make {} ({e})", folder.display())))?;
            let f = folder.join(format!("{}.3mf", safe_name(&s.doc.name)));
            std::fs::write(&f, model3mf::write(&parts).map_err(other)?).map_err(|e| other(format!("couldn't write {} ({e})", f.display())))?;
            f
        }
    };
    slice::open_in_studio(&exe, &file).map_err(other)?;
    reply(sch::OpenInStudioResult { file: file.to_string_lossy().into_owned(), ..Default::default() })
}

/// Where a printer is: Bambu Studio's serial and access code, and its IP address (given, readable
/// in Bambu Studio's settings, or found on the network).
fn address(p: &Value, cmd: &str) -> Result<PrinterAddr> {
    let serial = str_(p, "serial").map(str::trim).filter(|x| !x.is_empty()).ok_or_else(|| bad(cmd, "give `serial` (bambu.printers lists them)"))?;
    let conf = studio::read_conf(None);
    let known = studio::printers(&conf);
    let sp = known
        .iter()
        .find(|x| x.serial.eq_ignore_ascii_case(serial))
        .ok_or_else(|| bad(cmd, format!("Bambu Studio isn't set up with printer {serial} (connect it in Bambu Studio first)")))?;
    let mut ip = str_(p, "ip").map(str::trim).unwrap_or("").to_string();
    if !ip.is_empty() && ip.parse::<std::net::Ipv4Addr>().is_err() {
        return Err(bad(cmd, format!("`{ip}` isn't an IP address")));
    }
    if ip.is_empty() {
        ip.clone_from(&sp.ip);
    }
    if ip.is_empty() {
        ip = lan::discover(Duration::from_secs(5), std::slice::from_ref(&sp.serial))
            .into_iter()
            .find(|f| f.serial.eq_ignore_ascii_case(&sp.serial))
            .map(|f| f.ip)
            .ok_or_else(|| {
                other(format!("couldn't find the {} ({serial}) on the network. Is it on and on the same network? You can give its `ip`.", sp.name))
            })?;
    }
    Ok(PrinterAddr { ip, serial: sp.serial.clone(), access_code: sp.access_code.clone(), model: sp.model.clone() })
}

fn tray_info(t: &Tray) -> sch::TrayInfo {
    sch::TrayInfo { tray: t.index(), label: t.label(), material: t.material.clone(), color: t.color.clone(), filament_id: t.filament_id.clone() }
}

fn status(_: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bambu.status";
    let addr = address(p, cmd)?;
    let pr = Printer::connect(addr, Duration::from_secs(6)).map_err(other)?;
    let st = pr.status();
    pr.close();
    reply(sch::StatusResult {
        summary: st.summary(),
        trays: st.trays.iter().map(tray_info).collect(),
        state: st.state,
        percent: st.percent,
        minutes_left: st.minutes_left,
        layer: st.layer,
        layers: st.layers,
        job: st.job,
        nozzle: st.nozzle,
        bed: st.bed,
        error: st.error,
        ..Default::default()
    })
}

fn rgb(c: &str) -> Option<(i32, i32, i32)> {
    let h = c.trim().trim_start_matches('#');
    let n = |i: usize| h.get(i..i + 2).and_then(|x| i32::from_str_radix(x, 16).ok());
    Some((n(0)?, n(2)?, n(4)?))
}

/// Which tray each filament slot of the sliced plate prints from: the same material, nearest colour.
fn auto_map(used: &[UsedFilament], trays: &[Tray]) -> Vec<Option<Tray>> {
    let slots = used.iter().map(|u| u.slot).max().unwrap_or(0) as usize;
    let mut out: Vec<Option<Tray>> = vec![None; slots];
    for u in used {
        let dist = |t: &Tray| match (rgb(&u.color), rgb(&t.color)) {
            (Some(a), Some(b)) => (a.0 - b.0).pow(2) + (a.1 - b.1).pow(2) + (a.2 - b.2).pow(2),
            _ => i32::MAX / 2,
        };
        let pick = trays
            .iter()
            .filter(|t| !t.material.is_empty() && t.material.eq_ignore_ascii_case(&u.material))
            .min_by_key(|t| dist(t))
            .or_else(|| trays.iter().filter(|t| !t.material.is_empty()).min_by_key(|t| dist(t)));
        if let (Some(t), Some(slot)) = (pick, (u.slot as usize).checked_sub(1))
            && let Some(o) = out.get_mut(slot)
        {
            *o = Some(t.clone());
        }
    }
    out
}

fn choice(p: &Value, k: &str, last: &Map<String, Value>, cmd: &str) -> Result<String> {
    let v = match str_(p, k) {
        Some(v) => v.to_ascii_lowercase(),
        None => last.get(k).and_then(Value::as_str).unwrap_or("auto").to_ascii_lowercase(),
    };
    if ["auto", "on", "off"].contains(&v.as_str()) { Ok(v) } else { Err(bad(cmd, format!("`{k}` is auto, on or off"))) }
}

fn print(_: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "bambu.print";
    let file = PathBuf::from(str_(p, "file").ok_or_else(|| bad(cmd, "give `file`, a sliced .gcode.3mf (from bambu.slice)"))?);
    let name_ok = file.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(".gcode.3mf")).unwrap_or(false);
    if !name_ok || !file.is_file() {
        return Err(bad(cmd, format!("{} isn't a sliced .gcode.3mf", file.display())));
    }
    let data = std::fs::read(&file).map_err(|e| other(format!("couldn't read {} ({e})", file.display())))?;
    let sliced = slice::read_sliced(&file, 1).map_err(other)?;
    let addr = address(p, cmd)?;
    let conf = studio::read_conf(None);
    let last = studio::print_defaults(&conf, &addr.model);
    let opts = PrintOptions {
        bed_leveling: choice(p, "bed_leveling", &last, cmd)?,
        flow_cali: choice(p, "flow_cali", &last, cmd)?,
        nozzle_offset_cali: choice(p, "nozzle_offset_cali", &last, cmd)?,
        timelapse: bool_(p, "timelapse").unwrap_or_else(|| last.get("timelapse").and_then(Value::as_str) == Some("on")),
    };
    let mut pr = Printer::connect(addr, Duration::from_secs(8)).map_err(other)?;
    let st = pr.status();
    if st.busy() {
        pr.close();
        return Err(other(format!("the printer is busy ({})", st.summary())));
    }
    let mut mapping = auto_map(&sliced.filaments, &st.trays);
    if let Some(given) = p.get("mapping").and_then(Value::as_array) {
        for (i, v) in given.iter().enumerate().take(MAX_SLOTS as usize) {
            let Some(n) = v.as_i64() else { continue };
            if n < 0 {
                continue;
            }
            let t = st.trays.iter().find(|t| t.index() == n).cloned().ok_or_else(|| bad(cmd, format!("the printer has no tray {n}")))?;
            if mapping.len() <= i {
                mapping.resize(i + 1, None);
            }
            if let Some(slot) = mapping.get_mut(i) {
                *slot = Some(t);
            }
        }
    }
    let remote = format!(
        "{}.gcode.3mf",
        safe_name(file.file_name().map(|n| n.to_string_lossy().trim_end_matches(".gcode.3mf").to_string()).unwrap_or_default().as_str())
    );
    let job = str_(p, "name").map(str::to_string).unwrap_or_else(|| remote.trim_end_matches(".gcode.3mf").to_string());
    let result = pr.upload(&data, &remote, &mut |_, _| {}).and_then(|()| pr.start(&remote, &mapping, &opts, &job));
    pr.close();
    result.map_err(other)?;
    let mapping = mapping
        .iter()
        .enumerate()
        .map(|(i, t)| sch::SlotTray { slot: i + 1, tray: t.as_ref().map(Tray::index), label: t.as_ref().map(Tray::label) })
        .collect();
    reply(sch::PrintResult { uploaded: remote, started: true, mapping, time: sliced.time_text(), grams: sliced.grams, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tray(ams: u32, slot: u32, material: &str, color: &str) -> Tray {
        Tray { ams, slot, material: material.into(), color: color.into(), ..Tray::default() }
    }

    #[test]
    fn slots_print_from_the_matching_tray() {
        let trays = [tray(0, 0, "PLA", "#FFFFFF"), tray(0, 1, "PLA", "#C02A20"), tray(0, 2, "PETG", "#C02A20")];
        let used = [
            UsedFilament { slot: 1, material: "PLA".into(), color: "#D03020".into(), ..Default::default() },
            UsedFilament { slot: 3, material: "PETG".into(), color: "#000000".into(), ..Default::default() },
        ];
        let m = auto_map(&used, &trays);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].as_ref().map(Tray::index), Some(1));
        assert_eq!(m[1], None);
        assert_eq!(m[2].as_ref().map(Tray::index), Some(2));
        // hostile: no trays, a slot of 0
        assert!(auto_map(&[UsedFilament::default()], &[]).iter().all(Option::is_none));
    }

    #[test]
    fn colours_and_names() {
        assert_eq!(hex_color(Some([1.0, 0.0, 0.5])), "#FF0080");
        assert_eq!(hex_color(Some([f32::NAN, 2.0, -1.0])), "#00FF00");
        assert_eq!(hex_color(None), "#C8C8C8");
        assert_eq!(safe_name("My part / v2"), "My_part___v2");
        assert_eq!(safe_name("../"), "design");
    }
}

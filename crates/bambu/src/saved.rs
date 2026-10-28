//! Printers you added in Fission (by IP address and access code), kept in `~/Fission/printers.json`,
//! alongside the ones Bambu Studio knows. Yours win where both have the same serial number (a newer
//! IP address, your own name for it).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::Result;
use crate::studio::{self, StudioPrinter};

/// Most printers kept.
const MAX_PRINTERS: usize = 64;
/// Biggest printers file read.
const MAX_BYTES: u64 = 1 << 20;

/// Where Fission keeps them: `FISSION_PRINTERS`, else `~/Fission/printers.json`.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FISSION_PRINTERS") {
        return Some(PathBuf::from(p));
    }
    crate::home_folder().map(|d| d.join("printers.json"))
}

fn text(v: &Map<String, Value>, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim().chars().take(128).collect()
}

/// The printers added in Fission.
pub fn load(file: Option<&Path>) -> Vec<StudioPrinter> {
    let Some(file) = file.map(Path::to_path_buf).or_else(path) else { return Vec::new() };
    if std::fs::metadata(&file).map(|m| m.len() > MAX_BYTES).unwrap_or(true) {
        return Vec::new();
    }
    let Ok(Value::Array(list)) = std::fs::read_to_string(&file).map(|t| serde_json::from_str(&t).unwrap_or(Value::Null)) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(Value::as_object)
        .take(MAX_PRINTERS)
        .filter_map(|o| {
            let serial = text(o, "serial");
            let access_code = text(o, "access_code");
            if serial.is_empty() || access_code.is_empty() {
                return None;
            }
            let model = match text(o, "model") {
                m if m.is_empty() => studio::model_from_serial(&serial).to_string(),
                m => m,
            };
            let name = match text(o, "name") {
                n if n.is_empty() => studio::model_name(&model).to_string(),
                n => n,
            };
            Some(StudioPrinter { serial, ip: text(o, "ip"), access_code, model, name })
        })
        .collect()
}

fn write(file: &Path, list: &[StudioPrinter]) -> Result<()> {
    let out: Vec<Value> =
        list.iter().map(|p| json!({"serial": p.serial, "ip": p.ip, "access_code": p.access_code, "model": p.model, "name": p.name})).collect();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't make {} ({e})", dir.display()))?;
    }
    let body = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
    std::fs::write(file, body).map_err(|e| format!("couldn't save the printers ({e})"))
}

/// Add a printer, or change the one with this serial number.
pub fn save(file: Option<&Path>, printer: StudioPrinter) -> Result<Vec<StudioPrinter>> {
    let file = file.map(Path::to_path_buf).or_else(path).ok_or("no home folder to keep printers in")?;
    if printer.serial.trim().is_empty() || printer.access_code.trim().is_empty() {
        return Err("a printer needs its serial number and access code".into());
    }
    if !printer.ip.is_empty() && printer.ip.parse::<std::net::Ipv4Addr>().is_err() {
        return Err(format!("`{}` isn't an IP address", printer.ip));
    }
    let mut list = load(Some(&file));
    list.retain(|p| !p.serial.eq_ignore_ascii_case(&printer.serial));
    if list.len() >= MAX_PRINTERS {
        return Err(format!("at most {MAX_PRINTERS} printers"));
    }
    list.push(printer);
    list.sort_by(|a, b| a.serial.cmp(&b.serial));
    write(&file, &list)?;
    Ok(list)
}

/// Forget a printer added in Fission (Bambu Studio's own stay).
pub fn remove(file: Option<&Path>, serial: &str) -> Result<Vec<StudioPrinter>> {
    let file = file.map(Path::to_path_buf).or_else(path).ok_or("no home folder to keep printers in")?;
    let mut list = load(Some(&file));
    list.retain(|p| !p.serial.eq_ignore_ascii_case(serial.trim()));
    write(&file, &list)?;
    Ok(list)
}

/// Bambu Studio's printers and yours, one per serial number (yours win), and where each came from.
pub fn all(conf: &Map<String, Value>, file: Option<&Path>) -> Vec<(StudioPrinter, bool)> {
    let mine = load(file);
    let mut out: Vec<(StudioPrinter, bool)> =
        studio::printers(conf).into_iter().filter(|s| !mine.iter().any(|m| m.serial.eq_ignore_ascii_case(&s.serial))).map(|s| (s, false)).collect();
    out.extend(mine.into_iter().map(|m| (m, true)));
    out.sort_by(|a, b| a.0.serial.cmp(&b.0.serial));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printers_added_in_fission_are_kept_and_win() {
        let dir = std::env::temp_dir().join(format!("fission-printers-{}", std::process::id()));
        let file = dir.join("printers.json");
        let p = StudioPrinter {
            serial: "03900A000000001".into(),
            ip: "192.168.1.30".into(),
            access_code: "12345678".into(),
            model: String::new(),
            name: String::new(),
        };
        let list = save(Some(&file), p).unwrap_or_default();
        assert_eq!(list.len(), 1);
        let back = load(Some(&file));
        assert_eq!((back[0].model.as_str(), back[0].name.as_str()), ("N2S", "A1"));
        let conf: Map<String, Value> =
            serde_json::from_value(json!({"access_code": {"03900A000000001": "12345678", "09300B000000002": "abcdefgh"}})).unwrap_or_default();
        let all = all(&conf, Some(&file));
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|(p, mine)| *mine && p.ip == "192.168.1.30"));
        // hostile input: no access code, a bad IP, a broken file
        let bad = StudioPrinter { serial: "X".into(), ..Default::default() };
        assert!(save(Some(&file), bad).is_err());
        let bad_ip = StudioPrinter { serial: "X".into(), access_code: "1".into(), ip: "not an ip".into(), ..Default::default() };
        assert!(save(Some(&file), bad_ip).is_err());
        assert!(remove(Some(&file), "03900A000000001").unwrap_or_default().is_empty());
        std::fs::write(&file, "{{{").ok();
        assert!(load(Some(&file)).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}

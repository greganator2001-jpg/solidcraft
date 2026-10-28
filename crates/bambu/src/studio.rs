//! What Bambu Studio's settings file (`BambuStudio.conf`) knows: the printers it's connected to on your
//! network (IP address, serial number, access code), your last send-print choices per printer model, the
//! filament IDs it last saw in each AMS, and which system filaments you show. Read only: nothing here
//! writes to Bambu Studio's files.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::profiles::{settings_dir, value_text};

/// Biggest settings file read.
const MAX_CONF_BYTES: u64 = 16 << 20;

/// Serial-number prefix -> Bambu's model code.
pub const SERIAL_PREFIX: [(&str, &str); 9] = [
    ("00M", "BL-P001"), // X1 Carbon
    ("00W", "BL-P002"), // X1
    ("03W", "C13"),     // X1E
    ("01S", "C11"),     // P1P
    ("01P", "C12"),     // P1S
    ("030", "N1"),      // A1 mini
    ("039", "N2S"),     // A1
    ("094", "O1D"),     // H2D
    ("093", "O1S"),     // H2S
];

/// Model code -> the name people use.
pub const MODEL_NAMES: [(&str, &str); 9] = [
    ("N2S", "A1"),
    ("N1", "A1 mini"),
    ("O1S", "H2S"),
    ("O1D", "H2D"),
    ("C11", "P1P"),
    ("C12", "P1S"),
    ("C13", "X1E"),
    ("BL-P001", "X1 Carbon"),
    ("BL-P002", "X1"),
];

/// Bambu Studio's settings file: `FISSION_BAMBU_CONF`, else `<settings>/BambuStudio.conf`.
pub fn conf_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FISSION_BAMBU_CONF") {
        return Some(PathBuf::from(p));
    }
    settings_dir().map(|d| d.join("BambuStudio.conf"))
}

/// The settings (JSON followed by a `# MD5 checksum` line); empty when there are none.
pub fn read_conf(path: Option<&Path>) -> Map<String, Value> {
    let Some(path) = path.map(Path::to_path_buf).or_else(conf_path) else { return Map::new() };
    if std::fs::metadata(&path).map(|m| m.len() > MAX_CONF_BYTES).unwrap_or(true) {
        return Map::new();
    }
    let Ok(bytes) = std::fs::read(&path) else { return Map::new() };
    let text = String::from_utf8_lossy(&bytes);
    let Some(end) = text.rfind('}') else { return Map::new() };
    match serde_json::from_str::<Value>(&text[..=end]) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// A printer Bambu Studio is set up with on the local network.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StudioPrinter {
    pub serial: String,
    /// The IP address when Bambu Studio's settings hold it readably; "" when they hold it scrambled
    /// (current versions do): then the printer is found on the network by its serial number
    /// ([`crate::lan::discover`]).
    pub ip: String,
    pub access_code: String,
    /// Bambu's model code ("N2S" = A1, "O1S" = H2S), "" when the serial number doesn't say.
    pub model: String,
    /// "A1", "H2S" …
    pub name: String,
}

pub fn model_from_serial(serial: &str) -> &'static str {
    let pre: String = serial.chars().take(3).collect::<String>().to_uppercase();
    SERIAL_PREFIX.iter().find(|(p, _)| *p == pre).map(|(_, m)| *m).unwrap_or("")
}

pub fn model_name(model: &str) -> &'static str {
    MODEL_NAMES.iter().find(|(m, _)| m.eq_ignore_ascii_case(model)).map(|(_, n)| *n).unwrap_or("Bambu printer")
}

/// The printer preset name Bambu Studio uses for a model's 0.4 mm nozzle ("Bambu Lab A1 0.4 nozzle").
pub fn default_preset(model: &str) -> String {
    let n = model_name(model);
    if n == "Bambu printer" { String::new() } else { format!("Bambu Lab {n} 0.4 nozzle") }
}

/// The LAN printers in Bambu Studio's settings (those with an access code). See [`StudioPrinter::ip`].
pub fn printers(conf: &Map<String, Value>) -> Vec<StudioPrinter> {
    let Some(codes) = conf.get("access_code").and_then(Value::as_object) else { return Vec::new() };
    let ips = conf.get("user_access_dev_ip").and_then(Value::as_object);
    let mut out: Vec<StudioPrinter> = codes
        .iter()
        .filter_map(|(serial, code)| {
            let code = value_text(code);
            if code.is_empty() {
                return None;
            }
            let ip = ips.and_then(|m| m.get(serial)).map(value_text).filter(|s| s.parse::<std::net::Ipv4Addr>().is_ok()).unwrap_or_default();
            let model = model_from_serial(serial).to_string();
            Some(StudioPrinter { serial: serial.clone(), ip, access_code: code, name: model_name(&model).to_string(), model })
        })
        .collect();
    out.sort_by(|a, b| a.serial.cmp(&b.serial));
    out
}

/// Bambu Studio's last send-print choices for a printer model: bed_leveling, flow_cali,
/// nozzle_offset_cali, timelapse -> "on" / "off" / "auto".
pub fn print_defaults(conf: &Map<String, Value>, model: &str) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(m) = conf.get(model).and_then(Value::as_object) {
        for k in ["bed_leveling", "flow_cali", "nozzle_offset_cali", "timelapse"] {
            if let Some(v) = m.get(k) {
                out.insert(k.into(), Value::from(value_text(v)));
            }
        }
    }
    out
}

/// The filament IDs Bambu Studio last saw in this printer's AMS, in tray order.
pub fn last_ams(conf: &Map<String, Value>, serial: &str) -> Vec<String> {
    conf.get("ams_filament_ids")
        .and_then(Value::as_object)
        .and_then(|m| m.get(serial))
        .map(value_text)
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_bambu_studio_settings() {
        let dir = std::env::temp_dir().join(format!("fission-conf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let p = dir.join("BambuStudio.conf");
        let conf = json!({
            "access_code": {"03900A000000001": "12345678", "09300B000000002": "abcdefgh", "00000000000000X": ""},
            "user_access_dev_ip": {"03900A000000001": "192.168.1.20", "09300B000000002": "d4836a865e23cbbe9865cf19"},
            "N2S": {"bed_leveling": "off", "flow_cali": "on", "timelapse": "off"},
            "ams_filament_ids": {"03900A000000001": "GFA00,P1234567,,"}
        });
        std::fs::write(&p, format!("{conf:#}\n# MD5 checksum 0123456789ABCDEF\n")).ok();
        let c = read_conf(Some(&p));
        let ps = printers(&c);
        assert_eq!(ps.len(), 2);
        assert_eq!((ps[0].name.as_str(), ps[0].model.as_str(), ps[0].ip.as_str()), ("A1", "N2S", "192.168.1.20"));
        // a scrambled IP (current Bambu Studio) is left out: the printer is found on the network instead
        assert_eq!((ps[1].name.as_str(), ps[1].model.as_str(), ps[1].ip.as_str()), ("H2S", "O1S", ""));
        assert_eq!(default_preset("O1S"), "Bambu Lab H2S 0.4 nozzle");
        assert_eq!(print_defaults(&c, "N2S").get("flow_cali"), Some(&json!("on")));
        assert_eq!(last_ams(&c, "03900A000000001"), vec!["GFA00", "P1234567", "", ""]);
        // a missing or broken file is just empty
        assert!(read_conf(Some(&dir.join("nope.conf"))).is_empty());
        std::fs::write(&p, "not json at all }").ok();
        assert!(read_conf(Some(&p)).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}

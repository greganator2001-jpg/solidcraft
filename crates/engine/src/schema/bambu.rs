//! Contracts and result types for the commands in `cmd/bambu.rs` (Fission's Bambu Lab printing).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::types::StringList;

/// List the printer, process and filament presets of the installed Bambu Studio, for one printer.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PresetsParams {
    /// A printer preset name, as `printers` in the reply lists them. Default: the preset of the
    /// first printer Bambu Studio is connected to, else the first printer preset.
    #[serde(default)]
    pub printer: Option<String>,
    /// The process preset whose main settings `process_values` gives. Default: the printer's default.
    #[serde(default)]
    pub process: Option<String>,
}

/// What `bambu.presets` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct PresetsResult {
    /// The Bambu Studio program, or `null` when it isn't installed (the presets then come from its
    /// settings folder alone).
    pub studio: Option<String>,
    /// Every printer preset.
    pub printers: Vec<String>,
    /// The printer the rest of the reply is for.
    pub printer: String,
    /// Process presets for the printer.
    pub processes: Vec<String>,
    /// Filament presets for the printer: your own first, then the system ones you show in Bambu Studio.
    pub filaments: Vec<String>,
    pub default_process: Option<String>,
    /// The printer's PLA preset.
    pub default_filament: Option<String>,
    /// Plate types `bambu.slice` takes as `bed_type`.
    pub bed_types: Vec<String>,
    /// The printable area [x0, y0, x1, y1], in mm.
    pub bed: [f64; 4],
    /// The printable height, in mm.
    pub height: f64,
    /// The process `process_values` is for.
    pub process: Option<String>,
    /// That process's main settings by Bambu Studio's key (layer_height, wall_loops,
    /// sparse_infill_density, enable_support, brim_type …), as text.
    pub process_values: std::collections::BTreeMap<String, String>,
    /// Each filament preset's colour ('#rrggbb') and material, in `filaments` order.
    pub filament_info: Vec<FilamentInfo>,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// A filament preset's colour and material.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct FilamentInfo {
    pub color: String,
    pub material: String,
    /// Bambu's filament ID ('GFA00', or 'P…' for your own), what an AMS tray reports.
    pub filament_id: String,
}

/// List the Bambu Lab printers Bambu Studio is connected to on the local network.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PrintersParams {
    /// Listen for the printers' announcements to find their IP addresses (Bambu Studio keeps them
    /// scrambled) and which are on. Default: true.
    #[serde(default)]
    pub discover: Option<bool>,
    /// How long to listen, in seconds, 1 to 10 (clamped). Default: 4. Ignored without `discover`.
    #[serde(default)]
    pub wait: Option<f64>,
}

/// A printer Bambu Studio knows.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct PrinterInfo {
    pub serial: String,
    /// "A1", "H2S" …
    pub name: String,
    /// Bambu's model code ("N2S", "O1S" …).
    pub model: String,
    /// The printer preset for its 0.4 mm nozzle.
    pub preset: String,
    /// "" when unknown.
    pub ip: String,
    /// It answered on the network just now.
    pub online: bool,
    /// The filament IDs Bambu Studio last saw in its AMS, in tray order.
    pub ams: Vec<String>,
}

/// What `bambu.printers` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct PrintersResult {
    pub printers: Vec<PrinterInfo>,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// Slice the shown bodies (or `bodies`) with Bambu Studio's slicer and presets.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SliceParams {
    /// The printer preset (from `bambu.presets`). Default: as `bambu.presets`.
    #[serde(default)]
    pub printer: Option<String>,
    /// The process preset. Default: the printer's default.
    #[serde(default)]
    pub process: Option<String>,
    /// The filament of each slot, slot 1 first: a preset name or {preset, color: '#rrggbb'}, at most
    /// 16. Slots the bodies use beyond the list repeat its last filament (or the printer's PLA), in
    /// the colour of the slot's first body.
    #[serde(default)]
    pub filaments: Option<Vec<Value>>,
    /// The bodies to print. Default: the shown bodies.
    #[serde(default)]
    pub bodies: Option<StringList>,
    /// The filament slot (1 to 16) of each body, by name. Default: 1.
    #[serde(default)]
    pub slots: Option<Map<String, Value>>,
    /// The plate type, one of `bambu.presets`'s `bed_types`. Default: the printer's.
    #[serde(default)]
    pub bed_type: Option<String>,
    /// Let Bambu Studio arrange the bodies. Default: false (they keep their places, centred on the plate).
    #[serde(default)]
    pub arrange: Option<bool>,
    /// Let Bambu Studio turn the bodies to print best. Default: false.
    #[serde(default)]
    pub orient: Option<bool>,
    /// Process settings to change, by Bambu Studio's key: {"sparse_infill_density": "20%"}.
    #[serde(default)]
    pub overrides: Option<Map<String, Value>>,
}

/// A filament the sliced plate uses.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct UsedFilamentInfo {
    pub slot: u32,
    pub material: String,
    pub color: String,
    pub grams: f64,
    pub meters: f64,
}

/// What `bambu.slice` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct SliceResult {
    /// The sliced `.gcode.3mf`, what `bambu.print` sends.
    pub file: String,
    /// The predicted print time.
    pub seconds: f64,
    /// The predicted print time as text ("1h 23m").
    pub time: String,
    /// Filament weight, all slots.
    pub grams: f64,
    pub filaments: Vec<UsedFilamentInfo>,
    /// What was done differently, e.g. sliced with safer settings after the slicer crashed.
    pub note: String,
    pub printer: String,
    pub process: String,
    /// Bambu Studio's picture of the sliced plate (a PNG next to `file`), or `null`.
    pub thumbnail: Option<String>,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// Open a file, or the shown bodies, in Bambu Studio.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OpenInStudioParams {
    /// A .3mf or sliced .gcode.3mf. Default: the shown bodies (or `bodies`), written as a 3MF in
    /// their colours.
    #[serde(default)]
    pub file: Option<String>,
    /// The bodies to send. Default: the shown bodies. Ignored with `file`.
    #[serde(default)]
    pub bodies: Option<StringList>,
    /// The filament slot (1 to 16) of each body, by name. Default: 1. Ignored with `file`.
    #[serde(default)]
    pub slots: Option<Map<String, Value>>,
}

/// What `bambu.open_in_studio` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct OpenInStudioResult {
    /// What Bambu Studio was started with.
    pub file: String,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// A printer by its serial number, with Bambu Studio's access code.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct StatusParams {
    /// The printer's serial number (from `bambu.printers`).
    pub serial: String,
    /// Its IPv4 address. Default: found on the network.
    #[serde(default)]
    pub ip: Option<String>,
    /// A printer preset: each tray then names the filament preset that matches what is loaded.
    #[serde(default)]
    pub printer: Option<String>,
}

/// Pause, resume or stop the printer's job.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ControlParams {
    /// The printer's serial number (from `bambu.printers`).
    pub serial: String,
    /// Its IPv4 address. Default: found on the network.
    #[serde(default)]
    pub ip: Option<String>,
    /// pause, resume or stop.
    pub action: String,
}

/// What `bambu.control` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ControlResult {
    pub done: String,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// An AMS tray or the external spool.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct TrayInfo {
    /// The tray number `bambu.print`'s `mapping` takes: AMS unit × 4 + slot, 254 the external spool.
    pub tray: i64,
    pub label: String,
    pub material: String,
    pub color: String,
    pub filament_id: String,
    /// The filament preset that matches what is loaded, with `printer`; "" otherwise.
    pub preset: String,
}

/// What `bambu.status` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct StatusResult {
    pub state: String,
    pub percent: i64,
    pub minutes_left: i64,
    pub layer: i64,
    pub layers: i64,
    pub job: String,
    /// °C
    pub nozzle: f64,
    /// °C
    pub bed: f64,
    /// The printer's error code, 0 for none.
    pub error: i64,
    /// One line for people.
    pub summary: String,
    pub trays: Vec<TrayInfo>,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// Send a sliced plate to a printer and start printing it.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PrintParams {
    /// The printer's serial number (from `bambu.printers`).
    pub serial: String,
    /// The sliced .gcode.3mf (from `bambu.slice`).
    pub file: String,
    /// Its IPv4 address. Default: found on the network.
    #[serde(default)]
    pub ip: Option<String>,
    /// The tray each filament slot prints from, slot 1 first: a tray number (AMS unit × 4 + slot,
    /// 254 the external spool) or -1 for automatic. Default: automatic, the tray with the same
    /// material and nearest colour.
    #[serde(default)]
    pub mapping: Option<Vec<Value>>,
    /// auto, on or off. Default: Bambu Studio's last choice for this model.
    #[serde(default)]
    pub bed_leveling: Option<String>,
    /// auto, on or off. Default: Bambu Studio's last choice for this model.
    #[serde(default)]
    pub flow_cali: Option<String>,
    /// auto, on or off (H2 printers). Default: Bambu Studio's last choice for this model.
    #[serde(default)]
    pub nozzle_offset_cali: Option<String>,
    /// Record a timelapse. Default: Bambu Studio's last choice for this model.
    #[serde(default)]
    pub timelapse: Option<bool>,
    /// The job's name on the printer. Default: the file's.
    #[serde(default)]
    pub name: Option<String>,
}

/// Where a filament slot prints from.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct SlotTray {
    pub slot: usize,
    /// `null` when no tray matched (the printer then asks).
    pub tray: Option<i64>,
    pub label: Option<String>,
}

/// What `bambu.print` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct PrintResult {
    /// The file's name on the printer.
    pub uploaded: String,
    pub started: bool,
    pub mapping: Vec<SlotTray>,
    pub time: String,
    pub grams: f64,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

#[cfg(test)]
#[path = "bambu_tests.rs"]
mod tests;

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
    /// Bambu Studio's picture of the printer (a PNG), when it's installed.
    pub picture: Option<String>,
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
    /// Added in Fission (Printers window), not only known to Bambu Studio.
    pub added: bool,
    /// Bambu Studio's picture of this printer (a PNG), when it's installed.
    pub picture: Option<String>,
}

/// A filament preset whose main settings to read.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FilamentParams {
    /// A filament preset name (from `bambu.presets`).
    pub preset: String,
}

/// What `bambu.filament` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct FilamentResult {
    /// The preset's main settings by Bambu Studio's key, as text.
    pub values: std::collections::BTreeMap<String, String>,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// Add a printer by its details (or change the one with this serial number).
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SavePrinterParams {
    /// The printer's serial number (on its screen: Settings, Device).
    pub serial: String,
    /// Its LAN access code (on its screen: Settings, Network).
    pub access_code: String,
    /// Its IPv4 address. Default: found on the network each time.
    #[serde(default)]
    pub ip: Option<String>,
    /// A name for it. Default: the model's ("A1", "H2S").
    #[serde(default)]
    pub name: Option<String>,
}

/// Forget a printer added in Fission.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RemovePrinterParams {
    pub serial: String,
}

/// What `bambu.save_printer` and `bambu.remove_printer` return.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct SavedPrintersResult {
    /// How many printers Fission keeps.
    pub saved: usize,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// Connect to a printer with these details, without saving them.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TestPrinterParams {
    /// Its IPv4 address.
    pub ip: String,
    pub serial: String,
    pub access_code: String,
}

/// What `bambu.test_printer` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct TestPrinterResult {
    /// The printer's status, in a line.
    pub summary: String,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// A printer announcing itself on the network.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct FoundPrinter {
    pub ip: String,
    pub serial: String,
    pub model: String,
    pub name: String,
}

/// What `bambu.discover` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct DiscoverResult {
    pub found: Vec<FoundPrinter>,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
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
    /// The filament of each slot, slot 1 first: a preset name or {preset, color: '#rrggbb',
    /// overrides: {setting: text}} (the settings `bambu.filament` shows), at most 16. Slots the bodies use beyond the list repeat its last filament (or the printer's PLA), in
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
    /// Where bodies sit on the plate, by name: {x, y, rotation?, scale?, orient?}, the middle of the
    /// body's footprint in mm in the plate's coordinates; `orient` (a rotation, 9 numbers row by row)
    /// then `scale` then `rotation` degrees about Z, around the body's middle. Default: the design's
    /// layout, centred on the plate.
    #[serde(default)]
    pub placements: Option<Map<String, Value>>,
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

/// Run a Fusion 360 Python script.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FusionRunParams {
    /// The script: a .py with `def run(context):` that imports adsk.core and adsk.fusion.
    pub path: String,
    /// Seconds it may run, 5 to 3600. Default: 300.
    #[serde(default)]
    pub timeout: Option<f64>,
}

/// What `fusion.run_script` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct FusionRunResult {
    /// How many steps the script made.
    pub commands: usize,
    /// Its message boxes, in order.
    pub messages: Vec<String>,
    /// What it printed.
    pub log: String,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

/// Open a Fusion design saved with the Fission Export script (a .ffx).
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FfxOpenParams {
    /// The .ffx file.
    pub path: String,
}

/// What `fusion.open_ffx` returns.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct FfxOpenResult {
    /// The Fusion design's name.
    pub design: String,
    /// The bodies brought in.
    pub bodies: Vec<String>,
    /// The parameters added (your own, from Fusion's parameter list).
    pub parameters: Vec<String>,
    /// How many steps the design's Fusion timeline had.
    pub timeline: usize,
    /// Rebuilt as a Fission timeline (false: Fusion's finished bodies came in as they are).
    pub rebuilt: bool,
    /// What happened along the way (a font that wasn't there, why the timeline wasn't rebuilt).
    pub messages: Vec<String>,
    /// In the warn parameter-check mode: what the call's parameters got wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_warnings: Vec<String>,
}

#[cfg(test)]
#[path = "bambu_tests.rs"]
mod tests;

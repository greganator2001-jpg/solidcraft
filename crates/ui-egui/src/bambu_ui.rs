//! Fission's PRINT workspace, laid out like FissionCAD's BAMBU workspace (and Bambu Studio): the
//! design becomes a print plate. A sidebar holds Printer (the printers Bambu Studio is connected
//! to, the printer preset, the plate type, live status), Filament (one chip per slot, Sync from the
//! AMS), Objects (each body's filament) and Process (the preset and its main settings), with Slice
//! plate and Print plate at the bottom. Prepare shows the model; Preview shows the sliced plate with
//! its time and filament. Print plate asks which AMS tray feeds each filament, as Bambu Studio's
//! Send print dialog does. Everything slow (the slicer, the network) runs on a worker thread and
//! answers on a channel, so the window never waits.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::Color32;
use serde_json::{Value, json};

use crate::SolveApp;
use crate::i18n::{format, tr};
use crate::ui_kit;

/// What a worker answers: which job it was and its reply.
type Answer = (Job, Result<Value, String>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    Printers,
    Presets,
    Slice,
    Print,
    Status,
    Sync,
    Control,
    Discover,
    SavePrinter,
    TestPrinter,
    Filament,
}

/// The Bring to plate window: which shown bodies go on the plate, on which filament, and each
/// filament's colour (as FissionCAD asks when the slicer opens).
struct Bring {
    /// (body, on the plate, filament slot from 0, size).
    rows: Vec<(String, bool, usize, [f64; 3])>,
    colours: Vec<[u8; 3]>,
    ask: bool,
}

/// What a click asks for (done after drawing, when `app` is free again).
#[derive(Clone, Copy)]
enum Act {
    Slice,
    OpenSend,
    Send,
    Studio,
    Sync,
    Refresh,
    Control(&'static str),
    Leave,
    Bring,
    RemoveFromPlate,
    Export,
    Printers,
    EditFilament(usize),
}

/// The Printers window: add a printer by its details, find printers on the network, test one.
#[derive(Default)]
struct PrintersForm {
    serial: String,
    ip: String,
    code: String,
    name: String,
    found: Vec<Value>,
    note: Option<(String, bool)>,
}

const SEND_CHOICES: [&str; 3] = ["auto", "on", "off"];
/// How often the chosen printer's status is read while the workspace is open.
const STATUS_EVERY: f64 = 8.0;
const MAX_SLOTS: usize = 16;

/// A filament slot: its preset (index into the presets' filament list) and colour.
#[derive(Clone, Debug, Default)]
struct Slot {
    preset: usize,
    color: String,
    /// Filament settings changed for this print (Bambu Studio's keys, as text).
    overrides: BTreeMap<String, String>,
}

/// The Filament window: one slot's main settings, the preset's and the changed ones.
struct FilamentForm {
    slot: usize,
    base: BTreeMap<String, String>,
    edits: BTreeMap<String, String>,
}

/// The filament settings the Filament window shows, with their labels.
const FILAMENT_FIELDS: [(&str, &str); 9] = [
    ("nozzle_temperature", "fission.fil.nozzle"),
    ("nozzle_temperature_initial_layer", "fission.fil.nozzle_first"),
    ("textured_plate_temp", "fission.fil.bed_textured"),
    ("hot_plate_temp", "fission.fil.bed_smooth"),
    ("cool_plate_temp", "fission.fil.bed_cool"),
    ("fan_max_speed", "fission.fil.fan_max"),
    ("fan_min_speed", "fission.fil.fan_min"),
    ("filament_flow_ratio", "fission.fil.flow"),
    ("filament_max_volumetric_speed", "fission.fil.volumetric"),
];

/// The process settings shown, as text, by Bambu Studio's key.
#[derive(Clone, Debug, Default)]
struct Process {
    /// The preset's own values.
    base: BTreeMap<String, String>,
    /// What the user changed.
    edits: BTreeMap<String, String>,
}

impl Process {
    fn get(&self, k: &str) -> String {
        self.edits.get(k).or_else(|| self.base.get(k)).cloned().unwrap_or_default()
    }
    fn set(&mut self, k: &str, v: String) {
        if self.base.get(k) == Some(&v) {
            self.edits.remove(k);
        } else {
            self.edits.insert(k.to_string(), v);
        }
    }
}

#[derive(Default)]
pub struct State {
    /// A ribbon button's action, done when the sidebar next draws.
    pending: Option<Act>,
    /// The plate in the 3D view: the parts where they'll print, the sliced toolpaths.
    pub(crate) plate: crate::bambu_plate::Plate,
    /// The Filament window, while it's open.
    filament_form: Option<FilamentForm>,
    /// The Printers window, while it's open.
    printers_form: Option<PrintersForm>,
    /// Bambu Studio's picture of the chosen printer, and which file it came from.
    printer_picture: Option<(String, egui::TextureHandle)>,
    /// The Bring to plate window, while it's open.
    bring: Option<Bring>,
    /// Bring to plate has been asked since the tab was entered.
    asked: bool,
    /// Let Bambu Studio turn the parts to print best when slicing.
    orient: bool,
    /// Don't ask Bring to plate on entering the tab (its "Ask me every time" unticked).
    dont_ask: bool,
    /// The design's camera while the tab shows the plate (put back on leaving).
    design_camera: Option<(solidcraft_engine::render::Camera, bool)>,
    started: bool,
    preview: bool,
    channel: Option<(Sender<Answer>, Receiver<Answer>)>,
    busy: Vec<Job>,
    presets: Value,
    printers: Vec<Value>,
    /// Index into `presets.printers`, `presets.processes`, `presets.bed_types`.
    printer: usize,
    process: usize,
    bed: usize,
    settings: Process,
    slots: Vec<Slot>,
    /// Each body's slot, 1-based.
    body_slot: BTreeMap<String, usize>,
    /// Index into `printers`: where Print plate sends to.
    target: usize,
    status: Option<Value>,
    status_at: f64,
    /// The last slice (`bambu.slice`'s reply) and its picture.
    sliced: Option<Value>,
    picture: Option<egui::TextureHandle>,
    /// The Send print dialog: the tray chosen for each filament slot of the sliced plate.
    send: Option<Vec<usize>>,
    bed_leveling: usize,
    flow_cali: usize,
    timelapse: bool,
    message: Option<(String, bool)>,
    side: String,
    width: f32,
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn number(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// '#rrggbb' (or '#rrggbbaa') → colour; the theme's dim text colour when it isn't one.
fn colour(hex: &str) -> Color32 {
    crate::bambu_plate::hex(hex)
}

impl State {
    fn spawn(&mut self, job: Job, work: impl FnOnce() -> Result<Value, String> + Send + 'static) {
        let tx = self.channel.get_or_insert_with(channel).0.clone();
        self.busy.push(job);
        std::thread::spawn(move || {
            let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("Fission hit an internal error doing that (please report it)".into()));
            tx.send((job, answer)).ok();
        });
    }

    fn detached(&mut self, job: Job, id: &'static str, params: Value) {
        self.spawn(job, move || solidcraft_engine::cmd::bambu::run_detached(id, &params).map_err(|e| e.to_string()));
    }

    fn is_busy(&self, job: Job) -> bool {
        self.busy.contains(&job)
    }

    fn printer_name(&self) -> Option<String> {
        names(&self.presets, "printers").get(self.printer).cloned()
    }

    fn process_name(&self) -> Option<String> {
        names(&self.presets, "processes").get(self.process).cloned()
    }

    fn load_presets(&mut self, process: Option<String>) {
        let mut params = json!({});
        if let Some(p) = self.printer_name() {
            params["printer"] = json!(p);
        }
        if let Some(p) = process {
            params["process"] = json!(p);
        }
        self.detached(Job::Presets, "bambu.presets", params);
    }

    fn target_printer(&self) -> Option<&Value> {
        self.printers.get(self.target)
    }

    fn read_status(&mut self, job: Job) {
        let Some(p) = self.target_printer() else { return };
        let params = json!({"serial": text(p, "serial"), "ip": text(p, "ip"), "printer": self.printer_name()});
        self.detached(job, "bambu.status", params);
    }

    fn filament_names(&self) -> Vec<String> {
        names(&self.presets, "filaments")
    }

    fn filament_info(&self, i: usize) -> Value {
        self.presets.get("filament_info").and_then(|a| a.get(i)).cloned().unwrap_or(Value::Null)
    }
}

/// The ribbon tab that is the workspace, as FissionCAD's BAMBU tab.
pub const TAB: &str = "BAMBU";

/// Enter the workspace (the BAMBU tab; MODEL > PRINT > Slice Plate).
pub fn open(app: &mut SolveApp) {
    app.ui.tab = TAB.into();
    start(&mut app.bambu);
}

/// The first time the workspace shows: find the printers, read the presets.
fn start(s: &mut State) {
    if !s.started {
        s.started = true;
        s.side = "left".into();
        // wide enough for Bambu Studio's long preset names (the card caps it to the window)
        s.width = crate::theme::Tokens::get().form_card_width * 1.75;
        s.detached(Job::Printers, "bambu.printers", json!({"discover": true, "wait": 4}));
        s.load_presets(None);
    }
}

/// Whether the workspace is showing (its tab is selected).
pub fn active(app: &SolveApp) -> bool {
    app.ui.tab == TAB
}

/// A BAMBU ribbon button while the workspace shows: Slice Plate slices, Print Plate opens the
/// Send print dialog. False when it isn't one of those (or the workspace isn't showing).
pub fn ribbon(app: &mut SolveApp, id: &str) -> bool {
    if !active(app) {
        return false;
    }
    app.bambu.pending = match id {
        "bambu.slice" => Some(Act::Slice),
        "bambu.print" => Some(Act::OpenSend),
        _ => return false,
    };
    true
}

/// Take the workers' answers (every frame, before drawing).
pub fn tick(app: &mut SolveApp, ctx: &egui::Context) {
    if !active(app) {
        app.bambu.plate.fitted = false;
        app.bambu.asked = false;
        if let Some((cam, perspective)) = app.bambu.design_camera.take() {
            app.cam = cam;
            app.ui.perspective = perspective;
        }
    } else {
        app.bambu.plate.tick();
        if app.bambu.plate.loading() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        if app.bambu.plate.keys(ctx) {
            app.bambu.sliced = None;
        }
        // drag parts on the plate in Prepare
        if !app.bambu.preview
            && let Some(rect) = app.viewport.rect
        {
            let free = ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| !crate::shell::covered(app, p))
                && app.bambu.bring.is_none()
                && app.bambu.send.is_none();
            let cam = app.cam;
            if app.bambu.plate.drag(ctx, rect, &cam, free) {
                app.bambu.sliced = None;
            }
        }
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::I)) {
            app.bambu.pending = Some(Act::Bring);
        }
    }
    let answers: Vec<Answer> = app.bambu.channel.as_ref().map(|(_, rx)| rx.try_iter().collect()).unwrap_or_default();
    if !app.bambu.busy.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    for (job, answer) in answers {
        if let Some(i) = app.bambu.busy.iter().position(|j| *j == job) {
            app.bambu.busy.remove(i);
        }
        match (job, answer) {
            // a status read in the background that fails just shows as not connected
            (Job::Status, Err(_)) => app.bambu.status = None,
            (Job::Discover | Job::SavePrinter | Job::TestPrinter, Err(e)) if app.bambu.printers_form.is_some() => {
                if let Some(f) = app.bambu.printers_form.as_mut() {
                    f.note = Some((e, true));
                }
            }
            (_, Err(e)) => {
                app.bambu.message = Some((e.clone(), true));
                app.set_status(e, true);
            }
            (Job::Printers, Ok(v)) => {
                let s = &mut app.bambu;
                s.printers = v.get("printers").and_then(Value::as_array).cloned().unwrap_or_default();
                // Print goes to the printer whose preset is chosen, when Bambu Studio knows it
                let preset = s.printer_name().unwrap_or_default();
                s.target = s.printers.iter().position(|p| text(p, "preset") == preset).unwrap_or(0);
                s.status_at = 0.0;
            }
            (Job::Presets, Ok(v)) => took_presets(&mut app.bambu, v),
            (Job::Slice, Ok(v)) => {
                let msg = format("fission.bambu.sliced", &[("time", text(&v, "time")), ("grams", &format!("{:.1}", number(&v, "grams")))]);
                app.bambu.picture = picture(ctx, text(&v, "thumbnail"));
                app.bambu.plate.load_toolpaths(text(&v, "file"));
                app.bambu.sliced = Some(v);
                app.bambu.preview = true;
                app.bambu.message = Some((msg.clone(), false));
                app.set_status(msg, false);
            }
            (Job::Print, Ok(v)) => {
                let msg = format("fission.bambu.printing", &[("file", text(&v, "uploaded"))]);
                app.bambu.message = Some((msg.clone(), false));
                app.bambu.status_at = 0.0;
                app.set_status(msg, false);
            }
            (Job::Status, Ok(v)) => app.bambu.status = Some(v),
            (Job::Sync, Ok(v)) => {
                synced(&mut app.bambu, &v);
                app.bambu.status = Some(v);
            }
            (Job::Control, Ok(_)) => app.bambu.status_at = 0.0,
            (Job::Filament, Ok(v)) => {
                if let Some(f) = app.bambu.filament_form.as_mut() {
                    f.base = v.get("values").and_then(Value::as_object).map(|o| o.iter().map(|(k, v)| (k.clone(), text_of(v))).collect()).unwrap_or_default();
                }
            }
            (Job::Discover, Ok(v)) => {
                if let Some(f) = app.bambu.printers_form.as_mut() {
                    f.found = v.get("found").and_then(Value::as_array).cloned().unwrap_or_default();
                    let n = f.found.len().to_string();
                    f.note = Some((format("fission.printers.found", &[("n", &n)]), false));
                }
            }
            (Job::SavePrinter, Ok(_)) => {
                if let Some(f) = app.bambu.printers_form.as_mut() {
                    f.note = Some((tr("fission.printers.saved").to_string(), false));
                }
                app.bambu.detached(Job::Printers, "bambu.printers", json!({"discover": true, "wait": 3}));
            }
            (Job::TestPrinter, Ok(v)) => {
                if let Some(f) = app.bambu.printers_form.as_mut() {
                    f.note = Some((format("fission.printers.answered", &[("status", text(&v, "summary"))]), false));
                }
            }
        }
    }
    // keep the chosen printer's status fresh while the workspace is open
    let now = ctx.input(|i| i.time);
    let showing = active(app);
    let s = &mut app.bambu;
    if showing && s.target_printer().is_some_and(|p| p.get("online").and_then(Value::as_bool).unwrap_or(false)) {
        if !s.is_busy(Job::Status) && !s.is_busy(Job::Sync) && (s.status_at == 0.0 || now - s.status_at > STATUS_EVERY) {
            s.status_at = now;
            s.read_status(Job::Status);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }
}

fn took_presets(s: &mut State, v: Value) {
    let printer = text(&v, "printer").to_string();
    let printer_changed = s.printer_name().as_deref() != Some(printer.as_str()) || s.presets.is_null();
    s.printer = names(&v, "printers").iter().position(|p| *p == printer).unwrap_or(0);
    let processes = names(&v, "processes");
    let process = v.get("process").and_then(Value::as_str).unwrap_or("");
    s.process = processes.iter().position(|p| p == process).unwrap_or(0);
    let base: BTreeMap<String, String> =
        v.get("process_values").and_then(Value::as_object).map(|o| o.iter().map(|(k, v)| (k.clone(), text_of(v))).collect()).unwrap_or_default();
    s.settings = Process { base, edits: BTreeMap::new() };
    if printer_changed {
        // the filament lists differ per printer: start over with its PLA in each slot's colour
        let filaments = names(&v, "filaments");
        let pla = v.get("default_filament").and_then(Value::as_str).and_then(|d| filaments.iter().position(|f| f == d)).unwrap_or(0);
        let colour_of = |i: usize| v.get("filament_info").and_then(|a| a.get(i)).map(|f| text(f, "color").to_string()).unwrap_or_default();
        let n = s.slots.len().max(1);
        s.slots = (0..n)
            .map(|k| Slot { preset: pla, color: s.slots.get(k).map(|o| o.color.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| colour_of(pla)), ..Slot::default() })
            .collect();
        s.bed = 0;
        s.sliced = None;
    }
    s.presets = v;
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Sync from the AMS: one slot per loaded tray, with its colour and matching preset.
fn synced(s: &mut State, status: &Value) {
    let filaments = s.filament_names();
    let trays: Vec<&Value> =
        status.get("trays").and_then(Value::as_array).map(|a| a.iter().filter(|t| !text(t, "material").is_empty()).collect()).unwrap_or_default();
    if trays.is_empty() {
        s.message = Some((tr("fission.bambu.ams_empty").to_string(), true));
        return;
    }
    s.slots = trays
        .iter()
        .take(MAX_SLOTS)
        .map(|t| Slot { preset: filaments.iter().position(|f| f == text(t, "preset")).unwrap_or(0), color: text(t, "color").to_string(), ..Slot::default() })
        .collect();
    s.sliced = None;
    s.message = Some((format("fission.bambu.synced", &[("n", &s.slots.len().to_string())]), false));
}

fn picture(ctx: &egui::Context, path: &str) -> Option<egui::TextureHandle> {
    picture_named(ctx, path, "fission_plate_picture")
}

fn picture_named(ctx: &egui::Context, path: &str, name: &str) -> Option<egui::TextureHandle> {
    if path.is_empty() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let colour = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture(name, colour, egui::TextureOptions::LINEAR))
}

/// The printer's plate [x0, y0, x1, y1] (a 256 mm plate until the presets are read).
fn bed(s: &State) -> (f64, f64, f64, f64) {
    let b = |i: usize| s.presets.get("bed").and_then(|v| v.get(i)).and_then(Value::as_f64);
    match (b(0), b(1), b(2), b(3)) {
        (Some(x0), Some(y0), Some(x1), Some(y1)) if x1 > x0 && y1 > y0 => (x0, y0, x1, y1),
        _ => (0.0, 0.0, 256.0, 256.0),
    }
}

fn height(s: &State) -> f64 {
    s.presets.get("height").and_then(Value::as_f64).filter(|h| *h > 0.0).unwrap_or(256.0)
}

/// Each body's filament colour (its slot's).
fn body_colours(s: &State, bodies: &[(String, String)]) -> BTreeMap<String, String> {
    bodies
        .iter()
        .map(|(b, shown)| {
            let slot = s.body_slot.get(b).copied().unwrap_or(1);
            let c = s.slots.get(slot.saturating_sub(1)).map(|sl| sl.color.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| shown.clone());
            (b.clone(), c)
        })
        .collect()
}

/// The BAMBU tab's 3D view: the plate with the parts (Prepare) or the toolpaths (Preview), in place
/// of the design's bodies. Called by the preview each frame while the tab shows.
pub fn plate_scene(app: &mut SolveApp) {
    let (bed, height) = (bed(&app.bambu), height(&app.bambu));
    app.bambu.plate.refresh(&app.session, bed);
    // a plate changed since it was sliced shows no old toolpaths
    if app.bambu.sliced.is_none() && app.bambu.plate.has_toolpaths() {
        app.bambu.plate.forget_toolpaths();
    }
    let bodies = solidcraft_engine::cmd::bambu::plate_bodies(&app.session);
    let colours = body_colours(&app.bambu, &bodies);
    let preview = app.bambu.preview;
    let filaments: Vec<String> = app.bambu.slots.iter().map(|sl| sl.color.clone()).collect();
    let mut key = app.bambu.plate.scene_key(preview, bed, &colours);
    for f in &filaments {
        key = key.rotate_left(5) ^ f.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
    }
    let key = key ^ 0xF155_1011;
    let replaced: Vec<String> = bodies.iter().map(|(b, _)| b.clone()).collect();
    let bounds = crate::bambu_plate::Plate::bounds(bed, height);
    let plate = &app.bambu.plate;
    app.preview.show_plate(key, replaced, bounds, || plate.scene(preview, bed, height, &colours, &filaments));
    if !app.bambu.plate.fitted {
        app.bambu.plate.fitted = true;
        if app.bambu.design_camera.is_none() {
            app.bambu.design_camera = Some((app.cam, app.ui.perspective));
        }
        app.fit_bounds(bounds);
    }
}

fn slice_params(app: &SolveApp, bodies: &[(String, String)]) -> Value {
    let s = &app.bambu;
    let filaments = s.filament_names();
    let chosen: Vec<Value> =
        s.slots.iter().map(|sl| json!({"preset": filaments.get(sl.preset).cloned().unwrap_or_default(), "color": sl.color, "overrides": sl.overrides})).collect();
    let on_plate = s.plate.on_plate();
    let bodies: Vec<(String, String)> = bodies.iter().filter(|(b, _)| on_plate.contains(b)).cloned().collect();
    let slots: serde_json::Map<String, Value> =
        bodies.iter().map(|(b, _)| (b.clone(), json!(s.body_slot.get(b).copied().unwrap_or(1).clamp(1, s.slots.len().max(1))))).collect();
    let overrides: serde_json::Map<String, Value> = s.settings.edits.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let placements: serde_json::Map<String, Value> = s
        .plate
        .places
        .iter()
        .filter(|(b, _)| on_plate.contains(b))
        .map(|(b, at)| (b.clone(), json!({"x": at.x, "y": at.y, "rotation": at.turn, "scale": at.scale, "orient": at.orient.iter().flatten().collect::<Vec<_>>()})))
        .collect();
    let mut p = json!({"printer": s.printer_name(), "process": s.process_name(), "filaments": chosen, "slots": slots, "overrides": overrides, "placements": placements, "bodies": on_plate, "orient": s.orient});
    if let Some(bed) = names(&s.presets, "bed_types").get(s.bed) {
        p["bed_type"] = json!(bed);
    }
    p
}

fn slice(app: &mut SolveApp, bodies: &[(String, String)]) {
    let params = slice_params(app, bodies);
    // the meshes are taken here, on the window thread; the slicer runs on a worker
    match solidcraft_engine::cmd::bambu::prepare_slice(&app.session, &params) {
        Ok(plan) => {
            app.bambu.sliced = None;
            app.bambu.picture = None;
            app.bambu.message = Some((tr("fission.bambu.slicing").to_string(), false));
            app.bambu.spawn(Job::Slice, move || plan.run().map_err(|e| e.to_string()));
        }
        Err(e) => app.bambu.message = Some((e.to_string(), true)),
    }
}

/// The trays of the chosen printer, as its last status read them.
fn trays(s: &State) -> Vec<Value> {
    s.status.as_ref().and_then(|v| v.get("trays")).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// Open the Send print dialog: each filament of the sliced plate from the tray with the same
/// filament, else the same material and nearest colour.
fn open_send(s: &mut State) {
    let Some(sliced) = &s.sliced else { return };
    let trays = trays(s);
    let used: Vec<Value> = sliced.get("filaments").and_then(Value::as_array).cloned().unwrap_or_default();
    let slots = used.iter().map(|u| u.get("slot").and_then(Value::as_u64).unwrap_or(1) as usize).max().unwrap_or(1);
    let filaments = s.filament_names();
    let pick = |slot: usize| -> usize {
        let wanted = s.slots.get(slot.saturating_sub(1));
        let preset = wanted.and_then(|w| filaments.get(w.preset)).cloned().unwrap_or_default();
        let colour_of = |c: &str| {
            let (a, b) = (colour(c), colour(wanted.map(|w| w.color.as_str()).unwrap_or("")));
            let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
            d(a.r(), b.r()) + d(a.g(), b.g()) + d(a.b(), b.b())
        };
        let material = used.iter().find(|u| u.get("slot").and_then(Value::as_u64) == Some(slot as u64)).map(|u| text(u, "material")).unwrap_or("");
        trays
            .iter()
            .position(|t| !preset.is_empty() && text(t, "preset") == preset && colour_of(text(t, "color")) < 2000)
            .or_else(|| {
                trays
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| text(t, "material").eq_ignore_ascii_case(material))
                    .min_by_key(|(_, t)| colour_of(text(t, "color")))
                    .map(|(i, _)| i)
            })
            .map_or(0, |i| i + 1)
    };
    s.send = Some((1..=slots).map(pick).collect());
}

fn send(app: &mut SolveApp) {
    let s = &app.bambu;
    let (Some(sliced), Some(printer), Some(choice)) = (s.sliced.as_ref(), s.target_printer(), s.send.as_ref()) else { return };
    let trays = trays(s);
    // 0 in the dialog is "let the printer decide": -1
    let mapping: Vec<i64> = choice
        .iter()
        .map(|c| c.checked_sub(1).and_then(|i| trays.get(i)).and_then(|t| t.get("tray")).and_then(Value::as_i64).unwrap_or(-1))
        .collect();
    let params = json!({
        "serial": text(printer, "serial"),
        "ip": text(printer, "ip"),
        "file": text(sliced, "file"),
        "mapping": mapping,
        "bed_leveling": SEND_CHOICES.get(s.bed_leveling),
        "flow_cali": SEND_CHOICES.get(s.flow_cali),
        "timelapse": s.timelapse,
        "name": app.session.doc.name,
    });
    app.bambu.send = None;
    app.bambu.message = Some((tr("fission.bambu.sending").to_string(), false));
    app.bambu.detached(Job::Print, "bambu.print", params);
}

fn send_to_studio(app: &mut SolveApp, bodies: &[(String, String)]) {
    let params = match app.bambu.sliced.as_ref() {
        Some(v) => json!({"file": text(v, "file")}),
        None => json!({"slots": slice_params(app, bodies)["slots"]}),
    };
    match app.run("bambu.open_in_studio", params) {
        Ok(_) => app.bambu.message = Some((tr("fission.bambu.opened").to_string(), false)),
        Err(e) => app.bambu.message = Some((e, true)),
    }
}

/// A slot's chip: its colour with its number on it.
fn chip(ui: &mut egui::Ui, hex: &str, n: usize) -> egui::Response {
    let t = crate::theme::Tokens::get();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(t.control_height * 1.3, t.control_height * 0.8), egui::Sense::click());
    let fill = colour(hex);
    ui.painter().rect_filled(rect, t.radius_sm, fill);
    ui.painter().rect_stroke(rect, t.radius_sm, egui::Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    let ink = if crate::theme::contrast_ratio(t.text, fill) >= crate::theme::contrast_ratio(t.panel, fill) { t.text } else { t.panel };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, n.to_string(), egui::FontId::proportional(t.font_label), ink);
    response
}

fn section_title(ui: &mut egui::Ui, key: &str, id: &str) {
    ui_kit::divider(ui, &format!("{id}_rule"));
    ui_kit::text(ui, tr(key), ui_kit::Text::Heading(id));
}

/// The sidebar: Printer, Filament, Objects, Process; Slice plate and Print plate at its foot.
fn sidebar(app: &mut SolveApp, ui: &mut egui::Ui, bodies: &[(String, String)], act: &mut Option<Act>) {
    let s = &mut app.bambu;
    let mut mode = usize::from(s.preview);
    let modes = [tr("fission.bambu.prepare"), tr("fission.bambu.preview")];
    if ui_kit::segmented(ui, &mut mode, &modes, Default::default()) {
        s.preview = mode == 1;
    }
    if s.presets.is_null() {
        let wait = if s.is_busy(Job::Presets) { tr("fission.bambu.loading") } else { tr("fission.bambu.no_studio") };
        ui_kit::text(ui, wait, ui_kit::Text::Note("fission_bambu_wait"));
        if let Some((m, true)) = &s.message {
            ui_kit::text(ui, m, ui_kit::Text::Error("fission_bambu_error"));
        }
        return;
    }
    ui_kit::Panel::pinned_footer(ui, "fission_print_footer", 3, |ui| {
        let slicing = s.is_busy(Job::Slice);
        ui.horizontal(|ui| {
            let label = if slicing { tr("fission.bambu.slicing_short") } else { tr("fission.bambu.slice") };
            if ui_kit::button(ui, label, if slicing || bodies.is_empty() { ui_kit::Button::Enabled(false) } else { ui_kit::Button::Primary })
                .clicked()
            {
                *act = Some(Act::Slice);
            }
            let ready = s.sliced.is_some() && s.target_printer().is_some() && !s.is_busy(Job::Print) && !slicing;
            if ui_kit::button(ui, tr("fission.bambu.print"), if ready { ui_kit::Button::Primary } else { ui_kit::Button::Enabled(false) }).clicked() {
                *act = Some(Act::OpenSend);
            }
        });
        ui.horizontal(|ui| {
            if ui_kit::button(ui, tr("fission.bambu.studio"), ui_kit::Button::Small).clicked() {
                *act = Some(Act::Studio);
            }
            if ui_kit::button(ui, tr("fission.plate.export"), ui_kit::Button::Enabled(s.sliced.is_some())).clicked() {
                *act = Some(Act::Export);
            }
            if ui_kit::button(ui, tr("fission.bambu.back"), ui_kit::Button::Small).clicked() {
                *act = Some(Act::Leave);
            }
        });
    });
    egui::ScrollArea::vertical().id_salt("fission_print_side").auto_shrink([false, false]).show(ui, |ui| {
        // ---------------- Printer
        section_title(ui, "fission.bambu.printer_section", "fission_printer");
        // Bambu Studio's picture of the printer, and the Printers window
        let want = text(&s.presets, "picture").to_string();
        if !want.is_empty() && s.printer_picture.as_ref().is_none_or(|(p, _)| *p != want) {
            s.printer_picture = picture_named(ui.ctx(), &want, "fission_printer_picture").map(|t| (want.clone(), t));
        }
        ui.horizontal(|ui| {
            if let Some((_, tex)) = &s.printer_picture {
                ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(72.0, 72.0)));
            }
            if ui_kit::button(ui, tr("fission.printers.open"), ui_kit::Button::Small).clicked() {
                *act = Some(Act::Printers);
            }
        });
        let labels: Vec<String> = s
            .printers
            .iter()
            .map(|p| {
                let on = p.get("online").and_then(Value::as_bool).unwrap_or(false);
                let state = if on { tr("fission.bambu.online") } else { tr("fission.bambu.offline") };
                format!("{} ({state})", text(p, "name"))
            })
            .collect();
        if labels.is_empty() {
            let note = if s.is_busy(Job::Printers) { tr("fission.bambu.finding") } else { tr("fission.bambu.no_printers") };
            ui_kit::text(ui, note, ui_kit::Text::Note("fission_bambu_printers"));
        } else {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let before = s.target;
            ui_kit::choice_row(ui, tr("fission.bambu.send_to"), "fission_bambu_target", &mut s.target, &refs);
            if s.target != before {
                s.status = None;
                s.status_at = 0.0;
                // the preset follows the printer
                let preset = s.target_printer().map(|p| text(p, "preset").to_string()).unwrap_or_default();
                if let Some(i) = names(&s.presets, "printers").iter().position(|p| *p == preset)
                    && i != s.printer
                {
                    s.printer = i;
                    s.load_presets(None);
                }
            }
        }
        let status_line = match &s.status {
            Some(v) => text(v, "summary").to_string(),
            None if s.is_busy(Job::Status) => tr("fission.bambu.connecting").to_string(),
            None => tr("fission.bambu.not_connected").to_string(),
        };
        ui.horizontal(|ui| {
            ui_kit::text(ui, &status_line, ui_kit::Text::Note("fission_bambu_status"));
            let connected = s.status.is_some();
            let label = if connected { tr("fission.bambu.refresh") } else { tr("fission.printers.connect") };
            if ui_kit::button(ui, label, ui_kit::Button::Small).clicked() {
                *act = Some(Act::Refresh);
            }
        });
        let printers = names(&s.presets, "printers");
        let refs: Vec<&str> = printers.iter().map(String::as_str).collect();
        let before = s.printer;
        ui_kit::choice_row(ui, tr("fission.bambu.printer"), "fission_bambu_printer", &mut s.printer, &refs);
        if s.printer != before {
            s.load_presets(None);
        }
        let beds = names(&s.presets, "bed_types");
        let refs: Vec<&str> = beds.iter().map(String::as_str).collect();
        if ui_kit::choice_row(ui, tr("fission.bambu.plate"), "fission_bambu_plate", &mut s.bed, &refs).changed() {
            s.sliced = None;
        }

        // ---------------- Filament
        section_title(ui, "fission.bambu.filaments", "fission_filaments");
        let syncing = s.is_busy(Job::Sync);
        let can_sync = s.target_printer().is_some() && !syncing;
        if ui_kit::button(ui, if syncing { tr("fission.bambu.syncing") } else { tr("fission.bambu.sync") }, ui_kit::Button::Enabled(can_sync))
            .clicked()
        {
            *act = Some(Act::Sync);
        }
        let filaments = s.filament_names();
        let refs: Vec<&str> = filaments.iter().map(String::as_str).collect();
        if s.slots.is_empty() {
            s.slots.push(Slot::default());
        }
        let mut remove = None;
        for i in 0..s.slots.len() {
            let before = s.slots.get(i).map(|sl| sl.preset);
            let colour_now = s.slots.get(i).map(|sl| sl.color.clone()).unwrap_or_default();
            ui.horizontal(|ui| {
                chip(ui, &colour_now, i + 1);
                if let Some(slot) = s.slots.get_mut(i) {
                    ui_kit::dropdown(ui, &format!("fission_bambu_slot_{i}"), &mut slot.preset, &refs);
                }
                let edited = s.slots.get(i).is_some_and(|sl| !sl.overrides.is_empty());
                if ui_kit::button(ui, "✎", if edited { ui_kit::Button::SmallSelected } else { ui_kit::Button::Small })
                    .on_hover_text(tr("fission.fil.edit"))
                    .clicked()
                {
                    *act = Some(Act::EditFilament(i));
                }
                if i > 0
                    && i + 1 == s.slots.len()
                    && ui_kit::button(ui, "−", ui_kit::Button::Small).on_hover_text(tr("fission.bambu.remove_slot")).clicked()
                {
                    remove = Some(i);
                }
            });
            // a new preset brings its own colour
            let after = s.slots.get(i).map(|sl| sl.preset);
            if before != after
                && let Some(p) = after
            {
                let c = text(&s.filament_info(p), "color").to_string();
                if let Some(slot) = s.slots.get_mut(i) {
                    slot.color = c;
                    slot.overrides.clear();
                }
                s.sliced = None;
            }
        }
        if let Some(i) = remove {
            s.slots.truncate(i);
            for slot in s.body_slot.values_mut() {
                *slot = (*slot).min(i);
            }
            s.sliced = None;
        }
        if s.slots.len() < MAX_SLOTS && ui_kit::button(ui, tr("fission.bambu.add_slot"), ui_kit::Button::Small).clicked() {
            let last = s.slots.last().cloned().unwrap_or_default();
            s.slots.push(last);
        }

        // ---------------- Objects
        section_title(ui, "fission.bambu.objects", "fission_objects");
        if bodies.is_empty() {
            ui_kit::text(ui, tr("fission.bambu.no_bodies"), ui_kit::Text::Note("fission_bambu_no_bodies"));
        }
        let slot_names: Vec<String> = (1..=s.slots.len()).map(|n| format("fission.bambu.slot", &[("n", &n.to_string())])).collect();
        let refs: Vec<&str> = slot_names.iter().map(String::as_str).collect();
        let on_plate = s.plate.on_plate();
        for (body, _) in bodies.iter().filter(|(b, _)| on_plate.contains(b)) {
            let mut i = s.body_slot.get(body).copied().unwrap_or(1).clamp(1, s.slots.len().max(1)) - 1;
            let colour_now = s.slots.get(i).map(|sl| sl.color.clone()).unwrap_or_default();
            let picked = s.plate.selected.as_deref() == Some(body.as_str());
            ui.horizontal(|ui| {
                chip(ui, &colour_now, i + 1);
                // a click picks the part to move and turn on the plate
                if ui_kit::button(ui, body.as_str(), if picked { ui_kit::Button::SmallSelected } else { ui_kit::Button::Small }).clicked() {
                    s.plate.selected = if picked { None } else { Some(body.clone()) };
                    s.preview = false;
                }
                if s.slots.len() > 1 {
                    ui_kit::dropdown(ui, &format!("fission_bambu_body_{body}"), &mut i, &refs);
                }
            });
            if s.body_slot.insert(body.clone(), i + 1) != Some(i + 1) && s.body_slot.len() > 1 {
                s.sliced = None;
            }
            if picked && s.plate.placement_controls(ui, bed(s)) {
                s.sliced = None;
            }
        }
        if !bodies.is_empty() {
            // the plate tools: what a click on the plate does
            let tools = [crate::bambu_plate::Tool::Move, crate::bambu_plate::Tool::Lay, crate::bambu_plate::Tool::Measure];
            let mut tool = tools.iter().position(|t| *t == s.plate.tool).unwrap_or(0);
            let names = [tr("fission.plate.tool_move"), tr("fission.plate.tool_lay"), tr("fission.plate.tool_measure")];
            if ui_kit::segmented(ui, &mut tool, &names, Default::default()) {
                s.plate.tool = tools.get(tool).copied().unwrap_or_default();
                s.plate.measure.clear();
                s.preview = false;
            }
            if s.plate.tool == crate::bambu_plate::Tool::Measure {
                let line = match s.plate.measure.as_slice() {
                    [a, b] => {
                        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                        format(
                            "fission.plate.distance",
                            &[("d", &format!("{len:.2}")), ("x", &format!("{:.2}", d[0].abs())), ("y", &format!("{:.2}", d[1].abs())), ("z", &format!("{:.2}", d[2].abs()))],
                        )
                    }
                    [_] => tr("fission.plate.measure_second").to_string(),
                    _ => tr("fission.plate.measure_first").to_string(),
                };
                ui_kit::text(ui, &line, ui_kit::Text::Value("fission_plate_distance", true));
            }
            if s.plate.tool == crate::bambu_plate::Tool::Lay {
                ui_kit::text(ui, tr("fission.plate.lay_hint"), ui_kit::Text::Note("fission_plate_lay_hint"));
            }
            ui.horizontal(|ui| {
                if ui_kit::button(ui, tr("fission.plate.auto_orient"), ui_kit::Button::Small).clicked() && s.plate.auto_orient(None) {
                    s.sliced = None;
                    s.preview = false;
                }
            });
            ui.horizontal(|ui| {
                if ui_kit::button(ui, tr("fission.plate.bring"), ui_kit::Button::Small).clicked() {
                    *act = Some(Act::Bring);
                }
                if s.plate.selected.is_some() && ui_kit::button(ui, tr("fission.plate.remove"), ui_kit::Button::Small).clicked() {
                    *act = Some(Act::RemoveFromPlate);
                }
                if ui_kit::button(ui, tr("fission.plate.arrange"), ui_kit::Button::Small).clicked() {
                    let b = bed(s);
                    s.plate.arrange(b);
                    s.sliced = None;
                    s.preview = false;
                }
            });
            if ui_kit::checkbox_row(ui, "fission_plate_lock", tr("fission.plate.lock"), s.plate.locked) {
                s.plate.locked = !s.plate.locked;
            }
            if ui_kit::checkbox_row(ui, "fission_plate_orient", tr("fission.plate.orient"), s.orient) {
                s.orient = !s.orient;
                s.sliced = None;
            }
            ui_kit::text(ui, tr("fission.plate.hint"), ui_kit::Text::Note("fission_plate_hint"));
            let off = s.plate.off_plate(bed(s));
            if !off.is_empty() {
                ui_kit::text(ui, &format("fission.plate.off", &[("names", &off.join(", "))]), ui_kit::Text::Error("fission_plate_off"));
            }
        }

        // ---------------- Process
        section_title(ui, "fission.bambu.process", "fission_process");
        let processes = names(&s.presets, "processes");
        let refs: Vec<&str> = processes.iter().map(String::as_str).collect();
        let before = s.process;
        ui_kit::choice_row(ui, tr("fission.bambu.process_preset"), "fission_bambu_process", &mut s.process, &refs);
        if s.process != before {
            let p = s.process_name();
            s.load_presets(p);
            s.sliced = None;
        }
        process_settings(s, ui);
        if !s.settings.edits.is_empty() && ui_kit::button(ui, tr("fission.bambu.reset_process"), ui_kit::Button::Small).clicked() {
            s.settings.edits.clear();
            s.sliced = None;
        }
    });
}

/// How a process setting is edited.
enum Field {
    /// A number (or a percentage) typed in.
    Text,
    /// On or off ("1" / "0").
    Check,
    /// One of Bambu Studio's values, with labels.
    Choice(&'static [(&'static str, &'static str)]),
}

const SEAMS: &[(&str, &str)] = &[
    ("aligned", "fission.set.seam_aligned"),
    ("nearest", "fission.set.seam_nearest"),
    ("back", "fission.set.seam_back"),
    ("random", "fission.set.seam_random"),
];
const IRONING: &[(&str, &str)] = &[
    ("no ironing", "fission.set.ironing_none"),
    ("top", "fission.set.ironing_top"),
    ("topmost", "fission.set.ironing_topmost"),
    ("solid", "fission.set.ironing_solid"),
];
const PATTERNS: &[(&str, &str)] = &[
    ("grid", "fission.set.pattern_grid"),
    ("gyroid", "fission.set.pattern_gyroid"),
    ("cubic", "fission.set.pattern_cubic"),
    ("honeycomb", "fission.set.pattern_honeycomb"),
    ("line", "fission.set.pattern_line"),
    ("crosshatch", "fission.set.pattern_crosshatch"),
    ("triangles", "fission.set.pattern_triangles"),
    ("lightning", "fission.set.pattern_lightning"),
];
const SUPPORT_TYPES: &[(&str, &str)] = &[
    ("normal(auto)", "fission.bambu.support_normal"),
    ("tree(auto)", "fission.bambu.support_tree"),
    ("normal(manual)", "fission.bambu.support_normal_manual"),
    ("tree(manual)", "fission.bambu.support_tree_manual"),
];
const BRIM_TYPES: &[(&str, &str)] = &[
    ("auto_brim", "fission.bambu.brim_auto"),
    ("no_brim", "fission.bambu.brim_none"),
    ("outer_only", "fission.bambu.brim_outer"),
    ("inner_only", "fission.bambu.brim_inner"),
    ("outer_and_inner", "fission.bambu.brim_both"),
];
const SEQUENCES: &[(&str, &str)] = &[("by layer", "fission.set.by_layer"), ("by object", "fission.set.by_object")];

/// The process settings shown, page by page (a page: its title and its settings).
const PAGES: &[(&str, &[(&str, &str, Field)])] = &[
    (
        "fission.set.quality",
        &[
            ("layer_height", "fission.bambu.layer_height", Field::Text),
            ("initial_layer_print_height", "fission.set.first_layer", Field::Text),
            ("seam_position", "fission.set.seam", Field::Choice(SEAMS)),
            ("ironing_type", "fission.set.ironing", Field::Choice(IRONING)),
        ],
    ),
    (
        "fission.set.strength",
        &[
            ("wall_loops", "fission.bambu.walls", Field::Text),
            ("top_shell_layers", "fission.bambu.top_layers", Field::Text),
            ("bottom_shell_layers", "fission.bambu.bottom_layers", Field::Text),
            ("sparse_infill_density", "fission.bambu.infill", Field::Text),
            ("sparse_infill_pattern", "fission.set.pattern", Field::Choice(PATTERNS)),
        ],
    ),
    (
        "fission.set.speed",
        &[
            ("outer_wall_speed", "fission.set.outer_wall_speed", Field::Text),
            ("inner_wall_speed", "fission.set.inner_wall_speed", Field::Text),
            ("sparse_infill_speed", "fission.set.infill_speed", Field::Text),
            ("travel_speed", "fission.set.travel_speed", Field::Text),
        ],
    ),
    (
        "fission.set.support",
        &[
            ("enable_support", "fission.bambu.supports", Field::Check),
            ("support_type", "fission.bambu.support_type", Field::Choice(SUPPORT_TYPES)),
            ("support_threshold_angle", "fission.set.threshold", Field::Text),
            ("support_on_build_plate_only", "fission.set.plate_only", Field::Check),
        ],
    ),
    (
        "fission.set.others",
        &[
            ("brim_type", "fission.bambu.brim", Field::Choice(BRIM_TYPES)),
            ("brim_width", "fission.set.brim_width", Field::Text),
            ("skirt_loops", "fission.set.skirt", Field::Text),
            ("enable_prime_tower", "fission.set.prime_tower", Field::Check),
            ("print_sequence", "fission.set.sequence", Field::Choice(SEQUENCES)),
        ],
    ),
];

/// The process preset's main settings, page by page like Bambu Studio's (Quality, Strength,
/// Speed, Support, Others). A changed setting is sliced with, the preset itself stays as it is.
fn process_settings(s: &mut State, ui: &mut egui::Ui) {
    let mut changed = false;
    for (page, fields) in PAGES {
        let title = tr(page).to_string();
        ui_kit::section(ui, *page, &title, *page == "fission.set.strength", |ui| {
            for (key, label, kind) in fields.iter() {
                // a setting the preset doesn't have isn't offered
                if !s.settings.base.contains_key(*key) && !s.settings.edits.contains_key(*key) {
                    continue;
                }
                let mut v = s.settings.get(key);
                let before = v.clone();
                match kind {
                    Field::Text => {
                        ui_kit::text_row(ui, tr(label), &mut v, "");
                        v = v.trim().chars().take(16).collect();
                    }
                    Field::Check => {
                        let on = v == "1" || v == "true";
                        if ui_kit::checkbox_row(ui, &format!("fission_set_{key}"), tr(label), on) {
                            v = if on { "0" } else { "1" }.into();
                        }
                    }
                    Field::Choice(options) => {
                        // a value this list doesn't know is kept, shown as Bambu Studio writes it
                        let mut values: Vec<&str> = options.iter().map(|o| o.0).collect();
                        let mut labels: Vec<String> = options.iter().map(|o| tr(o.1).to_string()).collect();
                        if !values.contains(&v.as_str()) && !v.is_empty() {
                            values.push(&before);
                            labels.push(before.clone());
                        }
                        let mut i = values.iter().position(|x| *x == v).unwrap_or(0);
                        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                        if ui_kit::choice_row(ui, tr(label), &format!("fission_set_{key}"), &mut i, &refs).changed() {
                            v = values.get(i).copied().unwrap_or("").to_string();
                        }
                    }
                }
                if v != before {
                    s.settings.set(key, v);
                    changed = true;
                }
            }
        });
    }
    if changed {
        s.sliced = None;
    }
}

/// The card across the bottom: what the slice came to, and the printer's job while it prints.
fn result_strip(app: &mut SolveApp, ui: &mut egui::Ui, act: &mut Option<Act>) {
    let s = &app.bambu;
    let printing = s.status.clone().filter(|v| matches!(text(v, "state"), "RUNNING" | "PAUSE" | "PREPARE"));
    let rows = if s.preview { 3.4 } else { 2.2 } + if printing.is_some() { 1.0 } else { 0.0 };
    let height = crate::theme::Tokens::get().control_height * rows;
    let available = ui.available_rect_before_wrap();
    ui_kit::Panel::bottom_strip(ui, "fission_print_result", available, height, |ui| {
        let s = &mut app.bambu;
        if s.is_busy(Job::Slice) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui_kit::text(ui, tr("fission.bambu.slicing"), ui_kit::Text::Note("fission_bambu_slicing"));
            });
        } else if s.preview {
            if let Some(v) = &s.sliced {
                let summary = format("fission.bambu.sliced", &[("time", text(v, "time")), ("grams", &format!("{:.1}", number(v, "grams")))]);
                ui_kit::text(ui, &summary, ui_kit::Text::Value("fission_bambu_result", true));
            }
            let total = s.sliced.as_ref().map_or(0.0, |v| number(v, "seconds"));
            s.plate.preview_controls(ui, total);
        } else if let Some(v) = &s.sliced {
            ui.horizontal(|ui| {
                let summary = format("fission.bambu.sliced", &[("time", text(v, "time")), ("grams", &format!("{:.1}", number(v, "grams")))]);
                ui_kit::text(ui, &summary, ui_kit::Text::Value("fission_bambu_result", true));
                for f in v.get("filaments").and_then(Value::as_array).into_iter().flatten() {
                    let n = f.get("slot").and_then(Value::as_u64).unwrap_or(1) as usize;
                    let colour_hex = s.slots.get(n.saturating_sub(1)).map(|sl| sl.color.as_str()).unwrap_or(text(f, "color"));
                    chip(ui, colour_hex, n);
                    ui_kit::text(ui, &format!("{} {:.1} g", text(f, "material"), number(f, "grams")), ui_kit::Text::Inline);
                }
            });
        } else {
            ui_kit::text(ui, tr("fission.bambu.slice_hint"), ui_kit::Text::Note("fission_bambu_hint"));
        }
        if let Some(v) = &printing {
            ui.horizontal(|ui| {
                let line = format(
                    "fission.bambu.job",
                    &[
                        ("job", text(v, "job")),
                        ("percent", &format!("{:.0}", number(v, "percent"))),
                        ("minutes", &format!("{:.0}", number(v, "minutes_left"))),
                    ],
                );
                ui_kit::text(ui, &line, ui_kit::Text::Note("fission_bambu_job"));
                let paused = text(v, "state") == "PAUSE";
                if ui_kit::button(ui, if paused { tr("fission.bambu.resume") } else { tr("fission.bambu.pause") }, ui_kit::Button::Small).clicked() {
                    *act = Some(Act::Control(if paused { "resume" } else { "pause" }));
                }
                if ui_kit::button(ui, tr("fission.bambu.stop"), ui_kit::Button::Small).clicked() {
                    *act = Some(Act::Control("stop"));
                }
            });
        }
        if let Some((m, error)) = &s.message
            && !s.is_busy(Job::Slice)
        {
            let style = if *error { ui_kit::Text::Error("fission_bambu_message") } else { ui_kit::Text::Note("fission_bambu_message") };
            ui_kit::text(ui, m, style);
        }
    });
}

/// Bambu Studio's Send print dialog: printer, each filament's AMS tray, options.
fn send_dialog(app: &mut SolveApp, ctx: &egui::Context, act: &mut Option<Act>) {
    if app.bambu.send.is_none() {
        return;
    }
    let title = tr("fission.bambu.send_title").to_string();
    let (closed, action) = ui_kit::Window::new(egui::Id::new("fission_send_print"), "mesh_export", &title)
        .cards(2.2)
        .actions(tr("fission.bambu.send"), tr("ui.cancel"), None)
        .show(ctx, true, |ui| {
            let s = &mut app.bambu;
            if let Some(tex) = &s.picture {
                ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(160.0, 160.0)));
            }
            if let Some(p) = s.target_printer() {
                let line = format("fission.bambu.send_on", &[("printer", text(p, "name")), ("serial", text(p, "serial"))]);
                ui_kit::text(ui, &line, ui_kit::Text::Heading("fission_send_printer"));
            }
            let trays = trays(s);
            let mut labels: Vec<String> = vec![tr("fission.bambu.tray_auto").to_string()];
            labels.extend(trays.iter().map(|t| text(t, "label").to_string()));
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            if trays.is_empty() {
                ui_kit::text(ui, tr("fission.bambu.no_trays"), ui_kit::Text::Note("fission_send_no_trays"));
            }
            let colours: Vec<String> = s.slots.iter().map(|sl| sl.color.clone()).collect();
            if let Some(choice) = s.send.as_mut() {
                for (i, c) in choice.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        chip(ui, colours.get(i).map(String::as_str).unwrap_or(""), i + 1);
                        ui_kit::dropdown(ui, &format!("fission_send_tray_{i}"), c, &refs);
                    });
                }
            }
            ui_kit::choice_row(ui, tr("fission.bambu.bed_leveling"), "fission_bambu_leveling", &mut s.bed_leveling, &SEND_CHOICES);
            ui_kit::choice_row(ui, tr("fission.bambu.flow_cali"), "fission_bambu_flow", &mut s.flow_cali, &SEND_CHOICES);
            if ui_kit::checkbox_row(ui, "fission_bambu_timelapse", tr("fission.bambu.timelapse"), s.timelapse) {
                s.timelapse = !s.timelapse;
            }
        });
    if action == ui_kit::Action::Ok {
        *act = Some(Act::Send);
    } else if closed || action == ui_kit::Action::Cancel {
        app.bambu.send = None;
    }
}

fn rgb_of(hex: &str) -> [u8; 3] {
    let c = colour(hex);
    [c.r(), c.g(), c.b()]
}

/// Open Bring to plate with what's on the plate now.
fn open_bring(s: &mut State) {
    let sizes = s.plate.sizes();
    let mut colours: Vec<[u8; 3]> = s.slots.iter().map(|sl| rgb_of(&sl.color)).collect();
    if colours.is_empty() {
        colours.push(rgb_of("#C8C8C8"));
    }
    let rows = sizes
        .into_iter()
        .map(|(name, size)| {
            let slot = s.body_slot.get(&name).copied().unwrap_or(1).clamp(1, colours.len()) - 1;
            let on = !s.plate.off.contains(&name);
            (name, on, slot, size)
        })
        .collect();
    s.bring = Some(Bring { rows, colours, ask: !s.dont_ask });
}

fn apply_bring(s: &mut State, b: Bring) {
    s.plate.off = b.rows.iter().filter(|r| !r.1).map(|r| r.0.clone()).collect();
    for (name, _, slot, _) in &b.rows {
        s.body_slot.insert(name.clone(), slot + 1);
    }
    // one slot per colour chip: new chips take the first filament's preset
    let preset = s.slots.first().map(|sl| sl.preset).unwrap_or(0);
    s.slots.resize(b.colours.len().max(1), Slot { preset, ..Slot::default() });
    for (slot, c) in s.slots.iter_mut().zip(&b.colours) {
        slot.color = format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
    }
    s.dont_ask = !b.ask;
    s.sliced = None;
    s.preview = false;
    if s.plate.selected.as_ref().is_some_and(|n| s.plate.off.contains(n)) {
        s.plate.selected = None;
    }
}

/// Bring to plate: tick the bodies to print, pick each one's filament, set the filaments' colours.
fn bring_dialog(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut b) = app.bambu.bring.take() else { return };
    let title = tr("fission.plate.bring_title").to_string();
    let (closed, action) = ui_kit::Window::new(egui::Id::new("fission_bring"), "import", &title)
        .cards(2.4)
        .actions(tr("fission.plate.bring_ok"), tr("ui.cancel"), None)
        .show(ctx, true, |ui| {
            ui_kit::text(ui, tr("fission.plate.bring_intro"), ui_kit::Text::Note("fission_bring_intro"));
            ui.horizontal_wrapped(|ui| {
                ui_kit::text(ui, tr("fission.bambu.filaments"), ui_kit::Text::Inline);
                for (k, c) in b.colours.iter_mut().enumerate() {
                    chip(ui, &format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]), k + 1);
                    ui_kit::color_field(ui, c);
                }
                if b.colours.len() < MAX_SLOTS && ui_kit::button(ui, "+", ui_kit::Button::Small).on_hover_text(tr("fission.bambu.add_slot")).clicked()
                {
                    let last = b.colours.last().copied().unwrap_or([200, 200, 200]);
                    b.colours.push(last);
                }
            });
            let slot_names: Vec<String> = (1..=b.colours.len()).map(|n| format("fission.bambu.slot", &[("n", &n.to_string())])).collect();
            let refs: Vec<&str> = slot_names.iter().map(String::as_str).collect();
            egui::ScrollArea::vertical().id_salt("fission_bring_rows").max_height(crate::theme::Tokens::get().control_height * 10.0).show(ui, |ui| {
                for (name, on, slot, size) in b.rows.iter_mut() {
                    *slot = (*slot).min(b.colours.len().saturating_sub(1));
                    ui.horizontal(|ui| {
                        if ui_kit::checkbox_row(ui, &format!("fission_bring_{name}"), name, *on) {
                            *on = !*on;
                        }
                    });
                    ui.horizontal(|ui| {
                        let hex = b.colours.get(*slot).map(|c| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])).unwrap_or_default();
                        chip(ui, &hex, *slot + 1);
                        ui_kit::dropdown(ui, &format!("fission_bring_slot_{name}"), slot, &refs);
                        ui_kit::text(ui, &format!("{:.0} × {:.0} × {:.0} mm", size[0], size[1], size[2]), ui_kit::Text::Inline);
                    });
                }
            });
            ui.horizontal(|ui| {
                if ui_kit::button(ui, tr("fission.plate.all"), ui_kit::Button::Small).clicked() {
                    b.rows.iter_mut().for_each(|r| r.1 = true);
                }
                if ui_kit::button(ui, tr("fission.plate.none"), ui_kit::Button::Small).clicked() {
                    b.rows.iter_mut().for_each(|r| r.1 = false);
                }
            });
            if ui_kit::checkbox_row(ui, "fission_bring_ask", tr("fission.plate.ask"), b.ask) {
                b.ask = !b.ask;
            }
        });
    if action == ui_kit::Action::Ok {
        apply_bring(&mut app.bambu, b);
    } else if !(closed || action == ui_kit::Action::Cancel) {
        app.bambu.bring = Some(b);
    }
}

/// Right-click a part on the plate: turn it, tip it, lay it flat, centre it, take it off the plate.
fn part_menu(app: &mut SolveApp, ctx: &egui::Context) {
    let Some((name, at)) = app.bambu.plate.menu.clone() else { return };
    let mut pick: Option<&str> = None;
    let resp = ui_kit::Container::new(ctx, ui_kit::Surface::Popup)
        .area(egui::Id::new("fission_part_menu"))
        .fixed_pos(at)
        .constrain(true)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            crate::context_menu::menu_frame(ui, |ui| {
                for (id, label, icon) in [
                    ("turn", tr("fission.plate.turn"), "revolve"),
                    ("tip_x", tr("fission.plate.tip_x"), "revolve"),
                    ("tip_y", tr("fission.plate.tip_y"), "revolve"),
                    ("flat", tr("fission.plate.lay_flat"), "ground"),
                    ("centre", tr("fission.plate.centre"), "move"),
                    ("remove", tr("fission.plate.remove"), "delete"),
                ] {
                    let item = crate::context_menu::Item::action(id, label, icon);
                    if crate::context_menu::list_row(ui, &item).is_some() {
                        pick = Some(id);
                    }
                }
            })
        });
    let s = &mut app.bambu;
    if let Some(id) = pick {
        s.plate.menu = None;
        let b = bed(s);
        let locked = s.plate.locked;
        match id {
            "remove" => {
                s.plate.off.insert(name.clone());
                s.plate.selected = None;
            }
            "flat" => {
                s.plate.auto_orient(Some(&name));
            }
            _ if locked => {}
            _ => {
                if let Some(p) = s.plate.places.get_mut(&name) {
                    match id {
                        "turn" => p.turn = (p.turn + 90.0).rem_euclid(360.0),
                        "tip_x" => p.tip(0, 90.0),
                        "tip_y" => p.tip(1, 90.0),
                        _ => (p.x, p.y) = ((b.0 + b.2) / 2.0, (b.1 + b.3) / 2.0),
                    }
                }
            }
        }
        s.sliced = None;
        s.preview = false;
    } else if ctx.input(|i| i.pointer.any_pressed() || i.key_pressed(egui::Key::Escape)) && !resp.response.contains_pointer() {
        s.plate.menu = None;
    }
}

/// The Filament window: a slot's temperatures, fan, flow and speed, changed for this print only
/// (the preset in Bambu Studio stays as it is).
fn filament_window(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut f) = app.bambu.filament_form.take() else { return };
    let name = app.bambu.slots.get(f.slot).and_then(|sl| app.bambu.filament_names().get(sl.preset).cloned()).unwrap_or_default();
    let title = format("fission.fil.title", &[("n", &(f.slot + 1).to_string())]);
    let loading = app.bambu.is_busy(Job::Filament);
    let (closed, action) = ui_kit::Window::new(egui::Id::new("fission_filament"), "settings", &title)
        .cards(2.0)
        .actions(tr("fission.fil.apply"), tr("ui.cancel"), None)
        .fields()
        .show(ctx, true, |ui| {
            ui_kit::text(ui, &name, ui_kit::Text::Heading("fission_filament_name"));
            if loading {
                ui_kit::text(ui, tr("fission.bambu.loading"), ui_kit::Text::Note("fission_filament_loading"));
                return;
            }
            for (key, label) in FILAMENT_FIELDS {
                if !f.base.contains_key(key) && !f.edits.contains_key(key) {
                    continue;
                }
                let mut v = f.edits.get(key).or_else(|| f.base.get(key)).cloned().unwrap_or_default();
                let before = v.clone();
                ui_kit::text_row(ui, tr(label), &mut v, "");
                if v != before {
                    let v: String = v.trim().chars().take(24).collect();
                    if f.base.get(key) == Some(&v) || v.is_empty() {
                        f.edits.remove(key);
                    } else {
                        f.edits.insert(key.to_string(), v);
                    }
                }
            }
            ui_kit::text(ui, tr("fission.fil.note"), ui_kit::Text::Note("fission_filament_note"));
            if !f.edits.is_empty() && ui_kit::button(ui, tr("fission.bambu.reset_process"), ui_kit::Button::Small).clicked() {
                f.edits.clear();
            }
        });
    if action == ui_kit::Action::Ok {
        if let Some(slot) = app.bambu.slots.get_mut(f.slot) {
            slot.overrides = f.edits;
        }
        app.bambu.sliced = None;
    } else if !(closed || action == ui_kit::Action::Cancel) {
        app.bambu.filament_form = Some(f);
    }
}

/// The Printers window: the printers Fission knows (Bambu Studio's and yours), adding one by its
/// serial number, IP address and access code, finding printers on the network, testing one.
fn printers_window(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut f) = app.bambu.printers_form.take() else { return };
    let title = tr("fission.printers.title").to_string();
    let mut jobs: Vec<(Job, &'static str, Value)> = Vec::new();
    let finding = app.bambu.is_busy(Job::Discover);
    let testing = app.bambu.is_busy(Job::TestPrinter);
    let printers = app.bambu.printers.clone();
    let close = ui_kit::Window::new(egui::Id::new("fission_printers"), "settings", &title).cards(2.4).show_body(ctx, |ui| {
        // the printers known now
        for p in &printers {
            ui.horizontal(|ui| {
                let added = p.get("added").and_then(Value::as_bool).unwrap_or(false);
                let from = if added { tr("fission.printers.yours") } else { tr("fission.printers.studio") };
                let ip = if text(p, "ip").is_empty() { "?" } else { text(p, "ip") };
                ui_kit::text(ui, &format!("{} · {} · {ip} · {from}", text(p, "name"), text(p, "serial")), ui_kit::Text::Inline);
                if added && ui_kit::button(ui, tr("fission.printers.remove"), ui_kit::Button::Small).clicked() {
                    jobs.push((Job::SavePrinter, "bambu.remove_printer", json!({"serial": text(p, "serial")})));
                }
                if ui_kit::button(ui, tr("fission.printers.edit"), ui_kit::Button::Small).clicked() {
                    (f.serial, f.ip, f.name) = (text(p, "serial").into(), text(p, "ip").into(), text(p, "name").into());
                }
            });
        }
        ui_kit::divider(ui, "fission_printers_rule");
        // find on the network
        let label = if finding { tr("fission.bambu.finding") } else { tr("fission.printers.find") };
        if ui_kit::button(ui, label, ui_kit::Button::Enabled(!finding)).clicked() {
            jobs.push((Job::Discover, "bambu.discover", json!({"wait": 5})));
        }
        for found in &f.found.clone() {
            ui.horizontal(|ui| {
                ui_kit::text(ui, &format!("{} · {} · {}", text(found, "name"), text(found, "serial"), text(found, "ip")), ui_kit::Text::Inline);
                if ui_kit::button(ui, tr("fission.printers.use"), ui_kit::Button::Small).clicked() {
                    (f.serial, f.ip, f.name) = (text(found, "serial").into(), text(found, "ip").into(), text(found, "name").into());
                }
            });
        }
        // the details
        ui_kit::text_row(ui, tr("fission.printers.serial"), &mut f.serial, "");
        ui_kit::text_row(ui, tr("fission.printers.ip"), &mut f.ip, "192.168.1.20");
        ui_kit::row(ui, tr("fission.printers.code"), |ui| ui_kit::password_field(ui, egui::Id::new("fission_printer_code"), &mut f.code, ""));
        ui_kit::text_row(ui, tr("fission.printers.name"), &mut f.name, "");
        ui_kit::text(ui, tr("fission.printers.where"), ui_kit::Text::Note("fission_printers_where"));
        ui.horizontal(|ui| {
            let ready = !f.serial.trim().is_empty() && !f.code.trim().is_empty();
            let label = if testing { tr("fission.bambu.connecting") } else { tr("fission.printers.test") };
            if ui_kit::button(ui, label, ui_kit::Button::Enabled(ready && !testing && !f.ip.trim().is_empty())).clicked() {
                jobs.push((
                    Job::TestPrinter,
                    "bambu.test_printer",
                    json!({"ip": f.ip.trim(), "serial": f.serial.trim(), "access_code": f.code.trim()}),
                ));
            }
            if ui_kit::button(ui, tr("fission.printers.save"), if ready { ui_kit::Button::Primary } else { ui_kit::Button::Enabled(false) }).clicked()
            {
                jobs.push((
                    Job::SavePrinter,
                    "bambu.save_printer",
                    json!({"serial": f.serial.trim(), "ip": f.ip.trim(), "access_code": f.code.trim(), "name": f.name.trim()}),
                ));
            }
        });
        if let Some((m, error)) = &f.note {
            let style = if *error { ui_kit::Text::Error("fission_printers_note") } else { ui_kit::Text::Note("fission_printers_note") };
            ui_kit::text(ui, m, style);
        }
    });
    for (job, id, params) in jobs {
        app.bambu.detached(job, id, params);
    }
    if !close {
        app.bambu.printers_form = Some(f);
    }
}

/// Draw the workspace (sidebar, result strip, preview and dialogs) when it's open.
pub fn panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    if !active(app) {
        crate::shell::register_panel(app, "fission:print", None, None);
        return;
    }
    start(&mut app.bambu);
    let bodies = solidcraft_engine::cmd::bambu::plate_bodies(&app.session);
    let mut act = app.bambu.pending.take();
    let bounds = crate::shell::panel_bounds(app, ui.available_rect_before_wrap());
    let (mut side, mut width) = (app.bambu.side.clone(), app.bambu.width);
    let title = tr("fission.bambu.title").to_string();
    let content = ui_kit::Panel::workspace(
        ui,
        ui_kit::WorkspaceOptions { id: "fission:print", title: &title, icon: "mesh_export", bounds, fill_height: true },
        &mut side,
        &mut width,
        |ui| sidebar(app, ui, &bodies, &mut act),
    );
    (app.bambu.side, app.bambu.width) = (side.clone(), width);
    let rect = content.map(|c| c.response.rect);
    crate::shell::register_panel(app, "fission:print", rect, Some(if side == "left" { ui_kit::Edge::Left } else { ui_kit::Edge::Right }));
    result_strip(app, ui, &mut act);
    let ctx = ui.ctx().clone();
    send_dialog(app, &ctx, &mut act);
    bring_dialog(app, &ctx);
    part_menu(app, &ctx);
    printers_window(app, &ctx);
    filament_window(app, &ctx);
    // the first time the tab shows (each time it's entered), ask which bodies go on the plate
    if !app.bambu.asked && !app.bambu.dont_ask && !bodies.is_empty() && !app.bambu.plate.sizes().is_empty() {
        app.bambu.asked = true;
        open_bring(&mut app.bambu);
    }
    match act {
        Some(Act::Slice) => slice(app, &bodies),
        Some(Act::OpenSend) => {
            open_send(&mut app.bambu);
            // the trays come from the printer: read them now if they're not known yet
            if trays(&app.bambu).is_empty() {
                app.bambu.read_status(Job::Status);
            }
        }
        Some(Act::Send) => send(app),
        Some(Act::Studio) => send_to_studio(app, &bodies),
        Some(Act::Sync) => app.bambu.read_status(Job::Sync),
        Some(Act::Refresh) => {
            app.bambu.detached(Job::Printers, "bambu.printers", json!({"discover": true, "wait": 3}));
        }
        Some(Act::Control(what)) => {
            if let Some(p) = app.bambu.target_printer() {
                let params = json!({"serial": text(p, "serial"), "ip": text(p, "ip"), "action": what});
                app.bambu.detached(Job::Control, "bambu.control", params);
            }
        }
        Some(Act::Leave) => app.ui.tab = "MODEL".into(),
        Some(Act::Bring) => open_bring(&mut app.bambu),
        Some(Act::Printers) => app.bambu.printers_form = Some(PrintersForm::default()),
        Some(Act::EditFilament(slot)) => {
            let s = &mut app.bambu;
            let preset = s.slots.get(slot).and_then(|sl| s.filament_names().get(sl.preset).cloned());
            if let Some(preset) = preset {
                let edits = s.slots.get(slot).map(|sl| sl.overrides.clone()).unwrap_or_default();
                s.filament_form = Some(FilamentForm { slot, base: BTreeMap::new(), edits });
                s.detached(Job::Filament, "bambu.filament", json!({"preset": preset}));
            }
        }
        Some(Act::Export) => {
            if let Some(file) = app.bambu.sliced.as_ref().map(|v| text(v, "file").to_string()) {
                let name = format!("{}.gcode.3mf", app.session.doc.name);
                app.pick_save(&name, &["3mf"], move |app, path| {
                    let Some(path) = path else { return };
                    match std::fs::copy(&file, &path) {
                        Ok(_) => app.set_status(format("fission.plate.exported", &[("path", &path)]), false),
                        Err(e) => app.set_status(format!("{path}: {e}"), true),
                    }
                });
            }
        }
        Some(Act::RemoveFromPlate) => {
            if let Some(n) = app.bambu.plate.selected.take() {
                app.bambu.plate.off.insert(n);
                app.bambu.sliced = None;
            }
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_process_edits() {
        assert_eq!(colour("#FF8000"), Color32::from_rgb(255, 128, 0));
        assert_eq!(colour("ff8000ff"), Color32::from_rgb(255, 128, 0));
        assert_eq!(colour("nonsense"), crate::theme::Tokens::get().text_dim);
        let mut p = Process { base: [("wall_loops".to_string(), "2".to_string())].into_iter().collect(), edits: BTreeMap::new() };
        p.set("wall_loops", "3".into());
        assert_eq!(p.get("wall_loops"), "3");
        p.set("wall_loops", "2".into());
        assert!(p.edits.is_empty(), "back to the preset's value is no change");
    }

    #[test]
    fn ams_sync_takes_each_loaded_tray() {
        let mut s = State { presets: json!({"filaments": ["Bambu PLA Basic @BBL A1", "My PETG"]}), ..Default::default() };
        let status = json!({"trays": [
            {"material": "PLA", "color": "#FF0000", "preset": "Bambu PLA Basic @BBL A1"},
            {"material": "", "color": "", "preset": ""},
            {"material": "PETG", "color": "#00FF00", "preset": "My PETG"}
        ]});
        synced(&mut s, &status);
        assert_eq!(s.slots.len(), 2);
        assert_eq!((s.slots[1].preset, s.slots[1].color.as_str()), (1, "#00FF00"));
    }
}

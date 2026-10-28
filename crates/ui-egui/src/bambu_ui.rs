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
}

const SEND_CHOICES: [&str; 3] = ["auto", "on", "off"];
const BRIMS: [&str; 5] = ["auto_brim", "no_brim", "outer_only", "inner_only", "outer_and_inner"];
const SUPPORTS: [&str; 4] = ["normal(auto)", "tree(auto)", "normal(manual)", "tree(manual)"];
/// How often the chosen printer's status is read while the workspace is open.
const STATUS_EVERY: f64 = 8.0;
const MAX_SLOTS: usize = 16;

/// A filament slot: its preset (index into the presets' filament list) and colour.
#[derive(Clone, Debug, Default)]
struct Slot {
    preset: usize,
    color: String,
}

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
    /// The workspace is showing (instead of the design's browser and timeline).
    pub open: bool,
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

/// '#rrggbb' (or '#rrggbbaa') → colour; grey when it isn't one.
fn colour(hex: &str) -> Color32 {
    let h = hex.trim().trim_start_matches('#');
    let n = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    match (n(0), n(2), n(4)) {
        (Some(r), Some(g), Some(b)) => Color32::from_rgb(r, g, b),
        _ => Color32::GRAY,
    }
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

/// Enter the workspace (the ribbon's Slice Plate and Print Plate).
pub fn open(app: &mut SolveApp) {
    let s = &mut app.bambu;
    s.open = true;
    if !s.started {
        s.started = true;
        s.side = "left".into();
        s.width = 0.0;
        s.detached(Job::Printers, "bambu.printers", json!({"discover": true, "wait": 4}));
        s.load_presets(None);
    }
}

/// Whether the PRINT workspace is showing.
pub fn active(app: &SolveApp) -> bool {
    app.bambu.open
}

/// Take the workers' answers (every frame, before drawing).
pub fn tick(app: &mut SolveApp, ctx: &egui::Context) {
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
        }
    }
    // keep the chosen printer's status fresh while the workspace is open
    let now = ctx.input(|i| i.time);
    let s = &mut app.bambu;
    if s.open && s.target_printer().is_some_and(|p| p.get("online").and_then(Value::as_bool).unwrap_or(false)) {
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
            .map(|k| Slot { preset: pla, color: s.slots.get(k).map(|o| o.color.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| colour_of(pla)) })
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
        .map(|t| Slot { preset: filaments.iter().position(|f| f == text(t, "preset")).unwrap_or(0), color: text(t, "color").to_string() })
        .collect();
    s.sliced = None;
    s.message = Some((format("fission.bambu.synced", &[("n", &s.slots.len().to_string())]), false));
}

fn picture(ctx: &egui::Context, path: &str) -> Option<egui::TextureHandle> {
    if path.is_empty() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let colour = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture("fission_plate_picture", colour, egui::TextureOptions::LINEAR))
}

fn slice_params(app: &SolveApp, bodies: &[(String, String)]) -> Value {
    let s = &app.bambu;
    let filaments = s.filament_names();
    let chosen: Vec<Value> =
        s.slots.iter().map(|sl| json!({"preset": filaments.get(sl.preset).cloned().unwrap_or_default(), "color": sl.color})).collect();
    let slots: serde_json::Map<String, Value> =
        bodies.iter().map(|(b, _)| (b.clone(), json!(s.body_slot.get(b).copied().unwrap_or(1).clamp(1, s.slots.len().max(1))))).collect();
    let overrides: serde_json::Map<String, Value> = s.settings.edits.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let mut p = json!({"printer": s.printer_name(), "process": s.process_name(), "filaments": chosen, "slots": slots, "overrides": overrides});
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
            if ui_kit::button(ui, tr("fission.bambu.back"), ui_kit::Button::Small).clicked() {
                *act = Some(Act::Leave);
            }
        });
    });
    egui::ScrollArea::vertical().id_salt("fission_print_side").auto_shrink([false, false]).show(ui, |ui| {
        // ---------------- Printer
        section_title(ui, "fission.bambu.printer_section", "fission_printer");
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
            if ui_kit::button(ui, tr("fission.bambu.refresh"), ui_kit::Button::Small).clicked() {
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
        for (body, _) in bodies {
            let mut i = s.body_slot.get(body).copied().unwrap_or(1).clamp(1, s.slots.len().max(1)) - 1;
            let colour_now = s.slots.get(i).map(|sl| sl.color.clone()).unwrap_or_default();
            ui.horizontal(|ui| {
                chip(ui, &colour_now, i + 1);
                ui_kit::text(ui, body, ui_kit::Text::Inline);
                if s.slots.len() > 1 {
                    ui_kit::dropdown(ui, &format!("fission_bambu_body_{body}"), &mut i, &refs);
                }
            });
            if s.body_slot.insert(body.clone(), i + 1) != Some(i + 1) && s.body_slot.len() > 1 {
                s.sliced = None;
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

fn process_settings(s: &mut State, ui: &mut egui::Ui) {
    let mut changed = false;
    for (key, label) in [
        ("layer_height", "fission.bambu.layer_height"),
        ("wall_loops", "fission.bambu.walls"),
        ("top_shell_layers", "fission.bambu.top_layers"),
        ("bottom_shell_layers", "fission.bambu.bottom_layers"),
        ("sparse_infill_density", "fission.bambu.infill"),
    ] {
        let mut v = s.settings.get(key);
        let before = v.clone();
        ui_kit::text_row(ui, tr(label), &mut v, "");
        if v != before {
            s.settings.set(key, v.trim().chars().take(16).collect());
            changed = true;
        }
    }
    let mut supports = s.settings.get("enable_support") == "1";
    if ui_kit::checkbox_row(ui, "fission_bambu_supports", tr("fission.bambu.supports"), supports) {
        supports = !supports;
        s.settings.set("enable_support", if supports { "1" } else { "0" }.into());
        changed = true;
    }
    if supports {
        let current = s.settings.get("support_type");
        let mut i = SUPPORTS.iter().position(|x| *x == current).unwrap_or(0);
        let labels = [
            tr("fission.bambu.support_normal"),
            tr("fission.bambu.support_tree"),
            tr("fission.bambu.support_normal_manual"),
            tr("fission.bambu.support_tree_manual"),
        ];
        if ui_kit::choice_row(ui, tr("fission.bambu.support_type"), "fission_bambu_support_type", &mut i, &labels).changed() {
            s.settings.set("support_type", SUPPORTS.get(i).copied().unwrap_or("normal(auto)").into());
            changed = true;
        }
    }
    let current = s.settings.get("brim_type");
    let mut i = BRIMS.iter().position(|x| *x == current).unwrap_or(0);
    let labels = [
        tr("fission.bambu.brim_auto"),
        tr("fission.bambu.brim_none"),
        tr("fission.bambu.brim_outer"),
        tr("fission.bambu.brim_inner"),
        tr("fission.bambu.brim_both"),
    ];
    if ui_kit::choice_row(ui, tr("fission.bambu.brim"), "fission_bambu_brim", &mut i, &labels).changed() {
        s.settings.set("brim_type", BRIMS.get(i).copied().unwrap_or("auto_brim").into());
        changed = true;
    }
    if changed {
        s.sliced = None;
    }
}

/// The card across the bottom: what the slice came to, and the printer's job while it prints.
fn result_strip(app: &mut SolveApp, ui: &mut egui::Ui, act: &mut Option<Act>) {
    let s = &app.bambu;
    let printing = s.status.as_ref().filter(|v| matches!(text(v, "state"), "RUNNING" | "PAUSE" | "PREPARE"));
    let height = crate::theme::Tokens::get().control_height * if printing.is_some() { 3.2 } else { 2.2 };
    let available = ui.available_rect_before_wrap();
    ui_kit::Panel::bottom_strip(ui, "fission_print_result", available, height, |ui| {
        let s = &app.bambu;
        if s.is_busy(Job::Slice) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui_kit::text(ui, tr("fission.bambu.slicing"), ui_kit::Text::Note("fission_bambu_slicing"));
            });
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
        if let Some(v) = printing {
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

/// Preview: Bambu Studio's picture of the sliced plate, over the view.
fn preview_card(app: &mut SolveApp, ctx: &egui::Context) {
    let s = &app.bambu;
    if !s.preview {
        return;
    }
    let title = tr("fission.bambu.preview").to_string();
    let close = ui_kit::Window::new(egui::Id::new("fission_print_preview"), "mesh_export", &title).cards(2.0).show_body(ctx, |ui| {
        match (&s.sliced, &s.picture) {
            (Some(_), Some(tex)) => {
                let w = ui.available_width().min(512.0);
                ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(w, w)));
            }
            (Some(_), None) => {
                ui_kit::text(ui, tr("fission.bambu.no_picture"), ui_kit::Text::Note("fission_bambu_no_picture"));
            }
            (None, _) => {
                ui_kit::text(ui, tr("fission.bambu.slice_hint"), ui_kit::Text::Note("fission_bambu_preview_hint"));
            }
        }
    });
    if close {
        app.bambu.preview = false;
    }
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

/// Draw the workspace (sidebar, result strip, preview and dialogs) when it's open.
pub fn panel(app: &mut SolveApp, ui: &mut egui::Ui) {
    if !app.bambu.open {
        crate::shell::register_panel(app, "fission:print", None, None);
        return;
    }
    let bodies = solidcraft_engine::cmd::bambu::plate_bodies(&app.session);
    let mut act = None;
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
    preview_card(app, &ctx);
    send_dialog(app, &ctx, &mut act);
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
        Some(Act::Leave) => app.bambu.open = false,
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
        assert_eq!(colour("nonsense"), Color32::GRAY);
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

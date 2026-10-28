//! Fission's Print panel: slice the shown bodies with the Bambu Studio you have (its printer,
//! process and filament presets, your own first) and send the plate to a Bambu Lab printer on your
//! network, or hand it to Bambu Studio. The panel is a form over the `bambu.*` commands; anything
//! slow (the slicer, the network) runs on a worker thread and answers on a channel, so the window
//! never waits.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender, channel};

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
}

/// What a click in the panel asks for (done after drawing).
#[derive(Clone, Copy)]
enum Act {
    Slice,
    Print,
    Studio,
}

const SEND_CHOICES: [&str; 3] = ["auto", "on", "off"];

#[derive(Default)]
pub struct State {
    pub open: bool,
    started: bool,
    channel: Option<(Sender<Answer>, Receiver<Answer>)>,
    /// Jobs not answered yet.
    busy: Vec<Job>,
    presets: Value,
    printers: Vec<Value>,
    /// Index into `presets.printers`, `presets.processes`, `presets.bed_types`.
    printer: usize,
    process: usize,
    bed: usize,
    /// The filament preset of each slot (index into `presets.filaments`).
    filaments: Vec<usize>,
    /// Each body's slot, 1-based.
    slots: BTreeMap<String, usize>,
    /// Index into `printers`: where Print sends to.
    target: usize,
    bed_leveling: usize,
    flow_cali: usize,
    timelapse: bool,
    /// The last slice (`bambu.slice`'s reply).
    sliced: Option<Value>,
    message: Option<(String, bool)>,
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
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

    fn printer_name(&self) -> Option<String> {
        names(&self.presets, "printers").get(self.printer).cloned()
    }

    fn load_presets(&mut self) {
        let params = match self.printer_name() {
            Some(p) => json!({"printer": p}),
            None => json!({}),
        };
        self.detached(Job::Presets, "bambu.presets", params);
    }

    fn is_busy(&self, job: Job) -> bool {
        self.busy.contains(&job)
    }
}

/// Open the panel (the ribbon's Slice Plate, and Print Plate).
pub fn open(app: &mut SolveApp) {
    app.bambu.open = true;
    if !app.bambu.started {
        app.bambu.started = true;
        app.bambu.detached(Job::Printers, "bambu.printers", json!({"discover": true, "wait": 4}));
        app.bambu.detached(Job::Presets, "bambu.presets", json!({}));
    }
}

/// Take the workers' answers (every frame, before drawing).
pub fn tick(app: &mut SolveApp, ctx: &egui::Context) {
    let answers: Vec<Answer> = app.bambu.channel.as_ref().map(|(_, rx)| rx.try_iter().collect()).unwrap_or_default();
    if !app.bambu.busy.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    for (job, answer) in answers {
        let s = &mut app.bambu;
        if let Some(i) = s.busy.iter().position(|j| *j == job) {
            s.busy.remove(i);
        }
        match (job, answer) {
            (_, Err(e)) => {
                s.message = Some((e.clone(), true));
                app.set_status(e, true);
            }
            (Job::Printers, Ok(v)) => {
                s.printers = v.get("printers").and_then(Value::as_array).cloned().unwrap_or_default();
                s.target = s.target.min(s.printers.len().saturating_sub(1));
            }
            (Job::Presets, Ok(v)) => {
                let first_load = s.presets.is_null();
                let printer = text(&v, "printer").to_string();
                s.printer = names(&v, "printers").iter().position(|p| *p == printer).unwrap_or(0);
                let processes = names(&v, "processes");
                s.process = v.get("default_process").and_then(Value::as_str).and_then(|d| processes.iter().position(|p| p == d)).unwrap_or(0);
                let filaments = names(&v, "filaments");
                let pla = v.get("default_filament").and_then(Value::as_str).and_then(|d| filaments.iter().position(|f| f == d)).unwrap_or(0);
                s.filaments = vec![pla; s.filaments.len().max(1)];
                s.bed = 0;
                s.presets = v;
                if first_load {
                    // Print goes to the printer this preset is for, when Bambu Studio knows one
                    if let Some(i) = s.printers.iter().position(|p| text(p, "preset") == printer) {
                        s.target = i;
                    }
                }
            }
            (Job::Slice, Ok(v)) => {
                let msg = format(
                    "fission.bambu.sliced",
                    &[("time", text(&v, "time")), ("grams", &format!("{:.1}", v.get("grams").and_then(Value::as_f64).unwrap_or(0.0)))],
                );
                s.message = Some((msg.clone(), false));
                s.sliced = Some(v);
                app.set_status(msg, false);
            }
            (Job::Print, Ok(v)) => {
                let msg = format("fission.bambu.printing", &[("file", text(&v, "uploaded"))]);
                s.message = Some((msg.clone(), false));
                app.set_status(msg, false);
            }
        }
    }
}

fn slice_params(s: &State) -> Value {
    let filaments = names(&s.presets, "filaments");
    let chosen: Vec<Value> = s.filaments.iter().map(|i| json!(filaments.get(*i).cloned().unwrap_or_default())).collect();
    let mut p = json!({
        "printer": s.printer_name(),
        "process": names(&s.presets, "processes").get(s.process),
        "filaments": chosen,
        "slots": s.slots.iter().map(|(b, n)| (b.clone(), json!(n))).collect::<serde_json::Map<_, _>>(),
    });
    if let Some(bed) = names(&s.presets, "bed_types").get(s.bed) {
        p["bed_type"] = json!(bed);
    }
    p
}

fn slice(app: &mut SolveApp) {
    let params = slice_params(&app.bambu);
    // the meshes are taken here, on the window thread; the slicer runs on a worker
    match solidcraft_engine::cmd::bambu::prepare_slice(&app.session, &params) {
        Ok(plan) => {
            app.bambu.sliced = None;
            app.bambu.message = Some((tr("fission.bambu.slicing").to_string(), false));
            app.bambu.spawn(Job::Slice, move || plan.run().map_err(|e| e.to_string()));
        }
        Err(e) => app.bambu.message = Some((e.to_string(), true)),
    }
}

fn print(app: &mut SolveApp) {
    let s = &app.bambu;
    let (Some(sliced), Some(printer)) = (s.sliced.as_ref(), s.printers.get(s.target)) else { return };
    let params = json!({
        "serial": text(printer, "serial"),
        "ip": text(printer, "ip"),
        "file": text(sliced, "file"),
        "bed_leveling": SEND_CHOICES.get(s.bed_leveling),
        "flow_cali": SEND_CHOICES.get(s.flow_cali),
        "timelapse": s.timelapse,
        "name": app.session.doc.name,
    });
    app.bambu.message = Some((tr("fission.bambu.sending").to_string(), false));
    app.bambu.detached(Job::Print, "bambu.print", params);
}

fn send_to_studio(app: &mut SolveApp) {
    let params = match app.bambu.sliced.as_ref() {
        Some(v) => json!({"file": text(v, "file")}),
        None => json!({"slots": slice_params(&app.bambu)["slots"]}),
    };
    match app.run("bambu.open_in_studio", params) {
        Ok(_) => app.bambu.message = Some((tr("fission.bambu.opened").to_string(), false)),
        Err(e) => app.bambu.message = Some((e, true)),
    }
}

pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    if !app.bambu.open {
        return;
    }
    let bodies = solidcraft_engine::cmd::bambu::plate_bodies(&app.session);
    let mut action = None;
    let title = tr("fission.bambu.title").to_string();
    let close = ui_kit::Window::new(egui::Id::new("fission_bambu"), "mesh_export", &title).cards(2.2).show_body(ctx, |ui| {
        let s = &mut app.bambu;
        if s.presets.is_null() {
            let wait = if s.is_busy(Job::Presets) { tr("fission.bambu.loading") } else { tr("fission.bambu.no_studio") };
            ui_kit::text(ui, wait, ui_kit::Text::Note("fission_bambu_wait"));
            if let Some((m, true)) = &s.message {
                ui_kit::text(ui, m, ui_kit::Text::Error("fission_bambu_error"));
            }
            return;
        }
        // --- printer and process
        let printers = names(&s.presets, "printers");
        let refs: Vec<&str> = printers.iter().map(String::as_str).collect();
        let before = s.printer;
        ui_kit::choice_row(ui, tr("fission.bambu.printer"), "fission_bambu_printer", &mut s.printer, &refs);
        if s.printer != before {
            s.sliced = None;
            s.load_presets();
        }
        let processes = names(&s.presets, "processes");
        let refs: Vec<&str> = processes.iter().map(String::as_str).collect();
        ui_kit::choice_row(ui, tr("fission.bambu.process"), "fission_bambu_process", &mut s.process, &refs);
        let beds = names(&s.presets, "bed_types");
        let refs: Vec<&str> = beds.iter().map(String::as_str).collect();
        ui_kit::choice_row(ui, tr("fission.bambu.plate"), "fission_bambu_plate", &mut s.bed, &refs);

        // --- filaments: one slot per filament, each body on a slot
        ui_kit::divider(ui, "fission_bambu_filaments_rule");
        ui_kit::text(ui, tr("fission.bambu.filaments"), ui_kit::Text::Heading("fission_bambu_filaments"));
        let filaments = names(&s.presets, "filaments");
        let refs: Vec<&str> = filaments.iter().map(String::as_str).collect();
        if s.filaments.is_empty() {
            s.filaments.push(0);
        }
        let mut remove = None;
        for i in 0..s.filaments.len() {
            let label = format("fission.bambu.slot", &[("n", &(i + 1).to_string())]);
            if let Some(slot) = s.filaments.get_mut(i) {
                ui_kit::choice_row(ui, &label, &format!("fission_bambu_slot_{i}"), slot, &refs);
            }
            if i > 0 && s.filaments.len() == i + 1 && ui_kit::button(ui, tr("fission.bambu.remove_slot"), ui_kit::Button::Small).clicked() {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            s.filaments.truncate(i);
            for slot in s.slots.values_mut() {
                *slot = (*slot).min(i);
            }
        }
        if s.filaments.len() < 16 && ui_kit::button(ui, tr("fission.bambu.add_slot"), ui_kit::Button::Small).clicked() {
            let last = s.filaments.last().copied().unwrap_or(0);
            s.filaments.push(last);
        }
        if s.filaments.len() > 1 {
            let slot_names: Vec<String> = (1..=s.filaments.len()).map(|n| format("fission.bambu.slot", &[("n", &n.to_string())])).collect();
            let refs: Vec<&str> = slot_names.iter().map(String::as_str).collect();
            for (body, _) in &bodies {
                let mut i = s.slots.get(body).copied().unwrap_or(1).clamp(1, s.filaments.len()) - 1;
                ui_kit::choice_row(ui, body, &format!("fission_bambu_body_{body}"), &mut i, &refs);
                s.slots.insert(body.clone(), i + 1);
            }
        }
        if bodies.is_empty() {
            ui_kit::text(ui, tr("fission.bambu.no_bodies"), ui_kit::Text::Note("fission_bambu_no_bodies"));
        }

        // --- slice
        ui_kit::divider(ui, "fission_bambu_slice_rule");
        let slicing = s.is_busy(Job::Slice);
        let label = if slicing { tr("fission.bambu.slicing") } else { tr("fission.bambu.slice") };
        if ui_kit::button(ui, label, ui_kit::Button::Enabled(!slicing && !bodies.is_empty())).clicked() {
            action = Some(Act::Slice);
        }
        if let Some(v) = &s.sliced {
            let grams = format!("{:.1}", v.get("grams").and_then(Value::as_f64).unwrap_or(0.0));
            ui_kit::text(
                ui,
                &format("fission.bambu.sliced", &[("time", text(v, "time")), ("grams", &grams)]),
                ui_kit::Text::Value("fission_bambu_result", true),
            );
            for f in v.get("filaments").and_then(Value::as_array).into_iter().flatten() {
                let line = format(
                    "fission.bambu.used",
                    &[
                        ("n", &f.get("slot").and_then(Value::as_u64).unwrap_or(0).to_string()),
                        ("material", text(f, "material")),
                        ("grams", &format!("{:.1}", f.get("grams").and_then(Value::as_f64).unwrap_or(0.0))),
                    ],
                );
                ui_kit::text(ui, &line, ui_kit::Text::Note("fission_bambu_used"));
            }
            let note = text(v, "note");
            if !note.is_empty() {
                ui_kit::text(ui, note, ui_kit::Text::Note("fission_bambu_note"));
            }
        }

        // --- print on a printer, or hand it to Bambu Studio
        ui_kit::divider(ui, "fission_bambu_print_rule");
        let labels: Vec<String> = s
            .printers
            .iter()
            .map(|p| {
                let on = p.get("online").and_then(Value::as_bool).unwrap_or(false);
                let state = if on { tr("fission.bambu.online") } else { tr("fission.bambu.offline") };
                format!("{} ({}, {state})", text(p, "name"), text(p, "serial"))
            })
            .collect();
        if labels.is_empty() {
            let note = if s.is_busy(Job::Printers) { tr("fission.bambu.finding") } else { tr("fission.bambu.no_printers") };
            ui_kit::text(ui, note, ui_kit::Text::Note("fission_bambu_printers"));
        } else {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            ui_kit::choice_row(ui, tr("fission.bambu.send_to"), "fission_bambu_target", &mut s.target, &refs);
            ui_kit::choice_row(ui, tr("fission.bambu.bed_leveling"), "fission_bambu_leveling", &mut s.bed_leveling, &SEND_CHOICES);
            ui_kit::choice_row(ui, tr("fission.bambu.flow_cali"), "fission_bambu_flow", &mut s.flow_cali, &SEND_CHOICES);
            if ui_kit::checkbox_row(ui, "fission_bambu_timelapse", tr("fission.bambu.timelapse"), s.timelapse) {
                s.timelapse = !s.timelapse;
            }
            let ready = s.sliced.is_some() && !s.is_busy(Job::Print) && !slicing;
            if ui_kit::button(ui, tr("fission.bambu.print"), ui_kit::Button::Enabled(ready)).clicked() {
                action = Some(Act::Print);
            }
        }
        if ui_kit::button(ui, tr("fission.bambu.studio"), ui_kit::Button::Normal).clicked() {
            action = Some(Act::Studio);
        }
        if let Some((m, error)) = &s.message {
            let style = if *error { ui_kit::Text::Error("fission_bambu_message") } else { ui_kit::Text::Note("fission_bambu_message") };
            ui_kit::text(ui, m, style);
        }
        if s.is_busy(Job::Printers) && s.is_busy(Job::Presets) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    });
    match action {
        Some(Act::Slice) => slice(app),
        Some(Act::Print) => print(app),
        Some(Act::Studio) => send_to_studio(app),
        None => {}
    }
    if close {
        app.bambu.open = false;
    }
}

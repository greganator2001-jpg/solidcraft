//! Fission: Run Fusion Script (MANAGE ▸ ADD-INS). Pick a Fusion 360 script (.py); it runs in Python
//! against a copy of the design on a worker thread; when it's done, the copy (with its undo steps)
//! becomes the design, so the window never waits on the script and a failed script changes nothing.
//! (Changed meanwhile, the design gets the script's steps done again instead.)

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::Value;

use crate::SolveApp;
use crate::i18n::{format, tr};
use crate::ui_kit;

/// Longest a script may run.
const TIMEOUT: Duration = Duration::from_secs(600);

type Answer = (Result<solidcraft_engine::cmd::fusion::ScriptRun, String>, solidcraft_engine::Session);

#[derive(Default)]
pub struct State {
    open: bool,
    running: Option<(PathBuf, mpsc::Receiver<Answer>, Arc<solidcraft_engine::doc::Document>)>,
    /// The last run: the script's name, what it said, whether it failed.
    last: Option<(String, Vec<String>, bool)>,
    log: String,
}

/// Ask for a script and run it.
pub fn start(app: &mut SolveApp) {
    if app.fusion_script.running.is_some() {
        app.fusion_script.open = true;
        return;
    }
    app.pick_file(tr("fission.fusion.pick"), &["py"], |app, path| {
        if let Some(path) = path {
            run(app, PathBuf::from(path));
        }
    });
}

/// Ask for a .ffx (a Fusion design saved by the Fission Export script) and bring it in.
pub fn open_ffx(app: &mut SolveApp) {
    if app.fusion_script.running.is_some() {
        app.fusion_script.open = true;
        return;
    }
    app.pick_file(tr("fission.fusion.pick_ffx"), &["ffx"], |app, path| {
        if let Some(path) = path {
            run_job(app, PathBuf::from(path), true);
        }
    });
}

fn run(app: &mut SolveApp, path: PathBuf) {
    run_job(app, path, false);
}

/// Open a .ffx on `copy`, as a script run: the one command, and what it said.
fn ffx(copy: &mut solidcraft_engine::Session, path: &std::path::Path) -> Result<solidcraft_engine::cmd::fusion::ScriptRun, String> {
    let params = serde_json::json!({"path": path.to_string_lossy()});
    let r = copy.execute("fusion.open_ffx", &params).map_err(|e| e.to_string())?;
    let rebuilt = r.get("rebuilt").and_then(Value::as_bool).unwrap_or(false);
    let steps = r.get("timeline").and_then(Value::as_u64).unwrap_or(0).to_string();
    let bodies = r.get("bodies").and_then(Value::as_array).map_or(0, Vec::len).to_string();
    let mut messages = vec![if rebuilt {
        format("fission.fusion.ffx_rebuilt", &[("n", &steps), ("bodies", &bodies)])
    } else {
        format("fission.fusion.ffx_bodies", &[("bodies", &bodies)])
    }];
    messages.extend(r.get("messages").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string));
    Ok(solidcraft_engine::cmd::fusion::ScriptRun { journal: vec![("fusion.open_ffx".into(), params)], messages, log: String::new() })
}

fn run_job(app: &mut SolveApp, path: PathBuf, is_ffx: bool) {
    let mut copy = app.session.scratch();
    let start = app.session.doc.clone();
    let (tx, rx) = mpsc::channel();
    let script = path.clone();
    let spawned = std::thread::Builder::new().name("fusion-script".into()).spawn(move || {
        let answer = if is_ffx { ffx(&mut copy, &script) } else { solidcraft_engine::cmd::fusion::run_script(&mut copy, &script, TIMEOUT) };
        tx.send((answer, copy)).ok();
    });
    let s = &mut app.fusion_script;
    s.open = true;
    s.log.clear();
    match spawned {
        Ok(_) => {
            s.running = Some((path, rx, start));
            s.last = None;
        }
        Err(e) => s.last = Some((name_of(&path), vec![e.to_string()], true)),
    }
}

fn name_of(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Take the worker's answer and do the script's steps on the design.
pub fn tick(app: &mut SolveApp, ctx: &egui::Context) {
    let Some((path, rx, start)) = app.fusion_script.running.as_ref() else { return };
    let (answer, copy) = match rx.try_recv() {
        Ok((a, copy)) => (a, Some(copy)),
        Err(mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        Err(mpsc::TryRecvError::Disconnected) => (Err(tr("fission.fusion.lost").to_string()), None),
    };
    let name = name_of(path);
    let unchanged = Arc::ptr_eq(start, &app.session.doc);
    app.fusion_script.running = None;
    let result = answer.and_then(|run| {
        match copy {
            // The design is as the script found it: take the copy, with the script's undo steps.
            Some(copy) if unchanged && !run.journal.is_empty() => app.session.accept_computed(copy),
            _ => replay(app, &run.journal)?,
        }
        Ok(run)
    });
    app.fusion_script.last = Some(match result {
        Ok(run) => {
            app.fusion_script.log = run.log;
            let mut said = run.messages;
            if run.journal.first().is_none_or(|(c, _)| c != "fusion.open_ffx") {
                said.insert(0, format("fission.fusion.done", &[("n", &run.journal.len().to_string())]));
            }
            (name, said, false)
        }
        Err(e) => (name, vec![e], true),
    });
}

/// Do the steps on the real design; if one fails here, put the design back as it was.
fn replay(app: &mut SolveApp, journal: &[(String, Value)]) -> Result<(), String> {
    let mark = app.session.checkpoint();
    for (cmd, params) in journal {
        if let Err(e) = app.run(cmd, params.clone()) {
            app.session.restore_checkpoint(mark).ok();
            app.session.drop_checkpoint(mark);
            return Err(format("fission.fusion.replay_failed", &[("cmd", cmd), ("error", &e)]));
        }
    }
    app.session.drop_checkpoint(mark);
    Ok(())
}

pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    if !app.fusion_script.open {
        return;
    }
    let title = tr("fission.fusion.title").to_string();
    let mut again = false;
    let close = ui_kit::Window::new(egui::Id::new("fission_fusion"), "play", &title).cards(2.0).show_body(ctx, |ui| {
        let s = &app.fusion_script;
        ui_kit::text(ui, tr("fission.fusion.intro"), ui_kit::Text::Note("fission_fusion_intro"));
        if let Some((path, _)) = &s.running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui_kit::text(ui, &format("fission.fusion.running", &[("name", &name_of(path))]), ui_kit::Text::Note("fission_fusion_running"));
            });
        }
        if let Some((name, said, failed)) = &s.last {
            ui_kit::text(ui, name, ui_kit::Text::Note("fission_fusion_name"));
            for (i, line) in said.iter().enumerate() {
                let style = if *failed { ui_kit::Text::Error("fission_fusion_said") } else { ui_kit::Text::Note("fission_fusion_said") };
                ui_kit::text(ui, line, style);
                if i > 20 {
                    break;
                }
            }
        }
        if !s.log.trim().is_empty() {
            let tail: String = s.log.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            ui_kit::text(ui, &tail, ui_kit::Text::Note("fission_fusion_log"));
        }
        if ui_kit::button(ui, tr("fission.fusion.another"), ui_kit::Button::Enabled(s.running.is_none())).clicked() {
            again = true;
        }
    });
    if close {
        app.fusion_script.open = false;
    }
    if again {
        start(app);
    }
}

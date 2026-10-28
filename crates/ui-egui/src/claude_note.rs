//! Fission's Note for Claude: write down what to change or fix while using the app. Notes are
//! kept in `~/Fission/notes-for-claude.md` (one checkbox line each); a note can bring a folder of
//! evidence with it (`~/Fission/notes/note-<time>/`: a screenshot of the window, a copy of the
//! design, what Fission was doing). Copy request puts the notes on the clipboard as a request to
//! paste into Claude; Fix it now saves the note and asks Claude about that one straight away.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde_json::{Value, json};

use crate::SolveApp;
use crate::i18n::{format, tr};
use crate::ui_kit;

/// Longest note kept.
const MAX_NOTE: usize = 4000;
/// Biggest notes file read back.
const MAX_FILE: u64 = 1 << 20;
/// Where Fix it now goes.
const CLAUDE_URL: &str = "https://claude.ai/code";

pub struct State {
    pub open: bool,
    draft: String,
    message: Option<(String, bool)>,
    shot: bool,
    design: bool,
    details: bool,
    /// A screenshot being taken for a saved note: its answer, the note's line, and Fix it now.
    capturing: Option<(mpsc::Receiver<Value>, String, bool)>,
}

impl Default for State {
    fn default() -> Self {
        State { open: false, draft: String::new(), message: None, shot: true, design: true, details: true, capturing: None }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|d| PathBuf::from(d).join("Fission"))
}

fn notes_file() -> Option<PathBuf> {
    home().map(|d| d.join("notes-for-claude.md"))
}

/// The notes not ticked off yet.
fn open_notes() -> Vec<String> {
    let Some(path) = notes_file() else { return Vec::new() };
    if std::fs::metadata(&path).map(|m| m.len() > MAX_FILE).unwrap_or(true) {
        return Vec::new();
    }
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| l.trim_start().strip_prefix("- [ ] ")).map(str::to_string).collect()
}

/// The line a note is kept as (without the checkbox).
fn line(note: &str, design: &str, folder: Option<&Path>) -> String {
    let one_line: String = note.split_whitespace().collect::<Vec<_>>().join(" ");
    match folder {
        Some(f) => format!("{one_line} (design: {design}; files: {})", f.display()),
        None => format!("{one_line} (design: {design})"),
    }
}

fn save(line: &str) -> Result<PathBuf, String> {
    let path = notes_file().ok_or("no home folder for the notes")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut text =
        std::fs::read_to_string(&path).unwrap_or_else(|_| "# Notes for Claude (Fission)\n\nTick a note off with [x] once it's done.\n\n".into());
    text.push_str(&format!("- [ ] {line}\n"));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

/// What Copy request puts on the clipboard.
fn request(notes: &[String]) -> String {
    let mut r = String::from(
        "Please work on these notes about Fission, my fork of SolidCraft (Rust; GitHub greganator2001-jpg/solidcraft, branch `fission`; \
         test builds go to `fission-test`). Follow AGENTS.md, then build a test version for me to try. \
         Where a note lists files, they are on my computer (a screenshot, the design and what Fission was doing):\n\n",
    );
    for n in notes {
        r.push_str(&format!("- {n}\n"));
    }
    r
}

/// The evidence folder of a new note: what was asked for, written now (the screenshot follows).
fn evidence(app: &mut SolveApp, note: &str) -> Result<Option<PathBuf>, String> {
    let s = &app.claude_note;
    if !(s.shot || s.design || s.details) {
        return Ok(None);
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = home().ok_or("no home folder for the notes")?.join("notes").join(format!("note-{stamp}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {} ({e})", dir.display()))?;
    std::fs::write(dir.join("note.md"), format!("# Note for Claude\n\n{note}\n")).map_err(|e| e.to_string())?;
    if s.design {
        let path = dir.join("design.json");
        // A copy only: the design keeps its own file and name.
        app.session.execute("file.export", &json!({"path": path.to_string_lossy(), "format": "json"})).map_err(|e| e.to_string())?;
    }
    if s.details {
        let draft = crate::issue_report::draft_for(app, None, None);
        std::fs::write(dir.join("what-fission-was-doing.txt"), draft.body).map_err(|e| e.to_string())?;
    }
    Ok(Some(dir))
}

/// Save the note (and its evidence); a screenshot is taken next frame, with this window hidden.
fn keep(app: &mut SolveApp, fix: bool, ctx: &egui::Context) {
    let design = app.session.doc.name.clone();
    let note = app.claude_note.draft.clone();
    let folder = match evidence(app, &note) {
        Ok(f) => f,
        Err(e) => {
            app.claude_note.message = Some((e, true));
            return;
        }
    };
    let entry = line(&note, &design, folder.as_deref());
    if let Some(dir) = folder.filter(|_| app.claude_note.shot) {
        app.shot_token += 1;
        let token = app.shot_token;
        let (tx, rx) = mpsc::channel();
        let now = crate::now_ms();
        app.queued_shots.push((token, now + 150.0, 0));
        app.pending_shots.push((token, Some(dir.join("screenshot.png").to_string_lossy().into_owned()), tx, now + 8000.0));
        app.claude_note.capturing = Some((rx, entry, fix));
        return;
    }
    finish(app, &entry, fix, ctx);
}

fn finish(app: &mut SolveApp, entry: &str, fix: bool, ctx: &egui::Context) {
    let s = &mut app.claude_note;
    s.message = Some(match save(entry) {
        Ok(path) => {
            s.draft.clear();
            if fix {
                ctx.copy_text(request(&[entry.to_string()]));
                ctx.open_url(egui::OpenUrl::new_tab(CLAUDE_URL));
                (tr("fission.note.fixing").to_string(), false)
            } else {
                (format("fission.note.saved", &[("path", &path.to_string_lossy())]), false)
            }
        }
        Err(e) => (e, true),
    });
}

pub fn open(app: &mut SolveApp) {
    app.claude_note.open = true;
}

pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    if !app.claude_note.open {
        return;
    }
    // A screenshot for the note is on its way: the window stays out of it.
    if let Some((rx, entry, fix)) = app.claude_note.capturing.take() {
        match rx.try_recv() {
            Ok(_) | Err(mpsc::TryRecvError::Disconnected) => finish(app, &entry, fix, ctx),
            Err(mpsc::TryRecvError::Empty) => {
                app.claude_note.capturing = Some((rx, entry, fix));
                ctx.request_repaint();
                return;
            }
        }
    }
    let title = tr("fission.note.title").to_string();
    let mut save_now = None;
    let close = ui_kit::Window::new(egui::Id::new("fission_note"), "edit", &title).cards(2.0).show_body(ctx, |ui| {
        let s = &mut app.claude_note;
        ui_kit::text(ui, tr("fission.note.intro"), ui_kit::Text::Note("fission_note_intro"));
        ui_kit::text_area(ui, tr("fission.note.label"), egui::Id::new("fission_note_text"), &mut s.draft, 6, MAX_NOTE);
        if ui_kit::checkbox_row(ui, "fission_note_shot", tr("fission.note.with_shot"), s.shot) {
            s.shot = !s.shot;
        }
        if ui_kit::checkbox_row(ui, "fission_note_design", tr("fission.note.with_design"), s.design) {
            s.design = !s.design;
        }
        if ui_kit::checkbox_row(ui, "fission_note_details", tr("fission.note.with_details"), s.details) {
            s.details = !s.details;
        }
        let has_text = !s.draft.trim().is_empty();
        ui.horizontal(|ui| {
            if ui_kit::button(ui, tr("fission.note.save"), ui_kit::Button::Enabled(has_text)).clicked() {
                save_now = Some(false);
            }
            if ui_kit::button(ui, tr("fission.note.fix"), ui_kit::Button::Enabled(has_text)).clicked() {
                save_now = Some(true);
            }
        });
        let notes = open_notes();
        ui_kit::text(ui, &format("fission.note.count", &[("n", &notes.len().to_string())]), ui_kit::Text::Note("fission_note_count"));
        for n in notes.iter().rev().take(5) {
            ui_kit::text(ui, &format!("• {n}"), ui_kit::Text::Note("fission_note_item"));
        }
        if ui_kit::button(ui, tr("fission.note.copy"), ui_kit::Button::Enabled(!notes.is_empty())).clicked() {
            ui.ctx().copy_text(request(&notes));
            s.message = Some((tr("fission.note.copied").to_string(), false));
        }
        if let Some((m, error)) = &s.message {
            let style = if *error { ui_kit::Text::Error("fission_note_message") } else { ui_kit::Text::Note("fission_note_message") };
            ui_kit::text(ui, m, style);
        }
    });
    if let Some(fix) = save_now {
        keep(app, fix, ctx);
    }
    if close {
        app.claude_note.open = false;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_request_lists_every_note() {
        let r = super::request(&["Make the grid lighter".into(), "Fillet tool crashes on a cone".into()]);
        assert!(r.contains("- Make the grid lighter\n") && r.contains("- Fillet tool crashes on a cone\n") && r.contains("AGENTS.md"));
    }
}

//! Fission's Note for Claude: write down what to change or fix while using the app. Notes are
//! kept in `~/Fission/notes-for-claude.md` (one checkbox line each), and Copy request puts them on
//! the clipboard as a request to paste into Claude, with where Fission's code lives.

use std::path::PathBuf;

use crate::SolveApp;
use crate::i18n::{format, tr};
use crate::ui_kit;

/// Longest note kept.
const MAX_NOTE: usize = 4000;
/// Biggest notes file read back.
const MAX_FILE: u64 = 1 << 20;

#[derive(Default)]
pub struct State {
    pub open: bool,
    draft: String,
    message: Option<(String, bool)>,
}

fn notes_file() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|d| PathBuf::from(d).join("Fission").join("notes-for-claude.md"))
}

/// The notes not ticked off yet.
fn open_notes() -> Vec<String> {
    let Some(path) = notes_file() else { return Vec::new() };
    if std::fs::metadata(&path).map(|m| m.len() > MAX_FILE).unwrap_or(true) {
        return Vec::new();
    }
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| l.trim_start().strip_prefix("- [ ] ")).map(str::to_string).collect()
}

fn save(note: &str, design: &str) -> Result<PathBuf, String> {
    let path = notes_file().ok_or("no home folder for the notes")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut text =
        std::fs::read_to_string(&path).unwrap_or_else(|_| "# Notes for Claude (Fission)\n\nTick a note off with [x] once it's done.\n\n".into());
    let one_line: String = note.split_whitespace().collect::<Vec<_>>().join(" ");
    text.push_str(&format!("- [ ] {one_line} (design: {design})\n"));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

/// What Copy request puts on the clipboard.
fn request(notes: &[String]) -> String {
    let mut r = String::from(
        "Please work on these notes about Fission, my fork of SolidCraft (Rust; GitHub greganator2001-jpg/solidcraft, branch `fission`; \
         test builds go to `fission-test`). Follow AGENTS.md, then build a test version for me to try:\n\n",
    );
    for n in notes {
        r.push_str(&format!("- {n}\n"));
    }
    r
}

pub fn open(app: &mut SolveApp) {
    app.claude_note.open = true;
}

pub fn show(app: &mut SolveApp, ctx: &egui::Context) {
    if !app.claude_note.open {
        return;
    }
    let design = app.session.doc.name.clone();
    let title = tr("fission.note.title").to_string();
    let close = ui_kit::Window::new(egui::Id::new("fission_note"), "edit", &title).cards(2.0).show_body(ctx, |ui| {
        let s = &mut app.claude_note;
        ui_kit::text(ui, tr("fission.note.intro"), ui_kit::Text::Note("fission_note_intro"));
        ui_kit::text_area(ui, tr("fission.note.label"), egui::Id::new("fission_note_text"), &mut s.draft, 6, MAX_NOTE);
        let has_text = !s.draft.trim().is_empty();
        if ui_kit::button(ui, tr("fission.note.save"), ui_kit::Button::Enabled(has_text)).clicked() {
            s.message = Some(match save(&s.draft, &design) {
                Ok(path) => {
                    s.draft.clear();
                    (format("fission.note.saved", &[("path", &path.to_string_lossy())]), false)
                }
                Err(e) => (e, true),
            });
        }
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

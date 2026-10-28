//! The BAMBU tab's print plate in the 3D view, as in FissionCAD and Bambu Studio: Prepare draws the
//! printer's plate with the parts where they'll print (moved and turned here, without changing the
//! design), Preview draws the sliced toolpaths layer by layer, coloured by line type.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use egui::Color32;
use solidcraft_engine::cmd::bambu::gcode::{ROLES, Toolpaths};
use solidcraft_engine::cmd::bambu::{Part, arrange, centre_on_bed, place};
use solidcraft_engine::geom::{Aabb3, Vec3};

use crate::gpu::GpuScene;
use crate::i18n::{format, tr};
use crate::theme::Tokens;
use crate::ui_kit;

/// Most toolpath segments drawn at once (the layers below are left out first).
const DRAW_BUDGET: usize = 1_500_000;
/// Gap between parts when laying them out (mm).
const ARRANGE_GAP: f64 = 6.0;

/// A part's place on the plate: the middle of its footprint (mm) and its turn about Z (degrees).
pub type Place = (f64, f64, f64);

#[derive(Default)]
pub struct Plate {
    /// What `parts` were taken from (the design's revision, its shown bodies, the plate).
    key: u64,
    /// The shown bodies, where the design has them, centred on the plate.
    parts: Vec<Part>,
    pub places: BTreeMap<String, Place>,
    /// Shown bodies left off the plate (Bring to plate unticked them, or Remove from plate).
    pub off: BTreeSet<String>,
    pub selected: Option<String>,
    toolpaths: Option<Arc<Toolpaths>>,
    rx: Option<Receiver<Result<Toolpaths, String>>>,
    /// The highest layer shown in Preview.
    pub layer: usize,
    /// Line types hidden in Preview (ROLES indices).
    hidden: BTreeSet<u8>,
    /// Preview colours the toolpaths by filament instead of by line type.
    pub by_filament: bool,
    /// Nothing on the plate moves (its lock).
    pub locked: bool,
    /// A part being dragged on the plate: its name, where the drag started on the plate, its place then.
    dragging: Option<(String, [f64; 2], Place)>,
    /// The view has been fitted to the plate since the tab was entered.
    pub fitted: bool,
    pub error: Option<String>,
    /// The X, Y and turn fields as typed, for the part and place they were filled from.
    edit: Option<(String, [String; 3], Place)>,
}

fn rgba(c: Color32) -> [u8; 4] {
    c.to_array()
}

fn hex(c: &str) -> Color32 {
    let h = c.trim().trim_start_matches('#');
    let n = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    match (n(0), n(2), n(4)) {
        (Some(r), Some(g), Some(b)) => Color32::from_rgb(r, g, b),
        _ => Color32::GRAY,
    }
}

impl Plate {
    /// Take the shown bodies again when the design (or what's shown, or the plate) changed. New
    /// parts start where the design has them, the whole layout centred on the plate.
    pub fn refresh(&mut self, session: &solidcraft_engine::Session, bed: (f64, f64, f64, f64)) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        session.revision.hash(&mut h);
        session.visibility.hidden_bodies.hash(&mut h);
        [bed.0, bed.1, bed.2, bed.3].map(f64::to_bits).hash(&mut h);
        let key = h.finish();
        if key == self.key && !self.parts.is_empty() {
            return;
        }
        self.key = key;
        self.parts = solidcraft_engine::cmd::bambu::plate_parts_of(session).unwrap_or_default();
        let mut centred = self.parts.clone();
        centre_on_bed(&mut centred, bed);
        for p in &centred {
            if let Some((lo, hi)) = p.bounds() {
                self.places.entry(p.name.clone()).or_insert(((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, 0.0));
            }
        }
        let names: BTreeSet<&String> = self.parts.iter().map(|p| &p.name).collect();
        self.places.retain(|n, _| names.contains(n));
        if self.selected.as_ref().is_some_and(|s| !names.contains(s)) {
            self.selected = None;
        }
    }

    /// Every shown body and its size (mm), for Bring to plate.
    pub fn sizes(&self) -> Vec<(String, [f64; 3])> {
        self.parts.iter().filter_map(|p| p.bounds().map(|(lo, hi)| (p.name.clone(), [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]))).collect()
    }

    /// The bodies on the plate.
    pub fn on_plate(&self) -> Vec<String> {
        self.parts.iter().map(|p| p.name.clone()).filter(|n| !self.off.contains(n)).collect()
    }

    /// The parts on the plate, where they'll print.
    pub fn placed(&self) -> Vec<Part> {
        self.parts
            .iter()
            .filter(|p| !self.off.contains(&p.name))
            .map(|p| {
                let mut p = p.clone();
                if let Some((x, y, r)) = self.places.get(&p.name) {
                    place(&mut p, *x, *y, *r);
                }
                p
            })
            .collect()
    }

    /// Lay every part out in rows on the plate.
    pub fn arrange(&mut self, bed: (f64, f64, f64, f64)) {
        if self.locked {
            return;
        }
        // laid out as turned now
        let placed = self.placed();
        for (name, x, y) in arrange(&placed, bed, ARRANGE_GAP) {
            if let Some(p) = self.places.get_mut(&name) {
                (p.0, p.1) = (x, y);
            }
        }
    }

    /// Read the sliced plate's toolpaths on a worker.
    pub fn load_toolpaths(&mut self, file: &str) {
        self.toolpaths = None;
        self.error = None;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let path = std::path::PathBuf::from(file);
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(|| solidcraft_engine::cmd::bambu::gcode::read(&path, 1))
                .unwrap_or_else(|_| Err("couldn't read the sliced plate's toolpaths".into()));
            tx.send(r).ok();
        });
    }

    pub fn forget_toolpaths(&mut self) {
        self.toolpaths = None;
        self.rx = None;
    }

    pub fn loading(&self) -> bool {
        self.rx.is_some()
    }

    pub fn has_toolpaths(&self) -> bool {
        self.toolpaths.is_some()
    }

    /// Take the worker's toolpaths when they're read.
    pub fn tick(&mut self) {
        let Some(rx) = &self.rx else { return };
        if let Ok(r) = rx.try_recv() {
            self.rx = None;
            match r {
                Ok(tp) => {
                    self.layer = tp.layers.len().saturating_sub(1);
                    self.toolpaths = Some(Arc::new(tp));
                }
                Err(e) => self.error = Some(e),
            }
        }
    }

    /// What the plate scene shows, so it's built again only when that changes.
    pub fn scene_key(&self, preview: bool, bed: (f64, f64, f64, f64), colours: &BTreeMap<String, String>) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.key.hash(&mut h);
        preview.hash(&mut h);
        [bed.0, bed.1, bed.2, bed.3].map(f64::to_bits).hash(&mut h);
        for (n, (x, y, r)) in &self.places {
            n.hash(&mut h);
            [x, y, r].map(|v| v.to_bits()).hash(&mut h);
        }
        self.selected.hash(&mut h);
        self.off.hash(&mut h);
        colours.hash(&mut h);
        self.layer.hash(&mut h);
        self.hidden.hash(&mut h);
        self.by_filament.hash(&mut h);
        self.toolpaths.as_ref().map(|t| Arc::as_ptr(t) as usize).hash(&mut h);
        Tokens::get().panel.hash(&mut h);
        h.finish()
    }

    /// The plate, and the parts (Prepare) or the toolpaths (Preview). `colours`: each body's filament
    /// colour.
    pub fn scene(&self, preview: bool, bed: (f64, f64, f64, f64), height: f64, colours: &BTreeMap<String, String>, filaments: &[String]) -> GpuScene {
        let t = Tokens::get();
        let mut sc = GpuScene::default();
        // the plate: a thin sheet with a 10 mm grid and its edge
        let (x0, y0, x1, y1) = (bed.0 as f32, bed.1 as f32, bed.2 as f32, bed.3 as f32);
        let mut sheet = t.panel_header;
        sheet = Color32::from_rgba_unmultiplied(sheet.r(), sheet.g(), sheet.b(), 200);
        let fill = sc.color(rgba(sheet));
        let z = -0.05f32;
        for p in [[x0, y0, z], [x1, y0, z], [x1, y1, z], [x0, y0, z], [x1, y1, z], [x0, y1, z]] {
            sc.trans_tri(p, [0.0, 0.0, 1.0], fill);
        }
        let grid = sc.color(rgba(t.grid_major));
        let mut gx = (x0 / 10.0).ceil() * 10.0;
        while gx <= x1 {
            sc.under_line([gx, y0, 0.0], [gx, y1, 0.0], grid, 1.0);
            gx += 10.0;
        }
        let mut gy = (y0 / 10.0).ceil() * 10.0;
        while gy <= y1 {
            sc.under_line([x0, gy, 0.0], [x1, gy, 0.0], grid, 1.0);
            gy += 10.0;
        }
        let edge = sc.color(rgba(t.accent));
        for (a, b) in [([x0, y0], [x1, y0]), ([x1, y0], [x1, y1]), ([x1, y1], [x0, y1]), ([x0, y1], [x0, y0])] {
            sc.line([a[0], a[1], 0.0], [b[0], b[1], 0.0], edge, 2.0, false);
        }
        // the printable height, as a faint box outline
        let top = height as f32;
        if top > 0.0 {
            let dim = sc.color(rgba(t.border));
            for (a, b) in [([x0, y0], [x1, y0]), ([x1, y0], [x1, y1]), ([x1, y1], [x0, y1]), ([x0, y1], [x0, y0])] {
                sc.line([a[0], a[1], top], [b[0], b[1], top], dim, 1.0, false);
            }
            for c in [[x0, y0], [x1, y0], [x1, y1], [x0, y1]] {
                sc.line([c[0], c[1], 0.0], [c[0], c[1], top], dim, 1.0, false);
            }
        }
        if preview {
            self.toolpath_scene(&mut sc, filaments);
        } else {
            for p in self.placed() {
                let picked = self.selected.as_deref() == Some(p.name.as_str());
                let c = if picked { t.accent } else { colours.get(&p.name).map(|c| hex(c)).unwrap_or(Color32::GRAY) };
                let c = sc.color(rgba(c));
                for tri in &p.triangles {
                    let v = tri.map(|i| p.vertices.get(i as usize).copied().unwrap_or_default().map(|x| x as f32));
                    let (a, b) =
                        ([v[1][0] - v[0][0], v[1][1] - v[0][1], v[1][2] - v[0][2]], [v[2][0] - v[0][0], v[2][1] - v[0][1], v[2][2] - v[0][2]]);
                    let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                    let n = if len > 0.0 { n.map(|x| x / len) } else { [0.0, 0.0, 1.0] };
                    for p in v {
                        sc.tri(p, n, c);
                    }
                }
            }
        }
        sc
    }

    fn toolpath_scene(&self, sc: &mut GpuScene, filaments: &[String]) {
        let Some(tp) = &self.toolpaths else { return };
        let colours: Vec<_> = ROLES.iter().map(|(_, c)| sc.color(rgba(hex(c)))).collect();
        let by_tool: Vec<_> = filaments.iter().map(|c| sc.color(rgba(hex(c)))).collect();
        let grey = sc.color(rgba(Color32::GRAY));
        let top = self.layer.min(tp.layers.len().saturating_sub(1));
        // from the top layer down, until the budget is spent
        let mut budget = DRAW_BUDGET;
        for n in (0..=top).rev() {
            let segs = tp.layer(n);
            if segs.len() > budget {
                break;
            }
            budget -= segs.len();
            let width = if n == top { 3.0 } else { 2.0 };
            for s in segs {
                if self.hidden.contains(&s.role) {
                    continue;
                }
                let c = if self.by_filament {
                    by_tool.get(usize::from(s.tool)).copied().unwrap_or(grey)
                } else {
                    colours.get(usize::from(s.role)).copied().unwrap_or(grey)
                };
                sc.line(s.a, s.b, c, width, false);
            }
        }
    }

    /// The plate's box, for fitting the view.
    pub fn bounds(bed: (f64, f64, f64, f64), height: f64) -> Aabb3 {
        let mut b = Aabb3::EMPTY;
        b.add(Vec3::new(bed.0, bed.1, 0.0));
        b.add(Vec3::new(bed.2, bed.3, height.clamp(10.0, 400.0) / 3.0));
        b
    }

    /// Under the selected part in the Objects list: where it sits and its turn, with Turn 90°,
    /// Centre and Lay out all. True when something moved.
    pub fn placement_controls(&mut self, ui: &mut egui::Ui, bed: (f64, f64, f64, f64)) -> bool {
        let Some(name) = self.selected.clone() else { return false };
        if self.locked {
            ui_kit::text(ui, tr("fission.plate.locked_note"), ui_kit::Text::Note("fission_plate_locked"));
            return false;
        }
        let Some(&(x, y, r)) = self.places.get(&name) else { return false };
        let mut moved = false;
        let current = (x, y, r);
        if self.edit.as_ref().is_none_or(|(n, _, at)| *n != name || *at != current) {
            self.edit = Some((name.clone(), [format!("{x:.1}"), format!("{y:.1}"), format!("{r:.0}")], current));
        }
        if let Some((_, fields, at)) = self.edit.as_mut() {
            for (field, label) in fields.iter_mut().zip(["fission.plate.x", "fission.plate.y", "fission.plate.rotation"]) {
                ui_kit::text_row(ui, tr(label), field, "");
            }
            let parsed: Vec<Option<f64>> = fields.iter().map(|f| f.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect();
            if let [Some(nx), Some(ny), Some(nr)] = parsed[..] {
                let typed = (nx, ny, nr.rem_euclid(360.0));
                let shown = (format!("{x:.1}"), format!("{y:.1}"), format!("{r:.0}"));
                // only a real change of what the fields show moves the part (not the rounding)
                if (format!("{nx:.1}"), format!("{ny:.1}"), format!("{nr:.0}")) != shown {
                    *at = typed;
                    self.places.insert(name.clone(), typed);
                    moved = true;
                }
            }
        }
        ui.horizontal(|ui| {
            if ui_kit::button(ui, tr("fission.plate.turn"), ui_kit::Button::Small).clicked()
                && let Some(p) = self.places.get_mut(&name)
            {
                p.2 = (p.2 + 90.0).rem_euclid(360.0);
                moved = true;
            }
            if ui_kit::button(ui, tr("fission.plate.centre"), ui_kit::Button::Small).clicked()
                && let Some(p) = self.places.get_mut(&name)
            {
                (p.0, p.1) = ((bed.0 + bed.2) / 2.0, (bed.1 + bed.3) / 2.0);
                moved = true;
            }
        });
        moved
    }

    /// The point on the plate (z = 0) under a screen point of the 3D view.
    fn plate_point(cam: &solidcraft_engine::render::Camera, rect: egui::Rect, at: egui::Pos2) -> Option<[f64; 2]> {
        let (o, d) = cam.ray(f64::from(at.x - rect.left()), f64::from(at.y - rect.top()), f64::from(rect.width()), f64::from(rect.height()));
        if d.z.abs() < 1e-9 {
            return None;
        }
        let t = -o.z / d.z;
        (t.is_finite() && t > 0.0).then(|| [o.x + d.x * t, o.y + d.y * t])
    }

    /// Drag parts on the plate with the mouse (Prepare): a press on a part picks it, dragging moves
    /// it, releasing puts it down. `free`: the pointer is over the 3D view, not a panel. True when a
    /// part moved.
    pub fn drag(&mut self, ctx: &egui::Context, rect: egui::Rect, cam: &solidcraft_engine::render::Camera, free: bool) -> bool {
        let (pressed, down, pos) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos()));
        let Some(pos) = pos else { return false };
        if pressed && free && rect.contains(pos) {
            let Some(p) = Self::plate_point(cam, rect, pos) else { return false };
            // the smallest footprint under the pointer (a small part on a big one)
            let hit = self
                .placed()
                .iter()
                .filter_map(|part| part.bounds().map(|(lo, hi)| (part.name.clone(), lo, hi)))
                .filter(|(_, lo, hi)| p[0] >= lo[0] && p[0] <= hi[0] && p[1] >= lo[1] && p[1] <= hi[1])
                .min_by(|a, b| ((a.2[0] - a.1[0]) * (a.2[1] - a.1[1])).total_cmp(&((b.2[0] - b.1[0]) * (b.2[1] - b.1[1]))))
                .map(|(n, ..)| n);
            if let Some(name) = hit {
                self.selected = Some(name.clone());
                if !self.locked
                    && let Some(place) = self.places.get(&name)
                {
                    self.dragging = Some((name, p, *place));
                }
            }
            return false;
        }
        let Some((name, start, from)) = self.dragging.clone() else { return false };
        if !down {
            self.dragging = None;
            return false;
        }
        let Some(p) = Self::plate_point(cam, rect, pos) else { return false };
        let to = (from.0 + p[0] - start[0], from.1 + p[1] - start[1], from.2);
        if self.places.get(&name) == Some(&to) {
            return false;
        }
        self.places.insert(name, to);
        true
    }

    /// Nudge the selected part with the arrow keys (1 mm, 10 with Shift) and turn it with R.
    pub fn keys(&mut self, ctx: &egui::Context) -> bool {
        let Some(name) = self.selected.clone() else { return false };
        if self.locked {
            return false;
        }
        if ctx.egui_wants_keyboard_input() {
            return false;
        }
        let (mut dx, mut dy, mut dr) = (0.0, 0.0, 0.0);
        ctx.input(|i| {
            let step = if i.modifiers.shift { 10.0 } else { 1.0 };
            if i.key_pressed(egui::Key::ArrowLeft) {
                dx -= step;
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                dx += step;
            }
            if i.key_pressed(egui::Key::ArrowUp) {
                dy += step;
            }
            if i.key_pressed(egui::Key::ArrowDown) {
                dy -= step;
            }
            if i.key_pressed(egui::Key::R) {
                dr = 90.0;
            }
        });
        if (dx, dy, dr) == (0.0, 0.0, 0.0) {
            return false;
        }
        if let Some(p) = self.places.get_mut(&name) {
            p.0 += dx;
            p.1 += dy;
            p.2 = (p.2 + dr).rem_euclid(360.0);
        }
        true
    }

    /// Whether every part is on the plate (its footprint inside the plate's edge).
    pub fn off_plate(&self, bed: (f64, f64, f64, f64)) -> Vec<String> {
        self.placed()
            .iter()
            .filter(|p| {
                p.bounds().is_some_and(|(lo, hi)| lo[0] < bed.0 - 0.01 || lo[1] < bed.1 - 0.01 || hi[0] > bed.2 + 0.01 || hi[1] > bed.3 + 0.01)
            })
            .map(|p| p.name.clone())
            .collect()
    }

    /// Preview's controls: the layer slider and the legend (each line type's share; a click hides it).
    pub fn preview_controls(&mut self, ui: &mut egui::Ui, total_seconds: f64) {
        if self.loading() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui_kit::text(ui, tr("fission.plate.reading"), ui_kit::Text::Note("fission_plate_reading"));
            });
            return;
        }
        let Some(tp) = self.toolpaths.clone() else {
            let msg = self.error.clone().unwrap_or_else(|| tr("fission.bambu.slice_hint").to_string());
            ui_kit::text(ui, &msg, ui_kit::Text::Note("fission_plate_no_toolpaths"));
            return;
        };
        let layers = tp.layers.len().max(1);
        ui.horizontal(|ui| {
            let z = tp.layers.get(self.layer).map_or(0.0, |l| l.0);
            let label =
                format("fission.plate.layer", &[("n", &(self.layer + 1).to_string()), ("of", &layers.to_string()), ("z", &format!("{z:.2}"))]);
            ui_kit::text(ui, &label, ui_kit::Text::Inline);
            let mut n = self.layer;
            ui_kit::slider(ui, &mut n, 0..=layers - 1, Default::default());
            self.layer = n.min(layers - 1);
        });
        let mut mode = usize::from(self.by_filament);
        let modes = [tr("fission.plate.by_type"), tr("fission.plate.by_filament")];
        if ui_kit::segmented(ui, &mut mode, &modes, Default::default()) {
            self.by_filament = mode == 1;
        }
        // each line type's time: its share of the slicer's predicted time
        let seconds = tp.role_seconds();
        let feed_total: f64 = seconds.iter().map(|(_, s)| s).sum::<f64>().max(1e-9);
        let time_of = |role: u8| {
            let s = seconds.iter().find(|(r, _)| *r == role).map_or(0.0, |(_, s)| *s) / feed_total * total_seconds;
            let m = (s / 60.0).round() as i64;
            if m >= 60 { format!("{}h {}m", m / 60, m % 60) } else { format!("{m}m") }
        };
        ui.horizontal_wrapped(|ui| {
            for (role, share) in tp.role_share() {
                let Some((name, colour)) = ROLES.get(usize::from(role)) else { continue };
                let shown = !self.hidden.contains(&role);
                let label = if total_seconds > 0.0 {
                    format!("{} {:.0}% · {}", crate::i18n::tr_label(name), share * 100.0, time_of(role))
                } else {
                    format!("{} {:.0}%", crate::i18n::tr_label(name), share * 100.0)
                };
                let (rect, response) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::click());
                let fill = if shown { hex(colour) } else { Tokens::get().border };
                ui.painter().rect_filled(rect, 2.0, fill);
                let text = ui_kit::button(ui, label, if shown { ui_kit::Button::SmallSelected } else { ui_kit::Button::Small });
                if response.clicked() || text.clicked() {
                    if shown {
                        self.hidden.insert(role);
                    } else {
                        self.hidden.remove(&role);
                    }
                }
            }
        });
        if tp.cut_short {
            ui_kit::text(ui, tr("fission.plate.cut_short"), ui_kit::Text::Note("fission_plate_cut_short"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plate_is_drawn_with_its_parts() {
        let cube = solidcraft_engine::cmd::bambu::Part {
            name: "Cube".into(),
            vertices: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
            triangles: vec![[0, 1, 2]],
            color: "#ff0000".into(),
            filament: 1,
        };
        let mut plate = Plate { parts: vec![cube], ..Default::default() };
        plate.places.insert("Cube".into(), (50.0, 50.0, 90.0));
        let placed = plate.placed();
        let (lo, hi) = placed[0].bounds().unwrap_or_default();
        assert!(((lo[0] + hi[0]) / 2.0 - 50.0).abs() < 1e-9 && ((lo[1] + hi[1]) / 2.0 - 50.0).abs() < 1e-9);
        let bed = (0.0, 0.0, 100.0, 100.0);
        let sc = plate.scene(false, bed, 100.0, &BTreeMap::new(), &[]);
        assert!(!sc.tris.is_empty() && !sc.trans.is_empty() && !sc.lines.is_empty());
        assert!(plate.off_plate(bed).is_empty());
        plate.places.insert("Cube".into(), (99.0, 50.0, 0.0));
        assert_eq!(plate.off_plate(bed), vec!["Cube".to_string()]);
        plate.arrange(bed);
        assert!(plate.off_plate(bed).is_empty());
        // the key follows what's drawn
        let k = plate.scene_key(false, bed, &BTreeMap::new());
        plate.selected = Some("Cube".into());
        assert_ne!(k, plate.scene_key(false, bed, &BTreeMap::new()));
    }
}

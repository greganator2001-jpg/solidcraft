//! A sliced plate's G-code as toolpaths, for the layer-by-layer Preview (like Bambu Studio's).
//!
//! Bambu Studio marks its G-code with comments this reads:
//! `; CHANGE_LAYER`, `; Z_HEIGHT: 0.4`, `; LAYER_HEIGHT: 0.2` (a new layer), `; FEATURE: Outer wall`
//! (what the next moves are), `; LINE_WIDTH: 0.42`; the moves are G0/G1, and G2/G3 arcs when arc
//! fitting is on, with relative extrusion (M83).

use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::Result;

/// Bambu Studio's line types, in its legend's order, with their preview colours.
pub const ROLES: [(&str, &str); 19] = [
    ("Inner wall", "#FFE64D"),
    ("Outer wall", "#FF7D38"),
    ("Overhang wall", "#1F1FFF"),
    ("Sparse infill", "#B03029"),
    ("Internal solid infill", "#9654CC"),
    ("Top surface", "#F04040"),
    ("Bottom surface", "#66A5CC"),
    ("Ironing", "#FF8C69"),
    ("Bridge", "#4D80BA"),
    ("Gap infill", "#FFFFFF"),
    ("Skirt", "#00876E"),
    ("Brim", "#00876E"),
    ("Support", "#00FF00"),
    ("Support interface", "#008000"),
    ("Support transition", "#004000"),
    ("Prime tower", "#B3E3AB"),
    ("Floating vertical shell", "#9654CC"),
    ("Support ironing", "#FF8C69"),
    ("Custom", "#5ED194"),
];
const CUSTOM: u8 = 18;
/// Most segments kept (a huge print is shown up to here).
pub const MAX_SEGMENTS: usize = 6_000_000;
/// Biggest G-code read.
const MAX_GCODE: u64 = 1 << 30;

/// One extruded move.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Segment {
    pub a: [f32; 3],
    pub b: [f32; 3],
    /// Index into [`ROLES`].
    pub role: u8,
    /// Filament, from 0.
    pub tool: u8,
    /// Line width (mm).
    pub width: f32,
    /// Feed rate (mm/min).
    pub feed: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Toolpaths {
    pub segments: Vec<Segment>,
    /// Each layer's top Z and where its segments start in `segments`.
    pub layers: Vec<(f32, usize)>,
    /// Filaments used, from 0.
    pub tools: Vec<u8>,
    /// The G-code had more moves than [`MAX_SEGMENTS`].
    pub cut_short: bool,
}

impl Toolpaths {
    /// The segments of layers `0..=last`.
    pub fn up_to(&self, last: usize) -> &[Segment] {
        let end = self.layers.get(last + 1).map_or(self.segments.len(), |l| l.1).min(self.segments.len());
        self.segments.get(..end).unwrap_or(&[])
    }

    /// The segments of layer `n` alone.
    pub fn layer(&self, n: usize) -> &[Segment] {
        let start = self.layers.get(n).map_or(0, |l| l.1).min(self.segments.len());
        let end = self.layers.get(n + 1).map_or(self.segments.len(), |l| l.1).min(self.segments.len());
        self.segments.get(start..end.max(start)).unwrap_or(&[])
    }

    /// Each line type's share of the extruded length, by [`ROLES`] index.
    pub fn role_share(&self) -> Vec<(u8, f64)> {
        let mut len = [0.0f64; ROLES.len()];
        for s in &self.segments {
            if let Some(l) = len.get_mut(usize::from(s.role)) {
                *l += f64::from(dist(s.a, s.b));
            }
        }
        let total: f64 = len.iter().sum::<f64>().max(1e-9);
        len.iter().enumerate().filter(|(_, l)| **l > 0.0).map(|(i, l)| (i as u8, l / total)).collect()
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn role_of(name: &str) -> u8 {
    ROLES.iter().position(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map_or(CUSTOM, |i| i as u8)
}

/// The words of a move: letter → value.
fn words(code: &str) -> [Option<f32>; 9] {
    // X Y Z E F I J R (and an unused slot)
    let mut out = [None; 9];
    let b = code.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let slot = match b[i].to_ascii_uppercase() {
            b'X' => Some(0),
            b'Y' => Some(1),
            b'Z' => Some(2),
            b'E' => Some(3),
            b'F' => Some(4),
            b'I' => Some(5),
            b'J' => Some(6),
            b'R' => Some(7),
            _ => None,
        };
        i += 1;
        if let Some(slot) = slot {
            let start = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.' || b[i] == b'-' || b[i] == b'+') {
                i += 1;
            }
            if let Some(v) = code.get(start..i).and_then(|t| t.parse::<f32>().ok()).filter(|v| v.is_finite())
                && let Some(o) = out.get_mut(slot)
            {
                *o = Some(v);
            }
        }
    }
    out
}

/// Points along an arc from `a` to `b` around `a + (i, j)` (the end point included).
fn arc(a: [f32; 3], b: [f32; 3], i: f32, j: f32, clockwise: bool) -> Vec<[f32; 3]> {
    let (cx, cy) = (a[0] + i, a[1] + j);
    let r = (a[0] - cx).hypot(a[1] - cy);
    if !(r.is_finite() && r > 1e-6) {
        return vec![b];
    }
    let a0 = (a[1] - cy).atan2(a[0] - cx);
    let a1 = (b[1] - cy).atan2(b[0] - cx);
    let mut sweep = a1 - a0;
    let tau = std::f32::consts::TAU;
    if clockwise && sweep >= 0.0 {
        sweep -= tau;
    } else if !clockwise && sweep <= 0.0 {
        sweep += tau;
    }
    let steps = ((sweep.abs() / 12f32.to_radians()).ceil() as usize).clamp(1, 64);
    (1..=steps)
        .map(|k| {
            if k == steps {
                return b;
            }
            let t = k as f32 / steps as f32;
            let ang = a0 + sweep * t;
            [cx + r * ang.cos(), cy + r * ang.sin(), a[2] + (b[2] - a[2]) * t]
        })
        .collect()
}

/// Read G-code (a `.gcode`, or the plate's in a sliced `.gcode.3mf`).
pub fn read(path: &Path, plate: u32) -> Result<Toolpaths> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let f = std::fs::File::open(path).map_err(|e| format!("couldn't open {} ({e})", path.display()))?;
    if name.ends_with(".gcode") {
        return Ok(parse(BufReader::new(std::io::Read::take(f, MAX_GCODE))));
    }
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("couldn't read the sliced file ({e})"))?;
    let entry = z.by_name(&format!("Metadata/plate_{plate}.gcode")).map_err(|_| "the sliced file has no G-code in it".to_string())?;
    Ok(parse(BufReader::new(std::io::Read::take(entry, MAX_GCODE))))
}

/// Parse G-code text, line by line.
pub fn parse(reader: impl BufRead) -> Toolpaths {
    let mut tp = Toolpaths::default();
    let (mut x, mut y, mut z) = (0f32, 0f32, 0f32);
    let (mut width, mut feed) = (0.42f32, 3000f32);
    let mut e_abs = 0f32;
    let (mut rel_e, mut rel_xyz) = (true, false);
    let mut role = CUSTOM;
    let mut tool = 0u8;
    let mut tagged = false;
    let mut last_layer_z: Option<f32> = None;
    for line in reader.split(b'\n') {
        let Ok(raw) = line else { break };
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(c) = line.strip_prefix(';') {
            let c = c.trim();
            if let Some(f) = c.strip_prefix("FEATURE:") {
                role = role_of(f);
            } else if c == "CHANGE_LAYER" {
                tagged = true;
                tp.layers.push((z, tp.segments.len()));
            } else if let Some(w) = c.strip_prefix("LINE_WIDTH:") {
                width = w.trim().parse().ok().filter(|w: &f32| w.is_finite() && *w > 0.0 && *w < 10.0).unwrap_or(width);
            } else if let Some(h) = c.strip_prefix("Z_HEIGHT:")
                && let Some(last) = tp.layers.last_mut()
                && let Ok(h) = h.trim().parse::<f32>()
            {
                last.0 = h;
            }
            continue;
        }
        let code = line.split(';').next().unwrap_or("").trim();
        let head: String = code.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
        match head.as_str() {
            "G0" | "G1" | "G2" | "G3" => {
                let w = words(&code[head.len().min(code.len())..]);
                let pick = |v: Option<f32>, cur: f32| v.map_or(cur, |v| if rel_xyz { cur + v } else { v });
                let (nx, ny, nz) = (pick(w[0], x), pick(w[1], y), pick(w[2], z));
                if let Some(f) = w[4].filter(|f| *f > 0.0) {
                    feed = f;
                }
                let de = match w[3] {
                    Some(e) if rel_e => e,
                    Some(e) => {
                        let d = e - e_abs;
                        e_abs = e;
                        d
                    }
                    None => 0.0,
                };
                if de > 1e-6 && head != "G0" {
                    if !tagged && last_layer_z.is_none_or(|lz| nz > lz + 1e-4) {
                        tp.layers.push((nz, tp.segments.len()));
                        last_layer_z = Some(nz);
                    }
                    let end = [nx, ny, nz];
                    let points = if (head == "G2" || head == "G3") && (w[5].is_some() || w[6].is_some()) {
                        arc([x, y, z], end, w[5].unwrap_or(0.0), w[6].unwrap_or(0.0), head == "G2")
                    } else {
                        vec![end]
                    };
                    let mut prev = [x, y, z];
                    for p in points {
                        if tp.segments.len() >= MAX_SEGMENTS {
                            tp.cut_short = true;
                            break;
                        }
                        tp.segments.push(Segment { a: prev, b: p, role, tool, width, feed });
                        prev = p;
                    }
                    if !tp.tools.contains(&tool) {
                        tp.tools.push(tool);
                    }
                }
                (x, y, z) = (nx, ny, nz);
            }
            "M83" => rel_e = true,
            "M82" => rel_e = false,
            "G90" => rel_xyz = false,
            "G91" => rel_xyz = true,
            "G92" => {
                if let Some(e) = words(code.get(3..).unwrap_or(""))[3] {
                    e_abs = e;
                }
            }
            h if h.len() > 1 && h.starts_with('T') => {
                if let Ok(t) = h[1..].parse::<u8>()
                    && t < 64
                {
                    tool = t;
                }
            }
            _ => {}
        }
        if tp.cut_short {
            break;
        }
    }
    // a layer with nothing extruded in it (a CHANGE_LAYER before the end) is kept: it's short
    tp.tools.sort_unstable();
    tp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_layers_features_tools_and_arcs() {
        let g = "\
M83
G1 X0 Y0 Z0.2 F3000
; CHANGE_LAYER
; Z_HEIGHT: 0.2
; FEATURE: Outer wall
; LINE_WIDTH: 0.45
G1 X10 Y0 E0.5
G1 X10 Y10 E0.5
G0 X20 Y20
T1
; FEATURE: Sparse infill
G2 X30 Y20 I5 J0 E1
; CHANGE_LAYER
; Z_HEIGHT: 0.4
G1 Z0.4
G1 X0 Y0 E0.3 ; back
G1 X5 Y5 E-0.8 ; a retraction is no line
";
        let tp = parse(g.as_bytes());
        assert_eq!(tp.layers.len(), 2);
        assert_eq!(tp.layers[1].0, 0.4);
        let first = tp.layer(0);
        assert_eq!(first[0].role, 1, "outer wall");
        assert!((first[0].width - 0.45).abs() < 1e-6);
        // the half circle from (20,20) to (30,20) clockwise: several segments, each on the circle
        let arcs: Vec<&Segment> = first.iter().filter(|s| s.tool == 1).collect();
        assert!(arcs.len() > 4 && arcs.iter().all(|s| s.role == 3));
        for s in &arcs {
            assert!((((s.b[0] - 25.0).powi(2) + (s.b[1] - 20.0).powi(2)).sqrt() - 5.0).abs() < 1e-3);
        }
        assert_eq!(tp.layer(1).len(), 1);
        assert_eq!(tp.up_to(0).len(), first.len());
        assert_eq!(tp.tools, vec![0, 1]);
        let share: f64 = tp.role_share().iter().map(|(_, s)| s).sum();
        assert!((share - 1.0).abs() < 1e-9);
        // hostile input: garbage, huge numbers, a NaN
        let tp = parse("G1 Xnan Y1e40 E1\nG1 X-- E\nT999\n\u{0}\u{1}".as_bytes());
        assert!(tp.segments.iter().all(|s| s.a.iter().chain(&s.b).all(|v| v.is_finite())));
        assert!(tp.layer(5).is_empty() && tp.up_to(9).len() == tp.segments.len());
    }
}

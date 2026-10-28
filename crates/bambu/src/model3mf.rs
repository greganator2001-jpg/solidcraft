//! The plate as a 3MF that Bambu Studio's slicer reads: one object per part, each on its filament
//! (`Metadata/model_settings.config` and the 3MF build item's extruder), with its colour.

use std::io::Write;

use crate::Result;

/// One part on the plate: a closed triangle mesh in millimetres, already where it goes on the plate.
#[derive(Clone, Debug, Default)]
pub struct Part {
    pub name: String,
    pub vertices: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// '#rrggbb'
    pub color: String,
    /// Filament slot, 1-based.
    pub filament: u32,
}

impl Part {
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let mut it = self.vertices.iter();
        let first = *it.next()?;
        Some(it.fold((first, first), |(lo, hi), v| {
            ([lo[0].min(v[0]), lo[1].min(v[1]), lo[2].min(v[2])], [hi[0].max(v[0]), hi[1].max(v[1]), hi[2].max(v[2])])
        }))
    }
}

/// Move the parts together so they sit centred on the bed, on the plate (lowest point at z = 0).
pub fn centre_on_bed(parts: &mut [Part], bed: (f64, f64, f64, f64)) {
    let Some((lo, hi)) = bounds_of(parts) else { return };
    let (x0, y0, x1, y1) = bed;
    let d = [(x0 + x1) / 2.0 - (lo[0] + hi[0]) / 2.0, (y0 + y1) / 2.0 - (lo[1] + hi[1]) / 2.0, -lo[2]];
    for p in parts.iter_mut() {
        for v in &mut p.vertices {
            v[0] += d[0];
            v[1] += d[1];
            v[2] += d[2];
        }
    }
}

/// Where a part sits on the plate and how it's turned and scaled: `orient` (a rotation, applied
/// first), then `scale`, then `turn` degrees about Z, all around the middle of the part; then the
/// middle of its footprint goes to (`x`, `y`) and its lowest point onto the plate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub turn: f64,
    pub orient: [[f64; 3]; 3],
    pub scale: f64,
}

pub const IDENTITY: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl Default for Placement {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, turn: 0.0, orient: IDENTITY, scale: 1.0 }
    }
}

fn mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

fn apply(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// A turn of `deg` degrees about Z.
pub fn about_z(deg: f64) -> [[f64; 3]; 3] {
    let (s, c) = if deg.is_finite() { deg.to_radians().sin_cos() } else { (0.0, 1.0) };
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// A turn of `deg` degrees about X (`axis` 0) or Y (`axis` 1).
pub fn about_axis(axis: usize, deg: f64) -> [[f64; 3]; 3] {
    let (s, c) = if deg.is_finite() { deg.to_radians().sin_cos() } else { (0.0, 1.0) };
    if axis == 0 { [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]] } else { [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]] }
}

/// The rotation that turns unit direction `a` onto unit direction `b`.
pub fn rot_between(a: [f64; 3], b: [f64; 3]) -> [[f64; 3]; 3] {
    let n = |v: [f64; 3]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if l > 1e-12 && l.is_finite() { v.map(|x| x / l) } else { [0.0, 0.0, -1.0] }
    };
    let (a, b) = (n(a), n(b));
    let v = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let c = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    if c < -1.0 + 1e-9 {
        // opposite: half a turn about any axis square to `a`
        let axis = if a[0].abs() < 0.9 { n([0.0, -a[2], a[1]]) } else { n([-a[2], 0.0, a[0]]) };
        return [0, 1, 2].map(|i| [0, 1, 2].map(|j| 2.0 * axis[i] * axis[j] - if i == j { 1.0 } else { 0.0 }));
    }
    let k = 1.0 / (1.0 + c);
    let vx = [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]];
    let vx2 = mul(&vx, &vx);
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| if i == j { 1.0 } else { 0.0 } + vx[i][j] + vx2[i][j] * k))
}

impl Placement {
    /// Put the face whose outward direction (as the part is placed now) is `normal` down on the plate.
    pub fn lay_on(&mut self, normal: [f64; 3]) {
        let now = mul(&about_z(self.turn), &self.orient);
        self.orient = mul(&rot_between(normal, [0.0, 0.0, -1.0]), &now);
        self.turn = 0.0;
    }

    /// Turn the part `deg` degrees about the plate's X (`axis` 0) or Y (`axis` 1).
    pub fn tip(&mut self, axis: usize, deg: f64) {
        let now = mul(&about_z(self.turn), &self.orient);
        self.orient = mul(&about_axis(axis.min(1), deg), &now);
        self.turn = 0.0;
    }
}

/// Place a part as `at` says (see [`Placement`]).
pub fn place_with(part: &mut Part, at: &Placement) {
    let Some((lo, hi)) = part.bounds() else { return };
    if !(at.x.is_finite() && at.y.is_finite()) {
        return;
    }
    let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let scale = if at.scale.is_finite() && at.scale > 1e-6 { at.scale.min(1e4) } else { 1.0 };
    let m = mul(&about_z(at.turn), &at.orient);
    if m.iter().flatten().any(|v| !v.is_finite()) {
        return;
    }
    for v in &mut part.vertices {
        let d = [v[0] - c[0], v[1] - c[1], v[2] - c[2]].map(|x| x * scale);
        *v = apply(&m, d);
    }
    let Some((lo, hi)) = part.bounds() else { return };
    let (mx, my) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
    for v in &mut part.vertices {
        v[0] += at.x - mx;
        v[1] += at.y - my;
        v[2] -= lo[2];
    }
}

/// Put a part on the plate: turned `rotation` degrees about Z around the middle of its footprint,
/// that middle moved to (`x`, `y`), and its lowest point on the plate (z = 0).
pub fn place(part: &mut Part, x: f64, y: f64, rotation: f64) {
    place_with(part, &Placement { x, y, turn: rotation, ..Placement::default() });
}

/// The outward direction of the biggest flat area of a part (as placed): the face it stands
/// steadiest on, what Auto orient puts down.
pub fn best_bottom(part: &Part) -> [f64; 3] {
    let mut groups: Vec<([f64; 3], f64)> = Vec::new();
    for t in &part.triangles {
        let Some(v) = t.iter().map(|&i| part.vertices.get(i as usize).copied()).collect::<Option<Vec<[f64; 3]>>>() else { continue };
        let (a, b) = ([v[1][0] - v[0][0], v[1][1] - v[0][1], v[1][2] - v[0][2]], [v[2][0] - v[0][0], v[2][1] - v[0][1], v[2][2] - v[0][2]]);
        let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if !(len > 1e-12 && len.is_finite()) {
            continue;
        }
        let u = n.map(|x| x / len);
        let key = u.map(|x| (x * 50.0).round() / 50.0);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some(g) => g.1 += len / 2.0,
            None => groups.push((key, len / 2.0)),
        }
    }
    groups.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map_or([0.0, 0.0, -1.0], |(k, _)| k)
}

/// Lay the parts out in rows on the plate, `gap` mm apart, the rows centred: (name, x, y) of each
/// part's footprint middle. Parts that don't fit keep going past the plate's far edge.
pub fn arrange(parts: &[Part], bed: (f64, f64, f64, f64), gap: f64) -> Vec<(String, f64, f64)> {
    let (x0, y0, x1, y1) = bed;
    let mut sized: Vec<(String, f64, f64)> =
        parts.iter().filter_map(|p| p.bounds().map(|(lo, hi)| (p.name.clone(), hi[0] - lo[0], hi[1] - lo[1]))).collect();
    // tallest footprints first, like a shelf
    sized.sort_by(|a, b| b.2.total_cmp(&a.2));
    let width = x1 - x0;
    let mut rows: Vec<Vec<(String, f64, f64)>> = vec![Vec::new()];
    let mut used = 0.0;
    for p in sized {
        if rows.last().is_some_and(|r| !r.is_empty()) && used + gap + p.1 > width {
            rows.push(Vec::new());
            used = 0.0;
        }
        used += if used > 0.0 { gap } else { 0.0 } + p.1;
        if let Some(row) = rows.last_mut() {
            row.push(p);
        }
    }
    let heights: Vec<f64> = rows.iter().map(|r| r.iter().map(|p| p.2).fold(0.0, f64::max)).collect();
    let total: f64 = heights.iter().sum::<f64>() + gap * (rows.len().saturating_sub(1)) as f64;
    let mut y = (y0 + y1) / 2.0 + total / 2.0;
    let mut out = Vec::new();
    for (row, h) in rows.iter().zip(&heights) {
        let row_width: f64 = row.iter().map(|p| p.1).sum::<f64>() + gap * (row.len().saturating_sub(1)) as f64;
        let mut x = (x0 + x1) / 2.0 - row_width / 2.0;
        for (name, w, _) in row {
            out.push((name.clone(), x + w / 2.0, y - h / 2.0));
            x += w + gap;
        }
        y -= h + gap;
    }
    out
}

pub fn bounds_of(parts: &[Part]) -> Option<([f64; 3], [f64; 3])> {
    parts.iter().filter_map(Part::bounds).reduce(|(alo, ahi), (blo, bhi)| {
        ([alo[0].min(blo[0]), alo[1].min(blo[1]), alo[2].min(blo[2])], [ahi[0].max(bhi[0]), ahi[1].max(bhi[1]), ahi[2].max(bhi[2])])
    })
}

fn esc(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

fn hex(c: &str) -> String {
    let h: String = c.trim().trim_start_matches('#').chars().filter(|c| c.is_ascii_hexdigit()).take(6).collect();
    if h.len() == 6 { format!("#{}FF", h.to_uppercase()) } else { "#CCCCCCFF".into() }
}

/// The 3MF bytes.
pub fn write(parts: &[Part]) -> Result<Vec<u8>> {
    let parts: Vec<&Part> = parts.iter().filter(|p| !p.triangles.is_empty()).collect();
    if parts.is_empty() {
        return Err("there's nothing on the plate to print".into());
    }
    let mut names: Vec<String> = Vec::new();
    for p in &parts {
        let base = if p.name.trim().is_empty() { "Body".to_string() } else { p.name.clone() };
        let mut n = base.clone();
        let mut k = 2;
        while names.contains(&n) {
            n = format!("{base} ({k})");
            k += 1;
        }
        names.push(n);
    }
    let mut palette: Vec<String> = Vec::new();
    for p in &parts {
        let h = hex(&p.color);
        if !palette.contains(&h) {
            palette.push(h);
        }
    }
    let cg = parts.len() + 1;
    let mut m = String::with_capacity(1 << 20);
    m.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" xmlns:m=\"http://schemas.microsoft.com/3dmanufacturing/material/2015/02\" xmlns:slic3rpe=\"http://schemas.slic3r.org/3mf/2017/06\">\n");
    m.push_str(
        " <metadata name=\"Application\">Fission (SolidCraft)</metadata>\n <metadata name=\"BambuStudio:3mfVersion\">1</metadata>\n <resources>\n",
    );
    m.push_str(&format!("  <m:colorgroup id=\"{cg}\">\n"));
    for c in &palette {
        m.push_str(&format!("   <m:color color=\"{c}\"/>\n"));
    }
    m.push_str("  </m:colorgroup>\n");
    for (i, (p, n)) in parts.iter().zip(&names).enumerate() {
        let pi = palette.iter().position(|c| *c == hex(&p.color)).unwrap_or(0);
        let nv = p.vertices.len() as u64;
        m.push_str(&format!("  <object id=\"{}\" type=\"model\" name=\"{}\">\n   <mesh>\n    <vertices>\n", i + 1, esc(n)));
        for v in &p.vertices {
            if !(v[0].is_finite() && v[1].is_finite() && v[2].is_finite()) {
                return Err(format!("{n} has a broken point in its mesh"));
            }
            m.push_str(&format!("     <vertex x=\"{:.5}\" y=\"{:.5}\" z=\"{:.5}\"/>\n", v[0], v[1], v[2]));
        }
        m.push_str("    </vertices>\n    <triangles>\n");
        for t in &p.triangles {
            if t.iter().any(|&k| u64::from(k) >= nv) {
                return Err(format!("{n} has a broken triangle in its mesh"));
            }
            m.push_str(&format!("     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\" pid=\"{cg}\" p1=\"{pi}\"/>\n", t[0], t[1], t[2]));
        }
        m.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    m.push_str(" </resources>\n <build>\n");
    for (i, p) in parts.iter().enumerate() {
        m.push_str(&format!("  <item objectid=\"{}\" transform=\"1 0 0 0 1 0 0 0 1 0 0 0\" slic3rpe:extruder=\"{}\"/>\n", i + 1, p.filament.max(1)));
    }
    m.push_str(" </build>\n</model>\n");

    let mut cfg = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<config>\n");
    for (i, (p, n)) in parts.iter().zip(&names).enumerate() {
        cfg.push_str(&format!(
            "  <object id=\"{}\">\n    <metadata key=\"name\" value=\"{}\"/>\n    <metadata key=\"extruder\" value=\"{}\"/>\n  </object>\n",
            i + 1,
            esc(n),
            p.filament.max(1)
        ));
    }
    cfg.push_str("</config>\n");

    let types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/><Default Extension=\"config\" ContentType=\"text/xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Target=\"/3D/3dmodel.model\" Id=\"rel0\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/></Relationships>";

    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let err = |e: &dyn std::fmt::Display| format!("couldn't write the 3MF ({e})");
        for (name, body) in [
            ("[Content_Types].xml", types.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
            ("3D/3dmodel.model", m.as_bytes()),
            ("Metadata/model_settings.config", cfg.as_bytes()),
        ] {
            z.start_file(name, opt).map_err(|e| err(&e))?;
            z.write_all(body).map_err(|e| err(&e))?;
        }
        z.finish().map_err(|e| err(&e))?;
    }
    Ok(buf.into_inner())
}

/// A 20 mm test cube in the middle of the bed (to tell a model problem from a setup problem).
pub fn test_cube(bed: (f64, f64, f64, f64)) -> Part {
    let (x0, y0, x1, y1) = bed;
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let mut vertices = Vec::new();
    for z in [0.0, 20.0] {
        for sy in [-1.0, 1.0] {
            for sx in [-1.0, 1.0] {
                vertices.push([cx + sx * 10.0, cy + sy * 10.0, z]);
            }
        }
    }
    let triangles =
        vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
    Part { name: "Test cube".into(), vertices, triangles, color: "#CCCCCC".into(), filament: 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn writes_objects_on_their_filaments() {
        let mut a = test_cube((0.0, 0.0, 100.0, 100.0));
        a.name = "Plate & <Lid>".into();
        let mut b = test_cube((0.0, 0.0, 100.0, 100.0));
        b.name = "Letters".into();
        b.color = "#c02a20".into();
        b.filament = 2;
        let mut parts = vec![a, b];
        centre_on_bed(&mut parts, (0.0, 0.0, 256.0, 256.0));
        let (lo, hi) = bounds_of(&parts).unwrap_or_default();
        assert!((lo[0] + hi[0] - 256.0).abs() < 1e-9 && lo[2].abs() < 1e-12);
        let bytes = write(&parts).unwrap_or_default();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
        let mut model = String::new();
        z.by_name("3D/3dmodel.model").expect("model").read_to_string(&mut model).ok();
        assert!(model.contains("name=\"Plate &amp; &lt;Lid&gt;\"") && model.contains("slic3rpe:extruder=\"2\""));
        assert!(model.contains("#C02A20FF"));
        let mut cfg = String::new();
        z.by_name("Metadata/model_settings.config").expect("cfg").read_to_string(&mut cfg).ok();
        assert!(cfg.contains("<metadata key=\"extruder\" value=\"2\"/>"));
        // hostile input: an empty plate, a triangle pointing past the vertices, a NaN point
        assert!(write(&[]).is_err());
        let mut bad = test_cube((0.0, 0.0, 10.0, 10.0));
        bad.triangles.push([0, 1, 99]);
        assert!(write(&[bad]).is_err());
        // placing: a 20 mm cube turned 45° is about 28.3 mm across, centred where asked, on the plate
        let mut c = test_cube((0.0, 0.0, 100.0, 100.0));
        place(&mut c, 30.0, 40.0, 45.0);
        let (lo, hi) = c.bounds().unwrap_or_default();
        assert!(((hi[0] - lo[0]) - 20.0 * 2f64.sqrt()).abs() < 1e-6 && ((lo[0] + hi[0]) / 2.0 - 30.0).abs() < 1e-9);
        assert!(((lo[1] + hi[1]) / 2.0 - 40.0).abs() < 1e-9 && lo[2].abs() < 1e-12);
        place(&mut c, f64::NAN, 0.0, 0.0);
        assert!(c.vertices.iter().flatten().all(|v| v.is_finite()));
        // laying a cube on its side, then the biggest flat face of a flat plate goes down
        let mut cube = test_cube((0.0, 0.0, 100.0, 100.0));
        let mut at = Placement { x: 50.0, y: 50.0, ..Placement::default() };
        at.lay_on([1.0, 0.0, 0.0]);
        place_with(&mut cube, &at);
        let (lo, hi) = cube.bounds().unwrap_or_default();
        assert!((hi[2] - lo[2] - 20.0).abs() < 1e-9 && lo[2].abs() < 1e-12);
        let mut slab = test_cube((0.0, 0.0, 100.0, 100.0));
        slab.vertices.iter_mut().for_each(|v| v[0] *= 5.0);
        let mut at = Placement { x: 50.0, y: 50.0, ..Placement::default() };
        at.tip(0, 90.0);
        place_with(&mut slab, &at);
        let n = best_bottom(&slab);
        at.lay_on(n);
        let mut slab2 = test_cube((0.0, 0.0, 100.0, 100.0));
        slab2.vertices.iter_mut().for_each(|v| v[0] *= 5.0);
        place_with(&mut slab2, &at);
        let (lo, hi) = slab2.bounds().unwrap_or_default();
        assert!((hi[2] - lo[2] - 20.0).abs() < 1e-6, "laid on a big face: {lo:?} {hi:?}");
        // scale doubles the size; a broken placement changes nothing
        let mut big = test_cube((0.0, 0.0, 100.0, 100.0));
        place_with(&mut big, &Placement { x: 50.0, y: 50.0, scale: 2.0, ..Placement::default() });
        let (lo, hi) = big.bounds().unwrap_or_default();
        assert!((hi[0] - lo[0] - 40.0).abs() < 1e-9);
        let before = big.vertices.clone();
        place_with(&mut big, &Placement { x: 1.0, y: 1.0, orient: [[f64::NAN; 3]; 3], ..Placement::default() });
        assert_eq!(before, big.vertices);
        // arranging three cubes on a 50 mm wide plate: two in a row, one below
        let cubes: Vec<Part> = (0..3).map(|i| Part { name: format!("c{i}"), ..test_cube((0.0, 0.0, 100.0, 100.0)) }).collect();
        let spots = arrange(&cubes, (0.0, 0.0, 50.0, 100.0), 5.0);
        assert_eq!(spots.len(), 3);
        assert!((spots[0].2 - spots[1].2).abs() < 1e-9 && spots[2].2 < spots[0].2 - 20.0);
        let mut nan = test_cube((0.0, 0.0, 10.0, 10.0));
        nan.vertices[0][0] = f64::NAN;
        assert!(write(&[nan]).is_err());
    }
}

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
        let mut nan = test_cube((0.0, 0.0, 10.0, 10.0));
        nan.vertices[0][0] = f64::NAN;
        assert!(write(&[nan]).is_err());
    }
}

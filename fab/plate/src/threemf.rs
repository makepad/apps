//! 3MF export (3D Manufacturing Format core specification): an OPC zip
//! package with one model part holding every plate part as its own object,
//! in millimetres, with a base-material entry per filament/colour so each
//! object keeps its material and display colour.
//!
//! Package layout:
//! - `[Content_Types].xml`: the `rels` and `model` content types.
//! - `_rels/.rels`: the start-part relationship to `/3D/3dmodel.model`.
//! - `3D/3dmodel.model`: `<model unit="millimeter">` with `<basematerials>`,
//!   one `<object type="model">` per part (`pid`/`pindex` naming its material)
//!   and one `<build><item>` per object. Parts are already in printer
//!   coordinates, so items carry no transform.

use crate::{fmt_mm, Part, Plate};
use makepad_zip_file::writer::{ZipMethod, ZipWriteError, ZipWriter};

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
 <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
 <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>
"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>
"#;

pub const MODEL_PATH: &str = "3D/3dmodel.model";

/// Metadata written into the model part.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub title: String,
    pub designer: String,
}

/// Linear 0..1 to an sRGB byte.
fn srgb_byte(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5) as u8
}

/// `#RRGGBBAA` in sRGB, as 3MF's `displaycolor` wants it.
pub fn display_color(linear: [f32; 4]) -> String {
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        srgb_byte(linear[0]),
        srgb_byte(linear[1]),
        srgb_byte(linear[2]),
        (linear[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    )
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => out.push(c),
        }
    }
}

/// The material table: one entry per distinct (filament, colour), in first
/// use order; returns the table and each part's index into it.
fn materials(parts: &[Part]) -> (Vec<(u32, [f32; 4])>, Vec<usize>) {
    let mut table: Vec<(u32, [f32; 4])> = Vec::new();
    let mut index = Vec::with_capacity(parts.len());
    for p in parts {
        let key = (p.filament, p.color);
        let i = match table.iter().position(|m| *m == key) {
            Some(i) => i,
            None => {
                table.push(key);
                table.len() - 1
            }
        };
        index.push(i);
    }
    (table, index)
}

/// The `3D/3dmodel.model` XML for `plate`.
pub fn model_xml(plate: &Plate, info: &Info) -> String {
    let tris = plate.triangle_count();
    let mut x = String::with_capacity(1024 + tris * 60);
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str("<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n");
    x.push_str(" <metadata name=\"Application\">Makepad Fab</metadata>\n");
    if !info.title.is_empty() {
        x.push_str(" <metadata name=\"Title\">");
        escape(&info.title, &mut x);
        x.push_str("</metadata>\n");
    }
    if !info.designer.is_empty() {
        x.push_str(" <metadata name=\"Designer\">");
        escape(&info.designer, &mut x);
        x.push_str("</metadata>\n");
    }
    x.push_str(" <resources>\n");
    let (table, index) = materials(&plate.parts);
    let material_id = 1;
    if !table.is_empty() {
        x.push_str(&format!("  <basematerials id=\"{material_id}\">\n"));
        for (filament, color) in &table {
            x.push_str(&format!("   <base name=\"Filament {filament}\" displaycolor=\"{}\"/>\n", display_color(*color)));
        }
        x.push_str("  </basematerials>\n");
    }
    for (i, part) in plate.parts.iter().enumerate() {
        let id = i + 2;
        x.push_str(&format!("  <object id=\"{id}\" type=\"model\" name=\""));
        escape(&part.name, &mut x);
        x.push_str(&format!("\" pid=\"{material_id}\" pindex=\"{}\">\n   <mesh>\n    <vertices>\n", index[i]));
        for p in &part.mesh.positions {
            x.push_str("     <vertex x=\"");
            x.push_str(&fmt_mm(p[0]));
            x.push_str("\" y=\"");
            x.push_str(&fmt_mm(p[1]));
            x.push_str("\" z=\"");
            x.push_str(&fmt_mm(p[2]));
            x.push_str("\"/>\n");
        }
        x.push_str("    </vertices>\n    <triangles>\n");
        for t in &part.mesh.triangles {
            // Degenerate index triples are invalid 3MF; meshes from the
            // model are welded, so drop the collapsed ones here.
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                continue;
            }
            x.push_str(&format!("     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n", t[0], t[1], t[2]));
        }
        x.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    x.push_str(" </resources>\n <build>\n");
    for i in 0..plate.parts.len() {
        x.push_str(&format!("  <item objectid=\"{}\"/>\n", i + 2));
    }
    x.push_str(" </build>\n</model>\n");
    x
}

/// The whole 3MF package as bytes.
pub fn plate_to_3mf(plate: &Plate, info: &Info) -> Result<Vec<u8>, ZipWriteError> {
    let mut zip = ZipWriter::new();
    zip.add("[Content_Types].xml", CONTENT_TYPES.as_bytes(), ZipMethod::Deflate)?;
    zip.add("_rels/.rels", RELS.as_bytes(), ZipMethod::Deflate)?;
    zip.add(MODEL_PATH, model_xml(plate, info).as_bytes(), ZipMethod::Deflate)?;
    zip.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_cube;

    fn part(name: &str, filament: u32, color: [f32; 4], min: [f64; 3]) -> Part {
        Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: name.into(), name: name.into(), filament, color, nozzle: None, mesh: test_cube(min, 10.0), field_bounds: None, field_floor: None, ..Default::default() }
    }

    #[test]
    fn two_objects_two_materials() {
        let plate = Plate {
            excluded: Vec::new(), settings: Vec::new(),
            parts: vec![
                part("base <a&b>", 1, [1.0, 0.0, 0.0, 1.0], [100.0, 100.0, 0.0]),
                part("knob", 2, [0.0, 0.0, 1.0, 1.0], [130.0, 100.0, 0.0]),
                part("knob2", 2, [0.0, 0.0, 1.0, 1.0], [160.0, 100.0, 0.0]),
            ],
        };
        let xml = model_xml(&plate, &Info { title: "Test".into(), designer: String::new() });
        assert!(xml.contains("unit=\"millimeter\""));
        assert!(xml.contains("name=\"base &lt;a&amp;b&gt;\""));
        assert_eq!(xml.matches("<base ").count(), 2, "{xml}");
        assert!(xml.contains("displaycolor=\"#FF0000FF\""));
        assert!(xml.contains("<object id=\"4\" type=\"model\" name=\"knob2\" pid=\"1\" pindex=\"1\">"));
        assert_eq!(xml.matches("<item objectid=").count(), 3);
        assert_eq!(xml.matches("<triangle ").count(), 36);
        assert!(xml.contains("<vertex x=\"110\" y=\"100\" z=\"0\"/>"));

        let zip = plate_to_3mf(&plate, &Info::default()).unwrap();
        let mut cursor = std::io::Cursor::new(&zip);
        let dir = makepad_zip_file::zip_read_central_directory(&mut cursor).unwrap();
        let names: Vec<&str> = dir.file_headers.iter().map(|h| h.file_name.as_str()).collect();
        assert_eq!(names, vec!["[Content_Types].xml", "_rels/.rels", MODEL_PATH]);
        let model = dir.file_headers[2].extract(&mut cursor).unwrap();
        assert_eq!(String::from_utf8(model).unwrap(), model_xml(&plate, &Info::default()));
    }
}

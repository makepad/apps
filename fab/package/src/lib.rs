//! The sliced-plate package (`.gcode.3mf`, R1 §11): the plate G-code with
//! its uppercase-hex MD5, `slice_info.config`, model and plate settings,
//! project settings, thumbnails and the package relationships.

pub mod md5;
pub mod png;

use makepad_fab_gcode::packed::MovesRef;
use makepad_fab_gcode::record::{MoveKind, MoveRecord};
use makepad_fab_gcode::writer::Stats;
use makepad_fab_settings::machine::Job;
use makepad_zip_file::{Packed, ZipMethod, ZipWriter};
use png::Image;

#[derive(Clone, Debug)]
pub struct PlateObject {
    pub identify_id: u32,
    pub name: String,
    pub bbox: [f64; 4],
    pub area: f64,
}

pub struct PackageInput<'a> {
    pub job: &'a Job,
    pub gcode: &'a str,
    pub stats: &'a Stats,
    /// The plate's move records in print order, in runs (a timeline's parts, or a layer
    /// timeline's chunks): what the renders draw.
    pub moves: Vec<MovesRef<'a>>,
    pub objects: &'a [PlateObject],
    /// Per layer: the filament slots it uses (0-based).
    pub layer_filaments: &'a [Vec<usize>],
    /// The package format version the printer reads in `slice_info`.
    pub client_version: &'a str,
    /// Our application name for the model metadata.
    pub application: &'a str,
    pub printer_model_id: &'a str,
    pub date: &'a str,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn parse_colour(s: &str) -> [u8; 4] {
    let h = s.trim_start_matches('#');
    if h.len() >= 6 {
        let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(200);
        [v(0), v(2), v(4), 255]
    } else {
        [200, 200, 200, 255]
    }
}

/// Plate renders from the move records: shaded iso view (0), unlit iso
/// view (1), top view (2) and the object pick image (3). Each image is
/// drawn on its own, so the four can be drawn in parallel; every image gets
/// exactly the lines, in the same order, it would get drawn together.
fn thumbnail(inp: &PackageInput, which: usize, width: usize, height: usize) -> Image {
    // Read again for each pass, never gathered: a list of a big print's extrusions is gigabytes,
    // and the four images are drawn at once.
    let ext = || inp.moves.iter().flat_map(|run| run.iter()).filter(|m| m.kind == MoveKind::Extrude && m.filament > 0 && m.layer > 0);
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for m in ext() {
        for p in [m.p0, m.p1] {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k] as f64);
                hi[k] = hi[k].max(p[k] as f64);
            }
        }
    }
    if ext().next().is_none() {
        return Image::new(width, height);
    }
    let (c30, s30) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
    let iso = |p: [f32; 3]| -> (f64, f64, f32) {
        let (x, y, z) = (p[0] as f64, p[1] as f64, p[2] as f64);
        ((x - y) * c30, -(x + y) * s30 - z, (z + (x + y) * 0.001) as f32)
    };
    let margin = 0.08 * width.min(height) as f64;
    let colours: Vec<[u8; 4]> = (0..inp.job.filaments()).map(|f| parse_colour(inp.job.fil_str("filament_colour", f))).collect();
    let zr = (hi[2] - lo[2]).max(1e-6);
    // The iso views fit the iso projection; the top view and the pick image the plan.
    let (sc, ox, oy) = if which <= 1 {
        let mut ib = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for m in ext() {
            for p in [m.p0, m.p1] {
                let (u, v, _) = iso(p);
                ib[0] = ib[0].min(u);
                ib[1] = ib[1].min(v);
                ib[2] = ib[2].max(u);
                ib[3] = ib[3].max(v);
            }
        }
        let sc = ((width as f64 - 2.0 * margin) / (ib[2] - ib[0]).max(1e-6)).min((height as f64 - 2.0 * margin) / (ib[3] - ib[1]).max(1e-6)).min(40.0);
        (sc, width as f64 / 2.0 - (ib[0] + ib[2]) / 2.0 * sc, height as f64 / 2.0 - (ib[1] + ib[3]) / 2.0 * sc)
    } else {
        let tsc = ((width as f64 - 2.0 * margin) / (hi[0] - lo[0]).max(1e-6)).min((height as f64 - 2.0 * margin) / (hi[1] - lo[1]).max(1e-6)).min(40.0);
        (tsc, width as f64 / 2.0 - (lo[0] + hi[0]) / 2.0 * tsc, height as f64 / 2.0 + (lo[1] + hi[1]) / 2.0 * tsc)
    };
    let shaded = |m: &MoveRecord| {
        let base = colours.get(m.filament as usize - 1).copied().unwrap_or([200, 200, 200, 255]);
        let shade = 0.55 + 0.45 * ((m.p1[2] as f64 - lo[2]) / zr);
        [(base[0] as f64 * shade) as u8, (base[1] as f64 * shade) as u8, (base[2] as f64 * shade) as u8, 255]
    };
    // Every move, in order, into the band of rows from `y0`.
    let draw = |img: &mut Image, y0: i64| {
        for m in ext() {
            if which <= 1 {
                let (a, b) = (iso(m.p0), iso(m.p1));
                let a = (ox + a.0 * sc, oy + a.1 * sc, a.2);
                let b = (ox + b.0 * sc, oy + b.1 * sc, b.2);
                let r = (m.width as f64 * sc * 0.5).max(0.6);
                let c = if which == 0 { shaded(&m) } else { colours.get(m.filament as usize - 1).copied().unwrap_or([200, 200, 200, 255]) };
                img.line_band(a, b, r, c, y0);
            } else {
                let a = (ox + m.p0[0] as f64 * sc, oy - m.p0[1] as f64 * sc, m.p0[2]);
                let b = (ox + m.p1[0] as f64 * sc, oy - m.p1[1] as f64 * sc, m.p1[2]);
                let r = (m.width as f64 * sc * 0.5).max(0.6);
                let c = if which == 2 { shaded(&m) } else {
                    let id = if m.object == u16::MAX { 0 } else { m.object as u32 + 1 };
                    [(id & 0xff) as u8, ((id >> 8) & 0xff) as u8, ((id >> 16) & 0xff) as u8, 255]
                };
                img.line_band(a, b, r, c, y0);
            }
        }
    };
    // Bands of rows drawn apart, one per thread (each band passes over every move): a small
    // part fills the image with wide lines.
    let rows = height.div_ceil(makepad_fab_toolpath::par::threads().max(1)).max(32);
    let bands = makepad_fab_toolpath::par::map(height.div_ceil(rows), |bi| {
        let mut band = Image::new(width, rows.min(height - bi * rows));
        draw(&mut band, (bi * rows) as i64);
        band
    });
    Image::stacked(width, height, bands)
}

/// The plain G-code file, with one lit iso PNG for each PNG size in `thumbnails`,
/// in the configured order, immediately after the header and its blank line.
pub fn plain_gcode(inp: &PackageInput) -> Result<Vec<u8>, String> {
    let mut block = String::new();
    for entry in inp.job.cfg.str("thumbnails").split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((size, format)) = entry.split_once('/') else { continue };
        if format.trim() != "PNG" { continue; }
        let dimensions = size.trim().split_once('x').and_then(|(w, h)| Some((w.trim().parse::<usize>().ok()?, h.trim().parse::<usize>().ok()?)));
        let Some((w, h)) = dimensions.filter(|&(w, h)| w > 0 && h > 0 && u32::try_from(w).is_ok() && u32::try_from(h).is_ok() && w.checked_mul(h).and_then(|n| n.checked_mul(4)).is_some()) else {
            return Err(format!("invalid thumbnail size: {size}"));
        };
        let png = thumbnail(inp, 0, w, h).encode_png();
        let b64 = makepad_base64::base64_encode(&png, &makepad_base64::BASE64_STANDARD);
        block.push_str(&format!("; thumbnail begin {w}x{h} {}\n", b64.len()));
        for line in b64.chunks(78) {
            block.push_str("; ");
            block.push_str(std::str::from_utf8(line).map_err(|e| e.to_string())?);
            block.push('\n');
        }
        block.push_str("; thumbnail end\n");
    }
    if block.is_empty() { return Ok(inp.gcode.as_bytes().to_vec()); }
    let marker = "; HEADER_BLOCK_END\n\n";
    let at = inp.gcode.find(marker).ok_or("G-code header end is missing")? + marker.len();
    let mut out = Vec::with_capacity(inp.gcode.len() + block.len() + 64);
    out.extend_from_slice(&inp.gcode.as_bytes()[..at]);
    out.extend_from_slice(b"; THUMBNAIL_BLOCK_START\n");
    out.extend_from_slice(block.as_bytes());
    out.extend_from_slice(b"; THUMBNAIL_BLOCK_END\n\n");
    out.extend_from_slice(&inp.gcode.as_bytes()[at..]);
    Ok(out)
}

/// What `build` computes in parallel before writing the archive.
enum Part {
    Md5(String),
    /// Encoded PNGs: the image, and for the shaded view its small copy.
    Png(Vec<u8>, Option<Vec<u8>>),
    /// The G-code's CRC-32.
    Crc(u32),
    /// One part of the G-code's deflate stream.
    Deflated(Vec<u8>),
}

pub fn build(inp: &PackageInput) -> Result<Vec<u8>, String> {
    build_with(inp, &|_| {}, &|| false)
}

/// [`build`], telling `progress` the share done (0..1) as its parts finish. `stop` is asked
/// before each part: once true no part starts and the answer is a `STOPPED` error.
pub fn build_with(inp: &PackageInput, progress: &(dyn Fn(f64) + Sync), stop: &(dyn Fn() -> bool + Sync)) -> Result<Vec<u8>, String> {
    let job = inp.job;
    let cfg = &job.cfg;
    let gcode = inp.gcode.as_bytes();
    // The G-code's MD5 and CRC, the four renders (with their PNG encodes)
    // and the G-code's deflate in 1 MB parts (parts concatenate to one
    // stream) are independent of each other: one parallel map, the longest
    // items first.
    const PART: usize = 1 << 20;
    let pieces: Vec<&[u8]> = gcode.chunks(PART).collect();
    let np = pieces.len().max(1);
    let n = 6 + np;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let parts = makepad_fab_toolpath::par::map(n, |i| {
        if stop() {
            return None;
        }
        use makepad_fab_toolpath::{par::plate_span, prof::{scope, Stage}};
        let part = match i {
            0 => Part::Md5({ let _t = scope(Stage::PackageHashes); plate_span("package", || md5::md5_hex_upper(gcode)) }),
            1..=4 => {
                let img = { let _t = scope(Stage::PackageRenders); plate_span("package", || thumbnail(inp, i - 1, 512, 512)) };
                let _t = scope(Stage::PackagePngs);
                plate_span("package", || {
                    let small = (i == 1).then(|| img.downscale(4).encode_png());
                    Part::Png(img.encode_png(), small)
                })
            }
            5 => Part::Crc({ let _t = scope(Stage::PackageHashes); plate_span("package", || makepad_fast_inflate::crc32(gcode)) }),
            _ => Part::Deflated({ let _t = scope(Stage::PackageDeflate); plate_span("package", || makepad_fast_inflate::deflate::compress_part_to_vec(pieces.get(i - 6).copied().unwrap_or(&[]), 6, i + 1 == n)) }),
        };
        progress((done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1) as f64 / n as f64);
        Some(part)
    });
    let Some(mut parts) = parts.into_iter().collect::<Option<Vec<Part>>>() else {
        return Err(makepad_fab_gcode::writer::STOPPED.to_string());
    };
    let mut take = |i: usize| std::mem::replace(&mut parts[i], Part::Md5(String::new()));
    let Part::Md5(md5) = take(0) else { unreachable!() };
    let Part::Crc(crc) = take(5) else { unreachable!() };
    let deflated: Vec<u8> = (6..n).flat_map(|i| match take(i) {
        Part::Deflated(d) => d,
        _ => unreachable!(),
    }).collect();
    let gcode_packed = Packed::from_deflated(crc, deflated);
    let mut png = |i: usize| match take(i) {
        Part::Png(p, s) => (p, s.unwrap_or_default()),
        _ => unreachable!(),
    };
    let (lit, small) = png(1);
    let (unlit, _) = png(2);
    let (top, _) = png(3);
    let (pick, _) = png(4);
    let nf = job.filaments();
    let ne = job.extruders();

    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n <Default Extension=\"png\" ContentType=\"image/png\"/>\n <Default Extension=\"gcode\" ContentType=\"text/x.gcode\"/>\n</Types>\n";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel-1\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n <Relationship Target=\"/Metadata/plate_1.png\" Id=\"rel-2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail\"/>\n <Relationship Target=\"/Metadata/plate_1.png\" Id=\"rel-4\" Type=\"http://schemas.bambulab.com/package/2021/cover-thumbnail-middle\"/>\n <Relationship Target=\"/Metadata/plate_1_small.png\" Id=\"rel-5\" Type=\"http://schemas.bambulab.com/package/2021/cover-thumbnail-small\"/>\n</Relationships>\n";
    let ms_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/Metadata/plate_1.gcode\" Id=\"rel-1\" Type=\"http://schemas.bambulab.com/package/2021/gcode\"/>\n</Relationships>\n";
    let model = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" xmlns:BambuStudio=\"http://schemas.bambulab.com/package/2021\" xmlns:p=\"http://schemas.microsoft.com/3dmanufacturing/production/2015/06\" requiredextensions=\"p\">\n <metadata name=\"Application\">{}</metadata>\n <metadata name=\"BambuStudio:3mfVersion\">1</metadata>\n <metadata name=\"CreationDate\">{}</metadata>\n <metadata name=\"ModificationDate\">{}</metadata>\n <resources>\n </resources>\n <build/>\n</model>\n",
        esc(inp.application),
        inp.date,
        inp.date
    );
    let fmaps: Vec<String> = (0..nf).map(|f| (job.ext_of(f) + 1).to_string()).collect();
    let model_settings = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <plate>\n    <metadata key=\"plater_id\" value=\"1\"/>\n    <metadata key=\"plater_name\" value=\"\"/>\n    <metadata key=\"locked\" value=\"false\"/>\n    <metadata key=\"filament_map_mode\" value=\"{}\"/>\n    <metadata key=\"filament_maps\" value=\"{}\"/>\n    <metadata key=\"filament_volume_maps\" value=\"{}\"/>\n    <metadata key=\"gcode_file\" value=\"Metadata/plate_1.gcode\"/>\n    <metadata key=\"thumbnail_file\" value=\"Metadata/plate_1.png\"/>\n    <metadata key=\"thumbnail_no_light_file\" value=\"Metadata/plate_no_light_1.png\"/>\n    <metadata key=\"top_file\" value=\"Metadata/top_1.png\"/>\n    <metadata key=\"pick_file\" value=\"Metadata/pick_1.png\"/>\n    <metadata key=\"pattern_bbox_file\" value=\"Metadata/plate_1.json\"/>\n{}  </plate>\n</config>\n",
        esc(cfg.str("filament_map_mode")),
        fmaps.join(" "),
        vec!["0"; nf].join(" "),
        inp.objects.iter().enumerate().map(|(i, o)| format!("    <model_instance>\n      <metadata key=\"object_id\" value=\"{}\"/>\n      <metadata key=\"instance_id\" value=\"0\"/>\n      <metadata key=\"identify_id\" value=\"{}\"/>\n    </model_instance>\n", i + 1, o.identify_id)).collect::<String>()
    );
    // slice_info.config
    let ext_type: Vec<String> = (0..ne).map(|e| if cfg.raw("extruder_type", e) == "Bowden" { "1".into() } else { "0".into() }).collect();
    let vol_idx = |s: &str| -> &'static str {
        match s {
            "High Flow" => "1",
            "Hybrid" => "2",
            "TPU High Flow" => "3",
            "E3D High Flow" => "4",
            _ => "0",
        }
    };
    let vol: Vec<&str> = (0..ne).map(|e| vol_idx(cfg.raw("nozzle_volume_type", e))).collect();
    let nd: Vec<String> = (0..ne).map(|e| cfg.raw("nozzle_diameter", e).to_string()).collect();
    let weight: f64 = inp.stats.filament_g.iter().sum();
    let mut si = String::new();
    si.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <header>\n    <header_item key=\"X-BBL-Client-Type\" value=\"slicer\"/>\n");
    si.push_str(&format!("    <header_item key=\"X-BBL-Client-Version\" value=\"{}\"/>\n  </header>\n  <plate>\n", esc(inp.client_version)));
    let meta = |k: &str, v: &str| format!("    <metadata key=\"{k}\" value=\"{}\"/>\n", esc(v));
    si.push_str(&meta("index", "1"));
    si.push_str(&meta("extruder_type", &ext_type.join(" ")));
    si.push_str(&meta("nozzle_volume_type", &vol.join(" ")));
    si.push_str(&meta("printer_model_id", inp.printer_model_id));
    si.push_str(&meta("nozzle_diameters", &nd.join(",")));
    si.push_str(&meta("timelapse_type", cfg.str("timelapse_type")));
    si.push_str(&meta("prediction", &format!("{}", inp.stats.total_time.round() as i64)));
    si.push_str(&meta("weight", &format!("{weight:.2}")));
    si.push_str(&meta("pause_count", "0"));
    si.push_str(&meta("first_layer_time", &format!("{:.6}", inp.stats.first_layer_time)));
    si.push_str(&meta("outside", "false"));
    si.push_str(&meta("support_used", "false"));
    si.push_str(&meta("label_object_enabled", if inp.objects.len() > 1 { "true" } else { "false" }));
    si.push_str(&meta("support_material_on_wipe_tower", "false"));
    si.push_str(&meta("enable_filament_dynamic_map", "false"));
    si.push_str(&meta("has_filament_switcher", "false"));
    si.push_str(&meta("filament_maps", &fmaps.join(" ")));
    si.push_str(&meta("limit_filament_maps", &vec!["0"; nf].join(" ")));
    for o in inp.objects {
        si.push_str(&format!("    <object identify_id=\"{}\" name=\"{}\" skipped=\"false\" />\n", o.identify_id, esc(&o.name)));
    }
    for &f in &inp.stats.used {
        let e = job.ext_of(f);
        si.push_str(&format!(
            "    <filament id=\"{}\" tray_info_idx=\"{}\" type=\"{}\" color=\"{}\" used_m=\"{:.2}\" used_g=\"{:.2}\" group_id=\"{}\" nozzle_diameter=\"{:.2}\" volume_type=\"{}\" used_for_object=\"true\" used_for_support=\"false\" total_load_time=\"{:.2}\" total_unload_time=\"{:.2}\"/>\n",
            f + 1,
            esc(cfg.raw("filament_ids", f)),
            esc(cfg.raw("filament_type", f)),
            esc(cfg.raw("filament_colour", f)),
            inp.stats.filament_mm.get(f).copied().unwrap_or(0.0) / 1000.0,
            inp.stats.filament_g.get(f).copied().unwrap_or(0.0),
            e,
            job.nozzle_diameter(e),
            esc(cfg.raw("nozzle_volume_type", e)),
            job.f("machine_load_filament_time"),
            0.0
        ));
    }
    let mut used_ext: Vec<usize> = inp.stats.used.iter().map(|&f| job.ext_of(f)).collect();
    used_ext.sort();
    used_ext.dedup();
    for e in used_ext {
        si.push_str(&format!("    <nozzle id=\"{e}\" extruder_id=\"{}\" nozzle_diameter=\"{}\" volume_type=\"{}\"/>\n", e + 1, cfg.raw("nozzle_diameter", e), esc(cfg.raw("nozzle_volume_type", e))));
    }
    si.push_str("    <layer_filament_lists>\n");
    // Runs of layers with the same filament set.
    let mut runs: Vec<(Vec<usize>, usize, usize)> = Vec::new();
    for (li, fl) in inp.layer_filaments.iter().enumerate() {
        match runs.last_mut() {
            Some(r) if r.0 == *fl => r.2 = li,
            _ => runs.push((fl.clone(), li, li)),
        }
    }
    for (fl, a, b) in runs {
        si.push_str(&format!("      <layer_filament_list filament_list=\"{}\" layer_ranges=\"{a} {b}\" />\n", fl.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(" ")));
    }
    si.push_str("    </layer_filament_lists>\n  </plate>\n</config>\n");
    // plate_1.json
    let mut ball = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for o in inp.objects {
        ball[0] = ball[0].min(o.bbox[0]);
        ball[1] = ball[1].min(o.bbox[1]);
        ball[2] = ball[2].max(o.bbox[2]);
        ball[3] = ball[3].max(o.bbox[3]);
    }
    let objs: Vec<String> = inp
        .objects
        .iter()
        .map(|o| format!("{{\"area\":{},\"bbox\":[{},{},{},{}],\"id\":{},\"layer_height\":{},\"name\":\"{}\"}}", o.area, o.bbox[0], o.bbox[1], o.bbox[2], o.bbox[3], o.identify_id, job.f("layer_height"), o.name.replace('"', "\\\"")))
        .collect();
    let colours: Vec<String> = inp.stats.used.iter().map(|&f| format!("\"{}\"", cfg.raw("filament_colour", f))).collect();
    let fids: Vec<String> = inp.stats.used.iter().map(|f| f.to_string()).collect();
    let bed_type = match cfg.str("curr_bed_type") {
        "Textured PEI Plate" => "textured_plate",
        "Cool Plate" => "cool_plate",
        "Engineering Plate" => "eng_plate",
        "High Temp Plate" => "hot_plate",
        "Supertack Plate" => "supertack_plate",
        _ => "textured_plate",
    };
    let plate_json = format!(
        "{{\"bbox_all\":[{},{},{},{}],\"bbox_objects\":[{}],\"bed_type\":\"{bed_type}\",\"filament_colors\":[{}],\"filament_ids\":[{}],\"first_extruder\":{},\"first_layer_time\":{},\"is_seq_print\":false,\"nozzle_diameter\":{},\"version\":2}}",
        ball[0],
        ball[1],
        ball[2],
        ball[3],
        objs.join(","),
        colours.join(","),
        fids.join(","),
        inp.stats.used.first().map(|&f| job.ext_of(f)).unwrap_or(0),
        inp.stats.first_layer_time,
        job.nozzle_diameter(0)
    );
    let seq: Vec<String> = inp.layer_filaments.iter().flat_map(|l| l.iter().map(|f| (f + 1).to_string())).collect();
    let nseq: Vec<String> = inp.layer_filaments.iter().flat_map(|l| l.iter().map(|&f| job.ext_of(f).to_string())).collect();
    let filament_sequence = format!("{{\"plate_1\":{{\"nozzle_sequence\":[{}],\"optimal_assignment\":[{}],\"sequence\":[{}]}}}}", nseq.join(","), (0..nf).map(|f| job.ext_of(f).to_string()).collect::<Vec<_>>().join(","), seq.join(","));
    let cut = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<objects>\n</objects>\n";
    // project_settings.config: the job configuration as JSON. The job leaves out filament
    // overrides no filament sets (all `nil`); the project lists them, one `nil` per filament.
    let mut entries: std::collections::BTreeMap<&str, String> = cfg
        .map
        .iter()
        .map(|(k, v)| {
            let val = match v {
                makepad_fab_settings::Value::Scalar(s) => json_str(s),
                makepad_fab_settings::Value::Vector(items) => format!("[{}]", items.iter().map(|s| json_str(s)).collect::<Vec<_>>().join(", ")),
            };
            (k.as_str(), val)
        })
        .collect();
    let unset = format!("[{}]", vec![json_str("nil"); nf.max(1)].join(", "));
    for d in makepad_fab_settings::options::options().defs.values() {
        if d.preset == makepad_fab_settings::options::PresetKind::Filament && d.default == "nil" {
            entries.entry(d.key.as_str()).or_insert_with(|| unset.clone());
        }
    }
    let mut ps = String::from("{\n");
    let n = entries.len();
    for (i, (k, val)) in entries.iter().enumerate() {
        ps.push_str(&format!("    {}: {}{}\n", json_str(k), val, if i + 1 < n { "," } else { "" }));
    }
    ps.push_str("}\n");

    let mut z = ZipWriter::new();
    let add = |z: &mut ZipWriter<Vec<u8>>, name: &str, data: &[u8], m: ZipMethod| z.add(name, data, m).map_err(|e| format!("{name}: {e:?}"));
    add(&mut z, "[Content_Types].xml", content_types.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "Metadata/plate_1.png", &lit, ZipMethod::Store)?;
    add(&mut z, "Metadata/plate_1_small.png", &small, ZipMethod::Store)?;
    add(&mut z, "Metadata/plate_no_light_1.png", &unlit, ZipMethod::Store)?;
    add(&mut z, "Metadata/top_1.png", &top, ZipMethod::Store)?;
    add(&mut z, "Metadata/pick_1.png", &pick, ZipMethod::Store)?;
    add(&mut z, "Metadata/plate_1.json", plate_json.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "3D/3dmodel.model", model.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "Metadata/project_settings.config", ps.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "Metadata/plate_1.gcode.md5", md5.as_bytes(), ZipMethod::Deflate)?;
    z.add_packed("Metadata/plate_1.gcode", gcode, &gcode_packed).map_err(|e| format!("Metadata/plate_1.gcode: {e:?}"))?;
    add(&mut z, "Metadata/_rels/model_settings.config.rels", ms_rels.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "Metadata/model_settings.config", model_settings.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "Metadata/cut_information.xml", cut.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "Metadata/slice_info.config", si.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "Metadata/filament_sequence.json", filament_sequence.as_bytes(), ZipMethod::Deflate)?;
    add(&mut z, "_rels/.rels", rels.as_bytes(), ZipMethod::Deflate)?;
    z.finish().map_err(|e| format!("zip: {e:?}"))
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_thumbnails_preserve_order_dimensions_and_base64_lines() {
        let mut cfg = makepad_fab_settings::config::Config::new();
        cfg.set_str("thumbnails", "48x48/PNG, 20x20/JPG, 80x40/PNG");
        let job = Job::new(cfg);
        let stats = Stats::default();
        let gcode = "; HEADER_BLOCK_START\n; HEADER_BLOCK_END\n\n; EXECUTABLE_BLOCK_START\nG90\n";
        let moves = [MoveRecord { kind: MoveKind::Extrude, filament: 1, layer: 1, p0: [0.0, 0.0, 0.2], p1: [10.0, 10.0, 0.2], width: 0.4, ..Default::default() }];
        let inp = PackageInput { job: &job, gcode, stats: &stats, moves: vec![MovesRef::Records(&moves)], objects: &[], layer_filaments: &[], client_version: "", application: "test", printer_model_id: "", date: "" };
        let text = String::from_utf8(plain_gcode(&inp).unwrap()).unwrap();
        let prefix = "; HEADER_BLOCK_START\n; HEADER_BLOCK_END\n\n; THUMBNAIL_BLOCK_START\n";
        assert!(text.starts_with(prefix));
        let mut lines = text[prefix.len()..].lines();
        for (w, h) in [(48, 48), (80, 40)] {
            let begin = lines.next().unwrap();
            let len: usize = begin.strip_prefix(&format!("; thumbnail begin {w}x{h} ")).unwrap().parse().unwrap();
            let mut encoded = String::new();
            let mut widths = Vec::new();
            loop {
                let line = lines.next().unwrap();
                if line == "; thumbnail end" { break; }
                let chunk = line.strip_prefix("; ").unwrap();
                assert!(!chunk.is_empty() && chunk.len() <= 78);
                widths.push(chunk.len());
                encoded.push_str(chunk);
            }
            assert_eq!(encoded.len(), len);
            assert!(widths[..widths.len() - 1].iter().all(|&n| n == 78));
            let png = makepad_base64::base64_decode(encoded.as_bytes()).unwrap();
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(&png[12..16], b"IHDR");
            assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), w);
            assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), h);
        }
        assert_eq!(lines.next(), Some("; THUMBNAIL_BLOCK_END"));
        assert_eq!(lines.next(), Some(""));
        assert_eq!(lines.collect::<Vec<_>>().join("\n"), "; EXECUTABLE_BLOCK_START\nG90");
    }
}

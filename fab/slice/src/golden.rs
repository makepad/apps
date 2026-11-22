//! Regression comparison against reference slices: per layer and feature
//! (extruded length, filament, move counts, coordinates where the move
//! lists line up), the template-expanded sequences, the config block, the
//! header totals and the print-time estimate.
//!
//! Reference files are read from a local folder at run time; none ship.

use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Clone, Debug, Default)]
pub struct FeatureStats {
    pub moves: usize,
    pub length: f64,
    pub e: f64,
    /// Extrusion end points in order (for move-by-move comparison).
    pub pts: Vec<(f64, f64, f64)>,
}

#[derive(Clone, Debug, Default)]
pub struct LayerStats {
    pub z: f64,
    pub features: BTreeMap<String, FeatureStats>,
    pub travels: usize,
    pub retracts: usize,
}

#[derive(Clone, Debug, Default)]
pub struct GcodeStats {
    pub layers: Vec<LayerStats>,
    pub header: BTreeMap<String, String>,
    pub config: BTreeMap<String, String>,
    pub start: Vec<String>,
    pub end: Vec<String>,
    pub labels: usize,
    pub tool_changes: usize,
}

fn word(rest: &str, c: char) -> Option<f64> {
    rest.split_whitespace().find(|w| w.starts_with(c)).and_then(|w| w[1..].parse().ok())
}

pub fn analyse(text: &str) -> GcodeStats {
    let mut s = GcodeStats::default();
    let mut section = 0; // 0 header, 1 config, 2 exec
    let mut feature = String::from("Custom");
    let (mut x, mut y, mut z) = (0.0f64, 0.0f64, 0.0f64);
    let mut abs = true;
    let mut in_start = false;
    let mut in_end = false;
    for raw in text.lines() {
        let l = raw.trim_end();
        match l {
            "; HEADER_BLOCK_START" => continue,
            "; HEADER_BLOCK_END" => {
                section = 1;
                continue;
            }
            "; CONFIG_BLOCK_START" => {
                section = 1;
                continue;
            }
            "; CONFIG_BLOCK_END" => {
                section = 2;
                continue;
            }
            "; MACHINE_START_GCODE_END" => {
                in_start = false;
                continue;
            }
            "; MACHINE_END_GCODE_START" => {
                in_end = true;
                continue;
            }
            _ => {}
        }
        if section == 0 {
            if let Some(r) = l.strip_prefix("; ") {
                if let Some((k, v)) = r.split_once(':') {
                    s.header.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            continue;
        }
        if section == 1 {
            if let Some(r) = l.strip_prefix("; ") {
                if let Some((k, v)) = r.split_once(" = ") {
                    s.config.insert(k.to_string(), v.to_string());
                } else if let Some(k) = r.strip_suffix(" =") {
                    s.config.insert(k.to_string(), String::new());
                }
            }
            continue;
        }
        if l == "; FEATURE: Custom" && s.layers.is_empty() && !in_end {
            in_start = true;
            continue;
        }
        if in_start {
            if !l.starts_with("M73 P") {
                s.start.push(l.to_string());
            }
            continue;
        }
        if in_end {
            if !l.starts_with("M73 P") && l != "; EXECUTABLE_BLOCK_END" {
                s.end.push(l.to_string());
            }
            continue;
        }
        if l == "; CHANGE_LAYER" {
            s.layers.push(LayerStats::default());
            continue;
        }
        if let Some(r) = l.strip_prefix("; Z_HEIGHT: ") {
            if let Some(ly) = s.layers.last_mut() {
                ly.z = r.trim().parse().unwrap_or(0.0);
            }
            continue;
        }
        if let Some(r) = l.strip_prefix("; FEATURE: ") {
            feature = r.trim().to_string();
            continue;
        }
        if l.starts_with("M624 ") {
            s.labels += 1;
        }
        if l.starts_with('T') && l[1..].chars().next().map_or(false, |c| c.is_ascii_digit()) && !s.layers.is_empty() {
            s.tool_changes += 1;
        }
        let code = l.split(';').next().unwrap_or("").trim();
        if code == "G90" {
            abs = true;
        } else if code == "G91" {
            abs = false;
        }
        let is_arc = code.starts_with("G2 ") || code.starts_with("G3 ");
        if !(code.starts_with("G1 ") || code.starts_with("G0 ") || is_arc) {
            continue;
        }
        let rest = &code[3..];
        let nx = word(rest, 'X').map(|v| if abs { v } else { x + v }).unwrap_or(x);
        let ny = word(rest, 'Y').map(|v| if abs { v } else { y + v }).unwrap_or(y);
        let nz = word(rest, 'Z').map(|v| if abs { v } else { z + v }).unwrap_or(z);
        let e = word(rest, 'E');
        let mut d = ((nx - x).powi(2) + (ny - y).powi(2)).sqrt();
        if is_arc {
            // Arc length from the centre offset.
            let (i, j) = (word(rest, 'I').unwrap_or(0.0), word(rest, 'J').unwrap_or(0.0));
            let (cx, cy) = (x + i, y + j);
            let r = (i * i + j * j).sqrt();
            let a0 = (y - cy).atan2(x - cx);
            let a1 = (ny - cy).atan2(nx - cx);
            let mut sw = a1 - a0;
            let ccw = code.starts_with("G3");
            if ccw {
                while sw <= 0.0 {
                    sw += std::f64::consts::TAU;
                }
            } else {
                while sw >= 0.0 {
                    sw -= std::f64::consts::TAU;
                }
            }
            if word(rest, 'P').map_or(false, |p| p >= 1.0) && d < 1e-6 {
                sw = std::f64::consts::TAU;
            }
            d = sw.abs() * r;
        }
        if let Some(ly) = s.layers.last_mut() {
            match e {
                Some(e) if e > 0.0 && d > 0.0 => {
                    let f = ly.features.entry(feature.clone()).or_default();
                    f.moves += 1;
                    f.length += d;
                    f.e += e;
                    f.pts.push((nx, ny, nz));
                }
                Some(e) if e < 0.0 && d == 0.0 => ly.retracts += 1,
                _ if d > 0.0 && e.is_none() => ly.travels += 1,
                _ => {}
            }
        }
        x = nx;
        y = ny;
        z = nz;
    }
    s
}

fn parse_duration(s: &str) -> f64 {
    let mut t = 0.0;
    for part in s.split_whitespace() {
        let (num, unit) = part.split_at(part.len().saturating_sub(1));
        let v: f64 = num.parse().unwrap_or(0.0);
        t += match unit {
            "d" => v * 86400.0,
            "h" => v * 3600.0,
            "m" => v * 60.0,
            "s" => v,
            _ => 0.0,
        };
    }
    t
}

/// Header times: (model, total) in seconds.
pub fn header_times(h: &BTreeMap<String, String>) -> (f64, f64) {
    let line = h.get("model printing time").cloned().unwrap_or_default();
    // "7m 57s; total estimated time: 13m 24s"
    let (model, total) = match line.split_once(';') {
        Some((a, b)) => (a.trim().to_string(), b.split_once(':').map(|x| x.1.trim().to_string()).unwrap_or_default()),
        None => (line, String::new()),
    };
    (parse_duration(&model), parse_duration(&total))
}

fn pct(a: f64, b: f64) -> f64 {
    if b.abs() < 1e-9 {
        if a.abs() < 1e-9 {
            0.0
        } else {
            100.0
        }
    } else {
        (a - b) / b * 100.0
    }
}

/// Compare ours against the reference; returns the report.
pub fn compare(ours: &GcodeStats, gold: &GcodeStats) -> String {
    let mut r = String::new();
    let (om, ot) = header_times(&ours.header);
    let (gm, gt) = header_times(&gold.header);
    let _ = writeln!(r, "time: total ours {ot:.0} s vs ref {gt:.0} s ({:+.1}%), model ours {om:.0} s vs ref {gm:.0} s ({:+.1}%)", pct(ot, gt), pct(om, gm));
    for k in ["total layer number", "total filament length [mm]", "total filament weight [g]", "max_z_height", "filament"] {
        let _ = writeln!(r, "{k}: ours {} | ref {}", ours.header.get(k).map(|s| s.as_str()).unwrap_or("-"), gold.header.get(k).map(|s| s.as_str()).unwrap_or("-"));
    }
    // Config block.
    let mut cdiff = Vec::new();
    for (k, v) in &gold.config {
        match ours.config.get(k) {
            None => cdiff.push(format!("missing {k}")),
            Some(o) if o != v => cdiff.push(format!("differs {k}")),
            _ => {}
        }
    }
    let extra = ours.config.keys().filter(|k| !gold.config.contains_key(*k)).count();
    let _ = writeln!(r, "config block: {} reference keys, {} differ or missing, {} extra keys", gold.config.len(), cdiff.len(), extra);
    for d in cdiff.iter().take(12) {
        let _ = writeln!(r, "   {d}");
    }
    // Start / end sequences.
    let seq = |name: &str, a: &[String], b: &[String], r: &mut String| {
        let n = a.len().min(b.len());
        let mut diff = 0;
        let mut first = None;
        for i in 0..n {
            if a[i].trim() != b[i].trim() {
                diff += 1;
                if first.is_none() {
                    first = Some(i);
                }
            }
        }
        let _ = writeln!(r, "{name}: ours {} lines, ref {} lines, {} of the first {} differ", a.len(), b.len(), diff, n);
        if let Some(i) = first {
            let _ = writeln!(r, "   first diff at {i}: ours `{}` | ref `{}`", a[i], b[i]);
        }
    };
    seq("start sequence", &ours.start, &gold.start, &mut r);
    seq("end sequence", &ours.end, &gold.end, &mut r);
    let _ = writeln!(r, "object label blocks: ours {} ref {}; tool changes in layers: ours {} ref {}", ours.labels, gold.labels, ours.tool_changes, gold.tool_changes);
    // Layers.
    let _ = writeln!(r, "layers: ours {} ref {}", ours.layers.len(), gold.layers.len());
    let mut feat_tot: BTreeMap<String, (f64, f64, f64, f64, usize, usize)> = BTreeMap::new();
    // Support can have its own Z schedule. Totals must include every layer
    // on both sides, and coordinate comparisons only pair equal heights.
    for l in &ours.layers {
        for (name, f) in &l.features {
            let t = feat_tot.entry(name.clone()).or_default();
            t.0 += f.length;
            t.2 += f.e;
            t.4 += f.moves;
        }
    }
    for l in &gold.layers {
        for (name, f) in &l.features {
            let t = feat_tot.entry(name.clone()).or_default();
            t.1 += f.length;
            t.3 += f.e;
            t.5 += f.moves;
        }
    }
    let mut exact_layers = 0;
    let mut max_dev: f64 = 0.0;
    let mut compared_moves = 0;
    let mut wall_dev: f64 = 0.0;
    let mut oi = 0;
    for g in &gold.layers {
        while oi < ours.layers.len() && ours.layers[oi].z < g.z - 1e-6 {
            oi += 1;
        }
        let Some(o) = ours.layers.get(oi).filter(|o| (o.z - g.z).abs() < 1e-6) else { continue };
        let mut same = o.features.len() == g.features.len();
        for (name, gf) in &g.features {
            let Some(of) = o.features.get(name) else {
                same = false;
                continue;
            };
            if of.moves == gf.moves {
                for (a, b) in of.pts.iter().zip(gf.pts.iter()) {
                    let d = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
                    max_dev = max_dev.max(d);
                    if name.contains("wall") {
                        wall_dev = wall_dev.max(d);
                    }
                    compared_moves += 1;
                }
            } else {
                same = false;
            }
        }
        if same {
            exact_layers += 1;
        }
    }
    let _ = writeln!(r, "layers whose features have identical move counts: {exact_layers}/{}", gold.layers.len());
    let _ = writeln!(r, "moves compared point by point: {compared_moves}, max deviation {max_dev:.3} mm (walls {wall_dev:.3} mm)");
    let _ = writeln!(r, "per feature (ours vs ref): length mm | filament mm | moves");
    for (name, t) in &feat_tot {
        let _ = writeln!(r, "   {name:<24} {:>9.1} vs {:>9.1} ({:+6.1}%) | {:>8.2} vs {:>8.2} ({:+6.1}%) | {} vs {}", t.0, t.1, pct(t.0, t.1), t.2, t.3, pct(t.2, t.3), t.4, t.5);
    }
    r
}

/// Per-layer detail for one feature (debugging aid for the report).
pub fn layer_detail(ours: &GcodeStats, gold: &GcodeStats, layer: usize) -> String {
    let mut r = String::new();
    if let (Some(o), Some(g)) = (ours.layers.get(layer), gold.layers.get(layer)) {
        let _ = writeln!(r, "layer {} z ours {} ref {}", layer + 1, o.z, g.z);
        let names: std::collections::BTreeSet<&String> = o.features.keys().chain(g.features.keys()).collect();
        for n in names {
            let of = o.features.get(n).cloned().unwrap_or_default();
            let gf = g.features.get(n).cloned().unwrap_or_default();
            let _ = writeln!(r, "   {n:<24} moves {} vs {}  len {:.2} vs {:.2}  e {:.4} vs {:.4}", of.moves, gf.moves, of.length, gf.length, of.e, gf.e);
        }
        let _ = writeln!(r, "   travels {} vs {}  retracts {} vs {}", o.travels, g.travels, o.retracts, g.retracts);
    }
    r
}

//! Slice parity: what two G-code files of the same job do differently, by
//! print risk. Each file is read into the facts a print depends on (per
//! feature: plastic per millimetre, line width, speed, acceleration; per
//! layer: Z, temperatures, fan; retraction, wipe, travel; tool changes,
//! prime tower and flush; time and grams), and the differences are listed
//! worst first: flow and temperature, then dimensions, then speed, then
//! time and material.
//!
//! The reference files are read at run time; none ship.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One feature's totals over the print.
#[derive(Clone, Debug, Default)]
pub struct Feature {
    /// Extruding moves.
    pub moves: usize,
    /// Extruded path length (mm).
    pub length: f64,
    /// Filament pushed (mm).
    pub e: f64,
    /// Σ line width × length (from the width comments).
    pub width_len: f64,
    /// Σ layer height × length.
    pub height_len: f64,
    /// Σ speed (mm/s) × length.
    pub speed_len: f64,
    /// Σ acceleration × length.
    pub accel_len: f64,
    /// Fastest speed (mm/s).
    pub speed_max: f64,
    /// Layers it appears on.
    pub layers: usize,
    /// Separate runs (travel-separated): loops for walls.
    pub runs: usize,
}

impl Feature {
    pub fn e_per_mm(&self) -> f64 {
        if self.length > 0.0 { self.e / self.length } else { 0.0 }
    }
    pub fn width(&self) -> f64 {
        if self.length > 0.0 { self.width_len / self.length } else { 0.0 }
    }
    pub fn speed(&self) -> f64 {
        if self.length > 0.0 { self.speed_len / self.length } else { 0.0 }
    }
    pub fn accel(&self) -> f64 {
        if self.length > 0.0 { self.accel_len / self.length } else { 0.0 }
    }
    /// Plastic per mm divided by the line's nominal cross-section (width ×
    /// height): the flow the file asks for, independent of the width chosen.
    pub fn flow(&self, filament_area: f64) -> f64 {
        let wh = if self.length > 0.0 { (self.width_len / self.length) * (self.height_len / self.length) } else { 0.0 };
        if wh > 0.0 { self.e_per_mm() * filament_area / wh } else { 0.0 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Print Z per layer.
    pub z: Vec<f64>,
    pub features: BTreeMap<String, Feature>,
    /// (tool, temperature) set for printing, in order of first use, with the
    /// layer it was first set on (pre-heats while another tool prints excluded).
    pub nozzle_temps: Vec<(i64, i64, usize)>,
    /// Bed temperatures in order (first layer first).
    pub bed_temps: Vec<i64>,
    /// Per layer: the part-cooling fan (0..255) averaged over the layer's
    /// extrusion length.
    pub fan: Vec<i64>,
    /// Auxiliary fan settings seen (`M106 P2`), and chamber fan (`P3`).
    pub aux_fan: Vec<i64>,
    pub retracts: usize,
    pub retract_len: f64,
    /// Typical (most common) retraction length and speed (mm, mm/s).
    pub retract_typical: (f64, f64),
    pub wipes: usize,
    pub travels: usize,
    pub travel_len: f64,
    /// Travels that lift Z.
    pub z_hops: usize,
    /// Tool selections after the start sequence, in order.
    pub tool_changes: Vec<String>,
    /// Layers with top / bottom surface.
    pub top_layers: usize,
    pub bottom_layers: usize,
    /// Header: estimated total time (s) and filament (g, mm).
    pub time: f64,
    pub grams: f64,
    pub filament_mm: f64,
    pub filament_diameter: f64,
    /// Per filament (0-based): Σ nozzle target temperature × length over the
    /// part's extrusions (prime tower excluded), the length, and the lowest
    /// target seen while extruding the part.
    pub print_temp: BTreeMap<usize, (f64, f64, f64)>,
    /// Idle-nozzle temperature commands (pre-cool, pre-heat, tower reheat).
    pub idle_temp_cmds: usize,
}

impl Facts {
    /// The length-weighted nozzle target while filament `f` printed the part.
    pub fn mean_print_temp(&self, f: usize) -> Option<f64> {
        self.print_temp.get(&f).filter(|t| t.1 > 0.0).map(|t| t.0 / t.1)
    }
}

fn word(code: &str, c: char) -> Option<f64> {
    code.split_whitespace().skip(1).find(|w| w.starts_with(c)).and_then(|w| w[1..].parse().ok())
}

fn duration(s: &str) -> f64 {
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

/// Read the facts of a G-code file.
pub fn read(text: &str) -> Facts {
    let mut f = Facts { filament_diameter: 1.75, ..Default::default() };
    let mut feature = String::from("Custom");
    let (mut x, mut y, mut z) = (0.0f64, 0.0f64, 0.0f64);
    let mut feed = 0.0f64; // mm/min
    let mut accel = 0.0f64;
    let mut width = 0.0f64;
    let mut height = 0.0f64;
    let mut abs = true;
    let mut in_wipe = false;
    let mut started = false;
    let mut tool: i64 = -1;
    let mut last_was_travel = true;
    let mut seen_layer: BTreeMap<String, usize> = BTreeMap::new();
    let mut retract_hist: BTreeMap<(i64, i64), usize> = BTreeMap::new();
    let mut layer_fan = (0.0f64, 0.0f64);
    let mut current_fan = 0i64;
    // Nozzle targets by firmware index, the filament → nozzle maps from the
    // configuration block, and a tower temperature waiting for its change.
    let mut target: BTreeMap<i64, f64> = BTreeMap::new();
    let mut fil_map: Vec<i64> = Vec::new();
    let mut phys_map: Vec<i64> = Vec::new();
    let mut pending_tower: Option<f64> = None;
    let nums = |r: &str| -> Vec<i64> { r.split(',').filter_map(|v| v.trim().parse::<f64>().ok()).map(|v| v as i64).collect() };
    for raw in text.lines() {
        let l = raw.trim();
        if let Some(c) = l.strip_prefix(';') {
            let c = c.trim();
            if let Some(r) = c.strip_prefix("model printing time:") {
                if let Some((_, tot)) = r.split_once("total estimated time:") {
                    f.time = duration(tot.trim());
                }
            } else if let Some(r) = c.strip_prefix("total filament weight [g] :") {
                f.grams = r.split(',').filter_map(|v| v.trim().parse::<f64>().ok()).sum();
            } else if let Some(r) = c.strip_prefix("total filament length [mm] :") {
                f.filament_mm = r.split(',').filter_map(|v| v.trim().parse::<f64>().ok()).sum();
            } else if let Some(r) = c.strip_prefix("filament_diameter:").or_else(|| c.strip_prefix("filament_diameter =")) {
                if let Some(d) = r.split(',').next().and_then(|v| v.trim().parse::<f64>().ok()) {
                    f.filament_diameter = d;
                }
            } else if let Some(r) = c.strip_prefix("filament_map = ") {
                fil_map = nums(r);
            } else if let Some(r) = c.strip_prefix("physical_extruder_map = ") {
                phys_map = nums(r);
            } else if c == "CHANGE_LAYER" {
                if !f.z.is_empty() {
                    f.fan.push(if layer_fan.1 > 0.0 { (layer_fan.0 / layer_fan.1).round() as i64 } else { current_fan });
                }
                layer_fan = (0.0, 0.0);
                started = true;
                f.z.push(z);
                seen_layer.clear();
            } else if let Some(r) = c.strip_prefix("Z_HEIGHT:") {
                if let (Some(last), Ok(v)) = (f.z.last_mut(), r.trim().parse::<f64>()) {
                    *last = v;
                }
            } else if let Some(r) = c.strip_prefix("LAYER_HEIGHT:") {
                height = r.trim().parse().unwrap_or(height);
            } else if let Some(r) = c.strip_prefix("LINE_WIDTH:") {
                width = r.trim().parse().unwrap_or(width);
            } else if let Some(r) = c.strip_prefix("FEATURE:") {
                feature = r.trim().to_string();
                last_was_travel = true;
            } else if c == "WIPE_START" {
                in_wipe = true;
                f.wipes += 1;
            } else if c == "WIPE_END" {
                in_wipe = false;
            }
            continue;
        }
        let (code, comment) = match l.split_once(';') {
            Some((a, b)) => (a.trim(), b),
            None => (l, ""),
        };
        if code.is_empty() {
            continue;
        }
        let cmd = code.split_whitespace().next().unwrap_or("");
        match cmd {
            "G90" => abs = true,
            "G91" => abs = false,
            "M204" => {
                if let Some(s) = word(code, 'S') {
                    accel = s;
                }
            }
            "M104" | "M109" => {
                let phys_of = |f: i64| -> i64 {
                    let e = fil_map.get(f.max(0) as usize).copied().unwrap_or(1).max(1) - 1;
                    phys_map.get(e as usize).copied().unwrap_or(e)
                };
                let nozzle = word(code, 'T').map(|v| v as i64).unwrap_or_else(|| phys_of(tool.max(0)));
                if let Some(s) = word(code, 'S') {
                    target.insert(nozzle, s);
                }
                if code.contains(" N0") {
                    f.idle_temp_cmds += 1;
                }
                let t = word(code, 'T').map(|v| v as i64).unwrap_or(tool.max(0));
                let s = word(code, 'S').unwrap_or(0.0).round() as i64;
                let preheat = comment.to_ascii_lowercase().contains("pre heat") || code.contains(" N0");
                if started && s > 0 && !preheat && !f.nozzle_temps.iter().any(|&(tt, ss, _)| tt == t && ss == s) {
                    f.nozzle_temps.push((t, s, f.z.len()));
                }
            }
            "M140" | "M190" => {
                let s = word(code, 'S').unwrap_or(0.0).round() as i64;
                if s > 0 && f.bed_temps.last() != Some(&s) {
                    f.bed_temps.push(s);
                }
            }
            "M106" => {
                let p = word(code, 'P').map(|v| v as i64).unwrap_or(0);
                let s = word(code, 'S').unwrap_or(0.0).round() as i64;
                if p <= 1 {
                    current_fan = s;
                } else if !f.aux_fan.contains(&s) {
                    f.aux_fan.push(s);
                }
            }
            "M107" => current_fan = 0,
            "M620.15" => pending_tower = word(code, 'C'),
            _ if cmd.starts_with('T') && cmd[1..].chars().all(|c| c.is_ascii_digit()) && cmd.len() > 1 && cmd[1..].parse::<i64>().map_or(false, |t| t < 1000) => {
                let t: i64 = cmd[1..].parse().unwrap_or(0);
                if started && t != tool {
                    f.tool_changes.push(cmd.to_string());
                }
                tool = t;
                if let Some(c) = pending_tower.take() {
                    let e = fil_map.get(t.max(0) as usize).copied().unwrap_or(1).max(1) - 1;
                    target.insert(phys_map.get(e as usize).copied().unwrap_or(e), c);
                }
            }
            _ => {}
        }
        if !matches!(cmd, "G0" | "G1" | "G2" | "G3") {
            continue;
        }
        if let Some(fv) = word(code, 'F') {
            feed = fv;
        }
        let nx = word(code, 'X').map(|v| if abs { v } else { x + v }).unwrap_or(x);
        let ny = word(code, 'Y').map(|v| if abs { v } else { y + v }).unwrap_or(y);
        let nz = word(code, 'Z').map(|v| if abs { v } else { z + v }).unwrap_or(z);
        let e = word(code, 'E');
        let mut d = ((nx - x).powi(2) + (ny - y).powi(2)).sqrt();
        if cmd == "G2" || cmd == "G3" {
            let (i, j) = (word(code, 'I').unwrap_or(0.0), word(code, 'J').unwrap_or(0.0));
            let (cx, cy) = (x + i, y + j);
            let r = (i * i + j * j).sqrt();
            let mut sw = (ny - cy).atan2(nx - cx) - (y - cy).atan2(x - cx);
            if cmd == "G3" {
                while sw <= 0.0 {
                    sw += std::f64::consts::TAU;
                }
            } else {
                while sw >= 0.0 {
                    sw -= std::f64::consts::TAU;
                }
            }
            d = sw.abs() * r;
        }
        if started {
            match e {
                Some(e) if e > 0.0 && d > 1e-9 && !in_wipe => {
                    let ft = f.features.entry(feature.clone()).or_default();
                    ft.moves += 1;
                    ft.length += d;
                    ft.e += e;
                    ft.width_len += width * d;
                    ft.height_len += height * d;
                    ft.speed_len += feed / 60.0 * d;
                    ft.accel_len += accel * d;
                    ft.speed_max = ft.speed_max.max(feed / 60.0);
                    layer_fan.0 += current_fan as f64 * d;
                    layer_fan.1 += d;
                    if last_was_travel {
                        ft.runs += 1;
                    }
                    if feature != "Prime tower" && tool >= 0 {
                        let e = fil_map.get(tool as usize).copied().unwrap_or(1).max(1) - 1;
                        let nozzle = phys_map.get(e as usize).copied().unwrap_or(e);
                        let t = target.get(&nozzle).copied().unwrap_or(0.0);
                        let pt = f.print_temp.entry(tool as usize).or_insert((0.0, 0.0, f64::MAX));
                        pt.0 += t * d;
                        pt.1 += d;
                        pt.2 = pt.2.min(t);
                    }
                    if !seen_layer.contains_key(&feature) {
                        seen_layer.insert(feature.clone(), 1);
                        ft.layers += 1;
                        if feature == "Top surface" {
                            f.top_layers += 1;
                        } else if feature == "Bottom surface" {
                            f.bottom_layers += 1;
                        }
                    }
                    last_was_travel = false;
                }
                Some(e) if e < 0.0 && d < 1e-9 => {
                    f.retracts += 1;
                    f.retract_len += -e;
                    *retract_hist.entry(((-e * 1000.0).round() as i64, (feed / 60.0).round() as i64)).or_default() += 1;
                }
                None if d > 1e-9 || nz != z => {
                    if d > 1e-9 {
                        f.travels += 1;
                        f.travel_len += d;
                        last_was_travel = true;
                    }
                    if nz > z + 1e-6 && nz - z < 5.0 && !f.z.is_empty() && nz > *f.z.last().unwrap() + 1e-6 {
                        f.z_hops += 1;
                    }
                }
                _ => {}
            }
        }
        x = nx;
        y = ny;
        z = nz;
    }
    if !f.z.is_empty() {
        f.fan.push(if layer_fan.1 > 0.0 { (layer_fan.0 / layer_fan.1).round() as i64 } else { current_fan });
    }
    if let Some(((len, speed), _)) = retract_hist.iter().max_by_key(|(_, n)| **n) {
        f.retract_typical = (*len as f64 / 1000.0, *speed as f64);
    }
    f
}

/// How bad a difference is for the print.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    /// Too much or too little plastic, wrong temperature or cooling.
    Flow = 0,
    /// Retraction and travel (stringing, blobs, collisions).
    Retraction = 1,
    /// Layer heights, widths, wall and shell counts: the part's shape.
    Dimensions = 2,
    /// Speeds and accelerations.
    Speed = 3,
    /// Material use, tower and flush, time.
    Time = 4,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Flow => "flow/temp",
            Risk::Retraction => "retraction",
            Risk::Dimensions => "dimensions",
            Risk::Speed => "speed",
            Risk::Time => "time/material",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Delta {
    pub risk: Risk,
    pub what: String,
    pub ours: String,
    pub reference: String,
    /// Relative size of the difference (0.05 = 5 %), for ordering within a risk.
    pub size: f64,
}

fn rel(a: f64, b: f64) -> f64 {
    if b.abs() < 1e-9 {
        if a.abs() < 1e-9 { 0.0 } else { 1.0 }
    } else {
        (a - b) / b
    }
}

/// Features too small to judge (a few mm of extrusion).
const MIN_LEN: f64 = 30.0;

/// Every difference past its tolerance, worst first.
pub fn compare(ours: &Facts, r: &Facts) -> Vec<Delta> {
    let mut d = Vec::new();
    let mut add = |risk: Risk, what: String, ours: String, reference: String, size: f64| d.push(Delta { risk, what, ours, reference, size });
    let area = std::f64::consts::PI * r.filament_diameter * r.filament_diameter / 4.0;
    let names: std::collections::BTreeSet<&String> = ours.features.keys().chain(r.features.keys()).collect();
    for n in names {
        let (o, g) = (ours.features.get(n).cloned().unwrap_or_default(), r.features.get(n).cloned().unwrap_or_default());
        if o.length < MIN_LEN && g.length < MIN_LEN {
            continue;
        }
        if o.length < MIN_LEN || g.length < MIN_LEN {
            add(Risk::Dimensions, format!("{n}: printed by one side only"), format!("{:.0} mm", o.length), format!("{:.0} mm", g.length), 1.0);
            continue;
        }
        let ef = rel(o.flow(area), g.flow(area));
        if ef.abs() > 0.02 {
            add(Risk::Flow, format!("{n}: flow (plastic per line cross-section)"), format!("{:.3}", o.flow(area)), format!("{:.3}", g.flow(area)), ef.abs());
        }
        // Filament per mm follows the line's width and height (reported there); the flow
        // per cross-section above is the over/under-extrusion check.
        let ee = rel(o.e_per_mm(), g.e_per_mm());
        if ee.abs() > 0.03 && ef.abs() <= 0.02 {
            add(Risk::Dimensions, format!("{n}: filament per mm (from its width and height)"), format!("{:.4}", o.e_per_mm()), format!("{:.4}", g.e_per_mm()), ee.abs());
        } else if ee.abs() > 0.03 {
            add(Risk::Flow, format!("{n}: filament per mm"), format!("{:.4}", o.e_per_mm()), format!("{:.4}", g.e_per_mm()), ee.abs());
        }
        let ew = rel(o.width(), g.width());
        if ew.abs() > 0.02 {
            add(Risk::Dimensions, format!("{n}: line width"), format!("{:.3} mm", o.width()), format!("{:.3} mm", g.width()), ew.abs());
        }
        let el = rel(o.length, g.length);
        if el.abs() > 0.05 {
            add(Risk::Dimensions, format!("{n}: path length"), format!("{:.0} mm", o.length), format!("{:.0} mm", g.length), el.abs());
        }
        if o.layers != g.layers {
            add(Risk::Dimensions, format!("{n}: layers"), o.layers.to_string(), g.layers.to_string(), rel(o.layers as f64, g.layers as f64).abs());
        }
        if n.contains("wall") && g.layers > 0 && o.layers > 0 {
            let (lo, lg) = (o.runs as f64 / o.layers as f64, g.runs as f64 / g.layers as f64);
            if rel(lo, lg).abs() > 0.1 {
                add(Risk::Dimensions, format!("{n}: loops per layer"), format!("{lo:.2}"), format!("{lg:.2}"), rel(lo, lg).abs());
            }
        }
        let es = rel(o.speed(), g.speed());
        if es.abs() > 0.05 {
            add(Risk::Speed, format!("{n}: speed"), format!("{:.0} mm/s (max {:.0})", o.speed(), o.speed_max), format!("{:.0} mm/s (max {:.0})", g.speed(), g.speed_max), es.abs());
        }
        let ea = rel(o.accel(), g.accel());
        if ea.abs() > 0.05 {
            add(Risk::Speed, format!("{n}: acceleration"), format!("{:.0}", o.accel()), format!("{:.0}", g.accel()), ea.abs());
        }
    }
    let temps = |t: &[(i64, i64, usize)]| t.iter().map(|(tt, s, l)| format!("T{tt} {s}°C@L{}", l)).collect::<Vec<_>>().join(", ");
    let set = |t: &[(i64, i64, usize)]| t.iter().map(|(tt, s, _)| (*tt, *s)).collect::<std::collections::BTreeSet<_>>();
    if set(&ours.nozzle_temps) != set(&r.nozzle_temps) {
        add(Risk::Flow, "nozzle temperatures".into(), temps(&ours.nozzle_temps), temps(&r.nozzle_temps), 1.0);
    }
    for (&fil, _) in r.print_temp.iter().chain(ours.print_temp.iter()) {
        let (a, b) = (ours.mean_print_temp(fil).unwrap_or(0.0), r.mean_print_temp(fil).unwrap_or(0.0));
        let (amin, bmin) = (ours.print_temp.get(&fil).map_or(0.0, |t| t.2), r.print_temp.get(&fil).map_or(0.0, |t| t.2));
        if (a - b).abs() > 0.5 || (amin - bmin).abs() > 0.5 {
            add(Risk::Flow, format!("filament {} print temperature on the part (mean, lowest)", fil + 1), format!("{a:.1} °C, {amin:.0}"), format!("{b:.1} °C, {bmin:.0}"), (a - b).abs().max((amin - bmin).abs()) / 10.0);
        }
    }
    if (ours.idle_temp_cmds as f64 - r.idle_temp_cmds as f64).abs() > (r.idle_temp_cmds as f64 * 0.1).max(1.0) {
        add(Risk::Flow, "idle-nozzle temperature commands (pre-cool, pre-heat, tower reheat)".into(), ours.idle_temp_cmds.to_string(), r.idle_temp_cmds.to_string(), 0.8);
    }
    if ours.bed_temps != r.bed_temps {
        add(Risk::Flow, "bed temperatures".into(), format!("{:?}", ours.bed_temps), format!("{:?}", r.bed_temps), 1.0);
    }
    let n = ours.fan.len().min(r.fan.len());
    let fan_diff: Vec<usize> = (0..n).filter(|&i| (ours.fan[i] - r.fan[i]).abs() > 13).collect();
    if !fan_diff.is_empty() {
        let i = fan_diff[0];
        add(Risk::Flow, format!("part fan (mean over the layer's lines) differs by more than 5% on {} of {n} layers", fan_diff.len()), format!("first at layer {}: {}", i + 1, ours.fan[i]), format!("{}", r.fan[i]), fan_diff.len() as f64 / n.max(1) as f64);
    }
    if ours.aux_fan != r.aux_fan {
        add(Risk::Flow, "auxiliary / chamber fan settings".into(), format!("{:?}", ours.aux_fan), format!("{:?}", r.aux_fan), 0.5);
    }
    let er = rel(ours.retract_typical.0, r.retract_typical.0);
    if er.abs() > 0.02 || (ours.retract_typical.1 - r.retract_typical.1).abs() > 1.0 {
        add(Risk::Retraction, "typical retraction".into(), format!("{:.2} mm at {:.0} mm/s", ours.retract_typical.0, ours.retract_typical.1), format!("{:.2} mm at {:.0} mm/s", r.retract_typical.0, r.retract_typical.1), er.abs().max(0.05));
    }
    let ec = rel(ours.retracts as f64, r.retracts as f64);
    if ec.abs() > 0.15 {
        add(Risk::Retraction, "retractions".into(), ours.retracts.to_string(), r.retracts.to_string(), ec.abs());
    }
    let ew = rel(ours.wipes as f64, r.wipes as f64);
    if ew.abs() > 0.15 {
        add(Risk::Retraction, "wipes".into(), ours.wipes.to_string(), r.wipes.to_string(), ew.abs());
    }
    let eh = rel(ours.z_hops as f64, r.z_hops as f64);
    if eh.abs() > 0.15 {
        add(Risk::Retraction, "travel lifts".into(), ours.z_hops.to_string(), r.z_hops.to_string(), eh.abs());
    }
    let et = rel(ours.travel_len, r.travel_len);
    if et.abs() > 0.15 {
        add(Risk::Retraction, "travel distance".into(), format!("{:.0} mm ({} moves)", ours.travel_len, ours.travels), format!("{:.0} mm ({} moves)", r.travel_len, r.travels), et.abs());
    }
    if ours.z.len() != r.z.len() {
        add(Risk::Dimensions, "layer count".into(), ours.z.len().to_string(), r.z.len().to_string(), 1.0);
    }
    let zd = ours.z.iter().zip(&r.z).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    if zd > 0.005 {
        let i = ours.z.iter().zip(&r.z).position(|(a, b)| (a - b).abs() > 0.005).unwrap_or(0);
        add(Risk::Dimensions, "layer Z".into(), format!("layer {}: {:.3}", i + 1, ours.z.get(i).copied().unwrap_or(0.0)), format!("{:.3} (max diff {zd:.3})", r.z.get(i).copied().unwrap_or(0.0)), zd);
    }
    if ours.top_layers != r.top_layers {
        add(Risk::Dimensions, "layers with top surface".into(), ours.top_layers.to_string(), r.top_layers.to_string(), 0.5);
    }
    if ours.bottom_layers != r.bottom_layers {
        add(Risk::Dimensions, "layers with bottom surface".into(), ours.bottom_layers.to_string(), r.bottom_layers.to_string(), 0.5);
    }
    if ours.tool_changes != r.tool_changes {
        let s = |v: &[String]| if v.len() > 8 { format!("{} changes: {} …", v.len(), v[..8].join(" ")) } else { format!("{} changes: {}", v.len(), v.join(" ")) };
        add(Risk::Flow, "tool change sequence".into(), s(&ours.tool_changes), s(&r.tool_changes), 1.0);
    }
    let eg = rel(ours.grams, r.grams);
    if eg.abs() > 0.03 {
        add(Risk::Time, "filament weight".into(), format!("{:.2} g", ours.grams), format!("{:.2} g", r.grams), eg.abs());
    }
    let etime = rel(ours.time, r.time);
    if etime.abs() > 0.05 {
        add(Risk::Time, "estimated time".into(), format!("{:.0} s", ours.time), format!("{:.0} s", r.time), etime.abs());
    }
    d.sort_by(|a, b| a.risk.cmp(&b.risk).then(b.size.partial_cmp(&a.size).unwrap_or(std::cmp::Ordering::Equal)));
    d
}

/// The per-feature table and the ranked differences of one case.
pub fn report(case: &str, ours: &Facts, r: &Facts) -> String {
    let mut s = String::new();
    let area = std::f64::consts::PI * r.filament_diameter * r.filament_diameter / 4.0;
    let _ = writeln!(s, "== {case}");
    let _ = writeln!(s, "layers {} vs {} | time {:.0} vs {:.0} s | {:.2} vs {:.2} g | tool changes {} vs {} | retracts {} vs {}", ours.z.len(), r.z.len(), ours.time, r.time, ours.grams, r.grams, ours.tool_changes.len(), r.tool_changes.len(), ours.retracts, r.retracts);
    let _ = writeln!(s, "{:<26} {:>15} {:>13} {:>13} {:>15} {:>13}", "feature (ours / ref)", "length mm", "flow", "width mm", "speed mm/s", "accel");
    let names: std::collections::BTreeSet<&String> = ours.features.keys().chain(r.features.keys()).collect();
    for n in names {
        let (o, g) = (ours.features.get(n).cloned().unwrap_or_default(), r.features.get(n).cloned().unwrap_or_default());
        let _ = writeln!(s, "{:<26} {:>7.0}/{:<7.0} {:>6.3}/{:<6.3} {:>6.3}/{:<6.3} {:>7.0}/{:<7.0} {:>6.0}/{:<6.0}", n, o.length, g.length, o.flow(area), g.flow(area), o.width(), g.width(), o.speed(), g.speed(), o.accel(), g.accel());
    }
    let deltas = compare(ours, r);
    if deltas.is_empty() {
        let _ = writeln!(s, "no differences past tolerance");
    }
    for d in &deltas {
        let _ = writeln!(s, "[{}] {}: ours {} | ref {}", d.risk.label(), d.what, d.ours, d.reference);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "; total estimated time: x\n; model printing time: 1m; total estimated time: 2m 0s\n; total filament weight [g] : 1.5\nM140 S60\n; CHANGE_LAYER\n; Z_HEIGHT: 0.2\n; LAYER_HEIGHT: 0.2\nM104 S220\nM106 S255\n; FEATURE: Outer wall\n; LINE_WIDTH: 0.42\nG1 F6000\nM204 S5000\nG1 X40 Y0 E0.5\nG1 X40 Y40 E0.5\nG1 E-0.8 F1800\nG1 X20 Y20 F30000\n";

    #[test]
    fn reads_features_and_flags_flow() {
        let a = read(A);
        assert_eq!(a.z, vec![0.2]);
        let w = &a.features["Outer wall"];
        assert!((w.length - 80.0).abs() < 1e-9 && (w.e - 1.0).abs() < 1e-9);
        assert!((w.speed() - 100.0).abs() < 1e-9 && (w.accel() - 5000.0).abs() < 1e-9);
        assert_eq!(a.nozzle_temps, vec![(0, 220, 1)]);
        assert_eq!(a.retracts, 1);
        assert_eq!(a.time, 120.0);
        let b = read(&A.replace("E0.5", "E0.55").replace("S220", "S230"));
        let d = compare(&b, &a);
        assert!(d.iter().any(|x| x.risk == Risk::Flow && x.what.contains("flow")), "{d:?}");
        assert!(d.iter().any(|x| x.what == "nozzle temperatures"));
        assert_eq!(d[0].risk, Risk::Flow);
    }
}

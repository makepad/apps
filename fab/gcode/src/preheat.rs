//! The idle nozzle on a two-nozzle printer (R1 §9.5, R5.3 §1.6): while the
//! other nozzle prints, the idle one cools so it does not ooze, and is
//! heated again early enough to be at temperature when it is next needed.
//!
//! A pass over the written lines with their estimated end times: for every
//! idle stretch of a nozzle, a cooling command when it goes idle and a
//! pre-heat command (in a bracket the firmware may skip, [`Firmware::idle_heating`]) at the
//! moment heating must start. Cooling and heating follow the printer's
//! `hotend_cooling_rate` / `hotend_heating_rate`: the nozzle cools for as
//! long as it can and still reach the target in time.

use crate::firmware::Firmware;
use makepad_fab_settings::machine::Job;

/// What to insert after which line (index into the written lines).
pub type Inserts = Vec<(usize, Vec<String>)>;

/// The lowest temperature an idle nozzle is cooled to.
const ROOM: f64 = 25.0;

/// One idle stretch of a nozzle.
#[derive(Clone, Debug, PartialEq)]
pub struct Idle {
    pub nozzle: usize,
    /// The filament the nozzle printed before going idle.
    pub last_filament: usize,
    /// The line it goes idle after, and when.
    pub from_line: usize,
    pub from_t: f64,
    /// The temperature it was at.
    pub from_temp: f64,
    /// When it is needed again, for which filament, at which temperature.
    pub until_t: f64,
    pub next_filament: usize,
    pub target: f64,
}

/// The temperature to cool to, and how long before `until` heating must start.
pub fn plan(from_temp: f64, target: f64, dt: f64, cool: f64, heat: f64) -> (f64, f64) {
    let (cool, heat) = (cool.max(0.1), heat.max(0.1));
    if target > from_temp {
        // Only heating: start as late as possible.
        return (from_temp, ((target - from_temp) / heat).min(dt));
    }
    // Cool for t1, heat for dt - t1, ending at the target.
    let t1 = ((from_temp - target + heat * dt) / (cool + heat)).clamp(0.0, dt);
    let mid = (from_temp - cool * t1).max(ROOM);
    if mid >= target {
        // Not enough time to cool below the target: cool to it.
        return (target, 0.0);
    }
    (mid, ((target - mid) / heat).min(dt))
}

fn tool_of(l: &str) -> Option<usize> {
    let w = l.split_whitespace().next()?;
    let n = w.strip_prefix('T')?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    n.parse::<usize>().ok().filter(|&t| t < 64)
}

/// The idle stretches of every nozzle, from the lines (up to the end
/// sequence) and their end times.
pub fn idle_stretches(job: &Job, lines: &[&str], line_end: &[f64], end_line: usize, first_filament: usize, tower: bool) -> Vec<Idle> {
    let n = job.extruders();
    if n < 2 {
        return Vec::new();
    }
    let start = lines.iter().position(|l| *l == "; MACHINE_START_GCODE_END");
    let Some(start) = start else { return Vec::new() };
    let t_at = |i: usize| line_end.get(i).copied().unwrap_or(0.0);
    let temp_of = |f: usize, layer: usize| if layer == 0 { job.fil("nozzle_temperature_initial_layer", f) } else { job.fil("nozzle_temperature", f) };
    // Cooler only where the first thing it prints is the tower.
    let target_of = |f: usize, layer: usize| if layer == 0 || !tower { temp_of(f, layer) } else { temp_of(f, layer) - job.fil("filament_preheat_temperature_delta", f).max(0.0) };
    // Per nozzle: idle since (line, time, temperature, filament), when idle.
    let mut idle: Vec<Option<(usize, f64, f64, usize)>> = vec![None; n];
    let cur_noz = job.ext_of(first_filament);
    // Every other nozzle is idle from the end of the start sequence, as hot as
    // the start sequence left it for its first filament.
    let first_use: Vec<Option<usize>> = (0..n).map(|e| (0..job.filaments()).find(|&f| job.ext_of(f) == e)).collect();
    for e in 0..n {
        if e != cur_noz {
            if let Some(f) = first_use[e] {
                idle[e] = Some((start, t_at(start), temp_of(f, 0), f));
            }
        }
    }
    let mut out = Vec::new();
    let mut nozzle = cur_noz;
    let mut filament = first_filament;
    let mut layer = 0usize;
    let mut layers_seen = 0usize;
    let mut last_change_end: Option<usize> = None;
    for (i, l) in lines.iter().enumerate().take(end_line).skip(start + 1) {
        if l.starts_with("; PRINTING_OBJECT:") {
            idle.fill(None);
            layer = 0;
            layers_seen = 0;
            last_change_end = None;
            continue;
        }
        if *l == "; CHANGE_LAYER" {
            layers_seen += 1;
            layer = layers_seen.saturating_sub(1);
            continue;
        }
        if l.starts_with("; NOZZLE_CHANGE_END") {
            last_change_end = Some(i);
            continue;
        }
        let Some(f) = tool_of(l) else { continue };
        if f >= job.filaments() {
            continue;
        }
        let e = job.ext_of(f);
        if e != nozzle {
            // The old nozzle goes idle where its ramming ended (else at the change).
            let at = last_change_end.filter(|&k| k > start).unwrap_or(i);
            idle[nozzle] = Some((at, t_at(at), temp_of(filament, layer), filament));
            if let Some((from_line, from_t, from_temp, last_filament)) = idle[e].take() {
                out.push(Idle { nozzle: e, last_filament, from_line, from_t, from_temp, until_t: t_at(i), next_filament: f, target: target_of(f, layer) });
            }
            nozzle = e;
        }
        filament = f;
        last_change_end = None;
    }
    out
}

/// Where an insertion after line `k` goes so that it stays out of the printer's template blocks
/// (line ranges): before the block when that is not before line `floor`, else after it.
fn outside(k: usize, floor: usize, blocks: &[(usize, usize)]) -> usize {
    let i = blocks.partition_point(|b| b.1 <= k + 1);
    match blocks.get(i) {
        Some(&(s, e)) if s <= k && k + 1 < e => if s >= 1 && s - 1 >= floor { s - 1 } else { e - 1 },
        _ => k,
    }
}

/// A park may be the nozzle's last one, with no later use to preheat for.
struct Park {
    from_line: usize,
    from_t: f64,
    last_filament: usize,
    from_temp: f64,
    next: Option<(f64, usize, f64)>,
}

fn parks(job: &Job, lines: &[&str], line_end: &[f64], end_line: usize, first_filament: usize, blocks: &[(usize, usize)]) -> Vec<Park> {
    let Some(start) = lines.iter().position(|l| *l == "; MACHINE_START_GCODE_END") else { return Vec::new() };
    let t_at = |i: usize| line_end.get(i).copied().unwrap_or(0.0);
    let temp_of = |f: usize, layer: usize| if layer == 0 { job.fil("nozzle_temperature_initial_layer", f) } else { job.fil("nozzle_temperature", f) };
    let mut out = Vec::new();
    let mut idle = vec![None; job.extruders()];
    let mut nozzle = job.ext_of(first_filament);
    let mut filament = first_filament;
    for (e, slot) in idle.iter_mut().enumerate() {
        if e != nozzle {
            if let Some(f) = (0..job.filaments()).find(|&f| job.ext_of(f) == e) {
                *slot = Some(out.len());
                out.push(Park { from_line: start, from_t: t_at(start), last_filament: f, from_temp: temp_of(f, 0), next: None });
            }
        }
    }
    let mut layers_seen = 0usize;
    for (i, l) in lines.iter().enumerate().take(end_line).skip(start + 1) {
        // The next object starts from its first layer. A parked nozzle stays parked across
        // the boundary, so its park keeps waiting for its next use.
        if l.starts_with("; PRINTING_OBJECT:") {
            layers_seen = 0;
            continue;
        }
        if *l == "; CHANGE_LAYER" {
            layers_seen += 1;
            continue;
        }
        let Some(f) = tool_of(l).filter(|&f| f < job.filaments()) else { continue };
        let e = job.ext_of(f);
        let layer = layers_seen.saturating_sub(1);
        if e != nozzle {
            // After the writer's ramming and retraction, before the tool's template.
            // Include a tool select on the block's last line in that block.
            let b = blocks.partition_point(|b| b.1 <= i);
            let at = blocks.get(b).filter(|b| b.0 <= i).map_or(i - 1, |b| b.0.saturating_sub(1));
            let at = outside(at, start, blocks);
            idle[nozzle] = Some(out.len());
            out.push(Park { from_line: at, from_t: t_at(at), last_filament: filament, from_temp: temp_of(filament, layer), next: None });
            if let Some(p) = idle[e].take() {
                out[p].next = Some((t_at(i), f, temp_of(f, layer)));
            }
            nozzle = e;
        }
        filament = f;
    }
    out
}

fn standby_inserts(job: &Job, firmware: &Firmware, lines: &[&str], line_end: &[f64], end_line: usize, first_filament: usize, blocks: &[(usize, usize)]) -> Inserts {
    let lead = job.cfg.f64("preheat_time").max(0.0);
    let mut out = Vec::new();
    for p in parks(job, lines, line_end, end_line, first_filament, blocks) {
        if p.next.is_some_and(|(until, _, _)| until - p.from_t <= lead) {
            continue;
        }
        let idle_temp = job.fil("idle_temperature", p.last_filament);
        let standby = if idle_temp > 0.0 { idle_temp } else { p.from_temp + job.f("standby_temperature_delta") }.max(0.0);
        let at = outside(p.from_line, p.from_line, blocks);
        out.push((at, vec![firmware.idle_temperature(firmware.tool(job, p.last_filament), standby, "standby")]));
        if lead > 0.0 {
            if let Some((until, f, target)) = p.next {
                let k = line_end[..end_line.min(line_end.len())].partition_point(|&t| t < until - lead).saturating_sub(1).max(at);
                let k = outside(k, at, blocks);
                out.push((k, vec![firmware.idle_temperature(firmware.tool(job, f), target, "preheat")]));
            }
        }
    }
    out.sort_by_key(|x| x.0);
    out
}

/// The commands to insert for the idle stretches, never inside a template block (`blocks`:
/// sorted line ranges; the print check trusts only an unbroken rendering of a template).
pub fn inserts(job: &Job, firmware: &Firmware, lines: &[&str], line_end: &[f64], end_line: usize, first_filament: usize, blocks: &[(usize, usize)], tower: bool) -> Inserts {
    if job.b("ooze_prevention") && job.extruders() >= 2 && !job.b("single_extruder_multi_material") {
        return standby_inserts(job, firmware, lines, line_end, end_line, first_filament, blocks);
    }
    if !job.b("enable_pre_heating") {
        return Vec::new();
    }
    let mut out: Inserts = Vec::new();
    for s in idle_stretches(job, lines, line_end, end_line, first_filament, tower) {
        let dt = (s.until_t - s.from_t).max(0.0);
        let (mid, lead) = plan(s.from_temp, s.target, dt, job.ext("hotend_cooling_rate", s.nozzle), job.ext("hotend_heating_rate", s.nozzle));
        if mid < s.from_temp - 0.5 {
            let at = outside(s.from_line, s.from_line, blocks);
            out.push((at, firmware.idle_cooling(firmware.tool(job, s.last_filament), mid)));
        }
        if (s.target - mid).abs() > 0.5 {
            let t_heat = s.until_t - lead;
            // The last line that ends before heating must start.
            let k = line_end[..end_line.min(line_end.len())].partition_point(|&t| t < t_heat).saturating_sub(1).max(s.from_line);
            // After the cooling (which may itself have moved past a block).
            let k = outside(k, outside(s.from_line, s.from_line, blocks), blocks);
            out.push((k, firmware.idle_heating(firmware.tool(job, s.next_filament), s.target, s.next_filament)));
        }
    }
    out.sort_by_key(|x| x.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_settings::{config::Config, family::Flavor};

    fn parked_job() -> Job {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 4]);
        cfg.set_vec("filament_map", ["4", "2", "4"].map(String::from).to_vec());
        for (key, value) in [
            ("ooze_prevention", "1"),
            ("single_extruder_multi_material", "0"),
            ("enable_pre_heating", "1"),
            ("standby_temperature_delta", "-150"),
            ("preheat_time", "30"),
            ("nozzle_temperature", "220"),
            ("nozzle_temperature_initial_layer", "220"),
            ("filament_preheat_temperature_delta", "10"),
            ("hotend_cooling_rate", "2"),
            ("hotend_heating_rate", "4"),
        ] {
            cfg.set_str(key, value);
        }
        Job::new(cfg)
    }

    #[test]
    fn standby_and_preheat_surround_templates_and_use_filament_tools() {
        let job = parked_job();
        let lines = [
            "; MACHINE_START_GCODE_END", "; CHANGE_LAYER", "; CHANGE_LAYER",
            "G1 E1", "; NOZZLE_CHANGE_END", "G1 E-1",
            "; change", "T1", "; end",
            "G1 X1", "; timed template", "G1 X2", "; end", "G1 X3",
            "; change", "T2", "G1 X4",
        ];
        let times = [0.0, 0.0, 0.0, 5.0, 5.0, 10.0, 10.0, 12.0, 12.0, 40.0, 60.0, 65.0, 70.0, 80.0, 90.0, 100.0, 110.0];
        let blocks = [(6, 9), (10, 13), (14, 16)];
        let out = inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &blocks, true);
        assert_eq!(out, vec![
            (5, vec!["M104 T0 S70 ; standby".to_string()]),
            (9, vec!["M104 T2 S220 ; preheat".to_string()]),
            (13, vec!["M104 T1 S70 ; standby".to_string()]),
        ], "standby follows retraction; preheat moves before its template; the final park only cools");
        let out = inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], true);
        assert_eq!(out[1], (11, vec!["M104 T2 S220 ; preheat".to_string()]), "last line strictly before 70 seconds, without a template");
    }

    #[test]
    fn short_parks_stay_hot_and_start_parks_preheat_in_order() {
        let job = parked_job();
        let lines = ["; MACHINE_START_GCODE_END", "G1 X1", "T1", "G1 X2", "T0"];
        for duration in [20.0, 30.0] {
            let times = [0.0, 100.0, 100.0, 100.0 + duration, 100.0 + duration];
            assert_eq!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false), vec![
                (0, vec!["M104 T1 S70 ; standby".to_string()]),
                (0, vec!["M104 T1 S220 ; preheat".to_string()]),
                (3, vec!["M104 T1 S70 ; standby".to_string()]),
            ], "a {duration}-second park stays hot, unused nozzles get nothing, and preheat follows standby even at the same line");
        }
        let times = [0.0, 20.0, 20.0, 40.0, 40.0];
        assert_eq!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false), vec![
            (3, vec!["M104 T1 S70 ; standby".to_string()]),
        ], "a short initial park also stays hot");
        // Printing object by object: the marker before each object does not end a park.
        let lines = ["; MACHINE_START_GCODE_END", "; PRINTING_OBJECT: 0", "G1 X1", "; PRINTING_OBJECT: 1", "T1", "G1 X2", "T0"];
        for (times, expected) in [
            ([0.0, 0.0, 100.0, 100.0, 100.0, 130.0, 130.0], vec![(0, vec!["M104 T1 S70 ; standby".to_string()]), (1, vec!["M104 T1 S220 ; preheat".to_string()]), (5, vec!["M104 T1 S70 ; standby".to_string()])]),
            ([0.0, 0.0, 20.0, 20.0, 20.0, 50.0, 50.0], vec![(5, vec!["M104 T1 S70 ; standby".to_string()])]),
        ] {
            assert_eq!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false), expected, "object markers, first use at {}", times[4]);
        }
    }

    #[test]
    fn standby_override_first_layer_and_missing_preheat_time() {
        let mut job = parked_job();
        job.cfg.set_str("nozzle_temperature_initial_layer", "230");
        let lines = ["; MACHINE_START_GCODE_END", "; CHANGE_LAYER", "G1 X1", "T1", "G1 X2", "G1 X3", "T2"];
        let times = [0.0, 0.0, 10.0, 10.0, 60.0, 70.0, 100.0];
        assert_eq!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], true), vec![
            (2, vec!["M104 T0 S80 ; standby".to_string()]),
            (4, vec!["M104 T2 S230 ; preheat".to_string()]),
            (5, vec!["M104 T1 S80 ; standby".to_string()]),
        ], "both targets use the first layer, including the next filament, without the tower delta");
        job.cfg.set_vec("idle_temperature", ["100", "90", "0"].map(String::from).to_vec());
        let out = inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false);
        assert_eq!(out[0].1, vec!["M104 T0 S100 ; standby"]);
        assert_eq!(out[2].1, vec!["M104 T1 S90 ; standby"]);
        job.cfg.remove("idle_temperature");
        job.cfg.remove("preheat_time");
        job.cfg.set_str("standby_temperature_delta", "-300");
        assert_eq!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false), vec![
            (0, vec!["M104 T1 S0 ; standby".to_string()]),
            (2, vec!["M104 T0 S0 ; standby".to_string()]),
            (5, vec!["M104 T1 S0 ; standby".to_string()]),
        ], "missing optional values are zero and standby never goes below zero");
    }

    #[test]
    fn object_boundary_keeps_parks_and_restarts_layer_temperatures() {
        let mut job = parked_job();
        job.cfg.set_str("nozzle_temperature_initial_layer", "230");
        let lines = [
            "; MACHINE_START_GCODE_END", "; CHANGE_LAYER", "; CHANGE_LAYER", "G1 X1", "T1",
            "; PRINTING_OBJECT: 1", "; CHANGE_LAYER", "G1 X2", "T0",
        ];
        let times = [0.0, 0.0, 0.0, 10.0, 10.0, 40.0, 40.0, 90.0, 100.0];
        assert_eq!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false), vec![
            (3, vec!["M104 T0 S70 ; standby".to_string()]),
            (6, vec!["M104 T0 S230 ; preheat".to_string()]),
            (7, vec!["M104 T1 S80 ; standby".to_string()]),
        ], "a park waits across an object boundary; the next object starts on its first layer");
    }

    #[test]
    fn rate_policy_output_is_unchanged_and_standby_requires_separate_nozzles() {
        let mut job = parked_job();
        job.cfg.set_str("ooze_prevention", "0");
        let lines = ["; MACHINE_START_GCODE_END", "G1 X1", "T1", "G1 X2", "T0"];
        let times = [0.0, 20.0, 30.0, 50.0, 60.0];
        assert_eq!(inserts(&job, &Firmware(Flavor::Mask), &lines, &times, lines.len(), 0, &[], false), vec![
            (0, vec!["M400".to_string(), "M104 T1 S180 N0 ;Multi extruder pre cooling".to_string()]),
            (0, vec!["M632 S1 W".to_string(), "M104 T1 S220 N0 ;Multi extruder pre heating".to_string(), "M633".to_string()]),
            (2, vec!["M400".to_string(), "M104 T3 S180 N0 ;Multi extruder pre cooling".to_string()]),
            (2, vec!["M632 S0 W".to_string(), "M104 T3 S220 N0 ;Multi extruder pre heating".to_string(), "M633".to_string()]),
        ]);
        job.cfg.set_str("enable_pre_heating", "0");
        assert!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false).is_empty());
        job.cfg.set_str("ooze_prevention", "1");
        assert!(!inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false).is_empty());
        job.cfg.set_str("single_extruder_multi_material", "1");
        assert!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false).is_empty());
        job.cfg.set_str("single_extruder_multi_material", "0");
        job.cfg.set_str("nozzle_diameter", "0.4");
        job.fil_ext.fill(0);
        assert!(inserts(&job, &Firmware(Flavor::Named), &lines, &times, lines.len(), 0, &[], false).is_empty());
    }

    #[test]
    fn cools_as_long_as_it_can_and_still_reaches_the_target() {
        // 220 °C, back at 210 after 30 s, cooling 2 °C/s, heating 3.6 °C/s.
        let (mid, lead) = plan(220.0, 210.0, 30.0, 2.0, 3.6);
        let t1 = 30.0 - lead;
        assert!((mid - (220.0 - 2.0 * t1)).abs() < 1e-9);
        assert!((mid + 3.6 * lead - 210.0).abs() < 1e-9);
        assert!(mid > 170.0 && mid < 185.0, "{mid}");
        // Too short to cool below the target.
        assert_eq!(plan(220.0, 219.0, 0.1, 2.0, 3.6).0, 219.0);
        // Never below room temperature.
        assert!(plan(220.0, 210.0, 10_000.0, 2.0, 3.6).0 >= ROOM);
        // Heating only.
        assert_eq!(plan(200.0, 220.0, 100.0, 2.0, 4.0), (200.0, 5.0));
        // Back at the temperature it left at (no tower to print cooler on): it still cools.
        let (mid, lead) = plan(220.0, 220.0, 28.0, 2.0, 3.6);
        assert!((mid - 184.0).abs() < 1e-9 && (lead - 10.0).abs() < 1e-9, "{mid} {lead}");
    }
}

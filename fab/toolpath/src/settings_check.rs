use makepad_fab_settings::machine::Job;

const HOPS: [&str; 4] = ["Auto Lift", "Normal Lift", "Slope Lift", "Spiral Lift"];

/// The most nozzles a job can number.
const MAX_NOZZLES: usize = u8::MAX as usize + 1;

/// Collect invalid values and settings that Fab cannot apply yet. A key the job does not
/// set takes its default, as in `brim::check`.
pub fn check(job: &Job) -> Result<(), String> {
    let mut bad = Vec::new();
    // A move record, a printer's head and a tray unit each number a nozzle in a byte.
    if job.extruders() > MAX_NOZZLES {
        bad.push(format!("nozzle_diameter lists {} nozzles (Fab numbers at most {MAX_NOZZLES})", job.extruders()));
    }
    for (key, allowed) in [
        ("seam_position", &["nearest", "aligned", "back", "random"][..]),
        ("print_sequence", &["by layer", "by object"][..]),
        ("draft_shield", &["disabled", "limited", "enabled"][..]),
        ("wall_generator", &["classic", "arachne"][..]),
        ("wall_sequence", &["inner wall/outer wall", "outer wall/inner wall", "inner-outer-inner wall"][..]),
    ] {
        let v = job.s(key);
        if job.cfg.has(key) && !allowed.contains(&v) {
            bad.push(format!("{key} \"{v}\" (expected {})", allowed.join(", ")));
        }
    }
    for e in 0..job.extruders() {
        let v = job.cfg.raw("z_hop_types", e);
        if job.cfg.has("z_hop_types") && !HOPS.contains(&v) {
            bad.push(format!("z_hop_types[{e}] \"{v}\" (expected {})", HOPS.join(", ")));
        }
    }
    for f in 0..job.filaments() {
        let v = job.fil_str("filament_z_hop_types", f);
        if !v.is_empty() && v != "nil" && !HOPS.contains(&v) {
            bad.push(format!("filament_z_hop_types[{f}] \"{v}\" (expected {})", HOPS.join(", ")));
        }
    }
    for key in ["xy_contour_compensation", "xy_hole_compensation"] {
        let v = job.f(key);
        if !v.is_finite() || v.abs() > 2.0 {
            bad.push(format!("{key} \"{}\" (must be finite and between -2 and 2 mm)", job.s(key)));
        }
    }
    for (key, max) in [("raft_layers", 100), ("skirt_loops", 20)] {
        if !job.f(key).is_finite() || !(0..=max).contains(&job.cfg.i64_at(key, 0)) {
            bad.push(format!("{key} \"{}\" (must be finite and between 0 and {max})", job.s(key)));
        }
    }
    if !job.f("skirt_height").is_finite() || (job.cfg.i64_at("skirt_loops", 0) > 0 && job.cfg.i64_at("skirt_height", 0) < 1) {
        bad.push(format!("skirt_height \"{}\" (must be finite and at least 1 when skirt_loops > 0)", job.s("skirt_height")));
    }
    let distance = job.f("skirt_distance");
    if !distance.is_finite() || distance < 0.0 {
        bad.push(format!("skirt_distance \"{}\" (must be finite and non-negative)", job.s("skirt_distance")));
    }
    if job.cfg.i64_at("raft_layers", 0) > 0 {
        for (key, lo, hi) in [("raft_contact_distance", 0.0, 10.0), ("raft_expansion", 0.0, 50.0), ("raft_first_layer_density", 0.0, 100.0), ("raft_first_layer_expansion", -100.0, 50.0)] {
            let v = job.f(key);
            if !v.is_finite() || v < lo || v > hi {
                bad.push(format!("{key} \"{}\" (must be finite and between {lo} and {hi})", job.s(key)));
            }
        }
    }
    let wipe = job.f("wipe_speed");
    if job.cfg.has("wipe_speed") && (!wipe.is_finite() || wipe <= 0.0 || wipe > 100.0) {
        bad.push(format!("wipe_speed \"{}\" (must be finite, greater than 0 and at most 100%)", job.s("wipe_speed")));
    }
    // A restart pushes back the retraction plus the extra: never a negative push, and a prime
    // of at most 5 mm (the print check's allowance on an in-place push past a retraction).
    for f in 0..job.filaments() {
        let length = job.eff("retraction_length", f);
        let v = job.eff("retract_restart_extra", f);
        if !v.is_finite() || v < -length || v > 5.0 {
            bad.push(format!("retract_restart_extra[{f}] \"{}\" (must be finite and between {} and 5 mm)", job.eff_str("retract_restart_extra", f), -length));
        }
    }
    for e in 0..job.extruders() {
        let length = job.ext("retract_length_toolchange", e);
        let v = job.ext("retract_restart_extra_toolchange", e);
        if !v.is_finite() || v < -length || v > 5.0 {
            bad.push(format!("retract_restart_extra_toolchange[{e}] \"{}\" (must be finite and between {} and 5 mm)", job.cfg.raw("retract_restart_extra_toolchange", e), -length));
        }
    }
    if job.b("reduce_crossing_wall") {
        let v = job.f("max_travel_detour_distance");
        if !v.is_finite() || v < 0.0 {
            bad.push(format!("max_travel_detour_distance \"{}\" (must be finite and non-negative, in mm or %)", job.s("max_travel_detour_distance")));
        }
    }
    // Arachne walls (spiral vase forces classic): every bead layout its
    // settings ask for must exist, for each nozzle's normal and first-layer widths.
    if let Some(a) = crate::arachne::Settings::from_job(job).filter(|_| !job.b("spiral_mode")) {
        let loops = job.cfg.i64_at("wall_loops", 0).max(1) as usize;
        for e in 0..job.extruders() {
            let nozzle = job.nozzle_diameter(e).max(0.1);
            let lw = crate::line_width(job, "line_width", nozzle, nozzle * 1.05);
            let w = |k: &str| crate::line_width(job, k, nozzle, lw);
            let first = w("initial_layer_line_width");
            let layers = [
                (w("outer_wall_line_width"), w("inner_wall_line_width"), job.f("layer_height").max(0.01), ""),
                (first, first, job.f("initial_layer_print_height").max(0.01), " on the first layer"),
            ];
            for (wo, wi, h, which) in layers {
                if let Err(why) = a.config(loops, wo, wi, h, false).check() {
                    let msg = format!("wall_generator \"arachne\" for nozzle {e}{which}: {why}");
                    if !bad.contains(&msg) {
                        bad.push(msg);
                    }
                }
            }
        }
    }
    // Trees stand on the printer's bed (`bed_area`): a job with none would plan them to nothing.
    if crate::plans_trees(job) && job.cfg.points("printable_area").len() < 3 {
        bad.push("printable_area lists no bed (tree supports stand on the printer's bed)".into());
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!("invalid settings: {}", bad.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any number of nozzles a byte can number, and no more.
    #[test]
    fn a_job_numbers_its_nozzles_in_a_byte() {
        let job = |n: usize| {
            let mut cfg = makepad_fab_settings::config::Config::new();
            cfg.set_vec("nozzle_diameter", vec!["0.4".to_string(); n]);
            Job::new(cfg)
        };
        let nozzles = |n| check(&job(n)).err().is_some_and(|e| e.contains("nozzle_diameter lists"));
        assert!(!nozzles(9) && !nozzles(MAX_NOZZLES));
        assert!(nozzles(MAX_NOZZLES + 1));
    }

    /// Trees stand on the printer's bed: a job that plans them without one is refused, never
    /// planned to nothing; one with the default printer's bed, or with other supports, is not.
    #[test]
    fn trees_are_refused_without_the_printers_bed() {
        let job = |support: &str, bed: bool| {
            let mut cfg = makepad_fab_settings::config::Config::new();
            cfg.set_str("enable_support", "1");
            cfg.set_str("support_type", support);
            if bed {
                cfg.set_vec("printable_area", fab_plate::machine::Machine::dual_nozzle_350().bed.corners());
            }
            Job::new(cfg)
        };
        let refused = |support, bed| check(&job(support, bed)).err().is_some_and(|e| e.contains("printable_area lists no bed"));
        assert!(refused("tree(auto)", false));
        assert!(!refused("tree(auto)", true) && !refused("normal(auto)", false));
    }
}

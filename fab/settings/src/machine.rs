//! Typed reads of a collapsed job configuration: per physical extruder,
//! per filament slot, and the effective per-filament values where a
//! filament overrides its extruder (retraction, wipe, z-hop).

use crate::config::Config;

/// Bed plates and their filament temperature keys, in menu order.
pub const PLATES: &[(&str, &str)] = &[
    ("Cool Plate", "cool_plate_temp"),
    ("Engineering Plate", "eng_plate_temp"),
    ("High Temp Plate", "hot_plate_temp"),
    ("Textured PEI Plate", "textured_plate_temp"),
    ("Supertack Plate", "supertack_plate_temp"),
    ("Textured Cool Plate", "textured_cool_plate_temp"),
];

/// Installed selection indices into `PLATES`, for the first tree, fork and mainline.
pub const FIRST_TREE_PLATE_INDICES: &[(&str, usize)] = &[("1", 0), ("2", 1), ("3", 2), ("4", 3), ("5", 4)];
pub const FORK_PLATE_INDICES: &[(&str, usize)] = &[("0", 0), ("1", 0), ("2", 1), ("3", 2), ("4", 3), ("5", 5), ("7", 4)];
pub const MAINLINE_PLATE_INDICES: &[(&str, usize)] = &[("1", 0), ("2", 1), ("3", 2), ("4", 3), ("5", 5), ("6", 4)];

/// The job configuration seen through physical extruders and filaments.
#[derive(Clone)]
pub struct Job {
    pub cfg: Config,
    /// 0-based physical extruder per filament.
    pub fil_ext: Vec<usize>,
}

impl Job {
    pub fn new(cfg: Config) -> Self {
        let n = cfg.vec_len("filament_map").max(1);
        let fil_ext = (0..n).map(|f| (cfg.i64_at("filament_map", f).max(1) - 1) as usize).collect();
        Self { cfg, fil_ext }
    }

    /// This job with per-part setting overrides (key, value) applied on top.
    pub fn with_overrides(&self, overrides: &[(String, String)]) -> Job {
        let mut cfg = self.cfg.clone();
        for (k, v) in overrides {
            cfg.set_str(k, v.clone());
        }
        Job { cfg, fil_ext: self.fil_ext.clone() }
    }

    /// The G-code dialect of the job's printer.
    pub fn flavor(&self) -> Result<crate::family::Flavor, &'static str> {
        crate::family::of(&self.cfg).map(|f| f.flavor).ok_or(crate::family::UNSUPPORTED)
    }

    pub fn extruders(&self) -> usize {
        self.cfg.vec_len("nozzle_diameter").max(1)
    }

    pub fn filaments(&self) -> usize {
        self.fil_ext.len()
    }

    pub fn ext_of(&self, f: usize) -> usize {
        self.fil_ext.get(f).copied().unwrap_or(0)
    }

    /// The firmware's tool index of a physical extruder.
    pub fn physical_index(&self, e: usize) -> usize {
        let len = self.cfg.vec_len("physical_extruder_map");
        if self.cfg.has("physical_extruder_map") && (len >= self.extruders() || e < len) {
            self.cfg.i64_at("physical_extruder_map", e).max(0) as usize
        } else {
            e
        }
    }

    pub fn ext(&self, key: &str, e: usize) -> f64 {
        self.cfg.f64_at(key, e)
    }

    pub fn fil(&self, key: &str, f: usize) -> f64 {
        self.cfg.f64_at(key, f)
    }

    pub fn fil_str(&self, key: &str, f: usize) -> &str {
        self.cfg.raw(key, f)
    }

    pub fn f(&self, key: &str) -> f64 {
        self.cfg.f64(key)
    }

    pub fn b(&self, key: &str) -> bool {
        self.cfg.bool(key)
    }

    pub fn s(&self, key: &str) -> &str {
        self.cfg.str(key)
    }

    /// A filament override (`filament_<key>`) when set, else the value of
    /// the filament's extruder.
    pub fn eff(&self, key: &str, f: usize) -> f64 {
        let fk = format!("filament_{key}");
        let raw = self.cfg.raw(&fk, f);
        if self.cfg.has(&fk) && raw != "nil" && !raw.is_empty() {
            return self.cfg.f64_at(&fk, f);
        }
        self.cfg.f64_at(key, self.ext_of(f))
    }

    pub fn eff_str(&self, key: &str, f: usize) -> String {
        let fk = format!("filament_{key}");
        let raw = self.cfg.raw(&fk, f);
        if self.cfg.has(&fk) && raw != "nil" && !raw.is_empty() {
            return raw.to_string();
        }
        self.cfg.raw(key, self.ext_of(f)).to_string()
    }

    pub fn eff_bool(&self, key: &str, f: usize) -> bool {
        matches!(self.eff_str(key, f).as_str(), "1" | "true")
    }

    pub fn nozzle_diameter(&self, e: usize) -> f64 {
        self.cfg.f64_at("nozzle_diameter", e)
    }

    /// Bed temperature key prefix for the selected plate.
    pub fn bed_temp_key(&self) -> Option<&'static str> {
        PLATES.iter().find(|(name, _)| *name == self.cfg.str("curr_bed_type")).map(|(_, key)| *key)
    }

    pub fn bed_temp(&self, f: usize, first_layer: bool) -> f64 {
        if self.cfg.bool_at("fab_bed_unheated", f) { return 0.0; }
        // An unknown plate heats nothing: slicing refuses one (`prepare_job`).
        let Some(k) = self.bed_temp_key() else { return 0.0 };
        if first_layer {
            self.cfg.f64_at(&format!("{k}_initial_layer"), f)
        } else {
            self.cfg.f64_at(k, f)
        }
    }

    /// The printable rectangle of a physical extruder, (min, max) in mm.
    pub fn reach(&self, e: usize) -> ((f64, f64), (f64, f64)) {
        let groups = self.cfg.point_groups("extruder_printable_area");
        let pts = groups.get(e).cloned().filter(|g| !g.is_empty()).unwrap_or_else(|| self.cfg.points("printable_area"));
        let mut lo = (f64::MAX, f64::MAX);
        let mut hi = (f64::MIN, f64::MIN);
        for (x, y) in pts {
            lo.0 = lo.0.min(x);
            lo.1 = lo.1.min(y);
            hi.0 = hi.0.max(x);
            hi.1 = hi.1.max(y);
        }
        (lo, hi)
    }

    /// Filament cross-section in mm².
    pub fn filament_area(&self, f: usize) -> f64 {
        let d = self.cfg.f64_at("filament_diameter", f);
        std::f64::consts::PI * d * d / 4.0
    }
}

/// A filament suits a plate when it asks for no bed heat, or has a positive
/// temperature for that plate. Inputs may come from a preset or a job slot.
pub fn filament_suits_plate(unheated: bool, temperature: f64) -> bool {
    unheated || temperature > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_map_keeps_missing_heads_at_their_own_indices() {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 4]);
        for (map, expected) in [
            (vec!["0"], [0, 1, 2, 3]),
            (vec!["1", "0"], [1, 0, 2, 3]),
            (vec!["3", "2", "1", "0"], [3, 2, 1, 0]),
        ] {
            cfg.set_vec("physical_extruder_map", map.into_iter().map(String::from).collect());
            let job = Job::new(cfg.clone());
            assert_eq!([0, 1, 2, 3].map(|e| job.physical_index(e)), expected);
        }
    }
}

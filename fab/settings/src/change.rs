//! What a filament or tool change costs on the printer: how much of the incoming filament runs
//! through the nozzle to clear the one it held, and how long that takes. The writer hands the
//! purge to the printer's change sequence and the time model counts it, so the Sliced view,
//! Printability and the print time all carry it.
//!
//! - A nozzle that takes another filament out through a shared path purges the profile's
//!   flush volume for the pair (`flush_volumes_matrix`, from Fab's colour model
//!   [`crate::flush`] when the profile has none for this filament set), times the nozzle's
//!   flush multiplier, less the grab credit.
//! - A nozzle that still holds the filament purges nothing: the head switching to its second
//!   nozzle, or a tool changer's tool, which carries its own filament. That nozzle's prime is
//!   the prime tower's (`filament_prime_volume`), when the print has one.
//! - A filament on a dedicated path (`fab_dedicated_path`) shares no path with the others:
//!   changes into or out of it purge nothing.
//! - A nozzle loading its first filament purges nothing: the start sequence primes it.
//!
//! Besides the purge a change takes the profile's own times: the head switching to another
//! nozzle (`machine_switch_extruder_time`), and a nozzle loading a filament it does not hold
//! (`machine_load_filament_time`), unloading the one it held first
//! (`machine_unload_filament_time`).

use crate::machine::Job;

/// What one change costs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Change {
    /// The filament the nozzle gives up (None: it held the incoming one, or nothing).
    pub out: Option<usize>,
    /// Incoming filament run through the nozzle (mm³).
    pub purge_mm3: f64,
    /// The purge's time at the incoming filament's flush speed (s).
    pub purge_s: f64,
}

/// A job's change costs.
#[derive(Clone, Debug)]
pub struct ChangeModel {
    nf: usize,
    fil_ext: Vec<usize>,
    /// Per nozzle an N×N block (from, to) in mm³: the multiplier applied, the credit taken.
    purge: Vec<f64>,
    dedicated: Vec<bool>,
    /// Per filament, its flush speed (mm³/s).
    flush_speed: Vec<f64>,
    /// Per filament, its density (g/cm³).
    density: Vec<f64>,
    /// The head switching nozzles, a nozzle loading a filament and unloading one (s).
    pub switch_s: f64,
    pub load_s: f64,
    pub unload_s: f64,
}

impl ChangeModel {
    pub fn new(job: &Job) -> Self {
        let cfg = &job.cfg;
        let (nf, ne) = (job.filaments(), job.extruders());
        let matrix: Vec<f64> = if cfg.vec_len("flush_volumes_matrix") == ne * nf * nf {
            (0..ne * nf * nf).map(|i| cfg.f64_at("flush_volumes_matrix", i)).collect()
        } else {
            let colours: Vec<String> = (0..nf).map(|f| cfg.raw("filament_colour", f).to_string()).collect();
            crate::flush::matrix(&colours, ne)
        };
        let key = if cfg.str("prime_volume_mode") == "Fast" { "flush_multiplier_fast" } else { "flush_multiplier" };
        let mut purge = Vec::with_capacity(matrix.len());
        for e in 0..ne {
            // A job that names no multiplier purges the volumes as they are.
            let mult = if cfg.has(key) { cfg.f64_at(key, e).max(0.0) } else { 1.0 };
            let credit = cfg.f64_at("grab_length", e) * 2.4;
            purge.extend(matrix[e * nf * nf..(e + 1) * nf * nf].iter().map(|v| (v * mult - credit).max(0.0)));
        }
        let speed = |f: usize| {
            let v = cfg.f64_at("filament_flush_volumetric_speed", f);
            if v > 0.0 {
                v
            } else {
                cfg.f64_at("filament_max_volumetric_speed", f)
            }
        };
        ChangeModel {
            nf,
            fil_ext: job.fil_ext.clone(),
            purge,
            dedicated: (0..nf).map(|f| cfg.has("fab_dedicated_path") && cfg.bool_at("fab_dedicated_path", f)).collect(),
            flush_speed: (0..nf).map(speed).collect(),
            density: (0..nf).map(|f| cfg.f64_at("filament_density", f)).collect(),
            switch_s: cfg.f64("machine_switch_extruder_time"),
            load_s: cfg.f64("machine_load_filament_time"),
            unload_s: cfg.f64("machine_unload_filament_time"),
        }
    }

    /// The nozzle filament `f` prints with.
    pub fn ext_of(&self, f: usize) -> usize {
        self.fil_ext.get(f).copied().unwrap_or(0)
    }

    /// The mass of `mm3` of filament `f` (g).
    pub fn grams(&self, f: usize, mm3: f64) -> f64 {
        mm3 * self.density.get(f).copied().unwrap_or(0.0) / 1000.0
    }

    /// The printer's nozzles.
    pub fn nozzles(&self) -> usize {
        (self.purge.len() / (self.nf * self.nf).max(1)).max(1)
    }

    /// The same printer and filaments, each filament on the nozzle `fil_ext` gives it.
    pub fn on_nozzles(&self, fil_ext: &[usize]) -> ChangeModel {
        ChangeModel { fil_ext: fil_ext.to_vec(), ..self.clone() }
    }

    /// The change of a nozzle that holds `held` to filament `to` (its nozzle is `to`'s).
    pub fn change(&self, held: Option<usize>, to: usize) -> Change {
        let Some(out) = held.filter(|&h| h != to) else { return Change::default() };
        if out >= self.nf || to >= self.nf {
            return Change { out: Some(out), ..Change::default() };
        }
        let e = self.fil_ext.get(to).copied().unwrap_or(0);
        let shared = !self.dedicated[out] && !self.dedicated[to];
        let purge_mm3 = if shared { self.purge.get((e * self.nf + out) * self.nf + to).copied().unwrap_or(0.0) } else { 0.0 };
        let speed = self.flush_speed[to];
        Change { out: Some(out), purge_mm3, purge_s: if purge_mm3 > 0.0 && speed > 0.0 { purge_mm3 / speed } else { 0.0 } }
    }

    /// No change of this job purges: the profile says swaps cost nothing, or no nozzle ever
    /// takes another filament out.
    pub fn free(&self) -> bool {
        (0..self.nf).all(|to| (0..self.nf).all(|from| from == to || self.fil_ext.get(from) != self.fil_ext.get(to) || self.change(Some(from), to).purge_mm3 <= 0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn job(nozzles: usize, map: &[u32], colours: &[&str]) -> Job {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); nozzles]);
        cfg.set_vec("filament_map", map.iter().map(|m| m.to_string()).collect());
        cfg.set_vec("filament_colour", colours.iter().map(|c| c.to_string()).collect());
        cfg.set_vec("filament_max_volumetric_speed", vec!["20".into(); map.len()]);
        Job::new(cfg)
    }

    /// The three shapes of printer: a two-nozzle head whose nozzles share the material slots,
    /// one nozzle fed from four slots, and a tool changer whose tools carry a filament each.
    #[test]
    fn changes_purge_where_a_nozzle_takes_another_filament_out() {
        let colours = ["#FFFFFF", "#000000", "#FF0000", "#0000FF"];
        // Two nozzles, slots 1 and 3 on the left, 2 and 4 on the right.
        let two = ChangeModel::new(&job(2, &[1, 2, 1, 2], &colours));
        let swap = two.change(Some(0), 2);
        assert_eq!(swap.out, Some(0));
        assert_eq!(swap.purge_mm3, crate::flush::purge_volume(colours[0], colours[2]).round(), "a same-nozzle swap purges the flush model's volume");
        assert!((swap.purge_s - swap.purge_mm3 / 20.0).abs() < 1e-9, "at the flush speed: {swap:?}");
        assert_eq!(two.change(Some(1), 1), Change::default(), "the head switching to a nozzle that holds the filament purges nothing");
        assert_eq!(two.change(None, 1), Change::default(), "a first load purges nothing");
        assert!(two.change(Some(1), 3).purge_mm3 > two.change(Some(3), 1).purge_mm3, "dark to light costs more than light to dark on the right nozzle too");
        assert!(!two.free());
        // One nozzle, four slots: every change is a swap.
        let four = ChangeModel::new(&job(1, &[1, 1, 1, 1], &colours));
        for (a, b) in [(0, 1), (1, 0), (2, 3), (3, 2), (1, 2)] {
            assert_eq!(four.change(Some(a), b).purge_mm3, crate::flush::purge_volume(colours[a], colours[b]).round(), "{a} → {b}");
        }
        // A tool changer: a tool each, no change ever takes a filament out.
        let tools = ChangeModel::new(&job(4, &[1, 2, 3, 4], &colours));
        assert!(tools.free(), "a tool changer's changes cost nothing");
    }

    /// The profile's own matrix, its multiplier and grab credit, and a dedicated path.
    #[test]
    fn the_profile_sets_the_volumes() {
        let mut j = job(1, &[1, 1, 1], &["#FFFFFF", "#000000", "#FF0000"]);
        j.cfg.set_vec("flush_volumes_matrix", "0,100,200,300,0,400,500,600,0".split(',').map(String::from).collect());
        j.cfg.set_vec("flush_multiplier", vec!["1.5".into()]);
        j.cfg.set_vec("grab_length", vec!["10".into()]);
        let m = ChangeModel::new(&j);
        assert_eq!(m.change(Some(1), 2).purge_mm3, 400.0 * 1.5 - 24.0, "matrix × multiplier − credit");
        j.cfg.set_str("prime_volume_mode", "Fast");
        j.cfg.set_vec("flush_multiplier_fast", vec!["2".into()]);
        assert_eq!(ChangeModel::new(&j).change(Some(1), 2).purge_mm3, 400.0 * 2.0 - 24.0, "the fast mode's multiplier");
        j.cfg.set_vec("fab_dedicated_path", vec!["0".into(), "0".into(), "1".into()]);
        let m = ChangeModel::new(&j);
        assert_eq!(m.change(Some(1), 2).purge_mm3, 0.0, "into a dedicated path");
        assert_eq!(m.change(Some(2), 0).purge_mm3, 0.0, "out of one");
        assert!(m.change(Some(0), 1).purge_mm3 > 0.0, "the shared slots still purge");
        j.cfg.set_vec("flush_multiplier_fast", vec!["0".into()]);
        assert!(ChangeModel::new(&j).free(), "a zero multiplier: changes cost nothing");
    }
}

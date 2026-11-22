//! Calibration results into what prints: the filament's slicer settings, and the fits
//! Mechanics sizes joints with.

use crate::results::{names, Results};
use makepad_fab_settings::{Config, Value};

/// Mechanics' measured fits for one printer and filament; `None` where the step was not done
/// (Mechanics then keeps its first-principles rule).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MechFit {
    /// In-plane clearance of print-in-place joints (mm).
    pub c_xy: Option<f64>,
    /// Empty layers between print-in-place parts across layers.
    pub c_z_layers: Option<u32>,
    /// Per-side slip fit and press fit of parts printed apart (mm).
    pub slip_fit: Option<f64>,
    pub press_fit: Option<f64>,
    /// Snap interference: the largest that goes in, the smallest that holds (mm).
    pub snap_delta_max: Option<f64>,
    pub snap_delta_hold: Option<f64>,
    /// Joint friction coefficient.
    pub mu: Option<f64>,
}

impl MechFit {
    pub fn from_results(r: &Results) -> Self {
        MechFit {
            c_xy: r.get(names::C_XY),
            c_z_layers: r.get(names::C_Z_LAYERS).map(|v| v.round().max(1.0) as u32),
            slip_fit: r.get(names::SLIP_FIT),
            press_fit: r.get(names::PRESS_FIT),
            snap_delta_max: r.get(names::SNAP_MAX),
            snap_delta_hold: r.get(names::SNAP_HOLD),
            mu: r.get(names::MU),
        }
    }

    /// The measured fits of a printer and filament (from the calibration folder).
    pub fn load(printer: &str, filament: &str) -> Self {
        Self::from_results(&Results::load(printer, filament))
    }

    /// Clearance across layers in mm at a layer height.
    pub fn c_z(&self, layer: f64) -> Option<f64> {
        self.c_z_layers.map(|n| n as f64 * layer)
    }

    /// A snap interference inside the measured window, or `None` without a measurement.
    pub fn snap_delta(&self, wanted: f64) -> Option<f64> {
        match (self.snap_delta_hold, self.snap_delta_max) {
            (Some(lo), Some(hi)) => Some(wanted.clamp(lo, hi)),
            _ => None,
        }
    }
}

/// Every entry of a filament key set to `v` (variant vectors stay as long as they are).
fn set_all(values: &mut Config, key: &str, v: String) {
    let n = values.get(key).map(|x| x.len()).unwrap_or(1).max(1);
    values.set(key, Value::Vector(vec![v; n]));
}

/// Put the measured flow ratio, temperature and pressure advance into a filament preset's
/// values (as the job will use them). Returns what changed, for the status line.
pub fn apply_filament(r: &Results, values: &mut Config) -> Vec<String> {
    let mut said = Vec::new();
    if let Some(f) = r.get(names::FLOW_RATIO).filter(|f| (0.5..=1.5).contains(f)) {
        set_all(values, "filament_flow_ratio", format!("{f:.3}"));
        said.push(format!("flow {f:.3}"));
    }
    if let Some(t) = r.get(names::NOZZLE_TEMP).filter(|t| (150.0..=350.0).contains(t)) {
        // The first layer keeps the preset's offset from the other layers.
        let old = values.get("nozzle_temperature").map(|v| v.at(0).parse::<f64>().unwrap_or(t)).unwrap_or(t);
        let first = values.get("nozzle_temperature_initial_layer").map(|v| v.at(0).parse::<f64>().unwrap_or(old)).unwrap_or(old);
        set_all(values, "nozzle_temperature", format!("{t:.0}"));
        set_all(values, "nozzle_temperature_initial_layer", format!("{:.0}", (t + first - old).clamp(150.0, 350.0)));
        said.push(format!("{t:.0} °C"));
    }
    if let Some(k) = r.get(names::PRESSURE_ADVANCE).filter(|k| (0.0..=0.5).contains(k)) {
        set_all(values, "pressure_advance", format!("{k:.4}"));
        set_all(values, "enable_pressure_advance", "1".into());
        said.push(format!("PA {k:.3}"));
    }
    said
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filament_values_take_the_measurements() {
        let mut r = Results::new("p", "f");
        r.values.insert(names::FLOW_RATIO.into(), 0.96);
        r.values.insert(names::NOZZLE_TEMP.into(), 215.0);
        r.values.insert(names::PRESSURE_ADVANCE.into(), 0.03);
        let mut c = Config::new();
        c.set_vec("filament_flow_ratio", vec!["0.98".into(), "0.98".into()]);
        c.set_vec("nozzle_temperature", vec!["220".into()]);
        c.set_vec("nozzle_temperature_initial_layer", vec!["225".into()]);
        let said = apply_filament(&r, &mut c);
        assert_eq!(said.len(), 3);
        assert_eq!(c.get("filament_flow_ratio").unwrap().as_vec(), vec!["0.960", "0.960"]);
        assert_eq!(c.get("nozzle_temperature_initial_layer").unwrap().at(0), "220");
        assert_eq!(c.get("enable_pressure_advance").unwrap().at(0), "1");
        // Nothing measured: nothing changes.
        let mut d = Config::new();
        assert!(apply_filament(&Results::new("p", "f"), &mut d).is_empty());
        assert!(d.map.is_empty());
    }

    #[test]
    fn mech_fit_from_results() {
        let mut r = Results::new("p", "f");
        r.values.insert(names::C_XY.into(), 0.3);
        r.values.insert(names::C_Z_LAYERS.into(), 2.0);
        r.values.insert(names::SNAP_MAX.into(), 0.7);
        r.values.insert(names::SNAP_HOLD.into(), 0.3);
        let m = MechFit::from_results(&r);
        assert_eq!(m.c_xy, Some(0.3));
        assert!((m.c_z(0.2).unwrap() - 0.4).abs() < 1e-12);
        assert_eq!(m.snap_delta(0.9), Some(0.7));
        assert_eq!(m.mu, None);
    }
}

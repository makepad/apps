//! Calibration results into what prints: the filament's slicer settings, and the fits
//! Mechanics sizes joints with.

use crate::results::{names, EffectiveResults, Results};
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
        Self::from_values(|key| r.get(key))
    }

    pub fn from_effective(r: &EffectiveResults) -> Self {
        Self::from_values(|key| r.get(key))
    }

    fn from_values(get: impl Fn(&str) -> Option<f64>) -> Self {
        MechFit {
            c_xy: get(names::C_XY),
            c_z_layers: get(names::C_Z_LAYERS).map(|v| v.round().max(1.0) as u32),
            slip_fit: get(names::SLIP_FIT),
            press_fit: get(names::PRESS_FIT),
            snap_delta_max: get(names::SNAP_MAX),
            snap_delta_hold: get(names::SNAP_HOLD),
            mu: get(names::MU),
        }
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

/// The effective slicer measurements for a tool slot. Bitwise equality is also its slice-key identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SlicerCalibration {
    pub flow_ratio_bits: Option<u64>,
    pub nozzle_temp_bits: Option<u64>,
    pub pressure_advance_bits: Option<u64>,
}

impl SlicerCalibration {
    pub fn from_results(r: &Results) -> Self {
        Self::from_values(r.get(names::FLOW_RATIO), r.get(names::NOZZLE_TEMP), r.get(names::PRESSURE_ADVANCE))
    }

    pub fn from_effective(r: &EffectiveResults) -> Self {
        Self::from_values(r.get(names::FLOW_RATIO), r.get(names::NOZZLE_TEMP), r.get(names::PRESSURE_ADVANCE))
    }

    fn from_values(flow: Option<f64>, temp: Option<f64>, pa: Option<f64>) -> Self {
        let canon_bits = |v: f64| (if v == 0.0 { 0.0 } else { v }).to_bits();
        Self {
            flow_ratio_bits: flow.filter(|v| (0.5..=1.5).contains(v)).map(canon_bits),
            nozzle_temp_bits: temp.filter(|v| (150.0..=350.0).contains(v)).map(canon_bits),
            pressure_advance_bits: pa.filter(|v| (0.0..=0.5).contains(v)).map(canon_bits),
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
    apply_filament_values(r.get(names::FLOW_RATIO), r.get(names::NOZZLE_TEMP), r.get(names::PRESSURE_ADVANCE), values)
}

pub fn apply_filament_effective(r: &EffectiveResults, values: &mut Config) -> Vec<String> {
    apply_filament_values(r.get(names::FLOW_RATIO), r.get(names::NOZZLE_TEMP), r.get(names::PRESSURE_ADVANCE), values)
}

fn apply_filament_values(flow: Option<f64>, temp: Option<f64>, pa: Option<f64>, values: &mut Config) -> Vec<String> {
    let cal = SlicerCalibration::from_values(flow, temp, pa);
    let mut said = Vec::new();
    if let Some(f) = cal.flow_ratio_bits.map(f64::from_bits) {
        set_all(values, "filament_flow_ratio", format!("{f:.3}"));
        said.push(format!("flow {f:.3}"));
    }
    if let Some(t) = cal.nozzle_temp_bits.map(f64::from_bits) {
        // The first layer keeps the preset's offset from the other layers.
        let old = values.get("nozzle_temperature").map(|v| v.at(0).parse::<f64>().unwrap_or(t)).unwrap_or(t);
        let first = values.get("nozzle_temperature_initial_layer").map(|v| v.at(0).parse::<f64>().unwrap_or(old)).unwrap_or(old);
        set_all(values, "nozzle_temperature", format!("{t:.0}"));
        set_all(values, "nozzle_temperature_initial_layer", format!("{:.0}", (t + first - old).clamp(150.0, 350.0)));
        said.push(format!("{t:.0} °C"));
    }
    if let Some(k) = cal.pressure_advance_bits.map(f64::from_bits) {
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
    fn slicer_calibration_ignores_source_owner_and_mech_fits() {
        use crate::results::{EffectiveValue, ValueSource};
        let mut a = EffectiveResults { owner: Some("printer:A".into()), ..Default::default() };
        let mut b = EffectiveResults::default();
        for (r, source, gap) in [(&mut a, ValueSource::Owner, 0.30), (&mut b, ValueSource::Preset, 0.20)] {
            r.values.insert(names::FLOW_RATIO.into(), EffectiveValue { value: 0.96, source });
            r.values.insert(names::C_XY.into(), EffectiveValue { value: gap, source });
        }
        assert_eq!(SlicerCalibration::from_effective(&a), SlicerCalibration::from_effective(&b));
        let mut config = Config::new();
        apply_filament_effective(&a, &mut config);
        assert_eq!(config.get("filament_flow_ratio").unwrap().at(0), "0.960");
        assert_eq!(MechFit::from_effective(&a).c_xy, Some(0.30));
        a.values.get_mut(names::FLOW_RATIO).unwrap().value = 9.0;
        b.values.remove(names::FLOW_RATIO);
        for (r, zero) in [(&mut a, -0.0), (&mut b, 0.0)] {
            r.values.insert(names::PRESSURE_ADVANCE.into(), EffectiveValue { value: zero, source: ValueSource::Owner });
        }
        assert_eq!(SlicerCalibration::from_effective(&a), SlicerCalibration::from_effective(&b), "out-of-range flow is absent and signed zeros agree");
    }

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

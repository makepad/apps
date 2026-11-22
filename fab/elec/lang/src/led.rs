//! Generic LED forward voltages at rated or specified drive current.

use fab_elec_bench::{input::LedColor, ModelLibs};

/// Typical forward voltages at the rated current (20 mA), from the generic model library's curves.
/// Used by `e.led` for `d.vf` when no drive current is given, until a part states its own.
/// Colours are exact lowercase names; None for a colour without a curve.
pub fn forward_volts(color: &str) -> Option<f64> {
    forward_volts_at(color, fab_elec_bench::models::LED_RATED_A)
}

/// The forward voltage of a `color` LED at `amps`, read from the generic model library's curve (the
/// one the bench simulates). None for a colour without a curve, or a current that is not a
/// positive finite number. Colours are exact lowercase names.
pub fn forward_volts_at(color: &str, amps: f64) -> Option<f64> {
    if !amps.is_finite() || amps <= 0.0 || color != color.to_lowercase() {
        return None;
    }
    let color = LedColor::of_name(color)?;
    static LIBS: std::sync::OnceLock<ModelLibs> = std::sync::OnceLock::new();
    LIBS.get_or_init(ModelLibs::generic).led_vf(color, amps)
}

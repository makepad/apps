//! What a print takes: its time on the printer and its resin.
//!
//! The time is the job's layers on the profile: each layer the light off for its delay, its
//! exposure (the bottom layers' longer), and its peel, the plate up the lift's height at the
//! lift's speed and back down to the next layer at the retract speed. The resin is the layers'
//! covered area a layer thick, the parts' and the supports' own apart (a pixel the parts cure
//! is theirs).

use crate::grid::PixelGrid;
use crate::printer::ResinProfile;

/// Seconds layer `k` takes on `p`: the light off, the light on, the peel.
pub fn layer_time(p: &ResinProfile, k: u32) -> f64 {
    let lift = p.lift_of(k);
    p.light_off_delay + p.exposure_of(k) + lift.height / lift.speed + (lift.height - p.layer_height).max(0.0) / p.retract_speed
}

/// Seconds a job of `layers` layers takes on `p`.
pub fn print_time(p: &ResinProfile, layers: u32) -> f64 {
    LayerTimes::of(p).until(layers)
}

/// A job's time per layer: its bottom layers' and the others'. Kept with a job, it says the time
/// left at any layer when the job's profile isn't at hand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerTimes {
    pub bottom_layers: u32,
    /// Seconds per bottom layer, and per layer after them.
    pub bottom: f64,
    pub layer: f64,
}

impl LayerTimes {
    pub fn of(p: &ResinProfile) -> LayerTimes {
        LayerTimes { bottom_layers: p.bottom_layers, bottom: layer_time(p, 0), layer: layer_time(p, p.bottom_layers) }
    }

    /// Seconds the first `layers` layers take.
    pub fn until(&self, layers: u32) -> f64 {
        let bottom = layers.min(self.bottom_layers);
        bottom as f64 * self.bottom + (layers - bottom) as f64 * self.layer
    }
}

/// The resin (ml) of `covered` pixels (each by its share) on `grid`, a layer `height` mm thick.
pub fn resin_ml(covered: f64, grid: &PixelGrid, height: f64) -> f64 {
    covered * grid.pixel_area() * height / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::simulated;

    /// A job's time is its layers' light and peel: the bottom layers' long exposure and slow
    /// lift, then the others'.
    #[test]
    fn a_jobs_time_is_its_layers_light_and_peel() {
        let p = simulated();
        // light off, light on, up, back down to the next layer
        let bottom = 0.5 + 25.0 + 6.0 / 1.0 + (6.0 - 0.05) / 3.0;
        let normal = 0.5 + 2.5 + 5.0 / 2.0 + (5.0 - 0.05) / 3.0;
        assert!((print_time(&p, 100) - (4.0 * bottom + 96.0 * normal)).abs() < 1e-9, "{}", print_time(&p, 100));
        assert!((print_time(&p, 3) - 3.0 * bottom).abs() < 1e-9);
        assert_eq!(print_time(&p, 0), 0.0);
    }
}

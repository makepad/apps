//! The process models the planner plans with and the simulator runs (with its own true
//! figures): the laser-wire bead, the laser cut, the part's heat.

use crate::arm::AssistGas;
use crate::math::V3;

/// A laser-wire bead from the wire it melts. Mass balance gives the cross-section
/// (`efficiency` of the wire volume fed per mm travelled); the melt pool's width grows with the
/// energy per length, `width_k · sqrt(P / v)`; the section is near parabolic, so its height is
/// `1.5 · area / width`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeadModel {
    pub efficiency: f64,
    /// mm per sqrt(W·s/mm).
    pub width_k: f64,
    /// Below this travel speed (mm/s) the pool no longer moves on: the bead is taken at this
    /// speed (a blob grows).
    pub min_speed: f64,
}

impl BeadModel {
    /// Steel wire under a fibre laser (estimates).
    pub const NOMINAL: BeadModel = BeadModel { efficiency: 0.95, width_k: 0.3, min_speed: 0.5 };

    /// The section (mm²) of wire `wire_mm` thick fed at `feed` mm/s, travelling at `speed`.
    pub fn area(&self, wire_mm: f64, feed: f64, speed: f64) -> f64 {
        self.efficiency * 0.25 * std::f64::consts::PI * wire_mm * wire_mm * feed / speed.max(self.min_speed)
    }

    pub fn width(&self, watts: f64, speed: f64) -> f64 {
        self.width_k * (watts.max(0.0) / speed.max(self.min_speed)).sqrt()
    }

    pub fn height(area: f64, width: f64) -> f64 {
        if width > 1e-9 {
            1.5 * area / width
        } else {
            0.0
        }
    }

    /// The power (W) and wire feed (mm/s) for a bead `width` wide and `height` high at `speed`.
    pub fn settings(&self, wire_mm: f64, width: f64, height: f64, speed: f64) -> (f64, f64) {
        let v = speed.max(self.min_speed);
        let watts = (width / self.width_k).powi(2) * v;
        let area = height * width / 1.5;
        let feed = area * v / (self.efficiency * 0.25 * std::f64::consts::PI * wire_mm * wire_mm);
        (watts, feed)
    }
}

/// What a laser cut leaves and takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CutModel {
    /// A sliver cut free along its inner face falls away when it is at most this thick (mm).
    pub offcut: f64,
    /// The recast (heat-affected) layer left on the cut face (mm), by assist gas.
    pub recast_n2: f64,
    pub recast_o2: f64,
    /// Dross blown along the wall below per mm of cut depth (mm of height), by gas.
    pub dross_n2: f64,
    pub dross_o2: f64,
}

impl CutModel {
    /// Thin steel (estimates).
    pub const NOMINAL: CutModel = CutModel { offcut: 1.0, recast_n2: 0.015, recast_o2: 0.04, dross_n2: 0.005, dross_o2: 0.02 };

    pub fn recast(&self, gas: AssistGas) -> f64 {
        match gas {
            AssistGas::Nitrogen => self.recast_n2,
            AssistGas::Oxygen => self.recast_o2,
        }
    }

    pub fn dross(&self, gas: AssistGas) -> f64 {
        match gas {
            AssistGas::Nitrogen => self.dross_n2,
            AssistGas::Oxygen => self.dross_o2,
        }
    }
}

/// The part's heat, and the warp it makes. The part and its plate as one lump: the lasers'
/// absorbed power in, cooling to ambient with a time constant. Over that, the work zone: hotter
/// than the rest by an excess that follows the absorbed power with a lag and fades with the
/// distance from the height the heat goes in at. Each slice of the part grows in its plane
/// about the part frame's axis (where the fixture holds it) with its own temperature, and the
/// part grows up by its slices' growth below: a hot top flares out further than the base, a
/// warp and not only a growth. The plate is the datum (it does not move). Thermo-elastic only:
/// cooled, the part takes its shape back (no residual weld distortion).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thermal {
    /// The share of the deposition laser's power that ends up in the part.
    pub absorb: f64,
    /// The share of the cutting beam's that does, and the cutting beam's power (W).
    pub absorb_cut: f64,
    pub cut_power: f64,
    /// The share of an ablation source's power that does (most of it: ablation's specific
    /// energy is several times what vaporising takes; the plume carries the rest).
    pub absorb_ablate: f64,
    /// The plate's heat capacity (J/K) and the deposit's per mm³ (steel: 3.9e-3).
    pub base_capacity: f64,
    pub capacity_per_mm3: f64,
    /// Cooling time constant (s).
    pub cooling: f64,
    /// Linear expansion (1/K).
    pub expansion: f64,
    /// The work zone: its steady excess over the rest per watt absorbed (K/W), how fast it
    /// follows the power (s), and how far from the heat it reaches (mm, to 1/e).
    pub excess: f64,
    pub excess_time: f64,
    pub reach: f64,
}

/// Where a part's heat stands: its mean rise over ambient (K), the work zone's excess over
/// that (K), and the height the heat last went in at (mm, part frame).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Heat {
    pub bulk: f64,
    pub excess: f64,
    pub z: f64,
}

impl Thermal {
    /// A steel part on a small steel plate (estimates): 0.4 K/W puts a 1.5 kW weld's top about
    /// 200 K over the rest (interpass temperatures in wire DED run 100-400 °C over the base),
    /// following in 3 s, reaching 3 mm down (steel's diffusivity, about 4 mm²/s, over that time).
    pub const NOMINAL: Thermal = Thermal {
        absorb: 0.35,
        absorb_cut: 0.1,
        cut_power: 1000.0,
        absorb_ablate: 0.7,
        base_capacity: 50.0,
        capacity_per_mm3: 3.9e-3,
        cooling: 60.0,
        expansion: 12e-6,
        excess: 0.4,
        excess_time: 3.0,
        reach: 3.0,
    };

    /// The heat after `dt` s with `watts` absorbed at height `z`, from `h`, at heat capacity
    /// `capacity` (J/K).
    pub fn step(&self, h: Heat, watts: f64, z: f64, capacity: f64, dt: f64) -> Heat {
        let bulk = (h.bulk + watts * dt / capacity.max(1e-9)) * (-dt / self.cooling).exp();
        let steady = self.excess * watts;
        let excess = steady + (h.excess - steady) * (-dt / self.excess_time.max(1e-9)).exp();
        Heat { bulk, excess, z: if watts > 0.0 { z } else { h.z } }
    }

    /// The rise at height `z` of the part (K).
    pub fn rise(&self, h: &Heat, z: f64) -> f64 {
        h.bulk + h.excess * (-(z - h.z).abs() / self.reach).exp()
    }

    /// How much bigger the slice at height `z` is (a factor about the part frame's axis).
    pub fn growth(&self, h: &Heat, z: f64) -> f64 {
        1.0 + self.expansion * self.rise(h, z)
    }

    /// How far the slice at height `z` has risen (mm): the growth of the part below it.
    fn lift(&self, h: &Heat, z: f64) -> f64 {
        let (d, zh) = (self.reach, h.z.max(0.0));
        let below = |z: f64| d * ((-(zh - z) / d).exp() - (-zh / d).exp());
        let zone = if z <= zh { below(z) } else { below(zh) + d * (1.0 - (-(z - zh) / d).exp()) };
        self.expansion * (h.bulk * z + h.excess * zone)
    }

    /// Where the material at `p` (part frame, as it stands cold) stands with the part at `h`.
    pub fn warp(&self, h: &Heat, p: V3) -> V3 {
        if h.bulk == 0.0 && h.excess == 0.0 {
            return p;
        }
        let g = self.growth(h, p[2]);
        [p[0] * g, p[1] * g, p[2] + self.lift(h, p[2])]
    }

    /// The material (as it stands cold) at `p` with the part at `h`: [`Thermal::warp`]'s
    /// inverse.
    pub fn cold(&self, h: &Heat, p: V3) -> V3 {
        if h.bulk == 0.0 && h.excess == 0.0 {
            return p;
        }
        // The lift is a few µm per mm: four steps settle it far below a nanometre.
        let mut z = p[2];
        for _ in 0..4 {
            z = p[2] - self.lift(h, z);
        }
        let g = self.growth(h, z);
        [p[0] / g, p[1] / g, z]
    }
}

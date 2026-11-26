//! The Stratocumulus as a cloud volume (design §2.2's table, §4.3): a deck in a thin shell, one of the two looks the
//! owner kept from the look runner on 2026-11-05 (cloud_volume_deck.rs holds the deck, cloud_gpu's export/deck.rs
//! draws it):
//!
//! - **Billows under 0.5: the turbulence deck** (runner round 50, the low-sun look beside the real sky 103): a smooth
//!   connected deck with cracks at every scale, thin cream veil in the gaps, the cells drawn out along the wind by
//!   Rolls, opening toward the sun. Its cells 2.5 cycles a Size (1 a km at the default 2.5 km), the masses 0.75 a
//!   Size, 600 m deep at its look (Tops 1800).
//! - **Billows 0.5 and over: the cell deck** (round 22, the high-sun look beside sky 072): rounded Worley lumps with
//!   dark crevices, grey undersides and bright rims by the gaps. Its cells 7.5 cycles a Size (3 a km), the masses
//!   0.875 a Size, 300 m deep at its look (Tops 1500); Rolls over 0.3 draw its cells out along the wind.
//!
//! Density scales the extinction (0.08 /m at 1, the runner's). Softness and Shading are not read by these decks, and
//! the species' turrets (castellanus) and lens cells (lenticularis) of the older deck are not drawn: a species sets
//! Billows and Rolls, which pick the deck and its rolls.

use super::*;

/// The recipe's id in the footprint grid's key.
const SC_RECIPE: f32 = 7.0;
/// The looks' extinction a metre at Density 1 (the runner's density).
const SC_SIGMA: f32 = 0.08;

/// The volume of a Stratocumulus `spec` (design §2.2), None at no cover or no density.
pub(super) fn build(spec: &VolumeSpec, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Option<VolumeShape>, Cancelled> {
    let b = spec.layer.stratocumulus_params();
    let rolls = b.rolls.clamp(0.0, 1.0);
    let look = if b.billows < 0.5 {
        deck::DeckLook {
            kind: deck::DeckKind::Turbulence,
            cells_per_km: 2.5 / b.size_km,
            mass_per_km: 0.75 / b.size_km,
            mass: 0.3,
            cut_edge: 0.025,
            ramp: 0.1,
            squash: 0.5,
            profile: 0.25,
            base_rise: 0.12,
            top_fall: 0.35,
            sun_rise: 0.1,
            sun_km: 6.0,
            veil: 0.15,
            veil_band: 0.07,
            veil_per_km: 3.0,
            veil_share: 0.6,
            water_base: 0.2,
            water_full: 0.6,
            stretch: 1.0 + 1.6667 * rolls,
            texture: 0.6,
            far_from_m: 12_000.0,
            far_to_m: 35_000.0,
            fold: 2.0,
            erosion: 0.0,
            inside: 0.0,
            lift: 0.0,
            sigma: SC_SIGMA,
            forward_g: 0.6,
        }
    } else {
        deck::DeckLook {
            kind: deck::DeckKind::Cells,
            cells_per_km: 7.5 / b.size_km,
            mass_per_km: 0.875 / b.size_km,
            mass: 0.35,
            cut_edge: 0.0,
            ramp: 0.1,
            squash: 1.0,
            profile: 0.45,
            base_rise: 0.06,
            top_fall: 0.45,
            sun_rise: 0.0,
            sun_km: 6.0,
            veil: 0.0,
            veil_band: 0.0,
            veil_per_km: 3.0,
            veil_share: 0.6,
            water_base: 0.0,
            water_full: 0.6,
            stretch: 1.0 + 2.0 * (rolls - 0.3).max(0.0),
            texture: 0.0,
            far_from_m: 12_000.0,
            far_to_m: 35_000.0,
            fold: 2.0,
            erosion: 0.6,
            inside: 0.25,
            lift: 0.25,
            sigma: SC_SIGMA,
            forward_g: 0.75,
        }
    };
    deck::build(spec, look, b.density, b.base_m, b.tops_m, b.size_km, SC_RECIPE, cancelled, run)
}

//! Fab's calibration: one place for every measurement that needs a test print (and so the
//! user's explicit Print OK) or a hand measurement.
//!
//! - `coupon`: the test part of each step, made for the printer's nozzle, layer and filament.
//! - `results`: what the user found, per printer and filament, in a small text file under
//!   `~/Documents/Fab/calibration/`; turning an entered answer into the stored values.
//! - `apply`: the stored values into the slicer's filament settings (flow ratio, temperature,
//!   pressure advance) and into Mechanics' fits (clearances, snap grip, friction).
//! - `mesh`: the solids the test parts are made of.
//!
//! Nothing here prints or talks to a printer: a coupon becomes a plate the app slices and
//! previews, and only the app's own Print button (with its confirm) sends it.

pub mod apply;
pub mod coupon;
pub mod mesh;
pub mod results;

pub use apply::MechFit;
pub use coupon::{make, Band, Coupon, CouponSetup, Variant};
pub use results::{Answer, Results};

/// The calibration steps in the order the guide offers them: fits and friction for
/// Mechanics first, then the slicer's flow, temperature and pressure advance, then the head
/// measurement non-planar printing needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    /// Print-in-place clearance in the plane (spinners).
    Clearance,
    /// Print-in-place clearance across layers (flaps).
    ZGap,
    /// Slip and press fits of parts printed apart (hole gauge and peg).
    SlipFit,
    /// Snap-fit grip (cantilever hooks).
    Snap,
    /// Joint friction (weighted arm on a spinner).
    Friction,
    /// Filament flow ratio (staircase).
    Flow,
    /// Nozzle temperature (tower).
    Temperature,
    /// Pressure advance (square tube).
    PressureAdvance,
    /// The print head's shape, measured by hand (no print).
    HeadEnvelope,
}

impl Step {
    pub const ALL: [Step; 9] = [Step::Clearance, Step::ZGap, Step::SlipFit, Step::Snap, Step::Friction, Step::Flow, Step::Temperature, Step::PressureAdvance, Step::HeadEnvelope];

    pub fn title(self) -> &'static str {
        match self {
            Step::Clearance => "Clearance",
            Step::ZGap => "Layer gap",
            Step::SlipFit => "Slip and press fit",
            Step::Snap => "Snap fit",
            Step::Friction => "Joint friction",
            Step::Flow => "Flow",
            Step::Temperature => "Temperature",
            Step::PressureAdvance => "Pressure advance",
            Step::HeadEnvelope => "Print head shape",
        }
    }

    /// What the result feeds.
    pub fn feeds(self) -> &'static str {
        match self {
            Step::Clearance | Step::ZGap => "Mechanics: gaps of joints printed in place",
            Step::SlipFit | Step::Snap => "Mechanics: parts printed apart and assembled",
            Step::Friction => "Mechanics: how joints hold and swing in Simulate",
            Step::Flow | Step::Temperature | Step::PressureAdvance => "The slicer: this filament's settings",
            Step::HeadEnvelope => "Non-planar tops: the collision check",
        }
    }

    /// The key the step's file entries use.
    pub fn key(self) -> &'static str {
        match self {
            Step::Clearance => "clearance",
            Step::ZGap => "z_gap",
            Step::SlipFit => "slip_fit",
            Step::Snap => "snap",
            Step::Friction => "friction",
            Step::Flow => "flow",
            Step::Temperature => "temperature",
            Step::PressureAdvance => "pressure_advance",
            Step::HeadEnvelope => "head_envelope",
        }
    }

    /// Results depend on the filament (all but the head).
    pub fn per_filament(self) -> bool {
        self != Step::HeadEnvelope
    }
}

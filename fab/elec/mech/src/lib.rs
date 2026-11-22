//! Fab electronics mechanics (contract C8): board and assembly mechanical summaries for cases,
//! fixtures, fit checks and slicer inserts, and the panel-part generators bound to the design
//! (light pipes, keycaps, windows, knobs, jack rings). Owned by EMECH and ETOOL.

pub mod align;
pub mod align_answer;
pub mod csg;
pub mod derived;
pub mod draft;
pub mod generate;
pub mod scene;
pub mod mesh;
pub mod package;
pub mod params;
pub mod light_pipe;
pub mod display_window;
pub mod lower;
pub mod knob;
pub mod jack_ring;
pub mod board_mech;
pub mod mass;
pub mod bridge;
pub mod checks;
pub mod hull;
pub mod keys;
pub mod read;
pub mod motion;
pub mod assembly;
pub use board_mech::{board_mech, board_mech_with, BodyBox, BoardMech, MechHole, PlacedPort};
pub use mass::{board_mass, BoardMass};
pub use checks::{case_checks, CaseCheck, CaseFinding, CaseSolid, CheckOpts, Level};
pub use bridge::{board_base, case_binding, case_pipes, case_poses, case_solids, CaseBinding, CasePose, CaseRole};
pub use motion::{board_sensors, motion_mounts, MotionMount};
pub use assembly::{assembly_plan, AssemblyPlan, AssemblyStep, StepKind};

use fab_elec_core::Xform3;

/// The moving state of an assembly (C8): each mount's parent frame resolved. The default is a
/// static assembly, where every frame follows from the design alone; a mechanism supplies the
/// world poses of the bodies that mounts sit on (`Frame::Body`).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Pose {
    /// World poses of mechanism bodies (nm), by mechanism and body (the mechanics port's stable
    /// names). A body not listed is at the identity.
    pub bodies: Vec<(String, String, Xform3)>,
}

impl Pose {
    /// The world pose of `body` of `mechanism`.
    pub fn body(&self, mechanism: &str, body: &str) -> Xform3 {
        self.bodies.iter().find(|(m, b, _)| m == mechanism && b == body).map_or(Xform3::IDENTITY, |(_, _, x)| *x)
    }
}

//! Where a board's motion sensors sit, for Mechanics to probe them (Q11): a sensor's axes in its
//! footprint (its part's `motion.x` and `motion.z` fields), the footprint on the board, and the
//! board in the case part it is fixed to. Nothing here is one sensor's: any part that states the
//! two fields is a motion sensor.
//!
//! Cost: one pass over a board's placed parts, then `case_poses` for a board with a sensor, on
//! the pool when a plate is resolved, never per frame.

use crate::align::invert;
use crate::bridge::{bare_board, board_base, case_poses};
use crate::scene::placement_pose;
use fab_elec_board::Board;
use fab_elec_circuit::{Circuit, Component};
use fab_elec_core::{to_mm, BoardId, CompId, CompRef};
use fab_elec_design::Design;
use fab_model::eval::Evaluated;

/// A motion sensor on its board, in the board's frame (mm, z up from the board's bottom face): its
/// sampling point (the footprint's origin on the face the part sits on) and its X, Y and Z
/// directions.
#[derive(Clone, Debug, PartialEq)]
pub struct BoardSensor {
    pub comp: CompId,
    pub at: [f64; 3],
    pub axes: [[f64; 3]; 3],
}

/// A motion sensor where it sits in its board's closed case, as printed: the case part the board
/// is fixed to (its placed index, `Evaluated::placed_part`), and the sensor's sampling point and X,
/// Y and Z directions in that part's plate coordinates (mm). This is what a Mechanics probe on
/// that part's body takes.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionMount {
    pub comp: CompRef,
    pub part: usize,
    pub at: [f64; 3],
    pub axes: [[f64; 3]; 3],
}

/// One signed footprint axis: `+x`, `-x`, `+y`, `-y`, `+z` or `-z` (z out of the part's top).
fn axis(text: &str) -> Option<[f64; 3]> {
    let (sign, name) = match text.trim().split_at_checked(1)? {
        ("+", name) => (1.0, name),
        ("-", name) => (-1.0, name),
        _ => return None,
    };
    let mut v = [0.0; 3];
    v[match name { "x" => 0, "y" => 1, "z" => 2, _ => return None }] = sign;
    Some(v)
}

/// A motion sensor's X, Y and Z directions in its footprint's frame, from its `motion.x` and
/// `motion.z` fields (the component's over its part's), each a signed footprint axis. Y is Z x X:
/// the sensor's frame is right-handed. None for a part with neither field: not a motion sensor.
pub fn sensor_axes(comp: &Component) -> Option<Result<[[f64; 3]; 3], String>> {
    let field = |key: &str| comp.fields.get(key).or_else(|| comp.part.fields.get(key));
    let (x, z) = match (field("motion.x"), field("motion.z")) {
        (None, None) => return None,
        (Some(x), Some(z)) => (x, z),
        _ => return Some(Err("motion.x and motion.z come together".into())),
    };
    let (Some(xa), Some(za)) = (axis(x), axis(z)) else {
        return Some(Err(format!("motion.x '{x}' and motion.z '{z}' must each be +x, -x, +y, -y, +z or -z")));
    };
    if xa.iter().zip(&za).any(|(a, b)| a * b != 0.0) {
        return Some(Err(format!("motion.x '{x}' and motion.z '{z}' are not perpendicular")));
    }
    let y = [za[1] * xa[2] - za[2] * xa[1], za[2] * xa[0] - za[0] * xa[2], za[0] * xa[1] - za[1] * xa[0]];
    Some(Ok([xa, y, za]))
}

/// Every motion sensor placed on `board`, in the board's frame. A part whose fields are malformed
/// is said through `diag` and left out. O(placed parts).
pub fn board_sensors(board: &Board, circuit: &Circuit, diag: &mut impl FnMut(String)) -> Vec<BoardSensor> {
    board.placed.iter().filter_map(|placed| {
        let comp = &circuit.comps[placed.comp.ix()];
        let axes = match sensor_axes(comp)? {
            Ok(axes) => axes,
            Err(why) => {
                diag(format!("{}: {why}; its motion is not probed", comp.refdes));
                return None;
            }
        };
        let pose = placement_pose(placed, board.stackup.thickness);
        Some(BoardSensor { comp: placed.comp, at: pose.apply([0; 3]).map(to_mm), axes: axes.map(|a| pose.rotate(a)) })
    }).collect()
}

/// `sensor`, on board `unit`, carried into the plate frame of placed part `part`, which the board
/// is fixed to and whose plate coordinates map to the board's by `to_board` (`bridge::CasePose::to_board`).
/// None when `to_board` cannot be inverted.
pub fn mounted(sensor: &BoardSensor, unit: BoardId, part: usize, to_board: &[[f64; 4]; 3]) -> Option<MotionMount> {
    let m = invert(to_board)?;
    let turn = |v: [f64; 3]| -> [f64; 3] { std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2]) };
    let at = turn(sensor.at);
    Some(MotionMount {
        comp: CompRef { board: unit, comp: sensor.comp },
        part,
        at: std::array::from_fn(|r| at[r] + m[r][3]),
        axes: sensor.axes.map(turn),
    })
}

/// Every motion sensor on a board of `design` that sits in a closed case in `ev`, fixed to its
/// case's base ([`board_base`]), or on a board printed bare, fixed to the board itself
/// ([`bare_board`]). A board neither in a case nor printed has no body to probe and gives none.
/// O(placed parts) per board, and O(placed parts + objects) for a board with a sensor.
pub fn motion_mounts(design: &Design, ev: &Evaluated, diag: &mut impl FnMut(String)) -> Vec<MotionMount> {
    let mut mounts = Vec::new();
    for unit in &design.units {
        let Some(board) = unit.board.as_deref() else { continue };
        let sensors = board_sensors(board, &unit.circuit, diag);
        if sensors.is_empty() { continue; }
        let poses = case_poses(ev, &unit.name);
        let carrier = board_base(&poses).map(|base| (base.part, base.to_board)).or_else(|| bare_board(ev, &unit.name));
        let Some((part, to_board)) = carrier else { continue };
        for sensor in &sensors {
            match mounted(sensor, unit.id, part, &to_board) {
                Some(mount) => mounts.push(mount),
                None => diag(format!("{}: the frame of the part it is fixed to is degenerate; its motion is not probed", unit.name)),
            }
        }
    }
    mounts
}

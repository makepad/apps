//! Plastic on an arm: the layers a printer slicer planned (walls and fills with their widths,
//! heights, flows and speeds), printed one task per layer by an arm with an extruder. The
//! plan is the slicer's; what the arm adds is how it moves: its own acceleration, jerk and
//! corner limits time every path, its extruder's melt flow caps the speed, travels longer than
//! a hop lift over the part, and a layer printed faster than it cools is slowed down to its
//! shortest time (to the slowest speed allowed, never waiting), as the printer slicer does.

use crate::path::{Motion, Profile, Waypoint};
use crate::route::{Ctx, Route, Task, TaskKind};
use fab_robot::arm::*;
use fab_robot::math::*;

/// One extrusion path (part frame): its points (z per point; a planar path has the layer's),
/// the bead's width and height, the slicer's speed (mm/s) and per-segment flow multipliers on
/// its cross-section (segment i ends at `pts[i + 1]`; empty = 1 everywhere).
#[derive(Clone, Debug, PartialEq)]
pub struct FdmPath {
    pub pts: Vec<V3>,
    pub width: f64,
    pub height: f64,
    pub speed: f64,
    pub flow: Vec<f64>,
}

impl FdmPath {
    /// The bead's cross-section on segment `i` (mm²).
    pub fn area(&self, i: usize) -> f64 {
        bead_area(self.width, self.height) * self.flow.get(i).copied().unwrap_or(1.0)
    }
}

/// One layer: its print height (the nozzle's z), its thickness, its paths in print order.
#[derive(Clone, Debug, PartialEq)]
pub struct FdmLayer {
    pub z: f64,
    pub height: f64,
    pub paths: Vec<FdmPath>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FdmOpts {
    /// Travel speed (mm/s), and the arm's fastest TCP speed printing (mm/s).
    pub travel: f64,
    pub speed_max: f64,
    /// Travels longer than `hop_after` lift `hop` mm over the part (mm).
    pub hop: f64,
    pub hop_after: f64,
    /// The approach to a layer's first path and the retract after its last (mm, straight up).
    pub approach: f64,
    /// The extruder's melt flow ceiling (mm³/s).
    pub flow_max: f64,
    /// A layer's shortest time (s) and the slowest a path may be slowed to for it (mm/s).
    pub min_layer_time: f64,
    pub min_speed: f64,
}

/// The planar print route: the layers as planned, bottom up.
pub struct Fdm {
    pub layers: Vec<FdmLayer>,
    pub opts: FdmOpts,
    next: usize,
}

impl Fdm {
    pub fn new(layers: Vec<FdmLayer>, opts: FdmOpts) -> Fdm {
        Fdm { layers, opts, next: 0 }
    }

    /// Layer `k`'s task path and beads, its extrusions at `cap` mm/s at most.
    fn path(&self, k: usize, cap: f64) -> (Vec<Waypoint>, Vec<(f64, f64)>) {
        let o = &self.opts;
        let down = [0.0, 0.0, -1.0];
        let layer = &self.layers[k];
        let mut path: Vec<Waypoint> = Vec::new();
        let mut bead = Vec::new();
        let off = |path: &mut Vec<Waypoint>, bead: &mut Vec<(f64, f64)>, p: V3| {
            path.push(Waypoint { p, z: down, x: None, speed: o.travel, on: false });
            bead.push((0.0, 0.0));
        };
        for fp in layer.paths.iter().filter(|fp| fp.pts.len() >= 2) {
            let start = fp.pts[0];
            match path.last().map(|w| w.p) {
                None => off(&mut path, &mut bead, add(start, [0.0, 0.0, o.approach])),
                Some(at) if dist(at, start) > o.hop_after => {
                    let top = at[2].max(start[2]) + o.hop;
                    off(&mut path, &mut bead, [at[0], at[1], top]);
                    off(&mut path, &mut bead, [start[0], start[1], top]);
                }
                Some(_) => {}
            }
            off(&mut path, &mut bead, start);
            for (i, p) in fp.pts.iter().enumerate().skip(1) {
                let area = fp.area(i - 1);
                let speed = fp.speed.min(o.speed_max).min(cap).min(o.flow_max / area.max(1e-9));
                path.push(Waypoint { p: *p, z: down, x: None, speed, on: true });
                bead.push((bead_width(area, fp.height), fp.height));
            }
        }
        if let Some(last) = path.last().map(|w| w.p) {
            off(&mut path, &mut bead, add(last, [0.0, 0.0, o.approach]));
        }
        (path, bead)
    }

    /// Layer `k` as it prints under `motion` (None when it has nothing to print): a layer
    /// quicker than its shortest time has its extrusions capped at the speed that makes it last
    /// that long (no slower than the slowest allowed), for cooling.
    fn layer(&self, k: usize, motion: &Motion) -> Option<(Vec<Waypoint>, Vec<(f64, f64)>)> {
        let motion = Motion { rest: false, ..*motion };
        let (path, bead) = self.path(k, f64::MAX);
        if path.len() < 2 {
            return None;
        }
        let o = self.opts;
        if Profile::new(&path, &motion).duration() >= o.min_layer_time {
            return Some((path, bead));
        }
        let fastest = path.iter().filter(|w| w.on).map(|w| w.speed).fold(0.0f64, f64::max);
        let (mut lo, mut hi) = (o.min_speed.min(fastest), fastest);
        for _ in 0..24 {
            let mid = 0.5 * (lo + hi);
            if Profile::new(&self.path(k, mid).0, &motion).duration() < o.min_layer_time {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        Some(self.path(k, lo))
    }

    /// Every layer's path time under `motion`, cooling slowdowns included (s): the print
    /// without the moves between layers, for weighing what limits it.
    pub fn time(&self, motion: &Motion) -> f64 {
        let m = Motion { rest: false, ..*motion };
        (0..self.layers.len()).filter_map(|k| self.layer(k, motion)).map(|(path, _)| Profile::new(&path, &m).duration()).sum()
    }
}

impl Route for Fdm {
    fn next(&mut self, ctx: &Ctx) -> Option<Task> {
        let (arm, head) = ctx.arms.iter().enumerate().find_map(|(i, a)| a.heads.iter().position(|t| matches!(t.kind, ToolKind::Extruder(_))).map(|h| (ArmId(i as u8), h)))?;
        while self.next < self.layers.len() {
            let k = self.next;
            self.next += 1;
            let Some((path, bead)) = self.layer(k, &ctx.motion) else { continue };
            let layer = &self.layers[k];
            return Some(Task { arm, head, kind: TaskKind::Deposit { layer: k, top: layer.z }, path, bead, hold: false, table: ctx.level(), turns: None });
        }
        None
    }

    fn label(&self) -> &'static str {
        "Planar plastic layers"
    }
}

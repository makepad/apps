//! What a slice prints inside a region: the read of a print settings volume, walked over the plan's
//! own paths, so it says what's printed and not what was asked for. Run this on a worker.

use makepad_fab_toolpath::role::Role;
use makepad_fab_toolpath::{par, ExtrusionPath, ObjectLayer};

/// What the paths print inside a region, in one plan.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Printed {
    /// The lowest and highest layer (top z, mm) with anything inside; none: both 0.
    pub from_z: f64,
    pub to_z: f64,
    /// How many layers have anything inside.
    pub layers: usize,
    /// Extruded mm3 inside: walls, sparse infill, and every other fill.
    pub walls_mm3: f64,
    pub sparse_mm3: f64,
    pub solid_mm3: f64,
    /// Seconds of extruding inside, at the paths' planned speeds.
    pub seconds: f64,
}

impl Printed {
    pub fn mm3(&self) -> f64 {
        self.walls_mm3 + self.sparse_mm3 + self.solid_mm3
    }

    pub fn add(&mut self, o: &Printed) {
        if o.layers > 0 {
            self.from_z = if self.layers == 0 { o.from_z } else { self.from_z.min(o.from_z) };
            self.to_z = self.to_z.max(o.to_z);
        }
        self.layers += o.layers;
        self.walls_mm3 += o.walls_mm3;
        self.sparse_mm3 += o.sparse_mm3;
        self.solid_mm3 += o.solid_mm3;
        self.seconds += o.seconds;
    }
}

/// One path's segments with their middles inside.
fn path_inside(p: &ExtrusionPath, z: f64, inside: &(dyn Fn([f64; 3]) -> bool + Sync), out: &mut Printed) {
    let bucket = match p.role {
        Role::OuterWall | Role::InnerWall | Role::OverhangWall => 0,
        Role::SparseInfill => 1,
        _ => 2,
    };
    for (i, w) in p.pts.windows(2).enumerate() {
        if p.dry.get(i).copied().unwrap_or(false) {
            continue;
        }
        let (a, b) = ((w[0].x_mm(), w[0].y_mm()), (w[1].x_mm(), w[1].y_mm()));
        let dz = p.z.get(i + 1).copied().unwrap_or(0.0) - p.z.get(i).copied().unwrap_or(0.0);
        if !inside([0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1), z - 0.5 * p.height]) {
            continue;
        }
        let length = (b.0 - a.0).hypot(b.1 - a.1).hypot(dz);
        let mm3 = length * p.area * p.flow.get(i).copied().unwrap_or(1.0);
        match bucket {
            0 => out.walls_mm3 += mm3,
            1 => out.sparse_mm3 += mm3,
            _ => out.solid_mm3 += mm3,
        }
        if p.speed > 0.0 {
            out.seconds += length / p.speed;
        }
    }
}

/// What `layers` (a plan's: top z, height, layer) print inside `inside`, a test in the plan's frame.
/// `None` when stopped.
pub fn printed_inside(layers: &[(f64, f64, ObjectLayer)], inside: &(dyn Fn([f64; 3]) -> bool + Sync), stop: &(dyn Fn() -> bool + Sync)) -> Option<Printed> {
    let per = par::try_map(par::Ctl { cancel: Some(stop), ..Default::default() }, layers.len(), |i| {
        let (z, _, layer) = &layers[i];
        let mut out = Printed::default();
        if layer.support_only {
            return out;
        }
        for island in &layer.islands {
            for p in island.walls.iter().chain(&island.fills) {
                path_inside(p, *z, inside, &mut out);
            }
        }
        if out.mm3() > 0.0 {
            out.layers = 1;
            (out.from_z, out.to_z) = (*z, *z);
        }
        out
    }).ok()?;
    // a painted layer has one object layer per filament: one layer, however many
    let mut total = Printed::default();
    let mut last = None;
    for (i, p) in per.iter().enumerate() {
        let mut one = *p;
        if one.layers > 0 && last == Some(layers[i].0) {
            one.layers = 0;
        }
        if one.layers > 0 {
            last = Some(layers[i].0);
        }
        total.add(&one);
    }
    Some(total)
}

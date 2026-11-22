//! Parts for the coral tests, as their slices: the prototype's six scenes (a flat shelf, a bar,
//! three shelves, a tower, five pads, a cap round a stem), and others built the same way.

use crate::par::Ctl;
use crate::support::SupportParams;
use crate::support_rules::Legal;
use makepad_fab_slicer::clip::{difference_ex, union_ex};
use makepad_fab_slicer::geom::*;

/// The prototype's layer height and line width (mm).
pub const LH: f64 = 0.2;
pub const LW: f64 = 0.45;

/// A solid of a scene, over `z0..z1` (mm).
#[derive(Clone, Copy, Debug)]
pub enum Solid {
    /// Centred at (cx, cy), w by d.
    Box { cx: f64, cy: f64, w: f64, d: f64, z0: f64, z1: f64 },
    /// Centred at (cx, cy), radius r.
    Cyl { cx: f64, cy: f64, r: f64, z0: f64, z1: f64 },
    /// A box taken out of what the solids before it make.
    Cut { cx: f64, cy: f64, w: f64, d: f64, z0: f64, z1: f64 },
}

pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
    ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
}

pub fn disc(cx: f64, cy: f64, r: f64) -> ExPolygon {
    let n = 128;
    ExPolygon::new(Polygon::new((0..n).map(|i| {
        let a = std::f64::consts::TAU * i as f64 / n as f64;
        Pt::from_mm(cx + r * a.cos(), cy + r * a.sin())
    }).collect()))
}

/// A part's slices at 0.2 mm layers, its heights, its paint blockers, and the settings its
/// supports keep: the prototype's (45 degrees, 2 mm off the part, 0.2 mm under it, from the bed
/// only).
pub struct Scene {
    pub slices: Vec<Vec<ExPolygon>>,
    pub heights: Vec<(f64, f64)>,
    pub p: SupportParams,
    pub blocks: Vec<Vec<ExPolygon>>,
}

impl Scene {
    pub fn of(solids: &[Solid]) -> Scene {
        let top = solids.iter().map(|s| match *s { Solid::Box { z1, .. } | Solid::Cyl { z1, .. } | Solid::Cut { z1, .. } => z1 }).fold(0.0, f64::max);
        let n = (top / LH).round() as usize;
        let slices = (0..n).map(|l| {
            let z = LH * (l as f64 + 0.5);
            let mut set: Vec<ExPolygon> = Vec::new();
            for s in solids {
                match *s {
                    Solid::Box { cx, cy, w, d, z0, z1 } if z > z0 && z < z1 => set = union_ex(&[set, vec![rect(cx - w / 2.0, cy - d / 2.0, cx + w / 2.0, cy + d / 2.0)]].concat()),
                    Solid::Cyl { cx, cy, r, z0, z1 } if z > z0 && z < z1 => set = union_ex(&[set, vec![disc(cx, cy, r)]].concat()),
                    Solid::Cut { cx, cy, w, d, z0, z1 } if z > z0 && z < z1 && !set.is_empty() => set = difference_ex(&set, &[rect(cx - w / 2.0, cy - d / 2.0, cx + w / 2.0, cy + d / 2.0)]),
                    _ => {}
                }
            }
            set
        }).collect();
        let p = SupportParams { threshold_deg: 45.0, xy_gap: 2.0, top_gap: 0.2, bottom_gap: 0.2, interface_top_layers: 0, line_width: LW, wall_width: LW, tip_diameter: 0.8, branch_diameter: 5.0, taper_deg: 5.0, angle_max_deg: 45.0, angle_slow_deg: 25.0, build_plate_only: true, remove_small: true, first_expansion: 2.0, stand_inside: false };
        Scene { slices, heights: (0..n).map(|l| (LH * (l + 1) as f64, LH)).collect(), p, blocks: Vec::new() }
    }

    /// One of the prototype's six scenes: shelf, bar, branches, tower, pads, mushroom.
    pub fn named(name: &str) -> Scene {
        let slab = |cx: f64, cy: f64, w: f64, d: f64, h: f64| Solid::Box { cx, cy, w, d, z0: h, z1: h + 3.0 };
        match name {
            "shelf" => Scene::of(&[slab(0.0, 0.0, 30.0, 18.0, 36.0)]),
            "bar" => Scene::of(&[slab(0.0, 0.0, 43.0, 5.0, 40.0)]),
            "branches" => Scene::of(&[slab(-11.0, 3.0, 11.0, 8.0, 32.0), slab(11.0, 6.0, 10.0, 9.0, 38.0), slab(2.0, -10.0, 14.0, 6.0, 30.0)]),
            "tower" => Scene::of(&[Solid::Cyl { cx: 0.0, cy: 0.0, r: 13.0, z0: 42.0, z1: 45.0 }]),
            "pads" => Scene::of(&[slab(-14.0, 7.0, 5.0, 5.0, 34.0), slab(-9.0, -12.0, 5.0, 5.0, 30.0), slab(3.0, 15.0, 5.0, 5.0, 38.0), slab(14.0, -6.0, 5.0, 5.0, 32.0), slab(15.0, 9.0, 5.0, 5.0, 40.0)]),
            "mushroom" => Scene::of(&[Solid::Cyl { cx: 0.0, cy: 0.0, r: 4.0, z0: 0.0, z1: 46.0 }, Solid::Cyl { cx: 0.0, cy: 0.0, r: 13.0, z0: 46.0, z1: 49.0 }]),
            _ => panic!("no scene {name}"),
        }
    }

    pub fn legal(&self) -> Legal<'_> {
        Legal::new(&self.slices, &self.heights, &self.p, &self.blocks, &[])
    }

    /// What of each part layer's underside needs support.
    pub fn demand(&self) -> Vec<Vec<ExPolygon>> {
        crate::support::demand(Ctl::default(), &self.slices, &self.heights, &self.p).unwrap()
    }
}

//! How far a field is from the design: its surface points' signed distances to the design
//! surface (positive: material outside it, an excess; negative: missing, an undercut).

use crate::design::Design;
use fab_robot::field::BuildField;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Deviation {
    /// Largest absolute, mean absolute and 95th percentile (mm).
    pub max: f64,
    pub mean: f64,
    pub p95: f64,
    /// Largest excess and largest undercut (mm, both positive).
    pub excess: f64,
    pub undercut: f64,
    pub points: usize,
}

impl Deviation {
    fn of(mut d: Vec<f64>) -> Deviation {
        if d.is_empty() {
            return Deviation::default();
        }
        let excess = d.iter().fold(0.0f64, |m, v| m.max(*v));
        let undercut = d.iter().fold(0.0f64, |m, v| m.max(-*v));
        let mut a: Vec<f64> = d.drain(..).map(f64::abs).collect();
        a.sort_by(f64::total_cmp);
        let n = a.len();
        Deviation { max: a[n - 1], mean: a.iter().sum::<f64>() / n as f64, p95: a[((n as f64 * 0.95) as usize).min(n - 1)], excess, undercut, points: n }
    }
}

/// The deviation by face: the walls facing out from the part's axis, those facing in, and the
/// tops; points within `skip` mm of the plate, or further than `far` mm from the design (spatter,
/// what the scan saw of the plate), are left out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Faces {
    pub outer: Deviation,
    pub inner: Deviation,
    pub top: Deviation,
}

pub fn deviation(field: &BuildField, design: &Design, skip: f64, far: f64) -> Faces {
    let mut pts = Vec::new();
    field.surface_points(&mut pts);
    let c = [0.5 * (design.lo[0] + design.hi[0]), 0.5 * (design.lo[1] + design.hi[1])];
    let (mut outer, mut inner, mut top) = (Vec::new(), Vec::new(), Vec::new());
    for (p, _) in pts {
        if p[2] < skip {
            continue;
        }
        let d = design.distance(p);
        if d.abs() > far {
            continue;
        }
        let n = design.normal(p);
        if n[2] > 0.7 {
            top.push(d);
        } else if n[2] > -0.7 {
            let radial = n[0] * (p[0] - c[0]) + n[1] * (p[1] - c[1]);
            if radial >= 0.0 {
                outer.push(d);
            } else {
                inner.push(d);
            }
        }
    }
    Faces { outer: Deviation::of(outer), inner: Deviation::of(inner), top: Deviation::of(top) }
}

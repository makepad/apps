//! Pieces and parts as bodies for the Mechanics sim.

use crate::grid::DistGrid;
use crate::pieces::Piece;
use makepad_box3d::field::DistanceField;
use makepad_box3d::math_functions::{vec3, Vec3, AABB};
use makepad_csg_mech::geom::P3;
use makepad_csg_mech::mass::MassProps;
use makepad_csg_mech::sim::{BodySpec, SolidField};
use std::sync::Arc;

/// How much of a piece's outline is plastic: tree supports are thin walled, normal ones sparse.
pub const SUPPORT_FILL: f64 = 0.3;

/// A part's grid as the sim's solid.
#[derive(Debug)]
pub struct GridField(pub Arc<DistGrid>);

impl DistanceField for GridField {
    #[inline]
    fn distance(&self, p: Vec3) -> (f32, Vec3) {
        let (d, g) = self.0.sample([p.x, p.y, p.z]);
        (d, vec3(g[0], g[1], g[2]))
    }
    fn bounds(&self) -> AABB {
        let grid = &self.0;
        AABB { lower_bound: vec3(grid.box_lo[0], grid.box_lo[1], grid.box_lo[2]), upper_bound: vec3(grid.box_hi[0], grid.box_hi[1], grid.box_hi[2]) }
    }
}

/// The part's body with `grid` as its solid against the pieces: `body` as the sim would have it
/// without them.
pub fn with_field(mut body: BodySpec, grid: &Arc<DistGrid>) -> BodySpec {
    body.field = Some(SolidField(Arc::new(GridField(grid.clone()))));
    body
}

/// A piece as a body, of a plastic of `density` g/mm^3: mass from what it prints, its spread from
/// its balls.
pub fn piece_body(piece: &Piece, density: f64) -> BodySpec {
    let mass = (piece.volume * SUPPORT_FILL * density).max(1e-5);
    let weight = |r: f32| (r as f64).powi(3);
    let total: f64 = piece.balls.iter().map(|b| weight(b.r)).sum::<f64>().max(1e-12);
    let mut com = [0.0; 3];
    for b in &piece.balls {
        for k in 0..3 {
            com[k] += b.c[k] as f64 * weight(b.r) / total;
        }
    }
    let mut inertia = [[0.0; 3]; 3];
    for b in &piece.balls {
        let m = mass * weight(b.r) / total;
        let d: [f64; 3] = std::array::from_fn(|k| b.c[k] as f64 - com[k]);
        let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        let own = 0.4 * m * (b.r as f64).powi(2);
        for (i, row) in inertia.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell += m * (if i == j { dd } else { 0.0 } - d[i] * d[j]) + if i == j { own } else { 0.0 };
            }
        }
    }
    let p3 = |p: [f32; 3]| -> P3 { [p[0] as f64, p[1] as f64, p[2] as f64] };
    BodySpec {
        name: piece.name.clone(),
        mass: MassProps { volume: piece.volume, mass, com, inertia },
        hulls: Vec::new(),
        tubes: Vec::new(),
        balls: piece.balls.iter().map(|b| (p3(b.c), b.r as f64)).collect(),
        rods: piece.rods.iter().map(|r| (p3(r.a), p3(r.b), r.r as f64)).collect(),
        field: None,
    }
}

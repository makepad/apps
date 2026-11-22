//! What a piece looks like: tubes where a branch runs clear of the part, its balls where it was
//! cut to fit, and pads and footprints as thin slabs. In plate mm.

use makepad_csg_primitives::ear_clip_triangulate;
use makepad_fab_slicer::geom::ExPolygon;

const TUBE_SIDES: usize = 6;
const BALL_RINGS: usize = 3;
const BALL_SIDES: usize = 6;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PieceMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
    [v[0] / l, v[1] / l, v[2] / l]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

impl PieceMesh {
    /// A low round ball.
    pub fn ball(&mut self, c: [f32; 3], r: f32) {
        let base = self.positions.len() as u32;
        for j in 0..=BALL_RINGS {
            let v = std::f32::consts::PI * j as f32 / BALL_RINGS as f32;
            for i in 0..BALL_SIDES {
                let u = std::f32::consts::TAU * i as f32 / BALL_SIDES as f32;
                let n = [v.sin() * u.cos(), v.sin() * u.sin(), v.cos()];
                self.positions.push([c[0] + r * n[0], c[1] + r * n[1], c[2] + r * n[2]]);
                self.normals.push(n);
            }
        }
        let at = |j: usize, i: usize| base + (j * BALL_SIDES + i % BALL_SIDES) as u32;
        for j in 0..BALL_RINGS {
            for i in 0..BALL_SIDES {
                self.indices.extend([at(j, i), at(j + 1, i), at(j + 1, i + 1), at(j, i), at(j + 1, i + 1), at(j, i + 1)]);
            }
        }
    }

    /// A tube along (centre, radius) stops, its ends closed by balls.
    pub fn tube(&mut self, stops: &[([f32; 3], f32)]) {
        let Some((first, last)) = stops.first().zip(stops.last()) else { return };
        self.ball(first.0, first.1);
        if stops.len() < 2 {
            return;
        }
        self.ball(last.0, last.1);
        let base = self.positions.len() as u32;
        for (s, &(c, r)) in stops.iter().enumerate() {
            let prev = stops[s.saturating_sub(1)].0;
            let next = stops[(s + 1).min(stops.len() - 1)].0;
            let along = normalized([next[0] - prev[0], next[1] - prev[1], next[2] - prev[2]]);
            let side = if along[2].abs() < 0.9 { normalized(cross(along, [0.0, 0.0, 1.0])) } else { normalized(cross(along, [1.0, 0.0, 0.0])) };
            let other = cross(along, side);
            for i in 0..TUBE_SIDES {
                let a = std::f32::consts::TAU * i as f32 / TUBE_SIDES as f32;
                let n = [side[0] * a.cos() + other[0] * a.sin(), side[1] * a.cos() + other[1] * a.sin(), side[2] * a.cos() + other[2] * a.sin()];
                self.positions.push([c[0] + r * n[0], c[1] + r * n[1], c[2] + r * n[2]]);
                self.normals.push(n);
            }
        }
        let at = |s: usize, i: usize| base + (s * TUBE_SIDES + i % TUBE_SIDES) as u32;
        for s in 0..stops.len() - 1 {
            for i in 0..TUBE_SIDES {
                self.indices.extend([at(s, i), at(s, i + 1), at(s + 1, i + 1), at(s, i), at(s + 1, i + 1), at(s + 1, i)]);
            }
        }
    }

    /// An island on the plate from `z0` to `z1`: its outline's walls and its caps. A hole shows in
    /// the walls and is capped over.
    pub fn extrude(&mut self, e: &ExPolygon, at: [f64; 2], z0: f64, z1: f64) {
        let (z0, z1) = (z0.max(0.0) as f32, z1 as f32);
        let place = |p: makepad_fab_slicer::geom::Pt| [(p.x_mm() + at[0]) as f32, (p.y_mm() + at[1]) as f32];
        for ring in e.rings() {
            let pts: Vec<[f32; 2]> = ring.pts.iter().map(|&p| place(p)).collect();
            let n = pts.len();
            for i in 0..n {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let normal = normalized([b[1] - a[1], a[0] - b[0], 0.0]);
                let base = self.positions.len() as u32;
                self.positions.extend([[a[0], a[1], z0], [b[0], b[1], z0], [b[0], b[1], z1], [a[0], a[1], z1]]);
                self.normals.extend([normal; 4]);
                self.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
        let outline: Vec<[f32; 2]> = e.contour.pts.iter().map(|&p| place(p)).collect();
        let flat: Vec<[f64; 2]> = outline.iter().map(|p| [p[0] as f64, p[1] as f64]).collect();
        let triangles = ear_clip_triangulate(&flat);
        for (z, normal, flip) in [(z1, [0.0, 0.0, 1.0], false), (z0, [0.0, 0.0, -1.0], true)] {
            let base = self.positions.len() as u32;
            for p in &outline {
                self.positions.push([p[0], p[1], z]);
                self.normals.push(normal);
            }
            for t in &triangles {
                let (a, b, c) = (t[0] as u32, t[1] as u32, t[2] as u32);
                if flip {
                    self.indices.extend([base + a, base + c, base + b]);
                } else {
                    self.indices.extend([base + a, base + b, base + c]);
                }
            }
        }
    }
}

/// A mesh with each corner's normal the mean of its vertex's faces within `crease` degrees of its
/// own face (area-weighted): smooth round a strut, sharp at a pad's rim.
pub fn creased(positions: &[[f64; 3]], triangles: &[[u32; 3]], crease: f64) -> PieceMesh {
    let face: Vec<[f64; 3]> = triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| positions[i as usize]);
            let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
            [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
        })
        .collect();
    // each vertex's faces, as runs
    let mut start = vec![0u32; positions.len() + 1];
    for t in triangles {
        for &i in t {
            start[i as usize + 1] += 1;
        }
    }
    for i in 1..start.len() {
        start[i] += start[i - 1];
    }
    let mut fill = start.clone();
    let mut faces = vec![0u32; triangles.len() * 3];
    for (f, t) in triangles.iter().enumerate() {
        for &i in t {
            faces[fill[i as usize] as usize] = f as u32;
            fill[i as usize] += 1;
        }
    }
    let unit = |v: [f64; 3]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-30);
        [v[0] / l, v[1] / l, v[2] / l]
    };
    let cos = crease.to_radians().cos();
    let mut out = PieceMesh { positions: Vec::with_capacity(triangles.len() * 3), normals: Vec::with_capacity(triangles.len() * 3), indices: Vec::with_capacity(triangles.len() * 3) };
    for (f, t) in triangles.iter().enumerate() {
        let own = unit(face[f]);
        for &i in t {
            let mut n = [0.0f64; 3];
            for &g in &faces[start[i as usize] as usize..start[i as usize + 1] as usize] {
                let o = unit(face[g as usize]);
                if o[0] * own[0] + o[1] * own[1] + o[2] * own[2] >= cos {
                    let w = face[g as usize];
                    n = [n[0] + w[0], n[1] + w[1], n[2] + w[2]];
                }
            }
            let n = unit(n);
            let p = positions[i as usize];
            out.indices.push(out.positions.len() as u32);
            out.positions.push([p[0] as f32, p[1] as f32, p[2] as f32]);
            out.normals.push([n[0] as f32, n[1] as f32, n[2] as f32]);
        }
    }
    out
}

//! The few solids the test parts are made of, built straight as closed, outward-wound meshes:
//! solids of revolution (a closed profile turned about Z), prisms (a closed outline extruded),
//! boxes, and their sum. Overlapping solids in one part are fine: the slicer unions a part's
//! shells (non-zero winding).

use fab_plate::PartMesh;

/// Segments per full turn for a revolved solid: fine enough that the chord error of a 10 mm
/// radius stays under 0.01 mm, well below every clearance the parts test.
pub const TURN_SEGMENTS: usize = 96;

/// A closed profile in the (r, z) half-plane (r >= 0, counter-clockwise) turned once about the
/// Z axis. Points on the axis (r = 0) become one vertex each, so the solid is closed there.
pub fn lathe(profile: &[[f64; 2]], segments: usize) -> PartMesh {
    let n = profile.len();
    let seg = segments.max(3);
    let mut m = PartMesh::default();
    // Per profile point: its first vertex and whether it sits on the axis.
    let mut first = Vec::with_capacity(n);
    for p in profile {
        first.push(m.positions.len() as u32);
        if p[0] <= 1e-9 {
            m.positions.push([0.0, 0.0, p[1]]);
        } else {
            for j in 0..seg {
                let a = std::f64::consts::TAU * j as f64 / seg as f64;
                m.positions.push([p[0] * a.cos(), p[0] * a.sin(), p[1]]);
            }
        }
    }
    let at = |i: usize, j: usize| -> u32 { if profile[i][0] <= 1e-9 { first[i] } else { first[i] + (j % seg) as u32 } };
    for i in 0..n {
        let k = (i + 1) % n;
        if profile[i][0] <= 1e-9 && profile[k][0] <= 1e-9 {
            continue;
        }
        for j in 0..seg {
            let (a, b, c, d) = (at(i, j), at(k, j), at(k, j + 1), at(i, j + 1));
            // A counter-clockwise profile turned towards +Y winds (a, c, b) outward.
            if b != c {
                m.triangles.push([a, c, b]);
            }
            if a != d {
                m.triangles.push([a, d, c]);
            }
        }
    }
    m
}

/// A closed outline in the XY plane (counter-clockwise, simple, may be concave) extruded from
/// `z0` to `z1`.
pub fn prism(outline: &[[f64; 2]], z0: f64, z1: f64) -> PartMesh {
    let n = outline.len();
    let mut m = PartMesh::default();
    for p in outline {
        m.positions.push([p[0], p[1], z0]);
    }
    for p in outline {
        m.positions.push([p[0], p[1], z1]);
    }
    for t in triangulate(outline) {
        m.triangles.push([t[0], t[2], t[1]]);
        m.triangles.push([t[0] + n as u32, t[1] + n as u32, t[2] + n as u32]);
    }
    for i in 0..n {
        let k = (i + 1) % n;
        let (a, b, c, d) = (i as u32, k as u32, (k + n) as u32, (i + n) as u32);
        m.triangles.push([a, b, c]);
        m.triangles.push([a, c, d]);
    }
    m
}

/// A side profile in the (x, z) plane (counter-clockwise seen from -Y, simple) extruded along Y
/// from `y0` to `y1`.
pub fn prism_xz(profile: &[[f64; 2]], y0: f64, y1: f64) -> PartMesh {
    // Built in XY and turned +90° about X: (x, y, z) -> (x, -z, y). A rotation keeps the winding.
    let mut m = prism(profile, -y1, -y0);
    for p in &mut m.positions {
        *p = [p[0], -p[2], p[1]];
    }
    m
}

/// An axis-aligned box.
pub fn cuboid(min: [f64; 3], max: [f64; 3]) -> PartMesh {
    prism(&[[min[0], min[1]], [max[0], min[1]], [max[0], max[1]], [min[0], max[1]]], min[2], max[2])
}

/// A vertical cylinder standing on `z0`.
pub fn cylinder(c: [f64; 2], r: f64, z0: f64, z1: f64) -> PartMesh {
    let mut m = lathe(&[[0.0, z0], [r, z0], [r, z1], [0.0, z1]], TURN_SEGMENTS / 2);
    m.translate([c[0], c[1], 0.0]);
    m
}

/// Moved copy of a mesh.
pub fn moved(mut m: PartMesh, by: [f64; 3]) -> PartMesh {
    m.translate(by);
    m
}

/// The solids of `parts` as one mesh (their shells side by side).
pub fn merge(parts: impl IntoIterator<Item = PartMesh>) -> PartMesh {
    let mut out = PartMesh::default();
    for p in parts {
        let base = out.positions.len() as u32;
        out.positions.extend(p.positions);
        out.triangles.extend(p.triangles.into_iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
    }
    out
}

/// Volume and centre of mass of a closed, outward-wound mesh (uniform density).
pub fn mass_centre(m: &PartMesh) -> (f64, [f64; 3]) {
    let mut v = 0.0;
    let mut c = [0.0; 3];
    for t in &m.triangles {
        let (a, b, d) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
        let vt = (a[0] * (b[1] * d[2] - b[2] * d[1]) - a[1] * (b[0] * d[2] - b[2] * d[0]) + a[2] * (b[0] * d[1] - b[1] * d[0])) / 6.0;
        v += vt;
        for k in 0..3 {
            c[k] += vt * (a[k] + b[k] + d[k]) / 4.0;
        }
    }
    if v.abs() < 1e-12 {
        return (0.0, [0.0; 3]);
    }
    (v, [c[0] / v, c[1] / v, c[2] / v])
}

/// Ear-clipping triangulation of a simple counter-clockwise outline (indices into it).
fn triangulate(poly: &[[f64; 2]]) -> Vec<[u32; 3]> {
    let mut idx: Vec<usize> = (0..poly.len()).collect();
    let mut out = Vec::new();
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut guard = 0;
    while idx.len() > 3 && guard < 10_000 {
        guard += 1;
        let n = idx.len();
        let mut clipped = false;
        for i in 0..n {
            let (ip, ic, inx) = (idx[(i + n - 1) % n], idx[i], idx[(i + 1) % n]);
            let (a, b, c) = (poly[ip], poly[ic], poly[inx]);
            if cross(a, b, c) <= 1e-12 {
                continue;
            }
            let inside = idx.iter().any(|&q| {
                if q == ip || q == ic || q == inx {
                    return false;
                }
                let p = poly[q];
                cross(a, b, p) >= -1e-12 && cross(b, c, p) >= -1e-12 && cross(c, a, p) >= -1e-12
            });
            if inside {
                continue;
            }
            out.push([ip as u32, ic as u32, inx as u32]);
            idx.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    if idx.len() == 3 {
        out.push([idx[0] as u32, idx[1] as u32, idx[2] as u32]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(m: &PartMesh) {
        assert_eq!(m.open_edges(), 0, "the mesh is closed and consistently wound");
        assert!(m.volume() > 0.0, "outward winding");
    }

    #[test]
    fn solids_are_closed_with_their_volume() {
        let b = cuboid([0.0, 0.0, 0.0], [2.0, 3.0, 4.0]);
        closed(&b);
        assert!((b.volume() - 24.0).abs() < 1e-9);
        let c = cylinder([5.0, 5.0], 2.0, 0.0, 3.0);
        closed(&c);
        assert!((c.volume() - std::f64::consts::PI * 4.0 * 3.0).abs() < 0.2, "a 48-gon of r 2");
        // A washer: no axis points.
        let w = lathe(&[[2.0, 0.0], [4.0, 0.0], [4.0, 1.0], [2.0, 1.0]], 64);
        closed(&w);
        assert!((w.volume() - std::f64::consts::PI * 12.0).abs() < 0.1);
        // A concave outline (an L) and a side profile.
        let l = prism(&[[0.0, 0.0], [3.0, 0.0], [3.0, 1.0], [1.0, 1.0], [1.0, 3.0], [0.0, 3.0]], 0.0, 2.0);
        closed(&l);
        assert!((l.volume() - 10.0).abs() < 1e-9);
        let s = prism_xz(&[[0.0, 0.0], [4.0, 0.0], [4.0, 1.0], [0.0, 2.0]], 0.0, 5.0);
        closed(&s);
        assert!((s.volume() - 30.0).abs() < 1e-9);
        let bb = s.bounds();
        assert!((bb.min[1] - 0.0).abs() < 1e-9 && (bb.max[1] - 5.0).abs() < 1e-9 && (bb.max[2] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn centre_of_mass() {
        let (v, c) = mass_centre(&cuboid([0.0, 0.0, 0.0], [2.0, 4.0, 6.0]));
        assert!((v - 48.0).abs() < 1e-9);
        assert!((c[0] - 1.0).abs() < 1e-9 && (c[1] - 2.0).abs() < 1e-9 && (c[2] - 3.0).abs() < 1e-9);
    }
}

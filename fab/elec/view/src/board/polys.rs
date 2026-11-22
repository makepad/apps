//! Worker-side polygon fills and closed outlines in board millimetres.

use fab_elec_core::{Nm, PolySet};
use makepad_widgets_core::makepad_draw::vector::{
    tessellate_path_fill, FillRule, LineJoin, Tessellator, VVertex, VectorPath,
};

/// A polygon set as triangles (board mm, f32) and its rings as closed polylines (board mm).
pub struct PolyMesh {
    pub tris: Vec<[[f32; 2]; 3]>,
    /// Each ring closes from its last point to its first, without repeating the first point.
    pub rings: Vec<Vec<[f32; 2]>>,
}

/// A board coordinate (nm) as the rings carry it (mm, f32): through micrometres, so the same nm
/// always gives the same bits, and a ring point compares exactly with a box edge converted here.
pub fn ring_mm(nm: Nm) -> f32 {
    (nm as f64 / 1e3) as f32 / 1000.0
}

/// Triangulates the normalised fill of `poly`, retaining every ring for hairline outlines.
pub fn mesh(poly: &PolySet) -> PolyMesh {
    let mut path = VectorPath::new();
    // PolySet normalises ring_area2 > 0 for CCW outers and < 0 for CW holes.
    // Preserve those opposite windings: nonzero fill then leaves the holes empty.
    path.set_fill_rule(FillRule::NonZero);
    let mut rings = Vec::new();
    for ring in poly.rings() {
        let mut points = Vec::with_capacity(ring.len());
        for (i, p) in ring.iter().enumerate() {
            // Tessellate in micrometres: the fixed 0.01 point-merge tolerance is 10 nm.
            // Convert nm through f64 before f32; divide both rings and triangles by 1000
            // for board mm (f32 in um retains about 0.06 um precision over a metre).
            let x = (p.x as f64 / 1e3) as f32;
            let y = (p.y as f64 / 1e3) as f32;
            if i == 0 {
                path.move_to(x, y);
            } else {
                path.line_to(x, y);
            }
            points.push([ring_mm(p.x), ring_mm(p.y)]);
        }
        path.close();
        rings.push(points);
    }

    let mut tess = Tessellator::default();
    tess.set_trust_fill_winding(true);
    let mut verts: Vec<VVertex> = Vec::new();
    let mut indices = Vec::new();
    // aa = 0 leaves the body on its original edges and emits no fringe (fill() only
    // emits it when aa * 0.5 > 0). No GPU expansion; ring strokes supply the edge AA.
    tessellate_path_fill(
        &mut path, &mut tess, &mut verts, &mut indices, LineJoin::Miter, 4.0, 0.0, false, 0.25,
    );
    let mut tris = Vec::with_capacity(indices.len() / 3);
    for face in indices.chunks_exact(3) {
        let point = |i: u32| {
            let v = verts[i as usize];
            [v.x / 1000.0, v.y / 1000.0]
        };
        let tri = [point(face[0]), point(face[1]), point(face[2])];
        let [a, b, c] = tri;
        let area2 = (b[0] as f64 - a[0] as f64) * (c[1] as f64 - a[1] as f64)
            - (b[1] as f64 - a[1] as f64) * (c[0] as f64 - a[0] as f64);
        if area2.abs() >= 2e-12 {
            tris.push(tri);
        }
    }
    PolyMesh { tris, rings }
}

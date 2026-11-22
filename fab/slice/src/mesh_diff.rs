//! Where two meshes of the same triangles can slice differently: the Z ranges of the triangles
//! a moved vertex belongs to, old and new. A plane outside them crosses the same triangles at
//! the same points, met in the same order (the cut walks its triangles by their lowest Z, stable
//! in their order), so an edited mesh part re-cuts only the planes inside them.

use makepad_fab_slicer::mesh::Mesh;

/// The Z ranges (closed, merged, ascending) where `a` and `b` can slice differently: empty when
/// they are the same mesh, None when their triangles differ (another topology cuts anywhere).
pub fn changed_z(a: &Mesh, b: &Mesh) -> Option<Vec<(f64, f64)>> {
    if a.verts.len() != b.verts.len() || a.tris != b.tris {
        return None;
    }
    let moved: Vec<bool> = a.verts.iter().zip(&b.verts).map(|(p, q)| (0..3).any(|k| p[k].to_bits() != q[k].to_bits())).collect();
    // Neighbouring triangles mostly follow each other, so a triangle overlapping the last range
    // joins it: few ranges are left to sort.
    let mut ranges: Vec<(f64, f64)> = Vec::new();
    for t in &a.tris {
        if !t.iter().any(|&v| moved[v as usize]) {
            continue;
        }
        let zs = t.iter().flat_map(|&v| [a.verts[v as usize][2], b.verts[v as usize][2]]);
        let (lo, hi) = zs.fold((f64::MAX, f64::MIN), |(lo, hi), z| (lo.min(z), hi.max(z)));
        match ranges.last_mut() {
            Some(last) if lo <= last.1 && hi >= last.0 => *last = (last.0.min(lo), last.1.max(hi)),
            _ => ranges.push((lo, hi)),
        }
    }
    ranges.sort_unstable_by(|x, y| x.0.total_cmp(&y.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (lo, hi) in ranges {
        match out.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(top: f64) -> Mesh {
        // two quads stacked: z 0..1 and 1..top
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 0.0, top], [1.0, 0.0, top]];
        Mesh { verts, tris: vec![[0, 1, 3], [0, 3, 2], [2, 3, 5], [2, 5, 4]], paint: Vec::new() }
    }

    #[test]
    fn a_moved_vertex_reaches_its_triangles_old_and_new() {
        assert_eq!(changed_z(&strip(2.0), &strip(2.0)), Some(Vec::new()));
        assert_eq!(changed_z(&strip(2.0), &strip(3.0)), Some(vec![(1.0, 3.0)]));
        let mut other = strip(2.0);
        other.tris.swap(0, 1);
        assert_eq!(changed_z(&strip(2.0), &other), None, "other triangles");
    }
}

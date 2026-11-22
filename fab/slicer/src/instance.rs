//! Instancing: parts that are the same mesh at another XY position.
//!
//! A plate often carries copies of one object (a 3MF build item repeated,
//! or a duplicated part). Each copy slices to the same layers translated,
//! so the slicer plans one representative and places it at every copy's
//! offset (an exact integer-nm translation).

use crate::geom::Pt;
use crate::mesh::Mesh;
use std::collections::HashMap;

/// Largest vertex deviation (mm) from an exact translation that still
/// counts as a copy: far below the 1 nm grid the slicer rounds to.
const TOL_MM: f64 = 1e-9;

/// For each mesh: (its representative, the representative-to-copy XY
/// offset in grid units). A representative maps to itself with a zero
/// offset; the representative of a class is its first member.
///
/// A copy has the same triangles and paint as its representative, every
/// vertex equal to the representative's plus one XY offset (within
/// [`TOL_MM`]) with Z exactly equal, so its layer planes cut the same
/// triangles; the offset is snapped to whole nanometres.
pub fn classes(meshes: &[&Mesh]) -> Vec<(usize, Pt)> {
    let mut out: Vec<(usize, Pt)> = (0..meshes.len()).map(|i| (i, Pt::default())).collect();
    // Bucket by a translation-free signature: counts, topology, paint, and
    // every vertex relative to the first, rounded to 1e-6 mm.
    let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, m) in meshes.iter().enumerate() {
        buckets.entry(signature(m)).or_default().push(i);
    }
    let mut keys: Vec<u64> = buckets.keys().copied().collect();
    keys.sort_unstable();
    for k in keys {
        let members = &buckets[&k];
        let mut reps: Vec<usize> = Vec::new();
        for &i in members {
            let found = reps.iter().find_map(|&r| copy_offset(meshes[r], meshes[i]).map(|d| (r, d)));
            match found {
                Some((r, d)) => out[i] = (r, d),
                None => reps.push(i),
            }
        }
    }
    // Representatives are the first member by index.
    out
}

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x100000001b3).rotate_left(29)
}

fn signature(m: &Mesh) -> u64 {
    let mut h = mix(0xcbf29ce484222325, m.verts.len() as u64);
    h = mix(h, m.tris.len() as u64);
    for t in &m.tris {
        h = mix(h, (t[0] as u64) | (t[1] as u64) << 21 | (t[2] as u64) << 42);
    }
    for p in &m.paint {
        for b in p.bytes() {
            h = mix(h, b as u64);
        }
        h = mix(h, 0xff);
    }
    if let Some(v0) = m.verts.first() {
        for v in &m.verts {
            for k in 0..3 {
                let rel = if k == 2 { v[2] } else { v[k] - v0[k] };
                h = mix(h, (rel * 1e6).round() as i64 as u64);
            }
        }
    }
    h
}

/// The integer-nm XY offset taking `rep` onto `m`, when `m` is a copy.
fn copy_offset(rep: &Mesh, m: &Mesh) -> Option<Pt> {
    if rep.tris != m.tris || rep.paint != m.paint || rep.verts.len() != m.verts.len() {
        return None;
    }
    let (a, b) = (rep.verts.first()?, m.verts.first()?);
    let d = Pt::new(((b[0] - a[0]) * 1e6).round() as i64, ((b[1] - a[1]) * 1e6).round() as i64);
    let (dx, dy) = (d.x as f64 / 1e6, d.y as f64 / 1e6);
    for (p, q) in rep.verts.iter().zip(&m.verts) {
        if p[2] != q[2] || (p[0] + dx - q[0]).abs() > TOL_MM || (p[1] + dy - q[1]).abs() > TOL_MM {
            return None;
        }
    }
    Some(d)
}

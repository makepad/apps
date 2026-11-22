use fab_elec_mech::CaseSolid;
use fab_plate::PartMesh;

/// As in enclosure.rs, but on a CaseSolid already mapped into the board frame.
pub fn inside(mesh: &CaseSolid, p: [f64; 3]) -> bool {
    let d = [0.5773, 0.5774, 0.5775];
    let mut n = 0;
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|i| mesh.positions[i as usize]);
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let h = [d[1] * e2[2] - d[2] * e2[1], d[2] * e2[0] - d[0] * e2[2], d[0] * e2[1] - d[1] * e2[0]];
        let det = e1[0] * h[0] + e1[1] * h[1] + e1[2] * h[2];
        if det.abs() < 1e-12 {
            continue;
        }
        let s = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
        let u = (s[0] * h[0] + s[1] * h[1] + s[2] * h[2]) / det;
        let q = [s[1] * e1[2] - s[2] * e1[1], s[2] * e1[0] - s[0] * e1[2], s[0] * e1[1] - s[1] * e1[0]];
        let v = (d[0] * q[0] + d[1] * q[1] + d[2] * q[2]) / det;
        let t = (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) / det;
        if u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t > 0.0 {
            n += 1;
        }
    }
    n % 2 == 1
}

/// Edges one face uses (open) or more than two use (non-manifold), with vertices welded by exact
/// position as the printability check welds them, in model mm (the plate placement undone).
pub fn open_at(mesh: &PartMesh, to_plate: &[[f64; 4]; 3]) -> Vec<[[f64; 3]; 2]> {
    let mut ids = std::collections::HashMap::new();
    let weld: Vec<usize> = mesh.positions.iter().map(|p| {
        let next = ids.len();
        *ids.entry(p.map(|v| if v == 0.0 { 0 } else { v.to_bits() })).or_insert(next)
    }).collect();
    let mut edges: std::collections::HashMap<(usize, usize), (u32, u32, usize)> = std::collections::HashMap::new();
    for t in &mesh.triangles {
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            let (wa, wb) = (weld[a as usize], weld[b as usize]);
            if wa != wb {
                edges.entry((wa.min(wb), wa.max(wb))).or_insert((a, b, 0)).2 += 1;
            }
        }
    }
    let model = |v: u32| {
        let q = mesh.positions[v as usize];
        let d: [f64; 3] = std::array::from_fn(|r| q[r] - to_plate[r][3]);
        std::array::from_fn(|c| (0..3).map(|r| to_plate[r][c] * d[r]).sum::<f64>())
    };
    edges.values().filter(|(_, _, n)| *n != 2).map(|(a, b, _)| [model(*a), model(*b)]).collect()
}

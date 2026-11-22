use super::*;
use crate::source::MeshSource;

/// A closed prism: the ring `ring` (counter-clockwise, mm) from `z0` to `z1`.
fn prism(ring: &[[f64; 2]], z0: f64, z1: f64) -> Mesh {
    let n = ring.len() as u32;
    let mut verts: Vec<[f64; 3]> = ring.iter().map(|p| [p[0], p[1], z0]).collect();
    verts.extend(ring.iter().map(|p| [p[0], p[1], z1]));
    let mut tris = Vec::new();
    for k in 1..n - 1 {
        tris.push([0, k + 1, k]);
        tris.push([n, n + k, n + k + 1]);
    }
    for k in 0..n {
        let j = (k + 1) % n;
        tris.push([k, j, n + j]);
        tris.push([k, n + j, n + k]);
    }
    Mesh { verts, tris, ..Mesh::default() }
}

fn square(cx: f64, cy: f64, half: f64) -> Vec<[f64; 2]> {
    vec![[cx - half, cy - half], [cx + half, cy - half], [cx + half, cy + half], [cx - half, cy + half]]
}

fn joined(a: Mesh, b: Mesh) -> Mesh {
    let base = a.verts.len() as u32;
    let mut m = a;
    m.verts.extend(b.verts);
    m.tris.extend(b.tris.iter().map(|t| t.map(|i| i + base)));
    m
}

fn host() -> Arc<dyn ContourSource + Send + Sync> {
    Arc::new(MeshSource::new(prism(&square(10.0, 10.0, 10.0), 0.0, 12.0)))
}

/// Whether the part is cut away at `(x, y)` on plane `z`.
fn open_at(part: &Embedded, z: f64, x: f64, y: f64) -> bool {
    let regions = &part.contours(&[z])[0];
    assert!(!regions.is_empty(), "the host prints at {z}");
    !regions.iter().any(|e| e.contains(Pt::from_mm(x, y)))
}

#[test]
fn a_nut_is_cut_from_the_layers_that_reach_it_and_covered_above() {
    let r = 3.2;
    let hex: Vec<[f64; 2]> = (0..6).map(|k| {
        let a = k as f64 * std::f64::consts::PI / 3.0;
        [10.0 + r * a.cos(), 10.0 + r * a.sin()]
    }).collect();
    let nut = Item { name: "M3 nut".into(), mesh: prism(&hex, 3.0, 5.4), clearance: 0.2 };
    let part = Embedded::new(host(), vec![nut], 0.2);
    let embeds = part.embeds();
    assert_eq!(embeds.len(), 1);
    assert_eq!(embeds[0].name, "M3 nut");
    assert!((embeds[0].cover - 5.6).abs() < 1e-9, "the cover is its top plus its clearance: {}", embeds[0].cover);
    // Sideways: the hex grown by the clearance and the margin, no more.
    assert!(open_at(&part, 4.1, 10.0, 10.0), "the cavity holds the nut");
    assert!(open_at(&part, 4.1, 10.0 + r + 0.2, 10.0), "a corner keeps its clearance");
    assert!(!open_at(&part, 4.1, 10.0 + r + 0.25, 10.0), "the cavity is the nut's own shape, grown by its clearance and margin only");
    let flat = |d: f64| (10.0 + (r * 0.75f64.sqrt() + d) * 0.75f64.sqrt(), 10.0 + (r * 0.75f64.sqrt() + d) * 0.5);
    let (x, y) = flat(0.2);
    assert!(open_at(&part, 4.1, x, y), "a flat keeps its clearance");
    let (x, y) = flat(0.25);
    assert!(!open_at(&part, 4.1, x, y), "and no more");
    // Up: cut below the cover, whole from there.
    assert!(open_at(&part, 5.5, 10.0, 10.0), "the clearance above the nut is cut");
    assert!(!open_at(&part, 5.7, 10.0, 10.0), "the first plane at the cover closes it in");
    // Down: cut from where a layer reaches the nut's bottom.
    assert!(!open_at(&part, 2.85, 10.0, 10.0), "a layer that ends below the nut is whole: it rests on it");
    assert!(open_at(&part, 2.95, 10.0, 10.0), "a layer whose top reaches the nut is cut");
    // The first layer over the nut starts at or above its top, whatever its height up to the
    // thickest, even with no clearance above it.
    let tight = Embedded::new(host(), vec![Item { name: "M3 nut".into(), mesh: prism(&hex, 3.0, 5.4), clearance: 0.0 }], 0.2);
    assert!((tight.embeds()[0].cover - 5.5).abs() < 1e-9, "with no clearance, half the thickest layer");
    for part in [&part, &tight] {
        for h in [0.08, 0.12, 0.2] {
            let first = (0..400).map(|k| 0.5 * h + k as f64 * h).find(|&z| z > 3.0 && part.cavity(z).is_empty()).unwrap();
            assert!(first - 0.5 * h >= 5.4 - 1e-9, "{h} mm layers: the first whole layer over the nut starts at {} (its top 5.4)", first - 0.5 * h);
        }
    }
    // A plane on the cover is whole, whatever the rounding of the sums: 8.8 + 0.3 is a hair over
    // the plane of the 9.2 mm layer, 9.2 - 0.1. The print pauses before that layer, so it must
    // close the item in.
    let board = Embedded::new(host(), vec![Item { name: "Board".into(), mesh: prism(&square(10.0, 10.0, 2.0), 7.2, 8.8), clearance: 0.3 }], 0.2);
    assert!(board.embeds()[0].cover > 9.2 - 0.1, "the sum rounds up: {}", board.embeds()[0].cover);
    assert!(!open_at(&board, 9.2 - 0.1, 10.0, 10.0) && open_at(&board, 9.0 - 0.1, 10.0, 10.0), "the layer on the cover closes it in, the one under it is cut");
}

/// A wedge from x 8 to `bottom` at z 3 and to `top` at z 7 (y 8 to 12).
fn wedge(bottom: f64, top: f64) -> Mesh {
    Mesh {
        verts: vec![[8.0, 8.0, 3.0], [bottom, 8.0, 3.0], [bottom, 12.0, 3.0], [8.0, 12.0, 3.0], [8.0, 8.0, 7.0], [top, 8.0, 7.0], [top, 12.0, 7.0], [8.0, 12.0, 7.0]],
        tris: vec![[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]],
        ..Mesh::default()
    }
}

#[test]
fn a_cavity_is_its_item_swept_up_so_it_drops_in() {
    // A bolt head down: its head passes through the shank's layers as it goes in.
    let head_down = joined(prism(&square(10.0, 10.0, 3.0), 3.0, 5.0), prism(&square(10.0, 10.0, 1.0), 5.0, 9.0));
    let part = Embedded::new(host(), vec![Item { name: "Bolt".into(), mesh: head_down, clearance: 0.2 }], 0.2);
    assert!(open_at(&part, 4.0, 13.1, 10.0) && !open_at(&part, 4.0, 13.3, 10.0), "the head's own layers: the head and its clearance");
    assert!(open_at(&part, 7.0, 13.1, 10.0) && !open_at(&part, 7.0, 13.3, 10.0), "the shank's layers let the head through, no wider");
    assert!((part.embeds()[0].cover - 9.2).abs() < 1e-9);
    // Head up: nothing wide passes the shank's layers, and the head rests on the layer under it.
    let head_up = joined(prism(&square(10.0, 10.0, 1.0), 3.0, 6.0), prism(&square(10.0, 10.0, 3.0), 6.0, 8.0));
    let part = Embedded::new(host(), vec![Item { name: "Bolt".into(), mesh: head_up, clearance: 0.2 }], 0.2);
    assert!(open_at(&part, 4.0, 11.1, 10.0) && !open_at(&part, 4.0, 11.3, 10.0), "under the head only the shank is cut");
    assert!(!open_at(&part, 5.8, 12.0, 10.0) && open_at(&part, 5.95, 12.0, 10.0), "the head is cut from the layer that reaches it");
    assert!(open_at(&part, 7.0, 13.1, 10.0));
    // A face that leans in from a wide bottom: the bottom goes in through every layer over it. One
    // that leans out: each layer is cut as wide as the wedge up to its own top, no wider.
    let part = Embedded::new(host(), vec![Item { name: "Wedge".into(), mesh: wedge(14.0, 10.0), clearance: 0.2 }], 0.2);
    assert!(open_at(&part, 6.5, 14.1, 10.0) && !open_at(&part, 6.5, 14.3, 10.0), "the wide bottom drops through the narrow top's layers");
    let part = Embedded::new(host(), vec![Item { name: "Wedge".into(), mesh: wedge(10.0, 14.0), clearance: 0.2 }], 0.2);
    for z in [3.5, 5.0, 6.5] {
        // At the layer's top (z + 0.1) the wedge reaches x = 10 + (z + 0.1 - 3).
        let edge = 10.0 + (z + 0.1 - 3.0) + 0.2;
        assert!(open_at(&part, z, edge - 0.01, 10.0), "{z}: the wedge up to its layer's top and its clearance");
        assert!(!open_at(&part, z, edge + STEP + MARGIN + 0.02, 10.0), "{z}: and no more than a step over it");
    }
}

#[test]
fn several_items_cut_their_own_cavities() {
    let a = Item { name: "A".into(), mesh: prism(&square(5.0, 5.0, 1.0), 2.0, 4.0), clearance: 0.2 };
    let b = Item { name: "B".into(), mesh: prism(&square(15.0, 15.0, 1.0), 6.0, 7.0), clearance: 0.3 };
    let flat = Item { name: "Flat".into(), mesh: prism(&square(10.0, 10.0, 1.0), 5.0, 5.0), clearance: 0.2 };
    let part = Embedded::new(host(), vec![a, b, flat], 0.2);
    assert_eq!(part.embeds().iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["A", "B"], "an item with no height cuts nothing");
    assert!(open_at(&part, 3.0, 5.0, 5.0) && !open_at(&part, 3.0, 15.0, 15.0));
    assert!(open_at(&part, 6.5, 15.0, 15.0) && !open_at(&part, 6.5, 5.0, 5.0));
    assert!(open_at(&part, 6.5, 16.29, 15.0) && !open_at(&part, 6.5, 16.35, 15.0), "each keeps its own clearance");
}

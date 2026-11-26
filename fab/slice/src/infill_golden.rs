//! Every infill pattern slices and prints, and an unknown name fails the slice. (The comparison
//! with the private reference slices needed a folder that never ships and ran as a no-op everywhere,
//! so it is gone.)

use crate::golden;
use crate::{place, slice, PlacedObject};
use makepad_fab_slicer::mesh::Mesh;

fn cuboid(m: &mut Mesh, lo: [f64; 3], hi: [f64; 3]) {
    let base = m.verts.len() as u32;
    for z in [lo[2], hi[2]] {
        for (x, y) in [(lo[0], lo[1]), (hi[0], lo[1]), (hi[0], hi[1]), (lo[0], hi[1])] {
            m.verts.push([x, y, z]);
        }
    }
    let t = |a: u32, b: u32, c: u32| [base + a, base + b, base + c];
    m.tris.extend([t(0, 2, 1), t(0, 3, 2), t(4, 5, 6), t(4, 6, 7)]);
    for i in 0..4 {
        let j = (i + 1) % 4;
        m.tris.push(t(i, j, 4 + j));
        m.tris.push(t(i, 4 + j, 4 + i));
    }
}

/// The step box: a 40×40×12 base, a 20×20 tower up to 24.
fn step_box(x: f64, y: f64) -> Vec<PlacedObject> {
    let mut m = Mesh::default();
    cuboid(&mut m, [-20.0, -20.0, 0.0], [20.0, 20.0, 12.0]);
    cuboid(&mut m, [-10.0, -10.0, 11.0], [10.0, 10.0, 24.0]);
    place(&mut m, x, y);
    vec![PlacedObject { name: "step_box".into(), mesh: m, filament: 0 }]
}

/// Length of `feature` summed over layers [a, b).
fn range_len(s: &golden::GcodeStats, feature: &str, a: usize, b: usize) -> f64 {
    s.layers.iter().skip(a).take(b.saturating_sub(a)).map(|l| l.features.get(feature).map_or(0.0, |f| f.length)).sum()
}

/// Every pattern slices and lays sparse infill, and a name Fab does not know fails the slice
/// instead of printing another pattern.
#[test]
fn every_pattern_slices_and_unknown_fails() {
    use makepad_fab_toolpath::pattern::Sparse;
    use std::collections::BTreeSet;

    let fab_plate::SettingKind::Choice(choices) = fab_plate::print_setting("sparse_infill_pattern").unwrap().kind else { panic!("the sparse pattern is a choice") };
    let offered: BTreeSet<_> = choices.iter().map(|(name, _)| *name).collect();
    let printable: BTreeSet<_> = Sparse::ALL.into_iter().map(|(name, _)| name).collect();
    assert_eq!(offered, printable, "the panel must offer every sparse pattern the slicer builds");
    for (name, _) in Sparse::ALL {
        let st = crate::tests::bare_setup(1, &[("sparse_infill_pattern", name)]);
        let res = slice(&st, step_box(175.0, 160.0)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let stats = golden::analyse(&res.gcode);
        let sparse = range_len(&stats, "Sparse infill", 0, stats.layers.len());
        assert!(sparse > 100.0, "{name}: {sparse} mm of sparse infill");
    }
    for (name, _) in makepad_fab_toolpath::pattern::Surface::ALL {
        let mut kv = vec![("top_surface_pattern", name)];
        if name != "globalmonotonicline" {
            kv.push(("bottom_surface_pattern", name));
            kv.push(("internal_solid_infill_pattern", name));
        }
        let st = crate::tests::bare_setup(1, &kv);
        slice(&st, step_box(175.0, 160.0)).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    for (key, bad) in [("sparse_infill_pattern", "cubbic"), ("top_surface_pattern", "grid"), ("bottom_surface_pattern", "globalmonotonicline")] {
        let st = crate::tests::bare_setup(1, &[(key, bad)]);
        let err = slice(&st, step_box(175.0, 160.0)).err().unwrap_or_else(|| panic!("{key}={bad} sliced"));
        assert!(err.contains("unknown infill pattern") && err.contains(bad), "{err}");
    }
}

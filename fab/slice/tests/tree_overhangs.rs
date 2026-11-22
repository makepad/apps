//! R14 §2's overhang detector on the three bench cubes, against what the reference logs for them:
//! the layers with an overhang at the default threshold and at 45°, and every cantilever cluster
//! with its layer and length. Each cube is cut at layer middles from its lowest point, neither
//! closed nor simplified, and its slices prepared as the reference prepares them for §2 (§2.0,
//! `overhangs::slices`): closed by 0.049 mm and simplified at 0.0025 mm island by island.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_slicer::mesh::Mesh;
use makepad_fab_slicer::slice::SliceParams;
use makepad_fab_slicer::source::{ContourSource, MeshSource};
use makepad_fab_toolpath::par;
use makepad_fab_toolpath::tree::overhangs::{detect, Overhangs};
use makepad_fab_toolpath::tree::settings::{Keys, Settings};
use std::collections::BTreeMap;

const LATTICE: &str = include_str!("../../app/resources/lattice_cube.cad.splash");

/// The support-cover cube: 43.2 mm, the lattice at a 0.5 mm cell.
const COVER: &str = "use mod.fab.*\nscript(\"lattice_cube.cad.splash\", params: {lattice_period: 13mm lattice_size: 27mm})\nplace(\"lattice_cube\", object: \"lattice\", at: [90mm 160mm], scale: 1.6, name: \"Block\")\n";
/// The app's demo cube: 40 mm, the lattice at a 0.5 mm cell.
const DEMO: &str = "use mod.fab.*\nscript(\"lattice_cube.cad.splash\", params: {lattice_period: 13.2195mm lattice_size: 25mm})\nplace(\"lattice_cube\", object: \"lattice\", at: [175mm 160mm], scale: 1.6, name: \"Lattice cube\")\n";
/// The 46.4 mm cube, the lattice at a 0.2 mm cell.
const FINE: &str = "use mod.fab.*\nscript(\"lattice_cube.cad.splash\", params: {lattice_period: 13.2195mm lattice_size: 29mm})\nplace(\"lattice_cube\", object: \"lattice\", at: [80mm 165mm], scale: 1.6, name: \"Lattice cube\")\n";

/// How near (mm) a cantilever's length is to the one the reference logs.
const NEAR: f64 = 0.001;

/// The cube's overhangs at the default threshold (30°) and at 45°, in R14's profile: layers of
/// 0.2 mm on a 0.25 mm first layer, lines of 0.42 mm.
fn overhangs(plate: &str, cell: &str, layers: usize) -> [Overhangs; 2] {
    par::set_threads(par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)));
    let script = LATTICE.replace("cell: 0.5mm", cell);
    let scripts = BTreeMap::from([("lattice_cube.cad.splash".to_string(), script)]);
    let model = FabModel::load("bench", plate, scripts).unwrap_or_else(|f| panic!("{f}"));
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    e.mesh_fields = true;
    let ev: Evaluated = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    let part = &ev.plate.parts[0].mesh;
    let mut mesh = Mesh { verts: part.positions.clone(), tris: part.triangles.clone(), paint: Vec::new() };
    mesh.fix_orientation();
    let (lo, hi) = mesh.bbox();
    let heights = makepad_fab_toolpath::layer_heights(hi[2] - lo[2], 0.25, 0.2);
    assert_eq!(heights.len(), layers, "the layer stack");
    let mut src = MeshSource::new(mesh);
    src.params = SliceParams { closing_radius: 0.0, resolution: 0.0, max_gap: 2.0 };
    let cut = src.contours(&heights.iter().map(|&(z, h)| lo[2] + z - h / 2.0).collect::<Vec<_>>());
    let (slices, _) = makepad_fab_toolpath::tree::overhangs::slices(par::Ctl::default(), &cut, None).unwrap();
    [30.0, 45.0].map(|key| {
        let s = Settings::new(Keys { threshold_deg: key, ..Keys::new(0.2, 0.42, 0.42) });
        detect(par::Ctl::default(), &s, &slices, &heights, &[], &[], None).unwrap().0
    })
}

/// Both thresholds find the reference's cantilever clusters, layer for layer, each as long as it
/// logs to within [`NEAR`]: (layer, length in mm), in layer and length order.
fn clusters_match(o: &Overhangs, want: &[(usize, f64)], at: f64) {
    let mut got = o.cantilevers.clone();
    got.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let layers = |v: &[(usize, f64)]| v.iter().map(|c| c.0).collect::<Vec<_>>();
    assert_eq!(layers(&got), layers(want), "the cantilever clusters' layers at {at}°: {got:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g.1 - w.1).abs() < NEAR, "the cluster on layer {} at {at}° is {:.5} mm, the reference's {:.5}", g.0, g.1, w.1);
    }
}

#[test]
fn the_cover_cube_overhangs_as_the_reference_finds_them() {
    let [def, b45] = overhangs(COVER, "cell: 0.5mm", 216);
    assert_eq!(def.layers_with_overhang(), 62, "layers with an overhang at the default threshold");
    assert_eq!(b45.layers_with_overhang(), 143, "layers with an overhang at 45°");
    let want = [(204, 3.11774), (204, 3.20660), (204, 3.22354), (204, 3.22356), (204, 3.36795), (204, 3.36795), (204, 3.36796)];
    clusters_match(&def, &want, 30.0);
    clusters_match(&b45, &want, 45.0);
}

#[test]
fn the_demo_cube_overhangs_as_the_reference_finds_them() {
    let [def, b45] = overhangs(DEMO, "cell: 0.5mm", 200);
    assert_eq!(def.layers_with_overhang(), 58, "layers with an overhang at the default threshold");
    // The reference counts 148 as well. Before every offset dropped its close points first (§1.5)
    // this was 149, with #103's 1e-5 mm² sliver on the frame's wall the likely extra layer; the
    // drop took #27's 0.009 mm² piece instead, so the two may still differ by a layer each way.
    // The reference's log names only its cantilever layers.
    assert_eq!(b45.layers_with_overhang(), 148, "layers with an overhang at 45°");
    let want = [
        (116, 0.396231), (116, 0.540141), (140, 0.579242), (168, 0.250250),
        (169, 0.819537), (169, 0.864357), (169, 0.901069), (169, 1.108620),
        (188, 3.073090), (188, 3.196130), (188, 3.201090), (188, 3.211840), (188, 4.656470), (188, 5.068090), (188, 5.138580),
    ];
    clusters_match(&def, &want, 30.0);
    clusters_match(&b45, &want, 45.0);
}

#[test]
fn the_fine_cube_overhangs_as_the_reference_finds_them() {
    let [def, b45] = overhangs(FINE, "cell: 0.2mm", 232);
    assert_eq!(def.layers_with_overhang(), 81, "layers with an overhang at the default threshold");
    // The reference counts 171. The one more is a speck under 1e-5 mm² on layer #176 that §2.7
    // drops and Fab's departure keeps: it lies in a face Fab's own measure counts
    // (`Cluster::counted`). The detector also finds one small piece (0.02-0.11 mm²) a layer on
    // the cube's left face on layers #45 to #52, where the reference prints roofs up to layer
    // #47. Its log names only its cantilever layers, so whether the two agree layer for layer is
    // not known.
    assert_eq!(b45.layers_with_overhang(), 172, "layers with an overhang at 45°");
    let want = [
        (29, 7.110820), (29, 7.114650), (29, 7.114790),
        (81, 6.891640), (81, 6.894940), (81, 6.900570),
        (134, 6.948090), (134, 6.950210), (134, 6.952930),
        (137, 0.432357), (137, 0.445636), (137, 0.459372), (137, 0.473070),
        (150, 0.029400),
        (187, 7.031060), (187, 7.039030), (187, 7.047460),
    ];
    clusters_match(&def, &want, 30.0);
    clusters_match(&b45, &want, 45.0);
}

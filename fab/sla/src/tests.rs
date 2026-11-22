//! Layer images of known shapes from a field and from a mesh: their cured area is the true
//! cross-section's within what the grey levels round off, their edge pixels hold the share of
//! them inside, and the two sources draw the same image.

use crate::grid::PixelGrid;
use crate::image::{Layer, LayerStack};
use crate::spans::grey;
use crate::printer::{simulated, ResinProfile};
use crate::section::{FieldSection, MeshSection, Section};
use fab_plate::PartMesh;
use makepad_csg_sdf::{SdfNode, SdfOp, SdfShape};
use std::f64::consts::{PI, TAU};
use std::sync::Arc;

/// The model frame (Y up) onto the plate (Z up) about `c`: model `(x, y, z)` is printer
/// `(x, -z, y) + c`.
fn placed(c: [f64; 3]) -> [[f64; 4]; 3] {
    [[1.0, 0.0, 0.0, c[0]], [0.0, 0.0, -1.0, c[1]], [0.0, 1.0, 0.0, c[2]]]
}

pub(crate) fn field(root: SdfNode, c: [f64; 3], bounds: ([f64; 3], [f64; 3])) -> FieldSection {
    let shape = SdfShape { root, min: [-50.0; 3], max: [50.0; 3], materials: Vec::new() };
    FieldSection::new(&shape, placed(c), bounds).expect("the field compiles")
}

/// A prism between `z0` and `z1` over `outer` (counter-clockwise), less `hole` (counter-
/// clockwise, as many points, each at the same angle about the centre as its outer point).
pub(crate) fn prism(outer: &[[f64; 2]], hole: Option<&[[f64; 2]]>, z0: f64, z1: f64) -> PartMesh {
    let n = outer.len() as u32;
    let mut positions: Vec<[f64; 3]> = Vec::new();
    for ring in std::iter::once(outer).chain(hole) {
        for z in [z0, z1] {
            positions.extend(ring.iter().map(|p| [p[0], p[1], z]));
        }
    }
    let (ob, ot, hb, ht) = (0, n, 2 * n, 3 * n);
    let mut triangles = Vec::new();
    for a in 0..n {
        let b = (a + 1) % n;
        triangles.extend([[ob + a, ob + b, ot + b], [ob + a, ot + b, ot + a]]);
        if hole.is_some() {
            triangles.extend([[hb + b, hb + a, ht + a], [hb + b, ht + a, ht + b]]);
            triangles.extend([[ot + a, ot + b, ht + b], [ot + a, ht + b, ht + a]]);
            triangles.extend([[ob + a, hb + b, ob + b], [ob + a, hb + a, hb + b]]);
        } else if a > 0 && b > 0 {
            triangles.extend([[ob, ob + b, ob + a], [ot, ot + a, ot + b]]);
        }
    }
    PartMesh { positions, triangles }
}

pub(crate) fn rect(lo: [f64; 2], hi: [f64; 2]) -> Vec<[f64; 2]> {
    vec![lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]]
}

/// A sphere of `n` longitudes and `n / 2` latitudes.
pub(crate) fn sphere_mesh(c: [f64; 3], r: f64, n: u32) -> PartMesh {
    let m = n / 2;
    let mut positions = vec![[c[0], c[1], c[2] - r], [c[0], c[1], c[2] + r]];
    for j in 1..m {
        let phi = PI * j as f64 / m as f64 - 0.5 * PI;
        for i in 0..n {
            let th = TAU * i as f64 / n as f64;
            positions.push([c[0] + r * phi.cos() * th.cos(), c[1] + r * phi.cos() * th.sin(), c[2] + r * phi.sin()]);
        }
    }
    let at = |j: u32, i: u32| 2 + (j - 1) * n + i % n;
    let mut triangles = Vec::new();
    for i in 0..n {
        triangles.push([0, at(1, i + 1), at(1, i)]);
        triangles.push([1, at(m - 1, i), at(m - 1, i + 1)]);
        for j in 1..m - 1 {
            triangles.extend([[at(j, i), at(j, i + 1), at(j + 1, i + 1)], [at(j, i), at(j + 1, i + 1), at(j + 1, i)]]);
        }
    }
    PartMesh { positions, triangles }
}

/// Layer `k` of one part alone on the simulated printer.
fn image(p: &ResinProfile, s: Arc<dyn Section>, k: u32) -> Layer {
    LayerStack::new(vec![s], p).layer(k)
}

/// Pixel `(i, j)` as the screen of `levels` grey levels shows it.
fn shown(l: &Layer, i: u32, j: u32, levels: u16) -> u8 {
    grey(l.spans.at(i, j), levels)
}

/// The two layers as the screen shows them, over the same pixels: the pixels whose values
/// differ by more than one grey level, and the largest difference.
fn compare(a: &Layer, b: &Layer, levels: u16) -> (usize, u8) {
    let r = a.spans.rect.union(&b.spans.rect);
    let level = (255 / (levels - 1)) as u8;
    let (mut off, mut worst) = (0, 0u8);
    for j in r.y0..r.y1 {
        for i in r.x0..r.x1 {
            let d = shown(a, i, j, levels).abs_diff(shown(b, i, j, levels));
            off += (d > level) as usize;
            worst = worst.max(d);
        }
    }
    (off, worst)
}

struct Case {
    name: &'static str,
    field: Arc<FieldSection>,
    mesh: Arc<MeshSection>,
    layer: u32,
    /// The true cross-section's area (mm²) and outline length (mm) at that layer.
    area: f64,
    perimeter: f64,
}

fn cases() -> Vec<Case> {
    let cube = ([20.13, 30.27, 0.0], [30.13, 38.27, 6.0]);
    let hole = ([80.0, 10.0, 0.0], [100.0, 30.0, 5.0]);
    let wall = ([100.02, 50.0, 0.0], [100.32, 65.0, 5.0]);
    let fine = ([105.0125, 50.0, 0.0], [105.0875, 65.0, 5.0]);
    let circle = |c: [f64; 2], r: f64, n: usize| (0..n).map(|k| {
        let t = TAU * k as f64 / n as f64;
        [c[0] + r * t.cos(), c[1] + r * t.sin()]
    }).collect::<Vec<_>>();
    // the square about the hole's centre at the circle's angles: its corners among them
    let square = |c: [f64; 2], a: f64, n: usize| (0..n).map(|k| {
        let t = TAU * k as f64 / n as f64;
        let s = a / t.cos().abs().max(t.sin().abs());
        [c[0] + s * t.cos(), c[1] + s * t.sin()]
    }).collect::<Vec<_>>();
    let mid = |b: &([f64; 3], [f64; 3])| [0.5 * (b.0[0] + b.1[0]), 0.5 * (b.0[1] + b.1[1]), 0.5 * (b.0[2] + b.1[2])];
    let sphere_z = 5.025;
    vec![
        Case {
            name: "cube",
            field: Arc::new(field(SdfNode::Box { size: [10.0, 6.0, 8.0], radius: 0.0 }, mid(&cube), cube)),
            mesh: Arc::new(MeshSection::new(&prism(&rect([20.13, 30.27], [30.13, 38.27]), None, 0.0, 6.0))),
            layer: 10,
            area: 80.0,
            perimeter: 36.0,
        },
        Case {
            name: "sphere",
            field: Arc::new(field(SdfNode::Sphere { radius: 5.0 }, [60.0, 40.0, 5.0], ([55.0, 35.0, 0.0], [65.0, 45.0, 10.0]))),
            mesh: Arc::new(MeshSection::new(&sphere_mesh([60.0, 40.0, 5.0], 5.0, 256))),
            layer: 100,
            area: PI * (25.0 - (sphere_z - 5.0f64).powi(2)),
            perimeter: TAU * 5.0,
        },
        Case {
            name: "hole",
            field: Arc::new(field(
                SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![SdfNode::Box { size: [20.0, 5.0, 20.0], radius: 0.0 }, SdfNode::Cylinder { radius: 4.0, top_radius: 4.0, height: 10.0 }] },
                mid(&hole),
                hole,
            )),
            mesh: Arc::new(MeshSection::new(&prism(&square([90.0, 20.0], 10.0, 256), Some(&circle([90.0, 20.0], 4.0, 256)), 0.0, 5.0))),
            layer: 40,
            area: 400.0 - 16.0 * PI,
            perimeter: 80.0 + TAU * 4.0,
        },
        Case {
            name: "wall",
            field: Arc::new(field(SdfNode::Box { size: [0.3, 5.0, 15.0], radius: 0.0 }, mid(&wall), wall)),
            mesh: Arc::new(MeshSection::new(&prism(&rect([100.02, 50.0], [100.32, 65.0]), None, 0.0, 5.0))),
            layer: 40,
            area: 0.3 * 15.0,
            perimeter: 30.6,
        },
        Case {
            name: "wall of a pixel and a half",
            field: Arc::new(field(SdfNode::Box { size: [0.075, 5.0, 15.0], radius: 0.0 }, mid(&fine), fine)),
            mesh: Arc::new(MeshSection::new(&prism(&rect([105.0125, 50.0], [105.0875, 65.0]), None, 0.0, 5.0))),
            layer: 40,
            area: 0.075 * 15.0,
            perimeter: 30.15,
        },
    ]
}

/// Each shape's layer image, from its field and from its mesh: the cured area is the true
/// cross-section's within the grey levels' rounding along its outline (and the mesh's facets for
/// a curve), and the two images agree pixel for pixel but at the field's corners.
#[test]
fn layer_images_of_known_shapes_cure_their_area_from_field_and_mesh_alike() {
    let p = simulated();
    let grid = PixelGrid::of(&p.screen);
    let (levels, level) = (p.screen.grey_levels, (255 / (p.screen.grey_levels - 1)) as u8);
    for c in cases() {
        let f = image(&p, c.field.clone(), c.layer);
        let m = image(&p, c.mesh.clone(), c.layer);
        // each outline pixel rounds to the nearest of the grey levels: at most half a level
        let rounding = c.perimeter / grid.pitch[0] * 2.0 * 0.5 / (p.screen.grey_levels - 1) as f64 * grid.pixel_area();
        // a chord of a 256-gon cuts off a sliver of the circle (curves only)
        let facets = if c.name == "cube" || c.name.starts_with("wall") { 0.0 } else { 3.0e-4 * c.area };
        for (source, img, slack) in [("field", &f, rounding), ("mesh", &m, rounding + facets)] {
            let a = img.area(&grid);
            assert!((a - c.area).abs() <= slack, "{} from its {source}: {a:.4} mm² cured, {:.4} true (within {slack:.4})", c.name, c.area);
        }
        // a pixel apart only at corners, where a field's pixel sees one edge of two
        let (off, worst) = compare(&f, &m, levels);
        let corners = if c.name == "cube" || c.name.starts_with("wall") { 4 } else { 8 };
        assert!(off <= corners, "{}: {off} pixels differ by more than a grey level (worst {worst})", c.name);
        assert!(worst <= 3 * level, "{}: a pixel differs by {worst}", c.name);
    }
}

/// The cube's edges fall inside pixels: each edge pixel holds the share of it inside, on the
/// grey levels, from both sources; the interior is lit and the outside dark.
#[test]
fn edge_pixels_hold_their_covered_share() {
    let p = simulated();
    let c = &cases()[0];
    let g = |share: f64| ((share * 15.0).round() * 17.0) as u8;
    for (source, img) in [("field", image(&p, c.field.clone(), c.layer)), ("mesh", image(&p, c.mesh.clone(), c.layer))] {
        // the cube spans x 20.13..30.13 and y 30.27..38.27; pixels are 0.05 mm
        let (row, col) = (700, 500);
        let at = |i, j| shown(&img, i, j, 16);
        assert_eq!(at(402, row), g(0.4), "{source}: the left edge's pixel");
        assert_eq!(at(602, row), g(0.6), "{source}: the right edge's pixel");
        assert_eq!(at(col, 605), g(0.6), "{source}: the front edge's pixel");
        assert_eq!(at(col, 765), g(0.4), "{source}: the back edge's pixel");
        assert_eq!((img.spans.at(402, row), img.spans.at(col, 605)), (102, 153), "{source}: the shares themselves");
        assert_eq!((at(401, row), at(403, row), at(col, 700)), (0, 255, 255), "{source}");
        assert_eq!((at(603, row), at(col, 604), at(col, 766)), (0, 0, 0), "{source}");
    }
    // the hole's centre is dark, the ring around it lit
    let h = &cases()[2];
    for img in [image(&p, h.field.clone(), h.layer), image(&p, h.mesh.clone(), h.layer)] {
        // the hole spans x 86..94 at y 20, the box 80..100
        let at = |i, j| img.spans.at(i, j);
        assert_eq!((at(1800, 400), at(1860, 400), at(1900, 400), at(2000, 400)), (0, 0, 255, 0));
    }
}

/// A layer cut a hundredth of a millimetre under a field's flat top, or over it, grazes the face:
/// no edge crosses its pixels, so the top is wholly lit or dark, never grey.
#[test]
fn a_layer_grazing_a_flat_face_is_lit_or_dark() {
    let p = simulated();
    // layer 40 is cut at 2.025 mm
    for (top, inside) in [(2.035, 255), (2.015, 0)] {
        let cube = Arc::new(field(SdfNode::Box { size: [10.0, top, 10.0], radius: 0.0 }, [30.0, 30.0, 0.5 * top], ([24.9, 24.9, -0.1], [35.1, 35.1, top + 0.1])));
        let img = image(&p, cube, 40);
        assert_eq!(img.spans.at(600, 600), inside, "a top at {top} mm");
    }
}

/// A field's whole tiles proven clear of the surface take one read at their centre: the cube's
/// layer reads the field at far fewer points than it has pixels, and draws the same image.
#[test]
fn proven_tiles_are_filled_without_reading_their_pixels() {
    let p = simulated();
    let c = &cases()[0];
    let before = c.field.evals();
    // halfway up, the cube's top and bottom are 3 mm away
    let img = image(&p, c.field.clone(), 60);
    let read = c.field.evals() - before;
    let pixels = img.spans.rect.pixels() as u64;
    assert!(read * 2 < pixels, "{read} reads for {pixels} pixels");
    assert_eq!(img.spans.lit(), 201 * 161, "every pixel the cube touches is lit");
}

/// The whole stack: one image per layer up to the top, each the same as made alone.
#[test]
fn the_stack_is_every_layer() {
    let p = simulated();
    let c = &cases()[1];
    let stack = LayerStack::new(vec![c.field.clone() as Arc<dyn Section>], &p);
    assert_eq!(stack.layers.count, 200, "10 mm in 0.05 mm layers");
    let areas = stack.map(|img| img.area(&stack.grid));
    let grid = stack.grid;
    for k in [0u32, 37, 100, 199] {
        assert_eq!(areas[k as usize], stack.layer(k).area(&grid), "layer {k}");
        let z = stack.layers.z(k);
        let true_area = PI * (25.0 - (z - 5.0).powi(2));
        assert!((areas[k as usize] - true_area).abs() < 0.12, "layer {k}: {} against {true_area}", areas[k as usize]);
    }
}


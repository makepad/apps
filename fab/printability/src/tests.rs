use super::*;
use fab_plate::{machine::Machine, Part, PartMesh, Plate};
use makepad_fab_gcode::{process::Timeline, record::{MoveKind, MoveRecord, Role}};
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt};
use makepad_fab_toolpath::{ExtrusionPath, Island, Layer, ObjectLayer};

fn box_mesh(lo: [f64; 3], size: [f64; 3]) -> PartMesh {
    PartMesh {
        positions: (0..8).map(|i| [lo[0]+size[0]*(i&1) as f64, lo[1]+size[1]*((i>>1)&1) as f64, lo[2]+size[2]*((i>>2)&1) as f64]).collect(),
        triangles: vec![[0,2,1], [1,2,3], [4,5,6], [5,7,6], [0,1,4], [1,5,4], [2,6,3], [3,6,7], [0,4,2], [2,4,6], [1,3,5], [3,7,5]],
    }
}

fn part(name: &str, mesh: PartMesh) -> Part {
    Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: name.into(), name: name.into(), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() }
}

fn plate() -> Plate { Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("model", box_mesh([100.0,100.0,0.0], [30.0,20.0,10.0]))] } }

#[test]
fn a_bead_inside_an_insert_is_an_error_and_a_touching_one_is_not() {
    let nut = part("Nut", box_mesh([0.0, 0.0, 2.0], [10.0, 10.0, 2.0]));
    let insert = fab_plate::Insert { into: "Host".into(), clearance: 0.2, embed: false, melt: None };
    let inserts = [(&nut, &insert)];
    let bead = MoveRecord { kind: MoveKind::Extrude, p0: [-1.0, 5.0, 3.0], p1: [11.0, 5.0, 3.0], width: 0.4, height: 0.2, e: 1.0, layer: 14, z_layer: 3.0, ..Default::default() };
    let check = |moves: &[MoveRecord]| insert::bead_issues(&inserts, &mut moves.iter().copied());
    let hits = check(&[bead]);
    assert_eq!(hits.len(), 1, "one crossing yields one issue");
    assert_eq!((hits[0].kind, hits[0].severity, hits[0].object.as_deref(), hits[0].layer, hits[0].layer_to), ("insert_hit", Severity::Error, Some("Nut"), Some(14), Some(14)), "the error names the insert and move layer");
    assert_eq!(hits[0].message, "A bead enters Nut at layer 14 (it keeps 0.2 mm clear)", "single-layer message");
    assert!(hits[0].footprint.min[0] <= 0.0 && hits[0].footprint.max[0] >= 10.0, "the footprint spans hit points across the insert");
    assert!(check(&[MoveRecord { p0: [-1.0, 5.0, 2.0], p1: [11.0, 5.0, 2.0], z_layer: 2.0, layer: 9, ..bead }]).is_empty(), "a bead ending at the bottom face only touches");
    assert!(check(&[MoveRecord { p0: [-1.0, 5.0, 4.2], p1: [11.0, 5.0, 4.2], z_layer: 4.2, layer: 20, ..bead }]).is_empty(), "a bead starting at the top face only touches");
    let beside = |y| MoveRecord { p0: [1.0, y, 3.0], p1: [9.0, y, 3.0], ..bead };
    assert!(check(&[beside(-0.5)]).is_empty(), "0.5 mm leaves room for half the width and clearance");
    assert_eq!(check(&[beside(-0.25)]).len(), 1, "0.25 mm enters the sideways clearance");
    assert!(check(&[MoveRecord { kind: MoveKind::Travel, ..bead }]).is_empty(), "travels do not deposit beads");
    let runs = check(&[bead, MoveRecord { layer: 15, ..bead }, MoveRecord { layer: 17, ..bead }]);
    assert_eq!(runs.iter().map(|i| (i.layer, i.layer_to)).collect::<Vec<_>>(), [(Some(14), Some(15)), (Some(17), Some(17))], "only consecutive hit layers merge");
    assert_eq!(runs[0].message, "A bead enters Nut at layers 14 to 15 (it keeps 0.2 mm clear)", "layer-range message");
    let mut records = std::iter::once(bead);
    assert!(insert::bead_issues(&[], &mut records).is_empty(), "no inserts means no issues");
    assert!(records.next().is_some(), "no inserts means no record traversal");
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
    ExPolygon::new(Polygon::rect(Pt::from_mm(x0,y0), Pt::from_mm(x1,y1)))
}

fn layer(index: usize, slices: Vec<ExPolygon>) -> Layer {
    Layer { index, print_z: (index+1) as f64*0.2, height: 0.2, objects: vec![ObjectLayer { object: 0, filament: 0, slices: slices.into(), ..Default::default() }] }
}

fn sliced(layers: &[Layer]) -> Report {
    let (m, p, s) = (Machine::dual_nozzle_350(), plate(), Materials::default());
    let mut report = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, layers, &Timeline::default(), &mut report);
    report
}

#[test]
fn mushroom_degrees_connected_patch_and_bed_exemption() {
    let mut mesh = box_mesh([110.0,110.0,0.0], [6.0,6.0,8.0]);
    let cap = box_mesh([100.0,100.0,8.0], [26.0,26.0,3.0]);
    let base = mesh.positions.len() as u32;
    mesh.positions.extend(cap.positions);
    mesh.triangles.extend(cap.triangles.into_iter().map(|t| t.map(|v| v+base)));
    let p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("mushroom", mesh)] };
    let mut s = Materials::default();
    let r = analyse_plate(&Machine::dual_nozzle_350(), &p, &s);
    assert_eq!(&r.overhang[0][..2], &[0,0]);
    assert_eq!(&r.overhang[0][12..14], &[5,5]);
    let patches: Vec<_> = r.issues.iter().filter(|i| i.kind == "overhang").collect();
    assert_eq!(patches.len(), 1, "the two cap triangles share one patch");
    assert!((patches[0].area_mm2-676.0).abs() < 1e-6);
    assert_eq!(patches[0].at_mm, [113.0,113.0,8.0]);
    assert_eq!(patches[0].footprint, Footprint { min: [100.0, 100.0], max: [126.0, 126.0] }, "the connected overhang patch, not its centroid");
    assert_eq!(patches[0].lowest_z, 8.0);
    assert_eq!(patches[0].severity, Severity::Warning);
    s.supports = true;
    let r = analyse_plate(&Machine::dual_nozzle_350(), &p, &s);
    assert_eq!(r.issues.iter().find(|i| i.kind == "overhang").unwrap().severity, Severity::Info);
}

// Exterior faces of a block with a straight through tunnel. Omitting the
// second pillar leaves a shelf; lifting one end makes the underside sloped.
fn tunnel_mesh(span: f64, second_pillar: bool, rise: f64) -> PartMesh {
    let xs = [100.0, 104.0, 104.0 + span, 108.0 + span];
    let mut mesh = PartMesh { positions: Vec::new(), triangles: Vec::new() };
    let occupied = |x: i32, z: i32| (0..3).contains(&x) && (0..2).contains(&z)
        && (z == 1 || x == 0 || (x == 2 && second_pillar));
    for x in 0..3 {
        for z in 0..2 {
            if !occupied(x, z) { continue; }
            let zs = [0.0, 8.0, 12.0];
            let cell = box_mesh([xs[x as usize], 100.0, zs[z as usize]],
                [xs[x as usize + 1] - xs[x as usize], 24.0, zs[z as usize + 1] - zs[z as usize]]);
            let base = mesh.positions.len() as u32;
            mesh.positions.extend(cell.positions.into_iter().map(|mut p| {
                if p[2] == 8.0 { p[2] += rise * ((p[0] - 104.0) / span).clamp(0.0, 1.0); }
                p
            }));
            for (face, pair) in cell.triangles.chunks_exact(2).enumerate() {
                let inside = match face {
                    0 => occupied(x, z - 1), 1 => occupied(x, z + 1),
                    4 => occupied(x - 1, z), 5 => occupied(x + 1, z),
                    _ => false,
                };
                if !inside { mesh.triangles.extend(pair.iter().map(|t| t.map(|v| v + base))); }
            }
        }
    }
    mesh
}

fn check_tunnel(mesh: PartMesh, materials: &Materials) -> Report {
    let p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("tunnel", mesh)] };
    analyse_plate(&Machine::dual_nozzle_350(), &p, materials)
}

#[test]
fn mesh_short_tunnel_ceiling_is_a_safe_bridge() {
    for span in [8.0, 10.0] {
        let mesh = tunnel_mesh(span, true, 0.0);
        let rendered = render_overhang(&fab_plate::RenderMesh {
            positions: mesh.positions.iter().map(|p| p.map(|v| v as f32)).collect(),
            indices: mesh.triangles.iter().flatten().copied().collect(),
            ..Default::default()
        }, 45.0);
        let r = check_tunnel(mesh, &Materials::default());
        assert!(!r.issues.iter().any(|i| matches!(i.kind, "overhang" | "bridge")), "span {span}: {:?}", r.issues);
        assert_eq!(r.overhang[0], rendered, "display degrees still describe face tilt");
        assert!(rendered.contains(&5), "a bridge ceiling still faces down");
    }
    let r = check_tunnel(tunnel_mesh(8.0, true, 0.0), &Materials { max_bridge_length: 7.0, ..Materials::default() });
    assert!(r.issues.iter().any(|i| i.kind == "bridge" && (i.measure - 8.0).abs() < 0.01), "the material's span limit applies");
}

#[test]
fn mesh_long_tunnel_ceiling_reports_its_anchored_span() {
    let sliced = sliced(&[
        layer(0, vec![rect(100.0,100.0,104.0,124.0), rect(118.0,100.0,122.0,124.0)]),
        layer(1, vec![rect(100.0,100.0,122.0,124.0)]),
    ]);
    let expected = sliced.issues.iter().find(|i| i.kind == "bridge").expect("the slice bridges the tunnel");
    for angle in [0.0f64, 0.37] {
        let mut mesh = tunnel_mesh(14.0, true, 0.0);
        for p in &mut mesh.positions {
            let (x, y) = (p[0] - 110.0, p[1] - 112.0);
            p[0] = 110.0 + x * angle.cos() - y * angle.sin();
            p[1] = 112.0 + x * angle.sin() + y * angle.cos();
        }
        let r = check_tunnel(mesh, &Materials::default());
        assert!(!r.issues.iter().any(|i| i.kind == "overhang"), "{:?}", r.issues);
        let bridges: Vec<_> = r.issues.iter().filter(|i| i.kind == "bridge").collect();
        assert_eq!(bridges.len(), 1, "{:?}", r.issues);
        let b = bridges[0];
        assert!((b.measure - 14.0).abs() < 0.01, "{b:?}");
        assert_eq!((b.at_mm[2], b.lowest_z, b.highest_z), (8.0, 8.0, 8.0));
        assert_eq!(b.severity, Severity::Warning);
        assert_eq!(b.message, "14.00 mm unsupported span between anchored edges");
        assert!((b.measure - expected.measure).abs() < 0.01, "mesh and sliced spans agree");
        assert_eq!(b.hint, expected.hint);
        assert_eq!(b.message, expected.message);
    }
}

#[test]
fn mesh_free_shelf_and_sloped_ceiling_keep_overhang_warnings() {
    for mesh in [tunnel_mesh(8.0, false, 0.0), tunnel_mesh(8.0, true, 1.0)] {
        let r = check_tunnel(mesh, &Materials::default());
        assert!(r.issues.iter().any(|i| i.kind == "overhang" && i.severity == Severity::Warning), "{:?}", r.issues);
        assert!(!r.issues.iter().any(|i| i.kind == "bridge"), "{:?}", r.issues);
    }
}

// Boundary faces only: a two-level Menger sponge with 18 and 6 mm tunnels.
// The smaller vertical holes interrupt the larger ceilings' bridge lines.
#[test]
fn mesh_menger_ceiling_spans() {
    let occupied = |x: i32, y: i32, z: i32| {
        if ![x, y, z].iter().all(|v| (0..9).contains(v)) { return false; }
        [1, 3].iter().all(|scale| [x, y, z].iter().filter(|&&v| (v / scale) % 3 == 1).count() < 2)
    };
    let mut mesh = PartMesh { positions: Vec::new(), triangles: Vec::new() };
    for z in 0..9 {
        for y in 0..9 {
            for x in 0..9 {
                if !occupied(x, y, z) { continue; }
                let cell = box_mesh([100.0 + x as f64 * 6.0, 100.0 + y as f64 * 6.0, z as f64 * 6.0], [6.0; 3]);
                let base = mesh.positions.len() as u32;
                mesh.positions.extend(cell.positions);
                let neighbors = [(x,y,z-1), (x,y,z+1), (x,y-1,z), (x,y+1,z), (x-1,y,z), (x+1,y,z)];
                for (face, &(a,b,c)) in neighbors.iter().enumerate() {
                    if !occupied(a,b,c) {
                        mesh.triangles.extend(cell.triangles[face*2..face*2+2].iter().map(|t| t.map(|v| v + base)));
                    }
                }
            }
        }
    }
    let cuts = makepad_fab_slicer::slice::slice_mesh(
        &makepad_fab_slicer::mesh::Mesh { verts: mesh.positions.clone(), tris: mesh.triangles.clone(), paint: Vec::new() },
        &[35.9, 36.1], &makepad_fab_slicer::slice::SliceParams::default());
    let slice_report = sliced(&[layer(0, cuts[0].clone()), layer(1, cuts[1].clone())]);
    let slice_bridges: Vec<_> = slice_report.issues.iter().filter(|i| i.kind == "bridge").collect();
    assert_eq!(slice_bridges.len(), 4, "sliced crossed tunnel: {slice_bridges:?}");
    assert!(slice_bridges.iter().all(|i| (i.measure - 18.0).abs() < 0.01), "sliced spans: {slice_bridges:?}");
    assert!(!slice_report.issues.iter().any(|i| i.kind == "cantilever"), "perforated ceilings bridge in one direction");
    let report = check_tunnel(mesh.clone(), &Materials::default());
    let steep: Vec<_> = report.issues.iter().filter(|i| i.kind == "overhang").collect();
    assert!(steep.is_empty(), "flat tunnel ceilings bridge: {steep:?}");
    let bridges: Vec<_> = report.issues.iter().filter(|i| i.kind == "bridge").collect();
    assert_eq!(bridges.len(), 4, "four arms of the large crossed tunnel: {bridges:?}");
    assert!(bridges.iter().all(|i| (i.measure - 18.0).abs() < 0.01 && i.lowest_z == 36.0), "only 18 mm spans warn: {bridges:?}");
    assert!(bridges.iter().all(|i| (0..2).all(|d| i.at_mm[d] >= i.footprint.min[d] && i.at_mm[d] <= i.footprint.max[d])), "translated bridge markers stay on their own ceiling");
    let shorter = check_tunnel(mesh, &Materials { max_bridge_length: 5.0, ..Materials::default() });
    let bridges: Vec<_> = shorter.issues.iter().filter(|i| i.kind == "bridge").collect();
    assert_eq!(bridges.len(), 68, "the shorter material limit also warns on the 64 six-millimetre ceilings");
    assert_eq!(bridges.iter().filter(|i| (i.measure - 6.0).abs() < 0.01).count(), 64);
    assert!(!shorter.issues.iter().any(|i| i.kind == "overhang"), "a material change recomputes bridge spans");
}

#[test]
fn bridge_between_two_pillars_measures_the_gap() {
    let r = sliced(&[layer(0, vec![rect(100.0,100.0,104.0,112.0), rect(124.0,100.0,128.0,112.0)]), layer(1, vec![rect(100.0,100.0,128.0,112.0)])]);
    let b = r.issues.iter().find(|i| i.kind == "bridge").expect("slab bridges the pillars");
    assert!((b.measure-20.0).abs() <= 0.5, "{}", b.measure);
    assert_eq!(b.layer, Some(2));
    assert_eq!(b.at_mm[2], 0.4);
    assert!(!r.issues.iter().any(|i| i.kind == "mid_air"));
    assert!(!r.issues.iter().any(|i| i.kind == "cantilever"));
}

fn support_line(role: Role, pts: Vec<Pt>) -> ExtrusionPath {
    ExtrusionPath { role, pts, closed: false, loop_pts: Vec::new(), width: 0.4, height: 0.2, area: 0.08, speed: 30.0, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 }
}

/// A round ceiling over a roof of support lines: a loop, and chords inside it a line and a gap
/// apart (0.857 mm, as the trees' roofs lie, and 1.2 mm). Each gap is held on both sides, so it is
/// spanned across: none is told as a bridge along its length, however many hatch lines fit along
/// it.
#[test]
fn a_roof_of_lines_bridges_each_gap_across() {
    let (c, radius) = ([115.0, 110.0], 9.6);
    let ring = |r: f64| (0..600).map(|i| {
        let a = std::f64::consts::TAU * i as f64 / 600.0;
        Pt::from_mm(c[0] + r * a.cos(), c[1] + r * a.sin())
    }).collect::<Vec<_>>();
    for spacing in [0.857, 1.2] {
        let mut lap = ring(radius - 0.1);
        lap.push(lap[0]);
        let mut roof = vec![support_line(Role::SupportInterface, lap)];
        roof.extend((1..).map(|k| 0.1 - radius + spacing * k as f64).take_while(|&y| y < radius - 0.3).map(|y| {
            let x = ((radius - 0.1).powi(2) - y * y).sqrt();
            support_line(Role::SupportInterface, vec![Pt::from_mm(c[0] - x, c[1] + y), Pt::from_mm(c[0] + x, c[1] + y)])
        }));
        let mut under = layer(0, Vec::new());
        under.objects[0].islands.push(std::sync::Arc::new(Island { walls: Vec::new(), fills: roof, ..Default::default() }));
        let r = sliced(&[under, layer(1, vec![ExPolygon::new(Polygon::new(ring(radius)))])]);
        let told: Vec<_> = r.issues.iter().filter(|i| matches!(i.kind, "bridge" | "cantilever" | "mid_air")).map(|i| (i.kind, i.measure)).collect();
        assert!(told.is_empty(), "spacing {spacing}: {told:?}");
    }
}

/// A ceiling over an annular sector, held at its arcs by two curved support lines and open at its
/// radial sides. Moving every point of the support lines by up to a nanometre each way moves the
/// span and its marker by no more than a few nanometres and keeps its words.
#[test]
fn a_bridge_span_stays_when_its_support_moves_a_nanometre() {
    let (c, a0, a1) = ([114.0, 104.0], 21.3f64, 68.9f64);
    let arc = |r: f64, from: f64, to: f64, step: f64| {
        let n = ((to - from).to_radians() * r / step).ceil() as usize;
        (0..=n).map(|i| {
            let a = (from + (to - from) * i as f64 / n as f64).to_radians();
            Pt::from_mm(c[0] + r * a.cos(), c[1] + r * a.sin())
        }).collect::<Vec<_>>()
    };
    let mut outline = arc(20.1, a0, a1, 0.1);
    outline.extend(arc(3.9, a1, a0, 0.1));
    let ceiling = layer(1, vec![ExPolygon::new(Polygon::new(outline))]);
    let bridge = |seed: u64| {
        let moved = |pts: Vec<Pt>| pts.into_iter().enumerate().map(|(i, p)| {
            let h = (i as u64 * 0x9e37_79b9 + seed * 0x7f4a_7c15) >> 16;
            if seed == 0 { p } else { Pt::new(p.x + (h % 3) as i64 - 1, p.y + (h / 3 % 3) as i64 - 1) }
        }).collect();
        let mut under = layer(0, Vec::new());
        under.objects[0].islands.push(std::sync::Arc::new(Island { walls: Vec::new(), fills: vec![
            support_line(Role::Support, moved(arc(4.0, a0 - 3.0, a1 + 3.0, 0.05))),
            support_line(Role::Support, moved(arc(20.0, a0 - 3.0, a1 + 3.0, 0.05))),
        ], ..Default::default() }));
        let r = sliced(&[under, ceiling.clone()]);
        let bridges: Vec<_> = r.issues.iter().filter(|i| i.kind == "bridge").collect();
        assert_eq!(bridges.len(), 1, "seed {seed}: {:?}", r.issues);
        (bridges[0].measure, bridges[0].at_mm, bridges[0].message.clone())
    };
    let (span, at, words) = bridge(0);
    for seed in 1..=8 {
        let moved = bridge(seed);
        assert!((moved.0 - span).abs() < 1e-4 && (0..2).all(|d| (moved.1[d] - at[d]).abs() < 1e-4) && moved.2 == words, "seed {seed}: {moved:?} against {span} at {at:?}");
    }
}

#[test]
fn mushroom_slab_has_fourteen_mm_cantilever() {
    let disc = |radius: f64| ExPolygon::new(Polygon::new((0..96).map(|i| {
        let a = i as f64 * std::f64::consts::TAU / 96.0;
        Pt::from_mm(120.0 + radius * a.cos(), 120.0 + radius * a.sin())
    }).collect()));
    let r = sliced(&[layer(0, vec![disc(4.0)]), layer(1, vec![disc(18.0)]), layer(2, vec![disc(18.0)])]);
    let issues: Vec<_> = r.issues.iter().filter(|i| i.kind == "cantilever").collect();
    assert_eq!(issues.len(), 1);
    let i = issues[0];
    assert!((i.measure - 14.0).abs() < 0.5, "{i:?}");
    assert!((i.area_mm2 - std::f64::consts::PI * (18.0f64.powi(2) - 4.0f64.powi(2))).abs() < 2.0);
    assert_eq!((i.layer, i.layer_to), (Some(2), Some(2)));
    assert_eq!(i.at_mm[2], 0.4);
    assert!(!r.issues.iter().any(|i| i.kind == "bridge"));
}

#[test]
fn forty_five_degree_chamfer_steps_need_no_cantilever_warning() {
    let layers: Vec<_> = (0..12).map(|i| {
        let d = i as f64 * 0.2;
        layer(i, vec![rect(110.0-d,110.0-d,120.0+d,120.0+d)])
    }).collect();
    let r = sliced(&layers);
    assert!(r.issues.is_empty(), "{:?}", r.issues);
}

#[test]
fn cantilevers_merge_consecutive_layers_and_restart_after_a_gap() {
    let r = sliced(&[
        layer(0, vec![rect(100.0,100.0,104.0,112.0)]),
        layer(1, vec![rect(100.0,100.0,108.0,112.0)]),
        layer(2, vec![rect(100.0,100.0,114.0,112.0)]),
        layer(3, vec![rect(100.0,100.0,114.0,112.0)]),
        layer(4, vec![rect(100.0,100.0,118.0,112.0)]),
    ]);
    let issues: Vec<_> = r.issues.iter().filter(|i| i.kind == "cantilever").collect();
    assert_eq!(issues.len(), 2);
    let first = issues.iter().find(|i| i.layer == Some(2)).unwrap();
    assert_eq!(first.layer_to, Some(3));
    assert!((first.measure - 6.0).abs() < 0.01);
    assert!((first.area_mm2 - 72.0).abs() < 0.01);
    assert_eq!(first.at_mm[2], 0.4, "marker stays on the first layer");
    assert!(issues.iter().any(|i| i.layer == Some(5) && i.layer_to == Some(5)));
}

/// The thin-wall warning says whether a path prints the fin or it is left out.
#[test]
fn a_thin_fin_says_whether_it_prints() {
    let fin = || vec![rect(110.0,110.0,110.25,118.0)];
    let bare = sliced(&(0..2).map(|index| layer(index, fin())).collect::<Vec<_>>());
    let message = |r: &Report| r.issues.iter().find(|i| i.kind == "thin_wall").expect("a thin wall").message.clone();
    assert!(message(&bare).contains("too thin to print"), "{}", message(&bare));
    let line = ExtrusionPath { role: Role::GapFill, pts: vec![Pt::from_mm(110.125, 110.1), Pt::from_mm(110.125, 117.9)], width: 0.34, height: 0.2, ..Default::default() };
    let printed = sliced(&(0..2).map(|index| {
        let mut l = layer(index, fin());
        l.objects[0].islands.push(std::sync::Arc::new(Island { walls: Vec::new(), fills: vec![line.clone()] }));
        l
    }).collect::<Vec<_>>());
    assert!(message(&printed).contains("prints as one line"), "{}", message(&printed));
}

#[test]
fn quarter_millimetre_fin_is_flagged_at_point_four_line() {
    let layers: Vec<_> = (0..4).map(|index| layer(index, vec![rect(110.0,110.0,110.25,118.0)])).collect();
    let r = sliced(&layers);
    let thin: Vec<_> = r.issues.iter().filter(|i| i.kind == "thin_wall").collect();
    assert_eq!(thin.len(), 1, "one fin across consecutive layers is one issue");
    let i = thin[0];
    assert!((i.measure-0.25).abs() < 0.02, "{}", i.measure);
    assert!((i.area_mm2-2.0).abs() < 1e-5);
    assert_eq!(i.layer, Some(1));
    assert_eq!(i.layer_to, Some(4));
    assert!((i.lowest_z-0.2).abs() < 1e-9 && (i.highest_z-0.8).abs() < 1e-9);
}

#[test]
fn opening_slivers_and_plain_box_and_cylinder_are_not_thin_walls() {
    let circle = ExPolygon::new(Polygon::new((0..64).map(|i| {
        let angle = std::f64::consts::TAU * i as f64 / 64.0;
        Pt::from_mm(150.0 + 10.0 * angle.cos(), 150.0 + 10.0 * angle.sin())
    }).collect()));
    let r = sliced(&[layer(0, vec![
        rect(100.0,100.0,124.0,124.0), circle,
        // One sub-floor area and one long sliver above the area floor,
        // both narrower than a meaningful extrusion feature.
        rect(180.0,100.0,180.08,100.08),
        rect(190.0,100.0,190.005,108.0),
    ])]);
    assert!(!r.issues.iter().any(|i| i.kind == "thin_wall"), "{:?}", r.issues);
}

#[test]
fn thin_runs_end_at_empty_model_layers_and_remain_per_object() {
    let mut p = plate();
    p.parts.push(part("other", box_mesh([150.0,100.0,0.0], [10.0;3])));
    let m = Machine::dual_nozzle_350();
    let s = Materials::default();
    let mut layers: Vec<_> = (0..4).map(|index| {
        let mut l = layer(index, if index == 2 { Vec::new() } else { vec![rect(110.0,110.0,110.25,118.0)] });
        l.objects.push(ObjectLayer { object: 1, slices: vec![rect(151.0,101.0,151.25,109.0)].into(), ..Default::default() });
        l
    }).collect();
    // A support-only plane between the first two model layers is not a gap.
    for l in layers.iter_mut().skip(1) { l.index += 1; }
    layers.insert(1, Layer { index: 1, print_z: 0.3, height: 0.1,
        objects: vec![ObjectLayer { object: 0, support_only: true, ..Default::default() }] });
    let mut r = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, &layers, &Timeline::default(), &mut r);
    let ranges = |object: &str| r.issues.iter().filter(|i| i.kind == "thin_wall" && i.object.as_deref() == Some(object))
        .map(|i| (i.layer.unwrap(), i.layer_to.unwrap())).collect::<Vec<_>>();
    assert_eq!(ranges("model"), vec![(1,3), (5,5)]);
    assert_eq!(ranges("other"), vec![(1,5)]);
}

#[test]
fn a_floor_starting_over_the_bed_is_in_the_air() {
    let floors = |r: &Report| r.issues.iter().filter(|i| i.kind == "floor_in_air").map(|i| (i.severity, i.layer.unwrap_or(0))).collect::<Vec<_>>();
    let pads = || vec![rect(100.0,100.0,104.0,104.0), rect(136.0,100.0,140.0,104.0)];
    let floor = || vec![rect(100.0,100.0,140.0,120.0)];
    // Two pads on the bed for two layers, then the floor: flagged where it starts, and only there.
    let r = sliced(&[layer(0, pads()), layer(1, pads()), layer(2, floor()), layer(3, floor())]);
    assert_eq!(floors(&r).len(), 1, "{:?}", r.issues);
    assert_eq!(floors(&r)[0].0, Severity::Error);
    assert!(r.issues.iter().any(|i| i.kind == "floor_in_air" && i.message.contains("0.40 mm above the bed")), "{:?}", r.issues);
    // The floor on the bed, and a floor over a tall gap (a bridge's and a cantilever's to say).
    assert!(floors(&sliced(&[layer(0, floor()), layer(1, floor()), layer(2, floor())])).is_empty());
    let mut tall: Vec<Layer> = (0..6).map(|k| layer(k, pads())).collect();
    tall.push(layer(6, floor()));
    assert!(floors(&sliced(&tall)).is_empty());
    // Layers growing as a sphere's on the bed (area in proportion to height, sliced mid-layer: 1,
    // 3, 5...) and a cone's on its tip, too small to matter while it is near the bed.
    let square = |area: f64| rect(100.0, 100.0, 100.0 + area.sqrt(), 100.0 + area.sqrt());
    let sphere: Vec<Layer> = (0..5).map(|k| layer(k, vec![square(60.0 * (2 * k + 1) as f64)])).collect();
    assert!(floors(&sliced(&sphere)).is_empty(), "{:?}", sliced(&sphere).issues);
    let cone: Vec<Layer> = (0..5).map(|k| layer(k, vec![square(0.3 * ((2 * k + 1) as f64).powi(2))])).collect();
    assert!(floors(&sliced(&cone)).is_empty());
}

#[test]
fn floating_island_and_one_ended_cantilever_are_not_bridges() {
    let r = sliced(&[layer(0, vec![]), layer(1, vec![rect(110.0,110.0,120.0,120.0)])]);
    let island = r.issues.iter().find(|i| i.kind == "mid_air" && i.severity == Severity::Error).unwrap();
    assert_eq!(island.footprint, Footprint { min: [110.0, 110.0], max: [120.0, 120.0] }, "island bounds translated from the part frame");
    assert_eq!(island.layer, Some(2));
    assert!(!r.issues.iter().any(|i| i.kind == "bridge"));
    assert!(!r.issues.iter().any(|i| i.kind == "cantilever"));
    let r = sliced(&[layer(0, vec![rect(100.0,100.0,104.0,112.0)]), layer(1, vec![rect(100.0,100.0,128.0,112.0)])]);
    assert!(!r.issues.iter().any(|i| i.kind == "bridge"));
    assert!(r.issues.iter().any(|i| i.kind == "cantilever" && (i.measure - 24.0).abs() < 0.01));
}

#[test]
fn floating_block_keeps_its_printer_z_through_toolpath_generation() {
    use makepad_fab_settings::{machine::Job, profiles::{JobSetup, Preset}};
    use makepad_fab_slicer::{mesh::Mesh, source::MeshSource};
    use makepad_fab_toolpath::{generate, ObjectInput};
    let setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    let p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("floating", box_mesh([100.0,100.0,4.0], [4.0,4.0,2.0]))] };
    let mesh = &p.parts[0].mesh;
    let input = ObjectInput { overrides: Vec::new(), name: "floating".into(), filament: 0, source: Box::new(MeshSource::new(Mesh { verts: mesh.positions.clone(), tris: mesh.triangles.clone(), paint: Vec::new() })) };
    let paths = generate(&[input], &Job::new(setup.build_config()));
    let mut r = analyse_plate(&Machine::dual_nozzle_350(), &p, &Materials::default());
    analyse_slice(&Machine::dual_nozzle_350(), &p, &Materials::default(), &paths.layers, &Timeline::default(), &mut r);
    let issue = r.issues.iter().find(|i| i.kind == "mid_air" && i.layer.is_some()).unwrap();
    assert!(issue.at_mm[2] > 4.0 && issue.at_mm[2] < 4.5, "{:?}", issue.at_mm);
}

#[test]
fn support_contact_prevents_mid_air_and_uncovered_warnings() {
    use makepad_fab_toolpath::{ExtrusionPath, Island};
    let s = Materials { supports: true, ..Default::default() };
    let mut lower = layer(0, Vec::new());
    lower.objects[0].islands.push(std::sync::Arc::new(Island { walls: Vec::new(), fills: vec![ExtrusionPath {
        role: Role::SupportInterface, pts: vec![Pt::from_mm(105.0,105.0), Pt::from_mm(115.0,105.0)],
        closed: false, loop_pts: Vec::new(), width: 10.0, height: 0.2, area: 2.0, speed: 30.0, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0,
    }], ..Default::default() }));
    let upper = layer(1, vec![rect(100.0,100.0,120.0,110.0)]);
    let (m,p) = (Machine::dual_nozzle_350(), plate());
    let mut r = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, &[lower,upper.clone()], &Timeline::default(), &mut r);
    assert!(!r.issues.iter().any(|i| matches!(i.kind, "mid_air" | "uncovered_overhang")), "{:?}", r.issues.iter().map(|i| (i.kind, i.measure, i.area_mm2, i.layer)).collect::<Vec<_>>());
    let mut r = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, &[layer(0, Vec::new()),upper], &Timeline::default(), &mut r);
    assert!(r.issues.iter().any(|i| i.kind == "uncovered_overhang" && i.area_mm2 > 20.0));
}

/// What a part keeps its trees out from under, its plan's keep-out on the layer under, is never
/// told as uncovered overhang: the plan tells it, as the person's choice. What lies past it
/// still is, and only that.
#[test]
fn kept_out_overhang_is_not_told_as_uncovered() {
    let s = Materials { supports: true, ..Default::default() };
    let (m, p) = (Machine::dual_nozzle_350(), plate());
    // A post, and on it a shelf 40 by 10 mm from the post's corner: 382.36 mm² of it overhangs,
    // past the step a layer may take out over the post's two sides under the shelf (0.2 mm at 45
    // degrees, mitred round the post's corner).
    let layers = [layer(0, vec![rect(100.0,100.0,104.0,104.0)]), layer(1, vec![rect(100.0,100.0,140.0,110.0)])];
    let uncovered = |keep: Vec<ExPolygon>| {
        let kept = KeptOut { regions: std::sync::Arc::new(vec![keep, Vec::new()]), zs: layers.iter().map(|l| l.print_z).collect(), offset: Pt::default() };
        let mut r = analyse_plate(&m, &p, &s);
        let timeline = Timeline::default();
        assert!(analyse_slice_memo(&m, &p, &s, &layers, &timeline, &mut timeline.moves().copied(), &mut r, &mut |_, _, _| true, &|| false, &[1], &SliceMemo::default(), &[Some(kept)]));
        r.issues.iter().filter(|i| i.kind == "uncovered_overhang").map(|i| i.area_mm2).collect::<Vec<f64>>()
    };
    let none = uncovered(Vec::new());
    assert!(none.len() == 1 && (none[0] - (400.0 - (16.0 + 2.0 * 4.0 * 0.2 + 0.2 * 0.2))).abs() < 0.01, "nothing kept out: {none:?}");
    assert!(uncovered(vec![rect(90.0,90.0,150.0,120.0)]).is_empty(), "all of it kept out");
    let half = uncovered(vec![rect(90.0,90.0,120.0,120.0)]);
    assert!(half.len() == 1 && (half[0] - 200.0).abs() < 1.0, "the half past the keep-out: {half:?}");
}

/// A layer may step out over the one under it by its height times the tangent of the overhang
/// angle with nothing under it, and the planner asks its supports to hold only what reaches past
/// that: a block whose walls lean out steeper than the threshold, 0.15 mm a layer round a 40 mm
/// block (24 mm² a layer), has no uncovered overhang. A ledge a millimetre out has, all of it but
/// the step.
#[test]
fn a_wall_leaning_out_steeper_than_the_threshold_is_no_uncovered_overhang() {
    let s = Materials { supports: true, ..Default::default() };
    let (m, p) = (Machine::dual_nozzle_350(), plate());
    let out = |d: f64| rect(100.0 - d, 100.0 - d, 140.0 + d, 140.0 + d);
    let uncovered = |layers: &[Layer]| {
        let mut r = analyse_plate(&m, &p, &s);
        analyse_slice(&m, &p, &s, layers, &Timeline::default(), &mut r);
        r.issues.iter().filter(|i| i.kind == "uncovered_overhang").map(|i| (i.layer, i.area_mm2)).collect::<Vec<_>>()
    };
    let leaning: Vec<Layer> = (0..4).map(|i| layer(i, vec![out(0.15 * i as f64)])).collect();
    assert!(uncovered(&leaning).is_empty(), "a wall leaning out 0.15 mm a layer: {:?}", uncovered(&leaning));
    let told = uncovered(&[layer(0, vec![out(0.0)]), layer(1, vec![out(1.0)])]);
    let past = out(1.0).area_mm2() - (out(0.0).area_mm2() + 4.0 * 40.0 * 0.2 + 4.0 * 0.2 * 0.2);
    assert!(told.len() == 1 && (told[0].1 - past).abs() < 0.01, "a ledge a millimetre out, {past:.1} mm² past the step: {told:?}");
}

#[test]
fn full_circle_checks_interior_extrema_not_just_its_endpoints() {
    use makepad_fab_gcode::record::flags;
    let (m,p,s) = (Machine::dual_nozzle_350(), plate(), Materials::default());
    let circle = MoveRecord { kind: MoveKind::Extrude, role: Role::OuterWall,
        p0: [320.0,100.0,1.0], p1: [320.0,100.0,1.0], center: [340.0,100.0], sweep: std::f32::consts::TAU,
        flags: flags::ARC, e: 1.0, width: 0.4, height: 0.2, filament: 1, nozzle: 1, object: 0, layer: 5, z_layer: 1.0, ..Default::default() };
    let mut r = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, &[], &Timeline { parts: vec![vec![circle]], ..Default::default() }, &mut r);
    assert!(r.issues.iter().any(|i| i.kind == "outside_bed" && i.layer == Some(5)));
    assert!(r.issues.iter().any(|i| i.kind == "reach" && i.layer == Some(5)));
}

#[test]
fn interleaved_support_plane_does_not_erase_previous_model_layer() {
    let mut support = Layer { index: 1, print_z: 0.3, height: 0.1, ..Default::default() };
    support.objects.push(ObjectLayer { object: 0, support_only: true, ..Default::default() });
    let mut top = layer(2, vec![rect(100.0,100.0,110.0,110.0)]);
    top.print_z = 0.4;
    let r = sliced(&[layer(0, vec![rect(100.0,100.0,110.0,110.0)]),support,top]);
    assert!(!r.issues.iter().any(|i| matches!(i.kind, "mid_air" | "bridge")));
}

#[test]
fn group_box_crossing_a_keep_out_is_only_a_candidate() {
    let (mut m,p,s) = (Machine::dual_nozzle_350(), plate(), Materials::default());
    m.keep_out = vec![fab_plate::machine::Rect::new(110.0,110.0,120.0,120.0)];
    let movement = MoveRecord { kind: MoveKind::Extrude, role: Role::OuterWall,
        p0: [100.0,100.0,0.2], p1: [130.0,100.0,0.2], width: 0.4, height: 0.2, e: 1.0, filament: 1, object: 0, layer: 1, z_layer: 0.2, ..Default::default() };
    let around = Timeline { parts: vec![vec![movement, MoveRecord { p0: [100.0,130.0,0.2], p1: [130.0,130.0,0.2], ..movement }]], ..Default::default() };
    let mut r = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, &[], &around, &mut r);
    assert!(!r.issues.iter().any(|i| i.kind == "outside_bed" && i.layer.is_some()));
    let across = Timeline { parts: vec![vec![MoveRecord { p0: [100.0,115.0,0.2], p1: [130.0,115.0,0.2], ..movement }]], ..Default::default() };
    let mut r = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, &[], &across, &mut r);
    let i = r.issues.iter().find(|i| i.kind == "outside_bed" && i.layer.is_some()).unwrap();
    assert!((i.at_mm[0]-115.0).abs() < 0.01);
}

#[test]
fn left_only_filament_requires_left_reach_and_height() {
    let m = Machine::dual_nozzle_350();
    let s = Materials { filaments: vec![Filament { printable: 1, nozzle: None, ..Default::default() }], ..Default::default() };
    let p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("right_strip", box_mesh([330.0,100.0,0.0], [10.0;3]))] };
    let r = analyse_plate(&m, &p, &s);
    assert!(r.issues.iter().any(|i| i.kind == "reach" && i.flags & 1 != 0));
    let p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("tall", box_mesh([100.0,100.0,0.0], [10.0,10.0,322.0]))] };
    assert!(analyse_plate(&m, &p, &s).issues.iter().any(|i| i.kind == "height" && i.flags & 2 != 0));
}

#[test]
fn a_resin_printer_checks_where_parts_stand_not_filaments() {
    use fab_plate::machine::ResinScreen;
    let screen = ResinScreen { pixels: [2000, 1000], pitch: [0.05, 0.05], mirror: [false; 2], quarter_turns: 0, grey_levels: 256 };
    let m = Machine::resin("resin", screen, 150.0);
    // no nozzles and no filament: nothing to say about either
    let mut p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("model", box_mesh([20.0,20.0,0.0], [10.0;3]))] };
    p.parts[0].filament = 0;
    let r = analyse_plate(&m, &p, &Materials::default());
    let filament = ["filament_nozzle", "bed_temperature", "bed_temperature_mix", "temperature_mix", "abrasive", "reach"];
    assert!(!r.issues.iter().any(|i| filament.contains(&i.kind)), "{:?}", r.issues);
    // off the 100 x 50 mm screen, or taller than the printer: those it says
    p.parts.push(part("off", box_mesh([95.0,20.0,0.0], [10.0;3])));
    p.parts.push(part("tall", box_mesh([40.0,20.0,0.0], [10.0,10.0,160.0])));
    let r = analyse_plate(&m, &p, &Materials::default());
    assert!(r.issues.iter().any(|i| i.kind == "outside_bed" && i.object.as_deref() == Some("off")), "{:?}", r.issues);
    assert!(r.issues.iter().any(|i| i.kind == "height" && i.object.as_deref() == Some("tall")), "{:?}", r.issues);
}

#[test]
fn tpu_on_left_temperature_mix_bed_and_abrasive() {
    let m = Machine::dual_nozzle_350();
    let mut p = plate();
    p.parts[0].nozzle = Some(0);
    let s = Materials { filaments: vec![Filament { kind: "TPU".into(), printable: 2, nozzle: Some(0), ..Default::default() }], ..Default::default() };
    assert!(analyse_plate(&m, &p, &s).issues.iter().any(|i| i.kind == "filament_nozzle"));
    let mut other = part("other", box_mesh([150.0,100.0,0.0], [10.0;3]));
    other.filament = 2; p.parts.push(other);
    let mut s = Materials { filaments: vec![Filament::default(), Filament { kind: "PC-CF".into(), nozzle: Some(1), bed_temperature: 0.0, ..Default::default() }], ..Default::default() };
    let r = analyse_plate(&m, &p, &s);
    assert!(r.issues.iter().any(|i| i.kind == "temperature_mix" && i.severity == Severity::Error));
    assert!(r.issues.iter().any(|i| i.kind == "abrasive"));
    assert!(r.issues.iter().any(|i| i.kind == "bed_temperature"));
    s.filaments[1].kind = "PETG".into();
    assert!(analyse_plate(&m, &p, &s).issues.iter().any(|i| i.kind == "temperature_mix" && i.severity == Severity::Warning));
}

#[test]
fn filaments_that_cannot_share_the_bed_are_an_error() {
    let fil = |kind: &str, bed: f64| Filament { kind: kind.into(), bed_temperature: bed, first_bed_temperature: bed, ..Default::default() };
    let s = Materials { filaments: vec![fil("PLA", 55.0), fil("ABS", 90.0), fil("PETG", 70.0)], ..Default::default() };
    let why = s.bed_conflict(&[1, 2]).expect("PLA on an ABS bed");
    assert!(why.contains("filament 1 (PLA") && why.contains("90 °C") && why.contains("separate plates"), "{why}");
    // 15 °C apart still share the bed.
    assert!(s.bed_conflict(&[1, 3]).is_none());
    assert!(s.bed_conflict(&[2, 2]).is_none());
    let m = Machine::dual_nozzle_350();
    let mut p = plate();
    let mut other = part("other", box_mesh([150.0,100.0,0.0], [10.0;3]));
    other.filament = 2; p.parts.push(other);
    assert!(analyse_plate(&m, &p, &s).issues.iter().any(|i| i.kind == "bed_temperature_mix" && i.severity == Severity::Error));
}

#[test]
fn actual_extrusions_check_reach_empty_layers_flow_and_estimates() {
    let p = plate(); let m = Machine::dual_nozzle_350();
    let s = Materials { filaments: vec![Filament { max_flow: 1.0, cost_per_kg: 20.0, ..Default::default() }], ..Default::default() };
    let movement = MoveRecord { kind: MoveKind::Extrude, role: Role::OuterWall, p0: [330.0,110.0,0.2], p1: [340.0,110.0,0.2], e: 2.0, width: 0.4, height: 0.2, v_cruise: 100.0, filament: 1, object: 0, layer: 1, z_layer: 0.2, ..Default::default() };
    let timeline = Timeline { parts: vec![vec![movement, MoveRecord { layer: 5, z_layer: 1.0, p0: [110.0,110.0,1.0], p1: [120.0,110.0,1.0], ..movement }]], filament_mm: vec![5.0], total: 100.0, tool_changes: 0, ..Default::default() };
    let mut r = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, &[], &timeline, &mut r);
    assert!(r.issues.iter().any(|i| i.kind == "reach" && i.layer == Some(1)));
    assert!(r.issues.iter().any(|i| i.kind == "empty_layer"));
    assert!(r.issues.iter().any(|i| i.kind == "flow"));
    let plate = r.plate.as_ref().expect("a G-code's timeline has its figures");
    assert_eq!(plate.time_s, 100.0);
    assert_eq!(plate.filaments[0].length_mm, 5.0);
    assert!((plate.flush_mass_g/plate.mass_g-0.2).abs() < 1e-6);
    assert!((plate.cost-plate.mass_g*0.02).abs() < 1e-9);
    // The same records without the G-code's filament (live checks on toolpaths): no figures,
    // the checks the same.
    let mut live = analyse_plate(&m, &p, &s);
    analyse_slice(&m, &p, &s, &[], &Timeline { filament_mm: Vec::new(), ..timeline }, &mut live);
    assert!(live.plate.is_none(), "no 0 g that isn't one");
    assert!(live.issues.iter().any(|i| i.kind == "flow"));
}

/// A part sliced from its field is judged by the field's slopes: a box
/// display mesh whose field is a flat-topped slab hanging off a stem
/// overhangs where the field does, and plain boxes stay clean.
#[test]
fn field_parts_take_their_normals_from_the_gradient() {
    let (m, s) = (Machine::dual_nozzle_350(), Materials::default());
    // The display mesh: a 26 x 26 x 11 box standing on the bed.
    let p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("cap", box_mesh([100.0,100.0,0.0], [26.0,26.0,11.0]))] };
    // The field: a 6 x 6 stem up to z 8 under a 26 x 26 x 3 cap.
    let boxf = |c: [f64; 3], h: [f64; 3], q: [f64; 3]| {
        let d = [(q[0]-c[0]).abs()-h[0], (q[1]-c[1]).abs()-h[1], (q[2]-c[2]).abs()-h[2]];
        let out = (d[0].max(0.0).powi(2)+d[1].max(0.0).powi(2)+d[2].max(0.0).powi(2)).sqrt();
        out + d[0].max(d[1]).max(d[2]).min(0.0)
    };
    let field = move |x: f64, y: f64, z: f64| boxf([113.0,113.0,4.0], [3.0,3.0,4.0], [x,y,z]).min(boxf([113.0,113.0,9.5], [13.0,13.0,1.5], [x,y,z]));
    let mesh_only = analyse_plate(&m, &p, &s);
    assert!(mesh_only.issues.iter().all(|i| i.kind != "overhang"), "a plain box has no overhang");
    let f: &(dyn Fn(f64, f64, f64) -> f64 + Sync) = &field;
    let r = analyse_plate_with(&m, &p, &s, &|_| Some(f), &|| false).expect("nothing stops it");
    // The box's lower side facets sit where the field's nearest surface is
    // the cap's underside: they face down.
    assert!(r.issues.iter().any(|i| i.kind == "overhang"), "the field's cap overhangs");
}

/// Part `object`'s layers moved by `d` on the plate.
fn moved(layers: &[Layer], object: usize, d: Pt) -> Vec<Layer> {
    let mut layers = layers.to_vec();
    for o in layers.iter_mut().flat_map(|l| &mut l.objects).filter(|o| o.object == object) {
        o.translate(d);
    }
    layers
}

/// What a report found, issue by issue: kind, object, layers, measure, and where to 1 µm.
fn found(r: &Report) -> Vec<(&'static str, Option<String>, Option<u32>, Option<u32>, u64, [i64; 3], [[i64; 2]; 2])> {
    r.issues.iter().map(|i| (i.kind, i.object.clone(), i.layer, i.layer_to, i.measure.to_bits(), i.at_mm.map(|v| (v * 1000.0).round() as i64), [i.footprint.min, i.footprint.max].map(|xy| xy.map(|v| (v * 1000.0).round() as i64)))).collect()
}

/// A bridge on "model" and a cantilever shelf on "other", 40 mm apart.
fn two_parts() -> (Plate, Vec<Layer>) {
    let mut p = plate();
    p.parts.push(part("other", box_mesh([150.0,100.0,0.0], [30.0;3])));
    let mut layers = vec![layer(0, vec![rect(100.0,100.0,104.0,112.0), rect(124.0,100.0,128.0,112.0)]), layer(1, vec![rect(100.0,100.0,128.0,112.0)])];
    layers[0].objects.push(ObjectLayer { object: 1, slices: vec![rect(150.0,100.0,154.0,112.0)].into(), ..Default::default() });
    layers[1].objects.push(ObjectLayer { object: 1, slices: vec![rect(150.0,100.0,178.0,112.0)].into(), ..Default::default() });
    (p, layers)
}

fn memo_report(p: &Plate, layers: &[Layer], keys: &[u64], memo: &SliceMemo) -> Report {
    let (m, s) = (Machine::dual_nozzle_350(), Materials::default());
    let mut r = analyse_plate(&m, p, &s);
    let timeline = Timeline::default();
    assert!(analyse_slice_memo(&m, p, &s, layers, &timeline, &mut timeline.moves().copied(), &mut r, &mut |_, _, _| true, &|| false, keys, memo, &[]));
    r
}

fn fresh_report(p: &Plate, layers: &[Layer]) -> Report {
    let (m, s) = (Machine::dual_nozzle_350(), Materials::default());
    let mut r = analyse_plate(&m, p, &s);
    analyse_slice(&m, p, &s, layers, &Timeline::default(), &mut r);
    r
}

#[test]
fn a_moved_part_keeps_its_findings_and_has_them_where_it_stands() {
    let (p, layers) = two_parts();
    let memo = SliceMemo::default();
    let before = memo_report(&p, &layers, &[11, 22], &memo);
    assert_eq!(found(&before), found(&fresh_report(&p, &layers)), "kept findings are what a fresh check finds");
    assert!(before.issues.iter().any(|i| i.kind == "bridge" && i.object.as_deref() == Some("model")));
    assert!(before.issues.iter().any(|i| i.kind == "cantilever" && i.object.as_deref() == Some("other")));
    let held = memo.held();
    assert_eq!(held.len(), 4, "one finding per part and layer");
    // Moved by an odd distance: what it finds is the same, where it stands.
    let d = Pt::from_mm(7.3, -2.1);
    let after = memo_report(&p, &moved(&layers, 1, d), &[11, 22], &memo);
    assert_eq!(memo.held(), held, "nothing checked again: the same findings under the same keys");
    let shifted: Vec<_> = before.issues.iter().map(|i| {
        let mut i = i.clone();
        if i.object.as_deref() == Some("other") { i.at_mm[0] += d.x_mm(); i.at_mm[1] += d.y_mm(); i.footprint = i.footprint.translated([d.x_mm(), d.y_mm()]); }
        i
    }).collect();
    assert_eq!(found(&after), found(&Report { issues: shifted, ..Report::default() }));
    assert_eq!(found(&after), found(&fresh_report(&p, &moved(&layers, 1, d))));
}

/// A stale check stops between part-layers: stale from the start, it checks none and keeps
/// none; else it checks them all.
#[test]
fn a_stale_check_stops_between_part_layers() {
    let (p, layers) = two_parts();
    let (m, s) = (Machine::dual_nozzle_350(), Materials::default());
    let timeline = Timeline::default();
    let run = |stale: &(dyn Fn() -> bool + Sync), memo: &SliceMemo| {
        let mut r = analyse_plate(&m, &p, &s);
        analyse_slice_memo(&m, &p, &s, &layers, &timeline, &mut timeline.moves().copied(), &mut r, &mut |_, _, _| true, stale, &[11, 22], memo, &[])
    };
    let memo = SliceMemo::default();
    assert!(!run(&|| true, &memo), "stale from the start");
    assert!(memo.held().is_empty(), "no part-layer checked: {:?}", memo.held());
    assert!(run(&|| false, &memo), "never stale");
    assert_eq!(memo.held().len(), 4, "every part-layer checked");
}

#[test]
fn a_part_within_reach_is_checked_with_the_one_it_rests_on() {
    // "other" is a slab resting on "model"'s pillar; moved off it, the slab floats.
    let mut p = plate();
    p.parts.push(part("other", box_mesh([100.0,100.0,0.0], [10.0;3])));
    let layers = vec![layer(0, vec![rect(100.0,100.0,104.0,104.0)]),
        Layer { index: 1, print_z: 0.4, height: 0.2, objects: vec![ObjectLayer { object: 1, slices: vec![rect(100.0,100.0,106.0,106.0)].into(), ..Default::default() }] }];
    let memo = SliceMemo::default();
    let resting = memo_report(&p, &layers, &[11, 22], &memo);
    assert!(!resting.issues.iter().any(|i| i.kind == "mid_air"), "{:?}", resting.issues);
    let off = moved(&layers, 1, Pt::from_mm(20.0, 0.0));
    let floating = memo_report(&p, &off, &[11, 22], &memo);
    assert!(floating.issues.iter().any(|i| i.kind == "mid_air" && i.object.as_deref() == Some("other")), "{:?}", floating.issues);
    assert_eq!(found(&floating), found(&fresh_report(&p, &off)));
    // Back on the pillar, the first findings answer again.
    assert_eq!(found(&memo_report(&p, &layers, &[11, 22], &memo)), found(&resting));
}

/// The plate's check takes a field part where the layout rests it: a proof box reaching a little
/// under the plate is no failure; a rest point under the plate, or a box off the bed, is.
#[test]
fn a_field_part_is_outside_the_bed_by_where_it_rests() {
    let (m, s) = (Machine::dual_nozzle_350(), Materials::default());
    let outside = |x: f64, floor: f64| {
        let bounds = fab_plate::Bounds { min: [x, 100.0, -0.03], max: [x + 30.0, 130.0, 20.0] };
        let web = Part { field_bounds: Some(bounds), field_floor: Some(fab_plate::FieldFloor::proven(floor)), ..part("web", PartMesh::default()) };
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![web] };
        analyse_plate(&m, &plate, &s).issues.iter().any(|i| i.kind == "outside_bed")
    };
    assert!(!outside(100.0, 0.0), "its box dips under the plate by what the proof left open");
    assert!(outside(100.0, -0.25), "it rests under the plate");
    assert!(outside(1000.0, 0.0), "it is off the bed");
}

#[test]
fn a_mesh_lists_each_connected_overhang_with_where_it_is_lowest() {
    // Two separate undersides, 4 x 2 mm at z 10 and 3 x 3 mm at z 20, and a bed-side bottom that is no overhang.
    let (a, b, c) = (box_mesh([0.0, 0.0, 10.0], [4.0, 2.0, 1.0]), box_mesh([20.0, 0.0, 20.0], [3.0, 3.0, 1.0]), box_mesh([40.0, 0.0, 0.0], [5.0, 5.0, 1.0]));
    let mut mesh = PartMesh::default();
    for m in [&a, &b, &c] {
        let base = mesh.positions.len() as u32;
        mesh.positions.extend_from_slice(&m.positions);
        mesh.triangles.extend(m.triangles.iter().map(|t| t.map(|v| v + base)));
    }
    let degrees: Vec<u8> = mesh.triangles.iter().map(|t| {
        let [p, q, r] = t.map(|v| mesh.positions[v as usize]);
        let n = (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
        overhang_degree(if n < 0.0 { -1.0 } else { 0.0 }, [p, q, r], 45.0)
    }).collect();
    let mut patches = overhang_patches(&mesh, &degrees);
    patches.sort_by(|a, b| a.lowest[2].total_cmp(&b.lowest[2]));
    assert_eq!(patches.len(), 2, "{:?}", patches.iter().map(|p| p.area_mm2).collect::<Vec<_>>());
    assert!((patches[0].area_mm2 - 8.0).abs() < 1e-9 && patches[0].lowest[2] == 10.0 && patches[0].worst == 5);
    assert!((patches[1].area_mm2 - 9.0).abs() < 1e-9 && patches[1].lowest[2] == 20.0);
    assert_eq!(patches[1].centroid, [21.5, 1.5, 20.0]);
}

/// A rule the support breaks is told once per rule, with where and how much, on the layers it
/// breaks it on.
#[test]
fn a_support_rule_break_is_told_with_its_layers() {
    use makepad_fab_toolpath::support_rules::{Break, Rule};
    let at = |layer: usize, area_mm2: f64, x: f64| Break { rule: Rule::Growth, why: "past the steep angle of the layer under it", layer, area_mm2, at: (x, 5.0), bounds: makepad_fab_slicer::geom::bbox_of(&[rect(x - 1.0, 4.0, x + 1.0, 6.0)]) };
    let mut skeleton = makepad_fab_toolpath::skeleton::Skeleton::default();
    skeleton.breaks = vec![at(12, 0.5, 10.0), at(14, 1.5, 12.0)];
    let heights: Vec<(f64, f64)> = (0..20).map(|l| (0.2 * (l + 1) as f64, 0.2)).collect();
    let issues = support_issues("model", &skeleton, &heights, [100.0, 100.0]);
    let told: Vec<&Issue> = issues.iter().filter(|i| i.kind == "support_rule").collect();
    assert_eq!(told.len(), 1, "one issue per rule: {issues:?}");
    let i = told[0];
    assert!(i.message.contains("R2") && i.message.contains("2 places"), "{}", i.message);
    assert_eq!((i.layer, i.layer_to), (Some(12), Some(14)));
    assert!((i.area_mm2 - 2.0).abs() < 1e-9 && (i.at_mm[0] - 112.0).abs() < 1e-9, "{i:?}");
    assert!(support_issues("model", &makepad_fab_toolpath::skeleton::Skeleton::default(), &heights, [0.0; 2]).is_empty(), "no break, nothing told");
}

/// Given-up overhang is told per region: what one layer leaves beside what the next one leaves is
/// one item with its place, area and layer range; a region elsewhere is an item of its own; and
/// what the part is in the way of, what a support would be locked in under, and the support that
/// is locked in where the part lets it be are each told apart from what no tree can reach, the
/// last two with the window it would come out through.
#[test]
fn each_given_up_region_is_an_item_of_its_own() {
    use makepad_fab_toolpath::skeleton::{GivenUp, Hold, Skeleton};
    let at = |layer: usize, area: f64, x0: f64, y0: f64| GivenUp { layer, area, at: [x0 + 2.0, y0 + 1.0], bounds: makepad_fab_slicer::geom::bbox_of(&[rect(x0, y0, x0 + 4.0, y0 + 2.0)]) };
    let mut skeleton = Skeleton::default();
    // A face going up and out over layers 10..12, and a shelf 50 mm off it on layer 13.
    skeleton.given_up = vec![at(12, 2.0, 11.0, 0.0), at(10, 1.0, 10.0, 0.0), at(13, 5.0, 60.0, 0.0), at(11, 3.0, 10.5, 0.0)];
    // Under the face, with the part in the way.
    skeleton.inside = vec![at(4, 1.5, 10.0, 0.0)];
    // In a cell with a narrow window: left, and elsewhere held but locked in.
    skeleton.locked = vec![at(6, 4.0, 30.0, 0.0)];
    skeleton.unremovable = vec![at(8, 7.0, 80.0, 0.0)];
    skeleton.hold = Some(Hold { need: 1000.0, held: 983.5, bed_only: false, side_gap: 0.35, window: 4.9 });
    let heights: Vec<(f64, f64)> = (0..20).map(|l| (0.2 * (l + 1) as f64, 0.2)).collect();
    let issues = support_issues("model", &skeleton, &heights, [100.0, 100.0]);
    let unreached: Vec<&Issue> = issues.iter().filter(|i| i.kind == "support_unreached").collect();
    assert_eq!(unreached.len(), 2, "the face and the shelf: {issues:?}");
    let (face, shelf) = (unreached[0], unreached[1]);
    assert!((face.area_mm2 - 6.0).abs() < 1e-9 && (shelf.area_mm2 - 5.0).abs() < 1e-9, "the largest first, each its own area: {} {}", face.area_mm2, shelf.area_mm2);
    assert_eq!((face.layer, face.layer_to, shelf.layer, shelf.layer_to), (Some(10), Some(12), Some(13), Some(13)), "layer ranges");
    assert!((face.at_mm[0] - 112.5).abs() < 1e-9 && (face.at_mm[1] - 101.0).abs() < 1e-9 && (face.at_mm[2] - 2.4).abs() < 1e-9, "at its widest layer's place on the plate: {:?}", face.at_mm);
    assert!(face.footprint.min[0] == 110.0 && face.footprint.max[0] == 115.0, "its footprint holds its layers: {:?}", face.footprint);
    assert!(face.message.starts_with("Unsupported overhang: no tree can reach it") && face.message.contains("6.0 mm²") && face.message.contains("layers 11–13"), "{}", face.message);
    let inside: Vec<&Issue> = issues.iter().filter(|i| i.kind == "support_inside").collect();
    assert_eq!(inside.len(), 1, "{issues:?}");
    assert!(inside[0].message.contains("the part is in the way") && inside[0].message.contains("layer 5") && (inside[0].area_mm2 - 1.5).abs() < 1e-9, "{}", inside[0].message);
    let locked: Vec<&Issue> = issues.iter().filter(|i| i.kind == "support_locked").collect();
    assert!(locked.len() == 1 && locked[0].message.contains("could not come out, window 4.9 mm") && locked[0].message.contains("layer 7") && (locked[0].area_mm2 - 4.0).abs() < 1e-9, "{issues:?}");
    assert!(locked[0].hint.contains("Allow supports that can't be removed"), "{}", locked[0].hint);
    let unremovable: Vec<&Issue> = issues.iter().filter(|i| i.kind == "support_unremovable").collect();
    assert!(unremovable.len() == 1 && unremovable[0].message.starts_with("Support held, not removable through a 4.9 mm window") && unremovable[0].severity == Severity::Warning, "{issues:?}");
    assert_eq!(issues.len(), 5, "nothing else is told: {issues:?}");
}

/// Supports that hold under half of a part's overhang say so once, with the share, over all they
/// leave, pointing to supports on the part or a smaller side gap; holding most of it, or with
/// little overhang to hold, they say nothing of it.
#[test]
fn supports_that_hold_little_say_so_with_the_share() {
    use makepad_fab_toolpath::skeleton::{GivenUp, Hold, Skeleton};
    let at = |layer: usize, area: f64, x0: f64| GivenUp { layer, area, at: [x0 + 2.0, 1.0], bounds: makepad_fab_slicer::geom::bbox_of(&[rect(x0, 0.0, x0 + 4.0, 2.0)]) };
    let heights: Vec<(f64, f64)> = (0..20).map(|l| (0.2 * (l + 1) as f64, 0.2)).collect();
    let low = |need: f64, held: f64, bed_only: bool, side_gap: f64, left: Vec<GivenUp>| {
        let mut skeleton = Skeleton::default();
        skeleton.hold = Some(Hold { need, held, bed_only, side_gap, window: 4.9 });
        skeleton.given_up = left;
        support_issues("model", &skeleton, &heights, [100.0, 100.0]).into_iter().filter(|i| i.kind == "support_low_hold").collect::<Vec<Issue>>()
    };
    // 90 of 100 mm² left, in two places: a tenth held.
    let told = low(100.0, 10.0, true, 2.0, vec![at(10, 60.0, 0.0), at(12, 30.0, 50.0)]);
    assert_eq!(told.len(), 1, "once for the part: {told:?}");
    let i = &told[0];
    assert!(i.message == "Supports hold only 10 % of this part's overhang (10 of 100 mm²)", "{}", i.message);
    assert!(i.hint.contains("From the bed alone, 2 mm off") && i.hint.contains("Allow supports on the part") && i.hint.contains("smaller side gap"), "{}", i.hint);
    assert!((i.area_mm2 - 90.0).abs() < 1e-9 && i.footprint.min[0] == 100.0 && i.footprint.max[0] == 154.0, "over all it leaves: {} {:?}", i.area_mm2, i.footprint);
    // The gap as it was set.
    let on_part = low(100.0, 10.0, false, 0.35, vec![at(10, 90.0, 0.0)]);
    assert!(on_part.len() == 1 && on_part[0].hint.contains("Kept 0.35 mm off") && !on_part[0].hint.contains("Allow supports on the part") && on_part[0].hint.contains("smaller side gap"), "{on_part:?}");
    assert!(low(100.0, 60.0, true, 2.0, vec![at(10, 40.0, 0.0)]).is_empty(), "60 % held is not little");
    assert!(low(8.0, 0.0, true, 2.0, vec![at(10, 8.0, 0.0)]).is_empty(), "8 mm² is told on its own");
    assert!(low(100.0, 100.0, true, 2.0, Vec::new()).is_empty(), "all of it held");
}

/// Through the tree planner: a cap over the part's own wide foot, with supports from the bed only,
/// is listed where it is and with its area; the same cap on a bare stem is held and lists nothing.
#[test]
fn an_unreachable_overhang_is_listed_where_it_is_and_a_held_one_is_not() {
    use makepad_fab_settings::{machine::Job, profiles::{JobSetup, Preset}};
    let mut process = Preset::default();
    for (k, v) in [("enable_support", "1"), ("support_type", "tree(auto)"), ("support_on_build_plate_only", "1"), ("support_threshold_angle", "45")] {
        process.values.set_str(k, v);
    }
    let setup = JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    let params = makepad_fab_toolpath::Params::from_job(&Job::new(setup.build_config()), 0).support.expect("tree supports");
    let square = |half: f64| vec![rect(50.0 - half, 50.0 - half, 50.0 + half, 50.0 + half)];
    let heights: Vec<(f64, f64)> = (0..30).map(|l| (0.2 * (l + 1) as f64, 0.2)).collect();
    // A 40 mm cap on a 6 mm stem from layer 20 (z 4 mm); `foot`: the stem stands on a 60 mm foot
    // 2 mm high, so all under the cap is the part's.
    let all = |foot: bool| {
        let slices: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l >= 20 { square(20.0) } else if foot && l < 10 { square(30.0) } else { square(3.0) }).collect();
        let mut skeleton = makepad_fab_toolpath::skeleton::Skeleton::default();
        let ctl = makepad_fab_toolpath::par::Ctl::default();
        let tree = makepad_fab_toolpath::support::generate_recording(ctl, &slices, &heights, &params, &mut skeleton).expect("not cancelled");
        // Held as the toolpaths measure it, on what prints: here the regions, as no paths are made.
        let legal = makepad_fab_toolpath::support_rules::Legal::new(&slices, &heights, &params, &[], &tree.rings);
        let printed = |l: usize, _: &makepad_fab_slicer::geom::BBox| [tree.layers[l].base.clone(), tree.layers[l].interface.clone()].concat();
        skeleton.hold = Some(makepad_fab_toolpath::support_rules::hold(ctl, &legal, &tree.asked, &printed).expect("not cancelled"));
        support_issues("cap", &skeleton, &heights, [100.0, 100.0])
    };
    let told = |foot: bool| all(foot).into_iter().filter(|i| matches!(i.kind, "support_unreached" | "support_inside")).collect::<Vec<Issue>>();
    let low = |foot: bool| all(foot).into_iter().filter(|i| i.kind == "support_low_hold").collect::<Vec<Issue>>();
    // Over the foot the trees hold none of it, and say so once.
    let little = low(true);
    assert_eq!(little.len(), 1, "{:?}", little.iter().map(|i| &i.message).collect::<Vec<_>>());
    assert!(little[0].message.starts_with("Supports hold only ") && little[0].hint.contains("Allow supports on the part"), "{} ({})", little[0].message, little[0].hint);
    assert!(low(false).is_empty(), "on the stem they hold it all");
    let over_foot = told(true);
    assert_eq!(over_foot.len(), 1, "one region: {:?}", over_foot.iter().map(|i| &i.message).collect::<Vec<_>>());
    let cap = &over_foot[0];
    assert!(cap.kind == "support_inside" && cap.message.contains("the part is in the way"), "the foot is in the way of every branch from the bed: {} {}", cap.kind, cap.message);
    assert!((cap.at_mm[0] - 150.0).abs() < 1.0 && (cap.at_mm[1] - 150.0).abs() < 1.0 && (cap.at_mm[2] - 4.2).abs() < 0.5, "under the cap: {:?}", cap.at_mm);
    // The cap's underside less the stem: 40 x 40 - 6 x 6 mm.
    assert!((1400.0..1600.0).contains(&cap.area_mm2), "its area: {} mm2", cap.area_mm2);
    assert!(cap.layer.is_some_and(|l| (18..=20).contains(&l)), "from the cap's first layer: {:?}", (cap.layer, cap.layer_to));
    let on_stem = told(false);
    assert!(on_stem.is_empty(), "trees from the bed hold all of it: {:?}", on_stem.iter().map(|i| &i.message).collect::<Vec<_>>());
}

/// A stop asked true at any of the plate checks' asks ends them with None, never with part of
/// what they found; never true, they tell what [`analyse_plate`] tells.
#[test]
fn a_stop_anywhere_in_the_plate_checks_tells_nothing() {
    // More facets than the checks run through between asks: 1400 cubes, one of them afloat.
    let mut mesh = PartMesh { positions: Vec::new(), triangles: Vec::new() };
    for k in 0..1400 {
        let cube = box_mesh([100.0 + (k % 40) as f64 * 2.0, 100.0 + (k / 40) as f64 * 2.0, if k == 0 { 5.0 } else { 0.0 }], [1.0, 1.0, 1.0]);
        let base = mesh.positions.len() as u32;
        mesh.positions.extend(cube.positions);
        mesh.triangles.extend(cube.triangles.into_iter().map(|t| t.map(|v| v + base)));
    }
    let p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("cubes", mesh)] };
    let (m, s) = (Machine::dual_nozzle_350(), Materials::default());
    let asks = std::cell::Cell::new(0);
    let all = analyse_plate_with(&m, &p, &s, &|_| None, &|| {
        asks.set(asks.get() + 1);
        false
    })
    .expect("never stopped");
    assert_eq!(format!("{:?}", all.issues), format!("{:?}", analyse_plate(&m, &p, &s).issues));
    assert!(all.issues.iter().any(|i| i.kind == "floating_island"), "the cube afloat is checked");
    let n = asks.get();
    assert!(n >= 10, "{n} asks");
    for k in 0..n {
        let asked = std::cell::Cell::new(0);
        let stop = || {
            asked.set(asked.get() + 1);
            asked.get() > k
        };
        assert!(analyse_plate_with(&m, &p, &s, &|_| None, &stop).is_none(), "stopped from ask {k} of {n}");
    }
}

/// Two objects whose bodies meet are an error naming both, whether a surface crosses into the
/// other or one lies inside the other; parts that only touch, boxes that meet round bodies
/// that don't, and bodies of one object are not. An exact surface (a field) is judged by itself.
#[test]
fn objects_whose_bodies_meet_overlap_and_touching_ones_do_not() {
    let at = |x: f64, y: f64, z: f64, s: [f64; 3]| box_mesh([100.0 + x, 100.0 + y, z], s);
    fn overlaps<'f>(parts: Vec<Part>, field: &dyn Fn(usize) -> Option<PartField<'f>>) -> Vec<String> {
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts };
        let r = analyse_plate_with(&Machine::dual_nozzle_350(), &plate, &Materials::default(), field, &|| false).unwrap();
        r.issues.iter().filter(|i| i.kind == "overlap").map(|i| { assert_eq!(i.severity, Severity::Error); i.message.clone() }).collect()
    }
    let none = |_: usize| None;
    let block = || part("Block", at(0.0, 0.0, 0.0, [20.0, 20.0, 10.0]));
    // An L: a wall standing on a base, one closed mesh each, touching.
    let mut l = at(0.0, 0.0, 0.0, [30.0, 30.0, 5.0]);
    let wall = at(0.0, 0.0, 5.0, [30.0, 5.0, 25.0]);
    let n = l.positions.len() as u32;
    l.positions.extend(wall.positions);
    l.triangles.extend(wall.triangles.iter().map(|t| t.map(|v| v + n)));
    assert_eq!(overlaps(vec![block(), part("Peg", at(15.0, 5.0, 0.0, [20.0, 10.0, 10.0]))], &none), ["Block and Peg overlap: their bodies take the same space"], "a side pushed 5 mm into another");
    assert_eq!(overlaps(vec![block(), part("Nut", at(5.0, 5.0, 2.0, [5.0, 5.0, 5.0]))], &none).len(), 1, "a part inside another, no surface crossing");
    assert_eq!(overlaps(vec![part("Large block", at(0.0, 0.0, 0.0, [40.0, 40.0, 20.0])), part("Nested cube", at(18.0, 18.0, 8.0, [4.0, 4.0, 4.0]))], &none).len(), 1, "a fully nested 4 mm cube at least 8 mm from the block's surface overlaps");
    assert_eq!(overlaps(vec![block(), part("Thin plate", at(5.0, 9.85, 2.0, [10.0, 0.3, 6.0]))], &none).len(), 1, "a thin plate wholly inside a block, at least 2 mm from every face, overlaps");
    assert!(overlaps(vec![block(), part("Shallow contact", at(19.98, 5.0, 2.0, [5.0, 5.0, 5.0]))], &none).is_empty(), "a 5 mm block pushed 0.02 mm into the side stays within contact tolerance");
    assert_eq!(overlaps(vec![block(), part("Deeper contact", at(19.7, 5.0, 2.0, [5.0, 5.0, 5.0]))], &none).len(), 1, "a 5 mm block pushed 0.3 mm into the side overlaps");
    assert_eq!(overlaps(vec![block(), part("Fin", at(15.0, 10.0, 0.0, [10.0, 0.3, 10.0]))], &none).len(), 1, "a thin fin pushed 5 mm through the block's side overlaps");
    assert!(overlaps(vec![block(), part("Fin", at(20.0, 10.0, 0.0, [10.0, 0.3, 10.0]))], &none).is_empty(), "a thin fin standing against the block's side only touches");
    assert!(overlaps(vec![block(), part("Lid", at(0.0, 0.0, 10.0, [20.0, 20.0, 2.0])), part("Side", at(20.0, 0.0, 0.0, [5.0, 20.0, 10.0]))], &none).is_empty(), "parts standing on or against another only touch");
    assert!(overlaps(vec![part("L", l.clone()), part("Cube", at(10.0, 5.0, 5.0, [10.0, 10.0, 10.0]))], &none).is_empty(), "a part in an L's corner, on its base and against its wall: the boxes meet, the bodies only touch");
    assert!(overlaps(vec![part("L", l.clone()), part("Clear cube", at(10.0, 6.0, 6.0, [10.0, 10.0, 10.0]))], &none).is_empty(), "a cube 1 mm above the L's base and 1 mm off its wall is disjoint despite meeting boxes");
    let (mut a, mut b) = (block(), part("Pin", at(5.0, 5.0, 0.0, [5.0, 5.0, 15.0])));
    (a.body_of, b.body_of) = (Some("Assembly".into()), Some("Assembly".into()));
    assert!(overlaps(vec![a, b], &none).is_empty(), "bodies of one object print as one");
    // A cord a script bonds through the block: the two are fused by design.
    let (mut a, mut b) = (block(), part("Cord", at(5.0, 5.0, 0.0, [5.0, 5.0, 15.0])));
    (a.fused, b.fused) = (vec![b.object.clone()], vec![a.object.clone()]);
    assert_eq!(overlaps(vec![a, b, part("Peg", at(15.0, 5.0, 0.0, [20.0, 10.0, 10.0]))], &none), ["Block and Peg overlap: their bodies take the same space"], "parts fused by design share space; a part pushed into one of them still overlaps");
    // A ball (radius 6) as an exact surface with no mesh.
    let ball = |c: [f64; 3]| { let mut p = part("Ball", PartMesh { positions: Vec::new(), triangles: Vec::new() }); p.field_bounds = Some(fab_plate::Bounds { min: c.map(|v| v - 6.0), max: c.map(|v| v + 6.0) }); p };
    let field = |c: [f64; 3]| move |x: f64, y: f64, z: f64| ((x - c[0]).powi(2) + (y - c[1]).powi(2) + (z - c[2]).powi(2)).sqrt() - 6.0;
    let (sunk, cornered) = ([120.0, 110.0, 8.0], [117.0, 117.0, 13.0]);
    let (f1, f2) = (field(sunk), field(cornered));
    assert_eq!(overlaps(vec![block(), ball(sunk)], &|i| (i == 1).then_some(&f1 as PartField<'_>)), ["Block and Ball overlap: their bodies take the same space"], "a ball sunk into a block's corner");
    assert!(overlaps(vec![part("L", l), ball(cornered)], &|i| (i == 1).then_some(&f2 as PartField<'_>)).is_empty(), "a ball in an L's corner keeps off it");
    assert!(overlaps(vec![block(), ball(sunk)], &none).is_empty(), "a part with no mesh and no field is not judged");
}

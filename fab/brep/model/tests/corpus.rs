//! Exact-kernel parts through the tessellator: watertight meshes, guarded errors, edge convexity.
use makepad_csg_brep_model::ops::{self, Body};
use makepad_csg_brep_model::query;
use makepad_csg_brep_model::{guard, new_kernel, stack_size, tessellate, BrepError, CancelToken, TessOptions};
use makepad_csg_cad::{Shape, Sketch};

/// Signed volume and open-edge count of a mesh.
fn mesh_check(m: &makepad_csg_mesh::mesh::TriMesh) -> (f64, usize) {
    let mut vol = 0.0;
    let mut edges = std::collections::HashMap::<(u32, u32), i32>::new();
    for t in &m.triangles {
        let (a, b, c) = (m.vertices[t[0] as usize], m.vertices[t[1] as usize], m.vertices[t[2] as usize]);
        vol += a.x * (b.y * c.z - b.z * c.y) + a.y * (b.z * c.x - b.x * c.z) + a.z * (b.x * c.y - b.y * c.x);
        for (x, y) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let (lo, hi, s) = if x < y { (x, y, 1) } else { (y, x, -1) };
            *edges.entry((lo, hi)).or_default() += s;
        }
    }
    (vol / 6.0, edges.values().filter(|v| **v != 0).count())
}

/// Within the guard's margin: no stack to spare at all.
const STACK_MARGIN_FOR_TEST: usize = 1 << 20;

/// Watertight between faces: every primitive at both tolerances, and the mesh's volume
/// agrees with the exact one.
#[test]
fn primitives_are_watertight() {
    let handle = std::thread::Builder::new().stack_size(stack_size()).spawn(|| {
        guard::guarded(stack_size(), &CancelToken::new(), None, || {
            let mut k = new_kernel();
            let parts: Vec<(&str, Body)> = vec![
                ("box", ops::make_box(&mut k, [10.0, 20.0, 30.0])?),
                ("cylinder", ops::make_cylinder(&mut k, 5.0, 20.0)?),
                ("cone", ops::make_cone(&mut k, 5.0, 2.0, 20.0)?),
                ("sphere", ops::make_sphere(&mut k, 10.0)?),
                ("torus", ops::make_torus(&mut k, 20.0, 5.0)?),
            ];
            for (name, mut b) in parts {
                let measured = query::measure(&k, &mut b)?;
                let exact = measured.volume;
                for chord in [0.1, 0.01] {
                    let t = tessellate(&k, &mut b, &TessOptions { chord, ..TessOptions::default() })?;
                    let (vol, open) = mesh_check(&t.mesh);
                    assert_eq!(open, 0, "{name} at {chord}: open edges");
                    assert!(vol > 0.0, "{name} at {chord}: inside out ({vol})");
                    // Within the chord, over the whole surface.
                    assert!((vol - exact).abs() <= measured.area * chord, "{name} at {chord}: mesh {vol} exact {exact}");
                }
            }
            Ok(())
        })
    }).unwrap();
    handle.join().unwrap().unwrap();
}

/// Past its stack budget a kernel call comes back as an error, and it can be cancelled.
#[test]
fn stack_limit_and_cancel_are_errors() {
    let handle = std::thread::Builder::new().stack_size(stack_size()).spawn(|| {
        // A budget the very first probe is past: as if the kernel had recursed that deep.
        let deep = guard::guarded(STACK_MARGIN_FOR_TEST, &CancelToken::new(), None, || ops::make_box(&mut new_kernel(), [10.0, 10.0, 10.0]).map(|_| ()));
        assert!(matches!(deep, Err(BrepError::Internal(_))), "{deep:?}");
        let cancel = CancelToken::new();
        cancel.cancel();
        let cancelled = guard::guarded(stack_size(), &cancel, None, || ops::make_box(&mut new_kernel(), [10.0, 10.0, 10.0]).map(|_| ()));
        assert_eq!(cancelled, Err(BrepError::Cancelled));
        // The thread goes on working.
        let again = guard::guarded(stack_size(), &CancelToken::new(), None, || ops::make_box(&mut new_kernel(), [10.0, 10.0, 10.0]).map(|_| ()));
        assert_eq!(again, Ok(()));
    }).unwrap();
    handle.join().unwrap();
}

/// An L section has one concave edge, and a fillet of it or of the convex ones works.
#[test]
fn edges_know_concave_from_convex() {
    let handle = std::thread::Builder::new().stack_size(stack_size()).spawn(|| {
        guard::guarded(stack_size(), &CancelToken::new(), None, || {
            let s = Sketch::new(vec![Shape::Polygon { points: vec![[-20.0, 0.0], [20.0, 0.0], [20.0, 6.0], [-14.0, 6.0], [-14.0, 46.0], [-20.0, 46.0]], fillet: 0.0 }]);
            let frame = makepad_csg_brep_model::profile::Frame { origin: [-30.0, 0.0, 0.0], u: [0.0, 0.0, 1.0], v: [0.0, 1.0, 0.0] };
            let mut k = new_kernel();
            let mut body = ops::extrude(&mut k, &s, &frame, [1.0, 0.0, 0.0], 60.0)?;
            let edges = query::edges(&k, &mut body)?;
            let concave: Vec<_> = edges.iter().filter(|e| !e.convex).collect();
            assert_eq!(concave.len(), 1, "{edges:?}");
            assert!((concave[0].mid[1] - 6.0).abs() < 1e-9 && (concave[0].mid[2] + 14.0).abs() < 1e-9);
            ops::fillet(&mut k, &mut body, &[concave[0].id], 2.0)?;
            ops::check_solid(&body, "fillet")?;
            Ok(())
        })
    }).unwrap();
    handle.join().unwrap().unwrap();
}

/// One body per recipe, whatever the engine did before: a part meshed at print detail on an
/// engine that built nothing else, and at display detail on another, is byte for byte the part
/// meshed after a history of its own (another width's part built step by step, shown, measured
/// and listed; the part's own steps shown before it is built, then a drag's coarser mesh), and
/// its exact measure is the same.
#[test]
fn a_body_is_the_same_whatever_was_built_before() {
    use makepad_csg_brep_model::engine::Engine;
    use makepad_csg_graph::edges::{EdgeFilter, EdgeSel};
    use makepad_csg_graph::{BoolMode, BrepEngine, BrepOp, BrepStep};
    use makepad_csg_mesh::mesh::TriMesh;
    fn key(name: &str) -> [u8; 32] {
        let mut k = [0; 32];
        k[..name.len()].copy_from_slice(name.as_bytes());
        k
    }
    // A block with its top edges rounded and a bore through it: a fillet and a boolean, which draw
    // on the kernel's random numbers.
    let part = |width: f64| {
        let (block, round, bore, cut) = (key(&format!("block {width}")), key(&format!("round {width}")), key(&format!("bore {width}")), key(&format!("cut {width}")));
        vec![
            BrepStep { key: block, op: BrepOp::Box { size: [width, 20.0, 30.0] }, inputs: Vec::new() },
            BrepStep { key: round, op: BrepOp::Fillet { radius: 2.0, edges: EdgeSel { any: vec![EdgeFilter { top: true, ..EdgeFilter::default() }], ..EdgeSel::default() } }, inputs: vec![block] },
            BrepStep { key: bore, op: BrepOp::Cylinder { radius: 5.0, top_radius: 5.0, height: 60.0 }, inputs: Vec::new() },
            BrepStep { key: cut, op: BrepOp::Boolean { mode: BoolMode::Difference }, inputs: vec![round, bore] },
        ]
    };
    let bits = |m: &TriMesh| {
        let words = |v: &[makepad_csg_math::Vec3d]| v.iter().flat_map(|p| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]).collect::<Vec<u64>>();
        (words(&m.vertices), m.triangles.clone(), words(&m.corner_normals))
    };
    let part_40 = part(40.0);
    let print = Engine::default().mesh(part_40.clone(), 0.0125, None).expect("the part builds");
    let fresh = Engine::default();
    let display = fresh.mesh(part_40.clone(), 0.05, None).expect("the part builds");
    let measure = fresh.measure(part_40.clone()).expect("the part measures");
    let busy = Engine::default();
    let part_56 = part(56.0);
    for n in 1..=part_56.len() {
        busy.mesh(part_56[..n].to_vec(), 0.05, None).expect("the other part builds");
    }
    busy.measure(part_56.clone()).expect("the other part measures");
    busy.entities(part_56).expect("the other part lists its entities");
    for n in 1..part_40.len() {
        busy.mesh(part_40[..n].to_vec(), 0.05, None).expect("a step builds");
    }
    busy.mesh(part_40.clone(), 0.1, None).expect("the part builds");
    assert!(bits(&busy.mesh(part_40.clone(), 0.05, None).unwrap()) == bits(&display), "the display mesh depends on what was built before");
    assert_eq!(busy.measure(part_40.clone()).unwrap(), measure, "the measure depends on what was built before");
    busy.entities(part_40.clone()).expect("the part lists its entities");
    assert!(bits(&busy.mesh(part_40, 0.0125, None).unwrap()) == bits(&print), "the print mesh depends on what was built before");
}

#[test]
fn identity_placements_share_the_body_budget() {
    use makepad_csg_brep_model::engine::Engine;
    use makepad_csg_graph::edges::{EdgeName, EdgeSel, Face, FaceName, Role};
    use makepad_csg_graph::{BrepEngine, BrepOp, BrepStep};

    let engine = Engine::default();
    let (block, placed, round) = ([0; 32], [1; 32], [2; 32]);
    let identity = BrepOp::Place { at: [0.0; 3], rotate: [0.0; 3], scale: [1.0; 3] };
    let face = |f| FaceName { origin: placed, role: Role::Named(f) };
    let edge = EdgeName { a: face(Face::Top), b: face(Face::Front), near: None, chain: false };
    let mut steps = vec![
        BrepStep { key: block, op: BrepOp::Box { size: [10.0; 3] }, inputs: vec![] },
        BrepStep { key: placed, op: identity.clone(), inputs: vec![block] },
        BrepStep { key: round, op: BrepOp::Fillet { radius: 1.0, edges: EdgeSel { named: vec![edge], ..EdgeSel::default() } }, inputs: vec![placed] },
    ];

    // Cross the 64-body budget in keys while building just the box and its fillet.
    let mut previous = round;
    for n in 3..65 {
        let key = [n; 32];
        steps.push(BrepStep { key, op: identity.clone(), inputs: vec![previous] });
        previous = key;
    }
    let rounded = engine.measure(steps.clone()).expect("the fillet on the placement's named edge builds");
    let keys: Vec<_> = steps.iter().map(|s| s.key).collect();
    assert!(engine.built(&keys), "every key for the two bodies must still be kept");
    let original = engine.measure(steps[..1].to_vec()).expect("the box measures");
    let moved = engine.measure(steps[..2].to_vec()).expect("the identity placement measures");
    assert_eq!(original, moved, "the box and its identity placement must have the same measure");
    let one_edge = (1.0 - std::f64::consts::FRAC_PI_4) * 10.0;
    assert!((original.volume - rounded.volume - one_edge).abs() < one_edge * 1e-3, "only the filleted body must lose one edge's volume");
}

#[test]
fn committed_bodies_survive_drafts() {
    use makepad_csg_brep_model::engine::Engine;
    use makepad_csg_graph::{BrepEngine, BrepOp, BrepStep};

    let boxes = |range: std::ops::Range<u8>| -> Vec<BrepStep> {
        range.map(|n| BrepStep { key: [n; 32], op: BrepOp::Box { size: [1.0 + f64::from(n), 1.0, 1.0] }, inputs: vec![] }).collect()
    };
    let engine = Engine::default();
    let recipe = boxes(0..30);
    let committed: Vec<_> = recipe.iter().map(|s| s.key).collect();
    engine.measure(recipe).expect("the committed boxes build");
    engine.keep(committed.clone());
    engine.measure(boxes(30..60)).expect("the first draft builds");
    engine.measure(boxes(60..90)).expect("the second draft builds");
    assert!(engine.built(&committed), "all committed boxes must survive both drafts");
    assert!(!engine.built(&[[30; 32]]), "the oldest draft box must give way first");
    let recent: Vec<_> = (56..90).map(|n| [n; 32]).collect();
    assert!(engine.built(&recent), "the newest 34 draft boxes must fill the remaining slots");

    engine.keep([committed, recent].concat());
    let answer = engine.measure(boxes(90..91)).expect("the draft must answer even with all 64 bodies protected");
    assert!((answer.volume - 91.0).abs() < 1e-6, "the answer must measure the new box");
    assert!(engine.built(&[[90; 32]]), "the current answer must survive eviction");
    assert!(!engine.built(&[[0; 32]]), "the oldest protected box must make room for the answer");

    engine.keep(vec![[1; 32]]);
    engine.measure(boxes(91..92)).expect("a draft after the committed set changes builds");
    assert!(engine.built(&[[1; 32]]) && !engine.built(&[[2; 32]]), "replacing the set must release the previous build's other keys");
}

/// A Background build gives way to a preview sent while it builds: the preview's answer comes
/// first, at the build's next checkpoint, and the build then goes on from the steps it finished
/// to the body it builds alone. An engine holding a body's steps says so at once, whatever it is
/// building meanwhile.
#[test]
fn a_background_build_gives_way_to_a_preview() {
    use makepad_csg_brep_model::engine::Engine;
    use makepad_csg_graph::edges::{EdgeFilter, EdgeSel};
    use makepad_csg_graph::{BrepEngine, BrepOp, BrepStep, Open};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    fn key(name: &str) -> [u8; 32] {
        let mut k = [0; 32];
        k[..name.len()].copy_from_slice(name.as_bytes());
        k
    }
    // A demo's housing: a block, its top edges rounded, hollowed with its bottom open.
    let (block, round, hollow) = (key("demo block"), key("demo round"), key("demo hollow"));
    let demo = vec![
        BrepStep { key: block, op: BrepOp::Box { size: [96.0, 30.0, 96.0] }, inputs: Vec::new() },
        BrepStep { key: round, op: BrepOp::Fillet { radius: 4.0, edges: EdgeSel { any: vec![EdgeFilter { top: true, ..EdgeFilter::default() }], ..EdgeSel::default() } }, inputs: vec![block] },
        BrepStep { key: hollow, op: BrepOp::Shell { thickness: 2.4, open: Open::Bottom }, inputs: vec![round] },
    ];
    let preview = vec![BrepStep { key: key("preview block"), op: BrepOp::Box { size: [20.0, 20.0, 20.0] }, inputs: Vec::new() }];
    let bits = |m: &makepad_csg_mesh::mesh::TriMesh| (m.vertices.iter().flat_map(|p| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()]).collect::<Vec<u64>>(), m.triangles.clone());
    let alone = bits(&Engine::default().mesh(demo.clone(), 0.05, None).expect("the demo builds"));
    let engine = Arc::new(Engine::default());
    assert!(!engine.built(&[block]), "an engine that built nothing holds no body");
    let (done, answers) = channel();
    let background = {
        let (engine, demo, done) = (engine.clone(), demo.clone(), done.clone());
        std::thread::spawn(move || {
            makepad_cpu_budget::set_thread_class(makepad_cpu_budget::BACKGROUND);
            let mesh = engine.mesh(demo, 0.05, None).expect("the demo builds after the preview");
            done.send(("background", Some(mesh))).unwrap();
        })
    };
    // The preview is sent once the build is under way: its first body is in, the others to come.
    while !engine.built(&[block]) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let preview = {
        let engine = engine.clone();
        std::thread::spawn(move || {
            makepad_cpu_budget::set_thread_class(makepad_cpu_budget::INTERACTIVE);
            engine.mesh(preview, 0.05, None).expect("the preview builds");
            done.send(("preview", None)).unwrap();
        })
    };
    let (first, _) = answers.recv().unwrap();
    assert_eq!(first, "preview", "the preview waited for the background build");
    let (second, mesh) = answers.recv().unwrap();
    assert_eq!(second, "background");
    assert!(bits(&mesh.unwrap()) == alone, "the build that gave way made another body than one built alone");
    assert!(engine.built(&[block, round, hollow]), "the engine holds the demo's bodies");
    preview.join().unwrap();
    background.join().unwrap();
}

/// How far a point is from a mesh's surface.
fn nearest(m: &makepad_csg_mesh::mesh::TriMesh, p: makepad_csg_math::Vec3d) -> f64 {
    m.triangles.iter().map(|t| {
        let (a, b, c) = (m.vertices[t[0] as usize], m.vertices[t[1] as usize], m.vertices[t[2] as usize]);
        // The closest point of the triangle: inside it, else on one of its sides.
        let n = (b - a).cross(c - a);
        let side = |u: makepad_csg_math::Vec3d, v: makepad_csg_math::Vec3d| (v - u).cross(p - u).dot(n) >= 0.0;
        if n.length() > 0.0 && side(a, b) && side(b, c) && side(c, a) {
            return ((p - a).dot(n) / n.length()).abs();
        }
        [(a, b), (b, c), (c, a)].iter().map(|&(u, v)| {
            let e = v - u;
            let s = if e.dot(e) > 0.0 { ((p - u).dot(e) / e.dot(e)).clamp(0.0, 1.0) } else { 0.0 };
            (p - (u + e * s)).length()
        }).fold(f64::MAX, f64::min)
    }).fold(f64::MAX, f64::min)
}

/// A sweep runs through every point of its path at that point's size: a tapered lock from six
/// points, and paths of three and two points (a parabola, a line). A straight sweep from size 1
/// to 0.5 is the cone frustum between them.
#[test]
fn a_sweep_runs_through_its_points_at_their_sizes() {
    let handle = std::thread::Builder::new().stack_size(stack_size()).spawn(|| {
        guard::guarded(stack_size(), &CancelToken::new(), None, || {
            let r = 1.5;
            let round = Sketch::new(vec![Shape::Circle { center: [0.0, 0.0], r }]);
            let lock = vec![[0.0, 0.0, 0.0], [2.0, 6.0, 1.0], [6.0, 11.0, 0.0], [9.0, 17.0, -3.0], [8.0, 23.0, -6.0], [4.0, 27.0, -7.0]];
            let paths: [(&str, Vec<[f64; 3]>, Vec<f64>); 3] = [
                ("lock", lock, vec![1.0, 0.9, 0.75, 0.6, 0.4, 0.2]),
                ("three points", vec![[0.0, 0.0, 0.0], [0.0, 20.0, 5.0], [10.0, 40.0, 10.0]], vec![1.2, 1.0, 0.5]),
                ("two points", vec![[0.0, 0.0, 0.0], [0.0, 20.0, 0.0]], Vec::new()),
            ];
            for (name, path, scale) in paths {
                let mut k = new_kernel();
                let mut b = ops::sweep(&mut k, &round, &path, &scale)?;
                let measured = query::measure(&k, &mut b)?;
                let chord = 0.01;
                let t = tessellate(&k, &mut b, &TessOptions { chord, ..TessOptions::default() })?;
                let (vol, open) = mesh_check(&t.mesh);
                assert_eq!(open, 0, "{name}: open edges");
                assert!((vol - measured.volume).abs() <= measured.area * chord, "{name}: mesh {vol} exact {}", measured.volume);
                // The surface's nearest point to each inner path point is that point's radius away
                // (the end points lie on the flat end caps).
                for (i, p) in path.iter().enumerate().take(path.len() - 1).skip(1) {
                    let want = r * scale.get(i).copied().unwrap_or(1.0);
                    let near = nearest(&t.mesh, makepad_csg_math::Vec3d::new(p[0], p[1], p[2]));
                    assert!((near - want).abs() < 0.02 * r, "{name}: point {i} is {near} from the surface, its radius {want}");
                }
            }
            let mut k = new_kernel();
            let mut cone = ops::sweep(&mut k, &round, &[[0.0, 0.0, 0.0], [0.0, 20.0, 0.0]], &[1.0, 0.5])?;
            let frustum = std::f64::consts::PI * 20.0 / 3.0 * (r * r + r * r * 0.5 + r * r * 0.25);
            let volume = query::measure(&k, &mut cone)?.volume;
            assert!((volume - frustum).abs() < 1e-3 * frustum, "the taper holds {volume}, a frustum {frustum}");
            for (path, scale, why) in [
                (vec![[0.0, 0.0, 0.0]], vec![], "one point"),
                (vec![[0.0, 0.0, 0.0], [0.0, 0.0, 0.0]], vec![], "two points in one place"),
                (vec![[0.0, 0.0, 0.0], [0.0, 9.0, 0.0]], vec![1.0], "one size for two points"),
                (vec![[0.0, 0.0, 0.0], [0.0, 9.0, 0.0]], vec![1.0, 0.0], "a size of zero"),
            ] {
                assert!(matches!(ops::sweep(&mut k, &round, &path, &scale), Err(BrepError::Input(_))), "{why} is not a sweep");
            }
            Ok(())
        })
    }).unwrap();
    handle.join().unwrap().unwrap();
}

/// A shell or an inward offset never builds a wall the part has no room for: the kernel trims
/// inner faces that ran past each other into a closed solid of the wrong shape (a 40 x 32 x 20
/// box shelled 15.6 mm came out with 4.4 mm walls one way), so the op refuses in words, with
/// where. Walls that fit still build, on a box, a cylinder and an L-shaped extrude.
#[test]
fn a_shell_never_builds_a_wall_the_part_has_no_room_for() {
    let handle = std::thread::Builder::new().stack_size(stack_size()).spawn(|| {
        guard::guarded(stack_size(), &CancelToken::new(), None, || {
            let mut k = new_kernel();
            let ell = Sketch::new(vec![Shape::Polygon { points: vec![[0.0, 0.0], [40.0, 0.0], [40.0, 6.0], [6.0, 6.0], [6.0, 40.0], [0.0, 40.0]], fillet: 0.0 }]);
            let frame = makepad_csg_brep_model::profile::Frame { origin: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 0.0, -1.0] };
            let pi = std::f64::consts::PI;
            // (part, wall, the volume it leaves, or None: refused, with these words)
            let cases: Vec<(&str, Body, f64, Result<f64, &str>)> = vec![
                ("box", ops::make_box(&mut k, [40.0, 32.0, 20.0])?, 2.0, Ok(40.0 * 32.0 * 20.0 - 36.0 * 30.0 * 16.0)),
                ("box", ops::make_box(&mut k, [40.0, 32.0, 20.0])?, 15.6, Err("a wall of 15.6 mm is thicker than the part allows at [")),
                ("cylinder", ops::make_cylinder(&mut k, 10.0, 20.0)?, 2.0, Ok(pi * 100.0 * 20.0 - pi * 64.0 * 18.0)),
                ("cylinder", ops::make_cylinder(&mut k, 10.0, 20.0)?, 12.0, Err("a wall of 12 mm ")),
                ("L extrude", ops::extrude(&mut k, &ell, &frame, [0.0, 1.0, 0.0], 20.0)?, 2.5, Ok(f64::NAN)),
                ("L extrude", ops::extrude(&mut k, &ell, &frame, [0.0, 1.0, 0.0], 20.0)?, 4.0, Err("a wall of 4 mm ")),
            ];
            for (name, mut b, wall, want) in cases {
                let faces = query::faces(&k, &mut b)?;
                let top: Vec<_> = faces.iter().filter(|f| f.normal.is_some_and(|n| n[1] > 0.9)).map(|f| f.id).collect();
                assert_eq!(top.len(), 1, "{name}: one top face to leave open");
                match (ops::shell(&mut k, &b, wall, &top), want) {
                    (Ok(mut s), Ok(volume)) => {
                        let got = query::measure(&k, &mut s)?.volume;
                        if volume.is_nan() {
                            // the L's walls: its outline's length times the wall, give or take its corners
                            assert!(got > 0.0 && got < query::measure(&k, &mut b)?.volume, "{name} shelled {wall} mm keeps less than it had: {got}");
                        } else {
                            assert!((got - volume).abs() < 1e-3 * volume, "{name} shelled {wall} mm leaves {volume} mm³, built {got}");
                        }
                    }
                    (Err(e), Err(words)) => assert!(e.to_string().contains(words), "{name} shelled {wall} mm is refused in words ({words}...), said: {e}"),
                    (Ok(mut s), Err(words)) => panic!("{name} shelled {wall} mm must be refused ({words}...), built {:?}", query::measure(&k, &mut s)?),
                    (Err(e), Ok(_)) => panic!("{name} shelled {wall} mm must build, refused: {e}"),
                }
            }
            // An inward offset deeper than the part allows is refused the same way.
            let mut b = ops::make_box(&mut k, [40.0, 32.0, 20.0])?;
            let thin = ops::offset(&mut k, &mut b, -4.0).and_then(|mut o| query::measure(&k, &mut o));
            assert!(thin.as_ref().is_ok_and(|m| (m.volume - 32.0 * 24.0 * 12.0).abs() < 1e-2), "a 4 mm inward offset of the box: {thin:?}");
            assert!(ops::offset(&mut k, &mut b, -12.0).is_err(), "a 12 mm inward offset of a 20 mm box must be refused");
            Ok(())
        })
    }).unwrap();
    handle.join().unwrap().unwrap();
}

/// The most common enclosure: a box rounded on every edge, shelled open at the top. Where a round
/// runs tangent onto the open face its inside ends short of the rim, and the shell closes it with a
/// strip up to the rim (the kernel paired the two sides of that edge apart: status 1001). Its walls
/// follow the rounds, the rim tapers as the shape does and passes the crossing check, which still
/// refuses anything else nearer than the wall, beside the rim too; a wall the rounds have no room
/// for is refused in words, with where.
#[test]
fn a_rounded_box_shells_open_with_walls_that_follow_its_rounds() {
    let handle = std::thread::Builder::new().stack_size(stack_size()).spawn(|| {
        guard::guarded(stack_size(), &CancelToken::new(), None, || {
            let mut k = new_kernel();
            // 40 x 32 x 20, +Y up: rounded r 3 on every edge, or on its four top edges only
            let rounded = |k: &mut makepad_csg_brep::kernel::Kernel, top_only: bool| -> Result<(Body, Vec<makepad_csg_brep::topology::FaceId>), BrepError> {
                let mut b = ops::make_box(k, [40.0, 32.0, 20.0])?;
                let edges: Vec<_> = query::edges(k, &mut b)?.into_iter().filter(|e| !top_only || ((e.start[1] - e.end[1]).abs() < 1.0 && e.start[1] > 5.0)).map(|e| e.id).collect();
                ops::fillet(k, &mut b, &edges, 3.0)?;
                let top = query::faces(k, &mut b)?.iter().filter(|f| f.normal.is_some_and(|n| n[1] > 0.9)).map(|f| f.id).collect();
                Ok((b, top))
            };
            let (mut b, top) = rounded(&mut k, false)?;
            let mut s = ops::shell(&mut k, &b, 2.0, &top)?;
            // a box of core a x b x c grown by a ball of r; the inside is the core grown by r - t,
            // up to the rim over the core's top (a x c, t tall)
            let pi = std::f64::consts::PI;
            let (a, h, c) = (34.0, 26.0, 14.0);
            let grown = |r: f64| a * h * c + 2.0 * r * (a * h + h * c + c * a) + pi * r * r * (a + h + c) + 4.0 / 3.0 * pi * r * r * r;
            let volume = query::measure(&k, &mut s)?.volume;
            let want = grown(3.0) - grown(1.0) - a * c * 2.0;
            assert!((volume - want).abs() < 0.05, "a 2 mm wall leaves {want:.3} mm³ of the rounded box, built {volume:.3}");
            // the wall at a round: on a vertical edge's round, halfway round, the inside is 2 mm in
            let q = [17.0 + 0.5f64.sqrt(), 0.0, 7.0 + 0.5f64.sqrt()];
            let fine = TessOptions { chord: 0.01, ..TessOptions::default() };
            let (inside, outside) = (tessellate(&k, &mut s, &fine)?, tessellate(&k, &mut b, &fine)?);
            let on_shell = query::nearest_face(&inside.mesh, &inside.tri_face, q).map_or(f64::INFINITY, |(_, d)| d);
            let wall = query::nearest_face(&outside.mesh, &outside.tri_face, q).map_or(0.0, |(_, d)| d);
            assert!(on_shell < 0.02 && (wall - 2.0).abs() < 0.02, "the inside of the round is the shell's surface ({on_shell:.3} off it), 2 mm from the outside ({wall:.3})");
            // the rim's taper passes, and only it: straight under the rim of a top round (1 mm under
            // it, a 2 mm wall) is the shape's own; beside the rim, about as near, is a crossing
            let (r, open) = rounded(&mut k, true)?;
            let probe = |k: &mut makepad_csg_brep::kernel::Kernel, at: [f64; 3]| -> Result<Option<[f64; 3]>, BrepError> {
                let mut dot = ops::make_box(k, [0.02; 3])?;
                ops::place(k, &mut dot, at, [0.0; 3])?;
                ops::crossed(k, &r, &open, &mut dot, 2.0)
            };
            assert_eq!(probe(&mut k, [0.0, 15.0, 7.0])?, None, "1 mm straight under a round's rim is the rim's taper");
            let beside = probe(&mut k, [0.0, 15.3, 6.0])?;
            assert!(beside.is_some_and(|p| (p[1] - 15.3).abs() < 0.1 && (p[2] - 6.0).abs() < 0.1), "beside the rim, 1.2 mm from the round, is a crossing, found there: {beside:?}");
            // a wall the rounds have no room for: the round says where
            let e = ops::shell(&mut k, &r, 4.0, &open).expect_err("a 4 mm wall on 3 mm rounds is refused");
            let words = e.to_string();
            let at: Vec<f64> = words.split("round at [").nth(1).and_then(|w| w.split(']').next()).map(|w| w.split_whitespace().filter_map(|n| n.parse().ok()).collect()).unwrap_or_default();
            assert!(words.contains("a wall of 4 mm is thicker than the 3 mm round at [") && at.len() == 3 && at[1] > 13.0, "refused in words, at a round by the rim: {words}");
            Ok::<(), BrepError>(())
        })
    }).unwrap();
    handle.join().unwrap().unwrap();
}

/// Shelling a rounded extrusion open at the bottom preserves its analytic corner walls, even
/// just below the top fillet's radius. Fresh kernels agree on the geometry, tolerances and names.
#[test]
fn a_shell_of_smooth_corners_keeps_exact_corner_walls_and_its_derived_volume() {
    let handle = std::thread::Builder::new().stack_size(stack_size()).spawn(|| {
        guard::guarded(stack_size(), &CancelToken::new(), None, || {
            use makepad_csg_brep::kernel::Kernel;
            use makepad_csg_brep::topology::{brep::p1 as bp1, edge, face, vertex};
            use makepad_csg_brep_model::names;
            use makepad_csg_graph::{BrepOp, Plane};
            use std::f64::consts::PI;

            // Each skin has three eight-piece rings (rim, shoulder, roof), with four straight
            // runs and four quarter arcs each. Eight seams join each consecutive pair of rings.
            const VERTICES: usize = 2 * 3 * 8;
            const EDGES: usize = 2 * (3 + 2) * 8;
            const FACES: usize = 2 * (8 + 8 + 1) + 1; // walls, fillets and roof per skin, plus the rim
            const FACE_KINDS: [(&str, usize); 4] = [
                ("plane", 2 * (4 + 1) + 1), // four flat walls and a roof per skin, plus the annular rim
                ("extrusion", 2 * 4),       // the four quarter-arc walls on each skin
                ("cone", 2 * 4),            // cylindrical fillets along the four straight top edges
                ("revolution", 2 * 4),      // a circular fillet revolved around each corner
            ];

            // For either skin the corner centres form a square of side L = 20 - 2*4. Growing
            // that square by radius x gives area A(x) = L² + 4Lx + πx². The bottom prism is
            // 10 - 2 = 8 high on both skins, with corner radius R = 4 outside or 4 - w inside.
            // Each inner corner wall therefore has area (π/2)(4 - w)*8.
            //
            // Above the shoulder the top fillet has radius t = 2 or 2 - w. The roof has
            // corner radius q = R - t = 2. Its footprint contributes t*A(q). Around it the
            // four straight runs sweep a quarter disk of area D = πt²/4 over length 4L.
            // Around the four corners that disk's centroid (4t/(3π) out from the roof edge)
            // makes one full turn. Pappus gives 2π(q + 4t/(3π))*D for their volume.
            // Subtract the inner solid from the outer, both measured up from the open bottom.
            const RUN: f64 = 20.0 - 2.0 * 4.0;
            const STRAIGHT_HEIGHT: f64 = 10.0 - 2.0;
            fn solid_volume(inset: f64) -> f64 {
                let (r, t) = (4.0 - inset, 2.0 - inset);
                let q = r - t;
                let footprint = |x: f64| RUN * RUN + 4.0 * RUN * x + PI * x * x;
                let disk = PI * t * t / 4.0;
                STRAIGHT_HEIGHT * footprint(r) + t * footprint(q)
                    + disk * (4.0 * RUN + 2.0 * PI * (q + 4.0 * t / (3.0 * PI)))
            }

            fn sorted_tolerances<I>(
                b: &Body, ids: Vec<I>, tolerance: fn(&Body, I) -> makepad_csg_brep::Result<f64>,
            ) -> makepad_csg_brep::Result<Vec<u64>> {
                let mut bits: Vec<_> = ids.into_iter().map(|id| tolerance(b, id).map(f64::to_bits))
                    .collect::<makepad_csg_brep::Result<_>>()?;
                bits.sort_unstable();
                Ok(bits)
            }
            let mut setup = new_kernel();
            // 20 x 20, corners r 4, 10 tall (+Y), its top edges rounded r 2
            let sketch = Sketch::new(vec![Shape::Rect { center: [0.0, 0.0], size: [20.0, 20.0], corner: 4.0, chamfer: 0.0 }]);
            let frame = makepad_csg_brep_model::profile::Frame { origin: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 0.0, -1.0] };
            let mut b = ops::extrude(&mut setup, &sketch, &frame, [0.0, 1.0, 0.0], 10.0)?;
            let top_edges = |k: &Kernel, body: &mut Body| -> Result<Vec<_>, BrepError> {
                Ok(query::edges(k, body)?.into_iter().filter(|e| [e.start[1], e.end[1], e.mid[1]].iter().all(|y| (y - 10.0).abs() < 1e-6)).map(|e| e.id).collect())
            };
            // Name the generator and the picked edges on a copy, as the model worker does.
            let mut naming = new_kernel();
            let mut extrusion = [ops::copy_body(&mut naming, &b, 2)?];
            let op = BrepOp::Extrude { sketch, plane: Plane::Xz, height: 10.0, both: false };
            let source_names = [names::classify(&naming, &mut extrusion[0], [1; 32], &op)?];
            let selected = top_edges(&naming, &mut extrusion[0])?;
            let picked = names::picked(&naming, &mut extrusion[0], &source_names[0], &selected)?;
            let top = top_edges(&setup, &mut b)?;
            ops::fillet(&mut setup, &mut b, &top, 2.0)?;
            let open: Vec<_> = query::faces(&setup, &mut b)?.iter().filter(|f| f.normal.is_some_and(|n| n[1] < -0.9)).map(|f| f.id).collect();
            let blend = names::Step { key: [2; 32], offset: 0.0, shell: 0.0, blend: Some((&picked, 2.0, true)) };
            let mut rounded = ops::copy_body(&mut naming, &b, 1)?;
            let rounded_names = [names::carry(&naming, &mut rounded, &mut extrusion, &source_names, &blend)?];
            let shell_names = |shell: &mut Body, wall| {
                let mut k = new_kernel();
                let mut inputs = [ops::copy_body(&mut k, &b, 2)?];
                let step = names::Step { key: [3; 32], offset: 0.0, shell: wall, blend: None };
                names::carry(&k, shell, &mut inputs, &rounded_names, &step)
            };
            struct Snapshot {
                vertex_tol: Vec<u64>,
                edge_tol: Vec<u64>,
                face_tol: Vec<u64>,
                faces: Vec<String>,
                names: Vec<names::Names>,
                volume: f64,
            }

            let build = |wall: f64| -> Result<Snapshot, BrepError> {
                let mut k = new_kernel();
                let mut shell = ops::shell(&mut k, &b, wall, &open)?;
                let vertex_tol = sorted_tolerances(&shell, bp1::vertices(&shell, None)?, vertex::tolerance)?;
                let edge_tol = sorted_tolerances(&shell, bp1::edges(&shell, None)?, edge::tolerance)?;
                let face_tol = sorted_tolerances(&shell, bp1::faces(&shell, None)?, face::p2::tolerance)?;
                assert_eq!(vertex_tol.len(), VERTICES, "{wall} mm wall vertex count");
                assert_eq!(edge_tol.len(), EDGES, "{wall} mm wall edge count");

                let faces = query::faces(&k, &mut shell)?;
                assert_eq!(faces.len(), FACES, "{wall} mm wall face count");
                let mut kinds = std::collections::BTreeMap::new();
                for f in &faces {
                    *kinds.entry(f.kind).or_insert(0) += 1;
                }
                assert_eq!(kinds, FACE_KINDS.into_iter().collect(), "{wall} mm wall face kinds");

                let inner_radius = 4.0 - wall;
                let corner_area = (PI / 2.0) * inner_radius * STRAIGHT_HEIGHT;
                for sx in [-1.0, 1.0] {
                    for sz in [-1.0, 1.0] {
                        // The two arc centroids lie on this corner's bisector. The smaller
                        // radius puts the inner centroid closer to the square of corner centres.
                        let mut arcs: Vec<_> = faces.iter().filter(|f| f.kind == "extrusion"
                            && sx * f.centroid[0] > RUN / 2.0 && sz * f.centroid[2] > RUN / 2.0).collect();
                        arcs.sort_by(|a, b| a.centroid[0].abs().total_cmp(&b.centroid[0].abs()));
                        let [inner, outer] = arcs.as_slice() else {
                            panic!("{wall} mm wall, corner ({sx}, {sz}) needs two extrusions, found {}", arcs.len());
                        };
                        assert!((inner.area - corner_area).abs() <= 1e-6 * corner_area,
                            "{wall} mm wall, inner corner ({sx}, {sz}) area {} mm², expected {corner_area}", inner.area);

                        // At the same UV the inward offset scales the radial vector about the
                        // corner axis by (4 - wall)/4 and preserves height. Off-centre samples
                        // distinguish a changed arc parameterization from the right shape.
                        let outer = shell.face(outer.id)?;
                        let outer_surface = outer.surface.as_ref().expect("outer corner has a surface");
                        let inner_surface = face::p2::surface(&shell, inner.id)?.expect("inner corner has a surface");
                        let (cx, cz) = (sx * RUN / 2.0, sz * RUN / 2.0);
                        for (u, v) in [(0.25, 0.75), (0.75, 0.25)] {
                            let uv = outer.uv_domain.evaluate(u, v);
                            let p = outer_surface.evaluate_point(&k, uv)?;
                            let expected = makepad_csg_brep::math::Point3::new(
                                cx + (p.x - cx) * inner_radius / 4.0, p.y,
                                cz + (p.z - cz) * inner_radius / 4.0);
                            let actual = inner_surface.evaluate_point(&k, uv)?;
                            assert!(actual.distance(expected) <= k.tolerances.zone_tol_3d,
                                "{wall} mm wall, corner ({sx}, {sz}) at UV {uv:?} gave {actual:?}, expected {expected:?}");
                        }
                    }
                }

                // Request finer quadrature than the 1e-7 relative volume assertion.
                let (volume, _) = query::volume_area(&k, &mut shell, 1e-9)?;
                let expected = solid_volume(0.0) - solid_volume(wall);
                assert!((volume - expected).abs() <= 1e-7 * expected,
                    "{wall} mm wall volume {volume} mm³, expected {expected}");
                let mut face_strings: Vec<_> = faces.iter().map(|f| format!("{} {:.6} [{:.6} {:.6} {:.6}]",
                    f.kind, f.area, f.centroid[0], f.centroid[1], f.centroid[2])).collect();
                face_strings.sort();
                let names = shell_names(&mut shell, wall)?;
                assert!(names.iter().any(|n| !n.is_empty()), "{wall} mm wall has no named faces");
                Ok(Snapshot { vertex_tol, edge_tol, face_tol, faces: face_strings, names, volume })
            };
            for wall in [1.0, 1.9] {
                let a = build(wall)?;
                let again = build(wall)?;
                assert_eq!(a.vertex_tol, again.vertex_tol, "{wall} mm wall vertex tolerances differ between builds");
                assert_eq!(a.edge_tol, again.edge_tol, "{wall} mm wall edge tolerances differ between builds");
                assert_eq!(a.face_tol, again.face_tol, "{wall} mm wall face tolerances differ between builds");
                assert_eq!(a.faces, again.faces, "{wall} mm wall face kinds, areas or centroids differ between builds");
                assert_eq!(a.names, again.names, "{wall} mm wall face names differ between builds");
                assert!((a.volume - again.volume).abs() < 1e-9 * a.volume,
                    "{wall} mm wall volumes differ between builds, {} and {}", a.volume, again.volume);
            }
            Ok::<(), BrepError>(())
        })
    }).unwrap();
    handle.join().unwrap().unwrap();
}

/// A shell holds the volume of its wall: about the wall times the area of the walls (the part's
/// area less its open faces), here within 0.75-1.25 (rounded housings measure 0.86-0.98). A
/// housing shelled just over its top round (a 1.2 mm wall on a 1 mm round) came out 10 times
/// that at 100 mm across, solid inside with its open face uncut; at 120 mm it failed in the
/// kernel's bare words, and at 100 mm with tight corners it was refused for a crossing. Each now
/// builds the right body or is refused naming the round. (Walls that fit still build: the shells
/// of the tests above.)
#[test]
fn a_shell_holds_the_volume_of_its_wall() {
    let handle = std::thread::Builder::new().stack_size(stack_size()).spawn(|| {
        guard::guarded(stack_size(), &CancelToken::new(), None, || {
            let mut k = new_kernel();
            let frame = makepad_csg_brep_model::profile::Frame { origin: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 0.0, -1.0] };
            // (size, corner, wall): 30 tall (+Y), its top edges rounded 1 mm; stayed solid, failed
            // bare, crossed
            for (size, corner, wall) in [(100.0, 10.0, 1.2), (120.0, 10.0, 1.2), (100.0, 4.0, 1.2)] {
                let name = format!("{size} mm, corners {corner}, a {wall} mm wall");
                let sketch = Sketch::new(vec![Shape::Rect { center: [0.0, 0.0], size: [size, size], corner, chamfer: 0.0 }]);
                let mut b = ops::extrude(&mut k, &sketch, &frame, [0.0, 1.0, 0.0], 30.0)?;
                let top: Vec<_> = query::edges(&k, &mut b)?.into_iter().filter(|e| [e.start[1], e.end[1], e.mid[1]].iter().all(|y| (y - 30.0).abs() < 1e-6)).map(|e| e.id).collect();
                ops::fillet(&mut k, &mut b, &top, 1.0)?;
                let faces = query::faces(&k, &mut b)?;
                let open: Vec<_> = faces.iter().filter(|f| f.normal.is_some_and(|n| n[1] < -0.9)).map(|f| f.id).collect();
                let walls: f64 = faces.iter().filter(|f| !open.contains(&f.id)).map(|f| f.area).sum();
                match ops::shell(&mut k, &b, wall, &open) {
                    Ok(mut s) => {
                        let ratio = query::measure(&k, &mut s)?.volume / (wall * walls);
                        assert!((0.75..=1.25).contains(&ratio), "{name}: the volume of its wall, not {ratio:.2} times it");
                    }
                    Err(e) => {
                        let words = e.to_string();
                        assert!(words.contains(&format!("a wall of {wall} mm is just over the 1 mm round at [")), "{name}: refused naming the round: {words}");
                    }
                }
            }
            Ok::<(), BrepError>(())
        })
    }).unwrap();
    handle.join().unwrap().unwrap();
}

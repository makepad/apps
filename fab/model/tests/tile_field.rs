//! The tile view's compiled field on Fab's own parts: their code gives what their nodes' queries
//! give, its gradient what the value's differences give, and a parameter's number is new data
//! under the same code.
#![cfg(not(target_arch = "wasm32"))]
use fab_model::eval::{Evaluated, Evaluator};
use fab_model::{Change, FabModel};
use fab_plate::machine::Machine;
use makepad_csg_math::Vec3d;
use makepad_csg_sdf::{distance_many, node_bounds, SdfNode};
use fab_sdf_tiles::field::{compile, CompiledField};

const KNOB: &str = include_str!("../../app/resources/knurled_knob.cad.splash");
const VASE: &str = include_str!("../../app/resources/vase.cad.splash");
const LATTICE: &str = include_str!("../../app/resources/lattice_cube.cad.splash");
const CHAIN: &str = include_str!("../../app/resources/chain.cad.splash");
const VORONOI: &str = include_str!("../../app/resources/voronoi_vase.cad.splash");
/// The other Voronoi kernel: a foam of struts along the cells' edges inside the web.
const FOAM: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet b = m.box({size: [40mm, 40mm, 40mm] at: [0, 20mm, 0]})\nobject(m.voronoi(b, {size: 8mm strut: 1.5mm skin: 2mm}))\n";

struct Part {
    name: &'static str,
    model: FabModel,
    eval: Evaluator,
    root: SdfNode,
}

fn root_of(ev: &Evaluated, name: &str) -> SdfNode {
    ev.fields.iter().flatten().next().unwrap_or_else(|| panic!("{name}: no field part")).shape.root.clone()
}

fn part(name: &'static str, source: &str) -> Part {
    let model = FabModel::with_script(name, &format!("{name}.cad.splash"), source);
    let mut eval = Evaluator::new(Machine::dual_nozzle_350());
    let root = root_of(&model.evaluate(&mut eval, None).unwrap_or_else(|f| panic!("{name}: {f}")).1, name);
    Part { name, model, eval, root }
}

fn edit(p: &mut Part, param: &str, value: f64) {
    let head = p.model.head();
    let change = Change::Param { instance: p.name.into(), name: param.into(), value: Some(value) };
    let ev = p.model.apply(&mut p.eval, &format!("{param} {value}"), head, &[change], None).expect("the edit applies").1.expect("it rebuilds").1;
    p.root = root_of(&ev, p.name);
}

/// The field agrees with the node's own query through and around the part.
fn agrees(name: &str, root: &SdfNode, f: &CompiledField) {
    let (lo, hi) = node_bounds(root).unwrap_or_else(|| panic!("{name}: no bounds"));
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let n = 1500;
    let pts: Vec<Vec3d> = (0..n).map(|_| {
        let a: [f64; 3] = std::array::from_fn(|i| lo[i] - 2.0 + (hi[i] - lo[i] + 4.0) * rnd());
        Vec3d::new(a[0], a[1], a[2])
    }).collect();
    let xyz: Vec<f32> = pts.iter().flat_map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect();
    let (mut v, mut b, mut s) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    f.eval(&xyz, &mut v, &mut b, &mut s);
    let q: Vec<Vec3d> = xyz.chunks(3).map(|p| Vec3d::new(p[0] as f64, p[1] as f64, p[2] as f64)).collect();
    let mut want = vec![0.0; n];
    distance_many(root, &q, &mut want);
    for i in 0..n {
        assert!((v[i] as f64 - want[i]).abs() < 1.0e-3, "{name} at {:?}: {} against {}", q[i], v[i], want[i]);
    }
    gradient_agrees(name, root, f, &xyz, &v);
}

fn angle(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let (la, lb) = ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt(), (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt());
    (dot / (la * lb)).clamp(-1.0, 1.0).acos().to_degrees()
}

/// The gradient's code gives the value, and a gradient within half a degree of the central
/// differences of the shape's own query (f64: an f32 difference this short is mostly rounding far
/// from the surface) wherever those agree with themselves at twice the step (no crease within reach).
fn gradient_agrees(name: &str, root: &SdfNode, f: &CompiledField, xyz: &[f32], v: &[f32]) {
    let n = v.len();
    let (mut gv, mut g) = (vec![0.0; n], vec![0.0; n * 3]);
    f.gradient.eval(xyz, &mut gv, &mut g, 1.0e-3);
    let h = 1.0e-3f64;
    let mut around = Vec::with_capacity(n * 12);
    for p in xyz.chunks(3) {
        for s in [h, 2.0 * h] {
            for k in 0..3 {
                for sign in [1.0, -1.0] {
                    let mut q = [p[0] as f64, p[1] as f64, p[2] as f64];
                    q[k] += sign * s;
                    around.push(Vec3d::new(q[0], q[1], q[2]));
                }
            }
        }
    }
    let mut d = vec![0.0; around.len()];
    distance_many(root, &around, &mut d);
    let mut checked = 0;
    for i in 0..n {
        assert!((gv[i] - v[i]).abs() <= 1.0e-5 * (1.0 + v[i].abs()), "{name} point {i}: the gradient's value {} against {}", gv[i], v[i]);
        let w = &d[i * 12..i * 12 + 12];
        let diff = |o: usize, s: f64| -> [f64; 3] { std::array::from_fn(|k| (w[o + k * 2] - w[o + k * 2 + 1]) / (2.0 * s)) };
        let (near, far) = (diff(0, h), diff(6, 2.0 * h));
        let slope = (near[0] * near[0] + near[1] * near[1] + near[2] * near[2]).sqrt();
        if slope < 0.1 || angle(near, far) > 0.1 {
            continue;
        }
        checked += 1;
        let got = [g[i * 3] as f64, g[i * 3 + 1] as f64, g[i * 3 + 2] as f64];
        assert!(angle(got, near) < 0.5, "{name} point {i} at {:?}: the gradient {got:?} against the differences {near:?}", &xyz[i * 3..i * 3 + 3]);
    }
    assert!(checked * 4 > n * 3, "{name}: only {checked} of {n} points away from a crease");
}

#[test]
fn the_parts_code_gives_their_queries_values() {
    for (name, source, sites) in [("knurled_knob", KNOB, 0), ("vase", VASE, 0), ("lattice_cube", LATTICE, 0), ("chain", CHAIN, 1), ("voronoi_vase", VORONOI, 0), ("voronoi_foam", FOAM, 0)] {
        let p = part(name, source);
        let f = compile(&p.root, false).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(f.sites(), sites, "{name}: the leaves its query answers");
        agrees(name, &p.root, &f);
    }
}

/// On a box's face plane `max(q, 0)` and `min(max(q), 0)` both tie at 0, and the slopes on both
/// sides of the face are its normal: the gradient there is that normal, never zero. As a Splash
/// kernel and as the engine's box, at the f32 point of each face where the value is exactly 0.
#[test]
fn a_box_face_has_its_normal() {
    let kernel = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.sdf({bounds: [[-6mm, -6mm, -6mm], [6mm, 6mm, 6mm]] cell: 0.5mm field: fn(p) { sdf_box(p, vec3(10mm, 8mm, 6mm)) }}))\n";
    let node = SdfNode::Box { size: [10.0, 8.0, 6.0], radius: 0.0 };
    for (name, root) in [("box_kernel", part("box_kernel", kernel).root), ("box_node", node)] {
        let f = compile(&root, false).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (lo, hi) = node_bounds(&root).unwrap_or_else(|| panic!("{name}: no bounds"));
        let c: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) * 0.5);
        // per point on a face, the f32 steps across it: the one whose value is exactly 0 is the tie
        const STEPS: i32 = 64;
        let (mut xyz, mut want) = (Vec::new(), Vec::new());
        for (axis, half) in [(0, 5.0), (1, 4.0), (2, 3.0)] {
            for sign in [1.0, -1.0] {
                for (u, w) in [(0.0, 0.0), (1.5, -0.5), (-2.0, 2.5)] {
                    let mut q = c;
                    q[axis] += sign * half;
                    q[(axis + 1) % 3] += u;
                    q[(axis + 2) % 3] += w;
                    let q = q.map(|x| x as f32);
                    for k in -STEPS..=STEPS {
                        let mut s = q;
                        s[axis] = f32::from_bits((q[axis].to_bits() as i32 + k) as u32);
                        xyz.extend(s);
                    }
                    let mut n = [0.0; 3];
                    n[axis] = sign;
                    want.push(n);
                }
            }
        }
        let n = xyz.len() / 3;
        let (mut v, mut g) = (vec![0.0; n], vec![0.0; n * 3]);
        f.gradient.eval(&xyz, &mut v, &mut g, 1.0e-3);
        let mut ties = 0;
        for (i, normal) in want.iter().enumerate() {
            let per = (2 * STEPS + 1) as usize;
            let Some(t) = (i * per..(i + 1) * per).find(|&t| v[t] == 0.0) else { continue };
            ties += 1;
            let got = [g[t * 3] as f64, g[t * 3 + 1] as f64, g[t * 3 + 2] as f64];
            let len = (got[0] * got[0] + got[1] * got[1] + got[2] * got[2]).sqrt();
            assert!((len - 1.0).abs() < 1.0e-4 && angle(got, *normal) < 0.01, "{name} at {:?}: the gradient {got:?}, the face's normal {normal:?}", &xyz[t * 3..t * 3 + 3]);
        }
        assert!(ties * 2 > want.len(), "{name}: only {ties} of {} face points tie", want.len());
    }
}

/// A table past 2^24 words, its list starts in units of a multiple, and a list past the old loop cap.
#[test]
fn a_table_past_the_word_limit_reads_the_same() {
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet b = m.sphere({radius: 24mm at: [0, 24mm, 0]})\nobject(m.voronoi(b, {size: 3mm strut: 8mm skin: 1mm}))\n";
    let p = part("past_limit", source);
    let f = compile(&p.root, false).unwrap();
    assert_eq!(f.sites(), 0, "it splices");
    agrees("past_limit", &p.root, &f);
}

/// The drags the user makes: every step reads new numbers, none compiles.
#[test]
fn a_params_number_keeps_the_code() {
    for (name, source, drags) in [
        ("knurled_knob", KNOB, &[("knob_pitch", 0.002), ("knob_depth", 0.0008), ("knob_radius", 0.022), ("knob_height", 0.03)][..]),
        ("vase", VASE, &[("vase_belly", 0.03), ("vase_wall", 0.0025), ("vase_height", 0.12), ("vase_neck", 0.014), ("vase_blend", 0.015)][..]),
        ("lattice_cube", LATTICE, &[("lattice_period", 0.005), ("lattice_size", 0.04)][..]),
        // new seeds and a table of another size, under the same code
        ("voronoi_vase", VORONOI, &[("web_cell", 0.008), ("vase_belly", 0.03), ("web_strut", 0.002), ("vase_height", 0.1)][..]),
    ] {
        let mut p = part(name, source);
        let first = compile(&p.root, false).unwrap_or_else(|e| panic!("{name}: {e}"));
        for (param, value) in drags {
            edit(&mut p, param, *value);
            let f = compile(&p.root, false).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(f.structure, first.structure, "{name}: {param} changed the code");
            assert_eq!(f.compile_ms, 0.0, "{name}: {param} compiled");
            agrees(name, &p.root, &f);
        }
    }
}

/// A range known only at run time runs to its end, past the language's cap for a loop's passes: a
/// list of seeds is as long as its table says, and cutting it would draw another surface.
#[test]
fn a_run_time_range_runs_whole() {
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.sdf({field: fn(p) { let n = int(3000.0 + 0.0 * p.x)\n let v = 0.0\n for i in 0..n { v = v + 0.01mm }\n return length(p) - v } bounds: [[-40mm, -40mm, -40mm], [40mm, 40mm, 40mm]] cell: 1mm}))\n";
    let p = part("range", source);
    let f = compile(&p.root, false).unwrap();
    assert_eq!(f.sites(), 0, "it splices");
    agrees("range", &p.root, &f);
    let (mut v, mut b, mut s) = (vec![0.0; 8], vec![0.0; 8], vec![0.0; 8]);
    f.eval(&[10.0f32, 0.0, 0.0].repeat(8), &mut v, &mut b, &mut s);
    assert!((v[0] + 20.0).abs() < 0.1, "a ball of 30 mm, 10 mm from its centre: {} mm", v[0]);
    assert!(v.iter().all(|x| *x == v[0]), "every lane alike: {v:?}");
}

/// An open-ended loop past the language's cap stops there and says so; one that ends sooner says nothing.
#[test]
fn a_stopped_loop_is_told() {
    let f = |passes: &str| {
        let source = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.sdf({{field: fn(p) {{ let n = int({passes} + 0.0 * p.x)\n let i = 0\n let v = 0.0\n while i < n {{ i = i + 1\n v = v + 0.01mm }}\n return length(p) - v }} bounds: [[-40mm, -40mm, -40mm], [40mm, 40mm, 40mm]] cell: 1mm}}))\n");
        let p = part("a_loop", &source);
        let f = compile(&p.root, false).unwrap();
        let (mut v, mut b, mut s) = (vec![0.0; 8], vec![0.0; 8], vec![0.0; 8]);
        f.eval(&[10.0f32, 0.0, 0.0].repeat(8), &mut v, &mut b, &mut s);
        (v[0], f.loop_stopped())
    };
    let (value, stopped) = f("500.0");
    assert!((value - 5.0).abs() < 0.01 && !stopped, "{value} mm, stopped {stopped}");
    let (value, stopped) = f("3000.0");
    assert!((value + 0.24).abs() < 0.01 && stopped, "{value} mm, stopped {stopped}");
}

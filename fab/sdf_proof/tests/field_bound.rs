//! `fab_sdf_proof::bound` (the declared bounds, `bounds.splash`) against the fields it bounds: a
//! lattice (a gyroid sheet in a cube with a frame along its edges, a typical lattice cube) and a
//! blend of a stretched gyroid and a sphere. At many balls about points of the field, every bound
//! the proof gives is checked densely against the field evaluated in f64 (the same formulas, the
//! kernel's own f32 constants): along chords through the ball, every second difference within
//! [-K-, K+] h^2 (exact for a semi-concave and semi-convex function), every first difference
//! within L h; and the kernel's f32 value within e of the f64 one.
use fab_sdf_proof::{bound, BOUNDS};
use makepad_csg_sdf::{SdfSplashExpr, SplashPoint};
use makepad_script::*;
use makepad_script_compute::kernel::compile_with_modules;
use makepad_script_compute::Backend;
use makepad_script_compute_sched::kernel::KernelExt;

const NONE: f32 = 1.0e29;

fn make_vm() -> ScriptVm<'static> {
    let host = Box::leak(Box::new(ScriptVmHost::new(0i32, ())));
    ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) }
}

/// A document's field fn (its last value), traced.
fn traced(name: &str, doc: &str) -> SdfSplashExpr {
    let mut vm = make_vm();
    vm.capture_errors();
    let f = vm.eval(ScriptMod::new("", "", name, 0, 0, doc, &[]));
    assert!(vm.take_errors().is_empty(), "{name}'s document");
    let f = f.as_object().expect("a fn");
    let _keep = vm.new_object_ref(f);
    let field = SdfSplashExpr::compile_traced(&mut vm, f, SplashPoint::Vec3, &[]).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    match field.trace() {
        Some(Ok(_)) => field,
        other => panic!("{name}'s trace: {:?}", other.map(|t| t.err())),
    }
}

/// Per point (xyz, the ball's radius) the field's distance, K+, K-, L, and e.
fn kernel(name: &str, doc: &str) -> impl Fn(&[[f32; 4]]) -> Vec<[f32; 5]> {
    let field = traced(name, doc);
    move |pts: &[[f32; 4]]| pts.iter().map(|q| bound(&field, &field.params(), [q[0], q[1], q[2]], q[3]).expect("a proof")).collect()
}

fn len3(x: f64, y: f64, z: f64) -> f64 {
    (x * x + y * y + z * z).sqrt()
}

fn sdf_box(p: [f64; 3], s: [f64; 3]) -> f64 {
    let q = [p[0].abs() - s[0] * 0.5, p[1].abs() - s[1] * 0.5, p[2].abs() - s[2] * 0.5];
    len3(q[0].max(0.0), q[1].max(0.0), q[2].max(0.0)) + q[0].max(q[1].max(q[2])).min(0.0)
}

fn sdf_gyroid(p: [f64; 3], period: f32, wall: f64) -> f64 {
    // the kernel's k: TAU / period in f32
    let k = (std::f32::consts::TAU / period) as f64;
    let [x, y, z] = p.map(|c| c * k);
    let g = x.sin() * y.cos() + y.sin() * z.cos() + z.sin() * x.cos();
    let n = len3(x.cos() * y.cos() - z.sin() * x.sin(), y.cos() * z.cos() - x.sin() * y.sin(), z.cos() * x.cos() - y.sin() * z.sin());
    g.abs() / (k * n.max(1.0)) - wall * 0.5
}

fn smin(a: f64, b: f64, k: f64) -> f64 {
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b + (a - b) * h - k * h * (1.0 - h)
}

/// What the checks found over a field's balls.
#[derive(Default, Debug)]
struct Seen {
    balls: usize,
    /// balls with a K+ proof, with an L, and the largest L
    kp: usize,
    l: usize,
    l_most: f64,
    /// balls about a point outside (where a bake's copy is above 0), and those with a K+ proof
    outside: usize,
    outside_kp: usize,
    /// the most each bound was used (reached / declared)
    kp_use: f64,
    kn_use: f64,
    l_use: f64,
    e_use: f64,
}

/// Checks the kernel's bounds at `balls` against `f` (f64), along `chords` chords per ball,
/// each sampled at `n` + 1 points.
fn check(name: &str, run: &dyn Fn(&[[f32; 4]]) -> Vec<[f32; 5]>, f: &dyn Fn([f64; 3]) -> f64, balls: &[[f32; 4]], chords: usize, n: usize) -> Seen {
    let got = run(balls);
    let mut seen = Seen { balls: balls.len(), ..Seen::default() };
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut unit = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    for (b, &[d, kp, kn, l, e]) in balls.iter().zip(&got) {
        let c = [b[0] as f64, b[1] as f64, b[2] as f64];
        let r = b[3] as f64;
        // the value's error
        let fc = f(c);
        if e < NONE {
            let off = (d as f64 - fc).abs();
            assert!(off <= e as f64, "{name}: at {b:?} the kernel's {d} is {off:e} off the field's {fc}, more than its e {e}");
            seen.e_use = seen.e_use.max(off / e as f64);
        }
        if kp < NONE {
            seen.kp += 1;
        }
        if fc > 0.0 {
            seen.outside += 1;
            seen.outside_kp += (kp < NONE) as usize;
        }
        if l < NONE {
            seen.l += 1;
            seen.l_most = seen.l_most.max(l as f64);
        }
        if kp >= NONE && kn >= NONE && l >= NONE {
            continue;
        }
        for _ in 0..chords {
            // a chord: two points of the ball (on its sphere half the time)
            let mut ends = [[0.0; 3]; 2];
            for p in &mut ends {
                let (u, v, w) = (unit() * 2.0 - 1.0, unit() * std::f64::consts::TAU, unit());
                let s = (1.0 - u * u).sqrt();
                let rr = if w < 0.5 { r } else { r * unit().cbrt() };
                *p = [c[0] + rr * s * v.cos(), c[1] + rr * s * v.sin(), c[2] + rr * u];
            }
            let h = len3(ends[1][0] - ends[0][0], ends[1][1] - ends[0][1], ends[1][2] - ends[0][2]) / n as f64;
            if h <= 0.0 {
                continue;
            }
            let vals: Vec<f64> = (0..=n).map(|i| {
                let t = i as f64 / n as f64;
                f(std::array::from_fn(|k| ends[0][k] + (ends[1][k] - ends[0][k]) * t))
            }).collect();
            // the f64 evaluations' own rounding, in a difference over h and over h^2
            let (slack1, slack2) = (1e-13 / h + 1e-12, 1e-13 / (h * h) + 1e-9);
            for i in 0..n {
                let dv = (vals[i + 1] - vals[i]).abs() / h;
                if l < NONE {
                    assert!(dv <= l as f64 + slack1, "{name}: at {b:?} the field moves {dv} per unit along a chord, over its L {l}");
                    seen.l_use = seen.l_use.max(dv / l as f64);
                }
            }
            for i in 1..n {
                let dd = (vals[i - 1] + vals[i + 1] - 2.0 * vals[i]) / (h * h);
                if kp < NONE {
                    assert!(dd <= kp as f64 + slack2, "{name}: at {b:?} a second difference {dd} over its K+ {kp}");
                    if kp > 1e-3 {
                        seen.kp_use = seen.kp_use.max(dd / kp as f64);
                    }
                }
                if kn < NONE {
                    assert!(-dd <= kn as f64 + slack2, "{name}: at {b:?} a second difference {dd} under its -K- {kn}");
                    if kn > 1e-3 {
                        seen.kn_use = seen.kn_use.max(-dd / kn as f64);
                    }
                }
            }
        }
    }
    println!("{name}: {seen:?}");
    seen
}

/// Points over `lo..hi` (a grid, jittered) and some on the field's surface's neighbourhood,
/// each with each radius.
fn balls(lo: [f32; 3], hi: [f32; 3], per_axis: usize, radii: &[f32]) -> Vec<[f32; 4]> {
    let mut out = Vec::new();
    let mut s = 12345u32;
    let mut jit = move || {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (s >> 8) as f32 / (1u32 << 24) as f32
    };
    for i in 0..per_axis * per_axis * per_axis {
        let ijk = [i % per_axis, (i / per_axis) % per_axis, i / (per_axis * per_axis)];
        let p: [f32; 3] = std::array::from_fn(|k| lo[k] + (hi[k] - lo[k]) * (ijk[k] as f32 + jit()) / per_axis as f32);
        for r in radii {
            out.push([p[0], p[1], p[2], *r]);
        }
    }
    out
}

#[test]
fn the_lattice_bounds_hold() {
    let (size, period) = (24.0f32, 8.0f32);
    let run = kernel("lattice", "use mod.math.*
let size = 24.0
let period = 8.0
let field = fn(p) {
    let c = p - vec3(0.0, size / 2.0, 0.0)
    let cube = sdf_box(c, vec3(size, size, size))
    let inner = size - 3.0
    let open = sdf_union(sdf_union(sdf_box(c, vec3(size + 2.0, inner, inner)), sdf_box(c, vec3(inner, size + 2.0, inner))), sdf_box(c, vec3(inner, inner, size + 2.0)))
    let frame = sdf_difference(cube, open)
    return sdf_union(sdf_intersection(sdf_gyroid(p, period, 1.2), cube), frame)
}
(field)");
    let (s, inner) = (size as f64, (size - 3.0) as f64);
    let f = move |p: [f64; 3]| {
        let c = [p[0], p[1] - s / 2.0, p[2]];
        let cube = sdf_box(c, [s, s, s]);
        let open = sdf_box(c, [s + 2.0, inner, inner]).min(sdf_box(c, [inner, s + 2.0, inner])).min(sdf_box(c, [inner, inner, s + 2.0]));
        let frame = cube.max(-open);
        sdf_gyroid(p, period, 1.2f32 as f64).max(cube).min(frame)
    };
    // the bake's balls (half a cell's diagonal at 0.25 and 0.5 mm) and a bigger one
    for (r, share) in [(0.2165f32, 0.5), (0.433, 0.0), (1.5, 0.0)] {
        let b = balls([-13.0, -1.0, -13.0], [13.0, 25.0, 13.0], 22, &[r]);
        let seen = check(&format!("lattice, ball {r}"), &run, &f, &b, 6, 24);
        // the proofs hold where the fine bake asks: most balls about points outside prove K+
        assert!(seen.outside_kp as f64 >= share * seen.outside as f64, "ball {r}: the lattice proves K+ at {} of {} balls outside", seen.outside_kp, seen.outside);
        assert!(seen.l == seen.balls, "ball {r}: every ball has an L ({} of {})", seen.l, seen.balls);
    }
}

#[test]
fn a_stretched_blend_bounds_hold() {
    let period = 10.0f32;
    let run = kernel("stretched blend", "use mod.math.*
let period = 10.0
let field = fn(p) {
    let q = vec3(p.x * 1.2 - p.z * 0.3, p.y + 1.0, p.z * 0.9 + p.x * 0.2)
    smooth_union(sdf_gyroid(q, period, 1.0), sdf_sphere(p - vec3(1.0, 2.0, 3.0), 4.0), 0.75)
}
(field)");
    let f = move |p: [f64; 3]| {
        let q = [p[0] * 1.2 - p[2] * 0.3, p[1] + 1.0, p[2] * 0.9 + p[0] * 0.2];
        let s = len3(p[0] - 1.0, p[1] - 2.0, p[2] - 3.0) - 4.0;
        smin(sdf_gyroid(q, period, 1.0f32 as f64), s, 0.75)
    };
    for (r, share) in [(0.2165f32, 0.25), (0.8, 0.0)] {
        let b = balls([-6.0, -6.0, -6.0], [8.0, 9.0, 10.0], 16, &[r]);
        let seen = check(&format!("stretched blend, ball {r}"), &run, &f, &b, 6, 24);
        assert!(seen.outside_kp as f64 >= share * seen.outside as f64, "ball {r}: the blend proves K+ at {} of {} balls outside", seen.outside_kp, seen.outside);
    }
}

/// `sincos_error(m)` (`bounds.splash`) bounds the kernel's own sin and cos (exact32: the GPU's bits
/// too) at every argument of size up to m: dense over small arguments, spread up to 2^14, and
/// at the reduction's octant edges, where its error is largest.
#[test]
fn the_sincos_error_holds() {
    let src = "use sdf.bounds.*\nlet math = exact32\nlet xs = input(f32)\nlet s = output(f32)\nlet c = output(f32)\nlet e = output(f32)\nfn element(i) {\n let x = xs[i]\n s[i] = sin(x)\n c[i] = cos(x)\n e[i] = sincos_error(abs(x))\n}";
    let k = compile_with_modules(src, &[], Backend::Native, &[BOUNDS]).unwrap_or_else(|e| panic!("the sincos kernel: {e:?}"));
    let mut xs: Vec<f32> = (0..200_000).map(|i| -8.0 + 16.0 * i as f32 / 200_000.0).collect();
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..400_000 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let u = (rng >> 11) as f64 / (1u64 << 53) as f64;
        // log-spread magnitudes up to 2^14, both signs
        let m = (2.0f64).powf(u * 14.0) * if rng & 1 == 0 { 1.0 } else { -1.0 };
        xs.push(m as f32);
    }
    for q in 1..20_000 {
        let edge = (q as f64 * std::f64::consts::FRAC_PI_4) as f32;
        for d in -3i32..=3 {
            xs.push(f32::from_bits((edge.to_bits() as i32 + d) as u32));
        }
    }
    let n = xs.len();
    let (mut s, mut c, mut e) = (vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]);
    let mut call = k.call();
    call.input("xs", &xs).unwrap();
    call.output("s", &mut s).unwrap();
    call.output("c", &mut c).unwrap();
    call.output("e", &mut e).unwrap();
    call.run(n).unwrap();
    let mut worst = 0.0f64;
    for i in 0..n {
        let x = xs[i] as f64;
        let (so, co) = ((s[i] as f64 - x.sin()).abs(), (c[i] as f64 - x.cos()).abs());
        assert!(so <= e[i] as f64 && co <= e[i] as f64, "at {} sin is {so:e} off and cos {co:e}, over sincos_error {}", xs[i], e[i]);
        worst = worst.max(so.max(co) / e[i] as f64);
    }
    println!("sincos: {n} arguments, the most used of sincos_error {worst:.3}");
}

/// Every other declared function, over a field that takes each of them: the bound's call in
/// `bound_call.splash` reaches the function that was called, with its own arguments.
#[test]
fn every_declared_function_bounds_its_field() {
    let run = kernel("the others", "use mod.math.*
let field = fn(p) {
    let ball = sdf_round(sdf_round_box(p, vec3(6.0, 7.0, 8.0), 1.0), 0.25)
    let slab = sdf_plane(p, vec3(0.0, 1.0, 0.0), 1.5)
    let cut = smooth_difference(ball, sdf_shell(sdf_sphere(p, 2.0), 0.3), 0.4)
    return smooth_intersection(cut, slab, 0.5)
}
(field)");
    let f = |p: [f64; 3]| {
        let ball = sdf_box(p, [4.0, 5.0, 6.0]) - 1.0 - 0.25;
        let slab = p[1] - 1.5;
        let shell = (len3(p[0], p[1], p[2]) - 2.0).abs() - 0.3;
        let cut = -smin(-ball, shell, 0.4);
        -smin(-cut, -slab, 0.5)
    };
    let b = balls([-6.0, -6.0, -6.0], [6.0, 6.0, 6.0], 14, &[0.2]);
    let seen = check("the others", &run, &f, &b, 6, 24);
    assert_eq!(seen.l, seen.balls, "every ball has an L");
}

/// A document's own fn with a prelude function's name is that fn, not the prelude's: it declares
/// nothing, however it reads.
#[test]
fn a_documents_own_function_declares_nothing() {
    let run = kernel("own sphere", "use mod.math.*
let sdf_sphere = fn(p, r) { return 10.0 * (length(p) - r) }
let field = fn(p) { return sdf_sphere(p, 2.0) }
(field)");
    let [d, kp, kn, l, e] = run(&[[3.0, 0.0, 0.0, 0.5]])[0];
    assert_eq!(d, 10.0);
    assert!(kp >= NONE && kn >= NONE && l >= NONE && e >= NONE, "({kp}, {kn}, {l}, {e})");
}

/// A field is never cut for what it costs: its proof runs a range known only at run time to its
/// end, as the field does.
#[test]
fn a_long_range_runs_whole_in_the_proof() {
    let mut vm = make_vm();
    vm.capture_errors();
    let f = vm.eval(ScriptMod::new("", "", "long_range", 0, 0, "let f = fn(p, n) { let s = 0.0\n for k in 0..int(n) { s = s + 1.0 }\n return s }\n(f)", &[]));
    assert!(vm.take_errors().is_empty());
    let f = f.as_object().expect("a fn");
    let _keep = vm.new_object_ref(f);
    let mut range = SdfSplashExpr::compile_traced(&mut vm, f, SplashPoint::Vec3, &[1]).unwrap_or_else(|e| panic!("{e:?}"));
    range.set_uniforms(&[3000.0]);
    assert_eq!(bound(&range, &range.params(), [0.0; 3], 0.1).expect("a proof")[0], 3000.0);
}

/// The same part written as nested calls and as a let assigned again in line proves the same:
/// a head with three capsules blended on, their union by `min` and by the declared union. The
/// in-line `hair = min(hair, ..)` names each value in turn, and `min` takes the union's bound.
#[test]
fn a_part_proves_the_same_written_nested_or_in_line() {
    let c = |x: f64| format!("sdf_capsule(p, vec3({x:.1}, 12.0, 0.0), vec3({x:.1}, 22.0, 2.0), 1.5)");
    let doc = |body: String| format!("use mod.math.*\nlet field = fn(p) {{\n    let head = sdf_sphere(p - vec3(0.0, 8.0, 0.0), 8.0)\n{body}}}\n(field)");
    let forms = [
        ("nested min", doc(format!("    return smooth_union(head, min(min({}, {}), {}), 2.0)\n", c(-4.0), c(0.0), c(4.0)))),
        ("in-line min", doc(format!("    let hair = {}\n    hair = min(hair, {})\n    hair = min(hair, {})\n    return smooth_union(head, hair, 2.0)\n", c(-4.0), c(0.0), c(4.0)))),
        ("nested union", doc(format!("    return smooth_union(head, sdf_union(sdf_union({}, {}), {}), 2.0)\n", c(-4.0), c(0.0), c(4.0)))),
        ("in-line union", doc(format!("    let hair = {}\n    hair = sdf_union(hair, {})\n    hair = sdf_union(hair, {})\n    return smooth_union(head, hair, 2.0)\n", c(-4.0), c(0.0), c(4.0)))),
    ];
    let runs: Vec<_> = forms.iter().map(|(name, d)| kernel(name, d)).collect();
    let b = balls([-8.0, -2.0, -8.0], [8.0, 26.0, 8.0], 6, &[0.25, 1.0]);
    let all: Vec<Vec<[f32; 5]>> = runs.iter().map(|r| r(&b)).collect();
    for (k, (name, _)) in forms.iter().enumerate() {
        for (i, x) in all[k].iter().enumerate() {
            assert!(x[3] <= 1.0001, "{name} at {:?}: L {}", b[i], x[3]);
            assert_eq!(*x, all[0][i], "{name} at {:?} proves as the nested min does", b[i]);
        }
    }
}

// ---- Curves ----------------------------------------------------------------------------------

type P4 = [f64; 4];

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn xyz(p: P4) -> [f64; 3] {
    [p[0], p[1], p[2]]
}
fn lerp4(a: P4, b: P4, s: f64) -> P4 {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * s)
}
fn len4(a: P4) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2] + a[3] * a[3]).sqrt()
}

/// The capsule a..b of radius r.
fn capsule(p: [f64; 3], a: [f64; 3], b: [f64; 3], r: f64) -> f64 {
    let (pa, ba) = (sub3(p, a), sub3(b, a));
    let h = (dot3(pa, ba) / dot3(ba, ba)).clamp(0.0, 1.0);
    let q = [pa[0] - ba[0] * h, pa[1] - ba[1] * h, pa[2] - ba[2] * h];
    dot3(q, q).sqrt() - r
}

/// The prelude's round cone in f64: the hull of the two balls (the larger ball when one nearly
/// holds the other, as the prelude decides it).
fn round_cone(p: [f64; 3], a: [f64; 3], b: [f64; 3], ra: f64, rb: f64) -> f64 {
    let ba = sub3(b, a);
    let l2 = dot3(ba, ba).max(1e-30);
    let l = l2.sqrt();
    let pa = sub3(p, a);
    let da = dot3(pa, pa).sqrt() - ra;
    let pb = sub3(p, b);
    let db = dot3(pb, pb).sqrt() - rb;
    let dr = ra - rb;
    if dr.abs() >= l * 0.99609375 {
        return if dr >= 0.0 { da } else { db };
    }
    let ny = dr / l;
    let nx = (1.0 - ny * ny).max(0.0).sqrt();
    let y = dot3(pa, ba) / l;
    let h = dot3(pa, ba) / l2;
    let x = len3(pa[0] - ba[0] * h, pa[1] - ba[1] * h, pa[2] - ba[2] * h);
    let w = nx * y - ny * x;
    if w < 0.0 {
        da
    } else if w > nx * l {
        db
    } else {
        nx * x + ny * y - ra
    }
}

/// Span `k` of a curve through `pts` with its neighbours (past an end, the end mirrored), as the
/// prelude takes them.
fn span(pts: &[P4], k: usize) -> [P4; 4] {
    let (p1, p2) = (pts[k], pts[k + 1]);
    let p0 = if k > 0 { pts[k - 1] } else { std::array::from_fn(|i| p1[i] * 2.0 - p2[i]) };
    let p3 = if k + 2 < pts.len() { pts[k + 2] } else { std::array::from_fn(|i| p2[i] * 2.0 - p1[i]) };
    [p0, p1, p2, p3]
}

/// The knot step across a..b: the square root of the distance between their xyz.
fn knot(a: P4, b: P4) -> f64 {
    dot3(sub3(xyz(b), xyz(a)), sub3(xyz(b), xyz(a))).sqrt().max(1e-30).sqrt()
}

/// The centripetal Catmull-Rom span at s (0..1) as Barry and Goldman's pyramid of lerps over the
/// knots: the reference the prelude's Bezier form follows.
fn catmull(q: [P4; 4], s: f64) -> P4 {
    let t0 = 0.0;
    let t1 = t0 + knot(q[0], q[1]);
    let t2 = t1 + knot(q[1], q[2]);
    let t3 = t2 + knot(q[2], q[3]);
    let t = t1 + (t2 - t1) * s;
    let a1 = lerp4(q[0], q[1], (t - t0) / (t1 - t0));
    let a2 = lerp4(q[1], q[2], (t - t1) / (t2 - t1));
    let a3 = lerp4(q[2], q[3], (t - t2) / (t3 - t2));
    let b1 = lerp4(a1, a2, (t - t0) / (t2 - t0));
    let b2 = lerp4(a2, a3, (t - t1) / (t3 - t1));
    lerp4(b1, b2, (t - t1) / (t2 - t1))
}

/// The ideal tube's distance at `p` outside it: the nearest of the balls along the exact curve,
/// `per_span` of them a span.
fn ideal_tube(pts: &[P4], p: [f64; 3], per_span: usize) -> f64 {
    let mut best = f64::INFINITY;
    for k in 0..pts.len() - 1 {
        let q = span(pts, k);
        for i in 0..=per_span {
            let c = catmull(q, i as f64 / per_span as f64);
            let d = sub3(p, xyz(c));
            best = best.min(dot3(d, d).sqrt() - c[3].max(0.0));
        }
    }
    best
}

/// The prelude's tube in f64: each span's Bezier control points, its pieces, their round cones.
fn tube(pts: &[P4], p: [f64; 3]) -> f64 {
    let d0 = sub3(p, xyz(pts[0]));
    let mut best = dot3(d0, d0).sqrt() - pts[0][3].max(0.0);
    for (_, [p1, c1, c2, p2], n) in tube_spans(pts) {
        let mut prev = p1;
        for j in 0..n {
            let cur = bezier(p1, c1, c2, p2, (j + 1) as f64 / n as f64);
            best = best.min(round_cone(p, xyz(prev), xyz(cur), prev[3].max(0.0), cur[3].max(0.0)));
            prev = cur;
        }
    }
    best
}

fn bezier(a: P4, b: P4, c: P4, d: P4, t: f64) -> P4 {
    let u = 1.0 - t;
    std::array::from_fn(|i| a[i] * (u * u * u) + b[i] * (3.0 * u * u * t) + c[i] * (3.0 * u * t * t) + d[i] * (t * t * t))
}

/// Each span of the prelude's tube: its index, its Bezier control points and its piece count.
fn tube_spans(pts: &[P4]) -> Vec<(usize, [P4; 4], usize)> {
    (0..pts.len() - 1)
        .map(|k| {
            let [p0, p1, p2, p3] = span(pts, k);
            let (k0, k1, k2) = (knot(p0, p1), knot(p1, p2), knot(p2, p3));
            let c1: P4 = std::array::from_fn(|i| p1[i] + ((p1[i] - p0[i]) / k0 - (p2[i] - p0[i]) / (k0 + k1) + (p2[i] - p1[i]) / k1) * (k1 / 3.0));
            let c2: P4 = std::array::from_fn(|i| p2[i] - ((p2[i] - p1[i]) / k1 - (p3[i] - p1[i]) / (k1 + k2) + (p3[i] - p2[i]) / k2) * (k1 / 3.0));
            let dir = |a: P4, b: P4| -> P4 {
                let e: P4 = std::array::from_fn(|i| b[i] - a[i]);
                let l = len4(e).max(1e-30);
                e.map(|x| x / l)
            };
            let (u1, u2, u3) = (dir(p1, c1), dir(c1, c2), dir(c2, p2));
            let turn = len4(std::array::from_fn(|i| u1[i] - u2[i])) + len4(std::array::from_fn(|i| u2[i] - u3[i]));
            (k, [p1, c1, c2, p2], ((turn * 28.653708).ceil() as usize).clamp(1, 64))
        })
        .collect()
}

/// Kernel values at many points.
fn values(field: &SdfSplashExpr, pts: &[[f64; 3]]) -> Vec<f64> {
    let xyz: Vec<f32> = pts.iter().flat_map(|p| p.map(|x| x as f32)).collect();
    let mut out = vec![0.0f32; pts.len()];
    field.distance_batch(&xyz, &mut out);
    out.into_iter().map(|x| x as f64).collect()
}

const TUBE: [P4; 5] = [[0.0, 0.0, 0.0, 3.0], [4.0, 10.0, 2.0, 4.0], [-3.0, 18.0, 5.0, 2.5], [2.0, 26.0, -4.0, 3.5], [10.0, 30.0, 0.0, 1.5]];

/// A tube along an uneven bending curve whose radius swells and narrows (moved off the origin, so
/// its point is a map of the field's): near its surface it is the ideal tube, the balls along the
/// exact centripetal Catmull-Rom curve, to within its pieces' sag; its bound holds at every ball
/// (the kernel within e of its f64 copy, that copy within L = 1 along every chord); every ball has
/// L = 1.
#[test]
fn a_tube_follows_its_curve_and_proves_its_bound() {
    let at = [1.0, -2.0, 0.5];
    let doc = "use mod.math.*
let field = fn(p) {
    return sdf_tube(p - vec3(1.0, -2.0, 0.5), [vec4(0.0, 0.0, 0.0, 3.0), vec4(4.0, 10.0, 2.0, 4.0), vec4(-3.0, 18.0, 5.0, 2.5), vec4(2.0, 26.0, -4.0, 3.5), vec4(10.0, 30.0, 0.0, 1.5)])
}
(field)";
    let local = move |p: [f64; 3]| sub3(p, at);
    let pieces: Vec<usize> = tube_spans(&TUBE).iter().map(|s| s.2).collect();
    println!("tube: pieces per span {pieces:?}");
    assert!(pieces.iter().all(|n| *n > 1), "every span bends: {pieces:?}");

    // near the surface: the ideal tube, outside it to within the sag, inside on the same side
    let field = traced("tube", doc);
    let mut rng = 0x51ed_270b_2735_9a1du64;
    let mut unit = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut near = Vec::new();
    for _ in 0..600 {
        let k = (unit() * (TUBE.len() - 1) as f64) as usize;
        let c = catmull(span(&TUBE, k), unit());
        let (u, v) = (unit() * 2.0 - 1.0, unit() * std::f64::consts::TAU);
        let s = (1.0 - u * u).sqrt();
        let r = c[3] + unit() * 3.0 - 1.0;
        near.push([c[0] + at[0] + r * s * v.cos(), c[1] + at[1] + r * s * v.sin(), c[2] + at[2] + r * u]);
    }
    let got = values(&field, &near);
    let (tol, mut worst, mut outside) = (0.01, 0.0f64, 0);
    for (p, d) in near.iter().zip(&got) {
        let ideal = ideal_tube(&TUBE, local(*p), 1500);
        if ideal > tol {
            outside += 1;
            let off = (d - ideal).abs();
            worst = worst.max(off);
            assert!(off <= tol, "at {p:?} the tube is {d}, the ideal tube {ideal}: {off} apart, over {tol}");
        } else if ideal < -tol {
            assert!(*d < 0.0, "at {p:?} inside the ideal tube ({ideal}) the tube is {d}");
        }
    }
    println!("tube: {outside} points outside, at most {worst:e} off the ideal tube");
    assert!(outside > 200, "{outside} points outside");

    // the declared bound
    let run = kernel("tube", doc);
    let b = balls([-9.0, -8.0, -6.0], [17.0, 34.0, 13.0], 9, &[0.5]);
    let seen = check("tube", &run, &|p| tube(&TUBE, local(p)), &b, 4, 16);
    assert_eq!(seen.l, seen.balls, "every ball has an L");
    assert!(seen.l_most <= 1.0001, "L is 1, not {}", seen.l_most);
}

/// A tube blended into a capsule, a round cone on the capsule's other end: walked across the
/// blend over its surface in fine steps, the normal turns smoothly from the capsule's to the
/// tube's (no step turns it more than 0.6 degrees; the blend of two planes at right angles turns
/// it 0.2 a step at most, 90 degrees in all), and the blend's bound holds at every ball with L = 1.
/// The tube is straight where it meets the capsule (its first span's neighbours in line with it),
/// so what bends the walk is the blend alone.
#[test]
fn a_tube_blends_with_a_capsule_without_a_seam() {
    let doc = "use mod.math.*
let field = fn(p) {
    let arm = sdf_tube(p, [vec4(0.0, 3.0, 0.0, 2.5), vec4(8.0, 3.0, 0.0, 2.5), vec4(16.0, 3.0, 0.0, 2.5), vec4(21.0, 8.0, 0.0, 2.0)])
    let body = sdf_capsule(p, vec3(0.0, -10.0, 0.0), vec3(0.0, 10.0, 0.0), 5.0)
    let neck = sdf_round_cone(p, vec3(0.0, 12.0, 0.0), vec3(0.0, 19.0, 1.0), 3.5, 2.0)
    return smooth_union(smooth_union(arm, body, 2.0), neck, 1.5)
}
(field)";
    const ARM: [P4; 4] = [[0.0, 3.0, 0.0, 2.5], [8.0, 3.0, 0.0, 2.5], [16.0, 3.0, 0.0, 2.5], [21.0, 8.0, 0.0, 2.0]];
    let f = |p: [f64; 3]| {
        let arm = tube(&ARM, p);
        let body = capsule(p, [0.0, -10.0, 0.0], [0.0, 10.0, 0.0], 5.0);
        let neck = round_cone(p, [0.0, 12.0, 0.0], [0.0, 19.0, 1.0], 3.5, 2.0);
        smin(smin(arm, body, 2.0), neck, 1.5)
    };
    let run = kernel("blend", doc);
    let b = balls([-7.0, -16.0, -7.0], [23.0, 24.0, 7.0], 8, &[0.5]);
    let seen = check("blend", &run, &f, &b, 4, 16);
    assert_eq!(seen.l, seen.balls, "every ball has an L");
    assert!(seen.l_most <= 1.0001, "L is 1, not {}", seen.l_most);

    // up the capsule's side from below the arm, over the blend (it ends where the two differ by
    // the blend's 2, at x = 7), out along the arm's underside, in the plane z = 0 (the shapes'
    // plane of symmetry): each step along the surface's tangent, then back onto it along the normal
    let field = traced("blend", doc);
    let h = 1e-3;
    let normal = |p: [f64; 3]| {
        let v = values(&field, &[[p[0] + h, p[1], 0.0], [p[0] - h, p[1], 0.0], [p[0], p[1] + h, 0.0], [p[0], p[1] - h, 0.0]]);
        let (gx, gy) = ((v[0] - v[1]) / (2.0 * h), (v[2] - v[3]) / (2.0 * h));
        let l = (gx * gx + gy * gy).sqrt();
        [gx / l, gy / l]
    };
    let mut p = [5.0, -4.0, 0.0];
    let mut n = normal(p);
    let (first, ds) = (n, 0.005);
    let (mut most, mut steps) = (0.0f64, 0);
    while p[0] < 7.5 {
        p = [p[0] - n[1] * ds, p[1] + n[0] * ds, 0.0];
        for _ in 0..2 {
            let d = values(&field, &[p])[0];
            let m = normal(p);
            p = [p[0] - m[0] * d, p[1] - m[1] * d, 0.0];
        }
        let m = normal(p);
        most = most.max((n[0] * m[0] + n[1] * m[1]).clamp(-1.0, 1.0).acos().to_degrees());
        n = m;
        steps += 1;
        assert!(steps < 4000, "the walk reached the arm's underside");
    }
    let turned = (first[0] * n[0] + first[1] * n[1]).clamp(-1.0, 1.0).acos().to_degrees();
    println!("blend: {steps} steps, at most {most:.3} degrees a step, {turned:.1} in all");
    assert!(most <= 0.6, "a step turns the normal {most} degrees: a seam");
    assert!((turned - 90.0).abs() < 0.5, "from the capsule's side to the arm's underside: {turned} degrees");
}

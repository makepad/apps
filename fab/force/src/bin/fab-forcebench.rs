//! `fab-forcebench [steps]`: the solvers' iterations and times on the bench parts (the
//! cantilever and Kirsch's plate at two cell sizes each, isotropic; the demo bracket as printed
//! in PLA at a study's first `steps` cell sizes, default 2): per solve the CPU solver's line and
//! its levels (the GPU's are `fab-forceprogram`'s, on a hub).
//!
//! `fab-forcebench pairing`: the bench bracket's safety factor verdict at the study's default sizes, the coarser value read where the finer extreme is (FORCE A6 2.2) against its own extreme.
//!
//! `fab-forcebench sharp` reports the sharp inside edges and their cell masks at paired sizes
//! on boxes and brackets (FORCE A6 2.3, A8).
//!
//! `fab-forcebench zerotouch` counts, per bench field, the lattice samples where the field touches
//! zero inside the material, a seam a solve fills as a part-filled sheet (FORCE A13 F52): the
//! fillets as they were built before (the pieces joined by `min`) against the exact fields, at the
//! study's first three sizes.
//!
//! `fab-forcebench verify [filter]`: the static solver against closed forms and published
//! values: the patch test (a box on rollers in uniform tension, exact), a bar in tension (its
//! strain; aligned, shifted, turned), the MacNeal-Harder straight beam (extension, both bendings,
//! twist; aligned, shifted, turned), curved beam and twisted beam (aligned, shifted), NAFEMS LE1
//! and LE10 (the stress at D, aligned), and the bench cantilever (Timoshenko), at 2, 4 and 8
//! cells through each part's thin way (LE10 to 32); then the Richardson extrapolation of each
//! series' finest three steps. Each reference cites the page it was read on.
//!
//! `fab-forcebench reads [filter]` compares the element, the 27-cell fit and the FORCE A8
//! surface read on patch, le1, le10, fields, bar45 and bracket. The filter selects section names.
//!
//! `fab-forcebench engines [filter]` (with the `loopback` feature): the coarse rig's cases on the
//! Force program's CPU and GPU engines, and LOBPCG preconditioned by the GPU engine's solves.
//!
//! `fab-forcebench f32cells [filter]`: the GPU engine's f32 cell product emulated on the
//! coarse rig's cases (FORCE A4 1.3, F21): its energy on the f64 answer, plain and in
//! difference form.
//!
//! `fab-forcebench le10 16,32,64`: NAFEMS LE10's sigma_yy at D by cells through its thickness,
//! at D itself, recovered at D and the cell's mean, how full D's cell is, Richardson over the
//! finest three, and the value reported with FORCE A6's margin on each pair of sizes.
//! `fab-forcebench le10`: NAFEMS LE10's holds at 16 cells (FORCE A3 2.3), apart from the
//! quadrature: held nodes, the load and the reactions, and the stress at D with z held on one
//! node line.
//!
//! `fab-forcebench buckle [filter]`: the buckling rig. Clamped-free Euler columns (square and
//! round, 4 mm across, 10, 20 and 40 times as long) at 2 to 8 cells through, each on a grid
//! aligned with it, shifted half a cell, turned 20 degrees about two axes, and turned and
//! shifted, against the Euler load; the factor of the first mode, how close the second is (the
//! sections have two equal first modes), the mode's shape (one half-wave, the tip moving most),
//! and what the study's verdict (two cell sizes and the shifted grid) would say of it. The CPU
//! f64 solver and [`fab_force::buckle::lowest`]; cases run in parallel, largest first. `filter`
//! keeps the cases whose name holds it. Then the lateral-torsional strip (a narrow cantilever
//! pushed down at its tip, 2 to 6 cells through its thickness) against `4.013 sqrt(E I G J) /
//! L^2`, and the simply supported square plate pressed along one side (2 to 6 cells through,
//! aligned) against `k = 4`; `strip` or `plate` as the filter runs only those.

use fab_force::bc::{self, Kind};
use fab_force::bench::{self, turned, Member, BRACKET};
use fab_force::buckle::{self, Eigen, Loose, Start};
use fab_force::element::{corner, gradients_at, mul6, von_mises, GP};
use fab_force::grid::{Grid, Lattice};
use fab_force::material::{Materials, Print, PLA};
use fab_force::results::{evaluate, Reading, Recovery, Verdict, CONVERGED};
use fab_force::surface::{Pick, Surface};
use fab_force::solve::{solve_until, Dense, Hierarchy, Options};
use fab_force::study::{self, Model};
use fab_force::{Outcome, V3};
use std::collections::HashMap;
use std::f64::consts::FRAC_1_SQRT_2;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

fn line(name: &str, cells: usize, h: &Hierarchy, setup: f64, solve: f64, its: usize, extra: &str) {
    // per level: its nodes, and its cells with stiffness over the distinct matrices they keep
    let levels: Vec<String> = h.levels.iter().map(|l| {
        let (cells, mats) = l.matrices();
        format!("{} ({cells}:{mats})", l.grid.nodes())
    }).collect();
    println!("{name:<18} {cells:>7} cells {its:>4} its  setup {setup:>6.0} ms  solve {solve:>7.0} ms  {extra}");
    println!("    levels {}", levels.join(" / "));
}

/// `fab-forcebench shared`: the bracket's analysed level at the app's three study steps, its
/// cells whose Gauss points an offset moves against the distinct matrices the level keeps
/// (moved cells with equal point materials, fills and offset share one). Setup only, no solve.
fn shared_rig() {
    let m = Materials::new(PLA, &Print::default(), true);
    let (lo, hi) = BRACKET;
    let loads = bench::bracket_loads();
    let opts = Options::default();
    let mut field = |ps: &[V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = bench::bracket(*p));
    for h in study::steps(lo, hi, 3, 0.1) {
        let model = Model::new(lo, hi, h, &mut field, &m);
        let conditions = model.conditions(&loads);
        let (held, _) = fab_force::bc::nodes(&model.grid, &model.surface, &conditions);
        let g = &model.grid;
        let active = (0..g.cells()).filter(|&c| g.active(c)).count();
        let moved = (0..g.cells()).filter(|&c| g.active(c) && g.offset_of(c) != [0.0; 3]).count();
        // the moved cells' distinct matrices, keyed as `Level::new` shares them: each point's
        // mixture, the fills and the offset bits
        let distinct = (0..g.cells()).filter(|&c| g.active(c) && g.offset_of(c) != [0.0; 3]).map(|c| {
            (g.gp[c].map(|p| (p.wall, p.skin, p.angle, p.infill)), g.gp[c].map(|p| p.fill), g.offset_of(c).map(f64::to_bits))
        }).collect::<std::collections::HashSet<_>>().len();
        let t = Instant::now();
        let hier = Hierarchy::new(model.grid.clone(), &held, &m, &opts);
        let setup = t.elapsed().as_secs_f64() * 1e3;
        let (cells, mats) = hier.levels[0].matrices();
        println!("shared bracket h {h:.4}: active {active}, moved {moved} sharing {distinct} matrices ({:.1} MB, unshared {:.1} MB), analysed level {cells} cells keep {mats} matrices; setup {setup:.0} ms", distinct as f64 * 2400.0 / 1e6, moved as f64 * 2400.0 / 1e6);
        let levels: Vec<String> = hier.levels.iter().map(|l| { let (c, k) = l.matrices(); format!("{} ({c}:{k})", l.grid.nodes()) }).collect();
        println!("    levels {}", levels.join(" / "));
    }
}

fn max_len(u: &[V3]) -> f64 {
    u.iter().map(|u| (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt()).fold(0.0, f64::max)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("buckle") => buckle_rig(args.get(1).map_or("", String::as_str)),
        Some("verify") => verify_rig(args.get(1).map_or("", String::as_str)),
        Some("reads") => reads_rig(args.get(1).map_or("", String::as_str)),
        Some("coarse") => coarse_rig(args.get(1).map_or("", String::as_str)),
        Some("section") => section_rig(args.get(1).map_or("", String::as_str)),
        Some("le10") => match args.get(1) {
            Some(cells) => le10_series(&cells.split(',').filter_map(|c| c.parse().ok()).collect::<Vec<f64>>()),
            None => le10_holds(),
        },
        Some("f32cells") => f32_cells_rig(args.get(1).map_or("", String::as_str)),
        #[cfg(feature = "loopback")]
        Some("engines") => engines_rig(args.get(1).map_or("", String::as_str)),
        Some("pairing") => pairing_rig(),
        Some("shared") => shared_rig(),
        Some("sharp") => sharp_rig(),
        Some("zerotouch") => zero_touch_rig(),
        first => solvers(first.and_then(|s| s.parse().ok()).unwrap_or(2)),
    }
}

/// The solvers' iterations and times on the bench parts.
fn solvers(steps: usize) {
    let m = bench::isotropic();
    let opts = Options::default();
    for h in [0.5, 0.25] {
        let (grid, surface, conditions) = bench::cantilever(h, &m);
        let (held, force) = fab_force::bc::nodes(&grid, &surface, &conditions);
        let cells = (0..grid.cells()).filter(|&c| grid.active(c)).count();
        let t = Instant::now();
        let hier = Hierarchy::new(grid, &held, &m, &opts);
        let setup = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let sol = fab_force::solve::solve(&hier, &force, &opts);
        let solve = t.elapsed().as_secs_f64() * 1e3;
        let (l, s) = bench::BEAM;
        let tip = -fab_force::results::displacement(&hier.levels[0].grid, &sol.u, [l, 0.5 * s, 0.5 * s])[2];
        line(&format!("cantilever h {h}"), cells, &hier, setup, solve, sol.iterations, &format!("tip {:+.2} % vs beam theory", 100.0 * (tip / bench::cantilever_tip() - 1.0)));
    }
    for h in [0.5, 0.25] {
        let (grid, held, force) = bench::kirsch(2.5, 20.0, 10.0, 0.5, h, 10.0, &m);
        let cells = (0..grid.cells()).filter(|&c| grid.active(c)).count();
        let t = Instant::now();
        let hier = Hierarchy::new(grid, &held, &m, &opts);
        let setup = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let sol = fab_force::solve::solve(&hier, &force, &opts);
        let solve = t.elapsed().as_secs_f64() * 1e3;
        line(&format!("kirsch h {h}"), cells, &hier, setup, solve, sol.iterations, "");
    }
    // the bracket as the app solves it by default: as printed in PLA
    let m = Materials::new(PLA, &Print::default(), true);
    let (lo, hi) = BRACKET;
    let loads = bench::bracket_loads();
    let mut field = |ps: &[fab_force::V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = bench::bracket(*p);
        }
    };
    for h in study::steps(lo, hi, steps, 0.1) {
        let model = Model::new(lo, hi, h, &mut field, &m);
        let conditions = model.conditions(&loads);
        let (held, force) = fab_force::bc::nodes(&model.grid, &model.surface, &conditions);
        let cells = (0..model.grid.cells()).filter(|&c| model.grid.active(c)).count();
        let t = Instant::now();
        let hier = Hierarchy::new(model.grid.clone(), &held, &m, &opts);
        let setup = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let sol = fab_force::solve::solve(&hier, &force, &opts);
        let solve = t.elapsed().as_secs_f64() * 1e3;
        line(&format!("bracket h {h:.3}"), cells, &hier, setup, solve, sol.iterations, &format!("max |u| {:.5} mm", max_len(&sol.u)));
        let near = bc::near(&model.grid, &model.surface, &conditions);
        let grid = &hier.levels[0].grid;
        let timed = |surface: Option<&Surface>| {
            (0..3).map(|_| {
                let t = Instant::now();
                let r = evaluate(grid, &m, &sol.u, &near, surface);
                (t.elapsed().as_secs_f64() * 1e3, r.min_sf.0, r.max_vm.0)
            }).min_by(|a, b| a.0.total_cmp(&b.0)).unwrap()
        };
        let (a, sf_a, vm_a) = timed(None);
        let (b, sf_b, vm_b) = timed(Some(&model.surface));
        let quads = model.surface.quads.len();
        println!("  bracket h {h:.2} recovery: evaluate {a:.1} ms, with the surface's {b:.1} ms (+{:.1} ms, {:.2} % of the solve), {quads} quads; SF {sf_a:.3} -> {sf_b:.3}, largest von Mises {vm_a:.2} -> {vm_b:.2} MPa", b - a, 100.0 * (b - a) / solve);
    }
}

/// The bracket's verdict at neighbouring cell sizes, with the shifted check at the finest size.
fn pairing_rig() {
    let m = Materials::new(PLA, &Print::default(), true);
    let (lo, hi) = BRACKET;
    let loads = bench::bracket_loads();
    let sizes = study::steps(lo, hi, 3, 0.1);
    let solve = |h: f64, shift: V3| {
        let at = |p: V3| std::array::from_fn(|a| p[a] + shift[a]);
        let mut field = |ps: &[V3], out: &mut [f64]| {
            for (p, o) in ps.iter().zip(out) {
                *o = bench::bracket(*p);
            }
        };
        let model = Model::new(at(lo), at(hi), h, &mut field, &m);
        model.solve(&m, &model.conditions(&loads), &Options::default(), &mut |_, _| false).expect("the bracket is held")
    };
    println!("| sizes (mm) | slot | fine SF at (x, y, z) | coarse own SF at (x, y, z) | coarse paired SF | verdict by own | verdict by place |");
    println!("|---|---|---|---|---|---|---|");
    let mut coarse = solve(sizes[0], [0.0; 3]);
    for (i, &h) in sizes.iter().enumerate().skip(1) {
        let fine = solve(h, [0.0; 3]);
        pairing_rows(&coarse, &fine, None);
        if i + 1 == sizes.len() {
            let shifted = solve(h, study::shift(h));
            pairing_rows(&coarse, &fine, Some(&shifted));
        }
        coarse = fine;
    }
}

/// The coarse grid's own and paired readings for each slot, with the fine extreme that set the place.
fn pairing_rows(coarse: &Outcome, fine: &Outcome, shifted: Option<&Outcome>) {
    let reading = Reading::of(fine, Some(coarse), shifted);
    let name = |v| match v {
        Verdict::Converged { .. } => "Converged",
        Verdict::Apart { .. } => "Apart",
        Verdict::Unchecked(_) => "Unchecked",
    };
    let row = |slot: &str, extreme: fn(&Outcome) -> (f64, usize), by_place: Verdict| {
        let f = extreme(fine).0;
        let (own, cell) = extreme(coarse);
        let [cx, cy, cz] = coarse.grid.cell_center(cell);
        let by_own = Verdict::of(f, Some(own), shifted.map(|a| extreme(a).0));
        let lowest = shifted.filter(|a| extreme(a).0 < f).unwrap_or(fine);
        let (sf, cell) = extreme(lowest);
        let [x, y, z] = lowest.grid.cell_center(cell);
        let paired = match by_place {
            Verdict::Converged { coarse, .. } | Verdict::Apart { coarse, .. } => coarse,
            Verdict::Unchecked(_) => unreachable!("a pair includes the coarser solve"),
        };
        let check = if shifted.is_some() { " + shifted" } else { "" };
        println!("| {:.3} / {:.3}{check} | {slot} | {sf:.3} at ({x:.1}, {y:.1}, {z:.1}) | {own:.3} at ({cx:.1}, {cy:.1}, {cz:.1}) | {paired:.3} | {} | {} |",
            coarse.grid.h[0], fine.grid.h[0], name(by_own), name(by_place));
    };
    row("away", |a| a.results.min_sf, reading.away);
    row("near", |a| a.results.min_sf_fixed, reading.near);
}

/// The sharp inside corner along x at y = -17, z = 6 should persist at both sizes.
/// The boxes and the brackets with resolved fillets should have no kept points.
fn sharp_rig() {
    let m = Materials::new(PLA, &Print::default(), true);
    let lo = [0.0; 3];
    let box_hi = [10.0, 4.0, 4.0];
    sharp_case("plain box", (lo, box_hi), 1.0, &m, |p| bench::boxed(p, lo, box_hi));

    let bar_hi = [20.0, 2.0, 2.0];
    let coarse = 0.5;
    sharp_case("bar in tension", (lo, bar_hi), coarse, &m, |p| bench::boxed(p, lo, bar_hi));
    let frame = turned(30f64.to_radians());
    let (blo, bhi) = bench::turned_bounds(lo, bar_hi, &frame);
    let padded = (blo.map(|v| v - coarse), bhi.map(|v| v + coarse));
    sharp_case("turned bar", padded, coarse, &m, |p| bench::boxed(bench::to_own(&frame, p), lo, bar_hi));

    for coarse in [2.123, 1.061] {
        let fine = coarse / 2.0;
        sharp_case("sharp L", BRACKET, coarse, &m, |p| bench::bracket_filleted(p, 0.0));
        sharp_case("filleted L", BRACKET, coarse, &m, |p| bench::bracket_filleted(p, 4.0 * fine));
        sharp_case("demo bracket", BRACKET, coarse, &m, bench::bracket);
    }
}

/// Reports each surface's sharp edges, the points kept at both sizes, the cells they mark
/// and the bounds of the kept fine points in printer mm.
fn sharp_case(name: &str, (lo, hi): (V3, V3), coarse: f64, m: &Materials, distance: impl Fn(V3) -> f64) {
    let mut field = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out) {
            *o = distance(*p);
        }
    };
    let reach = 1.5 * coarse;
    let min_len = coarse / 4.0; // half a fine cell
    let [coarse, fine] = [coarse, coarse / 2.0].map(|h| Model::new(lo, hi, h, &mut field, m));
    let kept_fine = bc::sharp_points(&fine.surface, &coarse.surface, reach, min_len);
    let kept_coarse = bc::sharp_points(&coarse.surface, &fine.surface, reach, min_len);
    let masked = |own: &Model, other: &Model| {
        bc::sharp(&own.grid, &own.surface, &other.surface, reach, min_len).into_iter().filter(|&v| v).count()
    };
    let bounds = if let Some((&first, rest)) = kept_fine.split_first() {
        let (mut lo, mut hi) = (first, first);
        for p in rest {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        format!("{lo:?}..{hi:?}")
    } else {
        "none".into()
    };
    println!("{name}  h (coarse/fine) {}/{} mm  edges (coarse/fine) {}/{}  kept (fine/coarse) {}/{}  masked (coarse/fine) {}/{}  fine bounds (mm) {bounds}",
        coarse.h, fine.h, coarse.surface.sharp_edges(min_len).len(), fine.surface.sharp_edges(min_len).len(),
        kept_fine.len(), kept_coarse.len(), masked(&coarse, &fine), masked(&fine, &coarse));
}

/// FORCE A13 F52's zero-touch count: lattice samples with `d` in `(-a / 2, 0]` farther than
/// `1.5 a` from every surface vertex, the field touching zero inside the material (a seam), and
/// one of them.
fn zero_touch(lattice: &Lattice, surface: &Surface) -> (usize, Option<V3>) {
    let a = lattice.spacing;
    let reach = 1.5 * a;
    let key = |p: V3| p.map(|v| (v / reach).floor() as i64);
    let mut bins: HashMap<[i64; 3], Vec<V3>> = HashMap::new();
    for v in &surface.pos {
        let p = v.map(f64::from);
        bins.entry(key(p)).or_default().push(p);
    }
    let (mut count, mut one) = (0, None);
    for k in 0..lattice.n[2] {
        for j in 0..lattice.n[1] {
            for i in 0..lattice.n[0] {
                let d = lattice.at(i, j, k);
                if !(d > -0.5 * a && d <= 0.0) {
                    continue;
                }
                let p = lattice.pos(i, j, k);
                let [x, y, z] = key(p);
                let near = (-1..=1).any(|dx| (-1..=1).any(|dy| (-1..=1).any(|dz| {
                    bins.get(&[x + dx, y + dy, z + dz]).is_some_and(|vs| vs.iter().any(|v| {
                        (0..3).map(|c| (v[c] - p[c]).powi(2)).sum::<f64>() < reach * reach
                    }))
                })));
                if !near {
                    count += 1;
                    one.get_or_insert(p);
                }
            }
        }
    }
    (count, one)
}

/// The bench's filleted bracket as it was built before FORCE A13: the shelf, the wall and the
/// fillet's square less its circle, joined by `min`. The square shares a side with the shelf
/// and one with the wall, where the union touches zero inside the material.
fn bracket_constructive(p: V3, r: f64) -> f64 {
    let (y, z) = (p[1], p[2]);
    let shelf = bench::rect(y, z, [-23.0, 0.0], [23.0, 6.0]);
    let wall = bench::rect(y, z, [-23.0, 0.0], [-17.0, 52.0]);
    let (cy, cz) = (-17.0 + r, 6.0 + r);
    let fillet = bench::rect(y, z, [-17.0, 6.0], [cy, cz]).max(r - ((y - cy).powi(2) + (z - cz).powi(2)).sqrt());
    let section = shelf.min(wall).min(fillet);
    let q = [section, p[0].abs() - 32.0];
    let mut d = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt() + q[0].max(q[1]).min(0.0);
    let (r, x) = (2.3, 0.27 * 64.0);
    for sx in [-x, x] {
        d = d.max(r - ((p[0] - sx).powi(2) + (y - 7.5).powi(2)).sqrt());
        d = d.max(r - ((p[0] - sx).powi(2) + (z - 35.35).powi(2)).sqrt());
    }
    d
}

/// The saddle's box: a 56 mm square plate 6 mm thick, the post up to its top.
const POST_BOX: (V3, V3) = ([-28.0, -28.0, 0.0], [28.0, 28.0, 20.0]);

/// A round post of radius `post` on the plate with a fillet of `r` round its foot: exact (FORCE
/// A13: the L's profile in `(hypot(x, y) - post, z)`, revolved) or built as before (the plate,
/// the stem and the fillet's ring joined by `min`, touching along the ring's two sides).
fn post_field(exact: bool, post: f64, r: f64, [x, y, z]: V3) -> f64 {
    let s = x.hypot(y);
    let (lo, hi) = POST_BOX;
    if exact {
        return bench::boxed([x, y, z], lo, hi).max(r - bench::quadrant(s - post - r, z - 6.0 - r));
    }
    let (cs, cz) = (post + r, 6.0 + r);
    let plate = bench::boxed([x, y, z], lo, [hi[0], hi[1], 6.0]);
    let stem = bench::rect(s, z, [-post, 0.0], [post, hi[2]]);
    let fillet = bench::rect(s, z, [post, 6.0], [cs, cz]).max(r - (s - cs).hypot(z - cz));
    plate.min(stem).min(fillet)
}

/// F52's zero-touch count over each bench field at the study's first three sizes.
fn zero_touch_rig() {
    let m = Materials::new(PLA, &Print::default(), true);
    let sizes = study::steps(BRACKET.0, BRACKET.1, 4, 0.1);
    let r = 4.0 * sizes[1];
    type Field = Box<dyn Fn(V3) -> f64>;
    let fields: Vec<(String, (V3, V3), Field)> = vec![
        ("sharp L, as built before".into(), BRACKET, Box::new(|p| bracket_constructive(p, 0.0))),
        (format!("filleted L r {r:.3}, as built before"), BRACKET, Box::new(move |p| bracket_constructive(p, r))),
        (format!("filleted L r {r:.3}, exact"), BRACKET, Box::new(move |p| bench::bracket_filleted(p, r))),
        ("demo L r 9, as built before".into(), BRACKET, Box::new(|p| bracket_constructive(p, 9.0))),
        ("demo L r 9, exact".into(), BRACKET, Box::new(|p| bench::bracket_filleted(p, 9.0))),
        ("post 4.25, fillet 4.25, as built before".into(), POST_BOX, Box::new(|p| post_field(false, 4.25, 4.25, p))),
        ("post 4.25, fillet 4.25, exact".into(), POST_BOX, Box::new(|p| post_field(true, 4.25, 4.25, p))),
        ("post 17, fillet 4.25, as built before".into(), POST_BOX, Box::new(|p| post_field(false, 17.0, 4.25, p))),
        ("post 17, fillet 4.25, exact".into(), POST_BOX, Box::new(|p| post_field(true, 17.0, 4.25, p))),
        ("plain box".into(), BRACKET, Box::new(|p| bench::boxed(p, BRACKET.0, BRACKET.1))),
    ];
    println!("## F52 zero-touch samples: d in (-a/2, 0], over 1.5 a from every surface vertex");
    println!();
    println!("| field | {} |", sizes[..3].iter().map(|h| format!("{h:.3} mm")).collect::<Vec<_>>().join(" | "));
    println!("|---|---|---|---|");
    for (name, (lo, hi), field) in &fields {
        let mut distance = |ps: &[V3], out: &mut [f64]| {
            for (p, o) in ps.iter().zip(out) {
                *o = field(*p);
            }
        };
        let cells: Vec<String> = sizes[..3].iter().map(|&h| {
            let model = Model::new(*lo, *hi, h, &mut distance, &m);
            match zero_touch(model.lattice.as_deref().expect("sampled now"), &model.surface) {
                (n, Some(p)) => format!("{n} (one at {p:.2?})"),
                (n, None) => n.to_string(),
            }
        }).collect();
        println!("| {name} | {} |", cells.join(" | "));
    }
}

/// How a bench part sits on its grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Align {
    Aligned,
    Shifted,
    Turned,
    TurnedShifted,
}

impl Align {
    const ALL: [Align; 4] = [Align::Aligned, Align::Shifted, Align::Turned, Align::TurnedShifted];
    fn name(self) -> &'static str {
        match self {
            Align::Aligned => "aligned",
            Align::Shifted => "shifted",
            Align::Turned => "turned",
            Align::TurnedShifted => "turned+shifted",
        }
    }
    fn turned(self) -> bool {
        matches!(self, Align::Turned | Align::TurnedShifted)
    }
    fn shifted(self) -> bool {
        matches!(self, Align::Shifted | Align::TurnedShifted)
    }
    /// The same orientation shifted, the grid the verdict solves a second time.
    fn shifted_twin(self) -> Align {
        if self.turned() {
            Align::TurnedShifted
        } else {
            Align::Shifted
        }
    }
}

/// The turn of a turned bench part: 20 degrees about z, then about y.
const TURN_DEG: f64 = 20.0;

/// A column of the buckling rig.
#[derive(Clone, Copy, Debug)]
struct Column {
    round: bool,
    /// Length over section.
    ratio: f64,
    /// Cells through the section.
    cells: f64,
    align: Align,
}

/// The columns' section side or diameter (mm).
const SECTION: f64 = 4.0;
/// The holed bar's hole radius (mm), along its axis: E1's concave section.
const HOLE: f64 = 1.0;

impl Column {
    fn name(&self) -> String {
        format!("{} L/s {} n {} {}", if self.round { "round" } else { "square" }, self.ratio, self.cells, self.align.name())
    }
    fn member(&self) -> Member {
        let turn = if self.align.turned() { TURN_DEG.to_radians() } else { 0.0 };
        Member { l: self.ratio * SECTION, size: [SECTION; 2], round: self.round, frame: turned(turn) }
    }
    fn h(&self) -> f64 {
        SECTION / self.cells
    }
    /// The grid's cells, for the order cases run in (largest first).
    fn size(&self) -> f64 {
        let (lo, hi) = self.member().bounds();
        (0..3).map(|a| (hi[a] - lo[a]) / self.h() + 2.0).product()
    }
}

/// One column's answer.
#[derive(Clone, Debug)]
struct Row {
    cells: usize,
    reference: f64,
    /// The first two modes' factors (N: the load is 1 N).
    factor: [f64; 2],
    /// The first mode's sideways movement at mid-length over the tip's (Euler: 1 - cos(pi/4),
    /// 0.293), and whether it grows from the root to the tip all the way (one half-wave).
    mid_over_tip: f64,
    half_wave: bool,
    /// Where along the member (share of its length) the first mode moves most.
    peak_at: f64,
    /// The share of the first mode's tip movement across the member's own y (its thin way,
    /// for the strip: a lateral-torsional mode moves the tip sideways, not down).
    lateral: f64,
    residual: f64,
    steps: usize,
    cg: usize,
    converged: bool,
    static_its: usize,
    seconds: f64,
    /// The production eigensolver on the same level ([`fast_modes`]): its first factor, its
    /// cost in static solves, its steps.
    fast: (f64, f64, usize),
}

impl Row {
    fn error(&self) -> f64 {
        self.factor[0] / self.reference - 1.0
    }
}

/// The mode's shape along the member: stations on its axis, its sideways movement there.
fn shape(grid: &Grid, member: &Member, phi: &[V3]) -> (f64, bool, f64) {
    let axis = member.at([1.0, 0.0, 0.0]);
    let side = |t: f64| {
        let v = fab_force::results::displacement(grid, phi, member.at([t * member.l, 0.0, 0.0]));
        let along = v[0] * axis[0] + v[1] * axis[1] + v[2] * axis[2];
        [v[0] - along * axis[0], v[1] - along * axis[1], v[2] - along * axis[2]]
    };
    let tip = side(1.0);
    let len = (tip[0] * tip[0] + tip[1] * tip[1] + tip[2] * tip[2]).sqrt();
    let w: Vec<f64> = (0..=20).map(|k| {
        let s = side(k as f64 / 20.0);
        (s[0] * tip[0] + s[1] * tip[1] + s[2] * tip[2]) / len.max(f64::MIN_POSITIVE)
    }).collect();
    let half_wave = len > 0.0 && w.windows(2).all(|p| p[1] >= p[0] - 0.02 * len);
    // the node that moves most, as a share of the length from the root
    let mut peak = (0.0, 0.0);
    for c in (0..grid.cells()).filter(|&c| grid.active(c)) {
        for n in grid.cell_nodes(c) {
            let v = phi[n];
            let m = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
            if m > peak.0 {
                let p = grid.node_pos(n);
                peak = (m, (p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2]) / member.l);
            }
        }
    }
    (w[10] / len.max(f64::MIN_POSITIVE), half_wave, peak.1)
}

fn run_column(c: &Column, m: &Materials) -> Row {
    let member = c.member();
    let shift = if c.align.shifted() { study::shift(c.h()) } else { [0.0; 3] };
    run_member(&member, [-1.0, 0.0, 0.0], c.h(), shift, member.euler(bench::E, bench::NU), m)
}

/// A member clamped at its root with `load` (N, its own frame) on its tip face, at cells of `h`
/// on a grid moved by `shift`: its first two buckling factors against `reference` and its first
/// mode's shape.
/// LOBPCG ([`buckle::lobpcg`]) as the app runs it, cold (from the rotations), with this
/// process's CPU solve to 1e-2 as its preconditioner in place of the hub's: its first factor,
/// its cost in static solves (the preconditioner's iterations over a static solve's at the
/// product's tolerance from rest), its steps.
fn fast_modes(hier: &Hierarchy, f: &[V3], u0: &[V3]) -> (f64, f64, usize) {
    let fast = fast_counted(hier, f, u0, &Options::default());
    (fast.factor, fast.cost, fast.steps)
}

/// LOBPCG's run as [`fast_modes`] counts it, with its preconditioner solves and their
/// iterations; `opts` are the hierarchy's (how far it coarsened).
struct Fast {
    factor: f64,
    cost: f64,
    steps: usize,
    solves: usize,
    its: usize,
}

fn fast_counted(hier: &Hierarchy, f: &[V3], u0: &[V3], opts: &Options) -> Fast {
    let product = solve_until(hier, f, None, opts, &mut |_, _| false).iterations.max(1);
    let loose = Options { tolerance: 1e-2, ..*opts };
    let mut its = 0;
    let mut solves = 0;
    let modes = {
        let mut pre = |block: &[Vec<V3>]| {
            Ok(Some(block.iter().map(|r| {
                let s = solve_until(hier, r, None, &loose, &mut |_, _| false);
                its += s.iterations;
                solves += 1;
                s.u
            }).collect()))
        };
        buckle::lobpcg(&hier.levels[0], u0, Start::Rotations, &Loose::default(), &mut pre, &mut |_, _, _| false).ok().flatten().unwrap_or_default()
    };
    Fast { factor: modes.factor(0), cost: its as f64 / product as f64, steps: modes.steps, solves, its }
}

fn run_member(member: &Member, load: V3, h: f64, shift: V3, reference: f64, m: &Materials) -> Row {
    let t = Instant::now();
    let (grid, surface, conditions) = member.posed(load, h, shift, m);
    let (held, force) = bc::nodes(&grid, &surface, &conditions);
    let near = vec![false; grid.cells()];
    let posed = fab_force::pose(grid, held, force, near).expect("the member's root is held");
    let opts = Options::default();
    let hier = Hierarchy::new(posed.grid.clone(), &posed.held, m, &opts);
    let tight = Options { tolerance: 1e-10, max_iterations: 4000, ..opts };
    let stat = solve_until(&hier, &posed.force, None, &tight, &mut |_, _| false);
    let modes = buckle::lowest(&hier, &stat.u, &Eigen::default(), &mut |_, _| false);
    let (mid_over_tip, half_wave, peak_at) = shape(&posed.grid, member, &modes.vectors[0]);
    let tip = fab_force::results::displacement(&posed.grid, &modes.vectors[0], member.at([member.l, 0.0, 0.0]));
    let own = |axis: V3| {
        let a = member.at(axis);
        tip[0] * a[0] + tip[1] * a[1] + tip[2] * a[2]
    };
    let (y, z) = (own([0.0, 1.0, 0.0]), own([0.0, 0.0, 1.0]));
    Row {
        cells: (0..posed.grid.cells()).filter(|&k| posed.grid.active(k)).count(),
        reference,
        factor: [modes.factor(0), modes.factor(1)],
        mid_over_tip,
        half_wave,
        peak_at,
        lateral: y.abs() / (y * y + z * z).sqrt().max(f64::MIN_POSITIVE),
        residual: modes.residual[0],
        steps: modes.steps,
        cg: modes.cg,
        converged: modes.converged,
        static_its: stat.iterations,
        fast: fast_modes(&hier, &posed.force, &stat.u),
        seconds: t.elapsed().as_secs_f64(),
    }
}

/// The lateral-torsional strip: `l` long, `depth` deep (its own z, the load's way), `thick`
/// across (its own y); its tip pushed down through the section's centroid. d/t 8 keeps the
/// strip narrow (the formula's case), L/d 6 keeps it slender with the cells few.
const STRIP: (f64, f64, f64) = (48.0, 8.0, 1.0);

/// A strip case: cells through its thickness, how it sits on the grid.
#[derive(Clone, Copy, Debug)]
struct Strip {
    cells: f64,
    align: Align,
}

impl Strip {
    fn name(&self) -> String {
        format!("strip n {} {}", self.cells, self.align.name())
    }
    fn member(&self) -> Member {
        let turn = if self.align.turned() { TURN_DEG.to_radians() } else { 0.0 };
        bench::strip(STRIP.0, STRIP.1, STRIP.2, turned(turn))
    }
    fn h(&self) -> f64 {
        STRIP.2 / self.cells
    }
    fn size(&self) -> f64 {
        let (lo, hi) = self.member().bounds();
        (0..3).map(|a| (hi[a] - lo[a]) / self.h() + 2.0).product()
    }
}

/// The square plate: `a` wide, `t` thick (a/t 20, thin-plate theory's range), pressed along x by
/// 1 MPa: its factor is the critical stress.
/// The plates (width, thickness) and the cells through each is run at: a/t 20, and a/t 40,
/// whose shear and soft-support shares are about half a/t 20's.
const PLATES: [(f64, f64, &[f64]); 2] = [(40.0, 2.0, &[2.0, 4.0, 6.0]), (40.0, 1.0, &[2.0, 4.0])];

/// The plate's first mode across its middle: the out-of-plane movement at a quarter of the way
/// in over the middle's, along x and along y (one half-wave each way: sin(pi/4), 0.707).
fn plate_shape(grid: &Grid, phi: &[V3], a: f64, t: f64) -> (f64, f64) {
    let w = |x: f64, y: f64| fab_force::results::displacement(grid, phi, [x, y, 0.5 * t])[2];
    let mid = w(0.5 * a, 0.5 * a);
    let mid = if mid.abs() > 0.0 { mid } else { f64::MIN_POSITIVE };
    (w(0.25 * a, 0.5 * a) / mid, w(0.5 * a, 0.25 * a) / mid)
}

fn run_plate((a, t, cells): (f64, f64, f64), m: &Materials) -> (Row, (f64, f64)) {
    let t0 = Instant::now();
    let (grid, held, force) = bench::plate(a, t, t / cells, 1.0, m);
    let near = vec![false; grid.cells()];
    let posed = fab_force::pose(grid, held, force, near).expect("the plate is held");
    let opts = Options::default();
    let hier = Hierarchy::new(posed.grid.clone(), &posed.held, m, &opts);
    let tight = Options { tolerance: 1e-10, max_iterations: 4000, ..opts };
    let stat = solve_until(&hier, &posed.force, None, &tight, &mut |_, _| false);
    let modes = buckle::lowest(&hier, &stat.u, &Eigen::default(), &mut |_, _| false);
    let shape = plate_shape(&posed.grid, &modes.vectors[0], a, t);
    let row = Row {
        cells: (0..posed.grid.cells()).filter(|&k| posed.grid.active(k)).count(),
        reference: bench::plate_critical(a, t, bench::E, bench::NU),
        factor: [modes.factor(0), modes.factor(1)],
        mid_over_tip: 0.0,
        half_wave: true,
        peak_at: 0.0,
        lateral: 0.0,
        residual: modes.residual[0],
        steps: modes.steps,
        cg: modes.cg,
        converged: modes.converged,
        static_its: stat.iterations,
        fast: fast_modes(&hier, &posed.force, &stat.u),
        seconds: t0.elapsed().as_secs_f64(),
    };
    (row, shape)
}

/// Runs `cases` on every core, largest first (by `size`); each answer in its case's place.
fn parallel<C: Sync, R: Send>(cases: &[C], size: impl Fn(&C) -> f64, run: impl Fn(&C) -> R + Sync, name: impl Fn(&C) -> String + Sync) -> Vec<R> {
    let mut order: Vec<usize> = (0..cases.len()).collect();
    order.sort_by(|&a, &b| size(&cases[b]).total_cmp(&size(&cases[a])));
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..cases.len()).map(|_| None).collect());
    // the run's core grant (devhub's `--cores`), else the machine's cores
    let granted = std::env::var("MAKEPAD_CPU_CORES").ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|&n| n > 0);
    let workers = granted.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get())).min(cases.len()).max(1);
    let done = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(&i) = order.get(k) else { break };
                let t = Instant::now();
                let r = run(&cases[i]);
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                eprintln!("[{d}/{}] {} {:.1} s", cases.len(), name(&cases[i]), t.elapsed().as_secs_f64());
                out.lock().unwrap()[i] = Some(r);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.expect("every case ran")).collect()
}

fn buckle_rig(filter: &str) {
    let m = bench::isotropic();
    let mut cases = Vec::new();
    for round in [false, true] {
        for ratio in [10.0, 20.0, 40.0] {
            for cells in [2.0, 3.0, 4.0, 6.0, 8.0] {
                for align in Align::ALL {
                    let c = Column { round, ratio, cells, align };
                    if c.name().contains(filter) {
                        cases.push(c);
                    }
                }
            }
        }
    }
    if cases.is_empty() {
        return strip_and_plate(filter);
    }
    let t = Instant::now();
    let rows = parallel(&cases, Column::size, |c| run_column(c, &m), Column::name);
    println!("## Euler columns, clamped-free, E {} MPa, nu {}, section {SECTION} mm, load 1 N", bench::E, bench::NU);
    println!();
    println!("| section | L/s | cells through | alignment | active cells | Euler (N) | factor (N) | error | 2nd / 1st | mid / tip | half-wave | peak at x/L | residual | steps | CG its | static its | s | LOBPCG vs oracle | LOBPCG cost (static solves), steps |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (c, r) in cases.iter().zip(&rows) {
        println!(
            "| {} | {} | {} | {} | {} | {:.4} | {:.4} | {:+.2} % | {:.4} | {:.3} | {} | {:.2} | {:.1e} | {}{} | {} | {} | {:.1} | {:+.3} % | {:.1}, {} |",
            if c.round { "round" } else { "square" },
            c.ratio,
            c.cells,
            c.align.name(),
            r.cells,
            r.reference,
            r.factor[0],
            100.0 * r.error(),
            r.factor[1] / r.factor[0],
            r.mid_over_tip,
            if r.half_wave { "yes" } else { "NO" },
            r.peak_at,
            r.residual,
            r.steps,
            if r.converged { "" } else { " (unsettled)" },
            r.cg,
            r.static_its,
            r.seconds,
            100.0 * (r.fast.0 / r.factor[0] - 1.0),
            r.fast.1,
            r.fast.2
        );
    }
    // what a study's verdict would say: the coarser size, the finer (half the cell), and the
    // finer on its shifted grid
    let find = |round: bool, ratio: f64, cells: f64, align: Align| cases.iter().position(|c| c.round == round && c.ratio == ratio && c.cells == cells && c.align == align).map(|i| &rows[i]);
    println!();
    println!("## The verdict on each pair of cell sizes (CONVERGED = {CONVERGED})");
    println!();
    println!("| section | L/s | orientation | cells coarse -> fine | coarse | fine | fine shifted | verdict | value's true error | right? |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for round in [false, true] {
        for ratio in [10.0, 20.0, 40.0] {
            for align in [Align::Aligned, Align::Turned] {
                for (coarse, fine) in [(2.0, 4.0), (3.0, 6.0), (4.0, 8.0)] {
                    let (Some(c), Some(f), Some(s)) = (find(round, ratio, coarse, align), find(round, ratio, fine, align), find(round, ratio, fine, align.shifted_twin())) else { continue };
                    let v = Verdict::of(f.factor[0], Some(c.factor[0]), Some(s.factor[0]));
                    let (word, value) = match v {
                        Verdict::Converged { fine, .. } => ("Converged", fine),
                        Verdict::Apart { fine, .. } => ("Apart", fine),
                        Verdict::Unchecked(v) => ("Unchecked", v),
                    };
                    let err = value / f.reference - 1.0;
                    // right: Converged only where the value is within 10 %
                    let right = match v {
                        Verdict::Converged { .. } => err.abs() < 0.10,
                        _ => err.abs() >= 0.10,
                    };
                    println!(
                        "| {} | {ratio} | {} | {coarse} -> {fine} | {:.4} | {:.4} | {:.4} | {word} | {:+.2} % | {} |",
                        if round { "round" } else { "square" },
                        if align.turned() { "turned" } else { "straight" },
                        c.factor[0],
                        f.factor[0],
                        s.factor[0],
                        100.0 * err,
                        if right { "yes" } else if matches!(v, Verdict::Apart { .. }) { "apart (cautious)" } else { "NO" }
                    );
                }
            }
        }
    }
    println!();
    println!("{} cases in {:.0} s", cases.len(), t.elapsed().as_secs_f64());
    strip_and_plate(filter);
}

/// The lateral-torsional strip and the plate, after the columns.
fn strip_and_plate(filter: &str) {
    let m = bench::isotropic();
    // the strip at every alignment up to 4 cells through, aligned and shifted at 6 (a turned
    // strip's grid box is about 40 times its cells: 6 would take hours)
    let mut strips = Vec::new();
    let mut left_out = Vec::new();
    for cells in [2.0, 3.0, 4.0, 6.0] {
        for align in Align::ALL {
            let s = Strip { cells, align };
            if !s.name().contains(filter) {
                continue;
            }
            if align.turned() && cells > 4.0 {
                left_out.push(s.name());
            } else {
                strips.push(s);
            }
        }
    }
    if !strips.is_empty() {
        let t = Instant::now();
        let rows = parallel(&strips, Strip::size, |s| {
            let member = s.member();
            let shift = if s.align.shifted() { study::shift(s.h()) } else { [0.0; 3] };
            run_member(&member, [0.0, 0.0, -1.0], s.h(), shift, bench::lateral_torsional(&member, bench::E, bench::NU), &m)
        }, Strip::name);
        let (l, d, th) = STRIP;
        println!();
        println!("## Lateral-torsional strip, clamped-free, {l} x {d} x {th} mm, 1 N down through the tip section's centroid");
        println!();
        println!("Reference: 4.013 sqrt(E I_y G J) / L^2 = {:.4} N (J {:.4} mm^4, Saint-Venant).", bench::lateral_torsional(&strips[0].member(), bench::E, bench::NU), bench::torsion_constant(th, d));
        println!();
        println!("4.013 lets the section warp; the root's clamp holds it, which raises the load. The reference with warping held (Timoshenko and Gere, \"Theory of Elastic Stability\", 2nd ed. 1961, Table 6-3's gamma_2 against L^2 G J / (E Gamma)) could not be read on a published page, so the strip stays compared with 4.013.");
        println!();
        println!("| cells through | alignment | active cells | reference (N) | factor (N) | error | 2nd / 1st (none: the other mode buckles only under the load reversed) | sideways share at tip | half-wave | residual | steps | CG its | static its | s | LOBPCG vs oracle | LOBPCG cost (static solves), steps |");
        println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
        for (s, r) in strips.iter().zip(&rows) {
            println!(
                "| {} | {} | {} | {:.4} | {:.4} | {:+.2} % | {} | {:.3} | {} | {:.1e} | {}{} | {} | {} | {:.1} | {:+.3} % | {:.1}, {} |",
                s.cells,
                s.align.name(),
                r.cells,
                r.reference,
                r.factor[0],
                100.0 * r.error(),
                if r.factor[1].is_finite() { format!("{:.3}", r.factor[1] / r.factor[0]) } else { "none".into() },
                r.lateral,
                if r.half_wave { "yes" } else { "NO" },
                r.residual,
                r.steps,
                if r.converged { "" } else { " (unsettled)" },
                r.cg,
                r.static_its,
                r.seconds,
                100.0 * (r.fast.0 / r.factor[0] - 1.0),
                r.fast.1,
                r.fast.2
            );
        }
        for name in &left_out {
            println!("| {name}: left out, its grid box too large for the rig's time | | | | | | | | | | | | | |");
        }
        println!();
        println!("{} strip cases in {:.0} s", strips.len(), t.elapsed().as_secs_f64());
    }
    // the plates: the mid-plane must be a node plane (cells through even), aligned only
    for (a, th, sizes) in PLATES {
        let plates: Vec<(f64, f64, f64)> = sizes.iter().map(|&n| (a, th, n)).filter(|&(a, th, n)| format!("plate a/t {} n {n}", a / th).contains(filter)).collect();
        if plates.is_empty() {
            continue;
        }
        let t = Instant::now();
        let rows = parallel(&plates, |&(a, th, n)| (a / (th / n)).powi(2) * n, |&p| run_plate(p, &m), |&(a, th, n)| format!("plate a/t {} n {n}", a / th));
        println!();
        println!("## Square plate, simply supported on four edges, {a} x {a} x {th} mm (a/t {}), pressed along x by 1 MPa", a / th);
        println!();
        println!("Reference: k pi^2 E / (12 (1 - nu^2)) (t/b)^2, k = 4: {:.4} MPa. Beside it, not a reference: with the plate's shear (Mindlin, one half-wave each way) {:.4} MPa; the soft support (the edges held on the mid-plane line only, free to twist) takes off more.", bench::plate_critical(a, th, bench::E, bench::NU), bench::plate_critical_mindlin(a, th, bench::E, bench::NU));
        println!();
        println!("| cells through | active cells | reference (MPa) | factor (MPa) | error | 2nd / 1st | w(a/4)/w(a/2) along x | along y | residual | steps | CG its | static its | s | LOBPCG vs oracle | LOBPCG cost (static solves), steps |");
        println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
        for (&(_, _, n), (r, (sx, sy))) in plates.iter().zip(&rows) {
            println!(
                "| {n} | {} | {:.4} | {:.4} | {:+.2} % | {:.3} | {sx:.3} | {sy:.3} | {:.1e} | {}{} | {} | {} | {:.1} | {:+.3} % | {:.1}, {} |",
                r.cells,
                r.reference,
                r.factor[0],
                100.0 * r.error(),
                r.factor[1] / r.factor[0],
                r.residual,
                r.steps,
                if r.converged { "" } else { " (unsettled)" },
                r.cg,
                r.static_its,
                r.seconds,
                100.0 * (r.fast.0 / r.factor[0] - 1.0),
                r.fast.1,
                r.fast.2
            );
        }
        // the finest pair, extrapolated as second order: fine - (coarse - fine) / (r^2 - 1), r
        // the ratio of their cells through
        if let [.., (c, (rc, _)), (f, (rf, _))] = &plates.iter().zip(&rows).collect::<Vec<_>>()[..] {
            let r2 = (f.2 / c.2).powi(2);
            let value = rf.factor[0] - (rc.factor[0] - rf.factor[0]) / (r2 - 1.0);
            println!();
            println!("Richardson (second order) over {} and {} cells through: {value:.4} MPa, {:+.2} % against k = 4.", c.2, f.2, 100.0 * (value / rf.reference - 1.0));
        }
        println!();
        println!("{} plate cases in {:.0} s", plates.len(), t.elapsed().as_secs_f64());
    }
}


/// A posed part: its grid, the held axes of each node, each node's force.
type Posed3 = (Grid, Vec<u8>, Vec<V3>);
type Read = Box<dyn Fn(&Grid, &[V3]) -> f64 + Sync>;

/// One verification case: a part held and loaded, solved to 1e-10 and read, against its
/// published value.
struct Check {
    name: String,
    /// The case at every cell size (a series) and this step's cell size: each series' finest
    /// three steps are extrapolated (Richardson).
    series: String,
    h: f64,
    /// The grid box's cells (the order cases run in; a turned case over [`BOX_LIMIT`] is named
    /// and left out).
    size: f64,
    /// The published value; 0 for a patch test, whose reading is its error itself.
    reference: f64,
    /// Where the reference was read.
    source: &'static str,
    pose: Box<dyn Fn() -> Posed3 + Sync>,
    read: Read,
    /// Other readings of the same solve, each named (`read`'s name is the case's).
    also: Vec<(&'static str, Read)>,
    m: Materials,
}

/// A turned part's grid box past this many cells takes the rig hours: named and left out.
const BOX_LIMIT: f64 = 4.0e6;

fn run_check(c: &Check) -> (f64, Vec<f64>, usize, usize) {
    let (grid, held, force) = (c.pose)();
    let near = vec![false; grid.cells()];
    let posed = fab_force::pose(grid, held, force, near).expect("the part is held");
    let opts = Options::default();
    let hier = Hierarchy::new(posed.grid.clone(), &posed.held, &c.m, &opts);
    let tight = Options { tolerance: 1e-10, max_iterations: 4000, ..opts };
    let sol = solve_until(&hier, &posed.force, None, &tight, &mut |_, _| false);
    let active = (0..posed.grid.cells()).filter(|&k| posed.grid.active(k)).count();
    let value = (c.read)(&posed.grid, &sol.u);
    let also = c.also.iter().map(|(_, read)| read(&posed.grid, &sol.u)).collect();
    (value, also, active, sol.iterations)
}

/// A part posed by its picked regions: the nodes they hold and load.
fn held((grid, surface, conditions): (Grid, Surface, Vec<bc::Condition>)) -> Posed3 {
    let (held, force) = bc::nodes(&grid, &surface, &conditions);
    (grid, held, force)
}

/// The cell-mean stress component `k` (Voigt: xx yy zz yz xz xy, MPa) of the cell holding `p`.
fn stress_at(m: Materials, p: V3, k: usize) -> Read {
    Box::new(move |g: &Grid, u: &[V3]| {
        let r = evaluate(g, &m, u, &vec![false; g.cells()], None);
        r.stress[g.locate(p).0][k] as f64
    })
}

/// The stress component `k` (Voigt, MPa) at the point `p` itself of an isotropic part (`e`,
/// `nu`): its cell's strain there by the shape functions' gradients at `p`, the element's own
/// stress at the point (as a finite element code reads a corner), not the cell's mean.
fn stress_point(e: f64, nu: f64, p: V3, k: usize) -> Read {
    let d = fab_force::material::Ortho::isotropic(e, nu).stiffness();
    Box::new(move |g: &Grid, u: &[V3]| {
        let (c, xi) = g.locate(p);
        let dn = fab_force::element::gradients_at(xi).map(|v| std::array::from_fn(|a| v[a] / g.h[a]));
        let strain = fab_force::element::strain(&g.cell_nodes(c).map(|n| u[n]), &dn);
        (0..6).map(|j| d[k][j] * strain[j]).sum()
    })
}

/// The stress component `k` (Voigt, MPa) recovered at surface point `p` with outward
/// normal `n`, as the Force view reads a surface (FORCE A6 2.1).
fn stress_recovered(m: Materials, p: V3, n: V3, k: usize) -> Read {
    Box::new(move |g: &Grid, u: &[V3]| Recovery::new(g, u).stress_at(&m, p, n).map_or(f64::NAN, |s| s[k]))
}

/// This is the private SOLID fill threshold in results.rs.
const SOLID: u8 = 64;

/// All eight octants must meet the threshold, which also makes the cell active.
fn sound(g: &Grid, c: usize) -> bool {
    g.gp.get(c).is_some_and(|points| points.iter().all(|gp| gp.fill >= SOLID))
}

/// The table shows the inward step count, the three slope stencils and the least octant fill.
/// Zero means the first cell was sound. A dash means the search was off or found no sound cell.
#[derive(Clone, Copy)]
struct Note {
    steps: Option<u8>,
    fill: u8,
    slope: [char; 3],
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.steps {
            Some(steps) => write!(f, "{steps}")?,
            None => write!(f, "-")?,
        }
        let [x, y, z] = self.slope;
        write!(f, " {x}{y}{z} f{}", self.fill)
    }
}

/// Finds the first sound cell walking inward, with the active surface cell as a fallback.
fn a8_base(g: &Grid, p: V3, n: V3, step: bool) -> Option<(usize, Option<u8>)> {
    if g.gp.is_empty() {
        return None;
    }
    let origin = g.node_pos(0);
    let locate = |x: V3| {
        (0..3).all(|j| x[j] >= origin[j] && x[j] <= origin[j] + g.n[j] as f64 * g.h[j])
            .then(|| g.locate(x).0)
    };
    let [hx, hy, hz] = g.h;
    let hm = hx.min(hy).min(hz);
    let inward = |steps: u8| locate(std::array::from_fn(|j| p[j] - n[j] * hm * (0.25 + 0.5 * f64::from(steps))));
    let first = inward(0);
    if step {
        if let Some(b) = first.filter(|&c| sound(g, c)) {
            return Some((b, Some(0)));
        }
        for steps in 1..=3 {
            if let Some(b) = inward(steps).filter(|&c| sound(g, c)) {
                return Some((b, Some(steps)));
            }
        }
    }
    first.filter(|&c| g.active(c)).or_else(|| locate(p).filter(|&c| g.active(c))).map(|b| (b, None))
}

/// Reads the element at the surface point and restores each enabled gradient column's variation
/// along its own axis from the sound face neighbours.
fn a8_strain(g: &Grid, u: &[V3], p: V3, n: V3, step: bool, slopes: [bool; 3]) -> Option<([f64; 6], usize, Note)> {
    let (b, steps) = a8_base(g, p, n, step)?;
    let center = g.cell_center(b);
    let xi = std::array::from_fn(|j| 2.0 * (p[j] - center[j]) / g.h[j]);
    let dn: [V3; 8] = gradients_at(xi).map(|v| std::array::from_fn(|j| v[j] / g.h[j]));
    let column = |nodes: &[usize; 8], j: usize| -> V3 {
        let mut col = [0.0; 3];
        for (a, &node) in nodes.iter().enumerate() {
            for i in 0..3 {
                col[i] += u[node][i] * dn[a][j];
            }
        }
        col
    };
    let ijk = g.cell_ijk(b);
    let nodes = g.cell_nodes(b);
    let mut grad = [[0.0; 3]; 3];
    let mut note = Note {
        steps,
        fill: g.gp[b].iter().map(|gp| gp.fill).min()?,
        slope: slopes.map(|on| if on { '0' } else { 'x' }),
    };
    for j in 0..3 {
        let mut col = column(&nodes, j);
        if steps.is_some() && slopes[j] {
            let neighbour = |index: usize| {
                if index >= g.n[j] {
                    return None;
                }
                let mut at = ijk;
                at[j] = index;
                let c = g.cell(at[0], at[1], at[2]);
                sound(g, c).then(|| column(&g.cell_nodes(c), j))
            };
            let plus = neighbour(ijk[j] + 1);
            let minus = ijk[j].checked_sub(1).and_then(neighbour);
            let difference = match (plus, minus) {
                (Some(hi), Some(lo)) => Some((hi, lo, 2.0, 'c')),
                (Some(hi), None) => Some((hi, col, 1.0, '+')),
                (None, Some(lo)) => Some((col, lo, 1.0, '-')),
                (None, None) => None,
            };
            if let Some((hi, lo, span, kind)) = difference {
                note.slope[j] = kind;
                for i in 0..3 {
                    col[i] += (p[j] - center[j]) * (hi[i] - lo[i]) / (span * g.h[j]);
                }
            }
        }
        for i in 0..3 {
            grad[i][j] = col[i];
        }
    }
    let [x, y, z] = grad;
    Some(([x[0], y[1], z[2], y[2] + z[1], x[2] + z[0], x[1] + y[0]], b, note))
}

type Mixtures = HashMap<(u8, u8, u8, u16), [[f64; 6]; 6]>;

/// Uses the nearest filled Gauss point's material. The unshifted point positions are local
/// because the matching point_at helper in results.rs is private.
fn a8_stress(m: &Materials, g: &Grid, u: &[V3], p: V3, n: V3, step: bool, slopes: [bool; 3], cache: Option<&mut Mixtures>) -> Option<([f64; 6], Note)> {
    let (e, b, note) = a8_strain(g, u, p, n, step, slopes)?;
    let center = g.cell_center(b);
    let mut nearest = None;
    let mut distance = f64::INFINITY;
    for (q, gp) in g.gp[b].iter().enumerate().filter(|(_, gp)| gp.fill > 0) {
        let side = corner(q);
        let d = (0..3).map(|j| {
            let x = center[j] + (2.0 * side[j] as f64 - 1.0) * GP * g.h[j] / 2.0;
            (p[j] - x).powi(2)
        }).sum::<f64>();
        if d < distance {
            distance = d;
            nearest = Some(gp);
        }
    }
    let gp = nearest?;
    let mix = || {
        let (w, s) = gp.shares();
        m.mix(w, s, gp.angle, gp.infill)
    };
    let sigma = match cache {
        Some(cache) => mul6(cache.entry((gp.wall, gp.skin, gp.angle, gp.infill)).or_insert_with(mix), &e),
        None => mul6(&mix(), &e),
    };
    Some((sigma, note))
}

/// Reads one stress component with the inward search and the chosen own-direction slopes.
fn stress_a8(m: Materials, p: V3, n: V3, k: usize, slopes: [bool; 3]) -> Read {
    Box::new(move |g: &Grid, u: &[V3]| a8_stress(&m, g, u, p, n, true, slopes, None).map_or(f64::NAN, |(s, _)| s[k]))
}

/// Reads one stress component from the element a quarter cell inside, evaluated at p.
fn stress_element(m: Materials, p: V3, n: V3, k: usize) -> Read {
    Box::new(move |g: &Grid, u: &[V3]| a8_stress(&m, g, u, p, n, false, [false; 3], None).map_or(f64::NAN, |(s, _)| s[k]))
}

/// Keeps the displayed value and note from the same A8 read.
fn noted_value(read: Option<([f64; 6], Note)>, value: impl FnOnce([f64; 6]) -> f64) -> (f64, String) {
    match read {
        Some((s, note)) => (value(s), note.to_string()),
        None => (f64::NAN, "—".into()),
    }
}

/// Runs the surface reading comparisons whose short names contain the filter.
fn reads_rig(filter: &str) {
    if "patch".contains(filter) {
        verify_rig("patch");
    }
    if "le1".contains(filter) {
        verify_rig("LE1");
    }
    if "le10".contains(filter) {
        le10_series(&[3.0, 6.0, 12.0, 24.0]);
        println!();
        le10_series(&[2.0, 4.0, 8.0, 16.0]);
    }
    if "fields".contains(filter) {
        reads_fields();
    }
    if "gap".contains(filter) {
        reads_gap();
    }
    if "bar45".contains(filter) {
        reads_bar45();
    }
    if "bracket".contains(filter) {
        reads_bracket();
    }
}

/// A rotation about z alone turns the bar's axis within a lattice plane.
fn z45() -> [V3; 3] {
    let (s, c) = 45.0_f64.to_radians().sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// The normal stress along the unit axis a, with Voigt shears yz, xz and xy.
fn axial_stress(s: [f64; 6], [x, y, z]: V3) -> f64 {
    x * x * s[0] + y * y * s[1] + z * z * s[2]
        + 2.0 * (y * z * s[3] + x * z * s[4] + x * y * s[5])
}

/// Projects all three surface tensors onto the member's axis at an own-frame point and normal.
fn axial_reads(member: &Member, g: &Grid, u: &[V3], m: &Materials, fit: &Recovery<'_>, point: V3, normal: V3) -> ([f64; 3], String) {
    let (p, n) = (member.at(point), member.at(normal));
    let axis = member.at([1.0, 0.0, 0.0]);
    let axial = |s| axial_stress(s, axis);
    let element = a8_stress(m, g, u, p, n, false, [false; 3], None).map_or(f64::NAN, |(s, _)| axial(s));
    let recovered = fit.stress_at(m, p, n).map_or(f64::NAN, axial);
    let (a8, note) = noted_value(a8_stress(m, g, u, p, n, true, [true; 3], None), axial);
    ([element, recovered, a8], note)
}

/// Samples the tension and bending displacements at every node and compares surface stress
/// with the exact axial stress. Bending errors use the outer fibre even at the neutral axis.
fn reads_fields() {
    let m = bench::isotropic_of(bench::E, bench::NU);
    let frames = [("straight", turned(0.0)), ("z45", z45()), ("turned30", turned(30.0_f64.to_radians()))];
    let points = [
        ("+y", [8.0, 2.0, 0.0], [0.0, 1.0, 0.0]),
        ("-y", [8.0, -2.0, 0.0], [0.0, -1.0, 0.0]),
        ("edge", [8.0, 2.0, 2.0], [0.0, FRAC_1_SQRT_2, FRAC_1_SQRT_2]),
        ("+z", [8.0, 0.0, 2.0], [0.0, 0.0, 1.0]),
    ];
    let (k, nu) = (0.1, bench::NU);
    println!();
    println!("## Prescribed fields, axial stress errors");
    println!();
    println!("| frame | field | cells | point | element | 27-cell fit | A8 | A8 note |");
    println!("|---|---|---|---|---|---|---|---|");
    for (frame_name, frame) in frames {
        let member = Member { l: 16.0, size: [4.0, 4.0], round: false, frame };
        for cells in [3, 4, 6] {
            let h = 4.0 / f64::from(cells);
            let g = member.posed([0.0, 1.0, 0.0], h, [0.0; 3], &m).0;
            let nodes: usize = g.n.iter().map(|&n| n + 1).product();
            for bending in [false, true] {
                let field = if bending { "bending" } else { "tension" };
                let scale = if bending { bench::E * k * 2.0 } else { 1.0 };
                let u: Vec<V3> = (0..nodes).map(|node| {
                    let [x, y, z] = bench::to_own(&frame, g.node_pos(node));
                    let own = if bending {
                        [-x * y, (x * x + nu * (y * y - z * z)) / 2.0, nu * y * z].map(|v| k * v)
                    } else {
                        [x, -nu * y, -nu * z].map(|v| v / bench::E)
                    };
                    member.at(own)
                }).collect();
                let fit = Recovery::new(&g, &u);
                for (name, point, normal) in points {
                    let exact = if bending { -bench::E * k * point[1] } else { 1.0 };
                    let (reads, note) = axial_reads(&member, &g, &u, &m, &fit, point, normal);
                    let [element, recovered, a8] = reads.map(|v| 100.0 * (v - exact) / scale);
                    println!("| {frame_name} | {field} | {cells} | {name} | {element:+.4} % | {recovered:+.4} % | {a8:+.4} % | {note} |");
                }
            }
        }
    }
}

/// F50 (FORCE A10): two plates half a cell apart, each bent its own way, read on their inner
/// faces. They share no material, so neither's read may take a slope from the other. The gap
/// leaves one node plane under both (z = 2), so both fields are written about it and agree there.
fn reads_gap() {
    let m = bench::isotropic_of(bench::E, bench::NU);
    let nu = bench::NU;
    let h = 1.0;
    let (a_lo, a_hi) = ([0.0, 0.0, -2.0], [8.0, 8.0, 1.5]);
    let (b_lo, b_hi) = ([0.0, 0.0, 2.0], [8.0, 8.0, 4.0]);
    let (g, _, _) = bench::part([-1.0, -1.0, -3.0], [9.0, 9.0, 5.0], h, &m, |p| {
        bench::boxed(p, a_lo, a_hi).min(bench::boxed(p, b_lo, b_hi))
    });

    let (t, k_a, k_b) = (1e-3, 1e-3, -2e-3);
    let displacement = |k: f64, [x, y, z]: V3| {
        let zeta = z - 2.0;
        [
            t * x - k * x * zeta,
            -nu * t * y + nu * k * y * zeta,
            -nu * t * z + nu * k * zeta * zeta / 2.0,
        ]
    };
    let strain = |k: f64, [x, y, z]: V3| {
        let zeta = z - 2.0;
        let transverse = -nu * t + nu * k * zeta;
        [t - k * zeta, transverse, transverse, nu * k * y, -k * x, 0.0]
    };
    let nodes: usize = g.n.iter().map(|&n| n + 1).product();
    let u: Vec<V3> = (0..nodes).map(|node| {
        let p = g.node_pos(node);
        displacement(if p[2] < 1.75 { k_a } else { k_b }, p)
    }).collect();
    let fit = Recovery::new(&g, &u);

    let k = g.cell_ijk(g.locate([4.0, 4.0, 1.75]).0)[2];
    let mut fill = 0;
    for (c, octants) in g.gp.iter().enumerate() {
        if g.cell_ijk(c)[2] == k {
            for (q, gp) in octants.iter().enumerate() {
                if q & 4 != 0 {
                    fill = fill.max(gp.fill);
                }
            }
        }
    }
    println!();
    println!("## F50 two plates half a cell apart, inner-face strain errors");
    println!();
    println!("gap layer {k}: largest fill in its upper octants {fill} (0: the plates share no material)");
    println!();
    println!("| plate | point | cell | sound | error |");
    println!("|---|---|---|---|---|");
    for (plate, curvature, z, n) in [
        ("A", k_a, 1.5, [0.0, 0.0, 1.0]),
        ("B", k_b, 2.0, [0.0, 0.0, -1.0]),
    ] {
        for (x, y) in [(3.3, 4.6), (4.5, 2.7), (5.8, 5.2)] {
            let p = [x, y, z];
            let Some(c) = fit.cell_for(p, n) else {
                println!("| {plate} | ({x:.1}, {y:.1}, {z:.1}) | none | — | — |");
                continue;
            };
            let e = fit.fit(c).at(p);
            let exact = strain(curvature, p);
            let scale = exact.iter().map(|v| v.abs()).fold(0.0, f64::max);
            let error = (0..6).map(|i| (e[i] - exact[i]).abs()).fold(0.0, f64::max) / scale;
            let sound = if g.gp[c].iter().all(|gp| gp.fill >= 64) { "yes" } else { "no" };
            println!("| {plate} | ({x:.1}, {y:.1}, {z:.1}) | {:?} | {sound} | {error:.2e} |", g.cell_ijk(c));
        }
    }
}

/// This is the bar of FORCE A8's F45. Its axis and section rotate together about z alone,
/// putting the bending gradient across a lattice diagonal.
fn reads_bar45() {
    let m = bench::isotropic_of(bench::E, bench::NU);
    let member = Member { l: 40.0, size: [4.0, 4.0], round: false, frame: z45() };
    let axis = member.at([1.0, 0.0, 0.0]);
    let x = member.l / 2.0;
    let moment = member.l - x; // The tip load is 1 N along the member's y axis.
    let inertia = 4.0_f64.powi(4) / 12.0;
    let beam_stress = |y: f64| -moment * y / inertia;
    let sides = [("+y", 2.0_f64), ("-y", -2.0)];
    let mut signs = Vec::new();
    let mut fibre_reads = HashMap::new();
    println!();
    println!("## F45 cantilever, mid-length axial stress errors");
    println!();
    println!("| cells | point | reference (MPa) | element | 27-cell fit | A8 | carried (MPa) | element vs carried | 27-cell fit vs carried | A8 vs carried | A8 note | its |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|");
    for cells in [3, 4, 6] {
        let h = 4.0 / f64::from(cells);
        let (grid, fixed, force) = held(member.posed([0.0, 1.0, 0.0], h, [0.0; 3], &m));
        let near = vec![false; grid.gp.len()];
        let posed = fab_force::pose(grid, fixed, force, near).expect("the bar's root is held");
        // The static path and convergence settings of run_check, with solve starting from rest.
        let opts = Options { tolerance: 1e-10, max_iterations: 4000, ..Options::default() };
        let hier = Hierarchy::new(posed.grid, &posed.held, &m, &opts);
        let sol = fab_force::solve::solve(&hier, &posed.force, &opts);
        let g = &hier.levels[0].grid;
        let fit = Recovery::new(g, &sol.u);
        for (name, y) in sides {
            let reference = beam_stress(y);
            let (reads, note) = axial_reads(&member, g, &sol.u, &m, &fit, [x, y, 0.0], [0.0, y.signum(), 0.0]);
            let [element, recovered, a8] = reads.map(|v| 100.0 * (v / reference - 1.0));

            // Sample an interior cell halfway from the neutral axis to each fibre. The existing
            // isotropic point read at its centre gives a sign check independent of the surface reads.
            let c = g.locate(member.at([x, 0.5 * y, 0.0])).0;
            let center = g.cell_center(c);
            let own_y = bench::to_own(&member.frame, center)[1];
            let s = std::array::from_fn(|k| stress_point(bench::E, bench::NU, center, k)(g, &sol.u));
            let axial = axial_stress(s, axis);
            let agrees = g.active(c) && own_y * y > 0.0 && axial * own_y < 0.0;
            signs.push((cells, name, c, own_y, axial, agrees));
            let carried = axial * (y / own_y);
            let [element_carried, fit_carried, a8_carried] = reads.map(|v| 100.0 * (v / carried - 1.0));
            println!("| {cells} | {name} | {reference:.4} | {element:+.4} % | {recovered:+.4} % | {a8:+.4} % | {carried:.4} | {element_carried:+.4} % | {fit_carried:+.4} % | {a8_carried:+.4} % | {note} | {} |", sol.iterations);
            fibre_reads.insert((cells, name), reads);
        }
    }
    println!();
    println!("| cells | side | interior cell | centre own y (mm) | centre axial stress (MPa) | agrees with -M y / I | interior deficit | deficit x n^2 |");
    println!("|---|---|---|---|---|---|---|---|");
    for (cells, name, c, own_y, axial, agrees) in signs {
        let deficit = axial / beam_stress(own_y) - 1.0;
        let scaled = deficit * f64::from(cells).powi(2);
        println!("| {cells} | {name} | {c} | {own_y:+.4} | {axial:+.4} | {agrees} | {:+.4} % | {scaled:+.4} |", 100.0 * deficit);
    }
    println!();
    println!("| cells | side | read | coarse / fine (MPa) | reported with margin (MPa) | error | verdict |");
    println!("|---|---|---|---|---|---|---|");
    for (side, y) in sides {
        let coarse = fibre_reads[&(3, side)];
        let fine = fibre_reads[&(6, side)];
        for (i, read) in ["element", "27-cell fit", "A8"].into_iter().enumerate() {
            let (reported, verdict) = margin_read(coarse[i], fine[i]);
            let error = 100.0 * (reported / beam_stress(y) - 1.0);
            println!("| 3 / 6 | {side} | {read} | {:.4} / {:.4} | {reported:.4} | {error:+.2} % | {verdict} |", coarse[i], fine[i]);
        }
    }
}

/// The largest A8 von Mises stress on the surface, its quad, note and base cell centre.
fn a8_surface(g: &Grid, u: &[V3], m: &Materials, surface: &Surface) -> Option<(f64, usize, Note, V3)> {
    let mut cache = Mixtures::new();
    let point = |q: usize| surface.quad_center[q].map(f64::from);
    let normal = |q: usize| surface.quad_normal[q].map(f64::from);
    let (vm, q, note) = (0..surface.quads.len()).filter_map(|q| {
        let (s, note) = a8_stress(m, g, u, point(q), normal(q), true, [true; 3], Some(&mut cache))?;
        Some((von_mises(&s), q, note))
    }).max_by(|a, b| a.0.total_cmp(&b.0))?;
    let (base, _) = a8_base(g, point(q), normal(q), true)?;
    Some((vm, q, note, g.cell_center(base)))
}

/// Times three consecutive calls in milliseconds and keeps the last result.
fn three_passes<T>(mut pass: impl FnMut() -> T) -> ([f64; 3], T) {
    let mut timed = || {
        let start = Instant::now();
        let result = std::hint::black_box(pass());
        (start.elapsed().as_secs_f64() * 1e3, result)
    };
    let first = timed().0;
    let second = timed().0;
    let (third, result) = timed();
    ([first, second, third], result)
}

/// Compares the bracket's cell and surface extremes and the cost of each reading pass.
fn reads_bracket() {
    let m = Materials::new(PLA, &Print::default(), true);
    let (lo, hi) = BRACKET;
    let loads = bench::bracket_loads();
    let opts = Options::default();
    let mut field = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = bench::bracket(*p);
        }
    };
    for h in study::steps(lo, hi, 2, 0.1) {
        let model = Model::new(lo, hi, h, &mut field, &m);
        let conditions = model.conditions(&loads);
        let (held, force) = bc::nodes(&model.grid, &model.surface, &conditions);
        let hier = Hierarchy::new(model.grid.clone(), &held, &m, &opts);
        let start = Instant::now();
        let sol = fab_force::solve::solve(&hier, &force, &opts);
        let solve_ms = start.elapsed().as_secs_f64() * 1e3;
        let near = bc::near(&model.grid, &model.surface, &conditions);
        let grid = &hier.levels[0].grid;
        let cells = (0..grid.gp.len()).filter(|&c| grid.active(c)).count();
        println!();
        println!("## Bracket at h = {h:.3} mm");
        println!();
        println!("| h (mm) | active cells | solve (ms) | surface quads |");
        println!("|---|---|---|---|");
        println!("| {h:.3} | {cells} | {solve_ms:.2} | {} |", model.surface.quads.len());

        let (cell_ms, results) = three_passes(|| evaluate(grid, &m, &sol.u, &near, None));
        let sound_peak = results.cells.iter().enumerate().filter(|(c, _)| sound(grid, *c))
            .map(|(c, values)| (f64::from(values[0]), c)).max_by(|a, b| a.0.total_cmp(&b.0));
        println!();
        println!("| reading | largest von Mises (MPa) | source | cell | octant fills | sound | centre (mm) |");
        println!("|---|---|---|---|---|---|---|");
        for (name, peak) in [("all active cells", Some(results.max_vm)), ("sound cells", sound_peak)] {
            if let Some((vm, c)) = peak {
                let fills = grid.gp[c].map(|gp| gp.fill);
                println!("| {name} | {vm:.5} | cell reading | {c} | {fills:?} | {} | {:.3?} |", sound(grid, c), grid.cell_center(c));
            } else {
                println!("| {name} | — | — | — | — | — | — |");
            }
        }
        drop(results);

        let (fit_ms, fit) = three_passes(|| evaluate(grid, &m, &sol.u, &near, Some(&model.surface)));
        let (vm, c) = fit.max_vm;
        let source = fit.at_quad[0].map_or_else(|| "cell reading".to_string(), |q| format!("quad {q}"));
        let fills = grid.gp[c].map(|gp| gp.fill);
        println!("| 27-cell fit with surface | {vm:.5} | {source} | {c} | {fills:?} | {} | {:.3?} |", sound(grid, c), grid.cell_center(c));
        drop(fit);
        let (a8_ms, peak) = three_passes(|| a8_surface(grid, &sol.u, &m, &model.surface));
        println!();
        println!("| surface reading | largest von Mises (MPa) | quad | A8 note | base cell centre (mm) |");
        println!("|---|---|---|---|---|");
        if let Some((vm, q, note, center)) = peak {
            println!("| A8 | {vm:.5} | {q} | {note} | {center:.3?} |");
        } else {
            println!("| A8 | — | — | — | — |");
        }
        println!();
        println!("| pass | first (ms) | second (ms) | third (ms) |");
        println!("|---|---|---|---|");
        for (name, [first, second, third]) in [("evaluate cells", cell_ms), ("evaluate with surface (27-cell fit)", fit_ms), ("A8 surface", a8_ms)] {
            println!("| {name} | {first:.2} | {second:.2} | {third:.2} |");
        }
        println!();
        println!("The second A8 surface pass took {:.2} % of the solve.", 100.0 * a8_ms[1] / solve_ms);
    }
}

/// `hi` moved out so that `lo..hi` is whole cells of `h`: the grid's nodes then lie on `lo`'s
/// planes, where the rollers go.
fn whole_cells(lo: V3, hi: V3, h: f64) -> V3 {
    std::array::from_fn(|a| lo[a] + ((hi[a] - lo[a]) / h - 1e-9).ceil() * h)
}

/// The NAFEMS plate's outer edge: quads on its outer ellipse facing out.
fn outer_edge(c: V3, n: V3) -> bool {
    let (a, b) = (3250.0, 2750.0);
    let g = [c[0] / (a * a), c[1] / (b * b)];
    let r = (c[0] / a).powi(2) + (c[1] / b).powi(2);
    // the hole's edge reaches r 0.38 at most, the outer edge is r 1
    r > 0.7 && n[2].abs() < 0.5 && n[0] * g[0] + n[1] * g[1] > 0.5 * (g[0] * g[0] + g[1] * g[1]).sqrt()
}

/// The nodes a fixture on `region` holds (as [`bc::nodes`] picks them), held on the axes of
/// `axes` only.
fn hold_axes(g: &Grid, s: &Surface, region: &fab_force::surface::Region, held: &mut [u8], axes: u8) -> Vec<usize> {
    let mut picked = Vec::new();
    for &q in &region.faces {
        let c = s.quad_center[q as usize].map(|v| v as f64);
        let n = s.quad_normal[q as usize].map(|v| v as f64);
        for node in g.cell_nodes(g.locate(c).0) {
            let p = g.node_pos(node);
            if dot3([p[0] - c[0], p[1] - c[1], p[2] - c[2]], n).abs() <= 0.5 * g.h[0] {
                held[node] |= axes;
                picked.push(node);
            }
        }
    }
    picked
}

fn dot3(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Box cells of a part `lo..hi` at cells of `h`.
fn box_cells(lo: V3, hi: V3, h: f64) -> f64 {
    (0..3).map(|a| (hi[a] - lo[a]) / h + 2.0).product()
}

const MH_STRAIGHT: &str = "MacNeal and Harder 1985, as listed by Altair OptiStruct verification OS-V 0720 (Straight Cantilever Beam)";
const MH_CURVED: &str = "MacNeal and Harder 1985, as listed by Altair OptiStruct verification OS-V 0710 (Curved Cantilever Beam)";
const MH_TWISTED: &str = "MacNeal and Harder 1985, as listed by Abaqus Benchmarks 2.3.7 and Altair OS-V 0700 (Twisted Cantilever Beam)";

const NAFEMS_LE1: &str = "NAFEMS LE1, elliptic membrane (The Standard NAFEMS Benchmarks, 1990): sigma_yy at D 92.7 MPa, as listed by Altair OptiStruct verification OS-V 0010 (Elliptic Membrane)";
const NAFEMS_LE10: &str = "NAFEMS LE10, thick plate under pressure (The Standard NAFEMS Benchmarks, 1990): sigma_yy at D 5.38 MPa compressive, as listed by Abaqus Benchmarks \"LE10: Thick plate under pressure\" (its results negative) and Altair OptiStruct verification OS-V 0060";

/// `fab-forcebench le10`: FORCE A3 2.3's check of NAFEMS LE10's holds at 16 cells through its
/// thickness, apart from the quadrature. The benchmark holds x and y on the whole outer face
/// and z on that face's mid-plane line (EE') only. Prints the outer face's held nodes per axis
/// and their z range; the load's vertical sum against the pressure times the top face's area
/// (the quarter annulus between the 3250 x 2750 and 2000 x 1000 ellipses) and the vertical
/// reactions (`K u - f` at the held axes, `K` unheld); then sigma_yy at D (the cell's mean and
/// the point's) with today's holds and with z held on one node line, each mid-plane column of
/// the outer face's band held at its outermost node only.
fn le10_holds() {
    let steel = bench::isotropic_of(210.0e3, 0.3);
    let (n, t, p) = (16.0, 600.0, 1.0);
    let h = t / n;
    let lo = [0.0; 3];
    let hi = whole_cells(lo, [3250.0, 2750.0, t], h);
    let (grid, _, surface) = bench::part(lo, hi, h, &steel, |q| bench::nafems_plate(q, t));
    let top = surface.select(|_, nrm| nrm[2] > 0.9);
    let outer = surface.select(outer_edge);
    let (mut held, force) = bc::nodes(&grid, &surface, &[bc::Condition { kind: Kind::Pressure(p), region: top.clone() }]);
    bench::roller(&grid, &mut held, 0, 0.0);
    bench::roller(&grid, &mut held, 1, 0.0);
    let mut band = hold_axes(&grid, &surface, &outer, &mut held, 0b011);
    band.sort_unstable();
    band.dedup();
    let mid = |node: usize| (grid.node_pos(node)[2] - 0.5 * t).abs() < 1e-9;
    let today: Vec<usize> = band.iter().copied().filter(|&node| mid(node)).collect();
    // one node line: of the band's mid-plane nodes, the outermost (largest elliptic radius) at
    // each x column and at each y row, so the line follows the edge one node thick
    let r = |node: usize| {
        let q = grid.node_pos(node);
        (q[0] / 3250.0).powi(2) + (q[1] / 2750.0).powi(2)
    };
    let mut line: Vec<usize> = Vec::new();
    for axis in 0..2 {
        let mut best: std::collections::BTreeMap<i64, usize> = std::collections::BTreeMap::new();
        for &node in &today {
            let key = (grid.node_pos(node)[axis] / h).round() as i64;
            best.entry(key).and_modify(|b| if r(node) > r(*b) { *b = node }).or_insert(node);
        }
        line.extend(best.values());
    }
    line.sort_unstable();
    line.dedup();
    let zs = |nodes: &[usize]| nodes.iter().map(|&k| grid.node_pos(k)[2]).fold((f64::MAX, f64::MIN), |(a, b), z| (a.min(z), b.max(z)));
    let (b0, b1) = zs(&band);
    println!("## NAFEMS LE10's holds at {n} cells through t (h {h} mm)");
    println!();
    println!("- the outer face's band (`hold_axes`, x and y held): {} nodes, z {b0:.1} to {b1:.1}", band.len());
    println!("- z held today (the band at z = t/2): {} nodes; on one node line: {} nodes", today.len(), line.len());
    let held_on = |a: usize| held.iter().filter(|&&k| k & (1 << a) != 0).count();
    println!("- held per axis over the whole part (rollers included): x {}, y {}, z {} (today)", held_on(0), held_on(1), held_on(2) + today.len());
    let area = std::f64::consts::FRAC_PI_4 * (3250.0 * 2750.0 - 2000.0 * 1000.0);
    let fz: f64 = force.iter().map(|f| f[2]).sum();
    println!("- the load's z sum {fz:.0} N against p A {:.0} N (the top's picked area {:.0} mm^2 against {area:.0}): {:+.3} %", -p * area, top.area, 100.0 * (fz / (-p * area) - 1.0));
    println!();
    println!("| holds | z held nodes | its | sigma_yy at D, cell mean | at D itself | reactions z sum (N) | of p A |");
    println!("|---|---|---|---|---|---|---|");
    let free = vec![0u8; grid.nodes()];
    let unheld = fab_force::solve::Level::new(grid.clone(), &free, &steel);
    for (name, zline) in [("today: the band at z = t/2", &today), ("one node line at z = t/2", &line)] {
        let mut hz = held.clone();
        for &k in zline.iter() {
            hz[k] |= 0b100;
        }
        let near = vec![false; grid.cells()];
        let posed = fab_force::pose(grid.clone(), hz.clone(), force.clone(), near).expect("the plate is held");
        let opts = Options::default();
        let hier = Hierarchy::new(posed.grid.clone(), &posed.held, &steel, &opts);
        let sol = solve_until(&hier, &posed.force, None, &Options { tolerance: 1e-10, max_iterations: 4000, ..opts }, &mut |_, _| false);
        let d = [2000.0 + 1e-6 * h, 1e-6 * h, t - 1e-6 * h];
        let mean = stress_at(steel.clone(), d, 1)(&posed.grid, &sol.u);
        let point = stress_point(210.0e3, 0.3, d, 1)(&posed.grid, &sol.u);
        let mut ku = vec![[0.0; 3]; grid.nodes()];
        unheld.apply(&sol.u, &mut ku);
        let rz: f64 = (0..grid.nodes()).filter(|&k| posed.held[k] & 0b100 != 0).map(|k| ku[k][2] - force[k][2]).sum();
        println!("| {name} | {} | {} | {mean:.3} | {point:.3} | {rz:.0} | {:+.3} % |", zline.len(), sol.iterations, 100.0 * (rz / (p * area) - 1.0));
    }
    println!();
    println!("Reference: -5.38 MPa ({NAFEMS_LE10}).");
}

/// `fab-forcebench le10 16,32,64`: NAFEMS LE10's sigma_yy at D with the benchmark's holds (x and
/// y on the outer face, z on its mid-plane), solved to 1e-10 at each count of cells through
/// its thickness: the cell's mean, its stress at D itself and the stress recovered at D,
/// and how full D's cell is. Richardson over the finest three of each follows, and the
/// value the safety factor's verdict reports on each pair of
/// sizes, margin included (FORCE A6 2.2, F34). With an odd count no node lies on the
/// mid-plane: the layer half a cell under it is held, which removes the same rigid motion.
fn le10_series(cells: &[f64]) {
    let steel = bench::isotropic_of(210.0e3, 0.3);
    let (t, p) = (600.0, 1.0);
    println!("## NAFEMS LE10 sigma_yy at D, the benchmark's holds, by cells through t");
    println!();
    println!("| cells through t | h (mm) | z held (mm) | active cells | its | s | D's cell filled | at D itself | error | recovered | error | cell mean | error | A8 | error | A8, no y slope | error | element at D | error | A8 note |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let (mut points, mut recovered, mut means) = (Vec::new(), Vec::new(), Vec::new());
    let (mut a8s, mut no_y_slopes, mut elements) = (Vec::new(), Vec::new(), Vec::new());
    for &n in cells {
        let begun = Instant::now();
        let h = t / n;
        let lo = [0.0; 3];
        let hi = whole_cells(lo, [3250.0, 2750.0, t], h);
        let (grid, _, surface) = bench::part(lo, hi, h, &steel, |q| bench::nafems_plate(q, t));
        let top = surface.select(|_, nrm| nrm[2] > 0.9);
        let outer = surface.select(outer_edge);
        let (mut held, force) = bc::nodes(&grid, &surface, &[bc::Condition { kind: Kind::Pressure(p), region: top }]);
        bench::roller(&grid, &mut held, 0, 0.0);
        bench::roller(&grid, &mut held, 1, 0.0);
        let band = hold_axes(&grid, &surface, &outer, &mut held, 0b011);
        // the node layer nearest the mid-plane, the lower one at an odd count (half a cell off)
        let kz = ((0.5 * t - grid.origin[2]) / grid.h[2] + 1e-9).floor();
        let z_held = grid.origin[2] + kz * grid.h[2];
        for &k in &band {
            if (grid.node_pos(k)[2] - z_held).abs() < 1e-6 * grid.h[2] {
                held[k] |= 0b100;
            }
        }
        let active = (0..grid.cells()).filter(|&c| grid.active(c)).count();
        let posed = fab_force::pose(grid.clone(), held, force, vec![false; grid.cells()]).expect("the plate is held");
        let opts = Options::default();
        let hier = Hierarchy::new(posed.grid.clone(), &posed.held, &steel, &opts);
        let sol = solve_until(&hier, &posed.force, None, &Options { tolerance: 1e-10, max_iterations: 4000, ..opts }, &mut |_, _| false);
        let d = [2000.0 + 1e-6 * h, 1e-6 * h, t - 1e-6 * h];
        let mean = stress_at(steel.clone(), d, 1)(&posed.grid, &sol.u);
        let point = stress_point(210.0e3, 0.3, d, 1)(&posed.grid, &sol.u);
        let normal = [-FRAC_1_SQRT_2, 0.0, FRAC_1_SQRT_2];
        let at_surface = stress_recovered(steel.clone(), d, normal, 1)(&posed.grid, &sol.u);
        let (a8, note) = noted_value(a8_stress(&steel, &posed.grid, &sol.u, d, normal, true, [true; 3], None), |s| s[1]);
        let no_y = stress_a8(steel.clone(), d, normal, 1, [true, false, true])(&posed.grid, &sol.u);
        let element = stress_element(steel.clone(), d, normal, 1)(&posed.grid, &sol.u);
        let err = |v: f64| 100.0 * (v / -5.38 - 1.0);
        let filled = posed.grid.gp[posed.grid.locate(d).0].iter().map(|g| g.fill as f64).sum::<f64>() / (8.0 * 255.0);
        println!("| {n} | {h:.3} | {z_held:.1} | {active} | {} | {:.1} | {filled:.3} | {point:.4} | {:+.2} % | {at_surface:.4} | {:+.2} % | {mean:.4} | {:+.2} % | {a8:.4} | {:+.2} % | {no_y:.4} | {:+.2} % | {element:.4} | {:+.2} % | {note} |",
            sol.iterations, begun.elapsed().as_secs_f64(), err(point), err(at_surface), err(mean), err(a8), err(no_y), err(element));
        points.push(point);
        recovered.push(at_surface);
        means.push(mean);
        a8s.push(a8);
        no_y_slopes.push(no_y);
        elements.push(element);
    }
    println!();
    // Richardson over the finest three, cells doubling: the limit and the order
    let richardson = |v: &[f64]| {
        let [a, b, c] = [v[v.len() - 3], v[v.len() - 2], v[v.len() - 1]];
        let r = (a - b) / (b - c);
        (r > 0.0).then(|| (c - (b - c) / (r - 1.0), r.log2()))
    };
    let series = [("at D itself", &points), ("recovered", &recovered), ("cell mean", &means),
        ("A8", &a8s), ("A8, no y slope", &no_y_slopes), ("element at D", &elements)];
    if cells.len() >= 3 {
        for (name, v) in series {
            match richardson(v) {
                Some((limit, order)) => println!("- {name}: Richardson over the finest three {limit:.4} MPa ({:+.2} %), order {order:.2}", 100.0 * (limit / -5.38 - 1.0)),
                None => println!("- {name}: the finest three are not monotone, no Richardson limit"),
            }
        }
        println!();
    }
    println!("### Reported with the margin, each pair of sizes (FORCE A6 2.2, F34)");
    println!();
    println!("| cells | read | reported | error | verdict |");
    println!("|---|---|---|---|---|");
    for (name, v) in series {
        for i in 1..cells.len() {
            let (said, verdict) = margin_read(v[i - 1], v[i]);
            println!("| {} / {}, {name} | {:.4} / {:.4} | {said:.4} | {:+.2} % | {verdict} |", cells[i - 1], cells[i], v[i - 1], v[i], 100.0 * (said / -5.38 - 1.0));
        }
    }
    println!();
    println!("Reference: -5.38 MPa ({NAFEMS_LE10}).");
}

/// FORCE A6's margin on a stress read at two cell sizes, `coarse` then `fine`, as the safety
/// factor's verdict takes it (its safety factor goes as one over the stress): the stress it
/// reports, margin included, and the verdict.
fn margin_read(coarse: f64, fine: f64) -> (f64, &'static str) {
    let v = Verdict::of(1.0 / fine.abs(), Some(1.0 / coarse.abs()), None);
    let verdict = match v {
        Verdict::Unchecked(_) => "unchecked",
        Verdict::Converged { .. } => "converged",
        Verdict::Apart { .. } => "apart",
    };
    (fine.signum() / v.reported(), verdict)
}

/// `fab-forcebench f32cells [filter]`: FORCE A4 1.3's check (F21) of the GPU engine's f32 cell
/// product, with no GPU and no program change. On each of the coarse rig's cases, after the f64
/// solve `u` of the load `b` (to 1e-10): the analysed level applied to `u` rounded to f32 with
/// `gpu_cells`' and `gpu_gather`'s arithmetic emulated (each cell's f64 matrix rounded to f32,
/// unmasked as the GPU takes it; every multiply and add rounded, none fused; a node's eight
/// cells summed in the gather's order; held axes masked after), once as the kernel computes it
/// (plain) and once in A4 2.1's difference form (each axis's mean over the cell's corners taken
/// off `x` before the product and off the forces after). Prints `rho = u . (A32 u) / (u . b)`,
/// `|A32 u - b| / |b|`, and `rho` over the cells with a shared matrix and over those integrated
/// point by point apart (each cell's own energy, f32 against f64). Each case twice: `b` its
/// load, and its load turned a quarter about z (a column's axial load across it, so `u` is its
/// bending, the soft mode A4 1.1 is about). The cells integrated point
/// by point twice: their f64 matrices rounded, and assembled in f32 as `gpu_assemble` does (f32
/// weights, materials and shape gradients, f32 sums).
fn f32_cells_rig(filter: &str) {
    use fab_force::solve::{grads, CellStiffness, Level, EMPTY};
    println!("## The GPU engine's f32 cell product on the coarse rig's cases (FORCE A4 1.3, F21)");
    println!();
    println!("`u` the f64 solve to 1e-10; `A32 u`: `gpu_cells` and `gpu_gather` emulated in f32 on `u` rounded. Matrices: rounded, every cell's f64 matrix rounded to f32; assembled, the cells integrated point by point assembled in f32 as `gpu_assemble` does (the shared ones rounded). Each column: rho = `u . (A32 u) / (u . b)`, `|A32 u - b| / |b|` (each class of cells' own energy in f32 over f64: shared / point by point). Plain: the kernel's product; difference: A4 2.1's.");
    println!();
    println!("| case | cells: shared / point by point | rho f64 | plain, rounded | difference, rounded | plain, assembled | difference, assembled |");
    println!("|---|---|---|---|---|---|---|");
    let opts = Options::default();
    let tight = Options { tolerance: 1e-10, max_iterations: 4000, ..opts };
    for c in coarse_cases(filter) {
        let (grid, held, force, m) = (c.pose)();
        // the probe: the load's answer, and the answer to the load turned a quarter about z (a
        // column's axial load turned across it: its bending, the soft mode)
        for turned in [false, true] {
            let force: Vec<V3> = if turned { force.iter().map(|f| [f[1], -f[0], f[2]]).collect() } else { force.clone() };
            let hier = Hierarchy::new(grid.clone(), &held, &m, &opts);
            let s = solve_until(&hier, &force, None, &tight, &mut |_, _| false);
            let fine = &hier.levels[0];
            let g = &fine.grid;
            let nodes = g.nodes();
            let mut b = force.clone();
            fine.mask(&mut b);
            let dot = |a: &[V3], b: &[V3]| a.iter().zip(b).map(|(p, q)| p[0] * q[0] + p[1] * q[1] + p[2] * q[2]).sum::<f64>();
            let ub = dot(&s.u, &b);
            let mut au = vec![[0.0; 3]; nodes];
            fine.apply(&s.u, &mut au);
            let rho64 = dot(&s.u, &au) / ub;
            // which cells share a matrix (the rest are integrated point by point)
            let stiffness = fine.cell_stiffness();
            let shared_of: Vec<Option<bool>> = match &stiffness {
                CellStiffness::Points { cell, shared, .. } => cell.iter().map(|&i| (i != EMPTY).then_some((i as usize) < shared.len())).collect(),
                _ => (0..g.cells()).map(|c| g.active(c).then_some(true)).collect(),
            };
            // a cell integrated point by point as `gpu_assemble` assembles it: entry (r, c >= r) the
            // sum over the Gauss points of w (s_i0 g_x + s_i1 g_y + s_i2 g_z), s_i = D's row i
            // (columns 0..3 against the normal strains, 3..6 the shears) on the unit strain along r
            let dn = grads(g.h).0;
            let assembled = |c: usize| -> Option<[f32; 300]> {
                let CellStiffness::Points { cell, shared, materials, points, weights } = &stiffness else { return None };
                let i = cell[c] as usize;
                if cell[c] == EMPTY || i < shared.len() {
                    return None;
                }
                let (gp, w) = (&points[i - shared.len()], &weights[i - shared.len()]);
                let d32: Vec<[[f32; 6]; 6]> = materials.iter().map(|m| m.map(|row| row.map(|v| v as f32))).collect();
                let dot3 = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
                let mut packed = [0.0f32; 300];
                for r in 0..24 {
                    let (node, a) = (r / 3, r % 3);
                    for col in r..24 {
                        let (cn, b) = (col / 3, col % 3);
                        let idx = match b {
                            0 => [0, 5, 4],
                            1 => [5, 1, 3],
                            _ => [4, 3, 2],
                        };
                        let mut k = 0.0f32;
                        for q in 0..8 {
                            let m = &d32[gp[q] as usize];
                            let d = dn[q][node].map(|v| v as f32);
                            let (en, es) = match a {
                                0 => ([d[0], 0.0, 0.0], [0.0, d[2], d[1]]),
                                1 => ([0.0, d[1], 0.0], [d[2], 0.0, d[0]]),
                                _ => ([0.0, 0.0, d[2]], [d[1], d[0], 0.0]),
                            };
                            let stress = |i: usize| dot3([m[i][0], m[i][1], m[i][2]], en) + dot3([m[i][3], m[i][4], m[i][5]], es);
                            let gq = dn[q][cn].map(|v| v as f32);
                            k += w[q] as f32 * (stress(idx[0]) * gq[0] + stress(idx[1]) * gq[1] + stress(idx[2]) * gq[2]);
                        }
                        packed[r * (47 - r) / 2 + col] = k;
                    }
                }
                Some(packed)
            };
            // the matrices as the GPU takes them: not masked
            let free = Level::new(g.clone(), &vec![0u8; nodes], &m);
            let cells: Vec<usize> = (0..g.cells()).filter(|&c| shared_of[c].is_some()).collect();
            // each cell's matrix rounded, packed as the GPU's (the upper triangle by rows), and its
            // own f64 energy under u
            let (k32_rounded, energy64): (Vec<[f32; 300]>, Vec<f64>) = cells
                .iter()
                .map(|&c| {
                    let k = free.stiffness(c);
                    let mut packed = [0.0f32; 300];
                    for r in 0..24 {
                        for j in r..24 {
                            packed[r * (47 - r) / 2 + j] = k[r][j] as f32;
                        }
                    }
                    let corner = g.cell_nodes(c);
                    let ue: [f64; 24] = std::array::from_fn(|j| s.u[corner[j / 3]][j % 3]);
                    (packed, (0..24).map(|r| ue[r] * (0..24).map(|j| k[r][j] * ue[j]).sum::<f64>()).sum::<f64>())
                })
                .unzip();
            let x32: Vec<[f32; 3]> = s.u.iter().map(|v| v.map(|a| a as f32)).collect();
            // each node's cells by the corner it is of them (the gather's order)
            let mut at: Vec<[Option<usize>; 8]> = vec![[None; 8]; nodes];
            for (e, &c) in cells.iter().enumerate() {
                for (q, n) in g.cell_nodes(c).into_iter().enumerate() {
                    at[n][q] = Some(e);
                }
            }
            let k32_assembled: Vec<[f32; 300]> = cells.iter().zip(&k32_rounded).map(|(&c, k)| assembled(c).unwrap_or(*k)).collect();
            let shared_count = cells.iter().filter(|&&c| shared_of[c] == Some(true)).count();
            let mut columns = Vec::new();
            for (k32, difference) in [(&k32_rounded, false), (&k32_rounded, true), (&k32_assembled, false), (&k32_assembled, true)] {
                let cf: Vec<[f32; 24]> = cells
                    .iter()
                    .enumerate()
                    .map(|(e, &c)| {
                        let corner = g.cell_nodes(c);
                        let mut xs: [f32; 24] = std::array::from_fn(|j| x32[corner[j / 3]][j % 3]);
                        let mean = |v: &[f32; 24], a: usize| {
                            let mut t = 0.0f32;
                            for q in 0..8 {
                                t += v[q * 3 + a];
                            }
                            t / 8.0
                        };
                        if difference {
                            let means: [f32; 3] = std::array::from_fn(|a| mean(&xs, a));
                            for j in 0..24 {
                                xs[j] -= means[j % 3];
                            }
                        }
                        let k = &k32[e];
                        let mut f: [f32; 24] = std::array::from_fn(|r| {
                            let mut acc = 0.0f32;
                            for j in 0..24 {
                                // the packed upper triangle, as `gpu_cells` indexes it
                                let i = if j < r { j * (47 - j) / 2 + r } else { r * (47 - r) / 2 + j };
                                acc += k[i] * xs[j];
                            }
                            acc
                        });
                        if difference {
                            let means: [f32; 3] = std::array::from_fn(|a| mean(&f, a));
                            for j in 0..24 {
                                f[j] -= means[j % 3];
                            }
                        }
                        f
                    })
                    .collect();
                let mut y = vec![[0.0f64; 3]; nodes];
                for n in 0..nodes {
                    let mut sum = [0.0f32; 3];
                    for (q, e) in at[n].iter().enumerate() {
                        if let Some(e) = e {
                            for a in 0..3 {
                                sum[a] += cf[*e][q * 3 + a];
                            }
                        }
                    }
                    y[n] = sum.map(f64::from);
                }
                fine.mask(&mut y);
                let rho = dot(&s.u, &y) / ub;
                let miss: Vec<V3> = y.iter().zip(&b).map(|(p, q)| std::array::from_fn(|a| p[a] - q[a])).collect();
                let residual = (dot(&miss, &miss) / dot(&b, &b)).sqrt();
                // each class's own energy: u_e . (f32 forces) over u_e . K u_e
                let (mut e32, mut e64) = ([0.0f64; 2], [0.0f64; 2]);
                let e64_cell = &energy64;
                for (e, &c) in cells.iter().enumerate() {
                    let corner = g.cell_nodes(c);
                    let ue: [f64; 24] = std::array::from_fn(|j| s.u[corner[j / 3]][j % 3]);
                    let class = (shared_of[c] != Some(true)) as usize;
                    e32[class] += (0..24).map(|r| ue[r] * cf[e][r] as f64).sum::<f64>();
                    e64[class] += e64_cell[e];
                }
                let part = |k: usize| if e64[k] > 0.0 { format!("{:.4}", e32[k] / e64[k]) } else { "-".into() };
                columns.push(format!("{rho:.4}, {residual:.1e} ({} / {})", part(0), part(1)));
            }
            println!("| {}{} | {shared_count} / {} | {rho64:.6} | {} |", c.name, if turned { ", the load turned" } else { "" }, cells.len() - shared_count, columns.join(" | "));
        }
    }
}

fn verify_rig(filter: &str) {
    let mut checks: Vec<Check> = Vec::new();
    let mut left_out = Vec::new();
    // the patch test: a 4 x 2 x 2 box on rollers (x held on x = 0, y on y = 0, z on z = 0) pulled
    // at 1 MPa on x = 4. Its exact field is a uniform stress, u = (x, -nu y, -nu z) / E, which
    // trilinear cells hold exactly: it reads its largest node error over the end's 4 / E.
    let unit = bench::isotropic_of(bench::E, bench::NU);
    for n in [1.0, 2.0, 4.0] {
        let name = format!("patch: box 4 x 2 x 2 on rollers, uniform tension, n {n} aligned");
        if !name.contains(filter) {
            continue;
        }
        let h = 2.0 / n;
        let (lo, hi) = ([0.0; 3], [4.0, 2.0, 2.0]);
        let mc = unit.clone();
        let corner = hi.map(|x| x - 1e-6 * h);
        let normal = [1.0 / 3.0_f64.sqrt(); 3];
        let recovered = stress_recovered(unit.clone(), corner, normal, 0);
        let a8 = stress_a8(unit.clone(), corner, normal, 0, [true; 3]);
        let element = stress_element(unit.clone(), corner, normal, 0);
        checks.push(Check {
            name,
            series: "patch box".into(),
            h,
            size: box_cells(lo, hi, h),
            reference: 0.0,
            source: "the patch test: a uniform stress is exact (closed form)",
            pose: Box::new(move || {
                let (grid, _, surface) = bench::part(lo, hi, h, &mc, |p| bench::boxed(p, lo, hi));
                let end = bench::pick(&surface, [4.0, 1.0, 1.0], Pick::Face);
                let (mut held, force) = bc::nodes(&grid, &surface, &[bc::Condition { kind: Kind::Force([4.0, 0.0, 0.0]), region: end }]);
                for a in 0..3 {
                    bench::roller(&grid, &mut held, a, 0.0);
                }
                (grid, held, force)
            }),
            read: Box::new(|g: &Grid, u: &[V3]| {
                let mut worst: f64 = 0.0;
                for c in (0..g.cells()).filter(|&c| g.active(c)) {
                    for node in g.cell_nodes(c) {
                        let p = g.node_pos(node);
                        let exact = [p[0], -bench::NU * p[1], -bench::NU * p[2]].map(|v| v / bench::E);
                        for a in 0..3 {
                            worst = worst.max((u[node][a] - exact[a]).abs());
                        }
                    }
                }
                worst / (4.0 / bench::E)
            }),
            also: vec![
                ("recovered sigma_xx error at the far corner", Box::new(move |g: &Grid, u: &[V3]| recovered(g, u) - 1.0)),
                ("A8 sigma_xx error at the far corner", Box::new(move |g: &Grid, u: &[V3]| a8(g, u) - 1.0)),
                ("element sigma_xx error at the far corner", Box::new(move |g: &Grid, u: &[V3]| element(g, u) - 1.0)),
            ],
            m: unit.clone(),
        });
    }
    // a bar in tension: 8 x 2 x 2, its root held, 1 N along it at its tip; the strain along its
    // axis over its middle third against P / (E A), on grids aligned, shifted and turned
    for n in [2.0, 4.0, 8.0] {
        for align in Align::ALL {
            let name = format!("bar in tension, 8 x 2 x 2, middle-third strain, n {n} {}", align.name());
            if !name.contains(filter) {
                continue;
            }
            let turn = if align.turned() { TURN_DEG.to_radians() } else { 0.0 };
            let member = Member { l: 8.0, size: [2.0, 2.0], round: false, frame: turned(turn) };
            let h = 2.0 / n;
            let shift = if align.shifted() { study::shift(h) } else { [0.0; 3] };
            let (lo, hi) = member.bounds();
            let mc = unit.clone();
            checks.push(Check {
                name,
                series: format!("bar in tension, {}", align.name()),
                h,
                size: box_cells(lo, hi, h),
                reference: 1.0 / (bench::E * 4.0),
                source: "a bar's strain P / (E A) (closed form)",
                pose: Box::new(move || held(member.posed([1.0, 0.0, 0.0], h, shift, &mc))),
                read: Box::new(move |g: &Grid, u: &[V3]| {
                    let axis = member.at([1.0, 0.0, 0.0]);
                    let along = |x: f64| dot3(fab_force::results::displacement(g, u, member.at([x, 0.0, 0.0])), axis);
                    (along(16.0 / 3.0) - along(8.0 / 3.0)) / (8.0 / 3.0)
                }),
                also: Vec::new(),
                m: unit.clone(),
            });
        }
    }
    // MacNeal-Harder straight beam: 6 x 0.2 x 0.1, E 1e7, nu 0.3, unit loads at the tip
    let straight = bench::isotropic_of(1.0e7, 0.3);
    let loads: [(&str, Option<V3>, f64); 4] = [("extension", Some([1.0, 0.0, 0.0]), 3.0e-5), ("in-plane shear", Some([0.0, 1.0, 0.0]), 0.1081), ("out-of-plane shear", Some([0.0, 0.0, 1.0]), 0.4321), ("twist", None, 0.03208)];
    for n in [2.0, 4.0, 8.0] {
        for align in [Align::Aligned, Align::Shifted, Align::Turned] {
            for (what, load, reference) in loads {
                let turn = if align.turned() { TURN_DEG.to_radians() } else { 0.0 };
                let member = Member { l: 6.0, size: [0.2, 0.1], round: false, frame: turned(turn) };
                let h = 0.1 / n;
                let shift = if align.shifted() { study::shift(h) } else { [0.0; 3] };
                let name = format!("MH straight beam, {what}, n {n} {}", align.name());
                if !name.contains(filter) {
                    continue;
                }
                let (lo, hi) = member.bounds();
                let size = box_cells(lo, hi, h);
                if size > BOX_LIMIT {
                    left_out.push(name);
                    continue;
                }
                let m = straight.clone();
                let tip = member.l - 1e-6 * h;
                let read: Read = match load {
                    Some(v) => Box::new(move |g: &Grid, u: &[V3]| dot3(fab_force::results::displacement(g, u, member.at([tip, 0.0, 0.0])), member.at(v)).abs()),
                    // the twist: the tip's turn about the axis, from its movement across at y = +-0.05
                    None => Box::new(move |g: &Grid, u: &[V3]| {
                        let ez = member.at([0.0, 0.0, 1.0]);
                        let w = |y: f64| dot3(fab_force::results::displacement(g, u, member.at([tip, y, 0.0])), ez);
                        ((w(0.05) - w(-0.05)) / 0.1).abs()
                    }),
                };
                let mc = straight.clone();
                let pose: Box<dyn Fn() -> Posed3 + Sync> = match load {
                    Some(v) => Box::new(move || held(member.posed(v, h, shift, &mc))),
                    None => Box::new(move || held(member.posed_with(Kind::Torque(1.0), h, shift, &mc))),
                };
                let series = format!("MH straight beam, {what}, {}", align.name());
                checks.push(Check { name, series, h, size, reference, source: MH_STRAIGHT, pose, read, also: Vec::new(), m });
            }
        }
    }
    // MacNeal-Harder curved beam: radii 4.12 to 4.32, 0.1 wide, E 1e7, nu 0.25; root on y = 0,
    // tip on x = 0; in-plane load along y (radial at the tip), out-of-plane along z
    let (ri, ro, cw) = bench::CURVED;
    let curved = bench::isotropic_of(1.0e7, 0.25);
    for n in [2.0, 4.0, 8.0] {
        for align in [Align::Aligned, Align::Shifted] {
            for (what, v, reference) in [("in-plane", [0.0, 1.0, 0.0], 0.08734), ("out-of-plane", [0.0, 0.0, 1.0], 0.5022)] {
                let name = format!("MH curved beam, {what}, n {n} {}", align.name());
                if !name.contains(filter) {
                    continue;
                }
                let h = cw / n;
                let shift = if align.shifted() { study::shift(h) } else { [0.0; 3] };
                let (lo, hi) = ([0.0, 0.0, -0.5 * cw], [ro, ro, 0.5 * cw]);
                let mid = 0.5 * (ri + ro);
                let mc = curved.clone();
                checks.push(Check {
                    name,
                    series: format!("MH curved beam, {what}, {}", align.name()),
                    h,
                    size: box_cells(lo, hi, h),
                    reference,
                    source: MH_CURVED,
                    pose: Box::new(move || held(bench::cantilever_of(&bench::curved_beam, lo, hi, [mid, 0.0, 0.0], [0.0, mid, 0.0], Kind::Force(v), turned(0.0), h, shift, &mc))),
                    read: Box::new(move |g: &Grid, u: &[V3]| dot3(fab_force::results::displacement(g, u, [1e-6 * h, mid, 0.0]), v).abs()),
                    also: Vec::new(),
                    m: curved.clone(),
                });
            }
        }
    }
    // MacNeal-Harder twisted beam: 12 x 1.1 x 0.32 turning a quarter turn, E 29e6, nu 0.22; the
    // width along y at the root, along z at the tip: a load along z (the tip's width) bends the
    // root its weak way (0.005424), along y its strong way (0.001754)
    let (tl, tw, tt) = bench::TWISTED;
    let twisted = bench::isotropic_of(29.0e6, 0.22);
    let reach = 0.5 * (tw * tw + tt * tt).sqrt();
    for n in [2.0, 4.0, 8.0] {
        for align in [Align::Aligned, Align::Shifted] {
            for (what, v, reference) in [("load along z (the tip's width)", [0.0, 0.0, 1.0], 0.005424), ("load along y (the tip's thickness)", [0.0, 1.0, 0.0], 0.001754)] {
                let name = format!("MH twisted beam, {what}, n {n} {}", align.name());
                if !name.contains(filter) {
                    continue;
                }
                let h = tt / n;
                let shift = if align.shifted() { study::shift(h) } else { [0.0; 3] };
                let (lo, hi) = ([0.0, -reach, -reach], [tl, reach, reach]);
                let mc = twisted.clone();
                checks.push(Check {
                    name,
                    series: format!("MH twisted beam, {what}, {}", align.name()),
                    h,
                    size: box_cells(lo, hi, h),
                    reference,
                    source: MH_TWISTED,
                    pose: Box::new(move || held(bench::cantilever_of(&bench::twisted_beam, lo, hi, [0.0; 3], [tl, 0.0, 0.0], Kind::Force(v), turned(0.0), h, shift, &mc))),
                    read: Box::new(move |g: &Grid, u: &[V3]| dot3(fab_force::results::displacement(g, u, [tl - 1e-6 * h, 0.0, 0.0]), v).abs()),
                    also: Vec::new(),
                    m: twisted.clone(),
                });
            }
        }
    }
    // NAFEMS LE1 and LE10 (mm, MPa): a quarter of the elliptic plate on rollers on its cut planes,
    // in steel (E 210e3, nu 0.3). Each reads sigma_yy at D, the hole's edge on y = 0, by the
    // cell mean, at the point itself and by surface recovery. The rollers need aligned grids
    // with nodes on the cut planes.
    let steel = bench::isotropic_of(210.0e3, 0.3);
    // LE1: 100 thick (a thin solid, its faces free), 10 MPa pulling its outer edge out, its
    // mid-plane held in z
    for n in [2.0, 4.0, 8.0] {
        let name = format!("NAFEMS LE1 sigma_yy at D, {n} cells through t, aligned");
        if !name.contains(filter) {
            continue;
        }
        let t = 100.0;
        let h = t / n;
        let lo = [0.0; 3];
        let hi = whole_cells(lo, [3250.0, 2750.0, t], h);
        let mc = steel.clone();
        let d = [2000.0 + 1e-6 * h, 1e-6 * h, 0.5 * t + 1e-6 * h];
        checks.push(Check {
            name,
            series: "NAFEMS LE1 sigma_yy at D".into(),
            h,
            size: box_cells(lo, hi, h),
            reference: 92.7,
            source: NAFEMS_LE1,
            pose: Box::new(move || {
                let (grid, _, surface) = bench::part(lo, hi, h, &mc, |p| bench::nafems_plate(p, t));
                let outer = surface.select(outer_edge);
                let (mut held, force) = bc::nodes(&grid, &surface, &[bc::Condition { kind: Kind::Pressure(-10.0), region: outer }]);
                bench::roller(&grid, &mut held, 0, 0.0);
                bench::roller(&grid, &mut held, 1, 0.0);
                bench::roller(&grid, &mut held, 2, 0.5 * t);
                (grid, held, force)
            }),
            read: stress_at(steel.clone(), d, 1),
            also: vec![
                ("at D itself", stress_point(210.0e3, 0.3, d, 1)),
                ("recovered at D", stress_recovered(steel.clone(), d, [-1.0, 0.0, 0.0], 1)),
                ("A8 at D", stress_a8(steel.clone(), d, [-1.0, 0.0, 0.0], 1, [true; 3])),
                ("element at D", stress_element(steel.clone(), d, [-1.0, 0.0, 0.0], 1)),
            ],
            m: steel.clone(),
        });
    }
    // LE10: 600 thick, 1 MPa pressing its top face; its outer edge held in x and y, and in z
    // along that edge's mid-plane (EE'); D on the top face
    for n in [2.0, 4.0, 8.0, 16.0, 32.0] {
        let name = format!("NAFEMS LE10 sigma_yy at D, {n} cells through t, aligned");
        if !name.contains(filter) {
            continue;
        }
        let t = 600.0;
        let h = t / n;
        let lo = [0.0; 3];
        let hi = whole_cells(lo, [3250.0, 2750.0, t], h);
        let mc = steel.clone();
        let d = [2000.0 + 1e-6 * h, 1e-6 * h, t - 1e-6 * h];
        checks.push(Check {
            name,
            series: "NAFEMS LE10 sigma_yy at D".into(),
            h,
            size: box_cells(lo, hi, h),
            reference: -5.38,
            source: NAFEMS_LE10,
            pose: Box::new(move || {
                let (grid, _, surface) = bench::part(lo, hi, h, &mc, |p| bench::nafems_plate(p, t));
                let top = surface.select(|_, n| n[2] > 0.9);
                let outer = surface.select(outer_edge);
                let (mut held, force) = bc::nodes(&grid, &surface, &[bc::Condition { kind: Kind::Pressure(1.0), region: top }]);
                bench::roller(&grid, &mut held, 0, 0.0);
                bench::roller(&grid, &mut held, 1, 0.0);
                for node in hold_axes(&grid, &surface, &outer, &mut held, 0b011) {
                    if (grid.node_pos(node)[2] - 0.5 * t).abs() < 1e-9 {
                        held[node] |= 0b100;
                    }
                }
                (grid, held, force)
            }),
            read: stress_at(steel.clone(), d, 1),
            also: vec![
                ("at D itself", stress_point(210.0e3, 0.3, d, 1)),
                ("recovered at D", stress_recovered(steel.clone(), d, [-FRAC_1_SQRT_2, 0.0, FRAC_1_SQRT_2], 1)),
            ],
            m: steel.clone(),
        });
    }
    // the buckling rig's a/t 20 plate held the same way (the edges' mid-plane lines held across
    // it: a soft simple support), pressed by 0.001 MPa on its top face: its centre's movement
    // against Kirchhoff's thin plate, which the plate's shear and its soft support exceed
    let unit_plate = bench::isotropic_of(bench::E, bench::NU);
    for n in [2.0, 4.0, 8.0] {
        let name = format!("plate 40 x 40 x 2 pressed, centre deflection, n {n} aligned");
        if !name.contains(filter) {
            continue;
        }
        let (a, th, q) = (40.0, 2.0, 0.001);
        let h = th / n;
        let mc = unit_plate.clone();
        checks.push(Check {
            name,
            series: "plate 40 x 40 x 2 pressed, centre deflection".into(),
            h,
            size: box_cells([0.0; 3], [a, a, th], h),
            reference: bench::plate_deflection(a, th, q, bench::E, bench::NU),
            source: "Kirchhoff's thin plate, 0.00406 q a^4 / D, read on LIRA-FEM's verification test 1.46 (help.liraland.com; Birger and Panovko 1968)",
            pose: Box::new(move || bench::plate_pressed(a, th, h, q, &mc)),
            read: Box::new(move |g: &Grid, u: &[V3]| -fab_force::results::displacement(g, u, [0.5 * a, 0.5 * a, 0.5 * th])[2]),
            also: Vec::new(),
            m: unit_plate.clone(),
        });
    }
    // the bench cantilever, 24 x 4 x 4, 1 N down at its tip: Timoshenko's tip deflection
    let iso = bench::isotropic();
    for n in [2.0, 4.0, 8.0] {
        for align in [Align::Aligned, Align::Shifted] {
            let name = format!("cantilever 24 x 4 x 4, n {n} {}", align.name());
            if !name.contains(filter) {
                continue;
            }
            let (l, sec) = bench::BEAM;
            let h = sec / n;
            let shift = if align.shifted() { study::shift(h) } else { [0.0; 3] };
            let mc = iso.clone();
            checks.push(Check {
                name,
                series: format!("cantilever 24 x 4 x 4, {}", align.name()),
                h,
                size: box_cells([0.0; 3], [l, sec, sec], h),
                reference: bench::cantilever_tip(),
                source: "beam theory with shear (Timoshenko), bench::cantilever_tip",
                pose: Box::new(move || held(bench::cantilever_at(h, shift, &mc))),
                read: Box::new(move |g: &Grid, u: &[V3]| -fab_force::results::displacement(g, u, [l - 1e-6 * h, 0.5 * sec, 0.5 * sec])[2]),
                also: Vec::new(),
                m: iso.clone(),
            });
        }
    }
    let t = Instant::now();
    let solved = parallel(&checks, |c| c.size, run_check, |c| c.name.clone());
    // each reading a row: (case, series, h, reference, value, active cells, CG its)
    let mut rows: Vec<(String, String, f64, f64, f64, usize, usize)> = Vec::new();
    for (c, (value, also, active, its)) in checks.iter().zip(solved) {
        rows.push((c.name.clone(), c.series.clone(), c.h, c.reference, value, active, its));
        for ((what, _), v) in c.also.iter().zip(also) {
            rows.push((format!("{}, {what}", c.name), format!("{}, {what}", c.series), c.h, c.reference, v, active, its));
        }
    }
    println!("## Static solver against published values (solved to 1e-10)");
    println!();
    println!("| case | active cells | reads | reference | error | CG its |");
    println!("|---|---|---|---|---|---|");
    for (name, _, _, reference, value, active, its) in &rows {
        if *reference == 0.0 {
            println!("| {name} | {active} | {value:.2e} | exact | {:.2e} % | {its} |", 100.0 * value);
        } else {
            println!("| {name} | {active} | {value:.5e} | {reference:.5e} | {:+.2} % | {its} |", 100.0 * (value / reference - 1.0));
        }
    }
    for name in &left_out {
        println!("| {name}: left out, its grid box over {BOX_LIMIT:.0} cells | | | | | |");
    }
    println!();
    // Richardson over each series' finest three steps (each half the last's cells): the order
    // the steps converge at and the value they head to, f3 + (f3 - f2) / (2^p - 1)
    let mut series: Vec<&str> = Vec::new();
    for (_, s, _, reference, ..) in &rows {
        if *reference != 0.0 && !series.contains(&s.as_str()) {
            series.push(s);
        }
    }
    let mut extrapolated = Vec::new();
    for s in series {
        let mut steps: Vec<(f64, f64, f64)> = rows.iter().filter(|r| r.1 == s).map(|r| (r.2, r.4, r.3)).collect();
        steps.sort_by(|a, b| b.0.total_cmp(&a.0));
        if steps.len() < 3 {
            continue;
        }
        let [a, b, c] = [steps[steps.len() - 3], steps[steps.len() - 2], steps[steps.len() - 1]];
        if (a.0 / b.0 - 2.0).abs() > 1e-9 || (b.0 / c.0 - 2.0).abs() > 1e-9 {
            continue;
        }
        let (d1, d2) = (b.1 - a.1, c.1 - b.1);
        let row = if d1 * d2 > 0.0 && d2.abs() < d1.abs() {
            let p = (d1 / d2).log2();
            let value = c.1 + d2 / (2f64.powf(p) - 1.0);
            format!("| {s} | {:.5e} | {p:.2} | {value:.5e} | {:.5e} | {:+.2} % |", c.1, c.2, 100.0 * (value / c.2 - 1.0))
        } else {
            format!("| {s} | {:.5e} | not monotone | | {:.5e} | |", c.1, c.2)
        };
        extrapolated.push(row);
    }
    if !extrapolated.is_empty() {
        println!("## Richardson extrapolation over each series' finest three steps");
        println!();
        println!("| series | finest | order | extrapolated | reference | error |");
        println!("|---|---|---|---|---|---|");
        for row in extrapolated {
            println!("{row}");
        }
        println!();
    }
    // FORCE A6's margin (F34) on each stress series' pairs of steps, as the safety factor's
    // verdict reports it
    let mut stress: Vec<&str> = Vec::new();
    for (_, s, ..) in &rows {
        if s.contains("sigma") && !stress.contains(&s.as_str()) {
            stress.push(s);
        }
    }
    if !stress.is_empty() {
        println!("## Reported with the margin on each pair of steps (FORCE A6 2.2, F34)");
        println!();
        println!("| series | cells | read | reported | reference | error | verdict |");
        println!("|---|---|---|---|---|---|---|");
        for s in stress {
            let mut steps: Vec<(f64, f64, f64)> = rows.iter().filter(|r| r.1 == s).map(|r| (r.2, r.4, r.3)).collect();
            steps.sort_by(|a, b| b.0.total_cmp(&a.0));
            for w in steps.windows(2).filter(|w| (w[0].0 / w[1].0 - 2.0).abs() < 1e-9) {
                let (said, verdict) = margin_read(w[0].1, w[1].1);
                println!("| {s} | {:.3} / {:.3} mm | {:.5e} / {:.5e} | {said:.5e} | {:.5e} | {:+.2} % | {verdict} |", w[0].0, w[1].0, w[0].1, w[1].1, w[0].2, 100.0 * (said / w[0].2 - 1.0));
            }
        }
        println!();
    }
    let mut sources: Vec<&str> = checks.iter().map(|c| c.source).collect();
    sources.dedup();
    for s in sources {
        println!("- Reference: {s}.");
    }
    println!("- The straight beam's twist: the published 0.03208 rad is 6 % under elasticity's (Saint-Venant J of the 0.2 x 0.1 section: {:.5} rad); both are shown by this note, the table compares with the published value.", 6.0 / (1.0e7 / 2.6 * bench::torsion_constant(0.1, 0.2)));
    println!("- The stress rows read the mean of the cell just inside D, its stress at D by its shape functions, and the stress recovered at D as the Force view reads the surface.");
    println!("- The pressed plate: Kirchhoff's thin plate is the reference; the plate's shear and its soft support (edges free to twist) make the solid move more.");
    println!();
    println!("{} cases in {:.0} s", checks.len(), t.elapsed().as_secs_f64());
}

/// A case of the coarse and engines rigs.
struct Case {
    name: String,
    /// The grid, its held axes, its load and its materials.
    pose: Box<dyn Fn() -> (Grid, Vec<u8>, Vec<V3>, Materials) + Sync>,
    oracle: bool,
    lobpcg: bool,
    size: f64,
}

/// The coarse rig's cases whose name holds `filter`: the slender columns, the strips, the
/// pressed plate and the bracket (FORCE A3 E3 and F12).
fn coarse_cases(filter: &str) -> Vec<Case> {
    let iso = bench::isotropic;
    let member_case = |name: String, member: Member, load: V3, h: f64, shift: bool, oracle: bool, lobpcg: bool| {
        let (lo, hi) = member.bounds();
        let size = (0..3).map(|a| (hi[a] - lo[a]) / h + 2.0).product();
        Case {
            name,
            pose: Box::new(move || {
                let m = iso();
                let shift = if shift { study::shift(h) } else { [0.0; 3] };
                let (grid, surface, conditions) = member.posed(load, h, shift, &m);
                let (held, force) = bc::nodes(&grid, &surface, &conditions);
                let near = vec![false; grid.cells()];
                let posed = fab_force::pose(grid, held, force, near).expect("the member's root is held");
                (std::sync::Arc::unwrap_or_clone(posed.grid), posed.held, posed.force, m)
            }),
            oracle,
            lobpcg,
            size,
        }
    };
    let mut cases = Vec::new();
    for (round, ratio, cells, align, oracle) in [
        (false, 40.0, 4.0, Align::Aligned, true),
        (false, 40.0, 4.0, Align::Shifted, true),
        (false, 40.0, 4.0, Align::Turned, false),
        (true, 40.0, 4.0, Align::Aligned, true),
        (false, 40.0, 2.0, Align::Aligned, true),
        (true, 40.0, 2.0, Align::Aligned, true),
        // F12's slender additions: the coarsest level's condition grows as about (L/t)^4
        (false, 100.0, 2.0, Align::Aligned, true),
        (false, 100.0, 4.0, Align::Aligned, true),
    ] {
        let c = Column { round, ratio, cells, align };
        cases.push(member_case(c.name(), c.member(), [-1.0, 0.0, 0.0], c.h(), align.shifted(), oracle, true));
    }
    let s = Strip { cells: 2.0, align: Align::Aligned };
    cases.push(member_case(s.name(), s.member(), [0.0, 0.0, -1.0], s.h(), false, true, true));
    // the strip at L/d 20, whose static solve took 1204 iterations to 1e-10 (E0)
    cases.push(member_case("strip 160 n 3 aligned".into(), bench::strip(160.0, STRIP.1, STRIP.2, turned(0.0)), [0.0, 0.0, -1.0], STRIP.2 / 3.0, false, false, false));
    // past the GPU engine's f32 floor (FORCE A4 F25: the f64 engine finishes)
    cases.push(member_case("strip 500 n 2 aligned".into(), bench::strip(500.0, STRIP.1, STRIP.2, turned(0.0)), [0.0, 0.0, -1.0], STRIP.2 / 2.0, false, false, false));
    // F12's plate: 200 x 200 x 2 (a/t 100), simply supported along its mid-plane edges, pressed
    cases.push(Case {
        name: "plate 200 x 200 x 2 n 2 pressed".into(),
        pose: Box::new(|| {
            let m = bench::isotropic();
            let (grid, held, force) = bench::plate_pressed(200.0, 2.0, 1.0, 0.001, &m);
            (grid, held, force, m)
        }),
        oracle: false,
        lobpcg: false,
        size: 202.0 * 202.0 * 4.0,
    });
    // the bracket at the app's finer study step, as printed in PLA
    let (lo, hi) = BRACKET;
    let h = *study::steps(lo, hi, 2, 0.1).last().unwrap();
    cases.push(Case {
        name: format!("bracket h {h:.3}"),
        pose: Box::new(move || {
            let m = Materials::new(PLA, &Print::default(), true);
            let mut field = |ps: &[V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = bench::bracket(*p));
            let model = Model::new(lo, hi, h, &mut field, &m);
            let conditions = model.conditions(&bench::bracket_loads());
            let (held, force) = bc::nodes(&model.grid, &model.surface, &conditions);
            let near = vec![false; model.grid.cells()];
            let posed = fab_force::pose(model.grid.clone(), held, force, near).expect("the bracket is held");
            (std::sync::Arc::unwrap_or_clone(posed.grid), posed.held, posed.force, m)
        }),
        oracle: false,
        lobpcg: true,
        size: (0..3).map(|a| (hi[a] - lo[a]) / h + 2.0).product(),
    });
    cases.retain(|c| c.name.contains(filter));
    cases
}

/// A solve by the Force program in this process: its iterations, wall time, the factor's kernels'
/// time, and on the GPU engine its runs and iterations sent.
#[cfg(feature = "loopback")]
struct Engine {
    its: usize,
    wall: f64,
    factor_ms: f64,
    runs: f64,
    sent: f64,
    /// Loads the f64 engine finished past the GPU's f32 floor.
    finished: f64,
    /// The answer's residual (its share of the load).
    residual: f64,
    /// On the GPU engine: the program's waits for the device an iteration sent (ms; the device's
    /// kernels are not timed one by one, so its whole work, the setup's assembly included).
    wait_ms_it: f64,
}

/// `f` solved from rest by the Force program on `fine` (the GPU engine with `gpu`), as a hub
/// solves it. The kernels that build the coarsest level's factor are the `dense_` ones but its
/// four solve kernels.
#[cfg(feature = "loopback")]
fn engine(fine: &fab_force::solve::Level, f: &[V3], opts: &Options, gpu: Option<makepad_compute_program::gpu::GpuLink>) -> Result<(Engine, Vec<V3>), String> {
    engine_solving(fine, f, opts, gpu, fab_force::program::Solves::ToTolerance)
}

/// [`engine`], its load solved as `solves` says (one GPU run for a preconditioner's).
#[cfg(feature = "loopback")]
fn engine_solving(fine: &fab_force::solve::Level, f: &[V3], opts: &Options, gpu: Option<makepad_compute_program::gpu::GpuLink>, solves: fab_force::program::Solves) -> Result<(Engine, Vec<V3>), String> {
    use fab_force::program;
    use makepad_compute_program::{Session, SessionConfig, Updates, Value};
    let (params, inputs) = program::job_block(fine, &[f], None, opts, solves)?;
    let t = Instant::now();
    let session = Session::open(&program::program(), SessionConfig { params, inputs, limits: program::limits(), gpu, ..SessionConfig::default() });
    let mut all = Updates::default();
    let s = loop {
        all.merge(session.wait());
        if let Some(done) = program::answer(&all) {
            break done?;
        }
    };
    let wall = t.elapsed().as_secs_f64();
    let Some(Ok(end)) = &all.end else { return Err("no end".into()) };
    let gpu = |k: &str| end.get("gpu").and_then(|g| g.get(k)).and_then(Value::as_number).unwrap_or(0.0);
    let solving = ["dense_gather", "dense_lower", "dense_upper", "dense_scatter"];
    let kernels: Vec<(String, f64, f64)> = match end.get("stats").and_then(|s| s.get("kernels")) {
        Some(Value::Array(kernels)) => kernels
            .iter()
            .filter_map(|k| match k.get("label") {
                Some(Value::String(l)) => Some((l.to_string(), k.get("ms").and_then(Value::as_number)?, k.get("calls").and_then(Value::as_number)?)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let factor_ms = kernels.iter().filter(|(l, ..)| l.starts_with("dense_") && !solving.contains(&l.as_str())).fold(0.0, |a, k| a + k.1);
    let waits_ms = kernels.iter().find(|(l, ..)| l == "(waits for the GPU)").map_or(0.0, |k| k.1);
    let wait_ms_it = waits_ms / gpu("sent").max(1.0);
    Ok((Engine { its: s.iterations, wall, factor_ms, runs: gpu("runs"), sent: gpu("sent"), finished: gpu("finished"), residual: s.residual, wait_ms_it }, s.u))
}

/// `fab-forcebench engines [filter]` (built with the `loopback` feature: this machine's GPU
/// device): the coarse rig's cases solved by the Force program as a hub solves them, from rest,
/// one case at a time: its CPU engine (f64) and its GPU engine (f32 runs corrected in f64), to
/// 1e-6 and to buckling's preconditioner tolerance (1e-2); the CPU solver's iterations beside
/// them; and where the case runs LOBPCG, LOBPCG cold with the GPU engine's solves as its
/// preconditioner (as Fab's buckling stage runs it): its cost in static solves (the GPU engine's
/// at 1e-6), and the runs its solves took. FORCE A3 F9', F13 to F16. Each program solve runs
/// twice, the second timed: the first compiles its kernels.
#[cfg(feature = "loopback")]
fn engines_rig(filter: &str) {
    let device = makepad_compute_program_gpu::spawn().unwrap_or_else(|e| panic!("this machine's GPU device: {e:?}"));
    println!("## The Force program's engines on the coarse rig's cases");
    println!();
    println!("From rest. CPU solver: `fab_force::solve` in f64. Program: its CPU engine (f64) and its GPU engine (on this machine's GPU), iterations (s; the factor's kernels' ms); GPU: iterations, runs, iterations sent, `f64 finish` where the f64 engine finished past the f32 floor, the residual reached (s; factor ms; the program's waits for the device an iteration sent). 1e-2: buckling's preconditioner tolerance. LOBPCG: cold, the GPU engine's solves as its preconditioner, one run each as the app's: cost in the GPU engine's static solves, steps (solves, their mean iterations, the most runs one took), factor; and the CPU solver's (`solve.rs`, to 1e-2) steps and factor.");
    println!();
    println!("| case | levels | coarsest dofs | CPU solver its | program CPU its (s; factor ms) | program GPU its, runs, sent (s; factor ms) | 1e-2: CPU its / GPU its, runs, sent | LOBPCG on the GPU engine | LOBPCG s |");
    println!("|---|---|---|---|---|---|---|---|---|");
    let opts = Options::default();
    let loose = Options { tolerance: 1e-2, ..opts };
    for c in coarse_cases(filter) {
        let (grid, held, force, m) = (c.pose)();
        let hier = Hierarchy::new(grid, &held, &m, &opts);
        let cpu = solve_until(&hier, &force, None, &opts, &mut |_, _| false);
        let fine = &hier.levels[0];
        let coarsest = hier.levels.last().unwrap();
        let dofs = (0..coarsest.grid.nodes() * 3).filter(|&d| coarsest.held[d / 3] & (1 << (d % 3)) == 0).count();
        let timed = |o: &Options, gpu: Option<makepad_compute_program::gpu::GpuLink>| {
            engine(fine, &force, o, gpu.clone()).and_then(|_| engine(fine, &force, o, gpu)).map(|(e, _)| e)
        };
        let show = |e: &Result<Engine, String>, on_gpu: bool| match e {
            Ok(e) if on_gpu => format!(
                "{}, {:.0}, {:.0}{}, {:.0e} ({:.2}; {:.0}; {:.3} ms an it)",
                e.its, e.runs, e.sent, if e.finished > 0.0 { ", f64 finish" } else { "" }, e.residual, e.wall, e.factor_ms, e.wait_ms_it
            ),
            Ok(e) => format!("{} ({:.2}; {:.0})", e.its, e.wall, e.factor_ms),
            Err(e) => format!("failed: {e}"),
        };
        let here = timed(&opts, None);
        let there = timed(&opts, Some(device.clone()));
        let loose_cpu = timed(&loose, None);
        let loose_gpu = timed(&loose, Some(device.clone()));
        let (lobpcg, lobpcg_s) = match (&there, c.lobpcg) {
            (Ok(stat), true) => {
                let t = Instant::now();
                let (mut its, mut solves, mut most_runs) = (0, 0, 0.0f64);
                let mut pre = |block: &[Vec<V3>]| {
                    let mut out = Vec::with_capacity(block.len());
                    for r in block {
                        let (e, u) = engine_solving(fine, r, &loose, Some(device.clone()), fab_force::program::Solves::OneRun)?;
                        its += e.its;
                        solves += 1;
                        most_runs = most_runs.max(e.runs);
                        out.push(u);
                    }
                    Ok(Some(out))
                };
                let modes = buckle::lobpcg(fine, &cpu.u, Start::Rotations, &Loose::default(), &mut pre, &mut |_, _, _| false);
                let s = t.elapsed().as_secs_f64();
                match modes {
                    Ok(Some(modes)) => {
                        // the CPU solver's LOBPCG beside it (its steps: FORCE A4 F16')
                        let on_cpu = fast_counted(&hier, &force, &cpu.u, &opts);
                        (
                            format!(
                                "{:.1}, {} ({}, {:.1} its, {:.0} runs at most) {:.4}; CPU solver {} steps, {:.4}",
                                its as f64 / stat.its.max(1) as f64, modes.steps, solves, its as f64 / solves.max(1) as f64, most_runs, modes.factor(0), on_cpu.steps, on_cpu.factor
                            ),
                            format!("{s:.1}"),
                        )
                    }
                    Ok(None) => ("stopped".into(), "-".into()),
                    Err(e) => (format!("failed: {e}"), "-".into()),
                }
            }
            _ => ("-".into(), "-".into()),
        };
        println!(
            "| {} | {} | {dofs} | {} | {} | {} | {} / {} | {lobpcg} | {lobpcg_s} |",
            c.name,
            hier.levels.len(),
            cpu.iterations,
            show(&here, false),
            show(&there, true),
            loose_cpu.as_ref().map_or_else(|e| format!("failed: {e}"), |e| e.its.to_string()),
            show(&loose_gpu, true),
        );
    }
}

/// The multigrid's coarsest level solved exactly ([`fab_force::solve::Dense`]), its inverse
/// factor in f64 and narrowed to the GPU engine's f32 (FORCE A3 F12), on the slender cases where
/// the coarsest level is worst conditioned and buckling's solves cost most. Per case and
/// precision: the static solve's iterations (to the product's 1e-6 and to 1e-10), the oracle's
/// iterations a solve (its tight solves of the bending modes), and LOBPCG's cost as `buckle`
/// counts it.
fn coarse_rig(filter: &str) {
    let cases = coarse_cases(filter);
    // the coarsest level solved exactly, its inverse factor in f64 and rounded to f32 with f32
    // products (the GPU engine's arithmetic): FORCE A3 F12
    const COARSEST: [(&str, bool); 2] = [("exact, f64", false), ("exact, f32 factor", true)];
    let t = Instant::now();
    let rows = parallel(
        &cases,
        |c| c.size,
        |c| {
            let (grid, held, force, m) = (c.pose)();
            let opts = Options::default();
            let t = Instant::now();
            let mut hier = Hierarchy::new(grid.clone(), &held, &m, &opts);
            let setup = t.elapsed().as_secs_f64();
            // the factor alone, timed again
            let t = Instant::now();
            hier.coarsest = Dense::of(hier.levels.last().unwrap());
            let factor = t.elapsed().as_secs_f64();
            COARSEST.map(|(_, narrow)| {
                if narrow {
                    hier.coarsest.narrow();
                }
                let coarsest = hier.levels.last().unwrap();
                let (size, dropped) = (hier.coarsest.size(), hier.coarsest.dropped);
                // the factor's own check: A x against b for x = A^-1 b, b = A of a spread vector
                let exactness = {
                    let d = &hier.coarsest;
                    let n = coarsest.grid.nodes();
                    let mut v: Vec<V3> = (0..n).map(|i| std::array::from_fn(|a| ((i * 3 + a) as f64 * 0.7).sin())).collect();
                    coarsest.mask(&mut v);
                    let mut b = vec![[0.0; 3]; n];
                    coarsest.apply(&v, &mut b);
                    let mut x = vec![[0.0; 3]; n];
                    d.solve(&b, &mut x);
                    let mut ax = vec![[0.0; 3]; n];
                    coarsest.apply(&x, &mut ax);
                    let norm = |w: &[V3]| w.iter().flatten().map(|x| x * x).sum::<f64>().sqrt();
                    let diff: Vec<V3> = ax.iter().zip(&b).map(|(p, q)| std::array::from_fn(|a| p[a] - q[a])).collect();
                    norm(&diff) / norm(&b)
                };
                let t = Instant::now();
                let product = solve_until(&hier, &force, None, &opts, &mut |_, _| false);
                let product_s = t.elapsed().as_secs_f64();
                let tight = solve_until(&hier, &force, None, &Options { tolerance: 1e-10, max_iterations: 4000, ..opts }, &mut |_, _| false);
                let oracle = c.oracle.then(|| {
                    let e = Eigen { solve: Options { tolerance: 1e-8, max_iterations: 4000, ..opts }, ..Eigen::default() };
                    let modes = buckle::lowest(&hier, &tight.u, &e, &mut |_, _| false);
                    (modes.factor(0), modes.cg as f64 / (modes.steps * e.block).max(1) as f64)
                });
                let t = Instant::now();
                let fast = c.lobpcg.then(|| fast_counted(&hier, &force, &tight.u, &opts));
                let fast_s = t.elapsed().as_secs_f64();
                format!(
                    "| {} | {} | {} | {}{} | {:.0} / {:.2} | {} | {:.2} | {} | {} | {} | {} |",
                    hier.levels.len(),
                    coarsest.grid.nodes(),
                    coarsest.grid.n.map(|n| n.to_string()).join(" x "),
                    size,
                    match dropped {
                        0 => format!(" (A x - b: {exactness:.0e})"),
                        k => format!(" ({k} dropped, A x - b: {exactness:.0e})"),
                    },
                    setup * 1e3,
                    factor,
                    product.iterations,
                    product_s,
                    tight.iterations,
                    oracle.map_or("-".into(), |(f, its)| format!("{its:.0} ({f:.4})")),
                    fast.as_ref().map_or("-".into(), |f| format!("{:.1}, {} ({}, {:.0} its a solve) {:.4}", f.cost, f.steps, f.solves, f.its as f64 / f.solves.max(1) as f64, f.factor)),
                    fast.as_ref().map_or("-".into(), |_| format!("{fast_s:.1}")),
                )
            })
        },
        |c| c.name.clone(),
    );
    println!("## F12: the coarsest level's exact solve, its inverse factor in f64 and in f32");
    println!();
    println!("The coarsest level is solved as `x = R'(R b)`, `R = L^-1`; f32: `R` rounded to f32 and both products in f32 arithmetic (the GPU engine's). Iterations are V-cycle CG iterations from rest (CG in f64). Oracle: `buckle::lowest`, its tight (1e-8) solves of the bending block, iterations a solve (and its factor). LOBPCG: cold, preconditioned by solves to 1e-2; its cost in static solves (to 1e-6, this variant's), steps, preconditioner solves and their mean iterations, and its factor. Times run beside the other cases on a shared box (indicative).");
    println!();
    println!("| case | coarsest level | levels | coarsest nodes | coarsest cells | factored dofs | setup ms (the factor in it) / factor and invert s | static its 1e-6 | static s | static its 1e-10 | oracle its a solve (factor) | LOBPCG cost, steps (solves, its a solve) factor | LOBPCG s |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (c, row) in cases.iter().zip(&rows) {
        for ((label, _), r) in COARSEST.iter().zip(row) {
            println!("| {} | {label} {r}", c.name);
        }
    }
    println!();
    println!("{} cases in {:.0} s", cases.len(), t.elapsed().as_secs_f64());
}

/// E1, no solve: the section the Gauss points carry. Per bench section and cell count, over the
/// middle half of an L/s 10 column, the area and second moments of the points' weights (each
/// point where the cell's 2x2x2 rule puts it, weighted by its octant's filled share as the
/// stiffness takes it), about their own centroid, against the exact section's. A turned column
/// (20 degrees about z, then y: its axis oblique to every grid axis) is read in its own frame,
/// its points weighted by a window along its axis (flat over its middle, cosine ramps of a fifth
/// of its length from a tenth in at each end), since a cut square to an oblique axis is no
/// whole number of cell layers. The area is read twice: as the stiffness weighs the points (an
/// empty octant of an active cell at its fictitious share) and by the filled shares alone. The
/// active cells and cut octants are a layer's on an aligned column, the window's (cells whose
/// centre it spans) on a turned one. The holed bar (the square one with a 2 mm hole along its
/// axis, aligned and shifted only) is the concave check (FORCE A5 F28).
fn section_rig(filter: &str) {
    use std::f64::consts::PI;

    let m = bench::isotropic();
    println!("## E1: the quadrature's section against the exact one ({SECTION} mm sections, the middle half of an L/s 10 column)");
    println!();
    println!("| section | alignment | cells through | area | area by fill | I_y | I_z | I error x n^2 | centroid y, z (mm) | active cells | cut octants |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for kind in ["round", "square", "holed"] {
        let round = kind == "round";
        for align in Align::ALL {
            if kind == "holed" && align.turned() {
                continue;
            }
            for cells in [2.0, 3.0, 4.0, 6.0, 8.0] {
                let c = Column { round, ratio: 10.0, cells, align };
                if !format!("{kind} L/s {} n {} {}", c.ratio, c.cells, align.name()).contains(filter) {
                    continue;
                }
                let member = c.member();
                let h = c.h();
                let shift = if align.shifted() { study::shift(h) } else { [0.0; 3] };
                let (grid, _, _) = if kind == "holed" {
                    let lo = [0.0, -0.5 * SECTION, -0.5 * SECTION];
                    let hi = [member.l, 0.5 * SECTION, 0.5 * SECTION];
                    let field = |q: V3| bench::boxed(q, lo, hi).max(HOLE - (q[1] * q[1] + q[2] * q[2]).sqrt());
                    bench::cantilever_of(&field, lo, hi, [0.0; 3], [member.l, 0.0, 0.0], Kind::Force([-1.0, 0.0, 0.0]), member.frame, h, shift, &m)
                } else {
                    member.posed([-1.0, 0.0, 0.0], h, shift, &m)
                };
                let (lo, hi) = (0.25 * member.l, 0.75 * member.l);
                let mut layers = std::collections::BTreeSet::new();
                // weight, its first moments in y and z, its second moments
                let mut sum = [0.0f64; 5];
                // the filled volume, the active cells and the cut octants
                let (mut filled, mut active, mut cut) = (0.0f64, 0usize, 0usize);
                let vol = grid.h[0] * grid.h[1] * grid.h[2] / 8.0;
                // a turned column's window along its own axis, and the length it weighs
                let (w0, w1, ramp) = (0.1 * member.l, 0.9 * member.l, 0.2 * member.l);
                let window = |x: f64| {
                    if x <= w0 || x >= w1 {
                        0.0
                    } else if x < w0 + ramp {
                        0.5 * (1.0 - (std::f64::consts::PI * (x - w0) / ramp).cos())
                    } else if x > w1 - ramp {
                        0.5 * (1.0 - (std::f64::consts::PI * (w1 - x) / ramp).cos())
                    } else {
                        1.0
                    }
                };
                for cell in 0..grid.cells() {
                    let centre = grid.cell_center(cell);
                    if !grid.active(cell) {
                        continue;
                    }
                    if !align.turned() {
                        if centre[0] < lo || centre[0] > hi {
                            continue;
                        }
                        layers.insert(grid.cell_ijk(cell)[0]);
                    }
                    let own = bench::to_own(&member.frame, centre)[0];
                    if !align.turned() || (own > w0 && own < w1) {
                        active += 1;
                        cut += grid.gp[cell].iter().filter(|g| g.fill > 0 && g.fill < 255).count();
                    }
                    for (q, gp) in grid.gp[cell].iter().enumerate() {
                        // where the cell's rule puts the point, moved by the cell's offset
                        let xi = grid.point_xi(cell, q);
                        let at: V3 = std::array::from_fn(|a| centre[a] + xi[a] * 0.5 * grid.h[a]);
                        let (p, win) = match align.turned() {
                            true => {
                                let p = bench::to_own(&member.frame, at);
                                (p, window(p[0]))
                            }
                            false => (at, 1.0),
                        };
                        let w = vol * gp.weight() * win;
                        filled += vol * gp.fill as f64 / 255.0 * win;
                        for (s, v) in sum.iter_mut().zip([w, w * p[1], w * p[2], w * p[1] * p[1], w * p[2] * p[2]]) {
                            *s += v;
                        }
                    }
                }
                let length = if align.turned() { w1 - w0 - ramp } else { layers.len() as f64 * grid.h[0] };
                let area = sum[0] / length;
                let by_fill = filled / length;
                // a layer's on an aligned column
                let per = if align.turned() { 1 } else { layers.len().max(1) };
                let (y, z) = (sum[1] / sum[0], sum[2] / sum[0]);
                // I_y bends about y (offsets in z), I_z about z (offsets in y)
                let i_y = (sum[4] - sum[2] * z) / length;
                let i_z = (sum[3] - sum[1] * y) / length;
                let (a_exact, i_exact) = match kind {
                    "round" => (PI * SECTION * SECTION / 4.0, PI * SECTION.powi(4) / 64.0),
                    "square" => (SECTION * SECTION, SECTION.powi(4) / 12.0),
                    "holed" => (SECTION * SECTION - PI * HOLE * HOLE, SECTION.powi(4) / 12.0 - PI * HOLE.powi(4) / 4.0),
                    _ => unreachable!(),
                };
                let err = |v: f64, e: f64| v / e - 1.0;
                let i_mean = 0.5 * (err(i_y, i_exact) + err(i_z, i_exact));
                println!(
                    "| {} | {} | {} | {:+.2} % | {:+.2} % | {:+.2} % | {:+.2} % | {:+.2} | {:+.3}, {:+.3} | {} | {} |",
                    kind,
                    align.name(),
                    cells,
                    100.0 * err(area, a_exact),
                    100.0 * err(by_fill, a_exact),
                    100.0 * err(i_y, i_exact),
                    100.0 * err(i_z, i_exact),
                    i_mean * cells * cells,
                    y,
                    z,
                    active / per,
                    cut / per,
                );
            }
        }
    }
}

//! Analytic checks of the solver (beam theory, Kirsch's hole, a bar in tension), of the
//! surface picking and of the printed-part material model. Each builds its part from a
//! distance field the way the Force render mode does and picks its fixtures and loads on the surface.

use crate::bc::{Condition, Kind};
use crate::bench::{boxed, isotropic, part, pick, roller, E};
use crate::grid::Grid;
use crate::material::{Infill, Materials, Print, FILAMENTS, PLA};
use crate::solve::Options;
use crate::surface::Pick;
use crate::V3;

/// A 20 x 4 x 4 mm bar on rollers (each end plane held along its own axis only), pulled with
/// 100 N on its far end face (picked on the surface): every cell carries exactly F/A = 6.25
/// MPa and it stretches Fx/EA. The trilinear element holds a constant strain exactly: 0.1 %
/// on both, measured where the stress is even (three widths from the loaded end, whose load
/// is spread over the mesh's quads rather than as an exactly even traction).
#[test]
fn uniaxial_bar_carries_force_over_area_and_stretches_fl_over_ea() {
    let m = isotropic();
    let (lo, hi) = ([0.0; 3], [20.0, 4.0, 4.0]);
    let (grid, _, surface) = part(lo, hi, 1.0, &m, |p| boxed(p, lo, hi));
    let end = pick(&surface, [20.0, 2.0, 2.0], Pick::Face);
    assert!((end.area - 16.0).abs() < 1e-3, "the picked end face is 4 mm square: {} mm^2", end.area);
    let (_, force) = crate::bc::nodes(&grid, &surface, &[Condition { kind: Kind::Force([100.0, 0.0, 0.0]), region: end }]);
    let total: f64 = force.iter().map(|f| f[0]).sum();
    assert!((total - 100.0).abs() < 1e-9, "the load spreads to exactly 100 N: {total}");
    let mut held = vec![0u8; grid.nodes()];
    for a in 0..3 {
        roller(&grid, &mut held, a, 0.0);
    }
    let out = crate::analyse_nodes(grid, held, force, &m, &Options::default()).expect("a held part");
    let sigma = 100.0 / 16.0;
    let mut cells = 0;
    for c in 0..out.grid.cells() {
        if out.results.cells[c][3] < 0.999 {
            continue;
        }
        cells += 1;
        // the end's load spreads over the picked quads, not as an exactly even traction:
        // the stress is even three widths (of its quarter-symmetric 8 mm) away (Saint-Venant)
        if out.grid.cell_center(c)[0] > 8.0 {
            continue;
        }
        let s = out.results.stress[c];
        assert!((s[0] as f64 - sigma).abs() < 1e-3 * sigma, "cell {c}: sigma_xx {} against F/A {sigma}", s[0]);
        assert!(s[1..].iter().all(|v| (v.abs() as f64) < 1e-3 * sigma), "cell {c}: only sigma_xx: {s:?}");
    }
    assert_eq!(cells, 20 * 4 * 4, "every cell of the bar is full");
    // as far along as the stress is even: it has stretched F x / E A there
    let stretch = crate::results::displacement(&out.grid, &out.solution.u, [8.0, 2.0, 2.0])[0];
    let expect = 100.0 * 8.0 / (E * 16.0);
    assert!((stretch - expect).abs() < 1e-3 * expect, "8 mm along it moves {stretch} mm against Fx/EA {expect}");
}

/// On a lattice whose walls are a cell or two across, where a wall's few nodes hold `D^-1 A`'s
/// largest eigenvalues: a smoothing sweep grows no mode on any level the V-cycle smooths (growth
/// would compound through the cycle's sweeps), and the coarsest level is solved exactly, `A x`
/// within 1e-12 of `b` (machine precision times its factor's growth).
#[test]
fn thin_walls_smooth_without_growth_and_their_coarsest_level_solves_exactly() {
    let m = isotropic();
    let (grid, held, force) = crate::bench::lattice(1.0, &m);
    let posed = crate::pose(grid, held, force, Vec::new()).unwrap();
    let opts = Options::default();
    let h = crate::solve::Hierarchy::new(posed.grid.clone(), &posed.held, &m, &opts);
    let random = |n: usize| {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        (0..n).map(|_| std::array::from_fn(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as f64 / u64::MAX as f64 - 0.5
        })).collect::<Vec<V3>>()
    };
    let norm = |x: &[V3]| x.iter().flatten().map(|v| v * v).sum::<f64>().sqrt();
    let (last, smoothed) = h.levels.split_last().unwrap();
    assert!(!smoothed.is_empty(), "the lattice coarsens");
    for (i, level) in smoothed.iter().enumerate() {
        let n = level.grid.nodes();
        let mut x = random(n);
        level.mask(&mut x);
        let (zero, mut ax) = (vec![[0.0; 3]; n], vec![[0.0; 3]; n]);
        let mut rate = 0.0;
        for _ in 0..30 {
            let size = norm(&x);
            x.iter_mut().flatten().for_each(|v| *v /= size);
            level.jacobi(&mut x, &zero, &mut ax, opts.omega);
            rate = norm(&x);
        }
        assert!(rate <= 1.0, "level {i}: a sweep multiplies a mode by {rate}");
    }
    let n = last.grid.nodes();
    let mut v = random(n);
    last.mask(&mut v);
    let (mut b, mut x, mut ax) = (vec![[0.0; 3]; n], vec![[0.0; 3]; n], vec![[0.0; 3]; n]);
    last.apply(&v, &mut b);
    h.coarsest.solve(&b, &mut x);
    last.apply(&x, &mut ax);
    let miss: Vec<V3> = ax.iter().zip(&b).map(|(p, q)| std::array::from_fn(|a| p[a] - q[a])).collect();
    let exact = norm(&miss) / norm(&b);
    assert!(exact <= 1e-12, "the coarsest level ({} of {} degrees of freedom factored, {} dropped): A x - b is {exact:.1e} of b", h.coarsest.size(), 3 * n, h.coarsest.dropped);
    let s = crate::solve::solve(&h, &posed.force, &opts);
    assert!(s.residual <= opts.tolerance, "residual {} after {} iterations", s.residual, s.iterations);
}

/// A 24 mm cantilever of 4 x 4 mm section clamped at one end (the clamped face picked on the
/// surface), 1 N down on the other end face. Beam theory (Timoshenko, shear included):
/// tip deflection PL^3/3EI + PL/kGA = 0.2207 mm; bending stress P(L - x)c/I. At 0.5 mm cells
/// (8 through the depth) the trilinear element is a little stiff in bending (measured -2.2 %):
/// within 3 % on the deflection; the stress a quarter span from the root (clear of the clamp's
/// local disturbance), at the outer cells' centres (measured -0.9 %), within 2 %. The
/// Galerkin multigrid solves it in under 50 iterations (measured 44; re-discretised coarse
/// levels take half as many again, and more with finer cells). Built as two boxes that share a
/// face or overlap a little, it reads as the one box (FORCE A15).
#[test]
fn cantilever_tip_deflection_and_bending_stress_follow_beam_theory() {
    let m = isotropic();
    let h = 0.5;
    let (grid, surface, conditions) = crate::bench::cantilever(h, &m);
    let out = crate::analyse(grid, &surface, &m, &conditions, &Options::default()).expect("a held part");
    let (l, s) = crate::bench::BEAM;
    let expect = crate::bench::cantilever_tip();
    let tip = -crate::results::displacement(&out.grid, &out.solution.u, [l, 0.5 * s, 0.5 * s])[2];
    let its = out.solution.iterations;
    assert!((tip - expect).abs() < 0.03 * expect, "tip deflection {tip} mm against beam theory {expect} ({its} iterations)");
    assert!(its < 50, "{its} iterations: the multigrid has lost its coarse levels' stiffness");
    // the top cells (in tension) a quarter span from the root
    let i = s * s.powi(3) / 12.0;
    let x = 0.25 * l + 0.5 * h;
    let z = s - 0.5 * h;
    let (c, _) = out.grid.locate([x, 0.5 * s - 0.5 * h, z]);
    let sigma = out.results.stress[c][0] as f64;
    let beam = 1.0 * (l - x) * (z - 0.5 * s) / i;
    assert!((sigma - beam).abs() < 0.02 * beam, "bending stress {sigma} MPa against beam theory {beam}");
    // the same beam as two boxes sharing the face x = 6.3 (a radius-0 union reads zero there,
    // inside the material) and as two overlapping by 1 mm (it reads half the overlap deep):
    // regularised (FORCE A15), each reads as the one box, its samples within the walls' band to
    // 0.02 of a spacing and its tip as printed to 0.5 %; the one box keeps its field's samples
    // bit for bit
    let printed = Materials::new(PLA, &Print::default(), true);
    let (lo, hi) = ([0.0; 3], [l, s, s]);
    let as_printed = |field: &dyn Fn(V3) -> f64| {
        let (grid, lattice, surface) = part(lo, hi, h, &printed, field);
        let root = pick(&surface, [0.0, 0.5 * s, 0.5 * s], Pick::Face);
        let tip = pick(&surface, [l, 0.5 * s, 0.5 * s], Pick::Face);
        let conditions = vec![Condition { kind: Kind::Fixed, region: root }, Condition { kind: Kind::Force([0.0, 0.0, -1.0]), region: tip }];
        let out = crate::analyse(grid, &surface, &printed, &conditions, &Options::default()).expect("a held part");
        (lattice, -crate::results::displacement(&out.grid, &out.solution.u, [l, 0.5 * s, 0.5 * s])[2])
    };
    let one = |p: V3| boxed(p, lo, hi);
    let (twin, twin_tip) = as_printed(&one);
    let (_, _, mut sampled) = Grid::layout(lo, hi, h);
    sampled.sample(&mut |ps, out| ps.iter().zip(out).for_each(|(p, o)| *o = one(*p)));
    assert!(twin.d.iter().zip(&sampled.d).all(|(a, b)| a.to_bits() == b.to_bits()), "the one box's samples are its field's");
    let deep = crate::grid::deepest(&printed, twin.spacing);
    let joined = |left: f64, right: f64| move |p: V3| boxed(p, lo, [left, s, s]).min(boxed(p, [right, 0.0, 0.0], hi));
    for (name, field) in [("sharing a face", joined(6.3, 6.3)), ("overlapping 1 mm", joined(6.8, 5.8))] {
        let (lattice, tip) = as_printed(&field);
        let off = twin.d.iter().zip(&lattice.d).filter(|(t, _)| -deep < f64::from(**t) && **t < 0.0)
            .map(|(t, d)| (f64::from(*d) - f64::from(*t)).abs()).fold(0.0, f64::max) / twin.spacing;
        assert!(off <= 0.02, "two boxes {name}: a sample within {deep:.3} mm of the surface reads {off:.3} spacings off the one box's");
        assert!((tip - twin_tip).abs() <= 0.005 * twin_tip, "two boxes {name}: tip {tip} mm as printed against the one box's {twin_tip}");
    }
}

/// A fixed face holds one layer of nodes, the one nearest it, wherever it crosses its cells:
/// a 6 x 4 x 4 mm block in a grid with room around it, its x = x0 face at a tenth to nine
/// tenths of a cell past a node layer, picked and fixed. (Holding every node out to half a
/// cell inward held two layers wherever the face lay in a cell's inner half.)
#[test]
fn a_fixed_face_holds_the_one_layer_of_nodes_nearest_it_wherever_it_crosses_the_cells() {
    let m = isotropic();
    let h = 0.5;
    for k in [1, 2, 3, 4, 6, 7, 8, 9] {
        let x0 = 0.1 * k as f64 * h;
        let (lo, hi) = ([x0, 0.0, 0.0], [6.0, 4.0, 4.0]);
        let (grid, _, surface) = part([-2.0, -1.0, -1.0], [8.0, 5.0, 5.0], h, &m, |p| boxed(p, lo, hi));
        let face = pick(&surface, [x0, 2.0, 2.0], Pick::Face);
        let (held, _) = crate::bc::nodes(&grid, &surface, &[Condition { kind: Kind::Fixed, region: face }]);
        let xs: Vec<f64> = (0..grid.nodes()).filter(|&n| held[n] != 0).map(|n| grid.node_pos(n)[0]).collect();
        let layer = xs[0];
        assert!(xs.iter().all(|&x| (x - layer).abs() < 1e-9), "face at {x0}: the held nodes lie in more than one layer: x {:?}", {
            let mut d = xs.clone();
            d.sort_by(f64::total_cmp);
            d.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
            d
        });
        assert!((layer - x0).abs() <= 0.5 * h, "face at {x0}: the layer held, x {layer}, is not the one nearest it");
        assert!(xs.len() >= 81, "face at {x0}: {} nodes held, the face has 9 x 9", xs.len());
    }
}

/// A 10 x 4 x 4 mm block fixed at x = 0, with a second shape beside it: the grid, its
/// surface and the block's fixture and 1 N tip load (down, at x = 10).
fn block_and(other: Option<(V3, V3)>) -> (Grid, crate::surface::Surface, Vec<Condition>) {
    let m = isotropic();
    let (lo, hi) = ([0.0; 3], [10.0, 4.0, 4.0]);
    let (grid, _, surface) = part([-1.0, -1.0, -1.0], [17.0, 7.0, 5.0], 0.5, &m, |p| match other {
        Some((a, b)) => boxed(p, lo, hi).min(boxed(p, a, b)),
        None => boxed(p, lo, hi),
    });
    let root = pick(&surface, [0.0, 2.0, 2.0], Pick::Face);
    let tip = pick(&surface, [10.0, 2.0, 2.0], Pick::Face);
    (grid, surface, vec![Condition { kind: Kind::Fixed, region: root }, Condition { kind: Kind::Force([0.0, 0.0, -1.0]), region: tip }])
}

/// A cube apart from the fixed block is a loose piece: left out of the solve, reported with
/// its material, and the block's answer is the block's alone.
#[test]
fn a_piece_nothing_holds_is_left_out_of_the_solve_and_reported() {
    let m = isotropic();
    let opts = Options::default();
    let (grid, surface, conditions) = block_and(None);
    let alone = crate::analyse(grid, &surface, &m, &conditions, &opts).expect("a held part");
    assert_eq!(alone.loose, crate::pieces::Loose::default(), "one piece, held");
    let (grid, surface, conditions) = block_and(Some(([13.0, 0.0, 0.0], [15.0, 2.0, 2.0])));
    let out = crate::analyse(grid, &surface, &m, &conditions, &opts).expect("the block holds");
    assert_eq!(out.loose.pieces, 1, "{:?}", out.loose);
    assert!((out.loose.volume - 8.0).abs() < 0.8, "the cube's 8 mm^3: {:?}", out.loose);
    assert!(out.loose.describe().starts_with("1 loose piece, "), "{}", out.loose.describe());
    let tip = |o: &crate::Outcome| crate::results::displacement(&o.grid, &o.solution.u, [10.0, 2.0, 2.0])[2];
    assert_eq!(out.solution.iterations, alone.solution.iterations, "the same solve as the block alone");
    assert!((tip(&out) - tip(&alone)).abs() <= 1e-9 * tip(&alone).abs(), "tip {} against the block alone {}", tip(&out), tip(&alone));
}

/// A load on a piece nothing holds fails the study: no load path carries it.
#[test]
fn a_load_on_a_piece_nothing_holds_fails_the_study() {
    let m = isotropic();
    let (grid, surface, mut conditions) = block_and(Some(([13.0, 0.0, 0.0], [15.0, 2.0, 2.0])));
    conditions[1].region = pick(&surface, [15.0, 1.0, 1.0], Pick::Face);
    let err = crate::analyse(grid, &surface, &m, &conditions, &Options::default()).err().expect("an error, not an answer");
    assert!(err.contains("100 % of the load") && err.contains("1 loose piece") && err.contains("nothing holds"), "{err}");
}

/// A loose piece under one cell's plastic is how a surface fell on the cells: the load on it
/// goes to the rest of the load, the total stays, and the study is posed.
#[test]
fn a_load_on_a_sliver_under_one_cell_goes_to_the_rest_of_the_load() {
    let (mut grid, surface, conditions) = block_and(None);
    let (held, mut force) = crate::bc::nodes(&grid, &surface, &conditions);
    let total: f64 = force.iter().map(|f| f[2]).sum();
    let c = grid.cell(grid.n[0] - 2, grid.n[1] - 2, grid.n[2] - 2);
    grid.gp[c][0].fill = 8;
    let n = grid.cell_nodes(c)[0];
    force[n] = [0.0, 0.0, 0.01 * total];
    let near = vec![false; grid.cells()];
    let posed = crate::pose(grid, held, force, near).expect("a sliver does not fail the study");
    assert_eq!(posed.loose.pieces, 1, "{:?}", posed.loose);
    assert_eq!(posed.force[n], [0.0; 3], "the sliver carries nothing");
    let left: f64 = posed.force.iter().map(|f| f[2]).sum();
    assert!((left - 1.01 * total).abs() < 1e-9 * total.abs(), "the total stays: {left} of {}", 1.01 * total);
}

/// A sliver meeting the block only along a cell edge hinges there: it is a loose piece too,
/// so the solve is the block's alone, not one stalled on a free hinge.
#[test]
fn a_sliver_meeting_the_part_at_an_edge_is_left_out_not_solved() {
    let m = isotropic();
    let opts = Options::default();
    let (grid, surface, conditions) = block_and(None);
    let alone = crate::analyse(grid, &surface, &m, &conditions, &opts).expect("a held part");
    let (grid, surface, conditions) = block_and(Some(([10.0, 4.0, 0.0], [12.0, 6.0, 4.0])));
    assert_eq!(crate::pieces::label(&grid).1, 2, "the sliver meets the block at an edge only");
    let out = crate::analyse(grid, &surface, &m, &conditions, &opts).expect("the block holds");
    assert_eq!(out.loose.pieces, 1, "{:?}", out.loose);
    assert_eq!(out.solution.iterations, alone.solution.iterations, "iterations with the sliver against the block alone");
}

/// Kirsch's plate ([`crate::bench::kirsch`]) pulled at 10 MPa: Kt from the cell stresses
/// beside the hole (the column x = h/2, the first two cells clear of it) extrapolated to its
/// edge, and the iterations the solve took.
fn kirsch(a: f64, l: f64, w: f64, t: f64, h: f64) -> (f64, usize) {
    let m = isotropic();
    let pull = 10.0;
    let (grid, held, force) = crate::bench::kirsch(a, l, w, t, h, pull, &m);
    let out = crate::analyse_nodes(grid, held, force, &m, &Options::default()).expect("a held part");
    let column: Vec<(f64, f64)> = (0..out.grid.n[1]).filter_map(|j| {
        let y = out.grid.origin[1] + (j as f64 + 0.5) * h;
        let (c, _) = out.grid.locate([0.5 * h, y, 0.25 * t]);
        (y - 0.5 * h >= a - 1e-9).then(|| (y, out.results.stress[c][0] as f64))
    }).collect();
    let ((y1, s1), (y2, s2)) = (column[0], column[1]);
    let edge = s1 + (s1 - s2) * (y1 - a) / (y2 - y1);
    (edge / pull, out.solution.iterations)
}

/// A 40 x 20 mm strip 0.5 mm thick (a quarter of it) with a 5 mm hole: d/W = 0.25,
/// Kt = (2 + (1 - d/W)^3) / (1 - d/W) = 3.229 on the gross section (Heywood's fit to
/// Howland's series). From 5 to 10 cells across the radius Kt moves toward it from one side,
/// and the two extrapolate to it. The read is second order: the linear fit to the edge leaves
/// the stress's curvature, and the elements and the cut cells leave theirs too. So
/// the limit is the fine value plus a third of the change (Richardson at order 2), and while
/// the next order at the coarser size is under the leading one, the limit misses by at most
/// two thirds of the leading term at the fine size: 2/9 of the change. A fall faster than
/// second order, a crossing at 10 cells (-8.9 % then -0.3 %, then +1.8 % at 20), extrapolates
/// past it. Measured -9.5 % at 5 cells and -2.65 % at 10, extrapolated -0.35 % (within
/// 1.5 %). The read itself on Kirsch's exact field, with no solver error, extrapolates to
/// -1.2 %, so no bound much tighter holds for an exact solver.
#[test]
fn plate_with_a_hole_kt_converges_to_heywood_from_5_to_10_cells_across_the_radius() {
    let reference = (2.0 + 0.75f64.powi(3)) / 0.75;
    let (coarse, _) = kirsch(2.5, 20.0, 10.0, 0.5, 0.5);
    let (fine, its) = kirsch(2.5, 20.0, 10.0, 0.5, 0.25);
    let toward = (coarse < fine && fine < reference) || (coarse > fine && fine > reference);
    assert!(toward, "Kt {coarse} at 5 cells across the radius, {fine} at 10 ({its} iterations): not moving toward {reference} from one side");
    let limit = fine + (fine - coarse) / 3.0;
    let bound = 2.0 / 9.0 * (fine - coarse).abs();
    assert!((limit - reference).abs() <= bound, "Kt {coarse} at 5 cells across the radius, {fine} at 10: extrapolated {limit}, not within {bound} of {reference}");
}

/// A click picks the same region at every cell size, so a study's steps carry the same loads:
/// on a block with a hole through it, at 1, 0.5 and 0.25 mm cells (the hole 1.5, 3 and 6
/// cells across its radius), a click on the top picks the whole top less the hole (the surface
/// keeps the block's edges sharp), a click in the hole its whole wall with its axis and
/// radius; a force there spreads to exactly its total, a pressure to the pressure times the
/// face's true area, a torque in the hole to exactly its moment about the axis.
#[test]
fn a_click_picks_the_same_face_or_hole_at_every_cell_size() {
    use crate::study::{Load, Model, Spot};
    let m = isotropic();
    let (lo, hi) = ([0.0; 3], [12.0, 8.0, 4.0]);
    let (r, cx, cy) = (1.5, 6.0, 4.0);
    let field = |p: V3| boxed(p, lo, hi).max(r - ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt());
    let mut distance = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = field(*p);
        }
    };
    let (f, p, t) = ([3.0, -1.0, -10.0], 0.2, 50.0);
    let loads = [
        Load::new(Kind::Force(f), Spot::Seed { pick: Pick::Face, at: [2.0, 2.0, 4.0] }),
        Load::new(Kind::Pressure(p), Spot::Seed { pick: Pick::Face, at: [10.0, 6.0, 4.0] }),
        Load::new(Kind::Torque(t), Spot::Seed { pick: Pick::Hole, at: [cx + r, cy, 2.0] }),
    ];
    let face = 12.0 * 8.0 - std::f64::consts::PI * r * r;
    let wall = 2.0 * std::f64::consts::PI * r * 4.0;
    for h in [1.0, 0.5, 0.25] {
        let model = Model::new(lo, hi, h, &mut distance, &m);
        let c = model.conditions(&loads);
        let (top, hole) = (&c[0].region, &c[2].region);
        assert_eq!(top.faces.len(), c[1].region.faces.len(), "two clicks on the top pick the same face at {h} mm");
        // the hole's rim is a polygon of the lattice's crossings: its share less on a coarse model
        assert!((top.area - face).abs() < 0.01 * face, "top face {} mm^2 against {face} at {h} mm", top.area);
        assert!(top.normal[2] > 0.999, "the top's normal is up at {h} mm: {:?}", top.normal);
        assert!((hole.area - wall).abs() < 0.03 * wall, "hole wall {} mm^2 against {wall} at {h} mm", hole.area);
        let (centre, axis, radius) = hole.cylinder.expect("a hole has an axis");
        assert!(axis[2].abs() > 0.999, "the hole's axis is Z at {h} mm: {axis:?}");
        // the hole named as a feature (its own cylinder: within a cell of it, facing along
        // it) keeps the same wall, its axis fitted
        let named = Load::new(Kind::Torque(t), Spot::Within(crate::study::Within { key: 1, keep: std::sync::Arc::new(move |c: V3, n: V3, h: f64| {
            let (dx, dy) = (c[0] - cx, c[1] - cy);
            let d = (dx * dx + dy * dy).sqrt();
            (d - r).abs() <= 0.75 * h && ((dx * n[0] + dy * n[1]) / d.max(1e-9)).abs() >= 0.7
        }) }));
        let by_name = &model.conditions(std::slice::from_ref(&named))[0].region;
        assert!((by_name.area - hole.area).abs() < 0.02 * hole.area, "the named hole's wall {} mm^2 against the click's {} at {h} mm", by_name.area, hole.area);
        assert!(by_name.cylinder.is_some_and(|(_, a, q)| a[2].abs() > 0.999 && (q - r).abs() < 0.03 * r), "the named hole has its axis at {h} mm: {:?}", by_name.cylinder);
        assert!((radius - r).abs() < 0.03 * r && (centre[0] - cx).abs() < 0.02 && (centre[1] - cy).abs() < 0.02, "the hole at {centre:?} of radius {radius} against {r} at {h} mm");
        // what each load sums to
        let total = |i: usize| {
            let (_, force) = crate::bc::nodes(&model.grid, &model.surface, &c[i..=i]);
            let sum: V3 = std::array::from_fn(|a| force.iter().map(|f| f[a]).sum());
            // about the hole's axis: (q - c) x f . axis
            let moment: f64 = (0..model.grid.nodes()).map(|n| {
                let q = model.grid.node_pos(n);
                let d: V3 = std::array::from_fn(|a| q[a] - centre[a]);
                let fq = force[n];
                let m = [d[1] * fq[2] - d[2] * fq[1], d[2] * fq[0] - d[0] * fq[2], d[0] * fq[1] - d[1] * fq[0]];
                m[0] * axis[0] + m[1] * axis[1] + m[2] * axis[2]
            }).sum();
            (sum, moment)
        };
        let (sum, _) = total(0);
        assert!((0..3).all(|a| (sum[a] - f[a]).abs() < 1e-9), "the force sums to {sum:?} at {h} mm");
        let (sum, _) = total(1);
        let pf = p * face;
        assert!(sum[0].abs() < 1e-3 * pf && sum[1].abs() < 1e-3 * pf && (sum[2] + pf).abs() < 0.01 * pf, "the pressure sums to {sum:?} against {pf} N down at {h} mm");
        let (_, moment) = total(2);
        assert!((moment - t).abs() < 1e-6 * t, "the torque is {moment} N mm about the hole against {t} at {h} mm");
    }
}

/// The printed-part model's shape: the bead is weakest between layers, infill at full density
/// is the solid skin, every material is positive definite, an isotropic material does not
/// care how it is turned, and Hill's index of an isotropic strength is von Mises'.
#[test]
fn printed_material_is_anisotropic_as_printed_and_meets_the_solid_at_full_density() {
    let positive = |d: &crate::element::Mat6| {
        // Cholesky succeeds
        let mut l = [[0.0f64; 6]; 6];
        for i in 0..6 {
            for j in 0..=i {
                let s: f64 = (0..j).map(|k| l[i][k] * l[j][k]).sum();
                if i == j {
                    let v = d[i][i] - s;
                    if v <= 0.0 {
                        return false;
                    }
                    l[i][i] = v.sqrt();
                } else {
                    l[i][j] = (d[i][j] - s) / l[j][j];
                }
            }
        }
        true
    };
    let families = [Infill::Alternating, Infill::Aligned, Infill::Grid, Infill::Triangles, Infill::Honeycomb, Infill::Cubic, Infill::Gyroid, Infill::Curves, Infill::Lightning];
    for f in FILAMENTS {
        let m = Materials::new(f, &Print::default(), true);
        let s = crate::material::invert6(&m.bead).unwrap();
        let (e1, e2, e3) = (1.0 / s[0][0], 1.0 / s[1][1], 1.0 / s[2][2]);
        assert!(e1 >= e2 && e2 > e3, "{}: along beads {e1} >= across {e2} > between layers {e3}", f.name);
        assert!(m.bead_strength.x[2] < m.bead_strength.x[0], "{}: weaker between layers", f.name);
        for family in families {
            for density in [0.0, 0.15, 0.5, 1.0] {
                let p = Print { infill: family, density, ..Print::default() };
                let m = Materials::new(f, &p, true);
                assert!(positive(&m.fills[0].d), "{} {family:?} at {density}: positive definite", f.name);
                if density == 1.0 {
                    for r in 0..6 {
                        for c in 0..6 {
                            assert!((m.fills[0].d[r][c] - m.skin[r][c]).abs() < 1e-9 * m.skin[0][0], "{} {family:?}: full infill is the skin", f.name);
                        }
                    }
                }
            }
        }
        // taller layers bond less
        let tall = Materials::new(f, &Print { layer_height: 0.3, ..Print::default() }, true);
        assert!(tall.bead_strength.x[2] < m.bead_strength.x[2], "{}: 0.3 mm layers are weaker between layers than 0.2", f.name);
    }
    let iso = isotropic();
    let turned = crate::material::rotate_z(&iso.skin, 0.7);
    assert!((0..6).all(|r| (0..6).all(|c| (turned[r][c] - iso.skin[r][c]).abs() < 1e-9 * E)), "isotropic is the same turned");
    let stress = [30.0, -10.0, 5.0, 4.0, -7.0, 2.0];
    let x = iso.bead_strength.x[0];
    let vm = crate::element::von_mises(&stress);
    assert!((iso.bead_strength.hill(&stress) - (vm / x).powi(2)).abs() < 1e-12, "Hill's isotropic index is von Mises'");
}

/// A study steps from a coarse model to one twice as fine, its loads picked again from the
/// points clicked: on the finer model they pick the same faces (the load the same 1 N), and
/// its answer is a little softer (4 cells through the depth are stiffer in bending than 8).
#[test]
fn a_study_refines_with_its_loads_picked_again() {
    use crate::study::{Load, Model, Spot};
    let m = isotropic();
    let (lo, hi) = ([0.0; 3], [20.0, 4.0, 4.0]);
    let mut field = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = boxed(*p, lo, hi);
        }
    };
    let loads = [
        Load::new(Kind::Fixed, Spot::Seed { pick: Pick::Face, at: [0.0, 2.0, 2.0] }),
        Load::new(Kind::Force([0.0, 0.0, -1.0]), Spot::Seed { pick: Pick::Face, at: [20.0, 2.0, 2.0] }),
    ];
    assert!(Model::posed(&loads));
    assert!(!Model::posed(&loads[..1]), "a fixture alone is no study");
    let opts = Options::default();
    let coarse_model = Model::new(lo, hi, 1.0, &mut field, &m);
    let fine_model = Model::new(lo, hi, 0.5, &mut field, &m);
    for model in [&coarse_model, &fine_model] {
        let c = model.conditions(&loads);
        for c in &c {
            assert!((c.region.area - 16.0).abs() < 1e-3, "{:?} picks its whole end face at {} mm: {} mm^2", c.kind, model.h, c.region.area);
        }
        let (_, force) = crate::bc::nodes(&model.grid, &model.surface, &c);
        let total: f64 = force.iter().map(|f| f[2]).sum();
        assert!((total + 1.0).abs() < 1e-9, "the load is 1 N at {} mm: {total}", model.h);
    }
    let coarse = coarse_model.solve(&m, &coarse_model.conditions(&loads), &opts, &mut |_, _| false).expect("a held part");
    let fine = fine_model.solve(&m, &fine_model.conditions(&loads), &opts, &mut |_, _| false).expect("a held part");
    let tip = |o: &crate::Outcome| -crate::results::displacement(&o.grid, &o.solution.u, [20.0, 2.0, 2.0])[2];
    assert!(tip(&coarse) < tip(&fine) && tip(&coarse) > 0.8 * tip(&fine), "coarse tip {} against fine {}", tip(&coarse), tip(&fine));
}

/// The ray buckets find the same nearest quad as testing every quad, on a block with a hole
/// seen from many directions; a ray past the part finds nothing.
#[test]
fn rays_meet_the_nearest_quad_through_the_buckets() {
    use crate::display::Bins;
    let m = isotropic();
    let (lo, hi) = ([0.0; 3], [20.0, 12.0, 6.0]);
    let (_, _, s) = part(lo, hi, 1.0, &m, |p| boxed(p, lo, hi).max(2.0 - ((p[0] - 10.0).powi(2) + (p[1] - 6.0).powi(2)).sqrt()));
    let bins = Bins::new(&s.pos, &s.quads);
    let every = |o: V3, d: V3| {
        let mut best: Option<(f64, u32)> = None;
        for (qi, q) in s.quads.iter().enumerate() {
            let p = q.map(|v| s.pos[v as usize].map(|x| x as f64));
            for tri in [[p[0], p[1], p[2]], [p[0], p[2], p[3]]] {
                if let Some(t) = crate::surface::ray_triangle(o, d, tri) {
                    if best.is_none_or(|(b, _)| t < b) {
                        best = Some((t, qi as u32));
                    }
                }
            }
        }
        best.map(|(_, q)| q)
    };
    let mut hits = 0;
    for k in 0..200 {
        let a = k as f64 * 2.399;
        let z = 1.0 - 2.0 * (k as f64 + 0.5) / 200.0;
        let r = (1.0 - z * z).sqrt();
        let d: V3 = [r * a.cos(), r * a.sin(), z];
        let target = [10.0 + 6.0 * (k as f64 * 0.37).sin(), 6.0 + 4.0 * (k as f64 * 0.61).cos(), 3.0];
        let o: V3 = std::array::from_fn(|i| target[i] - 60.0 * d[i]);
        let got = bins.hit(&s.pos, &s.quads, o, d).map(|(q, _)| q);
        assert_eq!(got, every(o, d), "ray {k}");
        hits += got.is_some() as usize;
    }
    assert!(hits > 150, "{hits} of 200 rays at the part met it");
    assert_eq!(bins.hit(&s.pos, &s.quads, [-50.0, 6.0, 30.0], [1.0, 0.0, 0.0]), None);
}

/// The slicer's names map onto the model's: every sparse pattern the slicer offers has a
/// family, and the filament types pick their presets (an unfilled nylon has none).
#[test]
fn the_slicers_patterns_and_filament_types_map_onto_the_model() {
    use crate::material::{filament_of_type, Infill};
    for name in ["zig-zag", "alignedrectilinear", "line", "zigzag", "crosszag", "lockedzag", "grid", "crosshatch", "2dlattice", "triangles", "tri-hexagon", "cubic", "adaptivecubic", "supportcubic", "gyroid", "honeycomb", "3dhoneycomb", "lightning", "hilbertcurve", "archimedeanchords", "octagramspiral", "concentric"] {
        assert!(Infill::from_setting(name).is_some(), "{name}");
    }
    assert_eq!(Infill::from_setting("gyroid"), Some(Infill::Gyroid));
    assert_eq!(Infill::from_setting("nonsense"), None);
    let name = |t: &str| filament_of_type(t).map(|f| f.name);
    assert_eq!(name("PLA"), Some("PLA"));
    assert_eq!(name("PLA-CF"), Some("PLA"));
    assert_eq!(name("PETG"), Some("PETG"));
    assert_eq!(name("asa"), Some("ABS/ASA"));
    assert_eq!(name("PA6-CF"), Some("PA-CF"));
    assert_eq!(name("PAHT-CF"), Some("PA-CF"));
    assert_eq!(name("PA"), None);
    assert_eq!(name("TPU"), None);
}

/// A part given as triangles samples as its distance field does: a box's mesh with its corners
/// on lattice lines (rays run through its edges, corners and face diagonals) reads inside
/// exactly where the box's field is negative, at the field's distances (out to the reach it
/// measures outside), and gives the same cells; with a triangle missing, the other two axes'
/// rays outvote the one through the gap.
#[test]
fn a_mesh_samples_as_its_field_with_rays_through_its_edges_and_a_triangle_missing() {
    use crate::mesh::Mesh;
    let m = isotropic();
    let (lo, hi, h) = ([0.0; 3], [3.0, 3.0, 2.5], 0.5);
    let (_, layout) = Grid::from_samples(lo, hi, h, &mut |_| {}, &m);
    let (blo, bhi) = (layout.pos(2, 2, 2), layout.pos(10, 10, 8));
    let corners: Vec<V3> = (0..8).map(|c| std::array::from_fn(|a| if (c >> a) & 1 == 0 { blo[a] } else { bhi[a] })).collect();
    let faces = [[0, 2, 6, 4], [1, 5, 7, 3], [0, 4, 5, 1], [2, 3, 7, 6], [0, 1, 3, 2], [4, 6, 7, 5]];
    let tris: Vec<[u32; 3]> = faces.iter().flat_map(|f| [[f[0], f[1], f[2]], [f[0], f[2], f[3]]]).collect();
    let mut field = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = boxed(*p, blo, bhi);
        }
    };
    let (fgrid, flat) = Grid::from_field(lo, hi, h, &mut field, &m);
    let (mgrid, mlat) = Grid::from_samples(lo, hi, h, &mut |l| Mesh::new(&corners, &tris).sample(l), &m);
    let (_, gap) = Grid::from_samples(lo, hi, h, &mut |l| Mesh::new(&corners, &tris[1..]).sample(l), &m);
    let reach = 3.0 * mlat.spacing;
    let mut inside = 0;
    for s in 0..flat.d.len() {
        let (f, g) = (flat.d[s] as f64, mlat.d[s] as f64);
        if f.abs() > 1e-9 {
            assert_eq!(f < 0.0, g < 0.0, "sample {s}: field {f}, mesh {g}");
            assert_eq!(f < 0.0, gap.d[s] < 0.0, "sample {s} with a triangle missing: field {f}, mesh {}", gap.d[s]);
            inside += usize::from(f < 0.0);
        }
        assert!((f.min(reach) - g).abs() < 1e-5, "sample {s}: field {f}, mesh {g}");
    }
    assert!(inside > 100, "{inside} samples inside");
    for (c, (a, b)) in fgrid.gp.iter().zip(&mgrid.gp).enumerate() {
        for q in 0..8 {
            assert!((a[q].fill as i32 - b[q].fill as i32).abs() <= 1, "cell {c} octant {q}: field fill {}, mesh {}", a[q].fill, b[q].fill);
        }
    }
}

/// An edited load solves from the last answer ([`crate::solve::start`]): a load only scaled
/// is answered where it starts, a slightly turned one in fewer iterations than from rest, to
/// the same answer.
#[test]
fn an_edited_load_solves_from_the_last_answer() {
    use crate::solve::{solve, solve_until, Hierarchy};
    let m = isotropic();
    let (grid, held, force) = crate::bench::kirsch(2.5, 20.0, 10.0, 1.0, 1.0, 10.0, &m);
    let opts = Options::default();
    let h = Hierarchy::new(grid, &held, &m, &opts);
    let last = solve(&h, &force, &opts);
    let scaled: Vec<V3> = force.iter().map(|f| f.map(|v| 1.6 * v)).collect();
    let warm = solve_until(&h, &scaled, Some(&last.u), &opts, &mut |_, _| false);
    assert!(warm.iterations <= 1, "a scaled load from its last answer: {} iterations (from rest {})", warm.iterations, last.iterations);
    let turned: Vec<V3> = force.iter().map(|f| [f[0], 0.05 * f[0], 0.0]).collect();
    let cold = solve(&h, &turned, &opts);
    let warm = solve_until(&h, &turned, Some(&last.u), &opts, &mut |_, _| false);
    assert!(warm.iterations < cold.iterations, "a turned load: {} iterations from the last answer, {} from rest", warm.iterations, cold.iterations);
    let size = |v: &V3| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let top = cold.u.iter().map(size).fold(0.0, f64::max);
    let off = cold.u.iter().zip(&warm.u).map(|(a, b)| size(&[a[0] - b[0], a[1] - b[1], a[2] - b[2]])).fold(0.0, f64::max);
    assert!(off < 1e-4 * top, "the same answer: off by {off:.2e} of {top:.2e} mm");
}

/// A click says by itself whether it means a face or a hole's wall: on a block with a hole
/// through it and a round boss on top, at 1, 0.5 and 0.25 mm cells, a click on the hole's wall
/// reads the hole (its whole wall, with its axis), one on the top or a side a face, and one on
/// the boss's round side a face too (convex: its normals point away from its axis).
#[test]
fn a_click_reads_a_holes_wall_as_the_hole_and_anything_else_as_a_face() {
    use crate::study::Model;
    let m = isotropic();
    let (lo, hi) = ([0.0; 3], [16.0, 8.0, 4.0]);
    let (r, hx, bx, cy) = (1.5, 4.0, 12.0, 4.0);
    let boss = |p: V3| (((p[0] - bx).powi(2) + (p[1] - cy).powi(2)).sqrt() - 2.0).max(p[2] - 7.0).max(-p[2]);
    let field = |p: V3| boxed(p, lo, hi).max(r - ((p[0] - hx).powi(2) + (p[1] - cy).powi(2)).sqrt()).min(boss(p));
    let mut distance = |ps: &[V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = field(*p));
    for h in [1.0, 0.5, 0.25] {
        let model = Model::new(lo, [hi[0], hi[1], 7.0], h, &mut distance, &m);
        let s = &model.surface;
        let read = |at: V3| s.reading(s.nearest(at).expect("a surface"));
        let (pick, wall) = read([hx + r, cy, 2.0]);
        assert_eq!(pick, Pick::Hole, "the hole's wall at {h} mm");
        assert!(wall.cylinder.is_some_and(|(_, a, rr)| a[2].abs() > 0.99 && (rr - r).abs() < 0.15 * r), "its axis and radius at {h} mm: {:?}", wall.cylinder);
        assert_eq!(read([2.0, 2.0, 4.0]).0, Pick::Face, "the top at {h} mm");
        assert_eq!(read([0.0, 4.0, 2.0]).0, Pick::Face, "a side at {h} mm");
        assert_eq!(read([bx + 2.0, cy, 5.5]).0, Pick::Face, "the boss's round side at {h} mm");
    }
}

/// A plate bolted through a hole: the fixture on the hole holds what the bolt clamps, the faces
/// round the hole on both sides out to twice its radius (the wall left free), and keeps the
/// hole's axis; `rigid` holds the wall itself. A fixture on a face is that face either way.
#[test]
fn a_fixture_on_a_hole_holds_the_bolts_clamp_and_a_rigid_one_its_wall() {
    use crate::study::{Load, Model, Spot};
    let m = isotropic();
    let (lo, hi, r) = ([-10.0, -10.0, 0.0], [10.0, 10.0, 4.0], 2.0);
    let mut distance = |ps: &[V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = boxed(*p, lo, hi).max(r - (p[0] * p[0] + p[1] * p[1]).sqrt()));
    let h = 0.5;
    let model = Model::new(lo, hi, h, &mut distance, &m);
    let hole = Spot::Seed { pick: Pick::Hole, at: [r, 0.0, 2.0] };
    let face = Spot::Seed { pick: Pick::Face, at: [10.0, 0.0, 2.0] };
    let mut rigid = Load::new(Kind::Fixed, hole.clone());
    rigid.rigid = true;
    let c = model.conditions(&[Load::new(Kind::Fixed, hole), rigid, Load::new(Kind::Fixed, face)]);
    let s = &model.surface;
    let (clamp, wall, side) = (&c[0].region, &c[1].region, &c[2].region);
    let ring = 2.0 * std::f64::consts::PI * ((2.0 * r + 0.25 * h).powi(2) - r * r);
    assert!((clamp.area - ring).abs() < 0.1 * ring, "the clamp is both faces' rings: {:.1} mm^2 against {ring:.1}", clamp.area);
    assert!(clamp.faces.iter().all(|&q| s.quad_normal[q as usize][2].abs() >= 0.9), "no quad of the hole's wall is held");
    assert!(clamp.cylinder.is_some_and(|(_, a, rr)| a[2].abs() > 0.99 && (rr - r).abs() < 0.1 * r), "the clamp keeps the hole's axis: {:?}", clamp.cylinder);
    let tube = 2.0 * std::f64::consts::PI * r * 4.0;
    assert!((wall.area - tube).abs() < 0.1 * tube && wall.faces.iter().all(|&q| s.quad_normal[q as usize][2].abs() < 0.5), "rigid: the hole's wall, {:.1} mm^2 against {tube:.1}", wall.area);
    assert!((side.area - 80.0).abs() < 1.0, "a face fixture is its face: {:.1} mm^2", side.area);
}

/// The cantilever's safety factor read off two cell sizes and the finer one shifted half a
/// cell: away from the clamp (past its reach, a quarter of the face's side: 1 mm) the 0.5 and
/// 0.25 mm cells and the shifted 0.25 mm cells agree (measured 25.90, 24.83 and 24.98; 24.38 at
/// 0.125 mm), within 6 % of beam theory where it is lowest (23.57 at x 1.375 mm); at the clamp
/// each finer size finds more of its edge (23.5, 19.2, 15.4; 20.1 shifted): apart, left out of
/// the verdict. The cells within the reach are the ones near it, at every size.
#[test]
fn the_cantilevers_verdict_converges_away_from_the_clamp_and_not_at_it() {
    use crate::results::{Reading, Verdict};
    let m = isotropic();
    let (l, s) = crate::bench::BEAM;
    let i = s * s.powi(3) / 12.0;
    let beam = |x: f64| 50.0 / ((l - x) * 0.5 * s / i);
    let solved: Vec<crate::Outcome> = [0.5, 0.25].iter().map(|&h| {
        let (grid, surface, conditions) = crate::bench::cantilever(h, &m);
        let reach = crate::bc::reach(&conditions[0].region);
        assert!((reach - 1.0).abs() < 1e-3, "a 4 mm face reaches 1 mm: {reach}");
        let near = crate::bc::near(&grid, &surface, &conditions);
        // (a cell starting right at the reach may go either way)
        for c in (0..grid.cells()).filter(|&c| grid.active(c)) {
            let x = grid.cell_center(c)[0] - 0.5 * h;
            if (x - reach).abs() > 0.01 {
                assert_eq!(near[c], x < reach, "h {h}: the cell from x {x} is near the clamp only within its reach");
            }
        }
        crate::analyse(grid, &surface, &m, &conditions, &Options::default()).expect("a held part")
    }).collect();
    // the finest cells again on the grid shifted half a cell
    let shifted = {
        let (grid, surface, conditions) = crate::bench::cantilever_at(0.25, crate::study::shift(0.25), &m);
        crate::analyse(grid, &surface, &m, &conditions, &Options::default()).expect("a held part")
    };
    let reading = Reading::of(&solved[1], Some(&solved[0]), Some(&shifted));
    let Verdict::Converged { fine, coarse, shifted: Some(other) } = reading.away else { panic!("away from the clamp the cell sizes and both alignments agree: {reading:?}") };
    let lower = if shifted.results.min_sf.0 < solved[1].results.min_sf.0 { &shifted } else { &solved[1] };
    let x = lower.grid.cell_center(lower.results.min_sf.1)[0];
    assert!((fine - beam(x)).abs() < 0.06 * beam(x), "SF {fine:.3} (coarser {coarse:.3}, other alignment {other:.3}) at x {x:.3} against beam theory {:.3}", beam(x));
    assert!(matches!(reading.near, Verdict::Apart { fine, coarse, .. } if fine < coarse), "at the clamp the finer cells find more: {reading:?}");
    assert_eq!(reading.sf(), Some(reading.away.reported()), "the verdict is away's: {reading:?}");
    // the value said carries the rise once more: under the finer cells' reading, and on the safe
    // side of beam theory or within 3 % over it (FORCE A6, F34)
    let said = reading.away.reported();
    assert!(said < fine && said < 1.03 * beam(x) && said > beam(x) / 1.15, "SF said {said:.3} (read {fine:.3}, coarser {coarse:.3}) against beam theory {:.3}", beam(x));
}

/// The verdict takes the finest cells at two alignments: a step to finer cells that agrees
/// within the tolerance is not converged while the alignments disagree (the extreme moves with
/// where the cells cut the part), and it reads the lower alignment's value, less the margin: the
/// rise from the coarser cells once more, in 1 / SF.
#[test]
fn alignment_noise_cannot_pass_for_convergence() {
    use crate::results::Verdict;
    // fine and coarse 3 % apart, the alignments 15 %
    assert_eq!(Verdict::of(3.0, Some(3.1), Some(3.45)), Verdict::Apart { fine: 3.0, coarse: 3.1, shifted: Some(3.45) });
    // all within 10 % of the lowest, the shifted alignment the lower: its value
    assert_eq!(Verdict::of(3.2, Some(3.1), Some(3.0)), Verdict::Converged { fine: 3.0, coarse: 3.1, shifted: Some(3.2) });
    // 1 / 3.0 + (1 / 3.0 - 1 / 3.1) = 3.2 / 9.3
    let said = Verdict::of(3.2, Some(3.1), Some(3.0)).value().unwrap();
    assert!((said - 9.3 / 3.2).abs() < 1e-12, "the margin is the rise once more: {said}");
    // finer cells reading higher than the coarser: no margin, the lowest reading
    assert_eq!(Verdict::of(3.1, Some(3.0), Some(3.2)).value(), Some(3.0));
    assert_eq!(Verdict::of(f64::INFINITY, Some(f64::INFINITY), Some(f64::INFINITY)).value(), Some(f64::INFINITY));
    // unloaded everywhere agrees; a finite value against none does not
    assert!(matches!(Verdict::of(f64::INFINITY, Some(f64::INFINITY), Some(f64::INFINITY)), Verdict::Converged { .. }));
    assert!(matches!(Verdict::of(5.0, Some(f64::INFINITY), Some(5.0)), Verdict::Apart { .. }));
    // one step: unchecked, the lower alignment's
    assert_eq!(Verdict::of(3.0, None, Some(2.9)), Verdict::Unchecked(2.9));
}

/// A 24 x 6 x 6 mm beam as a slicer prints it at the default settings (0.2 mm layers, an
/// outer and an inner wall loop, three bottom and four top layers of lines along X, sparse
/// infill inside), its beads measured into the cells: the cells hold the beads' wall and skin
/// volume (within 2 %: measured 280.3 of 281.3 and 136.0 of 136.0 mm^3; the settings' estimate
/// has 249.2 and 160.7), the walls' beads run along the outline, and the cantilever deflects
/// within 5 % of the estimate (measured 0.2469 against 0.2559 mm: more wall, less skin).
#[test]
fn a_sliced_beam_reads_its_walls_and_skins_from_its_beads() {
    use crate::sliced::{Bead, Printed, Role};
    let print = Print::default();
    let m = Materials::new(crate::material::PLA, &print, true);
    let (l, s, h) = (24.0, 6.0, 0.75);
    let (lo, hi) = ([0.0; 3], [l, s, s]);
    let (grid, lattice, surface) = part(lo, hi, h, &m, |p| boxed(p, lo, hi));
    let lh = print.layer_height;
    let spacing = print.inner_width - lh * (1.0 - std::f64::consts::FRAC_PI_4);
    let wall = print.wall_thickness();
    let mut beads = Vec::new();
    let layers = (s / lh).round() as usize;
    for k in 0..layers {
        let z = (k + 1) as f64 * lh;
        // the loops' centrelines, inset from the outline, each its own width
        for (inset, width) in [(0.5 * print.outer_width, print.outer_width), (print.outer_width + 0.5 * spacing, spacing)] {
            let c = [[inset, inset], [l - inset, inset], [l - inset, s - inset], [inset, s - inset]];
            for e in 0..4 {
                let (a, b) = (c[e], c[(e + 1) % 4]);
                beads.push(Bead { a: [a[0], a[1], z], b: [b[0], b[1], z], width, height: lh, role: Role::Wall });
            }
        }
        if k < print.bottom_layers || k >= layers - print.top_layers {
            let inner = s - 2.0 * wall;
            let lines = (inner / print.inner_width).ceil();
            let width = inner / lines;
            for i in 0..lines as usize {
                let y = wall + (i as f64 + 0.5) * width;
                beads.push(Bead { a: [wall, y, z], b: [l - wall, y, z], width, height: lh, role: Role::Skin });
            }
        }
    }
    let printed = Printed::new(&lattice, &beads);
    let regraded = Grid::from_lattice(&lattice, grid.n, grid.origin, h, &m, Some(&printed));
    let volume = |g: &Grid, k: usize| -> f64 {
        g.gp.iter().flatten().map(|p| p.fill as f64 / 255.0 * [p.wall, p.skin][k] as f64 / 255.0).sum::<f64>() * (0.5 * h).powi(3)
    };
    for (k, role, what) in [(0, Role::Wall, "walls"), (1, Role::Skin, "skins")] {
        let beads: f64 = beads.iter().filter(|b| b.role == role).map(|b| ((b.b[0] - b.a[0]).powi(2) + (b.b[1] - b.a[1]).powi(2)).sqrt() * b.width * b.height).sum();
        let (sliced, estimated) = (volume(&regraded, k), volume(&grid, k));
        println!("sliced beam {what}: beads {beads:.1} mm^3, cells as sliced {sliced:.1}, estimated {estimated:.1}");
        assert!((sliced - beads).abs() < 0.02 * beads, "{what}: {sliced:.1} mm^3 in the cells, {beads:.1} in the beads");
    }
    // the side walls' beads run along X, the ends' along Y
    let bin = |p: V3| {
        let (c, _) = regraded.locate(p);
        regraded.gp[c].iter().filter(|g| g.wall > 128).map(|g| g.angle).next()
    };
    let near = |b: Option<u8>, want: u8| b.is_some_and(|b| (b as i32 - want as i32).rem_euclid(256).min((want as i32 - b as i32).rem_euclid(256)) <= 2);
    assert!(near(bin([12.0, 0.2, 3.0]), 0), "a side wall's beads: bin {:?}", bin([12.0, 0.2, 3.0]));
    assert!(near(bin([0.2, 3.0, 3.0]), 128), "an end's beads: bin {:?}", bin([0.2, 3.0, 3.0]));
    let tip = |g: Grid| {
        let root = pick(&surface, [0.0, 0.5 * s, 0.5 * s], Pick::Face);
        let end = pick(&surface, [l, 0.5 * s, 0.5 * s], Pick::Face);
        let conditions = [Condition { kind: Kind::Fixed, region: root }, Condition { kind: Kind::Force([0.0, 0.0, -10.0]), region: end }];
        let out = crate::analyse(g, &surface, &m, &conditions, &Options::default()).expect("a held part");
        -crate::results::displacement(&out.grid, &out.solution.u, [l, 0.5 * s, 0.5 * s])[2]
    };
    let (sliced, estimated) = (tip(regraded.clone()), tip(grid));
    println!("sliced beam tip: {sliced:.4} mm as sliced, {estimated:.4} estimated");
    assert!((sliced - estimated).abs() < 0.05 * estimated, "tip {sliced:.4} mm as sliced, {estimated:.4} estimated");
}

/// A 24 x 6 x 6 mm beam printed at the default settings (two walls, 15 % gyroid inside),
/// clamped at one end and pushed down 10 N at the other, with a zone of 100 % infill through
/// its middle third (a script's `m.settings` region). The zone's infill follows the same law as
/// the part's own: the beam bends less than without it and more than one solid all through,
/// and a zone over all of it bends it exactly as the part set to 100 % does. Five walls through
/// the middle third stiffen it too.
#[test]
fn a_solid_core_in_a_beams_middle_third_stiffens_it() {
    use crate::material::{Zone, PLA};
    use std::sync::Arc;
    let print = Print::default();
    let (l, s, h) = (24.0, 6.0, 0.75);
    let (lo, hi) = ([0.0; 3], [l, s, s]);
    // a box from x `from` to `to` through the beam's whole section
    let zone = |from: f64, to: f64, density: Option<f64>, wall_loops: Option<usize>| {
        let (zlo, zhi) = ([from, -1.0, -1.0], [to, s + 1.0, s + 1.0]);
        Zone { at: Arc::new(move |p| boxed(p, zlo, zhi)), bounds: Some((zlo, zhi)), density, infill: None, wall_loops }
    };
    let tip = |m: &Materials| {
        let (grid, _, surface) = part(lo, hi, h, m, |p| boxed(p, lo, hi));
        let root = pick(&surface, [0.0, 0.5 * s, 0.5 * s], Pick::Face);
        let end = pick(&surface, [l, 0.5 * s, 0.5 * s], Pick::Face);
        let conditions = [Condition { kind: Kind::Fixed, region: root }, Condition { kind: Kind::Force([0.0, 0.0, -10.0]), region: end }];
        let out = crate::analyse(grid, &surface, m, &conditions, &Options::default()).expect("a held part");
        -crate::results::displacement(&out.grid, &out.solution.u, [l, 0.5 * s, 0.5 * s])[2]
    };
    let part_m = Materials::new(PLA, &print, true);
    let plain = tip(&part_m);
    let cored = tip(&part_m.clone().with_zones(vec![zone(l / 3.0, 2.0 * l / 3.0, Some(1.0), None)]));
    let solid = tip(&Materials::new(PLA, &Print { density: 1.0, ..print.clone() }, true));
    let all = tip(&part_m.clone().with_zones(vec![zone(-1.0, l + 1.0, Some(1.0), None)]));
    let walled = tip(&part_m.clone().with_zones(vec![zone(l / 3.0, 2.0 * l / 3.0, None, Some(5))]));
    println!("beam tip: {plain:.4} mm plain, {cored:.4} with a solid core in its middle third, {solid:.4} solid all through, {all:.4} with a solid zone over all of it, {walled:.4} with five walls in its middle third");
    assert!(cored < plain, "a solid core in the middle third stiffens the beam: tip {cored:.4} mm with it, {plain:.4} without");
    assert!(cored > solid, "a core in a third of it stiffens it less than solid infill all through: tip {cored:.4} mm against {solid:.4}");
    assert!((all - solid).abs() <= 1e-9 * solid, "a 100 % zone over all of it reads as the part at 100 %: tip {all:.6} mm against {solid:.6}");
    assert!(walled < plain, "five walls in the middle third stiffen it: tip {walled:.4} mm with them, {plain:.4} without");
}

/// The Force program ([`crate::program`], the solve in Splash kernels) solves as the CPU solver
/// does: Kirsch's plate at 0.5 mm (cells integrated point by point around the hole, Galerkin
/// coarse levels) to the same tolerance in about as many iterations, its answer as close to the
/// CPU's as that tolerance leaves room for.
#[test]
fn the_force_program_solves_as_the_cpu_solver_does() {
    let m = isotropic();
    let opts = Options::default();
    let (grid, held, force) = crate::bench::kirsch(2.5, 20.0, 10.0, 0.5, 0.5, 10.0, &m);
    let h = crate::solve::Hierarchy::new(grid, &held, &m, &opts);
    assert!(h.levels.len() > 1, "a hierarchy of {} levels", h.levels.len());
    let cpu = crate::solve::solve(&h, &force, &opts);
    let prog = crate::program::solve(&h.levels[0], &force, None, &opts).unwrap();
    assert!(prog.residual <= opts.tolerance, "residual {} after {} iterations", prog.residual, prog.iterations);
    assert!(prog.iterations.abs_diff(cpu.iterations) <= 1, "{} iterations against the CPU's {}", prog.iterations, cpu.iterations);
    let len = |v: &V3| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let max = cpu.u.iter().map(len).fold(0.0, f64::max);
    let du = prog.u.iter().zip(&cpu.u).map(|(a, b)| len(&[a[0] - b[0], a[1] - b[1], a[2] - b[2]])).fold(0.0, f64::max);
    assert!(du <= 1e-5 * max, "|du| {du:e} of max |u| {max:e}");
}

/// From a guess (the answer to another load), the program starts where the CPU solver starts
/// (the multiple of the guess nearest the answer in energy) and solves as it does.
#[test]
fn the_force_program_from_a_guess_solves_as_the_cpu_solver_does() {
    let m = isotropic();
    let opts = Options::default();
    let (grid, held, force) = crate::bench::kirsch(2.5, 20.0, 10.0, 0.5, 0.5, 10.0, &m);
    let h = crate::solve::Hierarchy::new(grid, &held, &m, &opts);
    let before = crate::solve::solve(&h, &force, &opts);
    // 50 N to 80 N, with a sideways part the guess does not have
    let after: Vec<V3> = force.iter().map(|f| [1.6 * f[0] + 0.3 * f[1], 1.6 * f[1], 1.6 * f[2]]).collect();
    let rest = crate::solve::solve(&h, &after, &opts);
    let cpu = crate::solve::solve_until(&h, &after, Some(&before.u), &opts, &mut |_, _| false);
    let prog = crate::program::solve(&h.levels[0], &after, Some(&before.u), &opts).unwrap();
    assert!(cpu.iterations < rest.iterations, "the guess saves iterations: {} from it, {} from rest", cpu.iterations, rest.iterations);
    assert!(prog.residual <= opts.tolerance, "residual {} after {} iterations", prog.residual, prog.iterations);
    assert!(prog.iterations.abs_diff(cpu.iterations) <= 1, "{} iterations against the CPU's {}", prog.iterations, cpu.iterations);
    let len = |v: &V3| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let max = cpu.u.iter().map(len).fold(0.0, f64::max);
    let du = prog.u.iter().zip(&cpu.u).map(|(a, b)| len(&[a[0] - b[0], a[1] - b[1], a[2] - b[2]])).fold(0.0, f64::max);
    assert!(du <= 1e-5 * max, "|du| {du:e} of max |u| {max:e}");
}

/// A hub on loopback in this test binary (the service holds a process lock): its address and
/// a stream to it; one test at a time.
#[cfg(feature = "loopback")]
fn loopback_hub() -> (std::sync::MutexGuard<'static, ()>, String, crate::hub::Hub) {
    use std::sync::{Arc, Mutex, OnceLock};
    static TEST: Mutex<()> = Mutex::new(());
    let guard = TEST.lock().unwrap_or_else(|e| e.into_inner());
    static URL: OnceLock<String> = OnceLock::new();
    let url = URL.get_or_init(|| crate::hub::loopback("fab-force-hub").unwrap());
    let stream = makepad_ai_hub::kernel_client::KernelStream::open(&makepad_ai_hub::kernel_client::KernelNode::new(url)).unwrap();
    (guard, url.clone(), crate::hub::Hub::of(vec![(url.clone(), Arc::new(stream))]))
}

/// A study solved on the hub is the program's answer in this process, bit for bit, its
/// progress told as it runs; a solve gone stale mid-way leaves the hub free.
#[cfg(feature = "loopback")]
#[test]
fn a_study_on_the_hub_is_the_programs_answer_and_a_stale_one_frees_the_hub() {
    use crate::hub::Solve;
    use makepad_compute_program::Class;
    let (_guard, url, hub) = loopback_hub();
    let m = isotropic();
    let opts = Options::default();
    let (grid, held, force) = crate::bench::kirsch(2.5, 20.0, 10.0, 0.5, 0.5, 10.0, &m);
    let near = vec![false; grid.cells()];
    let posed = crate::pose(grid, held, force, near).unwrap();
    let fine = crate::solve::Level::new(posed.grid.clone(), &posed.held, &m);
    let here = crate::program::solve(&fine, &posed.force, None, &opts).unwrap();
    let solve = Solve::start(&hub, posed.clone(), &m, None, &opts, Class::Interactive, "", &mut || false).unwrap().unwrap();
    let mut told = Vec::new();
    let outcome = solve.wait(&mut |p| {
        told.extend(p.copied());
        false
    });
    let outcome = outcome.unwrap().expect("not stale");
    assert_eq!(outcome.solution.iterations, here.iterations);
    assert!(outcome.solution.u.iter().zip(&here.u).all(|(a, b)| (0..3).all(|k| a[k].to_bits() == b[k].to_bits())), "the hub's answer is this process's, bit for bit");
    // told as it goes (what came in together told as its newest)
    assert!(told.windows(2).all(|w| w[1].iterations > w[0].iterations), "{told:?}");
    assert!(told.iter().all(|p| (0.0..=1.0).contains(&p.fraction) && p.residual <= 1.0), "{told:?}");
    // to a tolerance it never reaches: it runs until it goes stale at its second iteration
    let endless = Options { tolerance: 0.0, max_iterations: usize::MAX, ..opts };
    let solve = Solve::start(&hub, posed, &m, None, &endless, Class::Interactive, "", &mut || false).unwrap().unwrap();
    assert!(matches!(solve.wait(&mut |p| p.is_some_and(|p| p.iterations >= 2)), Ok(None)), "a stale solve comes back as None");
    let node = makepad_ai_hub::kernel_client::KernelNode::new(&url);
    while node.info().unwrap().programs > 0 {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The hub keeps the coarse levels and the coarsest one's factor it built in the solve's keep
/// slot: a load edit kept there solves on them (it builds and factors none), its answer the
/// program's in this process bit for bit, and so does a block of loads in one session (each the
/// answer it has alone); one that keeps nothing, or on another model, builds and factors its own.
#[cfg(feature = "loopback")]
#[test]
fn the_hub_keeps_the_coarse_levels_for_a_load_edit_in_the_same_slot() {
    use makepad_ai_hub::kernel_client::{KernelNode, KernelStream};
    use makepad_ai_hub::program_client::{RemoteConfig, RemoteSession};
    use makepad_compute_program::{Class, Updates, Value};
    let (_guard, url, _hub) = loopback_hub();
    // the hub keeps a stream's levels: every solve on this one
    let stream = std::sync::Arc::new(KernelStream::open(&KernelNode::new(&url)).unwrap());
    let m = isotropic();
    let opts = Options::default();
    let (grid, held, force) = crate::bench::kirsch(2.5, 20.0, 10.0, 0.5, 0.5, 10.0, &m);
    let fine = crate::solve::Level::new(grid.clone(), &held, &m);
    // the plate held at one more of its nodes: another model
    let mut moved = held.clone();
    let free = fine.held.iter().position(|&h| h == 0).unwrap();
    moved[free] = crate::solve::HELD;
    let refixed = crate::solve::Level::new(grid, &moved, &m);
    let turned: Vec<V3> = force.iter().map(|f| [f[1], -f[0], f[2]]).collect();
    // a session on the hub solving a block of loads: their answers, and whether it built coarse
    // levels and factored the coarsest
    let solve = |fine: &crate::solve::Level, fs: &[&[V3]], keep: &str| {
        let (params, inputs) = crate::program::job_block(fine, fs, None, &opts, crate::program::Solves::ToTolerance).unwrap();
        let config = RemoteConfig { params, inputs, limits: crate::program::limits(), class: Class::Interactive, keep: keep.to_string(), ..RemoteConfig::default() };
        let session = RemoteSession::open(&stream, &crate::program::program(), config).unwrap();
        let mut all = Updates::default();
        loop {
            all.merge(session.wait());
            if let Some(done) = crate::program::answers(&all) {
                let solution = done.unwrap_or_else(|e| panic!("the hub's solve kept in {keep:?}: {e}"));
                let Some(Ok(end)) = &all.end else { unreachable!() };
                let Some(Value::Array(kernels)) = end.get("stats").and_then(|s| s.get("kernels")) else { panic!("no kernel stats: {end:?}") };
                let ran = |name: &str| kernels.iter().any(|k| matches!(k.get("label"), Some(Value::String(l)) if l == name));
                return (solution, [ran("galerkin"), ran("dense_panel")]);
            }
        }
    };
    let same = |a: &crate::solve::Solution, b: &crate::solve::Solution| a.iterations == b.iterations && a.u.iter().zip(&b.u).all(|(a, b)| (0..3).all(|k| a[k].to_bits() == b[k].to_bits()));
    let (first, built) = solve(&fine, &[&force], "plate");
    assert_eq!(built, [true; 2], "the first solve builds its coarse levels and factors the coarsest");
    let (edited, built) = solve(&fine, &[&turned], "plate");
    assert_eq!(built, [false; 2], "a load edit in the same slot solves on the levels and the factor kept there");
    let here = crate::program::solve(&fine, &turned, None, &opts).unwrap();
    assert!(same(&edited[0], &here), "on the kept levels the answer is this process's, bit for bit");
    let (block, built) = solve(&fine, &[&turned, &force], "plate");
    assert_eq!(built, [false; 2], "a block of loads solves on the levels and the factor kept there");
    assert!(block.len() == 2 && same(&block[0], &here) && same(&block[1], &first[0]), "a block's loads solved in one session: each the answer it has alone, bit for bit");
    assert_eq!(solve(&fine, &[&turned], "").1, [true; 2], "a solve that keeps nothing builds its own");
    assert_eq!(solve(&refixed, &[&turned], "plate").1, [true; 2], "another held node: another model");
}

/// A background batch places its jobs by their sizes alone: the largest first, each on the node
/// with the fewest cells so far (on a tie the first), so the same batch on the same nodes places
/// the same way every time.
#[cfg(feature = "node")]
#[test]
fn a_batch_places_the_largest_first_on_the_least_loaded_node() {
    let (placed, order) = crate::hub::place(&[10, 40, 30, 20], 2);
    assert_eq!(order, [1, 2, 3, 0], "the largest first");
    assert_eq!(placed, [0, 0, 1, 1], "each on the node with the fewest cells so far");
    assert_eq!(crate::hub::place(&[10, 40, 30, 20], 2), (placed, order), "the same every time");
}

/// A solve's fields read at a point: trilinear over the nodes round it, over those that have a
/// value; past the nodes with one, nothing, so a detail the cells did not see reads as not
/// analysed and not as unstressed.
#[test]
fn a_solves_fields_read_at_a_point_skip_the_nodes_with_none() {
    use crate::display::Values;
    let none = [f32::INFINITY; 4];
    let at = |v: f32| [v, -v, v * 0.5, 1.0];
    // one row of three cells: 0, 10, 20, no value, on both faces of the row
    let mut nodes = vec![none; 4 * 2 * 2];
    for j in 0..2 {
        for k in 0..2 {
            for (i, v) in [0.0, 10.0, 20.0].into_iter().enumerate() {
                nodes[i + 4 * (j + 2 * k)] = at(v);
            }
        }
    }
    let values = Values { origin: [100.0, 0.0, 0.0], h: 2.0, cells: [3, 1, 1], nodes };
    assert_eq!(values.at([101.0, 1.0, 1.0]), Some(at(5.0)), "halfway along the first cell");
    assert_eq!(values.at([105.0, 1.0, 1.0]).map(|v| v[0]), Some(20.0), "half its cell has values: those alone, not a blend with nothing");
    assert_eq!(values.at([106.0, 1.0, 1.0]), None, "on the nodes with none");
    let ramp = values.over_ramp(1, 20.0, true);
    assert_eq!((ramp[0], ramp[1], ramp[2], ramp[3]), (0.5, 0.25, 0.0, -1.0), "signed over 0..1, negative for none");
    assert_eq!(values.over_ramp(0, 10.0, false)[2], 2.0, "over the top stays over 1");
}

/// A load's region marked on the cells: on a block, the top lit at any point of it, also a point
/// a cell off the study's surface; a side wall through the same cells stays unlit, so a mark
/// stops at the real crease; another kind goes over it where it lies, and not round it.
#[test]
fn a_mark_lights_its_surface_and_stops_at_the_crease() {
    use crate::display::Marks;
    let m = isotropic();
    let (lo, hi) = ([0.0; 3], [12.0, 8.0, 4.0]);
    let (g, _, s) = part(lo, hi, 1.0, &m, |p| boxed(p, lo, hi));
    let up = [0.0, 0.0, 1.0];
    let top = s.select(|c, n| c[2] > 3.9 && n[2] > 0.9);
    let mut marks = Marks::new(&g);
    assert!(!marks.any);
    marks.mark(&s, &top.faces, 2);
    assert!(marks.any);
    assert_eq!(marks.at([6.0, 4.0, 4.0], up), 2, "on the top");
    assert_eq!(marks.at([6.0, 4.0, 4.0], [0.0, 0.0, -3.0]), 2, "either sign and any length of normal");
    assert_eq!(marks.at([6.0, 4.0, 3.2], up), 2, "under the study's surface, the same way up");
    assert_eq!(marks.at([12.0, 4.0, 3.5], [1.0, 0.0, 0.0]), 0, "a wall through a marked cell is not the top");
    assert_eq!(marks.at([6.0, 4.0, 0.5], up), 0, "the far side of the part");
    let spot = s.select(|c, n| n[2] > 0.9 && c[2] > 3.9 && (c[0] - 6.0).abs() < 1.0 && (c[1] - 4.0).abs() < 1.0);
    marks.mark(&s, &spot.faces, 6);
    assert_eq!(marks.at([6.0, 4.0, 4.0], up), 6, "the picked one over the other");
    assert_eq!(marks.at([1.0, 1.0, 4.0], up), 2, "and only where it lies");
}

/// A wall reads its own thickness whatever the cells that sampled it, in a field and in a mesh;
/// the row stops at the cell size that resolves it, and the demo bracket's plates are no fin.
#[test]
fn a_thin_wall_reads_its_thickness() {
    use crate::bench::{bracket, BRACKET};
    use crate::thin::Thickness;
    let m = isotropic();
    // a 1.6 mm fin on a block
    let fin = |p: V3| boxed(p, [0.0, 0.0, 0.0], [20.0, 20.0, 10.0]).min(boxed(p, [0.0, 9.0, 10.0], [20.0, 10.6, 40.0]));
    let (_, lattice, surface) = part([-2.0; 3], [22.0, 22.0, 42.0], 1.0, &m, fin);
    let mut distance = |ps: &[V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = fin(*p));
    let thick = Thickness::of_field(&surface, lattice.spacing, &mut distance);
    let thin = thick.thin_at(1.0).expect("1.6 cells through");
    assert!((thin.mm - 1.6).abs() < 0.05, "{thin:?}");
    assert!((0.3..0.7).contains(&thin.share), "the fin's faces are about half the surface: {thin:?}");
    assert_eq!(thick.thin_at(0.5), None, "3.2 cells through");
    // what a bent member's safety factor can read: the worst at or under its cells
    assert_eq!((crate::thin::reads_high(1.67), crate::thin::reads_high(2.3), crate::thin::reads_high(1.16)), (Some(3.5), Some(2.7), None));
    // a plate as triangles, 2 mm thick
    let (a, b) = ([0.0, 0.0, 0.0], [30.0, 30.0, 2.0]);
    let corners: Vec<[f64; 3]> = (0..8).map(|i: usize| std::array::from_fn(|k| if i >> k & 1 == 1 { b[k] } else { a[k] })).collect();
    let faces = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
    let triangles: Vec<[u32; 3]> = faces.iter().flat_map(|f: &[u32; 4]| [[f[0], f[1], f[2]], [f[0], f[2], f[3]]]).collect();
    let mesh = crate::mesh::Mesh::new(&corners, &triangles);
    let model = crate::study::Model::of_mesh([-2.0, -2.0, -2.0], [32.0, 32.0, 4.0], 1.0, &mesh, &m);
    let thin = Thickness::of_mesh(&model.surface, model.spacing(), &mesh).thin_at(1.0).expect("2 cells through");
    assert!((thin.mm - 2.0).abs() < 0.05, "{thin:?}");
    // 6 mm plates at the study's 1.06 mm cells: resolved, at twice that not
    let (_, lattice, surface) = part(BRACKET.0, BRACKET.1, 1.06, &m, bracket);
    let mut distance = |ps: &[V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = bracket(*p));
    let thick = Thickness::of_field(&surface, lattice.spacing, &mut distance);
    assert_eq!(thick.thin_at(1.06), None);
    assert!(thick.thin_at(2.12).is_some_and(|t| (t.mm - 6.0).abs() < 0.1), "{:?}", thick.thin_at(2.12));
}

/// A solve whose program says nothing still goes stale: asked while the hub is silent, and the
/// cancel reaches the program (the hub is free again).
#[cfg(feature = "loopback")]
#[test]
fn a_solve_goes_stale_while_the_hub_says_nothing() {
    use makepad_ai_hub::kernel_client::{KernelNode, KernelStream};
    use makepad_compute_program::Program;
    let (_guard, url, _hub) = loopback_hub();
    let node = KernelNode::new(&url);
    let stream = std::sync::Arc::new(KernelStream::open(&node).unwrap());
    let m = isotropic();
    let (grid, held, force) = crate::bench::kirsch(2.5, 20.0, 10.0, 0.5, 0.5, 10.0, &m);
    let near = vec![false; grid.cells()];
    let posed = crate::pose(grid, held, force, near).unwrap();
    let silent = Program::new("silent.splash", "{run: fn(s, p) { s.wait_inbox() return 1 }}");
    let solve = crate::hub::Solve::on_session(&stream, &silent, posed, &m);
    let mut asked = 0;
    let stale = solve.wait(&mut |p| {
        assert!(p.is_none(), "the program says nothing");
        asked += 1;
        asked == 3
    });
    assert!(matches!(stale, Ok(None)), "a stale solve comes back as None");
    while node.info().unwrap().programs > 0 {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

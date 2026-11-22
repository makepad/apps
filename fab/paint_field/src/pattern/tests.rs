use super::band::{each, Level, Surface};
use super::live::{self, Sim};
use crate::bake::Fan;
use makepad_csg_sdf::Clearance;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// The calling thread and `threads - 1` more.
fn threads(n: usize) -> impl Fn(usize, &(dyn Fn(usize) + Sync)) + Sync {
    move |len: usize, f: &(dyn Fn(usize) + Sync)| {
        let next = AtomicUsize::new(0);
        let work = || loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= len {
                break;
            }
            f(i);
        };
        std::thread::scope(|s| {
            for _ in 1..n.min(len) {
                s.spawn(work);
            }
            work();
        });
    }
}

fn all_threads() -> impl Fn(usize, &(dyn Fn(usize) + Sync)) + Sync {
    threads(crate::bake::tests::budget_threads())
}

/// A box `size` mm with a corner at the origin, its exact distance.
fn plate(size: P3, key: u64) -> Surface {
    let half = size.map(|v| v * 0.5);
    let distance = each(move |p: P3| {
        let q: [f64; 3] = std::array::from_fn(|k| (p[k] - half[k]).abs() - half[k]);
        let out = q.map(|v| v.max(0.0));
        (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt() + q[0].max(q[1]).max(q[2]).min(0.0)
    });
    Surface { distance, clearance: Clearance::uniform(1.0), bounds: ([0.0; 3], size), key }
}

/// An open tray `size` mm with a corner at the origin, its walls and floor `wall` thick.
fn tray(size: P3, wall: f64, key: u64) -> Surface {
    let cube = |p: P3, lo: P3, hi: P3| {
        let q: [f64; 3] = std::array::from_fn(|k| (p[k] - 0.5 * (lo[k] + hi[k])).abs() - 0.5 * (hi[k] - lo[k]));
        let out = q.map(|v| v.max(0.0));
        (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt() + q[0].max(q[1]).max(q[2]).min(0.0)
    };
    let distance = each(move |p: P3| cube(p, [0.0; 3], size).max(-cube(p, [wall; 3], [size[0] - wall, size[1] - wall, size[2] + wall])));
    Surface { distance, clearance: Clearance::uniform(1.0), bounds: ([0.0; 3], size), key }
}

fn ball(radius: f64, key: u64) -> Surface {
    let distance = each(move |p: P3| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() - radius);
    Surface { distance, clearance: Clearance::uniform(1.0), bounds: ([-radius; 3], [radius; 3]), key }
}

fn spec(preset: &str, scale: f64, seed: u32, steps: Option<u32>) -> Spec {
    let mut values = vec![("scale", scale), ("seed", seed as f64)];
    if let Some(s) = steps {
        values.push(("steps", s as f64));
    }
    Spec::new(Some(preset), &values).expect("a spec")
}

/// `spec` grown alone (out of the registry) to its steps.
fn grow(spec: &Spec, surface: &Surface, fan: Fan) -> (Arc<Sim>, Arc<Grid>) {
    let sim = Arc::new(Sim::alone(spec, surface, fan).expect("the surface has cells"));
    assert!(sim.advance(fan, &|| false, None), "it reaches its steps");
    let grid = sim.latest();
    assert!(grid.done && grid.step == spec.steps);
    (sim, grid)
}

/// A radix-2 FFT in place (`re`, `im` of a power-of-two length).
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let a = -2.0 * std::f64::consts::PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (c, s) = ((a * k as f64).cos(), (a * k as f64).sin());
                let (i, j) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[j] * c - im[j] * s, re[j] * s + im[j] * c);
                re[j] = re[i] - tr;
                im[j] = im[i] - ti;
                re[i] += tr;
                im[i] += ti;
            }
        }
        len <<= 1;
    }
}

/// The dominant wavelength (mm) of `field` sampled on an `n`×`n` grid `spacing` apart: the
/// peak of its radially averaged power spectrum (past the lowest rings).
fn wavelength(field: &[f64], n: usize, spacing: f64) -> f64 {
    let mean = field.iter().sum::<f64>() / field.len() as f64;
    let (mut re, mut im): (Vec<f64>, Vec<f64>) = (field.iter().map(|v| v - mean).collect(), vec![0.0; n * n]);
    for r in 0..n {
        fft(&mut re[r * n..(r + 1) * n], &mut im[r * n..(r + 1) * n]);
    }
    for c in 0..n {
        let (mut cr, mut ci): (Vec<f64>, Vec<f64>) = ((0..n).map(|r| re[r * n + c]).collect(), (0..n).map(|r| im[r * n + c]).collect());
        fft(&mut cr, &mut ci);
        for r in 0..n {
            re[r * n + c] = cr[r];
            im[r * n + c] = ci[r];
        }
    }
    let mut rings = vec![(0.0, 0usize); n / 2];
    for r in 0..n {
        for c in 0..n {
            let (fr, fc) = ((if r > n / 2 { r as i64 - n as i64 } else { r as i64 }) as f64, (if c > n / 2 { c as i64 - n as i64 } else { c as i64 }) as f64);
            let k = (fr * fr + fc * fc).sqrt().round() as usize;
            if k < n / 2 {
                rings[k].0 += re[r * n + c].powi(2) + im[r * n + c].powi(2);
                rings[k].1 += 1;
            }
        }
    }
    let power: Vec<f64> = rings.iter().map(|(p, c)| if *c > 0 { p / *c as f64 } else { 0.0 }).collect();
    let k = (3..n / 2 - 1).max_by(|a, b| power[*a].total_cmp(&power[*b])).expect("rings");
    // the peak between rings, by a parabola through it and its neighbours
    let (a, b, c) = (power[k - 1], power[k], power[k + 1]);
    let shift = if a - 2.0 * b + c != 0.0 { 0.5 * (a - c) / (a - 2.0 * b + c) } else { 0.0 };
    n as f64 * spacing / (k as f64 + shift.clamp(-0.5, 0.5))
}

/// The top face of a plate grown with `grid`, `n`×`n` samples `spacing` apart from (`x0`, `y0`),
/// just under the face at `z`.
fn top(grid: &Grid, n: usize, spacing: f64, x0: f64, y0: f64, z: f64) -> Vec<f64> {
    (0..n * n).map(|i| grid.value([x0 + (i % n) as f64 * spacing, y0 + (i / n) as f64 * spacing, z - 0.2])).collect()
}

fn ascii(field: &[f64], n: usize, every: usize) -> String {
    let mut s = String::new();
    for r in (0..n).step_by(every * 2) {
        for c in (0..n).step_by(every) {
            let v = field[r * n + c];
            s.push(if v > 0.66 { '#' } else if v > 0.33 { '+' } else { '.' });
        }
        s.push('\n');
    }
    s
}

/// Same spec, same surface: the same bytes, whatever the workers split it into.
#[test]
fn a_pattern_grows_the_same_bytes_from_the_same_seed() {
    let surface = plate([24.0, 24.0, 6.0], 1);
    let s = spec("spots", 3.0, 7, Some(400));
    let (_, a) = grow(&s, &surface, &threads(1));
    let (_, b) = grow(&s, &surface, &all_threads());
    assert_eq!(a.key, b.key);
    let bits = |g: &Grid| (0..2000).map(|i| g.value([(i % 50) as f64 * 0.48, (i / 50) as f64 * 0.6, 5.9]).to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b), "one worker or many, the same state");
    let (_, other) = grow(&spec("spots", 3.0, 8, Some(400)), &surface, &all_threads());
    assert_ne!(bits(&a), bits(&other), "another seed, another pattern");
}

/// Raising the steps carries on from where it is; lowering them replays from the seed: both
/// end on the state a run straight to that many steps ends on.
#[test]
fn more_steps_carry_on_and_fewer_replay_to_the_same_state() {
    let surface = plate([20.0, 20.0, 5.0], 2);
    let fan = all_threads();
    let (sim, _) = grow(&spec("stripes", 3.0, 1, Some(300)), &surface, &fan);
    let straight = |steps| grow(&spec("stripes", 3.0, 1, Some(steps)), &surface, &fan).1;
    let bits = |g: &Grid| (0..1600).map(|i| g.value([(i % 40) as f64 * 0.5, (i / 40) as f64 * 0.5, 4.9]).to_bits()).collect::<Vec<_>>();
    sim.set_target(900);
    assert!(sim.advance(&fan, &|| false, None));
    assert_eq!(bits(&sim.latest()), bits(&straight(900)), "300 then 600 more is 900");
    sim.set_target(500);
    assert!(sim.advance(&fan, &|| false, None));
    assert_eq!(bits(&sim.latest()), bits(&straight(500)), "back to 500 replays");
}

/// A pattern grown to its steps and let go by its paint: asked again for more, the
/// simulation it finds is the one that stopped there, and it carries on from that step to the
/// state a run straight to that many ends on.
#[test]
fn more_steps_on_a_finished_pattern_carry_on_from_where_it_stopped() {
    // a shape of this test's own: the process's simulations are shared
    let surface = plate([20.0, 20.0, 5.0], 0x5eed_0f_c0a7);
    let fan = all_threads();
    let shape = surface.key;
    let first = live::resolve(&spec("spots", 3.0, 5, Some(300)), shape, 0, &|| Some(surface.clone()), true, &fan, &|| false);
    assert!(first.sim.is_none() && first.grid.as_ref().is_some_and(|g| g.done && g.step == 300));
    drop(first);
    let more = live::resolve(&spec("spots", 3.0, 5, Some(900)), shape, 0, &|| Some(surface.clone()), false, &fan, &|| false);
    let sim = more.sim.clone().expect("it grows on");
    assert_eq!(sim.step(), 300, "it carries on from the 300 steps it grew, not from the seed");
    assert!(sim.advance(&fan, &|| false, None));
    let straight = grow(&spec("spots", 3.0, 5, Some(900)), &surface, &fan).1;
    let bits = |g: &Grid| (0..1600).map(|i| g.value([(i % 40) as f64 * 0.5, (i / 40) as f64 * 0.5, 4.9]).to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&sim.latest()), bits(&straight));
}

/// Each Turing preset grown on a flat plate at a scale: the dominant wavelength across its top
/// is within 15% of the scale asked for. (Prints the measured wavelength in cells, which is what
/// each preset's `cells` is.) A Lichtenberg figure's scale is its branches' spacing, not a
/// wavelength.
#[test]
fn the_scale_is_the_measured_wavelength() {
    let fan = all_threads();
    let scale = 4.0;
    let n = 128;
    let spacing = 0.35;
    let size = n as f64 * spacing + 8.0;
    let surface = plate([size, size, 4.0], 3);
    let mut wrong = Vec::new();
    for p in PRESETS.iter().filter(|p| p.kind == "Turing") {
        let s = spec(p.name, scale, 1, None);
        let t = Instant::now();
        let (sim, grid) = grow(&s, &surface, &fan);
        let took = t.elapsed().as_secs_f64();
        let field = top(&grid, n, spacing, 4.0, 4.0, 4.0);
        let lambda = wavelength(&field, n, spacing);
        let share = field.iter().filter(|v| **v > 0.5).count() as f64 / field.len() as f64;
        println!("{}: wavelength {lambda:.2} mm ({:.2} cells, preset {:.1}), {:.0}% second material, {} cells, {:.2} s\n{}", p.name, lambda / s.pitch(), p.cells, share * 100.0, sim.cells(), took, ascii(&field, n, 2));
        if (lambda / scale - 1.0).abs() > 0.15 || !(0.05..=0.95).contains(&share) {
            wrong.push(format!("{} {lambda:.2} mm ({:.0}% second)", p.name, share * 100.0));
        }
    }
    assert!(wrong.is_empty(), "off scale or no pattern: {wrong:?}");
}

/// On a ball the pattern is the same through the skin band along a radius (it follows the
/// surface, not cutting through it), and deep inside the part is the first material.
#[test]
fn a_pattern_follows_the_skin() {
    let fan = all_threads();
    let r = 15.0;
    let (_, grid) = grow(&spec("spots", 3.0, 2, Some(1500)), &ball(r, 4), &fan);
    let (mut differ, mut seen, mut high) = (0, 0, 0);
    for i in 0..400 {
        let (a, b) = (i as f64 * 0.61, i as f64 * 0.37);
        let dir = [a.cos() * b.sin(), a.sin() * b.sin(), b.cos()];
        let at = |d: f64| grid.value(dir.map(|x| x * (r - d)));
        let (outer, inner) = (at(0.05), at(0.45));
        seen += 1;
        high += (outer > 0.5) as usize;
        differ += ((outer > 0.5) != (inner > 0.5)) as usize;
        assert_eq!(at(4.0), 0.0, "deep inside is the first material");
    }
    assert!(high > seen / 20 && high < seen * 19 / 20, "a pattern on the ball: {high} of {seen} in the second material");
    assert!(differ * 10 < seen, "{differ} of {seen} radii change material within the skin");
}

/// The views bake a pattern from the sampler the slice reads: at every lattice point of the
/// view's volume, in whole bricks (the pattern's clear distance lets the bake skip them) and
/// sampled ones alike, the slot the slicer samples there.
#[test]
fn the_views_show_the_slot_the_slice_samples() {
    use crate::program::{Channel, Layer, Mix, MixField, Op, Region};
    let fan = all_threads();
    let surface = ball(15.0, 21);
    let s = spec("spots", 4.0, 3, Some(1500));
    let (_, grid) = grow(&s, &surface, &fan);
    let grown = Grown { spec: s, grid: Some(grid), sim: None };
    let mix = Mix { field: MixField::Pattern(grown), per_mm: 1.0, slots: vec![1, 2], sharp: 1.0, balance: 0.0 };
    let program = crate::Program::new(vec![Layer { region: Region::All, op: Op::Mix { channel: Channel::Color, mix: Arc::new(mix) } }]);
    let cell = crate::Volume::pitch(&program);
    let t = Instant::now();
    let v = crate::Volume::bake(&program, Channel::Color, ([-16.0; 3], [16.0; 3]), cell, None, &fan).expect("baked");
    println!("the view's bake: {} sample bricks in {:.1} ms", v.bricks(), t.elapsed().as_secs_f64() * 1e3);
    let (mut whole, mut sampled, mut second) = (0usize, 0usize, 0usize);
    for z in 0..v.dim[2] * 8 {
        for y in 0..v.dim[1] * 8 {
            for x in 0..v.dim[0] * 8 {
                let g = [v.min[0] * 8 + x, v.min[1] * 8 + y, v.min[2] * 8 + z];
                let p: P3 = g.map(|c| c as f64 * v.cell);
                let b: [i32; 3] = std::array::from_fn(|k| g[k].div_euclid(8) - v.min[k]);
                let is_whole = v.index[((b[2] * v.dim[1] + b[1]) * v.dim[0] + b[0]) as usize] < 0.0;
                let (shown, _) = crate::bake::tests::read(&v, g);
                let sliced = program.sample(Channel::Color, p).slot;
                assert_eq!(shown, sliced, "at {p:?} ({})", if is_whole { "a whole brick" } else { "a sampled brick" });
                if is_whole { whole += 1 } else { sampled += 1 }
                second += (sliced == 2) as usize;
            }
        }
    }
    println!("{whole} points in whole bricks, {sampled} sampled, {second} in the second material, cell {cell} mm");
    assert!(whole > sampled && second > 1000, "the bake skips bricks far from the skin and the pattern shows");
}


#[test]
fn a_pattern_reads_where_it_settled_and_still_grows_to_its_steps() {
    let fan = all_threads();
    let surface = plate([60.0, 60.0, 6.0], 0x5e77);
    for name in ["spots", "dots"] {
        let s = spec(name, 6.0, 7, None);
        let (sim, grid) = grow(&s, &surface, &fan);
        let settled = grid.settled.unwrap_or_else(|| panic!("{name} settles within its {} steps", s.steps));
        println!("{name}: settled at {settled} of {} steps, moving {:.1e} a step at the end", s.steps, sim.change());
        assert!(settled > s.preset.coarse + live::SETTLE_AFTER && settled < s.steps, "{name} settled at {settled}");
        // a readout only: the print's state is the one at its steps
        assert!(grid.done && grid.step == s.steps);
        // the same seed settles at the same step
        assert_eq!(grow(&s, &surface, &fan).1.settled, Some(settled));
    }
}

/// A Lichtenberg figure grows from the part's lowest point over its skin: on a plate standing on
/// its edge, a tree that climbs from that edge over the face, branches with gaps between them,
/// that stops once no gap is wide enough for another branch (settles); the same bytes from the
/// same seed whatever the workers.
#[test]
fn a_lichtenberg_figure_climbs_from_the_lowest_point() {
    let fan = all_threads();
    let surface = plate([90.0, 90.0, 20.0], 0x11c7);
    for name in ["tree", "circuit", "frost"] {
        let s = spec(name, 6.0, 3, None);
        let (_, grid) = grow(&s, &surface, &fan);
        let n = 96;
        let face = top(&grid, n, 0.9, 2.0, 2.0, 20.0);
        let share = face.iter().filter(|v| **v > 0.5).count() as f64 / face.len() as f64;
        // how high the tree climbs up the face, from the edge it stands on (y = 0)
        let reach = (0..n).rev().find(|r| (0..n).any(|c| face[r * n + c] > 0.5)).map_or(0.0, |r| 2.0 + r as f64 * 0.9);
        assert!(share > 0.1 && share < 0.6, "{name}: branches with gaps between them ({:.0}%)\n{}", share * 100.0, ascii(&face, n, 1));
        assert!(reach > 80.0, "{name} climbs the face: {reach:.0} mm\n{}", ascii(&face, n, 1));
        if name != "circuit" {
            assert!(grid.settled.is_some_and(|at| at < s.steps), "{name} stops growing within its steps");
        }
    }
    let s = spec("tree", 6.0, 3, Some(300));
    let (_, a) = grow(&s, &surface, &threads(1));
    let (_, b) = grow(&s, &surface, &fan);
    let bits = |g: &Grid| (0..4000).map(|i| g.value([(i % 80) as f64 * 1.1, (i / 80) as f64 * 1.7, 19.9]).to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b), "the same bytes whatever the workers");
    let other = grow(&spec("tree", 6.0, 4, Some(300)), &surface, &fan).1;
    assert_ne!(bits(&a), bits(&other), "another seed, another figure");
}

/// A cup: a ball's shell `wall` mm thick, `radius` mm to its middle, standing on the origin,
/// open above a rim `rim` mm over its centre.
fn cup(radius: f64, wall: f64, rim: f64, key: u64) -> Surface {
    let distance = each(move |p: P3| {
        let r = (p[0] * p[0] + (p[1] - radius).powi(2) + p[2] * p[2]).sqrt();
        ((r - radius).abs() - 0.5 * wall).max(p[1] - radius - rim)
    });
    let e = radius + wall;
    Surface { distance, clearance: Clearance::uniform(1.0), bounds: ([-e, -wall, -e], [e, radius + rim + wall, e]), key }
}

/// The cells of `level`'s band `shows` picks that no root reaches through cells it picks, face
/// to face.
fn strays(level: &Level, roots: &[usize], shows: &[bool]) -> usize {
    let mut seen = vec![false; level.band];
    let mut queue: std::collections::VecDeque<usize> = roots.iter().copied().filter(|&r| shows[r]).collect();
    for &r in &queue {
        seen[r] = true;
    }
    while let Some(i) = queue.pop_front() {
        for &j in &level.nb[i] {
            let j = j as usize;
            if shows[j] && !seen[j] {
                seen[j] = true;
                queue.push_back(j);
            }
        }
    }
    (0..level.band).filter(|&i| shows[i] && !seen[i]).count()
}

/// A Lichtenberg figure shows whole branches back to its roots: on a cup (its skin at every
/// slope, outside, over the rim and inside; at the larger scale the band is as thick as the
/// wall), a tree cell that shows hangs from one that shows and reads the material through, and
/// every band cell that reads it (over a half) reaches a root through such cells, face to face.
/// So does every band cell beside which the surface reads it (what the views and the slice
/// sample, where the skin crosses the lattice): no spot, no piece of a branch, stands alone.
#[test]
fn a_lichtenberg_figure_shows_whole_branches_back_to_its_roots() {
    let fan = all_threads();
    let surface = cup(30.0, 2.4, 12.0, 0xc0b);
    for scale in [6.0, 10.0] {
        for name in ["tree", "circuit", "frost"] {
            let s = spec(name, scale, 5, None);
            let (sim, grid) = grow(&s, &surface, &fan);
            let (level, state) = sim.fine_now();
            let branches = dla::branches(&state);
            let roots: Vec<usize> = branches.iter().filter(|b| b.0 == b.1).map(|b| b.0).collect();
            let places = dla::places(level, &state);
            // a cell that shows hangs from one that shows (a branch shows whole back to its
            // root), and reads the tree's material through (a twig's width at the least)
            let tree: std::collections::HashMap<usize, bool> = branches.iter().map(|b| (b.0, b.2)).collect();
            let cut = branches.iter().filter(|b| b.2 && !tree[&b.1]).count();
            let thin = branches.iter().filter(|b| b.2 && places[b.0] < 1.0).count();
            assert_eq!(cut, 0, "{name} at {scale} mm: no branch shows past a piece that does not");
            assert_eq!(thin, 0, "{name} at {scale} mm: every cell of a branch that shows reads the tree's material through");
            let shows: Vec<bool> = places.iter().map(|p| *p > 0.5).collect();
            // where the skin crosses the lattice beside a band cell (or through one), what it reads
            let (h, n) = (level.pitch, level.band);
            let dirs = [[0, 0, 0], [-1, 0, 0], [1, 0, 0], [0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1]];
            let at: Vec<P3> = (0..n).flat_map(|i| dirs.map(|o| std::array::from_fn(|k| (level.cells[i][k] + o[k]) as f64 * h))).collect();
            let mut d = vec![0.0; at.len()];
            (surface.distance)(&at, &mut d);
            let mut read = vec![false; n];
            for i in 0..n {
                let (a, da) = (at[7 * i], d[7 * i]);
                read[i] = da == 0.0 && grid.value(a) > 0.5;
                for o in 1..7 {
                    let (b, db) = (at[7 * i + o], d[7 * i + o]);
                    if (da > 0.0) != (db > 0.0) {
                        let t = da / (da - db);
                        read[i] |= grid.value(std::array::from_fn(|k| a[k] + t * (b[k] - a[k]))) > 0.5;
                    }
                }
            }
            let shown = shows.iter().filter(|s| **s).count();
            let (cells, crossed) = (strays(level, &roots, &shows), strays(level, &roots, &read));
            assert!(shown > 1000, "{name} at {scale} mm: the tree shows ({shown} cells)");
            assert_eq!(cells, 0, "{name} at {scale} mm: every cell that shows the tree reaches a root ({cells} of {shown} do not)");
            assert_eq!(crossed, 0, "{name} at {scale} mm: every spot the surface shows reaches a root ({crossed} cells do not)");
        }
    }
}

/// Scales tile the skin round their seeds: on a ball, each preset settles within its steps
/// with every band cell on the scale of the seed nearest it over the skin (its nearest point
/// of the surface to the seeds'), the seeds about a scale apart and the outlines a share of
/// the skin; `fill` makes scales the second material through, apart; the same bytes from the same
/// seed whatever the workers, another seed other scales.
#[test]
fn scales_tile_the_skin_round_their_seeds() {
    let fan = all_threads();
    let surface = ball(25.0, 0x5ca1e);
    let d2 = |a: [f32; 3], b: [f32; 3]| (0..3).map(|k| (a[k] - b[k]) * (a[k] - b[k])).sum::<f32>();
    // a scale to each (√3/2)·scale² of the ball, hexagonally packed
    let hex = 4.0 * std::f64::consts::PI * 25.0 * 25.0 / (0.75f64.sqrt() * 36.0);
    let second = |s: &Spec, level: &Level, state: &rd::State| rd::places(s, level, state, &fan)[..level.band].iter().filter(|p| **p > 0.5).count() as f64 / level.band as f64;
    let mut even_lines = 0.0;
    for name in ["spiral", "even"] {
        let s = spec(name, 6.0, 3, None);
        let (sim, grid) = grow(&s, &surface, &fan);
        let (level, state) = sim.fine_now();
        let n = level.band;
        let seeds = &state.seeds;
        assert!(grid.settled.is_some_and(|at| at < s.steps), "{name}: the scales meet within the steps ({:?})", grid.settled);
        let unclaimed = (0..n).filter(|&i| state.u[i] < 0.0 || state.v[i] < 0.0).count();
        assert_eq!(unclaimed, 0, "{name}: every cell on a scale, with a next one");
        let wrong = (0..n)
            .filter(|&i| {
                let p = level.surface[i];
                let best = seeds.iter().map(|q| d2(p, *q)).fold(f32::INFINITY, f32::min);
                best < d2(p, seeds[state.u[i] as usize]) - 1e-3
            })
            .count();
        assert_eq!(wrong, 0, "{name}: every cell on the scale of its nearest seed ({wrong} of {n} not)");
        let cells = s.scale / level.pitch;
        let near: Vec<f64> = seeds.iter().enumerate().map(|(k, a)| seeds.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, b)| d2(*a, *b)).fold(f32::INFINITY, f32::min).sqrt() as f64 / cells).collect();
        let mean = near.iter().sum::<f64>() / near.len() as f64;
        let least = near.iter().copied().fold(f64::MAX, f64::min);
        let count = seeds.len() as f64;
        if name == "spiral" {
            assert!(count > 0.9 * hex && count < 1.15 * hex, "spiral: a seed to each hexagon's worth of skin ({count} for {hex:.0})");
            assert!(least > 0.65 && mean > 0.8 && mean < 1.0, "spiral: seeds a scale apart (nearest {least:.2}, mean {mean:.2})");
        } else {
            assert!(least >= scales::APART - 1e-3 && mean < 1.2, "even: seeds no nearer than {} scales, about one apart (nearest {least:.2}, mean {mean:.2})", scales::APART);
        }
        let lines = second(&s, level, &state);
        assert!(lines > 0.15 && lines < 0.45, "{name}: outlines a share of the skin ({:.0}%)", lines * 100.0);
        // a share of the scales filled stand apart, not in streaks
        let through = scales::filled(s.seed, 0.15, seeds, cells);
        let taken: Vec<usize> = (0..seeds.len()).filter(|&k| through[k]).collect();
        assert_eq!(taken.len(), (0.15 * count).round() as usize, "{name}: the share filled");
        let touching = taken.iter().flat_map(|&a| taken.iter().map(move |&b| (a, b))).filter(|&(a, b)| a < b && (d2(seeds[a], seeds[b]) as f64) < (1.5 * cells).powi(2)).count();
        assert_eq!(touching, 0, "{name}: no two filled scales touch, at a side or a corner");
        even_lines = lines;
    }
    let filled = Spec::new(Some("even"), &[("scale", 6.0), ("seed", 3.0), ("fill", 0.3)]).expect("a spec");
    let (sim, _) = grow(&filled, &surface, &fan);
    let (level, state) = sim.fine_now();
    let through = second(&filled, level, &state);
    assert!(through > even_lines + 0.12 && through < even_lines + 0.4, "fill 0.3: some scales the second material through ({:.0}% against {:.0}%)", through * 100.0, even_lines * 100.0);
    let s = spec("spiral", 6.0, 3, None);
    let (_, a) = grow(&s, &surface, &threads(1));
    let (_, b) = grow(&s, &surface, &fan);
    let on = |i: usize| {
        let (z, t) = (1.0 - 2.0 * (i as f64 + 0.5) / 3000.0, i as f64 * 2.4);
        let r = (1.0 - z * z).sqrt() * 24.9;
        [r * t.cos(), z * 24.9, r * t.sin()]
    };
    let bits = |g: &Grid| (0..3000).map(|i| g.value(on(i)).to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b), "the same bytes whatever the workers");
    let other = grow(&spec("spiral", 6.0, 4, None), &surface, &fan).1;
    assert_ne!(bits(&a), bits(&other), "another seed, other scales");
}

/// The components of `field` (`n`×`n`) on the side of a half `on` gives, four-connected: their
/// sizes.
fn components(field: &[f64], n: usize, on: bool) -> Vec<usize> {
    let mut seen = vec![false; n * n];
    let mut sizes = Vec::new();
    for start in 0..n * n {
        if seen[start] || (field[start] > 0.5) != on {
            continue;
        }
        let (mut stack, mut size) = (vec![start], 0);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            size += 1;
            let (r, c) = (i / n, i % n);
            for (dr, dc) in [(0i64, 1i64), (0, -1), (1, 0), (-1, 0)] {
                let (r2, c2) = (r as i64 + dr, c as i64 + dc);
                if r2 < 0 || c2 < 0 || r2 >= n as i64 || c2 >= n as i64 {
                    continue;
                }
                let j = r2 as usize * n + c2 as usize;
                if !seen[j] && (field[j] > 0.5) == on {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        sizes.push(size);
    }
    sizes
}

/// A slime mould spreads from the top of a plate's middle: trunks there branch into threads,
/// each vein as wide as its trail is strong (the finest a twelfth of the scale), many threads
/// joined into one web round its meshes with no loose islands; the same bytes whatever the
/// workers, another seed another network.
#[test]
fn a_slime_mould_grows_trunks_that_branch_into_threads() {
    let fan = all_threads();
    let surface = plate([48.0, 4.0, 48.0], 0x51a1);
    let s = spec("network", 8.0, 3, None);
    let (_, grid) = grow(&s, &surface, &fan);
    // the top (up the y axis), where it starts, 0.2 mm apart from 2 mm in, just under it
    let (n, at) = (220, 0.2);
    let face: Vec<f64> = (0..n * n).map(|i| grid.value([2.0 + (i % n) as f64 * at, 3.8, 2.0 + (i / n) as f64 * at])).collect();
    let veins = face.iter().filter(|v| **v > 0.5).count();
    let part = veins as f64 / face.len() as f64;
    let web = components(&face, n, true).into_iter().max().unwrap_or(0);
    // each vein sample's width (mm): the shorter of its row's and its column's run of vein
    let run = |i: usize, step: (isize, isize)| {
        let (x, y) = ((i % n) as isize, (i / n) as isize);
        let on = |k: isize| {
            let (px, py) = (x + step.0 * k, y + step.1 * k);
            px >= 0 && py >= 0 && (px as usize) < n && (py as usize) < n && face[py as usize * n + px as usize] > 0.5
        };
        let (mut lo, mut hi) = (0, 0);
        while on(lo - 1) {
            lo -= 1;
        }
        while on(hi + 1) {
            hi += 1;
        }
        (hi - lo + 1) as f64 * at
    };
    let mut widths: Vec<f64> = (0..n * n).filter(|&i| face[i] > 0.5).map(|i| run(i, (1, 0)).min(run(i, (0, 1)))).collect();
    widths.sort_by(|a, b| a.total_cmp(b));
    let width = |f: f64| widths[((widths.len() - 1) as f64 * f) as usize];
    // the meshes: the gaps of a sample or more, by area
    let gaps: Vec<usize> = components(&face, n, false).into_iter().filter(|g| *g > 1).collect();
    let look = || format!("{:.0}% vein, {web} of {veins} joined, widths {:.1} to {:.1} mm, {} meshes\n{}", part * 100.0, width(0.1), width(0.9), gaps.len(), ascii(&face, n, 3));
    assert!(part > 0.1 && part < 0.45, "veins a share of the top: {}", look());
    assert!(web as f64 > 0.85 * veins as f64, "the veins join into one web, no islands: {}", look());
    assert!(width(0.9) > 2.5 * width(0.1), "trunks over twice a thread's width: {}", look());
    assert!(width(0.02) >= 0.08 * s.scale, "the finest thread a twelfth of the scale across: {}", look());
    // the threads: the veins every tenth row and column of samples crosses
    let on = |x: usize, y: usize| face[y * n + x] > 0.5;
    let crossed: usize = (0..n).step_by(10).map(|r| (1..n).filter(|&c| on(c, r) && !on(c - 1, r)).count() + (1..n).filter(|&c| on(r, c) && !on(r, c - 1)).count()).sum();
    assert!(gaps.len() >= 4 && crossed >= 60, "threads round meshes ({crossed} crossings, {} meshes): {}", gaps.len(), look());
    let s = spec("network", 8.0, 3, Some(200));
    let (_, a) = grow(&s, &surface, &threads(1));
    let (_, b) = grow(&s, &surface, &fan);
    let bits = |g: &Grid| (0..4000).map(|i| g.value([(i % 80) as f64 * 0.55 + 2.0, 3.8, (i / 80) as f64 * 0.55 + 2.0]).to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b), "the same bytes whatever the workers");
    let other = grow(&spec("network", 8.0, 4, Some(200)), &surface, &fan).1;
    assert_ne!(bits(&a), bits(&other), "another seed, another network");
}

/// A slime mould's veins as tubes on a plate: the middle of each vein as straight pieces lying in
/// its veins, as wide as they are; on the top the tubes print (nearly all of them: some cross
/// the edge and would hang off it), under it none does, and on the sides only those that stand
/// steep enough; every tube at least the thinnest; the same pieces whatever the workers.
#[test]
fn a_slime_moulds_veins_rise_as_tubes_that_print() {
    let fan = all_threads();
    let surface = plate([48.0, 4.0, 48.0], 0x51a1);
    let s = spec("network", 8.0, 3, None);
    let (sim, grid) = grow(&s, &surface, &fan);
    let (level, state) = sim.fine_now();
    let pieces = veins::centrelines(level, &state, &fan);
    let on = |p: [f64; 3], n: [f64; 3]| grid.value(std::array::from_fn(|k| p[k] - 0.2 * n[k])) > 0.5;
    let ends = pieces.iter().filter(|p| on(p.a, p.na) && on(p.b, p.nb)).count();
    let half: Vec<f64> = pieces.iter().flat_map(|p| [p.ra, p.rb]).collect();
    let look = || format!("{} pieces, {ends} with both ends on a vein, half-widths {:.2} to {:.2} mm", pieces.len(), half.iter().copied().fold(f64::MAX, f64::min), half.iter().copied().fold(0.0, f64::max));
    assert!(pieces.len() > 100, "{}", look());
    assert!(ends as f64 > 0.9 * pieces.len() as f64, "the pieces lie in the veins (a tip may end at its edge): {}", look());
    assert!(half.iter().all(|r| *r > 0.0 && *r < 0.6 * s.scale), "as wide as the veins: {}", look());
    let lowest = -(45f64.to_radians().cos());
    let tubes = veins::printable(&pieces, &surface.distance, [0.0, 1.0, 0.0], lowest, 0.0);
    let facing = |dir: f64| pieces.iter().filter(|p| p.na[1] * dir > 0.99 && p.nb[1] * dir > 0.99).count();
    let raised = |dir: f64| tubes.iter().filter(|t| t.cut[1] * dir > 0.99).count();
    let (top, under) = (facing(1.0), facing(-1.0));
    let told = format!("{} of {} pieces print: {} of {top} on the top, {} of {under} under it", tubes.len(), pieces.len(), raised(1.0), raised(-1.0));
    assert!(top > 50 && under > 50, "{told}");
    assert!(raised(1.0) as f64 > 0.9 * top as f64, "the top's tubes print: {told}");
    assert_eq!(raised(-1.0), 0, "none under it: {told}");
    // on the sides: steep ones only
    for t in tubes.iter().filter(|t| t.cut[1].abs() < 0.1) {
        let along = [t.b[0] - t.a[0], t.b[1] - t.a[1], t.b[2] - t.a[2]];
        let l = (along[0] * along[0] + along[1] * along[1] + along[2] * along[2]).sqrt();
        assert!(l < 2.0 * t.ra.max(t.rb) || (along[1] / l).abs() > 0.5, "a tube on a side that lies along it would hang: {t:?}");
    }
    let thinnest = (veins::THINNEST * veins::THINNEST + veins::LOWEST * veins::LOWEST) / (2.0 * veins::LOWEST);
    assert!(tubes.iter().all(|t| t.ra.min(t.rb) >= thinnest - 1e-9), "every tube at least the thinnest");
    let s = spec("network", 8.0, 3, Some(600));
    let (a, _) = grow(&s, &surface, &threads(1));
    let (b, _) = grow(&s, &surface, &fan);
    let (pa, pb) = (veins::centrelines(a.fine_now().0, &a.fine_now().1, &threads(1)), veins::centrelines(b.fine_now().0, &b.fine_now().1, &fan));
    assert!(!pa.is_empty(), "veins by 600 steps");
    assert_eq!(pa, pb, "the same pieces whatever the workers");
}

/// On the relief demo's dish (100 mm across, 20 high, a 2.4 mm wall, the floor's corner rounded
/// 8 mm) the tubes left after the cull make one network: no scrap of a vein the rest of which did
/// not print stands off it. Where veins meet on the floor the dome is as wide as the widest of
/// them, not as the trail's blob there: each vein's half-width the median along it from three
/// domes out (the dome's half-width the widest of its pieces at the junction).
#[test]
fn a_slime_moulds_tubes_on_a_dish_make_one_network() {
    let fan = all_threads();
    let len2 = |x: f64, y: f64| (x * x + y * y).sqrt();
    let distance = each(move |p: P3| {
        let (radius, height, wall, round) = (50.0, 20.0, 2.4, 8.0);
        let r = len2(p[0], p[2]);
        let (ox, oy) = (r - radius, (p[1] - height / 2.0).abs() - height / 2.0);
        let outer = len2(ox.max(0.0), oy.max(0.0)) + ox.max(oy).min(0.0);
        let (mid, half) = ((wall + height + 10.0) / 2.0, (height + 10.0 - wall) / 2.0);
        let (ix, iy) = (r - (radius - wall) + round, (p[1] - mid).abs() - half + round);
        outer.max(round - len2(ix.max(0.0), iy.max(0.0)) - ix.max(iy).min(0.0))
    });
    let surface = Surface { distance, clearance: Clearance::uniform(1.0), bounds: ([-51.0, 0.0, -51.0], [51.0, 21.0, 51.0]), key: 0x51a2 };
    let s = Spec::new(Some("trunks"), &[("scale", 10.0), ("seed", 3.0), ("spread", 0.5)]).expect("a spec");
    let (sim, _) = grow(&s, &surface, &fan);
    let (level, state) = sim.fine_now();
    let pieces = veins::centrelines(level, &state, &fan);
    let tubes = veins::printable(&pieces, &surface.distance, [0.0, 1.0, 0.0], -(45f64.to_radians().cos()), 0.0);
    let bits = |t: &veins::Tube| t.words().map(f32::to_bits);
    let kept: std::collections::HashSet<[u32; 12]> = tubes.iter().map(bits).collect();
    let on: Vec<(veins::Piece, veins::Tube)> = pieces.iter().map(|p| (*p, p.tube())).filter(|(_, t)| kept.contains(&bits(t))).collect();
    let floor = |p: &veins::Piece| p.na[1] > 0.99 && p.nb[1] > 0.99 && (p.a[1] - 2.4).abs() < 0.01;
    let told = format!("{} of {} pieces print, {} of {} on the floor", on.len(), pieces.len(), on.iter().filter(|(p, _)| floor(p)).count(), pieces.iter().filter(|p| floor(p)).count());
    assert!(on.iter().filter(|(p, _)| floor(p)).count() * 10 > pieces.iter().filter(|p| floor(p)).count() * 9, "the floor's tubes print: {told}");
    // one network: joined where the middle's end of one is in another
    let mut parent: Vec<usize> = (0..on.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            i = parent[i];
        }
        i
    }
    for i in 0..on.len() {
        for j in i + 1..on.len() {
            let ((pi, ti), (pj, tj)) = (on[i], on[j]);
            if ti.distance(pj.a) < 0.0 || ti.distance(pj.b) < 0.0 || tj.distance(pi.a) < 0.0 || tj.distance(pi.b) < 0.0 {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let networks = (0..on.len()).filter(|&i| root(&mut parent, i) == i).count();
    assert_eq!(networks, 1, "one network: {told}");
    // the domes where three pieces or more end on the floor
    let mut ends: std::collections::HashMap<[u64; 3], Vec<(usize, bool)>> = Default::default();
    for (i, p) in pieces.iter().enumerate() {
        ends.entry(p.a.map(f64::to_bits)).or_default().push((i, true));
        ends.entry(p.b.map(f64::to_bits)).or_default().push((i, false));
    }
    let far = |a: P3, b: P3| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    let mut over: Vec<(f64, String)> = Vec::new();
    for (k, list) in ends.iter().filter(|(k, l)| l.len() >= 3 && (f64::from_bits(k[1]) - 2.4).abs() < 0.01) {
        let dome = list.iter().map(|&(i, a)| if a { pieces[i].ra } else { pieces[i].rb }).fold(veins::THINNEST, f64::max);
        let (from, to) = (3.0 * dome, 3.0 * dome + 8.0);
        let mut widths = Vec::new();
        for &(first, at_a) in list {
            let (mut i, mut a, mut gone, mut rs) = (first, at_a, 0.0, Vec::new());
            loop {
                let p = pieces[i];
                let (here, there, r) = if a { (p.a, p.b, p.rb) } else { (p.b, p.a, p.ra) };
                gone += far(here, there);
                if gone >= from && gone <= to {
                    rs.push(r.max(veins::THINNEST));
                }
                let next: Vec<(usize, bool)> = ends[&there.map(f64::to_bits)].iter().copied().filter(|&(j, _)| j != i).collect();
                match next.as_slice() {
                    &[(j, ja)] if gone <= to => (i, a) = (j, ja),
                    _ => break,
                }
            }
            if !rs.is_empty() {
                rs.sort_by(f64::total_cmp);
                widths.push(rs[rs.len() / 2]);
            }
        }
        if let Some(widest) = widths.iter().copied().reduce(f64::max) {
            over.push((dome / widest, format!("{:.2} over {widths:.2?} at {:.1?}", dome, k.map(f64::from_bits))));
        }
    }
    over.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert!(over.len() > 10, "junctions on the floor: {over:?}");
    let (median, most) = (&over[over.len() / 2], &over[over.len() - 1]);
    assert!(median.0 < 1.1 && most.0 < 2.0, "a dome as wide as the widest vein meeting there: the middle {} ({}), the most {} ({})", median.0, median.1, most.0, most.1);
}

/// The extent (samples, the larger side of its box) of each component of `field` (`n`×`n`) on
/// the side of a half `on` gives, four-connected.
fn extents(field: &[f64], n: usize, on: bool) -> Vec<usize> {
    let mut seen = vec![false; n * n];
    let mut out = Vec::new();
    for s in 0..n * n {
        if seen[s] || (field[s] > 0.5) != on {
            continue;
        }
        seen[s] = true;
        let (mut stack, mut lo, mut hi) = (vec![s], [n; 2], [0; 2]);
        while let Some(c) = stack.pop() {
            let (x, y) = (c % n, c / n);
            (lo, hi) = ([lo[0].min(x), lo[1].min(y)], [hi[0].max(x), hi[1].max(y)]);
            let near = [(x > 0).then(|| c - 1), (x + 1 < n).then(|| c + 1), (y > 0).then(|| c - n), (y + 1 < n).then(|| c + n)];
            for d in near.into_iter().flatten() {
                if !seen[d] && (field[d] > 0.5) == on {
                    seen[d] = true;
                    stack.push(d);
                }
            }
        }
        out.push((hi[0] - lo[0]).max(hi[1] - lo[1]) + 1);
    }
    out
}

/// Flow lines run evenly over the skin: on a tray, each preset draws its lines till no start is
/// left (settling within its steps), the second material a share of the floor inside and of the
/// bottom outside about the lines' width over the scale, as much inside as out, and every skin
/// cell within a scale and a half of a line; long lines a scale apart across them (the measured
/// wavelength within 15% of it), streaks no longer than their length; the same bytes from the
/// same seed whatever the workers, another seed other lines.
#[test]
fn flow_lines_run_evenly_over_the_skin() {
    let fan = all_threads();
    let surface = tray([60.0, 60.0, 20.0], 2.4, 0xf10e);
    let (n, every) = (64, 0.8);
    let share = |face: &[f64]| face.iter().filter(|v| **v > 0.5).count() as f64 / face.len() as f64;
    for (preset, width) in [("lines", 0.35), ("contours", 0.35), ("streaks", 0.45 * 2.0 / 3.0)] {
        let s = spec(preset, 4.0, 3, None);
        let (sim, grid) = grow(&s, &surface, &fan);
        assert!(grid.settled.is_some_and(|at| at < s.steps), "@{preset} runs out of starts within its steps ({:?})", grid.settled);
        let (_, state) = sim.fine_now();
        let bare = state.v.iter().filter(|v| **v > 1.5 * s.preset.cells as f32).count();
        assert_eq!(bare, 0, "@{preset}: every skin cell within a scale and a half of a line");
        let floor = top(&grid, n, every, 2.8, 2.8, 2.4);
        let bottom = top(&grid, n, every, 2.8, 2.8, 0.4);
        let (inside, outside) = (share(&floor), share(&bottom));
        for (face, part, what) in [(&floor, inside, "the floor"), (&bottom, outside, "the bottom")] {
            assert!(part > 0.7 * width && part < 1.3 * width, "@{preset}: lines a share of {what} about their width ({:.0}% for {:.0}%)\n{}", part * 100.0, width * 100.0, ascii(face, n, 1));
            let long = extents(face, n, true).into_iter().max().unwrap_or(0) as f64 * every;
            if preset == "streaks" {
                let Model::Flow { length, .. } = s.model else { unreachable!("a flow") };
                assert!(long <= length * s.scale + 1.0, "@{preset}: no streak on {what} longer than {} mm ({long:.1} mm)\n{}", length * s.scale, ascii(face, n, 1));
            } else {
                assert!(long > 6.0 * s.scale, "@{preset}: long lines over {what} ({long:.1} mm)\n{}", ascii(face, n, 1));
            }
            if preset == "lines" {
                let wl = wavelength(face, n, every);
                assert!((wl - s.scale).abs() < 0.15 * s.scale, "@{preset}: lines a scale apart over {what} ({wl:.2} mm for {} mm)\n{}", s.scale, ascii(face, n, 1));
            }
        }
        assert!(inside < 1.3 * outside && outside < 1.3 * inside, "@{preset}: as much line inside as out ({:.0}% and {:.0}%)", inside * 100.0, outside * 100.0);
    }
    let s = spec("lines", 4.0, 3, Some(12));
    let (_, a) = grow(&s, &surface, &threads(1));
    let (_, b) = grow(&s, &surface, &fan);
    let bits = |g: &Grid| (0..4000).map(|i| g.value([(i % 80) as f64 * 0.7 + 1.0, (i / 80) as f64 * 0.7 + 1.0, 0.2]).to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&a), bits(&b), "the same bytes whatever the workers");
    let other = grow(&spec("lines", 4.0, 4, Some(12)), &surface, &fan).1;
    assert_ne!(bits(&a), bits(&other), "another seed, other lines");
}


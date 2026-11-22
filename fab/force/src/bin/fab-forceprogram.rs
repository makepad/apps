//! `fab-forceprogram [--node host:port,...] [--steps N] [--threads N] [--batch N] [--scenarios N] [--buckle N] [--repeat N] [--class C] [--codec C]`:
//! the demo bracket as printed in PLA at a study's first `N` cell sizes (default 1), solved three
//! ways: the CPU solver (`fab_force::solve`), the Force program in this process
//! (`fab_force::program`, on `--threads` workers, default every core), and the program on a hub:
//! one started in this process on loopback (as the AI hub app serves this machine's apps, with
//! this machine's GPU device where it has one: the program's GPU engine), then each `--node` (an
//! AI hub serving compute programs). Per way its iterations and time, and the program's time per
//! kernel (and on a hub, its GPU engine's runs and reads); the program's answer against the
//! CPU's, and each hub's against this process's, bit for bit. Each GPU and program run goes
//! twice: the first compiles its kernels, the second is the solve a study pays for.
//!
//! `--scenarios N` (in place of the above): a study and its edits as the app runs them, on the
//! loopback hub (`fab_force::hub`, each step's model kept there in its slot: the app's route) and,
//! to compare, on the CPU solver in this process. A study is its steps, then the finest on its
//! grid shifted half a cell; each step solves from its last answer on what it keeps while its
//! shape and held nodes are the same, as the app's study does: the CPU solver its hierarchy, the
//! hub route its analysed level (the hub keeps the coarse levels it built of it in the step's
//! slot). In order: the first study (cold: nothing kept), the load moved from the
//! shelf's top to its front edge, the load turned, and the fillet made larger (the shape edited:
//! every hierarchy built again). Each route runs that `N` times from cold (the hub on a new stream, so it keeps
//! nothing); per row the time from the edit to the refined answer and to the first step's,
//! each round's and their medians. A scaling of the loads is not a row: the app answers it
//! without a solve on every route. Then where the turned load's finest solve on the hub spends
//! its time, phase by phase (`phases`), and a cold solve of the finest step cancelled at its
//! second iteration: how long the hub takes to be free again.
//!
//! `--buckle N` (in place of the above): Fab's buckling stage on the bracket as the app runs it
//! on the loopback hub, `N` times: per step its LOBPCG steps, its preconditioner's sessions (a
//! block of loads each), where a session's time goes, phase by phase, and the step's wall time.
//!
//! `--class interactive|background`: the class the hub's solves run at (default interactive, a
//! study someone waits on: its GPU passes stay short while the app draws on the same GPU).
//!
//! `--codec raw|lz4|shuffle-lz4|delta-lz4|delta-deflate`: how the program's inputs and outputs
//! cross to each node (default the stream's: as they are on loopback, delta-lz4 past it); a run
//! on a node says how long its job took to lay out (`job`) and to open there (`open`: coded,
//! sent, decoded, the session started).
//!
//! `--repeat N`: then the finest step's hierarchy built and solved on the CPU solver `N` times in
//! a row, each one's ms (the code settled, under the JIT).
//!
//! `--batch N`: then an infill floor's round, `N` densities at every cell size as one background
//! batch (`fab_force::hub::analyse`), on the loopback hub alone and on it and the `--node`s
//! (just those, not the fleet's beacon); its wall time each way and how far the answers are
//! apart.

use fab_force::bench::{self, BRACKET};
use fab_force::buckle::{self, Loose, Start};
use fab_force::grid::Grid;
use fab_force::surface::Pick;
use fab_force::bc::Kind;
use fab_force::hub::{self, Hub, Job, Solve};
use fab_force::material::{Materials, Print, PLA};
use fab_force::program;
use fab_force::solve::{Hierarchy, Level, Options, Solution};
use fab_force::study::{self, Load, Model, Spot};
use fab_force::{Posed, V3};
use makepad_ai_hub::kernel_client::{KernelNode, KernelStream};
use makepad_ai_hub::program_client::{RemoteConfig, RemoteSession};
use makepad_compute_program::codec::Codec;
use makepad_compute_program::{Class, Session, SessionConfig, Updates, Value};
use std::sync::Arc;
use std::time::Instant;

fn die(m: &str) -> ! {
    eprintln!("fab-forceprogram: {m}");
    std::process::exit(2)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn len(v: &V3) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// The largest |a - b| over the largest |b|.
fn apart(a: &[V3], b: &[V3]) -> f64 {
    let max = b.iter().map(len).fold(0.0, f64::max);
    a.iter().zip(b).map(|(a, b)| len(&[a[0] - b[0], a[1] - b[1], a[2] - b[2]])).fold(0.0, f64::max) / max
}

/// Values whose bits differ.
fn differing(a: &[V3], b: &[V3]) -> usize {
    a.iter().zip(b).flat_map(|(a, b)| (0..3).map(move |k| (a[k].to_bits() != b[k].to_bits()) as usize)).sum::<usize>() + 3 * a.len().abs_diff(b.len())
}

/// The program's time per kernel, from its end value: `label calls ms (ns an element)`, longest
/// first.
fn kernel_times(u: &Updates) -> String {
    let Some(Ok(end)) = &u.end else { return String::new() };
    let Some(Value::Array(kernels)) = end.get("stats").and_then(|s| s.get("kernels")) else { return String::new() };
    let mut rows: Vec<(String, f64, f64, f64)> = kernels
        .iter()
        .filter_map(|k| match k.get("label") {
            Some(Value::String(l)) => Some((l.clone(), k.get("calls")?.as_number()?, k.get("ms")?.as_number()?, k.get("elements")?.as_number()?)),
            _ => None,
        })
        .collect();
    rows.sort_by(|a, b| b.2.total_cmp(&a.2));
    let total: f64 = rows.iter().map(|r| r.2).sum();
    let list: Vec<String> = rows.iter().map(|(l, c, ms, e)| format!("{l} {c:.0}x {ms:.0} ms ({:.1} ns)", ms * 1e6 / e.max(1.0))).collect();
    format!("kernels {total:.0} ms: {}", list.join(", "))
}

/// The bytes a job sends a node: in all, and its largest inputs.
fn upload(fine: &Level, f: &[V3], opts: &Options) -> String {
    let Ok((_, mut inputs)) = program::job(fine, f, None, opts) else { return String::new() };
    inputs.sort_by_key(|i| std::cmp::Reverse(i.data.len()));
    let mb = |words: usize| words as f64 * 4e-6;
    let total: usize = inputs.iter().map(|i| i.data.len()).sum();
    let largest: Vec<String> = inputs.iter().take(4).map(|i| format!("{} {:.1}", i.name, mb(i.data.len()))).collect();
    format!("inputs {:.1} MB ({})", mb(total), largest.join(", "))
}

/// The program in this process on `threads` workers: ms, its answer and its kernel times.
fn here(fine: &Level, f: &[V3], opts: &Options, threads: usize) -> Result<(f64, Solution, String), String> {
    let t = Instant::now();
    let (params, inputs) = program::job(fine, f, None, opts)?;
    let session = Session::open(&program::program(), SessionConfig { params, inputs, limits: program::limits(), threads, ..SessionConfig::default() });
    let mut all = Updates::default();
    loop {
        all.merge(session.wait());
        if let Some(done) = program::answer(&all) {
            return done.map(|s| (ms(t), s, kernel_times(&all)));
        }
    }
}

/// Its GPU engine's runs, reads and iterations sent, from the program's end value (none: it
/// solved on the CPU).
fn gpu_engine(u: &Updates) -> String {
    let Some(Ok(end)) = &u.end else { return String::new() };
    let Some(g) = end.get("gpu") else { return "on the CPU".into() };
    let n = |k: &str| g.get(k).and_then(Value::as_number).unwrap_or(0.0);
    format!("GPU engine: {:.0} runs, {:.0} reads, {:.0} iterations sent, {:.0} loads finished in f64", n("runs"), n("reads"), n("sent"), n("finished"))
}

/// The program on a node: (job, open, solve) ms, its answer, and its kernel times and engine.
fn on_node(stream: &Arc<KernelStream>, fine: &Level, f: &[V3], opts: &Options, class: Class, codec: Option<Codec>) -> Result<(f64, f64, f64, Solution, String), String> {
    let t = Instant::now();
    let (params, inputs) = program::job(fine, f, None, opts)?;
    let job = ms(t);
    let t = Instant::now();
    let config = RemoteConfig { params, inputs, limits: program::limits(), class, codec, ..RemoteConfig::default() };
    let session = RemoteSession::open(stream, &program::program(), config).map_err(|e| format!("{e:?}"))?;
    let open = ms(t);
    let t = Instant::now();
    let mut all = Updates::default();
    loop {
        all.merge(session.wait());
        if let Some(done) = program::answer(&all) {
            return done.map(|s| (job, open, ms(t), s, format!("{}; {}", gpu_engine(&all), kernel_times(&all))));
        }
    }
}

/// A hub on loopback in this process, as the AI hub app serves this machine's apps: its address.
fn loopback_hub() -> String {
    let url = hub::loopback("fab-forceprogram").unwrap_or_else(|e| die(&e));
    // with this machine's GPU, as the AI hub app has it
    match makepad_compute_program_gpu::spawn() {
        Ok(link) => makepad_ai_hub::program_serve::service().set_gpu(link),
        Err(e) => println!("the loopback hub has no GPU device ({e}): it solves on the CPU"),
    }
    url
}

/// Where a study solves: the CPU solver in this process, or this machine's hub on loopback (the
/// app's route).
#[derive(Clone, Copy, PartialEq)]
enum Route {
    Cpu,
    Hub,
}

impl Route {
    /// The hub first: its GPU device presents its first frame at once (the run's first-frame
    /// deadline), where the CPU solver's rounds would hold it back a minute.
    const ALL: [Route; 2] = [Route::Hub, Route::Cpu];

    fn name(self) -> &'static str {
        match self {
            Route::Cpu => "CPU solver",
            Route::Hub => "hub",
        }
    }
}

/// What a route builds of a step's model and held nodes before it solves: the CPU solver the
/// hierarchy; the hub route the analysed level (the hub builds the coarse levels itself).
#[derive(Clone)]
enum Built {
    Hierarchy(Arc<Hierarchy>),
    Analysed(Arc<Level>),
}

/// A step between solves, as the app keeps it: its shape (the fillet), held nodes, what its
/// route built for them, and its last answer on its grid.
struct Kept {
    fillet: f64,
    held: Vec<u8>,
    built: Built,
    grid: std::sync::Arc<Grid>,
    answer: Vec<V3>,
}

fn same_lattice(a: &Grid, b: &Grid) -> bool {
    a.n == b.n && a.h == b.h && a.origin == b.origin
}

/// One study on `route` with `loads` on `models` (the steps', the finest shifted last, of the
/// bracket with this `fillet`), each step from what `kept` holds for it: what it built while its
/// shape and held nodes are the same, its last answer while its lattice is (the shifted step
/// else the finest's carried over). Each step's ms (building, solve, outcome); `kept` updated.
#[allow(clippy::too_many_arguments)]
fn study(route: Route, hub: &Hub, models: &[Model], fillet: f64, loads: &[Load], kept: &mut [Option<Kept>], m: &Materials, opts: &Options, class: Class) -> Result<Vec<f64>, String> {
    let mut times = Vec::new();
    let mut finest: Option<(std::sync::Arc<Grid>, Vec<V3>)> = None;
    for (i, model) in models.iter().enumerate() {
        let posed = model.pose(&model.conditions(loads))?;
        let t = Instant::now();
        let k = kept[i].as_ref();
        let guess = match (k.filter(|k| same_lattice(&k.grid, &posed.grid)), &finest) {
            (Some(k), _) => Some(k.answer.clone()),
            (None, Some((grid, u))) if i + 1 == models.len() => Some(fab_force::results::carry(grid, u, &posed.grid)),
            _ => None,
        };
        let built = match k.filter(|k| k.fillet == fillet && k.held == posed.held) {
            Some(k) => k.built.clone(),
            None if route == Route::Hub => Built::Analysed(Arc::new(Level::new(posed.grid.clone(), &posed.held, m))),
            None => Built::Hierarchy(Arc::new(Hierarchy::new(posed.grid.clone(), &posed.held, m, opts))),
        };
        let outcome = match (route, &built) {
            (Route::Cpu, Built::Hierarchy(h)) => fab_force::analyse_on(h, posed, m, guess.as_deref(), opts, &mut |_, _| false),
            (Route::Hub, Built::Analysed(fine)) => Solve::start_on(hub, fine, posed, m, guess.as_deref(), opts, class, &format!("step {i}"), &mut || false)?.ok_or("stale")?.wait(&mut |_| false)?.ok_or("stale")?,
            _ => return Err(format!("{} kept what another route built", route.name())),
        };
        times.push(ms(t));
        if i + 2 == models.len() {
            finest = Some((outcome.grid.clone(), outcome.solution.u.clone()));
        }
        kept[i] = Some(Kept { fillet, held: outcome.held, built, grid: outcome.grid, answer: outcome.solution.u });
    }
    Ok(times)
}

/// The bracket's study with its shelf load `force` (N) at `at` on the shelf's top.
fn shelf_load(force: V3, at: V3) -> Vec<Load> {
    bench::bracket_loads().into_iter().map(|l| match l.kind {
        Kind::Force(_) => Load::new(Kind::Force(force), Spot::Seed { pick: Pick::Face, at }),
        _ => l,
    }).collect()
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(f64::total_cmp);
    xs[xs.len() / 2]
}

/// The study and its edits on every route, `rounds` times each from cold (see the header's
/// `--scenarios`), then the hub's cancel-to-free.
fn scenarios(url: &str, steps: &[f64], m: &Materials, opts: &Options, rounds: usize, class: Class) {
    let (lo, hi) = BRACKET;
    let at = |v: V3, s: V3| [v[0] + s[0], v[1] + s[1], v[2] + s[2]];
    let finest = *steps.last().unwrap();
    let grids: Vec<(f64, V3)> = steps.iter().map(|&h| (h, [0.0; 3])).chain([(finest, study::shift(finest))]).collect();
    let models_of = |fillet: f64| -> Vec<Model> {
        let mut field = |ps: &[V3], out: &mut [f64]| {
            for (p, o) in ps.iter().zip(out.iter_mut()) {
                *o = bench::bracket_filleted(*p, fillet);
            }
        };
        grids.iter().map(|&(h, shift)| Model::new(at(lo, shift), at(hi, shift), h, &mut field, m)).collect()
    };
    let shapes = [(9.0, models_of(9.0)), (12.0, models_of(12.0))];
    // the shelf's top, then its front edge's face (a face pick takes the whole face)
    let (top, front) = ([0.0, 10.0, 6.0], [0.0, 23.0, 3.0]);
    let rows: [(&str, usize, Vec<Load>); 4] = [
        ("first study (cold)", 0, shelf_load([0.0, 0.0, -100.0], top)),
        ("load moved to the shelf's front", 0, shelf_load([0.0, 0.0, -100.0], front)),
        ("load turned (60 out, 80 down)", 0, shelf_load([0.0, 60.0, -80.0], front)),
        ("fillet 9 -> 12 mm (re-mesh)", 1, shelf_load([0.0, 60.0, -80.0], front)),
    ];
    // [row][route] -> each round's (edit to refined, first step) ms
    let mut taken: Vec<Vec<Vec<(f64, f64)>>> = rows.iter().map(|_| Route::ALL.iter().map(|_| Vec::new()).collect()).collect();
    for round in 0..rounds {
        for (r, &route) in Route::ALL.iter().enumerate() {
            let hub = Hub::of(vec![("loopback".to_string(), Arc::new(KernelStream::open(&KernelNode::new(url)).unwrap_or_else(|e| die(&format!("the loopback hub's stream: {e:?}")))))]);
            let mut kept: Vec<Option<Kept>> = grids.iter().map(|_| None).collect();
            for (row, (name, shape, loads)) in rows.iter().enumerate() {
                let (fillet, models) = &shapes[*shape];
                match study(route, &hub, models, *fillet, loads, &mut kept, m, opts, class) {
                    Ok(times) => {
                        let total: f64 = times.iter().sum();
                        let steps: Vec<String> = times.iter().map(|t| format!("{t:.0}")).collect();
                        println!("round {round} {:<10} {name:<32} {total:>7.0} ms (steps {})", route.name(), steps.join(" + "));
                        taken[row][r].push((total, times[0]));
                    }
                    Err(e) => println!("round {round} {:<10} {name}: {e}", route.name()),
                }
            }
        }
    }
    println!("medians of {rounds}, edit to refined (to the first step's answer), ms:");
    println!("  {:<32} {}", "", Route::ALL.iter().map(|r| format!("{:>18}", r.name())).collect::<String>());
    for (row, (name, ..)) in rows.iter().enumerate() {
        let cells: String = taken[row]
            .iter()
            .map(|runs| match runs.is_empty() {
                true => format!("{:>18}", "-"),
                false => format!("{:>18}", format!("{:.0} ({:.0})", median(runs.iter().map(|r| r.0).collect()), median(runs.iter().map(|r| r.1).collect()))),
            })
            .collect();
        println!("  {name:<32} {cells}");
    }
    phases(url, &shapes[0].1[steps.len() - 1], &rows[1].2, &rows[2].2, m, opts, rounds, class);
    // a cold solve of the shifted finest step, gone stale at its second iteration
    let hub = Hub::of(vec![("loopback".to_string(), Arc::new(KernelStream::open(&KernelNode::new(url)).unwrap_or_else(|e| die(&format!("{e:?}")))))]);
    let model = shapes[0].1.last().unwrap();
    let posed = model.pose(&model.conditions(&rows[2].2)).unwrap_or_else(|e| die(&e));
    let solve = Solve::start(&hub, posed, m, None, opts, class, "", &mut || false).unwrap_or_else(|e| die(&e)).unwrap_or_else(|| die("a solve went stale"));
    let mut told: Vec<(usize, Instant)> = Vec::new();
    let stale = solve.wait(&mut |p| {
        told.extend(p.map(|p| (p.iterations, Instant::now())));
        p.is_some_and(|p| p.iterations >= 2)
    });
    let t = Instant::now();
    if !matches!(stale, Ok(None)) {
        die("the solve did not go stale");
    }
    let iteration = told.windows(2).last().map_or(0.0, |w| (w[1].1 - w[0].1).as_secs_f64() * 1e3 / (w[1].0 - w[0].0) as f64);
    let node = KernelNode::new(url);
    while node.info().map_or(true, |i| i.programs > 0) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    println!("cancel: the hub was free {:.1} ms after a solve went stale at iteration 2 (an iteration {iteration:.1} ms)", ms(t));
}

/// Where a load edit's solve on the hub spends its time: `model` (a study's finest step) with the
/// `turned` loads, from the answer to the `moved` ones, its analysed level kept in its slot (the
/// row "load turned" of [`scenarios`], as the app solves it), `rounds` times (each after the
/// moved loads again from its answer). Per round: `job` (laid out here), `open` (hashed, coded,
/// sent, decoded and started there), `first` (to the start's residual: the coarse levels
/// recalled, every level's inverse diagonal, the load), `setup` (from there to the first GPU
/// read, its iterations taken out: the GPU's layout and copies), its iterations (an iteration's
/// ms, with the f64 corrections), `back` (from the last residual to the end here: the answer
/// published, coded, sent and decoded by `u`, then the program's end) and `outcome` (the
/// stresses here). Then the last round's kernels.
#[allow(clippy::too_many_arguments)]
fn phases(url: &str, model: &Model, moved: &[Load], turned: &[Load], m: &Materials, opts: &Options, rounds: usize, class: Class) {
    let stream = Arc::new(KernelStream::open(&KernelNode::new(url)).unwrap_or_else(|e| die(&format!("the loopback hub's stream: {e:?}"))));
    let hub = Hub::of(vec![("loopback".to_string(), stream.clone())]);
    let keep = "phases";
    let posed_of = |loads: &[Load]| model.pose(&model.conditions(loads)).unwrap_or_else(|e| die(&e));
    let start = posed_of(moved);
    let fine = Level::new(start.grid.clone(), &start.held, m);
    let solve = |posed, guess: Option<&[V3]>| match Solve::start_on(&hub, &fine, posed, m, guess, opts, class, keep, &mut || false).and_then(|s| s.map_or(Ok(None), |s| s.wait(&mut |_| false))) {
        Ok(Some(o)) => o.solution.u,
        Ok(None) => die("a solve went stale"),
        Err(e) => die(&e),
    };
    let mut last = solve(start, None);
    println!("the turned load's finest step on the hub, from the moved load's answer, ms:");
    for round in 0..rounds {
        let Posed { grid, held, force, near, loose, surface } = posed_of(turned);
        let t = Instant::now();
        let (params, inputs) = program::job(&fine, &force, Some(&last), opts).unwrap_or_else(|e| die(&e));
        let job = ms(t);
        let t = Instant::now();
        let config = RemoteConfig { params, inputs, limits: program::limits(), class, keep: keep.to_string(), ..RemoteConfig::default() };
        let session = RemoteSession::open(&stream, &program::program(), config).unwrap_or_else(|e| die(&format!("{e:?}")));
        let open = ms(t);
        let t = Instant::now();
        // (ms since open, iterations) as each residual came
        let mut told: Vec<(f64, f64)> = Vec::new();
        let mut answered = None;
        let mut all = Updates::default();
        let solution = loop {
            let u = session.wait();
            let now = ms(t);
            if let Some(its) = u.value("cg").and_then(|cg| cg.get("its")).and_then(Value::as_number) {
                told.push((now, its));
            }
            if u.output("u0").is_some() {
                answered.get_or_insert(now);
            }
            all.merge(u);
            if let Some(done) = program::answer(&all) {
                break done.unwrap_or_else(|e| die(&e));
            }
        };
        let ended = ms(t);
        let t = Instant::now();
        let outcome = fab_force::outcome(grid, held, &near, solution, m, loose, surface.as_deref());
        let outcome_ms = ms(t);
        let (first, last_told) = (told.first().copied().unwrap_or((ended, 0.0)), told.last().copied().unwrap_or((ended, 0.0)));
        let second = told.get(1).copied().unwrap_or(last_told);
        let iteration = match last_told.1 > second.1 {
            true => (last_told.0 - second.0) / (last_told.1 - second.1),
            false => (second.0 - first.0) / second.1.max(1.0),
        };
        let setup = second.0 - first.0 - iteration * (second.1 - first.1);
        let its = outcome.solution.iterations;
        println!(
            "  round {round}: {:.0} = job {job:.1} + open {open:.1} + first {:.1} + setup {setup:.1} + {its} its at {iteration:.2} + back {:.1} (u at {:.1}) + outcome {outcome_ms:.1}",
            job + open + ended + outcome_ms,
            first.0,
            ended - last_told.0,
            answered.unwrap_or(ended) - last_told.0
        );
        if round + 1 == rounds {
            println!("    {}; {}", gpu_engine(&all), kernel_times(&all));
        }
        // the moved load again from this answer: the next round starts where this one did
        last = solve(posed_of(moved), Some(&outcome.solution.u));
    }
}

/// A hub session timed phase by phase, as `phases` reads one: `job` (laid out here), `open`
/// (coded, sent, started), `first` (to its first residual: the levels recalled, the inverse
/// diagonals, the load), `setup` (to the second, its iterations taken out: the GPU's layout
/// and copies), `run` (its iterations: a block's loads in turn) and `back` (from the last
/// residual to its end here: the answers published, sent and decoded), ms; its iterations and
/// loads.
#[derive(Clone, Copy, Default)]
struct Timed {
    job: f64,
    open: f64,
    first: f64,
    setup: f64,
    run: f64,
    back: f64,
    its: usize,
    loads: usize,
}

impl Timed {
    fn total(&self) -> f64 {
        self.job + self.open + self.first + self.setup + self.run + self.back
    }
}

/// The loads `fs` solved on `stream` in one session in the keep slot `keep`, timed ([`Timed`],
/// the setup from the first load's residuals); their answers and the updates.
fn timed_session(stream: &Arc<KernelStream>, fine: &Level, fs: &[Vec<V3>], opts: &Options, class: Class, keep: &str) -> Result<(Vec<Vec<V3>>, Timed, Updates), String> {
    let t = Instant::now();
    let loads: Vec<&[V3]> = fs.iter().map(Vec::as_slice).collect();
    let (params, inputs) = program::job_block(fine, &loads, None, opts, program::Solves::OneRun)?;
    let job = ms(t);
    let t = Instant::now();
    let config = RemoteConfig { params, inputs, limits: program::limits(), class, keep: keep.to_string(), ..RemoteConfig::default() };
    let session = RemoteSession::open(stream, &program::program(), config).map_err(|e| format!("{e:?}"))?;
    let open = ms(t);
    let t = Instant::now();
    // each residual told: (ms, iterations), the iterations from 0 again for each load
    let mut told: Vec<(f64, f64)> = Vec::new();
    let mut all = Updates::default();
    let solutions = loop {
        let u = session.wait();
        let now = ms(t);
        if let Some(its) = u.value("cg").and_then(|cg| cg.get("its")).and_then(Value::as_number) {
            told.push((now, its));
        }
        all.merge(u);
        if let Some(done) = program::answers(&all) {
            break done?;
        }
    };
    let ended = ms(t);
    let first = told.first().copied().unwrap_or((ended, 0.0));
    let last = told.last().copied().unwrap_or(first);
    // the first load's residuals: up to where the iterations start again
    let own = told.iter().skip(1).position(|w| w.1 == 0.0).map_or(told.len(), |k| k + 1);
    let mine = &told[..own];
    let mine_last = mine.last().copied().unwrap_or(first);
    let second = mine.get(1).copied().unwrap_or(mine_last);
    let iteration = match mine_last.1 > second.1 {
        true => (mine_last.0 - second.0) / (mine_last.1 - second.1),
        false => (second.0 - first.0) / second.1.max(1.0),
    };
    let setup = (second.0 - first.0 - iteration * (second.1 - first.1)).max(0.0);
    let its = solutions.iter().map(|s| s.iterations).sum();
    let run = (last.0 - first.0 - setup).max(0.0);
    let timed = Timed { job, open, first: first.0, setup, run, back: ended - last.0, its, loads: fs.len() };
    Ok((solutions.into_iter().map(|s| s.u).collect(), timed, all))
}

/// `--buckle N`: Fab's buckling stage on the bracket as the app runs it, on the loopback hub,
/// `N` times: the study's steps (its cell sizes, then the finest shifted half a cell), each
/// step's static solve kept in its slot, then LOBPCG on it with the hub's solves (to 1e-2, in
/// the step's slot) as its preconditioner, the first step cold and each next one from the
/// step before's modes. Per step its LOBPCG steps, its sessions and their mean time phase by
/// phase ([`Timed`]), and its wall time; then the last session's engine and kernels.
fn buckle(url: &str, sizes: &[f64], m: &Materials, opts: &Options, rounds: usize, class: Class) {
    let stream = Arc::new(KernelStream::open(&KernelNode::new(url)).unwrap_or_else(|e| die(&format!("the loopback hub's stream: {e:?}"))));
    let hub = Hub::of(vec![("loopback".to_string(), stream.clone())]);
    let (lo, hi) = BRACKET;
    let at = |v: V3, s: V3| [v[0] + s[0], v[1] + s[1], v[2] + s[2]];
    let finest = *sizes.last().unwrap();
    let mut field = |ps: &[V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = bench::bracket(*p));
    let models: Vec<(String, Model)> = sizes
        .iter()
        .map(|&h| (h, [0.0; 3]))
        .chain([(finest, study::shift(finest))])
        .enumerate()
        .map(|(k, (h, shift))| (format!("{}h {h:.3}", if k == sizes.len() { "shifted, " } else { "" }), Model::new(at(lo, shift), at(hi, shift), h, &mut field, m)))
        .collect();
    let loads = bench::bracket_loads();
    let pre_opts = Options { tolerance: 1e-2, ..*opts };
    println!("## Buckling on the hub, the bracket ({} steps), where a session's time goes", models.len());
    println!();
    println!("| round | step | static its | LOBPCG steps | sessions | loads a session | its a load | job | open | first | setup | run | back | a session ms | step s | factor |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut last_updates = Updates::default();
    for round in 0..rounds {
        let mut carried: Option<(Vec<Vec<V3>>, std::sync::Arc<Grid>)> = None;
        for (k, (name, model)) in models.iter().enumerate() {
            let posed = model.pose(&model.conditions(&loads)).unwrap_or_else(|e| die(&e));
            let fine = Level::new(posed.grid.clone(), &posed.held, m);
            let keep = format!("buckle {round} {k}");
            let outcome = match Solve::start_on(&hub, &fine, posed, m, None, opts, class, &keep, &mut || false).and_then(|s| s.map_or(Ok(None), |s| s.wait(&mut |_| false))) {
                Ok(Some(o)) => o,
                Ok(None) => die("the static solve went stale"),
                Err(e) => die(&e),
            };
            let start = match carried.take() {
                Some((modes, from)) => Start::Modes(modes.iter().map(|v| fab_force::results::carry(&from, v, &fine.grid)).collect()),
                None => Start::Rotations,
            };
            let mut sessions: Vec<Timed> = Vec::new();
            let t = Instant::now();
            let modes = {
                let mut pre = |block: &[Vec<V3>]| {
                    let (u, timed, all) = timed_session(&stream, &fine, block, &pre_opts, class, &keep)?;
                    sessions.push(timed);
                    last_updates = all;
                    Ok(Some(u))
                };
                buckle::lobpcg(&fine, &outcome.solution.u, start, &Loose::default(), &mut pre, &mut |_, _, _| false)
            };
            let wall = t.elapsed().as_secs_f64();
            let modes = match modes {
                Ok(Some(modes)) => modes,
                Ok(None) => die("LOBPCG went stale"),
                Err(e) => die(&e),
            };
            let count = sessions.len().max(1) as f64;
            let mean = |f: fn(&Timed) -> f64| sessions.iter().map(f).sum::<f64>() / count;
            println!(
                "| {round} | {name} | {} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.0} | {wall:.1} | {:.3} |",
                outcome.solution.iterations,
                modes.steps,
                sessions.len(),
                mean(|s| s.loads as f64),
                sessions.iter().map(|s| s.its).sum::<usize>() as f64 / sessions.iter().map(|s| s.loads).sum::<usize>().max(1) as f64,
                mean(|s| s.job),
                mean(|s| s.open),
                mean(|s| s.first),
                mean(|s| s.setup),
                mean(|s| s.run),
                mean(|s| s.back),
                mean(Timed::total),
                modes.factor(0),
            );
            carried = Some((modes.vectors, fine.grid.clone()));
        }
    }
    println!();
    println!("The last session: {}; {}", gpu_engine(&last_updates), kernel_times(&last_updates));
}

fn main() {
    let mut nodes: Vec<String> = Vec::new();
    let mut steps = 1;
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut threads = cores;
    let mut batch = 0usize;
    let mut rounds = 0usize;
    let mut buckling = 0usize;
    let mut repeat = 0usize;
    let mut class = Class::Interactive;
    let mut codec = RemoteConfig::default().codec;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| die(&format!("{a} needs a value")));
        match a.as_str() {
            "--node" => nodes.extend(val().split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from)),
            "--steps" => steps = val().parse().unwrap_or_else(|_| die("--steps N")),
            "--threads" => threads = val().parse().unwrap_or_else(|_| die("--threads N")),
            "--batch" => batch = val().parse().unwrap_or_else(|_| die("--batch N")),
            "--repeat" => repeat = val().parse().unwrap_or_else(|_| die("--repeat N")),
            "--scenarios" => rounds = val().parse().unwrap_or_else(|_| die("--scenarios N")),
            "--buckle" => buckling = val().parse().unwrap_or_else(|_| die("--buckle N")),
            "--class" => class = match val().as_str() {
                "interactive" => Class::Interactive,
                "background" => Class::Background,
                _ => die("--class interactive|background"),
            },
            "--codec" => codec = Some(Codec::from_name(&val()).unwrap_or_else(|| die("--codec raw|lz4|shuffle-lz4|delta-lz4|delta-deflate"))),
            _ => die(&format!("unknown argument {a}")),
        }
    }
    let url = loopback_hub();
    let here_hub = Arc::new(KernelStream::open(&KernelNode::new(&url)).unwrap_or_else(|e| die(&format!("the loopback hub's stream: {e:?}"))));
    let streams: Vec<(String, Arc<KernelStream>)> = [("loopback".to_string(), here_hub.clone())]
        .into_iter()
        .chain(nodes.iter().filter_map(|host| {
            makepad_network::fleet::id::mark_fleet_endpoint(host);
            match KernelStream::open(&KernelNode::new(&format!("https://{host}"))) {
                Ok(s) => Some((host.clone(), Arc::new(s))),
                Err(e) => {
                    println!("{host}: {e:?}");
                    None
                }
            }
        }))
        .collect();
    println!("fab-forceprogram: {} {}, {cores} cores here, the program on {threads}", std::env::consts::OS, std::env::consts::ARCH);
    let m = Materials::new(PLA, &Print::default(), true);
    let opts = Options::default();
    let (lo, hi) = BRACKET;
    let loads = bench::bracket_loads();
    let mut field = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = bench::bracket(*p);
        }
    };
    let sizes = study::steps(lo, hi, steps, 0.1);
    if buckling > 0 {
        buckle(&url, &sizes, &m, &opts, buckling, class);
        return;
    }
    if rounds > 0 {
        scenarios(&url, &sizes, &m, &opts, rounds, class);
        return;
    }
    let mut models = Vec::new();
    for &h in &sizes {
        let model = Model::new(lo, hi, h, &mut field, &m);
        let conditions = model.conditions(&loads);
        let (held, force) = fab_force::bc::nodes(&model.grid, &model.surface, &conditions);
        let cells = (0..model.grid.cells()).filter(|&c| model.grid.active(c)).count();
        let t = Instant::now();
        let hier = Hierarchy::new(model.grid.clone(), &held, &m, &opts);
        let setup = ms(t);
        // what the program is sent: the analysed level (it builds the coarse levels)
        let t = Instant::now();
        let fine = Level::new(model.grid.clone(), &held, &m);
        let analysed = ms(t);
        println!("bracket h {h:.3}: {cells} cells, {} levels, hierarchy {setup:.0} ms, analysed level {analysed:.0} ms, {}", hier.levels.len(), upload(&fine, &force, &opts));
        let t = Instant::now();
        let cpu = fab_force::solve::solve(&hier, &force, &opts);
        println!("  cpu solver          {:>4} its  {:>8.0} ms", cpu.iterations, ms(t));
        let mut here = None;
        for run in ["first", "again"] {
            match self::here(&fine, &force, &opts, threads) {
                Ok((wall, s, kernels)) => {
                    println!("  program here {run:<6} {:>4} its  {:>8.0} ms  |du| {:.1e} of max", s.iterations, wall, apart(&s.u, &cpu.u));
                    println!("    {kernels}");
                    here = Some(s);
                }
                Err(e) => println!("  program here {run}: {e}"),
            }
        }
        for (host, stream) in &streams {
            for run in ["first", "again"] {
                match on_node(stream, &fine, &force, &opts, class, codec) {
                    Ok((job, open, solve, s, kernels)) => {
                        let bits = here.as_ref().map_or("no answer here to compare".to_string(), |here| match differing(&s.u, &here.u) {
                            0 => "the same bits as here".to_string(),
                            n => format!("{n} values differ from here"),
                        });
                        println!("  program {host} {run:<6} {:>4} its  {:>8.0} ms (job {job:.0} ms, open {open:.0} ms, solve {solve:.0} ms)  |du| {:.1e} of max, {bits}", s.iterations, job + open + solve, apart(&s.u, &cpu.u));
                        println!("    {kernels}");
                    }
                    Err(e) => println!("  program {host} {run}: {e}"),
                }
            }
        }
        models.push((model, conditions));
    }
    if let Some((model, conditions)) = models.last().filter(|_| repeat > 0) {
        let (held, force) = fab_force::bc::nodes(&model.grid, &model.surface, conditions);
        for k in 0..repeat {
            let t = Instant::now();
            let hier = Hierarchy::new(model.grid.clone(), &held, &m, &opts);
            let setup = ms(t);
            let t = Instant::now();
            let cpu = fab_force::solve::solve(&hier, &force, &opts);
            println!("repeat {k}: h {:.3} hierarchy {setup:.0} ms, cpu solver {} its {:.0} ms", model.h, cpu.iterations, ms(t));
        }
    }
    if batch > 0 {
        let hubs = [("the loopback hub", Hub::of(streams[..1].to_vec())), ("it and the nodes", Hub::of(streams.clone()))];
        let densities: Vec<f64> = (1..=batch).map(|k| k as f64 / (batch + 1) as f64).collect();
        println!("batch: {batch} densities ({}) at {} cell sizes", densities.iter().map(|d| format!("{:.0} %", 100.0 * d)).collect::<Vec<_>>().join(", "), models.len());
        let mut first: Option<Vec<Solution>> = None;
        for (name, hub) in &hubs {
            let jobs: Vec<Job> = densities
                .iter()
                .flat_map(|&d| {
                    let m = Materials::new(PLA, &Print { density: d, ..Print::default() }, true);
                    models.iter().map(move |(model, conditions)| Job { posed: model.with_materials(&m).pose(conditions).unwrap_or_else(|e| die(&e)), materials: m.clone(), guess: None }).collect::<Vec<_>>()
                })
                .collect();
            let t = Instant::now();
            let out: Result<Vec<_>, String> = hub::analyse(hub, jobs, &opts, &|| false, Class::Background).into_iter().collect();
            match out {
                Ok(out) => {
                    let solutions: Vec<_> = out.into_iter().map(|o| o.solution).collect();
                    let off = first.as_ref().map_or(String::new(), |f| {
                        let worst = f.iter().zip(&solutions).map(|(a, b)| apart(&b.u, &a.u)).fold(0.0, f64::max);
                        format!(", at most {worst:.1e} of the largest movement from the first")
                    });
                    println!("  {name} ({} nodes): {:.0} ms{off}", hub.nodes().len(), ms(t));
                    first.get_or_insert(solutions);
                }
                Err(e) => println!("  {name}: {e}"),
            }
        }
    }
}

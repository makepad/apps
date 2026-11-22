//! Hand-routing latency on each board's own routes, drawn again with their copper taken off:
//! a clicked route through a via (script), the longest track in one drag (track), and that
//! track's corridor dragged back and forth to a long trail (scribble). Every head has a legal way
//! to its target, so the timings are of heads that reach it, not of heads stopped by a part.
//! The drag case grabs the straight segment with the most copper round it and drags it across
//! that copper, 2 mm to each side and back: it pushes the tracks in its way, or walks round them
//! when one cannot be pushed (counted as pushed and walked samples); its blocked samples are drags
//! that cannot be laid. Its waves are stepped and checked as the part case's.
//! The shove case draws a pad-to-pad track again in Shove mode along its own path with one
//! segment's middle pressed 0.6 mm or 1 mm into a neighbour's track, which the head pushes (pushed
//! samples) or, when it cannot, walks round (walked); its waves are stepped and checked as the
//! drag cases', and it runs with a job and at the 8 ms cadence too. The track-shove case is the
//! track case drawn in Shove mode (the board's default), checked and timed the same way.
//! The part case moves the part with the most attached tracks round a 1 mm circle and back; its
//! blocked samples are answers with a collision (the part on copper), and it counts the attached
//! tracks cut back (opened, never left in a short: no answer has two of them colliding). A sample
//! whose stretch answer is not the last runs its shove wave to the end (`head::shove_step`, what
//! the worker steps between its other work), timed apart from tier 1, as is each step. Every Full
//! answer is checked: no collision between two tracks; DRC finds nothing new but a moved part
//! against copper that stayed; each pushed track met the moved part in tier 1; and the same sample
//! answered again from a fresh state gives the same ops. The part moves case does the same for
//! single moves (two of a resistor, a two-pad part into a run of tracks). The slowest steps are
//! printed with where their time went. The part cadence case sends the part samples at a fixed
//! cadence whatever the answers (8 ms and 16.7 ms, as a pointer does), so a sample can arrive
//! while the previous one's wave runs: tier 1 from the sample to its answer, all and those sent
//! during a wave.
//! The finish case draws the track case's route to half its length and presses F: the time to the
//! Finish's answer (`head::finish`, what the worker runs on the pool) on a base with its pours
//! filled, over five runs, and apart from it the job basis the first Finish on a base builds.
//! Every Full answer's change is also checked as it lands: the board applies its ops as one
//! batch (`Board::apply_batch`), and every track an op adds or replaces is on the board exactly
//! as the op wrote it (a free end it writes where a reader attaches one is attached as the reader
//! attaches it), with every anchored track end on its anchor. On a board read from text
//! (ledctl, drum16, a --kicad board), the same ops spliced into the text and read back give the
//! model's copper, anchors included (`splice_verified`).
//! --kicad PATH reads a KiCad board as data (never stored), writes it to Splash and evaluates it,
//! as the app opens one, and runs only the circle case: each part of --parts R1,R2 (the part
//! with most attached tracks when empty) moved round a circle of --radius-um N (1000) in
//! --samples steps, each Full answer through the gates above. A large board's BGA at 1 mm in 20
//! samples is the regression of mutually anchored tracks re-laid in one answer: applied one op at
//! a time, five of its answers left a track off what its op wrote, three of them with new
//! clearance or short findings. Two gates have open findings on such a board on per-op apply
//! too, so the circle counts them instead of stopping: the push cause (a pushed track that met
//! no moved part) and the round trip (the text reads a track end the model anchors on another
//! track as free). It prints their counts and first findings; every other gate stops the run.
//! Options are --board ledctl|generated|drum16|all (all), --segments N (10000), --seed N (1),
//! --samples N (400, at least 4), --scribble-points N (2000 including the starting pad).
//! Compare the head timings with the worker budget of 2000 us median and 4000 us p95.
//! The with-job wall time includes delivery in both directions. Query counts come from
//! the direct run of the same samples. Timings are printed and assertions check correctness.

use fab_elec_board::zone::fill_all;
use fab_elec_board::{connect, drc, resolve_rules, Anchor, Board, BoardDelta, BoardOp, IndexChange, Item, Origin, PieceIndex, Track};
use fab_elec_core::{mm, BBox, BoardId, ItemId, LayerId, NetId, Nm, OnTrack, Path, Seg, P};
use fab_elec_design::DesignOp;
use fab_elec_lang::{evaluate, splice_verified, std_libs, Evaluated, Libs, Sources, NO_CANCEL};
use fab_elec_route::check::Basis;
use fab_elec_route::head::{answer_counted, finish, shove, shove_step, Answer, GestureState, Spent, WaveStats};
use fab_elec_route::{
    route_channel, Base, BlockCause, Effort, Engine, Gesture, GestureId, GestureKind,
    GestureSample, HeadAnswer, JobId, Leg, Method, Mode, Progress, ProposalId, ProposalSource, RBase, Reason, WidthRule, Xform,
    RouteEvent, RouteHandle, RouteJob, RouteProposal, RouteRequest, RouteScope, RouteWorker, Spawn,
    new_pieces, verify, World,
};
use fab_elec_view::board::gen::{generated_board, GenSpec};
use makepad_widgets::makepad_platform::thread::PriorityStatus;
use makepad_widgets::makepad_platform::{Cx, CxThreadPriority};
use std::ops::ControlFlow;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Flags {
    segments: usize,
    seed: u64,
    samples: usize,
    scribble_points: usize,
    board: String,
    kicad: Option<String>,
    parts: Vec<String>,
    radius: Nm,
}

fn flags() -> Flags {
    let mut flags = Flags {
        segments: 10_000, seed: 1, samples: 400, scribble_points: 2000, board: "all".into(), kicad: None, parts: Vec::new(), radius: mm(1.0),
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_else(|| panic!("missing value for {flag}"));
        match flag.as_str() {
            "--segments" => flags.segments = value.parse().expect("--segments needs an unsigned integer"),
            "--seed" => flags.seed = value.parse().expect("--seed needs an unsigned integer"),
            "--samples" => flags.samples = value.parse().expect("--samples needs an unsigned integer"),
            "--scribble-points" => flags.scribble_points = value.parse().expect("--scribble-points needs an unsigned integer"),
            "--board" => flags.board = value,
            "--kicad" => flags.kicad = Some(value),
            "--parts" => flags.parts = value.split(',').filter(|r| !r.is_empty()).map(String::from).collect(),
            "--radius-um" => flags.radius = value.parse::<Nm>().expect("--radius-um needs an integer") * 1000,
            _ => panic!("unknown flag {flag}"),
        }
    }
    assert!(flags.samples >= 4, "--samples must allow two clicks and a sample after the via click");
    assert!(flags.scribble_points >= 2, "--scribble-points needs a start and at least one more point");
    assert!(matches!(flags.board.as_str(), "ledctl" | "generated" | "drum16" | "all"), "invalid --board");
    flags
}

fn us(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e6
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    let value = f();
    std::hint::black_box(&value);
    (value, us(start))
}

struct Stats {
    median: f64,
    p95: f64,
    worst: f64,
    mean: f64,
}

fn stats(values: &[f64]) -> Stats {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    assert!(n > 0, "statistics need samples");
    Stats {
        median: (sorted[(n - 1) / 2] + sorted[n / 2]) / 2.0,
        // ceil(0.95 * n) - 1, evaluated exactly without floating-point rounding.
        p95: sorted[n - n / 20 - 1],
        worst: sorted[n - 1],
        mean: values.iter().sum::<f64>() / n as f64,
    }
}

struct Recorded {
    sample: GestureSample,
    queries: usize,
}

#[derive(Default)]
struct Timings {
    wall: Vec<f64>,
    /// The sample seq of each `wall` entry, in order.
    seqs: Vec<u64>,
    solve: Vec<f64>,
    /// From the sample to its Full answer, including when it replaced an untaken Stretch.
    full: Vec<f64>,
    /// Samples whose own Full replaced their Stretch in the outbox before the bench took it.
    /// They have a Full time and no tier-1 time.
    superseded: usize,
    blocked: usize,
    vias: u32,
}

impl Timings {
    fn record<'a>(&mut self, wall: f64, proposal: &'a RouteProposal, sample: &GestureSample, base: &Base) -> &'a HeadAnswer {
        let head = check_answer(proposal, sample, base);
        self.wall.push(wall);
        self.seqs.push(sample.seq);
        self.solve.push(f64::from(proposal.solve_us));
        self.blocked += usize::from(head.blocked.is_some());
        self.vias = proposal.metrics.vias;
        head
    }

    /// A segment drag's answers: tier 1's times, and the head of its last answer (`full`, which
    /// is tier 1's when no wave follows), checked as a route answer's.
    fn record_drag<'a>(&mut self, wall: f64, stretched: &RouteProposal, full: &'a RouteProposal, sample: &GestureSample, base: &Base) -> &'a HeadAnswer {
        let head = check_answer(full, sample, base);
        self.wall.push(wall);
        self.seqs.push(sample.seq);
        self.solve.push(f64::from(stretched.solve_us));
        self.blocked += usize::from(head.blocked.is_some());
        head
    }

    /// A part drag's answer: it has no head, and a collision counts as blocked.
    fn record_part(&mut self, wall: f64, proposal: &RouteProposal, sample: &GestureSample, base: &Base) {
        check_part_answer(proposal, sample, base);
        self.wall.push(wall);
        self.seqs.push(sample.seq);
        self.solve.push(f64::from(proposal.solve_us));
        self.blocked += usize::from(!proposal.verified);
    }

    fn superseded(&mut self, full_us: f64, full: &RouteProposal, sample: &GestureSample, base: &Base) {
        if full.head.is_some() {
            check_answer(full, sample, base);
        } else {
            check_part_answer(full, sample, base);
        }
        self.full.push(full_us);
        self.superseded += 1;
    }

    fn report(&self, label: &str, board: &str, case: &str, samples: &[Recorded], extra: &str) {
        let mut queries = None;
        for (suffix, values) in [("", &self.wall), ("-solve", &self.solve)] {
            assert_eq!(values.len() + self.superseded, samples.len(), "each submitted sample has one tier-1 timing or a superseded Stretch");
            if values.is_empty() {
                println!("{label}{suffix} board={board} case={case} samples=0 superseded={}: no tier-1 answers", self.superseded);
                continue;
            }
            let queries = queries.get_or_insert_with(|| stats(&samples.iter().map(|r| r.queries as f64).collect::<Vec<_>>()));
            let s = stats(values);
            let peak = values.iter().position(|&us| us == s.worst).expect("a slowest sample");
            let seq = self.seqs[peak];
            let at = &samples.iter().find(|r| r.sample.seq == seq).expect("the slowest timing names a submitted sample").sample;
            println!("{label}{suffix} board={board} case={case} samples={} median_us={:.1} p95_us={:.1} max_us={:.1} max_seq={} max_trail_points={} max_legs={} mean_us={:.1} reached={} blocked={} vias={} direct_queries_median={:.1} direct_queries_max={:.0} superseded={}{extra}",
                values.len(), s.median, s.p95, s.worst, at.seq, at.gesture.trail.len(), at.gesture.legs.len(),
                s.mean, values.len() - self.blocked, self.blocked, self.vias, queries.median, queries.worst, self.superseded);
        }
        if !self.full.is_empty() {
            let s = stats(&self.full);
            println!("{label}-full board={board} case={case} waves={} median_us={:.1} p95_us={:.1} p99_us={:.1} max_us={:.1}",
                self.full.len(), s.median, s.p95, p99(&self.full), s.worst);
        }
    }
}

fn p99(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    sorted[n - n / 100 - 1]
}

/// The shove waves of a part case: each wave's time to its Full answer, each step's, and the
/// counts of what the waves did.
#[derive(Default)]
struct Waves {
    wave: Vec<f64>,
    steps: Vec<f64>,
    longest: Vec<f64>,
    worst_seq: u64,
    stats: WaveStats,
    tier1_cut: usize,
    full_cut: usize,
    colliding: usize,
    /// The slowest steps: time, sample, and what the step did.
    slow: Vec<(f64, u64, WaveStats)>,
    /// Each sample's tracks cut in its last answer, in sample order (to compare runs sample by
    /// sample).
    cuts: Vec<usize>,
}

/// `a - b`, field by field (what happened between two snapshots of one wave).
fn minus(a: &WaveStats, b: &WaveStats) -> WaveStats {
    let (x, y) = (&a.spent, &b.spent);
    WaveStats {
        members: a.members.saturating_sub(b.members), steps: a.steps.saturating_sub(b.steps), embeds: a.embeds.saturating_sub(b.embeds),
        repairs: a.repairs.saturating_sub(b.repairs), pushed: a.pushed.saturating_sub(b.pushed), kept: a.kept.saturating_sub(b.kept),
        relaid: a.relaid.saturating_sub(b.relaid), cut: a.cut.saturating_sub(b.cut), spikes: a.spikes.saturating_sub(b.spikes),
        joined: a.joined.saturating_sub(b.joined), vias: a.vias.saturating_sub(b.vias), vias_kept: a.vias_kept.saturating_sub(b.vias_kept),
        spent: Spent {
            lay_ns: x.lay_ns.saturating_sub(y.lay_ns), embed_ns: x.embed_ns.saturating_sub(y.embed_ns),
            verify_ns: x.verify_ns.saturating_sub(y.verify_ns), cut_ns: x.cut_ns.saturating_sub(y.cut_ns),
            rebuild_ns: x.rebuild_ns.saturating_sub(y.rebuild_ns), hulls: x.hulls.saturating_sub(y.hulls),
            hull_ns: x.hull_ns.saturating_sub(y.hull_ns), hulls_added: x.hulls_added.saturating_sub(y.hulls_added),
        },
    }
}

fn spent_words(s: &Spent) -> String {
    let ms = |ns: u64| ns as f64 / 1e6;
    format!("lay_ms={:.2} embed_ms={:.2} verify_ms={:.2} cut_ms={:.2} rebuild_ms={:.2} hulls={} hulls_added={} hull_ms={:.2}",
        ms(s.lay_ns), ms(s.embed_ns), ms(s.verify_ns), ms(s.cut_ns), ms(s.rebuild_ns), s.hulls, s.hulls_added, ms(s.hull_ns))
}

impl Waves {
    /// One step's time and what it did; the ten slowest are kept.
    fn step(&mut self, us: f64, seq: u64, did: WaveStats) {
        self.steps.push(us);
        let at = self.slow.partition_point(|(t, ..)| *t >= us);
        if at < 10 {
            self.slow.insert(at, (us, seq, did));
            self.slow.truncate(10);
        }
    }

    fn add(&mut self, stats: &WaveStats) {
        let s = &mut self.stats;
        s.members += stats.members;
        s.steps += stats.steps;
        s.embeds += stats.embeds;
        s.repairs += stats.repairs;
        s.pushed += stats.pushed;
        s.kept += stats.kept;
        s.relaid += stats.relaid;
        s.cut += stats.cut;
        s.spikes += stats.spikes;
        s.joined += stats.joined;
        s.vias += stats.vias;
        s.vias_kept += stats.vias_kept;
        let (t, d) = (&mut s.spent, &stats.spent);
        t.lay_ns += d.lay_ns;
        t.embed_ns += d.embed_ns;
        t.verify_ns += d.verify_ns;
        t.cut_ns += d.cut_ns;
        t.rebuild_ns += d.rebuild_ns;
        t.hulls += d.hulls;
        t.hull_ns += d.hull_ns;
        t.hulls_added += d.hulls_added;
    }

    fn report(&self, board: &str, case: &str, samples: usize) {
        if self.wave.is_empty() {
            println!("gesture-wave board={board} case={case} waves=0");
            return;
        }
        let (wave, step, longest) = (stats(&self.wave), stats(&self.steps), stats(&self.longest));
        let s = &self.stats;
        println!("gesture-wave board={board} case={case} samples={samples} waves={} median_us={:.1} p95_us={:.1} p99_us={:.1} max_us={:.1} max_seq={} step_median_us={:.1} step_p95_us={:.1} step_max_us={:.1} longest_step_p95_us={:.1} members={} steps={} embeds={} repairs={} pushed={} kept={} relaid={} cut={} spikes={} joined={} vias={} vias_kept={} tier1_cut={} full_cut={} full_colliding={}",
            self.wave.len(), wave.median, wave.p95, p99(&self.wave), wave.worst, self.worst_seq, step.median, step.p95, step.worst, longest.p95,
            s.members, s.steps, s.embeds, s.repairs, s.pushed, s.kept, s.relaid, s.cut, s.spikes, s.joined, s.vias, s.vias_kept, self.tier1_cut, self.full_cut, self.colliding);
        println!("gesture-wave-spent board={board} case={case} {}", spent_words(&s.spent));
        println!("gesture-wave-cuts board={board} case={case} cuts={}", self.cuts.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(","));
        for (us, seq, did) in &self.slow {
            println!("gesture-slow-step board={board} case={case} seq={seq} step_us={us:.1} {} embeds={} spikes={} repairs={} cut={} kept={} joined={} laid={}",
                spent_words(&did.spent), did.embeds, did.spikes, did.repairs, did.cut, did.kept, did.joined, did.pushed + did.relaid);
        }
    }
}

/// The DRC of a base with its pours filled, to check what an answer's ops add: `check` fills the
/// pours of the answer's board, as the worker's base holds them, while the heads' own base has none.
struct Checker {
    fills: Vec<fab_elec_board::ZoneFill>,
    conn: fab_elec_board::Connectivity,
    index: PieceIndex,
    violations: Vec<fab_elec_circuit::Violation>,
    /// The base's text, when it is read from one: the round trip of each Full answer.
    text: Option<Arc<Text>>,
    /// The push cause and round trip findings, counted (with the first) instead of stopping the
    /// run, when set: the --kicad circle.
    open: Option<Mutex<Vec<(&'static str, usize, String)>>>,
}

impl Checker {
    fn new(base: &Base, text: Option<&Arc<Text>>) -> Checker {
        let fills = fill_all(&base.board, &base.rules);
        let conn = connect(&base.board, &fills);
        Checker {
            index: PieceIndex::build(&base.board, &fills),
            violations: drc(&base.board, &conn, &base.rules, &fills, None).violations,
            fills,
            conn,
            text: text.cloned(),
            open: None,
        }
    }

    /// A finding `found` of `gate`: counted when the open gates are counted, else it stops the run.
    fn finding(&self, gate: &'static str, found: Option<String>) {
        let Some(found) = found else { return };
        let Some(open) = &self.open else { panic!("{found}") };
        let mut open = open.lock().expect("the open findings");
        match open.iter_mut().find(|(g, ..)| *g == gate) {
            Some((_, count, _)) => *count += 1,
            None => open.push((gate, 1, found)),
        }
    }

    /// The shove gates on a Full answer `full` of `sample`, whose stretch answer was `stretched`:
    /// no collision between two tracks; every new or worse DRC finding on copper (`check::new_or_worse`) is
    /// a moved part's against copper the answer did not change; each pushed track (replaced, not on a moved part) met a
    /// moved part in the stretch answer, or its base copper meets the new copper of another track
    /// the answer changed, and so does each pushed via but a moved part's own fanout via that rides
    /// with it by its translation; and a fresh state answers the sample with the same ops.
    fn gates(&self, what: &str, base: &Base, world: &RBase, sample: &GestureSample, stretched: &RouteProposal, full: &RouteProposal) {
        let board = &base.board;
        let unit = board.unit;
        // What the person moves: the parts, or the dragged track.
        let mut moved: Vec<ItemId> = sample.gesture.moved.iter().map(|(id, _)| *id).collect();
        if let GestureKind::DragSeg { track, .. } = sample.gesture.kind {
            moved.push(track);
        }
        // A route: its new copper.
        if sample.gesture.kind == GestureKind::Route {
            moved.extend(full.ops.iter().filter_map(|op| match op { BoardOp::Add(item) => Some(item.id()), _ => None }));
        }
        let comps: Vec<_> = board.placed.iter().filter(|p| moved.contains(&p.id)).map(|p| p.comp).collect();
        let is_track = |sel: &fab_elec_core::Sel| matches!(sel, fab_elec_core::Sel::Item(_, id) if board.tracks.iter().any(|t| t.id == *id));
        assert!(!full.collisions.iter().any(|c| is_track(&c.a) && is_track(&c.b)), "{what}: no Full answer has two tracks colliding: {:?}", full.collisions);
        let on_moved = |track: &Track| track.ends.iter().any(|end| end.item().is_some_and(|item| moved.contains(&item)));
        // The vias the answer pushes (moves the person did not make): their tracks follow them.
        let pushed_vias: Vec<(ItemId, P)> = full.ops.iter().filter_map(|op| match op {
            BoardOp::Move { item, by } if !moved.contains(item) => Some((*item, *by)),
            _ => None,
        }).collect();
        assert!(pushed_vias.iter().all(|(id, _)| board.vias.iter().any(|v| v.id == *id)), "{what}: the answer moves no part the person did not move");
        let on_pushed = |track: &Track| track.ends.iter().any(|end| matches!(*end, Anchor::Via(v) if pushed_vias.iter().any(|(id, _)| *id == v)));
        let changed: Vec<ItemId> = full.ops.iter().filter_map(|op| match op {
            BoardOp::Replace(Item::Track(t)) | BoardOp::Add(Item::Track(t)) => Some(t.id),
            BoardOp::Remove(id) => Some(*id),
            BoardOp::Move { item, .. } if !moved.contains(item) => Some(*item),
            _ => None,
        }).collect();
        let reshaped: Vec<&Track> = full.ops.iter().filter_map(|op| match op {
            BoardOp::Replace(Item::Track(t)) if board.tracks.iter().any(|b| b.id == t.id && b.path != t.path) => Some(t),
            _ => None,
        }).collect();
        let mut laid = World::new(world);
        for t in &reshaped {
            laid.remove_item(t.id);
            for piece in new_pieces(board, &Item::Track((*t).clone())) { laid.add(piece); }
        }
        for &(id, by) in &pushed_vias {
            laid.remove_item(id);
            for &rid in world.pieces(id) { laid.add_moved(rid, by); }
        }
        let met_mover = |id: ItemId| stretched.collisions.iter().any(|c| [&c.a, &c.b].contains(&&fab_elec_core::Sel::Item(unit, id)));
        // What `pieces` (an item's base copper) meets of the answer's new copper but its own.
        let met_new = |own: ItemId, pieces: &[fab_elec_route::world::RItem]| verify(&laid, &base.rules, board, pieces).iter()
            .any(|c| matches!(c.b, fab_elec_core::Sel::Item(_, id) if id != own && (reshaped.iter().any(|r| r.id == id) || pushed_vias.iter().any(|(v, _)| *v == id))));
        for t in &reshaped {
            let old = board.tracks.iter().find(|b| b.id == t.id).expect("a reshaped track is on the board");
            if on_moved(old) || on_pushed(old) || moved.contains(&old.id) { continue; }
            let met = met_mover(t.id) || met_new(t.id, &new_pieces(board, &Item::Track(old.clone())));
            self.finding("push cause", (!met).then(|| format!(
                "{what}: pushed track {:?} met a moved part in the stretch answer, or another track or via the answer changed: nothing else moves", t.id)));
        }
        // A moved part's own fanout via moved by the part's own translation rides with it (E6 3.2).
        let rides = |id: ItemId, by: P| board.tracks.iter().any(|t| t.ends.contains(&Anchor::Via(id))
            && t.ends.iter().any(|end| end.item().is_some_and(|item| moved.contains(&item) && full.ops.contains(&BoardOp::Move { item, by }))));
        for &(id, by) in &pushed_vias {
            if rides(id, by) {
                continue;
            }
            let base_pieces: Vec<_> = world.pieces(id).iter().filter_map(|&rid| world.get(rid)).cloned().collect();
            let met = met_mover(id) || met_new(id, &base_pieces);
            self.finding("push cause", (!met).then(|| format!(
                "{what}: pushed via {id:?} met a moved part in the stretch answer, or a track or via the answer changed: nothing else moves")));
        }
        let mover = |sel: &fab_elec_core::Sel| match sel {
            fab_elec_core::Sel::Comp(c) | fab_elec_core::Sel::Feature(fab_elec_core::FeatureRef::Pad(c, _) | fab_elec_core::FeatureRef::Part(c)
                | fab_elec_core::FeatureRef::Body(c) | fab_elec_core::FeatureRef::Port(c, _)) => comps.contains(&c.comp),
            fab_elec_core::Sel::Item(_, id) => moved.contains(id),
            _ => false,
        };
        let since = IndexChange::all();
        let state = fab_elec_route::check::State {
            rules: &base.rules, fills: &self.fills, index: &self.index, conn: &self.conn, violations: &self.violations, since: &since,
        };
        let mut after = (**board).clone();
        let outcome = fab_elec_route::check::check(&mut after, &full.ops, &state).expect("the Full answer's ops apply to its base");
        for v in outcome.new.iter().map(|&i| &outcome.violations[i]) {
            use fab_elec_circuit::CheckKind;
            if matches!(v.kind, CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::Unconnected) { continue; }
            let fixed = |sel: &fab_elec_core::Sel| !mover(sel) && !matches!(sel, fab_elec_core::Sel::Item(_, id) if changed.contains(id));
            assert!(v.items.iter().any(mover) && v.items.iter().all(|sel| mover(sel) || fixed(sel)),
                "{what}: DRC finds nothing new or worse but a moved part against copper that stayed: {v:?}");
        }
        landed_as_written(what, &after, &full.ops);
        if let Some(text) = &self.text {
            self.finding("round trip", text.round_trip(what, &after, &full.ops).err());
        }
        let mut fresh = GestureState::default();
        let again = match answer_counted(base, world, sample, &mut fresh, ProposalId(0)).0 {
            Answer::Proposal(p) if p.last => p,
            Answer::Proposal(_) => shove(base, world, &mut fresh, ProposalId(0)).expect("a wave follows").0,
            other => panic!("{what}: the sample is answered again: {other:?}"),
        };
        assert_eq!(again.ops, full.ops, "{what}: the same sample answered again gives the same ops");
    }
}

/// One part or segment drag sample's answers: the stretch answer, timed, then its wave (if any)
/// stepped to the end, each step timed, and the gates checked on the last answer.
fn wave_sample(what: &str, base: &Base, world: &RBase, checker: &Checker, sample: &GestureSample, state: &mut GestureState, waves: &mut Waves)
    -> (RouteProposal, RouteProposal, usize, f64) {
    let ((answer, queries), wall) = timed(|| answer_counted(base, world, sample, state, ProposalId(0)));
    let stretched = match answer {
        Answer::Proposal(p) => p,
        Answer::Refused(cause, text) => panic!("{what} refused ({cause:?}): {text}"),
        Answer::Covered => panic!("{what} unexpectedly covered"),
    };
    if stretched.last {
        waves.cuts.push(stretched.open.len());
        checker.gates(what, base, world, sample, &stretched, &stretched);
        return (stretched.clone(), stretched, queries, wall);
    }
    let (mut total, mut longest) = (0.0, 0.0f64);
    let (full, stats) = loop {
        let before = state.wave_stats().expect("a wave to step");
        let (full, step) = timed(|| shove_step(base, world, state, || ProposalId(1)));
        total += step;
        longest = longest.max(step);
        // The Full answer's stats also count what was laid; a step's own are its snapshot's.
        let after = match &full { Some((_, stats)) => *stats, None => state.wave_stats().expect("a wave goes on until its Full answer") };
        let mut did = minus(&after, &before);
        if full.is_some() {
            (did.pushed, did.relaid) = (0, 0);
        }
        waves.step(step, sample.seq, did);
        if let Some(full) = full { break full; }
    };
    if waves.wave.iter().all(|&w| w < total) { waves.worst_seq = sample.seq; }
    waves.wave.push(total);
    waves.longest.push(longest);
    waves.add(&stats);
    // Sharp corners the answer kept are findings, not cuts.
    let cut = |p: &RouteProposal| p.open.iter().filter(|(_, why)| !matches!(why, Reason::SharpCorner { .. })).count();
    waves.tier1_cut += cut(&stretched);
    waves.full_cut += cut(&full);
    waves.cuts.push(cut(&full));
    waves.colliding += usize::from(!full.verified);
    checker.gates(what, base, world, sample, &stretched, &full);
    (stretched, full, queries, wall)
}

/// Answers to samples sent on a cadence: when each was sent and whether a wave ran then, and
/// tier 1's times.
struct Cadence {
    sent: Vec<Option<(Instant, bool)>>,
    /// Per stretch answer: from the sample to its answer, the answer's own compute (`solve_us`),
    /// and whether a wave ran when the sample was sent.
    answers: Vec<(f64, f64, bool)>,
    fulls: usize,
    newest: u64,
    /// The sample whose stretch answer said a Full answer follows, until it comes or a newer
    /// sample's answer drops its wave.
    pending: Option<u64>,
}

impl Cadence {
    fn event(&mut self, event: RouteEvent) {
        let p = match event {
            RouteEvent::Proposal(p) => p,
            RouteEvent::Status(_) => return,
            RouteEvent::Refused(r) => panic!("worker refused gesture {:?} seq={} ({:?}): {}", r.id, r.seq, r.cause, r.text),
            RouteEvent::Explain(_) => panic!("no explanation was requested"),
        };
        let ProposalSource::Gesture { seq, tier, .. } = p.source else { panic!("no route job runs") };
        if tier == fab_elec_route::Tier::Full {
            self.fulls += 1;
            // At equal seq the outbox replaces an untaken Stretch with its Full. The sample is
            // answered, that Stretch is superseded, and any older pending wave was dropped.
            self.newest = self.newest.max(seq);
            if self.pending.is_some_and(|p| p <= seq) { self.pending = None; }
            return;
        }
        assert!(seq > self.newest, "stretch answers come in sample order");
        let (at, waving) = self.sent[seq as usize].expect("an answer names a sent sample");
        let us = at.elapsed().as_secs_f64() * 1e6;
        self.answers.push((us, f64::from(p.solve_us), waving));
        self.newest = seq;
        self.pending = (!p.last).then_some(seq);
    }
}

/// The part samples sent to the worker every `cadence_us` whatever the answers, as a pointer
/// sends them: the waves the samples leave run on the worker's thread between them (a step in
/// progress finishes before the next sample is read, which then drops the wave). Tier 1 from each
/// sample to its stretch answer: all, and those sent while a wave ran. A sample a newer one
/// replaced before the worker read it has no answer: superseded. A sample whose own Full replaced
/// its Stretch before this thread took it also counts as superseded. The first sample, cold, and
/// its wave go before the cadence starts. The worker's thread runs at the app's priority for it
/// (normal), or at `interactive` (the user-interactive class) to compare.
fn with_waves(name: &str, case: &str, base: &Base, samples: &[Recorded], cadence_us: u64, interactive: bool) {
    let (pool, threads, _starts) = pool();
    let (wake, wakes) = mpsc::sync_channel(1);
    let (handle, inbox) = route_channel(move || { let _ = wake.try_send(()); });
    let worker = RouteWorker::new(inbox, pool.clone());
    let worker = thread::spawn(move || {
        if interactive { priority(CxThreadPriority::UserInteractive); }
        worker.run()
    });
    let qos = if interactive { "interactive" } else { "normal" };
    let result = catch_unwind(AssertUnwindSafe(|| {
        handle.request(RouteRequest::Base(base.clone())).expect("base fits the command queue");
        let last = samples.last().expect("samples").sample.seq;
        let mut c = Cadence { sent: vec![None; last as usize + 1], answers: Vec::new(), fulls: 0, newest: 0, pending: None };
        let first = &samples[0].sample;
        c.sent[first.seq as usize] = Some((Instant::now(), false));
        handle.sample(first.clone());
        while c.newest < first.seq || c.pending.is_some() { c.event(next_event(&handle, &wakes)); }
        c.answers.clear();
        c.fulls = 0;
        let start = Instant::now();
        for (i, recorded) in samples[1..].iter().enumerate() {
            let due = start + Duration::from_micros(cadence_us * i as u64);
            loop {
                while let Some(event) = handle.try_recv() { c.event(event); }
                let now = Instant::now();
                if now >= due { break; }
                let _ = wakes.recv_timeout(due - now);
            }
            c.sent[recorded.sample.seq as usize] = Some((Instant::now(), c.pending.is_some()));
            handle.sample(recorded.sample.clone());
        }
        while c.newest < last || c.pending.is_some() { c.event(next_event(&handle, &wakes)); }
        c
    }));
    drop(handle);
    let worker_result = worker.join();
    drop(pool);
    let joined: Vec<_> = threads.into_iter().map(|t| t.join()).collect();
    let c = match result {
        Ok(c) => c,
        Err(panic) => resume_unwind(panic),
    };
    worker_result.expect("worker joins without a panic");
    for result in joined { result.expect("pool thread joins without a panic"); }
    let sent = samples.len() - 1;
    if c.answers.is_empty() {
        println!("gesture-cadence board={name} case={case} cadence_us={cadence_us} qos={qos} sent={sent} answered=0 superseded={sent} fulls={}: no Stretch answers after warm-up", c.fulls);
        return;
    }
    let pick = |during: Option<bool>, f: fn(&(f64, f64, bool)) -> f64| -> Vec<f64> {
        c.answers.iter().filter(|a| during.is_none_or(|d| a.2 == d)).map(f).collect()
    };
    let all = stats(&pick(None, |a| a.0));
    let during = pick(Some(true), |a| a.0);
    let during = (!during.is_empty()).then(|| stats(&during));
    println!("gesture-cadence board={name} case={case} cadence_us={cadence_us} qos={qos} sent={sent} answered={} superseded={} fulls={} tier1_median_us={:.1} tier1_p95_us={:.1} tier1_max_us={:.1} during_wave={} during_median_us={:.1} during_p95_us={:.1} during_max_us={:.1}",
        c.answers.len(), sent - c.answers.len(), c.fulls, all.median, all.p95, all.worst, c.answers.iter().filter(|a| a.2).count(),
        during.as_ref().map_or(0.0, |s| s.median), during.as_ref().map_or(0.0, |s| s.p95), during.as_ref().map_or(0.0, |s| s.worst));
    // Outside waves, the time from a sample to its answer split: the answer's compute, and the rest
    // (the worker waking, reading the sample, publishing, and this thread waking).
    let compute = pick(Some(false), |a| a.1);
    let rest = pick(Some(false), |a| a.0 - a.1);
    if !compute.is_empty() {
        let (compute, rest) = (stats(&compute), stats(&rest));
        println!("gesture-cadence-split board={name} case={case} cadence_us={cadence_us} qos={qos} outside_wave={} compute_median_us={:.1} compute_p95_us={:.1} compute_max_us={:.1} rest_median_us={:.1} rest_p95_us={:.1} rest_max_us={:.1}",
            c.answers.iter().filter(|a| !a.2).count(), compute.median, compute.p95, compute.worst, rest.median, rest.p95, rest.worst);
    }
}

/// What idling between samples costs a thread that waits as the worker does (a condition variable
/// signalled with each sample), at each cadence (0: back to back): the time from the signal to the
/// thread running (wake), then a fixed chain of multiply-adds (the core's clock: no memory), timed
/// whole and in its first and last tenth (a clock still rising after the wake runs the first tenth
/// slower), and a walk over a fixed 4 MB buffer it walked the round before (its caches). 200
/// rounds each, at the normal priority and at the user-interactive class (`interactive`).
/// Set the calling thread's priority class, as the app's thread spawner does, and say when the
/// platform did not apply it (the comparison is then of two normal threads).
fn priority(class: CxThreadPriority) {
    let status = Cx::set_thread_priority(class);
    if !matches!(status, PriorityStatus::Applied) {
        println!("gesture-qos class={class:?} status={status:?}");
    }
}

fn idle_probe(interactive: bool) {
    use std::sync::{Condvar, Mutex};
    let shared = std::sync::Arc::new((Mutex::new(None::<Instant>), Condvar::new()));
    let (done, results) = mpsc::channel::<[f64; 5]>();
    let waiter = {
        let shared = shared.clone();
        thread::spawn(move || {
            if interactive { priority(CxThreadPriority::UserInteractive); }
            let buffer = vec![1u8; 4 << 20];
            let (lock, ready) = &*shared;
            loop {
                let sent = {
                    let mut slot = lock.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if let Some(sent) = slot.take() { break sent; }
                        slot = ready.wait(slot).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let wake = sent.elapsed().as_secs_f64() * 1e6;
                let t = Instant::now();
                let mut x = 1u64;
                let mut tenths = [0.0f64; 10];
                for tenth in &mut tenths {
                    let at = Instant::now();
                    for _ in 0..100_000 { x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)); }
                    *tenth = at.elapsed().as_secs_f64() * 1e6;
                }
                let chain = t.elapsed().as_secs_f64() * 1e6;
                let t = Instant::now();
                let mut sum = 0u64;
                for i in (0..buffer.len()).step_by(64) { sum = sum.wrapping_add(u64::from(std::hint::black_box(buffer[i]))); }
                let walk = t.elapsed().as_secs_f64() * 1e6;
                std::hint::black_box((x, sum));
                if done.send([wake, chain, tenths[0], tenths[9], walk]).is_err() { return; }
            }
        })
    };
    for cadence_us in [0u64, 8_000, 16_667] {
        let mut rounds = Vec::new();
        for round in 0..210 {
            if cadence_us > 0 { thread::sleep(Duration::from_micros(cadence_us)); }
            *shared.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
            shared.1.notify_one();
            let result = results.recv().expect("the probe thread answers");
            if round >= 10 { rounds.push(result); }
        }
        let of = |k: usize| stats(&rounds.iter().map(|r: &[f64; 5]| r[k]).collect::<Vec<_>>());
        let (wake, chain, first, last, walk) = (of(0), of(1), of(2), of(3), of(4));
        println!("gesture-idle cadence_us={cadence_us} qos={} rounds={} wake_median_us={:.1} wake_p95_us={:.1} wake_max_us={:.1} chain_median_us={:.1} chain_p95_us={:.1} chain_max_us={:.1} first_tenth_median_us={:.1} first_tenth_p95_us={:.1} last_tenth_median_us={:.1} last_tenth_p95_us={:.1} walk_median_us={:.1} walk_p95_us={:.1} walk_max_us={:.1}",
            if interactive { "interactive" } else { "normal" }, rounds.len(), wake.median, wake.p95, wake.worst, chain.median, chain.p95, chain.worst, first.median, first.p95, last.median, last.p95, walk.median, walk.p95, walk.worst);
    }
    // Its next answer finds no receiver, and it ends.
    drop(results);
    *shared.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    shared.1.notify_one();
    waiter.join().expect("the probe thread joins");
}

fn tenths(values: &[f64]) -> String {
    let mut medians = Vec::with_capacity(10);
    for i in 0..10 {
        let slice = &values[i * values.len() / 10..(i + 1) * values.len() / 10];
        medians.push(if slice.is_empty() { "NA".into() } else { format!("{:.1}", stats(slice).median) });
    }
    medians.join(",")
}

fn item_count(b: &Board) -> usize {
    b.placed.len() + b.tracks.len() + b.vias.len() + b.zones.len() + b.keepouts.len()
        + b.holes.len() + b.cutouts.len() + b.graphics.len() + b.texts.len() + b.tunings.len()
}

/// A board read from text: its documents and their evaluation, to splice an answer's ops into
/// and read the result back (`round_trip`).
struct Text {
    sources: Sources,
    libs: Libs,
    prev: Evaluated,
    unit: BoardId,
}

impl Text {
    /// Evaluate `sources`; the board of the unit named `unit` (else the first with a board).
    fn new(what: &str, sources: Sources, unit: &str) -> (Arc<Board>, Arc<Text>) {
        let libs = std_libs();
        let prev = evaluate(&sources, &libs).unwrap_or_else(|diags| panic!("{what} must evaluate: {diags:?}"));
        let units = &prev.design.units;
        let unit = units.iter().find(|u| u.name == unit && u.board.is_some()).or_else(|| units.iter().find(|u| u.board.is_some()))
            .unwrap_or_else(|| panic!("{what} has a board"));
        let (board, id) = (unit.board.clone().expect("a unit with a board"), unit.id);
        (board, Arc::new(Text { sources, libs, prev, unit: id }))
    }

    /// `ops` spliced into the text and read back give `after`, the board the model's batch left:
    /// the same placements, tracks (their anchors included) and vias; else the first difference.
    fn round_trip(&self, what: &str, after: &Board, ops: &[BoardOp]) -> Result<(), String> {
        let ops: Vec<DesignOp> = ops.iter().map(|op| DesignOp::Board { unit: self.unit, op: op.clone() }).collect();
        let verified = splice_verified(&self.sources, &self.libs, &self.prev, &ops, &NO_CANCEL)
            .unwrap_or_else(|e| panic!("{what}: the answer's ops splice into the text: {e:?}"));
        let read = verified.evaluated.design.unit(self.unit).and_then(|u| u.board.as_deref()).expect("the spliced text has the board");
        same(what, "placement", &read.placed, &after.placed)?;
        same(what, "via", &read.vias, &after.vias)?;
        same(what, "track (anchors included)", &read.tracks, &after.tracks)
    }
}

/// `read` (the spliced text read back) is `model`, item by item; else the first that differs.
fn same<T: PartialEq + std::fmt::Debug>(what: &str, kind: &str, read: &[T], model: &[T]) -> Result<(), String> {
    if let Some((r, m)) = read.iter().zip(model).find(|(r, m)| r != m) {
        return Err(format!("{what}: the spliced text reads back as the model's {kind}: read {r:?}, model {m:?}"));
    }
    if read.len() != model.len() {
        return Err(format!("{what}: the spliced text reads back as many of each {kind} as the model: read {}, model {}", read.len(), model.len()));
    }
    Ok(())
}

/// Every track an op of `ops` adds, inserts or replaces (the last op naming it) is on `after`
/// exactly as written, but a free end written where a reader attaches one, which is attached as
/// the reader attaches it (`Board::infer_anchor`); and every anchored track end of `after` is on
/// its anchor.
fn landed_as_written(what: &str, after: &Board, ops: &[BoardOp]) {
    let mut written: Vec<&Track> = Vec::new();
    for op in ops {
        match op {
            BoardOp::Add(Item::Track(t)) | BoardOp::Insert { item: Item::Track(t), .. } | BoardOp::Replace(Item::Track(t)) => {
                written.retain(|w| w.id != t.id);
                written.push(t);
            }
            BoardOp::Remove(id) => written.retain(|w| w.id != *id),
            _ => {}
        }
    }
    for t in written {
        let landed = after.tracks.iter().find(|x| x.id == t.id).unwrap_or_else(|| panic!("{what}: track {:?} is on the board", t.id));
        let mut written = t.clone();
        for (e, p) in [t.path.start, t.path.end()].into_iter().enumerate() {
            if t.ends[e] == Anchor::Free {
                written.ends[e] = after.infer_anchor(t, p);
            }
        }
        assert_eq!(landed, &written, "{what}: track {:?} lands exactly as its op wrote it", t.id);
    }
    for t in &after.tracks {
        for (e, a) in t.ends.iter().enumerate() {
            let Some(at) = after.anchor_pos(*a) else { continue };
            let end = if e == 0 { t.path.start } else { t.path.end() };
            assert_eq!(end, at, "{what}: track {:?}'s end {e} is on its anchor {a:?}", t.id);
        }
    }
}

fn setup(name: &str, flags: &Flags) -> (Base, RBase, Option<Arc<Text>>) {
    let ((board, text), board_us) = timed(|| {
        match name {
            "ledctl" => {
                let (board, text) = Text::new("the ledctl fixture", fab_elec_fixtures::docs("ledctl").expect("ledctl fixture documents"), "ledctl");
                (board, Some(text))
            }
            "generated" => (Arc::new(generated_board(&GenSpec { segments: flags.segments, seed: flags.seed, ..GenSpec::bar() })), None),
            "drum16" => {
                let (board, text) = Text::new("the drum16 fixture", fab_elec_fixtures::docs("drum16").expect("drum16 fixture documents"), "drum16");
                (board, Some(text))
            }
            "kicad" => {
                let path = flags.kicad.as_deref().expect("--kicad names the board");
                let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("the --kicad board reads: {e}"));
                let pcb = fab_elec_kicad::read_kicad_pcb(&bytes, "kicad").unwrap_or_else(|r| panic!("the --kicad board imports: {}", r.message));
                let project = fab_elec_kicad::splash::to_splash(&pcb, "kicad").unwrap_or_else(|e| panic!("the --kicad board writes to Splash: {e}"));
                let mut sources = Sources::new();
                for (name, text) in project.docs {
                    sources.add(name, text).unwrap_or_else(|d| panic!("the --kicad board's documents load: {}", d.message));
                }
                let (board, text) = Text::new("the --kicad board", sources, "kicad");
                (board, Some(text))
            }
            _ => unreachable!("unknown board"),
        }
    });
    let (rules, rules_us) = timed(|| Arc::new(resolve_rules(&board, None)));
    // Heads ignore zone fills, so every phase uses connectivity with empty fills.
    let fills = Arc::new(Vec::new());
    let (conn, connect_us) = timed(|| Arc::new(connect(&board, &fills)));
    let (base, base_us) = timed(|| Base::new(board, rules, conn, fills));
    let (world, rbase_us) = timed(|| RBase::build(&base.board));
    println!("gesture-setup board={name} items={} board_us={board_us:.1} rules_us={rules_us:.1} connect_us={connect_us:.1} base_us={base_us:.1} rbase_us={rbase_us:.1}",
        item_count(&base.board));
    (base, world, text)
}

struct CursorPath {
    // Cumulative arc length lets both the cursor and the half-mm trail use the same path.
    marks: Vec<(f64, P)>,
}

impl CursorPath {
    fn new(start: P) -> Self {
        Self { marks: vec![(0.0, start)] }
    }

    fn push(&mut self, point: P) {
        let &(distance, last) = self.marks.last().unwrap();
        if point != last {
            self.marks.push((distance + point.sub(last).len(), point));
        }
    }

    fn length(&self) -> f64 {
        self.marks.last().unwrap().0
    }

    fn point_at(&self, distance: f64) -> P {
        let right = self.marks.partition_point(|&(d, _)| d < distance);
        if right == 0 { return self.marks[0].1; }
        if right == self.marks.len() { return self.marks[right - 1].1; }
        let (d0, a) = self.marks[right - 1];
        let (d1, b) = self.marks[right];
        let t = (distance - d0) / (d1 - d0);
        P::from_f(a.x as f64 + t * f64::from(b.x - a.x), a.y as f64 + t * f64::from(b.y - a.y))
    }
}

/// A route the person draws again: the board's own copper for it is taken off, and the cursor
/// follows where it lay, so the head has a legal way to its target at every sample.
struct Route {
    /// What kind of route this is, for the report.
    what: &'static str,
    removed: Vec<ItemId>,
    start: Anchor,
    target: Anchor,
    layer: LayerId,
    net: NetId,
    path: CursorPath,
    /// Arc lengths along `path` where the person clicks, with the layer a click's via goes to.
    clicks: Vec<(f64, Option<LayerId>)>,
}

/// A track's points from its end at `from`, when every segment is a line, and octilinear unless
/// `any_angle`: the head lays octilinear copper, so it follows an octilinear track exactly.
fn from_end_at(track: &Track, from: Anchor, any_angle: bool) -> Option<Vec<P>> {
    let mut points = vec![track.path.start];
    for seg in &track.path.segs {
        let fab_elec_core::Seg::Line(p) = seg else { return None };
        let a = *points.last()?;
        let (dx, dy) = ((p.x - a.x).abs(), (p.y - a.y).abs());
        if !any_angle && dx != 0 && dy != 0 && dx != dy { return None; }
        points.push(*p);
    }
    if track.ends[0] == from { Some(points) } else if track.ends[1] == from { points.reverse(); Some(points) } else { None }
}

fn from_end(track: &Track, from: Anchor) -> Option<Vec<P>> {
    from_end_at(track, from, false)
}

fn length(points: &[P]) -> f64 {
    points.windows(2).map(|w| w[1].sub(w[0]).len()).sum()
}

/// Whether `items` are legal copper on `full`: a generated board's tracks need not be, and a head
/// never lays an illegal one.
fn legal(full: &Base, world: &RBase, items: &[Item]) -> bool {
    let over = World::new(world);
    items.iter().all(|item| verify(&over, &full.rules, &full.board, &new_pieces(&full.board, item)).is_empty())
}

/// The longest legal front track from a pad to another pad.
fn longest_track<'a>(full: &'a Base, world: &RBase) -> Option<(&'a Track, Vec<P>)> {
    let mut tracks: Vec<_> = full.board.tracks.iter()
        .filter(|t| t.layer == LayerId::F_CU && matches!(t.ends, [Anchor::Pad { .. }, Anchor::Pad { .. }]) && t.ends[0] != t.ends[1])
        .filter_map(|t| Some((t, from_end(t, t.ends[0])?)))
        .collect();
    tracks.sort_by(|a, b| length(&b.1).total_cmp(&length(&a.1)));
    tracks.into_iter().find(|(t, _)| legal(full, world, &[Item::Track((*t).clone())]))
}

/// The longest legal route from a pad on the front through through vias to another pad: a chain
/// of tracks that meet only at vias (exactly two tracks at each), alternating front and back.
fn longest_via_route(full: &Base, world: &RBase) -> Option<Route> {
    let board = &full.board;
    let mut best: Option<(f64, Route)> = None;
    let starts = board.tracks.iter().filter(|t| t.layer == LayerId::F_CU)
        .flat_map(|t| t.ends.iter().filter(|e| matches!(e, Anchor::Pad { .. })).map(move |&e| (t, e)));
    for (first, start) in starts {
        let mut items = vec![Item::Track(first.clone())];
        // A routed chain enters its pads with a short stub at any angle: the head makes it octilinear.
        let Some(mut points) = from_end_at(first, start, true) else { continue };
        let mut clicks = vec![(0.4 * length(&points), None)];
        let (mut track, mut at) = (first, if first.ends[0] == start { first.ends[1] } else { first.ends[0] });
        let target = loop {
            let Anchor::Via(id) = at else { break Some(at).filter(|end| matches!(end, Anchor::Pad { .. }) && *end != start) };
            let Some(via) = board.vias.iter().find(|v| v.id == id && v.span == (LayerId::F_CU, LayerId::B_CU)) else { break None };
            let attached: Vec<&Track> = board.tracks.iter().filter(|t| t.ends.contains(&at)).collect();
            let [a, b] = attached[..] else { break None };
            let next = if std::ptr::eq(a, track) { b } else { a };
            if next.layer == track.layer || items.len() > 16 { break None; }
            let Some(more) = from_end_at(next, at, true) else { break None };
            clicks.push((length(&points), Some(next.layer)));
            points.extend_from_slice(&more[1..]);
            items.push(Item::Via(via.clone()));
            items.push(Item::Track(next.clone()));
            at = if next.ends[0] == at { next.ends[1] } else { next.ends[0] };
            track = next;
        };
        let Some(target) = target else { continue };
        if clicks.len() < 2 { continue; }
        let total = length(&points);
        if best.as_ref().is_some_and(|(len, _)| *len >= total) || !legal(full, world, &items) { continue; }
        let mut path = CursorPath::new(points[0]);
        for &p in &points[1..] { path.push(p); }
        let removed = items.iter().map(|item| match item { Item::Track(t) => t.id, Item::Via(v) => v.id, _ => unreachable!("tracks and vias") }).collect();
        best = Some((total, Route { what: "pad-vias-pad", removed, start, target, layer: LayerId::F_CU, net: first.net, path, clicks }));
    }
    best.map(|(_, route)| route)
}

/// The case's route on `board`. script: the longest front-to-back route through one via, with
/// a click partway and the via click (else the longest track with two clicks); track: the
/// longest front track, one drag; scribble: that track's corridor dragged back and forth to
/// `scribble_points` trail points.
fn route_for(full: &Base, world: &RBase, case: &str, scribble_points: usize) -> Route {
    if case == "script" {
        if let Some(route) = longest_via_route(full, world) { return route; }
    }
    let (track, points) = longest_track(full, world).expect("a legal front track from pad to pad");
    let mut path = CursorPath::new(points[0]);
    let len = length(&points);
    let clicks = match case {
        "script" => vec![(0.4 * len, None), (0.7 * len, None)],
        _ => Vec::new(),
    };
    if case == "scribble" {
        let end = (scribble_points - 1) as f64 * f64::from(mm(0.5));
        let mut back = points.clone();
        back.reverse();
        let mut forward = true;
        while path.length() < end {
            for &p in (if forward { &points } else { &back }).iter().skip(1) { path.push(p); }
            forward = !forward;
        }
        let last = path.point_at(end);
        path.marks.retain(|(d, _)| *d < end);
        path.marks.push((end, last));
    } else {
        for &p in &points[1..] { path.push(p); }
    }
    Route {
        what: if case == "script" { "pad-pad, clicked" } else if case == "scribble" { "pad-pad, back and forth" } else { "pad-pad" },
        removed: vec![track.id], start: track.ends[0], target: track.ends[1], layer: track.layer, net: track.net, path, clicks,
    }
}

/// `full` without the route's copper: its own base and world.
fn without(full: &Base, route: &Route) -> (Base, RBase) {
    let mut board = (*full.board).clone();
    let ops: Vec<BoardOp> = route.removed.iter().map(|&item| BoardOp::Remove(item)).collect();
    board.apply_all(&ops).expect("the route's copper comes off");
    let board = Arc::new(board);
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(Vec::new());
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules, conn, fills);
    let world = RBase::build(&base.board);
    (base, world)
}

fn check_part_answer(p: &RouteProposal, sample: &GestureSample, base: &Base) {
    assert!(matches!(p.source, ProposalSource::Gesture { id, seq, .. } if id == sample.id && seq == sample.seq), "answer must name the submitted sample");
    assert_eq!((p.base_generation, p.base_hash), (base.board.generation, base.hash), "answer must use the held base");
}

fn check_answer<'a>(p: &'a RouteProposal, sample: &GestureSample, base: &Base) -> &'a HeadAnswer {
    assert!(matches!(p.source, ProposalSource::Gesture { id, seq, .. } if id == sample.id && seq == sample.seq),
        "answer must name the submitted sample");
    assert_eq!((p.unit, p.base_generation, p.base_hash), (base.unit, base.board.generation, base.hash), "answer must use the held base");
    if !p.verified {
        eprintln!("unverified gesture seq={} collisions={:#?}", sample.seq, p.collisions);
        panic!("unverified gesture answer");
    }
    let head = p.head.as_ref().expect("a route answer has a head");
    if let Some(blocked) = head.blocked {
        if matches!(blocked.cause, BlockCause::Gap | BlockCause::End) {
            assert!(blocked.have < blocked.need, "a blocked head must report a real deficit: {blocked:?}");
        }
    }
    head
}

fn direct(name: &str, case: &str, route: &Route, base: &Base, world: &RBase, flags: &Flags) -> (Vec<Recorded>, RouteProposal, Timings) {
    let count = flags.samples;
    let path = &route.path;
    let length = path.length();
    let mut gesture = Gesture::new(GestureKind::Route, base.unit, base.board.generation, route.start, path.point_at(0.0), route.layer);
    gesture.net = Some(route.net);
    let mut state = GestureState::default();
    let mut samples = Vec::with_capacity(count);
    let mut times = Timings::default();
    let mut last = None;
    let mut next_trail = mm(0.5) as f64;
    let mut clicks = route.clicks.iter().copied().peekable();
    for i in 1..=count {
        let mut distance = length * (i as f64 / count as f64);
        // A click lands where the person clicked: that sample's cursor is moved back to it.
        let click = clicks.peek().copied().filter(|(at, _)| *at <= distance);
        if let Some((at, _)) = click { distance = at; }
        gesture.cursor = path.point_at(distance);
        gesture.target = (i == count && case != "scribble").then_some(route.target);
        // Keep the half-mm trail since the last click. The head appends an intermediate cursor.
        while next_trail <= distance {
            gesture.trail.push(path.point_at(next_trail));
            next_trail += mm(0.5) as f64;
        }
        let sample = GestureSample { id: GestureId(1), seq: i as u64, gesture: gesture.clone() };
        let ((answer, queries), wall) = timed(|| answer_counted(base, world, &sample, &mut state, ProposalId(0)));
        let proposal = match answer {
            Answer::Proposal(p) => p,
            Answer::Refused(cause, text) => panic!("{name} {case} seq={i} refused ({cause:?}): {text}"),
            Answer::Covered => panic!("{name} {case} seq={i} unexpectedly covered"),
        };
        let head = times.record(wall, &proposal, &sample, base);
        if i == count && case != "scribble" {
            assert_eq!(head.end, route.target, "{name} {case}: the last sample's head ends on the route's target (blocked {:?}, layer {:?}, route {})", head.blocked, head.layer, route.what);
        }
        samples.push(Recorded { sample, queries });
        if let Some((_, via_to)) = click {
            clicks.next();
            gesture.click(Leg { path: head.path.clone(), layer: head.layer, w: head.w, via_to });
            next_trail = distance + mm(0.5) as f64;
        }
        last = Some(proposal);
    }
    let last = last.expect("nonempty gesture");
    if case == "scribble" {
        assert_eq!(gesture.trail.len(), flags.scribble_points, "the final sample carries the whole scribble");
        assert!(gesture.legs.is_empty(), "a scribble is one unbroken drag");
    }
    let extra = format!(" route=\"{}\" route_mm={:.1}", route.what, length / 1e6);
    times.report("gesture", name, case, &samples, &extra);
    (samples, last, times)
}

/// The finish case: the track case's route drawn to half its length, then F. Prints the median and
/// worst of five Finishes, the basis build, the ops each side lays and whether the net is joined.
fn finish_case(name: &str, full: &Base, full_world: &RBase) {
    let route = route_for(full, full_world, "track", 2);
    let (base, world) = without(full, &route);
    // Pours filled: a Finish settles the hand part against them, as the worker's base has them.
    let fills = Arc::new(fill_all(&base.board, &base.rules));
    let conn = Arc::new(connect(&base.board, &fills));
    let base = Base::new(base.board.clone(), base.rules.clone(), conn, fills);
    let (basis, basis_us) = timed(|| Basis {
        index: PieceIndex::build(&base.board, &base.fills),
        violations: Arc::new(drc(&base.board, &base.conn, &base.rules, &base.fills, None).violations),
    });
    let path = &route.path;
    let half = path.length() / 2.0;
    let mut gesture = Gesture::new(GestureKind::Route, base.unit, base.board.generation, route.start, path.point_at(0.0), route.layer);
    gesture.net = Some(route.net);
    let mut along = mm(0.5) as f64;
    while along < half {
        gesture.trail.push(path.point_at(along));
        along += mm(0.5) as f64;
    }
    gesture.cursor = path.point_at(half);
    gesture.finish = true;
    let sample = GestureSample { id: GestureId(1), seq: 1, gesture };
    let hand = match answer_counted(&base, &world, &sample, &mut GestureState::default(), ProposalId(0)).0 {
        Answer::Proposal(proposal) => proposal,
        other => panic!("{name} finish: the half-drawn sample is answered: {other:?}"),
    };
    let mut walls = Vec::new();
    let mut last = None;
    for run in 1..=5 {
        let (finished, wall) = timed(|| finish(&base, &world, &basis, &sample, &hand, ProposalId(run), &Progress::new()));
        walls.push(wall);
        last = Some(finished);
    }
    let finished = last.expect("five runs");
    let joined = !finished.open.iter().any(|(net, _)| *net == route.net);
    let s = stats(&walls);
    println!("gesture board={name} case=finish runs=5 median_us={:.1} max_us={:.1} basis_us={basis_us:.1} hand_ops={} finish_ops={} joined={joined} route_mm={:.1}",
        s.median, s.worst, hand.ops.len(), finished.ops.len() - hand.ops.len(), path.length() / 1e6);
}

/// The drag case: a straight 45-degree segment at least 1 mm long of a legal track, its re-laid
/// window all lines, the one with the most copper within 2 mm, dragged 2 mm to each side and back.
/// The drag of `track`'s straight segment `seg`, pressed at its middle: the gesture, the press
/// and the segment's unit normal.
fn drag_gesture(board: &Board, unit: BoardId, track: &Track, seg: u32) -> (Gesture, P, (f64, f64)) {
    let segs = &track.path.segs;
    let (a, b) = (if seg == 0 { track.path.start } else { segs[seg as usize - 1].end() }, segs[seg as usize].end());
    let start = Anchor::Track { id: track.id, at: OnTrack::along(seg, a, None, b, P::new(a.x + (b.x - a.x) / 2, a.y + (b.y - a.y) / 2)) };
    let at = board.anchor_pos(start).expect("the press is on the segment");
    let (ux, uy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let len = ux.hypot(uy);
    let mut gesture = Gesture::new(GestureKind::DragSeg { track: track.id, seg }, unit, board.generation, start, at, track.layer);
    gesture.net = Some(track.net);
    (gesture, at, (-uy / len, ux / len))
}

/// Whether `track`'s segment `seg`, dragged 0.6 mm to either side, pushes another track.
fn drag_pushes(base: &Base, world: &RBase, track: &Track, seg: u32) -> bool {
    let (mut gesture, at, (nx, ny)) = drag_gesture(&base.board, base.unit, track, seg);
    [mm(0.6) as f64, -mm(0.6) as f64].into_iter().any(|by| {
        gesture.cursor = at.add(P::new((nx * by).round() as _, (ny * by).round() as _));
        let sample = GestureSample { id: GestureId(1), seq: 1, gesture: gesture.clone() };
        let mut state = GestureState::default();
        let Answer::Proposal(stretched) = answer_counted(base, world, &sample, &mut state, ProposalId(0)).0 else { return false };
        if stretched.last { return false; }
        let full = loop {
            if let Some((full, _)) = shove_step(base, world, &mut state, || ProposalId(1)) { break full; }
        };
        pushes_a_neighbour(&full.ops, Some(track.id))
    })
}

/// Whether `ops` lay a track again other than `except` and the tracks of the vias they move: a
/// neighbour pushed, as the drag-push and shove cases ask (a via's tracks follow it whoever
/// pushes it).
fn pushes_a_neighbour(ops: &[BoardOp], except: Option<ItemId>) -> bool {
    let moved: Vec<ItemId> = ops.iter().filter_map(|op| match op { BoardOp::Move { item, .. } => Some(*item), _ => None }).collect();
    ops.iter().any(|op| matches!(op, BoardOp::Replace(Item::Track(t))
        if Some(t.id) != except && !t.ends.iter().any(|end| matches!(end, Anchor::Via(v) if moved.contains(v)))))
}

/// The straight segments a drag can grab, the most copper within 2 mm first: at least 1 mm long,
/// at 45 degrees, with lines on each side; with that copper count.
fn drag_candidates<'a>(base: &'a Base, world: &RBase) -> Vec<(usize, &'a Track, u32)> {
    let board = &base.board;
    let mut candidates: Vec<(usize, &Track, u32)> = Vec::new();
    for track in board.tracks.iter().filter(|t| !t.locked) {
        let segs = &track.path.segs;
        let vertex = |k: usize| if k == 0 { track.path.start } else { segs[k - 1].end() };
        for seg in 0..segs.len() {
            if segs[seg.saturating_sub(1)..(seg + 2).min(segs.len())].iter().any(|s| !matches!(s, Seg::Line(_))) { continue; }
            let (a, b) = (vertex(seg), vertex(seg + 1));
            let (dx, dy) = ((b.x - a.x).abs(), (b.y - a.y).abs());
            if dx != 0 && dy != 0 && dx != dy || a.dist2(b) < (mm(1.0) as i128).pow(2) { continue; }
            let mut near = 0;
            world.query(track.layer, BBox::new(a, b).inflate(mm(2.0)), |_, _| { near += 1; ControlFlow::Continue(()) });
            candidates.push((near, track, seg as u32));
        }
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    candidates
}

/// A segment drag: `drag`, the straight segment with the most copper near it; `drag-push`, the
/// first of those that pushes a track.
fn drag(name: &str, case: &str, base: &Base, world: &RBase, text: Option<&Arc<Text>>, flags: &Flags) -> Vec<Recorded> {
    let board = &base.board;
    let best = drag_candidates(base, world).into_iter().filter(|(_, track, _)| legal(base, world, &[Item::Track((*track).clone())]))
        .find(|(_, track, seg)| case == "drag" || drag_pushes(base, world, track, *seg));
    let Some((near, track, seg)) = best else {
        println!("gesture board={name} case={case} none: no straight segment to drag");
        return Vec::new();
    };
    let segs = &track.path.segs;
    let (a, b) = (if seg == 0 { track.path.start } else { segs[seg as usize - 1].end() }, segs[seg as usize].end());
    let (mut gesture, at, (nx, ny)) = drag_gesture(board, base.unit, track, seg);
    let mut state = GestureState::default();
    let mut samples = Vec::with_capacity(flags.samples);
    let mut times = Timings::default();
    let mut causes = std::collections::BTreeMap::new();
    let checker = Checker::new(base, text);
    let mut waves = Waves::default();
    let (mut pushed, mut walked) = (0, 0);
    for i in 1..=flags.samples {
        // Out 2 mm, back through the press to 2 mm on the other side, and back to the press.
        let t = 4.0 * i as f64 / flags.samples as f64;
        let side = if t <= 1.0 { t } else if t <= 3.0 { 2.0 - t } else { t - 4.0 };
        let by = side * mm(2.0) as f64;
        gesture.cursor = at.add(P::new((nx * by).round() as _, (ny * by).round() as _));
        let sample = GestureSample { id: GestureId(1), seq: i as u64, gesture: gesture.clone() };
        let (stretched, full, queries, wall) = wave_sample(&format!("{name} {case} seq={i}"), base, world, &checker, &sample, &mut state, &mut waves);
        let head = times.record_drag(wall, &stretched, &full, &sample, base);
        if let Some(blocked) = head.blocked { *causes.entry(format!("{:?}", blocked.cause)).or_insert(0) += 1; }
        walked += usize::from(head.unpushed.is_some());
        pushed += usize::from(full.ops.iter().any(|op| matches!(op, BoardOp::Replace(Item::Track(t)) if t.id != track.id)));
        if i == flags.samples {
            assert!(full.ops.is_empty() && head.path == track.path, "{name} {case}: back at the press, the track springs back");
        }
        samples.push(Recorded { sample, queries });
    }
    let extra = format!(" route=\"segment drag\" seg_mm={:.1} near={near} pushed={pushed} walked={walked} blocked_by={causes:?}", (a.dist2(b) as f64).sqrt() / 1e6);
    times.report("gesture", name, case, &samples, &extra);
    waves.report(name, case, flags.samples);
    samples
}

/// `track`'s points with the middle half of its straight segment `seg` moved `by` nm along the
/// segment's unit normal: a cursor path that presses into what lies beside that segment.
fn pressed_path(track: &Track, seg: u32, by: f64) -> Vec<P> {
    let points: Vec<P> = std::iter::once(track.path.start).chain(track.path.segs.iter().map(Seg::end)).collect();
    let (a, b) = (points[seg as usize], points[seg as usize + 1]);
    let (ux, uy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let len = ux.hypot(uy);
    let aside = |t: f64| P::new((a.x as f64 + ux * t - uy / len * by).round() as _, (a.y as f64 + uy * t + ux / len * by).round() as _);
    let mut path = Vec::with_capacity(points.len() + 2);
    for (i, &p) in points.iter().enumerate() {
        if i == seg as usize + 1 {
            path.extend([aside(0.25), aside(0.75)]);
        }
        path.push(p);
    }
    path
}

/// Whether `track` drawn in Shove mode on `base` (where it still is: its own net is no obstacle)
/// along its `pressed_path` to the middle of the pressed segment pushes another track.
fn press_pushes(base: &Base, world: &RBase, track: &Track, seg: u32, by: f64) -> bool {
    let path = pressed_path(track, seg, by);
    let tip = seg as usize + 2;
    let (a, b) = (path[tip - 1], path[tip]);
    let cursor = P::new(a.x + (b.x - a.x) / 2, a.y + (b.y - a.y) / 2);
    let mut gesture = Gesture::new(GestureKind::Route, base.unit, base.board.generation, track.ends[0], path[0], track.layer);
    gesture.net = Some(track.net);
    gesture.width = WidthRule::Fixed(track.w);
    gesture.mode = Mode::Shove;
    gesture.trail = path[..tip].iter().copied().chain([cursor]).collect();
    gesture.cursor = cursor;
    let sample = GestureSample { id: GestureId(1), seq: 1, gesture };
    let mut state = GestureState::default();
    let Answer::Proposal(stretched) = answer_counted(base, world, &sample, &mut state, ProposalId(0)).0 else { return false };
    if stretched.last { return false; }
    let full = loop {
        if let Some((full, _)) = shove_step(base, world, &mut state, || ProposalId(1)) { break full; }
    };
    pushes_a_neighbour(&full.ops, None)
}

/// The shove case: the first pad-to-pad track among the drag cases' segments that, drawn again in
/// Shove mode with the middle half of that segment pressed 0.6 mm or 1 mm aside, pushes another
/// track there; taken off and drawn so from its first pad to its second. Each sample's stretch
/// answer is timed and its wave stepped to the end, the gates checked on its Full answer, as the
/// drag cases' are. Returns the base without the track and the samples, for the with-job and
/// cadence runs.
fn shove_route(name: &str, full: &Base, full_world: &RBase, flags: &Flags) -> Option<(Base, Vec<Recorded>)> {
    let pads = |track: &Track| track.ends.iter().all(|end| matches!(end, Anchor::Pad { .. }));
    let offsets = [mm(0.6) as f64, -mm(0.6) as f64, mm(1.0) as f64, -mm(1.0) as f64];
    let found = drag_candidates(full, full_world).into_iter()
        .filter(|(_, track, _)| pads(track) && legal(full, full_world, &[Item::Track((*track).clone())]))
        .find_map(|(near, track, seg)| offsets.into_iter().find(|&by| press_pushes(full, full_world, track, seg, by)).map(|by| (near, track, seg, by)));
    let Some((near, track, seg, by)) = found else {
        println!("gesture board={name} case=shove none: no pad-to-pad segment whose pressed redraw pushes a track");
        return None;
    };
    let points = pressed_path(track, seg, by);
    let mut path = CursorPath::new(points[0]);
    for &p in &points[1..] { path.push(p); }
    let route = Route {
        what: "pad-pad, pressed aside", removed: vec![track.id], start: track.ends[0], target: track.ends[1], layer: track.layer, net: track.net, path,
        clicks: Vec::new(),
    };
    let (base, world) = without(full, &route);
    let extra = format!(" near={near} pressed_mm={:.1}", by / 1e6);
    let samples = shoved(name, "shove", &route, Some(track.w), &base, &world, flags, &extra);
    Some((base, samples))
}

/// `route` drawn in Shove mode in `flags.samples` samples, its last on its target: each sample's
/// stretch answer timed, its wave stepped to the end and the gates checked on its Full answer.
/// Prints the case's head timings (with pushed and walked samples, and whether the last head
/// reached the target) and its waves.
fn shoved(name: &str, case: &str, route: &Route, w: Option<Nm>, base: &Base, world: &RBase, flags: &Flags, extra: &str) -> Vec<Recorded> {
    let length = route.path.length();
    let mut gesture = Gesture::new(GestureKind::Route, base.unit, base.board.generation, route.start, route.path.point_at(0.0), route.layer);
    gesture.net = Some(route.net);
    if let Some(w) = w { gesture.width = WidthRule::Fixed(w); }
    gesture.mode = Mode::Shove;
    let mut state = GestureState::default();
    let mut samples = Vec::with_capacity(flags.samples);
    let mut times = Timings::default();
    let checker = Checker::new(base, None);
    let mut waves = Waves::default();
    let (mut pushed, mut walked, mut reached) = (0, 0, false);
    let mut next_trail = mm(0.5) as f64;
    for i in 1..=flags.samples {
        let distance = length * (i as f64 / flags.samples as f64);
        gesture.cursor = route.path.point_at(distance);
        gesture.target = (i == flags.samples).then_some(route.target);
        while next_trail <= distance {
            gesture.trail.push(route.path.point_at(next_trail));
            next_trail += mm(0.5) as f64;
        }
        let sample = GestureSample { id: GestureId(1), seq: i as u64, gesture: gesture.clone() };
        let (stretched, full, queries, wall) = wave_sample(&format!("{name} {case} seq={i}"), base, world, &checker, &sample, &mut state, &mut waves);
        let head = times.record_drag(wall, &stretched, &full, &sample, base);
        walked += usize::from(head.unpushed.is_some());
        pushed += usize::from(full.ops.iter().any(|op| matches!(op, BoardOp::Replace(Item::Track(_)))));
        reached = head.end == route.target;
        samples.push(Recorded { sample, queries });
    }
    let extra = format!(" route=\"{}\" route_mm={:.1} pushed={pushed} walked={walked} reached_target={reached}{extra}", route.what, length / 1e6);
    times.report("gesture", name, case, &samples, &extra);
    waves.report(name, case, flags.samples);
    samples
}

/// The part case: the part with the most attached tracks, moved round a 1 mm circle and back
/// to its place.
fn part(name: &str, base: &Base, world: &RBase, text: Option<&Arc<Text>>, flags: &Flags) -> Vec<Recorded> {
    let board = &base.board;
    let attached = |id: ItemId| board.tracks.iter().filter(|t| t.ends.iter().any(|a| a.item() == Some(id))).count();
    let Some(part) = board.placed.iter().max_by_key(|p| (attached(p.id), std::cmp::Reverse(p.id))) else {
        println!("gesture board={name} case=part none: no part");
        return Vec::new();
    };
    let tracks = attached(part.id);
    let ids: Vec<ItemId> = board.tracks.iter().filter(|t| t.ends.iter().any(|a| a.item() == Some(part.id))).map(|t| t.id).collect();
    // Answers with a collision: on an attached track (left straight, or laid into another), else
    // only the part's own pads on other copper.
    let (mut on_tracks, mut on_part, mut relaid) = (0, 0, 0);
    let (mut cut, mut cut_samples, mut cut_max) = (0, 0, 0);
    let mut gesture = Gesture::new(GestureKind::DragParts, base.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU);
    let mut state = GestureState::default();
    let mut samples = Vec::with_capacity(flags.samples);
    let mut times = Timings::default();
    let checker = Checker::new(base, text);
    let mut waves = Waves::default();
    for i in 1..=flags.samples {
        let by = if i == flags.samples { P::ZERO } else {
            let a = std::f64::consts::TAU * i as f64 / (flags.samples - 1) as f64;
            P::new((mm(1.0) as f64 * a.cos()).round() as _, (mm(1.0) as f64 * a.sin()).round() as _).sub(P::new(mm(1.0), 0))
        };
        gesture.moved = vec![(part.id, Xform::translate(by))];
        gesture.cursor = part.at.add(by);
        let sample = GestureSample { id: GestureId(1), seq: i as u64, gesture: gesture.clone() };
        let (stretched, proposal, queries, wall) = wave_sample(&format!("{name} part seq={i}"), base, world, &checker, &sample, &mut state, &mut waves);
        times.record_part(wall, &stretched, &sample, base);
        relaid += proposal.ops.len().saturating_sub(1);
        let track = |sel: &fab_elec_core::Sel| ids.iter().any(|id| *sel == fab_elec_core::Sel::Item(board.unit, *id));
        assert!(!proposal.collisions.iter().any(|c| track(&c.a) && track(&c.b)),
            "{name} part seq={i}: no answer leaves two of the part's tracks in a short: {:?}", proposal.collisions);
        if !proposal.verified {
            if proposal.collisions.iter().any(|c| track(&c.a) || track(&c.b)) { on_tracks += 1 } else { on_part += 1 }
        }
        cut += proposal.open.len();
        cut_samples += usize::from(!proposal.open.is_empty());
        cut_max = cut_max.max(proposal.open.len());
        if i == flags.samples {
            assert!(proposal.ops.is_empty(), "{name} part: back at its place, nothing changes");
        }
        samples.push(Recorded { sample, queries });
    }
    let extra = format!(" route=\"part drag\" part={} attached_tracks={tracks} relaid_per_sample={:.1} colliding_on_tracks={on_tracks} colliding_part_only={on_part} cut_per_sample={:.2} samples_cutting={cut_samples} cut_max={cut_max}",
        part.path, relaid as f64 / flags.samples as f64, cut as f64 / flags.samples as f64);
    times.report("gesture", name, "part", &samples, &extra);
    waves.report(name, "part", flags.samples);
    samples
}

/// Single part moves, each from a fresh state, by reference designator; a board without the
/// parts has none.
fn part_moves(name: &str, base: &Base, world: &RBase, text: Option<&Arc<Text>>) {
    let board = &base.board;
    let checker = Checker::new(base, text);
    for (refdes, by) in [("R22", P::mm(1.5, -1.0)), ("R22", P::mm(1.0, -0.5)), ("C7", P::mm(0.0, -1.0)), ("D31", P::mm(-0.7, -0.7))] {
        let Some(part) = board.placed.iter().find(|p| p.refdes == refdes) else { continue };
        let mut gesture = Gesture::new(GestureKind::DragParts, base.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU);
        gesture.moved = vec![(part.id, Xform::translate(by))];
        let sample = GestureSample { id: GestureId(1), seq: 1, gesture };
        let mut waves = Waves::default();
        let what = format!("{name} part {refdes} by ({:.1}, {:.1}) mm", f64::from(by.x) / 1e6, f64::from(by.y) / 1e6);
        let (mut tier1, mut wall) = (Vec::new(), Vec::new());
        // Five runs, each from a fresh state, so each pays what a first drag of the part pays.
        for _ in 0..5 {
            let mut state = GestureState::default();
            let (stretched, full, _, us) = wave_sample(&what, base, world, &checker, &sample, &mut state, &mut waves);
            tier1.push(us);
            wall.push(us + waves.wave.last().copied().unwrap_or(0.0));
            assert_eq!(full.ops.first(), Some(&BoardOp::Move { item: part.id, by }), "{what}: the part's move comes first");
            let _ = stretched;
        }
        let s = &waves.stats;
        println!("gesture-part-move board={name} part={refdes} by_mm=({:.1},{:.1}) runs=5 tier1_median_us={:.1} wave_median_us={:.1} longest_step_us={:.1} to_full_median_us={:.1} pushed={} kept={} relaid={} cut={} repairs={} spikes={} joined={} tier1_open={} full_open={} full_colliding={}",
            f64::from(by.x) / 1e6, f64::from(by.y) / 1e6, stats(&tier1).median, if waves.wave.is_empty() { 0.0 } else { stats(&waves.wave).median },
            waves.longest.iter().copied().fold(0.0, f64::max), stats(&wall).median,
            s.pushed / 5, s.kept / 5, s.relaid / 5, s.cut / 5, s.repairs / 5, s.spikes / 5, s.joined / 5, waves.tier1_cut / 5, waves.full_cut / 5, waves.colliding / 5);
    }
}

type Task = Box<dyn FnOnce() + Send>;
struct Pool(Sender<Task>);

impl Spawn for Pool {
    fn spawn(&self, task: Task) {
        self.0.send(task).expect("pool is open while tasks can spawn tasks");
    }
}

fn pool() -> (Arc<Pool>, Vec<JoinHandle<()>>, Receiver<()>) {
    let (send, recv) = mpsc::channel::<Task>();
    let recv = Arc::new(Mutex::new(recv));
    let (started, starts) = mpsc::channel();
    let threads = (0..3).map(|_| {
        let recv = recv.clone();
        let started = started.clone();
        thread::spawn(move || loop {
            let task = { recv.lock().expect("pool queue lock").recv() };
            let Ok(task) = task else { break };
            let _ = started.send(());
            task();
        })
    }).collect();
    (Arc::new(Pool(send)), threads, starts)
}

fn next_event(handle: &RouteHandle, wakes: &Receiver<()>) -> RouteEvent {
    loop {
        if let Some(event) = handle.try_recv() { return event; }
        wakes.recv().expect("worker wake channel must stay open");
    }
}

struct ReplayJob {
    id: JobId,
    finished: bool,
    proposals: usize,
}

impl ReplayJob {
    fn start(&mut self, handle: &RouteHandle, base: &Base, starts: &Receiver<()>) {
        // Every checker that published the previous final has already taken its task.
        // Discard those start notifications before waiting for this route's own pool start.
        for () in starts.try_iter() {}
        self.id = JobId(self.id.0.checked_add(1).expect("route job id fits"));
        handle.request(RouteRequest::Route(RouteJob {
            job: self.id, unit: base.unit, scope: RouteScope::All, method: Method::default(), engine: Engine::Grid,
            style: None, effort: Effort::default(), layers: None, reroute: false,
        })).expect("route fits the command queue");
        starts.recv().expect("route task starts");
        self.finished = false;
    }

    fn event(&mut self, event: RouteEvent) -> Option<RouteProposal> {
        match event {
            RouteEvent::Proposal(p) => match p.source {
                ProposalSource::Job { job, .. } => {
                    assert_eq!(job, self.id, "only the requested job publishes");
                    self.proposals += 1;
                    self.finished = p.last;
                    None
                }
                ProposalSource::Gesture { .. } => Some(p),
            },
            RouteEvent::Status(_) => None,
            RouteEvent::Refused(r) => panic!("worker refused gesture {:?} seq={} ({:?}): {}", r.id, r.seq, r.cause, r.text),
            RouteEvent::Explain(_) => panic!("no explanation was requested"),
        }
    }
}

fn with_job(name: &str, case: &str, base: &Base, samples: &[Recorded]) -> Timings {
    let (pool, threads, starts) = pool();
    // Events stay in the real outbox. One pending wake suffices for the reader to check it.
    let (wake, wakes) = mpsc::sync_channel(1);
    let (handle, inbox) = route_channel(move || { let _ = wake.try_send(()); });
    let worker = RouteWorker::new(inbox, pool.clone());
    let worker = thread::spawn(move || worker.run());
    let mut job = ReplayJob { id: JobId(0), finished: true, proposals: 0 };
    // Always cancel, close and join, including after a correctness panic in the replay.
    let result = catch_unwind(AssertUnwindSafe(|| {
        handle.request(RouteRequest::Base(base.clone())).expect("base fits the command queue");
        let mut times = Timings::default();
        for recorded in samples {
            let sample = &recorded.sample;
            let replay = sample.clone();
            // A small job can finish during a long drag. Resume the pool load between samples.
            while let Some(event) = handle.try_recv() {
                assert!(job.event(event).is_none(), "unexpected gesture answer between samples");
            }
            if job.finished { job.start(&handle, base, &starts); }
            let start = Instant::now();
            handle.sample(replay);
            // The first answer is tier 1, unless the sample's own Full replaced it (below). A
            // Stretch that is not the last is followed by the sample's Full answer (a part or
            // segment drag's shove wave), timed to its arrival. A segment drag's Stretch then lies
            // over the tracks it pushes: its Full answer is checked.
            let (mut first, mut pushing) = (true, None);
            loop {
                if let Some(p) = job.event(next_event(&handle, &wakes)) {
                    let last = p.last;
                    if first {
                        if let ProposalSource::Gesture { id, seq, tier: fab_elec_route::Tier::Full } = p.source {
                            // This Full replaced its untaken Stretch. Its arrival time includes the wave.
                            assert_eq!((id, seq), (sample.id, sample.seq), "the Full answer must name the submitted sample");
                            times.superseded(us(start), &p, sample, base);
                            break;
                        }
                        match (&p.head, last) {
                            (None, _) => times.record_part(us(start), &p, sample, base),
                            (Some(_), true) => { times.record(us(start), &p, sample, base); }
                            (Some(_), false) => pushing = Some((us(start), p)),
                        }
                    } else {
                        assert!(matches!(p.source, ProposalSource::Gesture { id, seq, tier: fab_elec_route::Tier::Full } if id == sample.id && seq == sample.seq),
                            "the answer after a stretch answer that is not the last is the sample's Full answer");
                        times.full.push(us(start));
                        if let Some((wall, stretched)) = pushing.take() { times.record_drag(wall, &stretched, &p, sample, base); }
                    }
                    first = false;
                    if last { break; }
                }
            }
        }
        times
    }));
    let cancelled = handle.request(RouteRequest::Cancel(job.id));
    drop(handle);
    let worker_result = worker.join();
    // Checker tasks retain the sender while they can enqueue more work. Closing its last
    // owner drains queued work before the receivers stop; no sentinel can overtake a check.
    drop(pool);
    let joined: Vec<_> = threads.into_iter().map(|t| t.join()).collect();
    let times = match result {
        Ok(result) => result,
        Err(panic) => resume_unwind(panic),
    };
    cancelled.expect("cancel fits the command queue");
    worker_result.expect("worker joins without a panic");
    for result in joined { result.expect("pool thread joins without a panic"); }
    let extra = format!(" jobs_started={} job_proposals={}", job.id.0, job.proposals);
    times.report("gesture-with-job", name, case, samples, &extra);
    times
}

fn commit(name: &str, base: &Base, world: &RBase, proposal: &RouteProposal) {
    let mut board = (*base.board).clone();
    board.apply_all(&proposal.ops).expect("final gesture ops apply");
    let (delta, between_us) = timed(|| BoardDelta::between(&base.board, &board));
    let mut held = world.clone();
    let (_, update_us) = timed(|| held.update(&board, &delta));
    drop(std::hint::black_box(held));
    println!("gesture-commit board={name} between_us={between_us:.1} update_us={update_us:.1}");
}

fn gesture_thread(name: &str, base: &Base, world: &RBase) {
    let mut moved = (*base.board).clone();
    let part = moved.placed.iter().find(|part| !part.locked && part.origin.has_statement())
        .expect("a movable part for the delta").id;
    moved.apply(&BoardOp::Move { item: part, by: P::mm(0.5, 0.0) }).expect("the part moves");

    let (mut builds, mut diffs, mut updates, mut clones) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..5 {
        let (first, build_us) = timed(|| RBase::build(&base.board));
        drop(std::hint::black_box(first));
        let (delta, between_us) = timed(|| BoardDelta::between(&base.board, &moved));
        assert!(!delta.global && delta.changed.contains(&part), "the delta must update the moved part incrementally");
        // A queued job can retain the old world over a base change. Measure that deep copy
        // separately, and give every update its own world at the original generation.
        let (mut held, clone_us) = timed(|| RBase::clone(world));
        let (_, update_us) = timed(|| held.update(&moved, &delta));
        assert_eq!(held.generation(), moved.generation, "the updated world follows the move");
        drop(std::hint::black_box(held));
        builds.push(build_us);
        diffs.push(between_us);
        updates.push(update_us);
        clones.push(clone_us);
    }
    println!("gesture-thread board={name} runs={} first_base_median_us={:.1} between_median_us={:.1} update_median_us={:.1} clone_median_us={:.1}",
        builds.len(), stats(&builds).median, stats(&diffs).median, stats(&updates).median, stats(&clones).median);
}

fn free_spot(world: &RBase, bounds: BBox, layer: LayerId, offset: usize) -> P {
    let step = mm(1.0);
    let nx = ((bounds.width() - 2 * i64::from(step)) / i64::from(step)) as usize;
    let ny = ((bounds.height() - 2 * i64::from(step)) / i64::from(step)) as usize;
    for i in 0..nx * ny {
        let k = (i + offset * 7919) % (nx * ny);
        let at = bounds.min.add(P::new(step + (k % nx) as i32 * step, step + (k / nx) as i32 * step));
        let area = BBox::new(at, at.add(P::mm(0.2, 0.2))).inflate(mm(0.5));
        let mut empty = true;
        world.query(layer, area, |_, _| { empty = false; ControlFlow::Break(()) });
        if empty { return at; }
    }
    panic!("no free spot for an index edit on {layer:?}");
}

fn query_boxes(bounds: BBox) -> Vec<BBox> {
    // A 20 by 10 covering grid reaches retained additions as well as removal sites.
    let point = |x: i64, y: i64| P::new(
        (i64::from(bounds.min.x) + bounds.width() * x / 20) as i32,
        (i64::from(bounds.min.y) + bounds.height() * y / 10) as i32,
    );
    (0..200).map(|i| BBox::new(point(i % 20, i / 20), point(i % 20 + 1, i / 20 + 1))).collect()
}

type Identity = (Option<ItemId>, u32);

fn query_all(world: &RBase, boxes: &[BBox]) -> (Vec<Vec<Identity>>, f64) {
    let mut results = vec![Vec::new(); world.layers().len() * boxes.len()];
    let start = Instant::now();
    for (li, &layer) in world.layers().iter().enumerate() {
        for (qi, &area) in boxes.iter().enumerate() {
            let result = &mut results[li * boxes.len() + qi];
            world.query(layer, area, |_, item| {
                result.push((item.item, item.rank));
                ControlFlow::Continue(())
            });
        }
    }
    let elapsed = us(start);
    // Compare sets of stable piece identities, outside the query timer.
    for result in &mut results { result.sort_unstable(); result.dedup(); }
    (results, elapsed)
}

fn index_deltas(name: &str, base: &Base, world: &RBase) {
    let mut board = (*base.board).clone();
    let mut updated = world.clone();
    let bounds = board.outline_bbox();
    let mut added = Vec::new();
    assert!(!board.nets.is_empty(), "board has nets");
    let net = NetId(0);
    for i in 0..200 {
        let old = board.clone();
        let op = if i % 4 == 3 {
            BoardOp::Remove(added.pop().expect("a preceding addition exists"))
        } else {
            let layer = updated.layers()[(i - i / 4) % updated.layers().len()];
            let at = free_spot(&updated, bounds, layer, i);
            let id = board.alloc_id();
            added.push(id);
            BoardOp::Add(Item::Track(Track {
                id, net, layer, w: mm(0.2), widths: None,
                path: Path::new(at).line(at.add(P::mm(0.2, 0.0))).line(at.add(P::mm(0.2, 0.2))),
                ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
            }))
        };
        board.apply_all(&[op]).expect("index edit applies");
        let delta = BoardDelta::between(&old, &board);
        assert!(!delta.global && !delta.is_empty(), "index edit must exercise an incremental delta");
        updated.update(&board, &delta);
    }
    let fresh = RBase::build(&board);
    let boxes = query_boxes(bounds);
    let (updated_sets, updated_query_us) = query_all(&updated, &boxes);
    let (fresh_sets, fresh_query_us) = query_all(&fresh, &boxes);
    assert_eq!(updated.layers(), fresh.layers(), "updated copper layers agree");
    assert_eq!(updated.len(), fresh.len(), "updated piece count agrees");
    assert_eq!(updated.generation(), board.generation, "updated generation follows commits");
    for (i, (updated, fresh)) in updated_sets.iter().zip(&fresh_sets).enumerate() {
        assert_eq!(updated, fresh, "index identities differ at query {i}");
    }
    println!("gesture-index board={name} deltas=200 updated_query_us={updated_query_us:.1} fresh_query_us={fresh_query_us:.1}");
}

/// Each part of `flags.parts` (else the one with most attached tracks) moved round a circle of
/// `flags.radius` in `flags.samples` steps, the last back at its place: each sample from the
/// gesture's state so far, its wave stepped to the end and the gates checked on its Full answer.
fn circle(name: &str, base: &Base, world: &RBase, text: Option<&Arc<Text>>, flags: &Flags) {
    let board = &base.board;
    let attached = |id: ItemId| board.tracks.iter().filter(|t| t.ends.iter().any(|a| a.item() == Some(id))).count();
    let parts: Vec<&fab_elec_board::Placed> = if flags.parts.is_empty() {
        board.placed.iter().max_by_key(|p| (attached(p.id), std::cmp::Reverse(p.id))).into_iter().collect()
    } else {
        flags.parts.iter().map(|r| board.placed.iter().find(|p| p.refdes == *r).unwrap_or_else(|| panic!("{name} places {r}"))).collect()
    };
    let mut checker = Checker::new(base, text);
    checker.open = Some(Mutex::new(Vec::new()));
    for part in parts {
        let mut gesture = Gesture::new(GestureKind::DragParts, base.unit, board.generation, Anchor::Free, part.at, LayerId::F_CU);
        let (mut state, mut waves, mut times) = (GestureState::default(), Waves::default(), Timings::default());
        let mut samples = Vec::with_capacity(flags.samples);
        let r = flags.radius as f64;
        for i in 1..=flags.samples {
            let a = std::f64::consts::TAU * i as f64 / flags.samples as f64;
            let by = P::new((r * a.cos()).round() as _, (r * a.sin()).round() as _).sub(P::new(flags.radius, 0));
            gesture.moved = vec![(part.id, Xform::translate(by))];
            gesture.cursor = part.at.add(by);
            let sample = GestureSample { id: GestureId(1), seq: i as u64, gesture: gesture.clone() };
            let what = format!("{name} circle {} radius_um={} seq={i}", part.refdes, flags.radius / 1000);
            let (stretched, _, queries, wall) = wave_sample(&what, base, world, &checker, &sample, &mut state, &mut waves);
            times.record_part(wall, &stretched, &sample, base);
            samples.push(Recorded { sample, queries });
        }
        let case = format!("circle-{}-{}um", part.refdes, flags.radius / 1000);
        times.report("gesture", name, &case, &samples, "");
        waves.report(name, &case, flags.samples);
        let open = std::mem::take(&mut *checker.open.as_ref().expect("the circle counts open gates").lock().expect("the open findings"));
        for (gate, count, first) in open {
            println!("gesture-open board={name} case={case} gate=\"{gate}\" count={count} first=\"{first}\"");
        }
    }
}

fn main() {
    let flags = flags();
    if flags.kicad.is_some() {
        let (full, full_world, text) = setup("kicad", &flags);
        circle("kicad", &full, &full_world, text.as_ref(), &flags);
        return;
    }
    for name in ["ledctl", "generated", "drum16"] {
        if flags.board != "all" && flags.board != name { continue; }
        let (full, full_world, text) = setup(name, &flags);
        gesture_thread(name, &full, &full_world);
        for case in ["script", "track", "scribble"] {
            let route = route_for(&full, &full_world, case, flags.scribble_points);
            let (base, world) = without(&full, &route);
            let (samples, proposal, direct_times) = direct(name, case, &route, &base, &world, &flags);
            let worker_times = with_job(name, case, &base, &samples);
            // The track drawn again as the board draws it by default: in Shove mode.
            if case == "track" {
                let samples = shoved(name, "track-shove", &route, None, &base, &world, &flags, "");
                with_job(name, "track-shove", &base, &samples);
            }
            if case == "scribble" {
                println!("gesture-tenths board={name} case={case} trail_points={} direct_median_us={} with_job_median_us={} with_job_solve_median_us={}",
                    flags.scribble_points, tenths(&direct_times.wall), tenths(&worker_times.wall), tenths(&worker_times.solve));
            }
            if case == "script" { commit(name, &base, &world, &proposal); }
        }
        for case in ["drag", "drag-push"] {
            let samples = drag(name, case, &full, &full_world, text.as_ref(), &flags);
            if samples.is_empty() { continue; }
            with_job(name, case, &full, &samples);
            if case == "drag-push" { with_waves(name, case, &full, &samples, 8_000, false); }
        }
        if let Some((base, samples)) = shove_route(name, &full, &full_world, &flags) {
            with_job(name, "shove", &base, &samples);
            with_waves(name, "shove", &base, &samples, 8_000, false);
        }
        let samples = part(name, &full, &full_world, text.as_ref(), &flags);
        if !samples.is_empty() {
            with_job(name, "part", &full, &samples);
            for (cadence_us, interactive) in [(8_000, false), (16_667, false), (8_000, true), (16_667, true)] {
                with_waves(name, "part", &full, &samples, cadence_us, interactive);
            }
            for interactive in [false, true] { idle_probe(interactive); }
        }
        part_moves(name, &full, &full_world, text.as_ref());
        finish_case(name, &full, &full_world);
        index_deltas(name, &full, &full_world);
    }
}

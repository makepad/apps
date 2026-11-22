//! Per-sample engine cost with a snapshot retaining the committed design and board.

use fab_elec_board::{Anchor, Board, BoardOp, Factories, Item};
use fab_elec_core::{mm, BoardId, Contour, P};
use fab_elec_design::{By, Design, DesignOp};
use fab_elec_fixtures::ledctl;
use fab_elec_host::{Engine, Mailbox, Sample};
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::{atomic::AtomicU64, Arc};
use std::time::{Duration, Instant};

const WARMUP: usize = 20;
const REPEATS: usize = 200;
const TARGET_PLACED: usize = 10_000;
const UNIT: BoardId = BoardId(0);

fn board(design: &Design) -> &Board {
    design.unit(UNIT).expect("ledctl has unit 0").board.as_deref().expect("ledctl has a board")
}

/// Like the scaling fixture `ledctl_grid`, retain ledctl's circuit and nets: the copied
/// placements have no separate schematic. Only board geometry is scaled for this timing.
fn repeated_ledctl() -> (Design, usize) {
    let mut design = ledctl();
    let source = board(&design).clone();
    assert!(!source.placed.is_empty(), "ledctl must have placed parts");
    let copies = TARGET_PLACED.div_ceil(source.placed.len());
    let mut columns = 1;
    while columns * columns < copies {
        columns += 1;
    }
    let rows = copies.div_ceil(columns);

    // Include footprint extents as well as the outline, with a gap between copies.
    let mut bounds = source.outline_bbox();
    for item in source.placed.iter().cloned().map(Item::Placed)
        .chain(source.tracks.iter().cloned().map(Item::Track))
        .chain(source.vias.iter().cloned().map(Item::Via))
    {
        bounds = bounds.union(item.bbox(&source));
    }
    bounds = bounds.inflate(mm(1.0));
    let pitch = P::new(
        i32::try_from(bounds.width()).expect("cell width fits board coordinates"),
        i32::try_from(bounds.height()).expect("cell height fits board coordinates"),
    );
    let unit = design.units.iter_mut().find(|u| u.id == UNIT).expect("ledctl has unit 0");
    let board = Arc::make_mut(unit.board.as_mut().expect("ledctl has a board"));
    for copy in 1..copies {
        let by = P::new((copy % columns) as i32 * pitch.x, (copy / columns) as i32 * pitch.y);
        let mut ids = HashMap::new();
        // Allocate every id first so tracks can refer to other copied tracks.
        for id in source.placed.iter().map(|p| p.id)
            .chain(source.tracks.iter().map(|t| t.id))
            .chain(source.vias.iter().map(|v| v.id))
        {
            ids.insert(id, board.alloc_id());
        }
        for part in &source.placed {
            let mut part = part.clone();
            part.id = ids[&part.id];
            part.at = part.at.add(by);
            part.path = format!("{}#{}", part.path, copy);
            board.placed.push(part);
        }
        for via in &source.vias {
            let mut via = via.clone();
            via.id = ids[&via.id];
            via.at = via.at.add(by);
            board.vias.push(via);
        }
        for track in &source.tracks {
            let mut track = track.clone();
            track.id = ids[&track.id];
            track.path = track.path.translated(by);
            for end in &mut track.ends {
                *end = match *end {
                    Anchor::Free => Anchor::Free,
                    Anchor::Pad { placed, pad } => Anchor::Pad { placed: ids[&placed], pad },
                    Anchor::Via(id) => Anchor::Via(ids[&id]),
                    Anchor::Track { id, at } => Anchor::Track { id: ids[&id], at },
                    // Holes are not replicated; preserve the translated endpoint as free.
                    Anchor::Hole(_) => Anchor::Free,
                };
            }
            board.tracks.push(track);
        }
    }
    board.outline = vec![Contour::rect(
        bounds.min,
        bounds.max.add(P::new((columns - 1) as i32 * pitch.x, (rows - 1) as i32 * pitch.y)),
        0,
    )];
    board.reindex();
    (design, copies)
}

fn report(case: &str, measure: &str, placed: usize, times: &mut [Duration]) {
    times.sort_unstable();
    let median = (times[REPEATS / 2 - 1].as_secs_f64() + times[REPEATS / 2].as_secs_f64()) * 0.5;
    let max = times[REPEATS - 1].as_secs_f64();
    println!("sample_timing {case} {measure} median {:.3} ms max {:.3} ms (placed {placed})", median * 1e3, max * 1e3);
}

fn measure(case: &str, design: Design) {
    let b = board(&design);
    let placed = b.placed.len();
    let via = b.vias.first().expect("ledctl has a via to move").id;
    println!("sample_timing {case} counts placed {placed} tracks {} vias {}", b.tracks.len(), b.vias.len());

    // Loading full results and constructing samples are outside the sample timer.
    let steps = Arc::new(AtomicU64::new(0));
    let logged = Arc::new(AtomicU64::new(0));
    let mut engine = Engine::new(design, Arc::new(Factories::new()), steps, logged, "timing".into());
    let mailbox = Mailbox::default();
    let mut newest = engine.snapshot();
    let mut samples = Vec::with_capacity(REPEATS);
    for repeat in 0..WARMUP + REPEATS {
        let sample = Sample {
            seq: repeat as u64 + 1,
            tag: 1,
            ops: vec![DesignOp::Board {
                unit: UNIT,
                op: BoardOp::Move { item: via, by: P::mm(if repeat % 2 == 0 { 0.1 } else { -0.1 }, 0.0) },
            }],
            place: vec![],
            label: "move via".into(),
            by: By::Person,
            proposal: None,
            notes: vec![],
            circuits: vec![], expect: None,
        };
        let start = Instant::now();
        mailbox.preview(sample);
        let step = engine.step(mailbox.take(), &mut |snapshot| newest = snapshot);
        let elapsed = start.elapsed();
        assert!(step.rejected.is_empty(), "{case}: sample refused: {:?}", step.rejected);
        black_box(&newest);
        if repeat >= WARMUP {
            samples.push(elapsed);
        }
        // Fill jobs belong to the pool; only their creation in step is part of this timing.
        drop(step);
    }
    report(case, "sample", placed, &mut samples);

    let committed = board(engine.design());
    let mut clones = Vec::with_capacity(REPEATS);
    for repeat in 0..WARMUP + REPEATS {
        let start = Instant::now();
        let cloned = black_box(Board::clone(black_box(committed)));
        let elapsed = start.elapsed();
        if repeat >= WARMUP {
            clones.push(elapsed);
        }
        // Measure Board::clone alone, excluding disposal of the copied board.
        drop(cloned);
    }
    report(case, "clone", placed, &mut clones);
    black_box(newest);
}

fn main() {
    measure("ledctl", ledctl());
    let (design, copies) = repeated_ledctl();
    measure(&format!("ledctl x {copies}"), design);
}

//! Test prints through the engine: a piece's layers are the print's, cut to its cube, and it
//! takes all else about a point from the original at that point.
use super::tests::{cube, engine, engine_seeing, plate, sdf_plate, settled, setup, turned_vase, NoTreesBox};
use super::*;
use crate::piece::{read_pieces, test_input, Cube, Pieces, TestPrint};
use makepad_fab_gcode::record::{MoveKind, MoveRecord};
use makepad_fab_slicer::clip::intersection_ex;
use makepad_fab_toolpath::Role;
use std::sync::mpsc;


fn job(settings: &[(&str, &str)]) -> JobSetup {
    let mut job = (*setup()).clone();
    for &(k, v) in settings {
        job.process.values.set_str(k, v);
    }
    job
}

fn whole(id: u64, plate: &Plate, settings: &[(&str, &str)]) -> LiveInput {
    LiveInput::ready(id, 1, job(settings), Arc::new(plate.clone()), Arc::new(Vec::new()))
}

fn test(name: &str, center: [f64; 3], size: [f64; 3], turn: f64) -> TestPrint {
    test3(name, center, size, [0.0, 0.0, turn])
}

fn test3(name: &str, center: [f64; 3], size: [f64; 3], turn: [f64; 3]) -> TestPrint {
    TestPrint { name: name.into(), cube: Cube { center, size, turn }, pose: None, brim: false }
}

fn pieces(input: &LiveInput, tests: &[TestPrint]) -> LiveInput {
    test_input(input, tests).expect("they stand on the bed")
}

/// A settle's result and each part's last whole cut.
fn settled_with_cuts(e: &LiveEngine, rx: &mpsc::Receiver<LiveEvent>, tag: u64, input: LiveInput) -> (Arc<Settled>, HashMap<usize, Arc<CutLayers>>) {
    e.settle(tag, input);
    let mut cuts = HashMap::new();
    loop {
        match rx.recv().expect("a settled slice") {
            LiveEvent::PartCut { part, layers, stale, .. } if stale.is_empty() => {
                cuts.insert(part, layers);
            }
            LiveEvent::Settled { tag: t, result, .. } if t == tag => return (result.expect("sliced"), cuts),
            LiveEvent::Failed { error, .. } => panic!("{error}"),
            _ => {}
        }
    }
}

/// What is extruded, per object, by the role's name and rounded ends, sorted: what prints, in
/// whatever order and with whatever else on the plate. `d` is taken off each end.
fn printed(r: &Settled, object: u16, d: [f64; 2]) -> Vec<(u8, [i64; 6])> {
    let mut out: Vec<(u8, [i64; 6])> = r.layers.records().filter(|m| m.object == object && m.kind == MoveKind::Extrude && m.role != Role::PrimeTower && m.role != Role::Flush).map(|m| {
        let at = |v: f32, k: usize| ((v as f64 - if k < 2 { d[k] } else { 0.0 }) * 500.0).round() as i64;
        (m.role as u8, [at(m.p0[0], 0), at(m.p0[1], 1), at(m.p0[2], 2), at(m.p1[0], 0), at(m.p1[1], 1), at(m.p1[2], 2)])
    }).collect();
    out.sort_unstable();
    out
}

fn is_model(m: &MoveRecord) -> bool {
    m.kind == MoveKind::Extrude && !matches!(m.role, Role::Support | Role::SupportInterface | Role::SupportTransition | Role::Brim | Role::Skirt | Role::PrimeTower | Role::Flush | Role::Custom | Role::None)
}

fn read_demo(supports: bool) -> (Arc<Settled>, Arc<Pieces>) {
    if supports {
        read_of(plate(), vec![test("Cap", [100.0, 100.0, 8.0], [20.0, 20.0, 8.0], 0.0)],
            &[("enable_support", "1"), ("support_type", "tree(auto)"), ("brim_type", "outer_only"), ("brim_width", "3")])
    } else {
        read_two(&[("brim_type", "no_brim"), ("skirt_loops", "0")])
    }
}

/// Two small blocks, each with a piece of its own.
fn read_two(settings: &[(&str, &str)]) -> (Arc<Settled>, Arc<Pieces>) {
    let mut p = plate();
    p.parts[0].mesh = cube([96.0, 96.0, 0.0], [8.0, 8.0, 2.0]);
    p.parts[1].mesh = cube([200.0, 100.0, 0.0], [8.0, 8.0, 2.0]);
    read_of(p, vec![test("First", [100.0, 100.0, 1.0], [6.0, 6.0, 2.0], 0.0), test("Second", [204.0, 104.0, 1.0], [6.0, 6.0, 2.0], 0.0)], settings)
}

fn read_of(p: Plate, tests: Vec<TestPrint>, settings: &[(&str, &str)]) -> (Arc<Settled>, Arc<Pieces>) {
    let mut setup = job(settings);
    for (filament, density) in setup.filaments.iter_mut().zip(["1.24", "1.27"]) {
        filament.values.set_vec("filament_density", vec![density.into()]);
    }
    let whole = LiveInput::ready(1, 1, setup, Arc::new(p), Arc::new(Vec::new()));
    let input = pieces(&whole, &tests);
    let list = input.pieces.clone().unwrap();
    let (e, rx) = engine();
    e.settle(1, input);
    (settled(&rx, 1), list)
}

#[test]
fn a_pieces_read_adds_up_to_the_job() {
    let (r, pieces) = read_demo(false);
    let start = std::time::Instant::now();
    let read = read_pieces(r.layers.records(), &r.stats, &pieces);
    let elapsed = start.elapsed();
    assert_eq!(read.pieces.iter().map(|p| (p.test, p.name.as_str())).collect::<Vec<_>>(), [(0, "First"), (1, "Second")]);
    assert!((read.pieces.iter().map(|p| p.seconds).sum::<f64>() + read.shared_seconds - read.total_seconds).abs() < 1e-6, "time adds up");
    assert!((read.pieces.iter().map(|p| p.grams).sum::<f64>() + read.shared_grams - read.total_grams).abs() < 1e-6, "grams add up");
    assert_eq!(read.total_seconds, r.stats.total_time, "job time");
    assert_eq!(read.total_grams, r.stats.filament_g.iter().sum::<f64>(), "job grams");
    assert!(read.shared_seconds > 0.0, "start, end and changes take shared time");
    for p in &read.pieces {
        let own: Vec<_> = r.layers.records().filter(|m| m.object as usize == p.test).collect();
        let seconds: f64 = own.iter().map(|m| m.t_end - m.t_start).sum();
        assert!(p.seconds > 0.0 && (p.seconds - seconds).abs() < 1e-6, "only its own moves' time");
        for (slot, &mm) in p.filament_mm.iter().enumerate() {
            let expected: f64 = own.iter().filter(|m| m.filament as usize == slot + 1).map(|m| m.e as f64).sum();
            assert_eq!(mm, expected, "signed filament in slot {slot}");
        }
        let grams: f64 = p.filament_mm.iter().enumerate().filter(|(slot, _)| r.stats.filament_mm[*slot] != 0.0).map(|(slot, mm)| mm * r.stats.filament_g[slot] / r.stats.filament_mm[slot]).sum();
        assert!(p.grams > 0.0 && (p.grams - grams).abs() < 1e-12, "slot density converts its own length: {} g, expected {grams} g", p.grams);
    }
    eprintln!("piece read: {} records, {elapsed:?}, {:.6} s, {:.6} g", r.layers.records().count(), read.total_seconds, read.total_grams);
}

/// A skirt goes round the whole job: its length is shared, and no piece's bounds or footprint.
#[test]
fn a_skirt_round_the_pieces_belongs_to_none_of_them() {
    let (r, pieces) = read_two(&[("brim_type", "no_brim"), ("skirt_loops", "1")]);
    assert!(r.layers.records().any(|m| m.role == Role::Skirt && m.e > 0.0), "there is a skirt");
    let read = read_pieces(r.layers.records(), &r.stats, &pieces);
    let (plain, plain_pieces) = read_two(&[("brim_type", "no_brim"), ("skirt_loops", "0")]);
    let without = read_pieces(plain.layers.records(), &plain.stats, &plain_pieces);
    let skirt: f64 = r.layers.records().filter(|m| m.role == Role::Skirt).map(|m| m.e as f64).sum::<f64>() * r.stats.filament_g[0] / r.stats.filament_mm[0];
    assert!((read.shared_grams - without.shared_grams - skirt).abs() < 0.01, "the skirt's {skirt} g are shared: {} g with it, {} g without", read.shared_grams, without.shared_grams);
    for (p, q) in read.pieces.iter().zip(&without.pieces) {
        assert!((p.grams - q.grams).abs() < 0.01, "{}: {} g with a skirt, {} g without", p.name, p.grams, q.grams);
    }
    for (p, part) in read.pieces.iter().zip(&pieces.parts) {
        let (lo, hi) = part.cube.footprint();
        for k in 0..2 {
            assert!(p.bed_min[k] >= lo[k] + part.shift[k] - 0.5 && p.bed_max[k] <= hi[k] + part.shift[k] + 0.5, "{} axis {k}: {:?} .. {:?} from {lo:?} .. {hi:?}", p.name, p.bed_min, p.bed_max);
            assert!(p.footprint[k] <= hi[k] - lo[k] + 1.0, "{} axis {k}: footprint {:?}", p.name, p.footprint);
        }
    }
}

/// A test print's brim of its own rings its piece's first layer alone, as wide as the profile's
/// brim though the profile prints none, and changes nothing else that prints: the other piece
/// and the piece's own walls are as they were to the move. A brim is another test plate.
#[test]
fn a_test_prints_own_brim_rings_its_piece_alone() {
    let settings = [("brim_type", "no_brim"), ("brim_width", "3"), ("skirt_loops", "0")];
    let (plain, plain_pieces) = read_two(&settings);
    let mut p = plate();
    p.parts[0].mesh = cube([96.0, 96.0, 0.0], [8.0, 8.0, 2.0]);
    p.parts[1].mesh = cube([200.0, 100.0, 0.0], [8.0, 8.0, 2.0]);
    let tests = vec![TestPrint { brim: true, ..test("First", [100.0, 100.0, 1.0], [6.0, 6.0, 2.0], 0.0) }, test("Second", [204.0, 104.0, 1.0], [6.0, 6.0, 2.0], 0.0)];
    let (r, pieces) = read_of(p, tests.clone(), &settings);
    assert!(!plain.layers.records().any(|m| m.role == Role::Brim && m.e > 0.0), "the profile prints no brim");

    let read = read_pieces(plain.layers.records(), &plain.stats, &plain_pieces);
    let first = &read.pieces[0];
    let brim: Vec<MoveRecord> = r.layers.records().filter(|m| m.role == Role::Brim && m.kind == MoveKind::Extrude && m.e > 0.0).collect();
    assert!(!brim.is_empty(), "the test print with a brim of its own has one");
    let reach = 3.0 + 1.0;
    for m in &brim {
        for q in [m.p0, m.p1] {
            for k in 0..2 {
                assert!((q[k] as f64) >= first.bed_min[k] - reach && (q[k] as f64) <= first.bed_max[k] + reach, "the brim at {q:?} rings the first piece ({:?} .. {:?}), no further than the profile's 3 mm", first.bed_min, first.bed_max);
            }
            assert!((q[2] as f64) < 0.5, "the brim is on the first layer: {q:?}");
        }
    }
    let with = read_pieces(r.layers.records(), &r.stats, &pieces);
    assert!(first.brim_grams == 0.0 && with.pieces[0].brim_grams > 0.0, "its brim's grams are its own: {} g without, {} g with", first.brim_grams, with.pieces[0].brim_grams);
    assert!(with.pieces[0].support_grams >= with.pieces[0].brim_grams, "a brim's grams are of its support grams");
    assert_eq!(with.pieces[1].brim_grams, 0.0, "the other piece has no brim");
    let own = |r: &Settled, object: u16| printed(r, object, [0.0; 2]).into_iter().filter(|(role, _)| *role != Role::Brim as u8).collect::<Vec<_>>();
    assert_eq!(pieces.parts[1].shift, plain_pieces.parts[1].shift, "the other piece stands where it did");
    assert_eq!(own(&r, 1), own(&plain, 1), "the other piece prints as it did, to the move");
    assert_eq!(own(&r, 0), own(&plain, 0), "the piece's own walls and fill print as they did, to the move");
    let plan = crate::piece::TestPlan { reached: vec![vec![0], vec![1]], shifts: pieces_shifts(&pieces) };
    let without: Vec<TestPrint> = tests.iter().map(|t| TestPrint { brim: false, ..t.clone() }).collect();
    assert_ne!(crate::piece::test_plate_id(1, &tests, &plan), crate::piece::test_plate_id(1, &without, &plan), "a brim is another test plate");
}

/// Each test's shift, as its pieces stand.
fn pieces_shifts(pieces: &Pieces) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> = Vec::new();
    for p in &pieces.parts {
        if out.len() <= p.test {
            out.push(p.shift);
        }
    }
    out
}

/// The Slicer's figures read the live slice's estimate: they say what its settled slice does.
#[test]
fn a_pieces_read_of_the_estimate_is_the_settled_slices() {
    let mut p = plate();
    p.parts[0].mesh = cube([90.0, 90.0, 0.0], [20.0, 20.0, 20.0]);
    p.parts.truncate(1);
    let mut setup = job(&[]);
    for filament in &mut setup.filaments {
        filament.values.set_vec("filament_density", vec!["1.24".into()]);
    }
    let input = pieces(&LiveInput::ready(1, 1, setup, Arc::new(p), Arc::new(Vec::new())), &[test("Corner", [100.0, 100.0, 10.0], [8.0, 8.0, 8.0], 0.0)]);
    let list = input.pieces.clone().unwrap();
    let (e, rx) = engine();
    e.set_input(input.clone());
    let revision = e.revisions().0;
    loop {
        match rx.recv().expect("it slices") {
            LiveEvent::Toolpaths { revision: r, .. } if r == revision => break,
            LiveEvent::Failed { error, .. } => panic!("{error}"),
            _ => {}
        }
    }
    let (parts, records) = mpsc::channel();
    e.set_records(Arc::new(move |_, _| {
        let parts = parts.clone();
        Some(Box::new(move |streamed| {
            parts.send(streamed).expect("the estimate's records");
            None
        }))
    }));
    e.estimate();
    // The records as the time model makes them, then the estimate they make.
    let (estimate, records) = loop {
        match rx.recv().expect("its estimate") {
            LiveEvent::Estimated { streamed: true, .. } => {
                let (mut moves, mut done) = (Vec::new(), None);
                // The completed estimate follows Done, so all its records are ready.
                for streamed in records.try_iter() {
                    match streamed {
                        Streamed::Part { records, .. } => moves.extend(records),
                        Streamed::Done { estimate, .. } => done = Some(estimate),
                    }
                }
                break (done.expect("the estimate after its records"), moves);
            }
            LiveEvent::Failed { error, .. } => panic!("{error}"),
            _ => {}
        }
    };
    let (fresh, frx) = engine();
    fresh.settle(1, input);
    let r = settled(&frx, 1);
    let (of_estimate, of_slice) = (read_pieces(records.iter().copied(), &estimate.stats, &list), read_pieces(r.layers.records(), &r.stats, &list));
    assert!(of_estimate.pieces[0].grams > 0.0 && of_estimate.pieces[0].grams <= of_estimate.total_grams, "a piece is part of its job: {} g of {} g", of_estimate.pieces[0].grams, of_estimate.total_grams);
    assert!((of_estimate.total_grams - of_slice.total_grams).abs() < 1e-6, "the job's grams: {} and {}", of_estimate.total_grams, of_slice.total_grams);
    assert!((of_estimate.pieces[0].grams - of_slice.pieces[0].grams).abs() < 1e-6, "the piece's grams: {} and {}", of_estimate.pieces[0].grams, of_slice.pieces[0].grams);
    assert!((of_estimate.pieces[0].seconds - of_slice.pieces[0].seconds).abs() < 1e-6, "the piece's time: {} and {}", of_estimate.pieces[0].seconds, of_slice.pieces[0].seconds);
}

#[test]
fn a_pieces_read_matches_its_gcode() {
    let (r, pieces) = read_demo(false);
    let read = read_pieces(r.layers.records(), &r.stats, &pieces);
    let mut lengths = vec![0.0f64; pieces.parts.len()];
    let (mut object, mut absolute, mut position) = (None, true, 0.0);
    let mut labels = 0;
    for line in r.gcode.text().unwrap().lines() {
        if let Some(label) = line.strip_prefix("; OBJECT_ID: ").or_else(|| line.strip_prefix("; start printing object, unique label id: ")) {
            let label: u32 = label.trim().parse().unwrap();
            object = Some(r.stats.label_ids.iter().position(|id| *id == label).expect("a known object label"));
            labels += 1;
        } else if line.starts_with("; stop printing object,") {
            object = None;
        } else if let Some(feature) = line.strip_prefix("; FEATURE: ") {
            if matches!(Role::from_name(feature), Role::PrimeTower | Role::Flush | Role::Custom | Role::Skirt) {
                object = None;
            }
        }
        let mut words = line.split(';').next().unwrap().split_whitespace();
        let Some(command) = words.next() else { continue };
        let e = words.find_map(|w| w.strip_prefix('E').map(|v| v.parse::<f64>().expect("numeric E")));
        match command {
            "M82" => absolute = true,
            "M83" => absolute = false,
            "G92" => { if let Some(e) = e { position = e; } }
            "G0" | "G1" | "G2" | "G3" => if let Some(e) = e {
                let delta = if absolute { e - position } else { e };
                position += delta;
                if let Some(object) = object { lengths[object] += delta; }
            },
            _ => {}
        }
    }
    assert!(labels > pieces.parts.len(), "the text labels objects across layers");
    for (p, expected) in read.pieces.iter().zip(lengths) {
        let mm: f64 = p.filament_mm.iter().sum();
        let error = (mm - expected).abs();
        assert!(expected > 0.0 && error < expected * 0.005 && error < 0.5, "{}: records {mm} mm, G-code {expected} mm", p.name);
    }
}

#[test]
fn a_cube_over_two_parts_reads_as_one_piece() {
    let mut p = plate();
    p.parts[0].mesh = cube([100.0, 100.0, 0.0], [4.0, 8.0, 2.0]);
    p.parts[1].mesh = cube([106.0, 100.0, 0.0], [4.0, 8.0, 2.0]);
    let input = pieces(&whole(1, &p, &[("brim_type", "no_brim"), ("skirt_loops", "0")]), &[test("Across", [105.0, 104.0, 1.0], [10.0, 6.0, 2.0], 0.0)]);
    let list = input.pieces.clone().unwrap();
    assert_eq!(list.parts.len(), 2, "the cube reaches both parts");
    let (e, rx) = engine();
    e.settle(1, input);
    let r = settled(&rx, 1);
    let read = read_pieces(r.layers.records(), &r.stats, &list);
    assert_eq!(read.pieces.len(), 1, "one test print");
    assert_eq!(read.pieces[0].name, "Across");
    let mut sums = vec![0.0; r.stats.filament_mm.len()];
    for object in 0..2 {
        let mut length = 0.0;
        for m in r.layers.records().filter(|m| m.object == object && m.filament > 0) {
            sums[m.filament as usize - 1] += m.e as f64;
            length += m.e as f64;
        }
        assert!(length > 0.0, "part {object} contributes filament");
    }
    for (actual, expected) in read.pieces[0].filament_mm.iter().zip(sums) {
        assert!((actual - expected).abs() < 1e-9, "the sum of both parts, per slot");
    }
}

#[test]
fn a_pieces_read_has_its_supports_in_it() {
    let (r, pieces) = read_demo(true);
    let read = read_pieces(r.layers.records(), &r.stats, &pieces);
    let p = &read.pieces[0];
    let mut support_grams = 0.0;
    let (mut supports, mut brims) = (0, 0);
    let mut own_seconds = 0.0;
    for m in r.layers.records() {
        if m.object == 0 { own_seconds += m.t_end - m.t_start; }
        if m.kind != MoveKind::Extrude || m.e <= 0.0 { continue; }
        if matches!(m.role, Role::Support | Role::SupportInterface | Role::SupportTransition | Role::Brim) {
            assert_eq!(m.object, 0, "supports and brim retain the part's label");
            if m.role == Role::Brim { brims += 1; } else { supports += 1; }
            let slot = m.filament as usize - 1;
            support_grams += m.e as f64 * r.stats.filament_g[slot] / r.stats.filament_mm[slot];
        }
    }
    assert!(supports > 0 && brims > 0, "both support and brim print");
    assert!(p.support_grams > 0.0 && p.support_grams <= p.grams, "support is part of its grams");
    assert!((p.support_grams - support_grams).abs() < 1e-9, "all support and brim filament belongs to the piece");
    assert!((p.seconds - own_seconds).abs() < 1e-6, "support time stays with the piece");
    assert!((read.shared_seconds - (r.stats.total_time - own_seconds)).abs() < 1e-6, "none of its moves' time is shared");
    // The same slice without support labels still assigns their material by bed position.
    let unlabelled = r.layers.records().map(|mut m| {
        if m.kind == MoveKind::Extrude && matches!(m.role, Role::Support | Role::SupportInterface | Role::SupportTransition | Role::Brim) {
            m.object = u16::MAX;
        }
        m
    });
    let unlabelled = read_pieces(unlabelled, &r.stats, &pieces);
    assert_eq!(unlabelled.pieces[0].filament_mm, p.filament_mm, "unlabelled supports still belong to the nearby piece");
    assert_eq!(unlabelled.pieces[0].support_grams, p.support_grams, "their material is not shared");
}

#[test]
fn a_pieces_bounds_stand_where_it_prints() {
    let (r, pieces) = read_demo(true);
    let read = read_pieces(r.layers.records(), &r.stats, &pieces);
    let p = &read.pieces[0];
    let part = &pieces.parts[0];
    let (lo, hi) = part.cube.footprint();
    // Trees can lean and widen towards the bed; their brim starts at that support footprint.
    let job = prepare_job(&r.setup).unwrap();
    let keys = makepad_fab_toolpath::tree_told(&job).unwrap().keys;
    let support_reach = r.stats.max_z * (keys.branch_angle_deg.to_radians().tan() + keys.branch_diameter_angle_deg.to_radians().tan()) + keys.branch_diameter / 2.0 + keys.first_layer_gap;
    let reach = support_reach + job.f("brim_width") + job.nozzle_diameter(0) / 2.0;
    for k in 0..2 {
        assert!(p.bed_min[k] >= lo[k] + part.shift[k] - reach && p.bed_max[k] <= hi[k] + part.shift[k] + reach, "axis {k}: shifted bounds {:?} .. {:?}", p.bed_min, p.bed_max);
    }
    assert_eq!(p.layers.0, 0, "first printed layer");
    assert_eq!(p.layers.1 as usize, r.stats.layers - 1, "last printed layer");
    let (mut min, mut max) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    let (mut first_min, mut first_max) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for m in r.layers.records().filter(|m| m.object == 0 && m.kind == MoveKind::Extrude && m.e > 0.0) {
        for q in [m.p0, m.p1] {
            for k in 0..3 { min[k] = min[k].min(q[k] as f64); max[k] = max[k].max(q[k] as f64); }
            if m.layer == 0 {
                for k in 0..2 { first_min[k] = first_min[k].min(q[k] as f64); first_max[k] = first_max[k].max(q[k] as f64); }
            }
        }
    }
    for k in 0..3 {
        assert!(p.bed_min[k] <= min[k] && p.bed_max[k] >= max[k], "axis {k}: bounds contain every extrusion endpoint");
    }
    for k in 0..2 {
        assert!(p.footprint[k] >= first_max[k] - first_min[k] && p.footprint[k] <= p.bed_max[k] - p.bed_min[k], "axis {k}: first layer's footprint contains its paths and fits the print");
    }
    assert_eq!(p.bed_max[2], max[2], "height above the bed");
    assert_eq!(p.height_over_footprint, max[2] / p.footprint[0].min(p.footprint[1]), "height over the shorter side");
}

/// Each layer of a piece is that print's layer, cut to the cube, whatever the print's layers are:
/// a first layer unlike the others, adaptive ones, a field's.
#[test]
fn a_pieces_layers_are_the_prints_cut_to_its_cube() {
    let first_unlike: &[(&str, &str)] = &[("initial_layer_print_height", "0.3"), ("layer_height", "0.15")];
    let adaptive: &[(&str, &str)] = &[("fab_adaptive_layers", "1"), ("layer_height", "0.2")];
    let mut vase = plate();
    vase.parts.truncate(1);
    vase.parts[0].mesh = turned_vase(30.0);
    let (lo, hi) = (vase.parts[0].mesh.bounds().min, vase.parts[0].mesh.bounds().max);
    let on_vase = test("Rim", [(lo[0] + hi[0]) / 2.0 + 4.0, (lo[1] + hi[1]) / 2.0, 15.0], [14.0, 14.0, 10.0], 20.0);
    let (field, frames) = sdf_plate([150.0, 100.0, 0.0]);
    let mut on_field = LiveInput { frames: Some(Arc::new(frames)), ..whole(1, &field, first_unlike) };
    on_field.plate_id = 3;
    let cases = [
        (whole(1, &plate(), first_unlike), test("Stem and cap", [100.0, 100.0, 8.0], [14.0, 30.0, 5.0], 30.0), 0, Pt::default()),
        (whole(2, &vase, adaptive), on_vase, 0, Pt::default()),
        (on_field, test("Field", [146.0, 100.0, 3.0], [10.0, 12.0, 3.0], 0.0), 1, Pt::from_mm(150.0, 100.0)),
    ];
    for (input, t, part, off) in cases {
        let (e, rx) = engine();
        let (_, print) = settled_with_cuts(&e, &rx, 1, input.clone());
        let setup = (input.setup)(None).unwrap();
        let params = Params::from_job(&prepare_job(&setup).unwrap(), 0);
        let (_, own) = settled_with_cuts(&e, &rx, 2, pieces(&input, std::slice::from_ref(&t)));
        let (print, own) = (&print[&part], &own[&0]);
        let planes: Vec<f64> = print.iter().map(|l| makepad_fab_toolpath::cut_z(&params, l.0, l.1)).collect();
        let k = planes.partition_point(|z| *z < t.cube.bottom());
        let n = planes[k..].partition_point(|z| *z < t.cube.top());
        assert!(n >= 3, "{}: {n} layers", t.name);
        assert_eq!(own.len(), n, "{}: the layers whose planes lie in the cube", t.name);
        assert_eq!(own[0].1.to_bits(), params.first_layer_height.to_bits(), "{}: the bottom cut is the first layer", t.name);
        let outline = t.cube.outline_at(t.cube.center[2], off);
        for i in 0..n {
            if i > 0 {
                assert_eq!(own[i].1.to_bits(), print[k + i].1.to_bits(), "{}: layer {i} is as thick as the print's", t.name);
            }
            assert_eq!(*own[i].2.regions, *intersection_ex(&print[k + i].2.regions, &outline), "{}: layer {i} is the print's layer {} cut to the cube", t.name, k + i);
        }
        assert!(own.iter().any(|l| !l.2.regions.is_empty()), "{}: the piece has material", t.name);
    }
}

/// Compare the piece to the full print in this orientation, then check every model move
/// against the section at its layer's original cut plane.
fn assert_sections(input: LiveInput, t: TestPrint, part: usize, off: Pt) {
    let (e, rx) = engine();
    let (_, print) = settled_with_cuts(&e, &rx, 1, input.clone());
    let setup = (input.setup)(None).unwrap();
    let params = Params::from_job(&prepare_job(&setup).unwrap(), 0);
    let piece = pieces(&input, std::slice::from_ref(&t));
    let shift = piece.pieces.as_ref().unwrap().parts[0].shift;
    let (result, own) = settled_with_cuts(&e, &rx, 2, piece);
    let (print, own) = (&print[&part], &own[&0]);
    let planes: Vec<f64> = print.iter().map(|l| makepad_fab_toolpath::cut_z(&params, l.0, l.1)).collect();
    let k = planes.partition_point(|z| *z < t.cube.bottom());
    let n = planes[k..].partition_point(|z| *z < t.cube.top());
    // It prints from its lowest material: a turned cube's low corner may hang in the air.
    let empty = (k..k + n).take_while(|&i| intersection_ex(&print[i].2.regions, &t.cube.outline_at(planes[i], off)).is_empty()).count();
    let (k, n) = (k + empty, n - empty);
    assert!(n >= 3, "{}: {n} layers", t.name);
    assert_eq!(own.len(), n, "{}: the print's layers inside the cube, from its lowest material", t.name);
    assert_eq!(own[0].1.to_bits(), params.first_layer_height.to_bits(), "{}: the bottom cut is the first layer", t.name);
    for i in 0..n {
        if i > 0 {
            assert_eq!(own[i].1.to_bits(), print[k + i].1.to_bits(), "{}: layer {i} keeps its thickness", t.name);
        }
        let outline = t.cube.outline_at(planes[k + i], off);
        assert_eq!(*own[i].2.regions, *intersection_ex(&print[k + i].2.regions, &outline), "{}: layer {i} is the print's layer {} cut to its section", t.name, k + i);
    }
    assert!(own.iter().any(|l| !l.2.regions.is_empty()), "{}: the piece has material", t.name);
    let sections: Vec<_> = planes[k..k + n].iter().map(|z| t.cube.section(*z)).collect();
    let mut moves = 0;
    for m in result.layers.records().filter(is_model) {
        moves += 1;
        for q in [m.p0, m.p1] {
            let i = own.iter().position(|l| (l.0 - q[2] as f64).abs() < 1e-3).expect("model moves stay on a piece layer");
            let section = &sections[i];
            assert!(section.len() >= 3, "{}: material needs a section at layer {i}", t.name);
            let p = [q[0] as f64 - shift[0], q[1] as f64 - shift[1]];
            for (a, b) in section.iter().zip(section.iter().cycle().skip(1)) {
                let edge = [b[0] - a[0], b[1] - a[1]];
                let side = edge[0] * (p[1] - a[1]) - edge[1] * (p[0] - a[0]);
                assert!(side >= -1e-3 * edge[0].hypot(edge[1]), "{}: {:?} at {q:?} leaves layer {i}'s section", t.name, m.role);
            }
        }
    }
    assert!(moves > 20, "{}: {moves} model moves", t.name);
}

#[test]
fn a_turned_cube_cuts_each_layer_to_its_section() {
    let settings: &[(&str, &str)] = &[("initial_layer_print_height", "0.3"), ("layer_height", "0.15")];
    let (field, frames) = sdf_plate([150.0, 100.0, 0.0]);
    let cases = [
        (whole(1, &plate(), settings), test3("Turned stem and cap", [100.0, 100.0, 8.0], [14.0, 16.0, 5.0], [15.0, 20.0, 30.0]), 0, Pt::default()),
        (LiveInput { frames: Some(Arc::new(frames)), ..whole(2, &field, settings) }, test3("Turned field", [146.0, 100.0, 3.0], [8.0, 10.0, 3.0], [12.0, -18.0, 25.0]), 1, Pt::from_mm(150.0, 100.0)),
    ];
    for (input, t, part, off) in cases {
        assert_ne!(t.cube.section(t.cube.center[2]), t.cube.section(t.cube.center[2] + 1.0), "the section changes through the cube");
        assert_sections(input, t, part, off);
    }
}

#[test]
fn a_part_laid_flat_keeps_what_its_cube_cuts() {
    let turn = [90.0, 0.0, 0.0];
    let r = fab_model::eval::rotation_xyz(turn);
    let center = [100.0, 100.0, 3.0];
    let mut mesh = cube([-9.0, -3.0, -6.0], [18.0, 6.0, 12.0]);
    for q in &mut mesh.positions {
        *q = std::array::from_fn(|k| center[k] + r[k][0] * q[0] + r[k][1] * q[1] + r[k][2] * q[2]);
    }
    let mut p = plate();
    p.parts.truncate(1);
    p.parts[0].mesh = mesh;
    let t = test3("Laid flat", center, [12.0, 4.0, 8.0], turn);
    assert_sections(whole(1, &p, &[]), t, 0, Pt::default());
}

#[test]
fn cube_sections_and_reach_follow_its_body() {
    let upright = test("Upright", [12.3, 45.6, 7.8], [4.2, 6.4, 8.6], 37.0).cube;
    assert_eq!(upright.bottom().to_bits(), (upright.center[2] - upright.size[2] / 2.0).to_bits(), "upright bottom stays exact");
    assert_eq!(upright.top().to_bits(), (upright.center[2] + upright.size[2] / 2.0).to_bits(), "upright top stays exact");
    for z in [upright.bottom(), upright.center[2], upright.top()] {
        let section = upright.section(z);
        assert_eq!(section.len(), 4, "upright section has four corners");
        for (a, b) in section.iter().zip(upright.corners()) {
            assert_eq!(a.map(f64::to_bits), b.map(f64::to_bits), "upright section keeps the corner bits and order");
        }
    }
    let tilted = test3("Tilted", [0.0; 3], [2.0; 3], [45.0, 0.0, 0.0]).cube;
    let h = 2.0f64.sqrt();
    let expected = [[-1.0, -h], [1.0, -h], [1.0, h], [-1.0, h]];
    let section = tilted.section(0.0);
    assert_eq!(section.len(), 4, "the middle is a rectangle");
    for (a, b) in section.iter().zip(expected) {
        assert!((a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12, "middle corner {a:?}, expected {b:?}");
    }
    assert!(tilted.section(tilted.top() + 0.1).is_empty(), "nothing beyond the top");
    assert!(tilted.section(tilted.bottom() - 0.1).is_empty(), "nothing below the bottom");
    assert!(tilted.section(tilted.top()).is_empty(), "an edge has no section area");
    assert!(tilted.outline_at(tilted.top() + 0.1, Pt::default()).is_empty(), "no outline without a section");
    let miss = fab_plate::Bounds { min: [0.1, 1.1, 1.1], max: [0.3, 1.3, 1.3] };
    let (lo, hi) = tilted.footprint();
    assert!((0..2).all(|k| lo[k] < miss.max[k] && miss.min[k] < hi[k]) && tilted.bottom() < miss.max[2] && miss.min[2] < tilted.top(), "the bounding boxes overlap");
    assert!(!tilted.reaches(&miss), "the bodies miss");
    assert!(tilted.reaches(&fab_plate::Bounds { min: [-0.1; 3], max: [0.1; 3] }), "a box inside is reached");
}

/// The checks' last word on a settle.
fn last_report(rx: &mpsc::Receiver<LiveEvent>, revision: u64) -> Arc<fab_printability::Report> {
    loop {
        if let LiveEvent::Issues { revision: r, report, done, total } = rx.recv().expect("the checks finish") {
            if r >= revision && done == total {
                return report;
            }
        }
    }
}

/// Only its supports, brim and purge leave a piece's cube; what falls out of the cube's bottom
/// stands on the bed; its supports are grown for it, under the rules: nothing on thin air, none
/// a dead end.
#[test]
fn a_piece_prints_inside_its_cube_on_its_own_supports() {
    let settings: &[(&str, &str)] = &[("enable_support", "1"), ("support_type", "tree(auto)"), ("brim_type", "outer_only"), ("brim_width", "3")];
    let input = whole(1, &plate(), settings);
    let t = test("Under the cap", [100.0, 100.0, 8.0], [20.0, 20.0, 8.0], 0.0);
    let piece = pieces(&input, std::slice::from_ref(&t));
    let shift = piece.pieces.as_ref().unwrap().parts[0].shift;
    let (e, rx) = engine();
    let (r, cuts) = settled_with_cuts(&e, &rx, 1, piece);
    let (revision, _) = e.revisions();
    let report = last_report(&rx, revision);
    let (lo, hi) = t.cube.footprint();
    let top = cuts[&0].last().unwrap().0;
    let moves: Vec<MoveRecord> = r.layers.records().filter(is_model).collect();
    assert!(moves.len() > 100, "{} model moves", moves.len());
    for m in &moves {
        for q in [m.p0, m.p1] {
            let (x, y) = (q[0] as f64 - shift[0], q[1] as f64 - shift[1]);
            assert!(x > lo[0] - 1e-3 && x < hi[0] + 1e-3 && y > lo[1] - 1e-3 && y < hi[1] + 1e-3, "{:?} at {q:?} leaves the cube", m.role);
            assert!((q[2] as f64) < top + 1e-3, "{:?} at {q:?} over the piece's top {top}", m.role);
        }
    }
    let lowest = moves.iter().map(|m| m.p0[2]).fold(f32::MAX, f32::min);
    assert!((lowest as f64 - cuts[&0][0].0).abs() < 1e-3, "the first layer is on the bed: {lowest}");
    assert!(r.layers.records().any(|m| m.kind == MoveKind::Extrude && m.role == Role::Support), "the cap over the stem is supported");
    assert!(r.layers.records().any(|m| m.kind == MoveKind::Extrude && m.role == Role::Brim), "the profile's brim");
    let bad: Vec<&str> = report.issues.iter().filter(|i| ["support_rule", "mid_air", "support_unreached", "support_inside"].contains(&i.kind)).map(|i| i.kind).collect();
    assert!(bad.is_empty(), "the rules hold on a piece: {bad:?}");
}

/// A settings box over a part, in the plate's frame.
fn settings_box(lo: [f64; 3], hi: [f64; 3], values: &[(&str, &str)]) -> fab_plate::SpatialPaint {
    let at = move |q: [f64; 3]| (0..3).map(|k| (lo[k] - q[k]).max(q[k] - hi[k])).fold(f64::MIN, f64::max);
    fab_plate::SpatialPaint {
        sample_at: Arc::new(|_| fab_plate::PaintSample { slot: 0, dist: 1e9, mix: None }),
        support_block: None,
        support_enforce: None,
        no_trees: None,
        settings: vec![fab_plate::SettingsRegion { region: fab_plate::SupportRegion { at: Arc::new(at), bounds: Some((lo, hi)) }, values: values.iter().map(|&(k, v)| (k.to_string(), v.to_string())).collect() }],
        colors: vec![fab_plate::paint::PaintColor { rgb: [1.0; 3], color: None, filament: None, nozzle: None }],
        key: lo.iter().chain(&hi).fold(0xcbf2_9ce4_8422_2325u64, |h, v| (h ^ v.to_bits()).wrapping_mul(0x100_0000_01b3)) | 1,
        no_trees_key: 0,
        regions_key: 0,
        source: Arc::new(NoTreesBox),
    }
}

/// Printed length of a role, inside or outside a box of X over the layers `z` holds.
fn length(r: &Settled, role: Role, x: [f64; 2], inside: bool, z: [f64; 2]) -> f64 {
    r.layers.records().filter(|m| m.kind == MoveKind::Extrude && m.role == role && (z[0]..z[1]).contains(&(m.p0[2] as f64))).filter(|m| {
        let mid = (m.p0[0] + m.p1[0]) as f64 / 2.0;
        (x[0]..x[1]).contains(&mid) == inside
    }).map(|m| ((m.p1[0] - m.p0[0]) as f64).hypot((m.p1[1] - m.p0[1]) as f64)).sum()
}

/// Printed length of a role over the layers `z` holds, inside a window of the plate.
fn length_in(r: &Settled, role: Role, lo: [f64; 2], hi: [f64; 2], z: [f64; 2]) -> f64 {
    r.layers.records().filter(|m| m.kind == MoveKind::Extrude && m.role == role && (z[0]..z[1]).contains(&(m.p0[2] as f64))).map(|m| {
        let (a, d) = ([m.p0[0] as f64, m.p0[1] as f64], [(m.p1[0] - m.p0[0]) as f64, (m.p1[1] - m.p0[1]) as f64]);
        // the part of the segment in the window, by its parameter
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        for k in 0..2 {
            if d[k].abs() < 1e-9 {
                if a[k] < lo[k] || a[k] > hi[k] {
                    return 0.0;
                }
            } else {
                let (u, v) = ((lo[k] - a[k]) / d[k], (hi[k] - a[k]) / d[k]);
                t0 = t0.max(u.min(v));
                t1 = t1.min(u.max(v));
            }
        }
        (t1 - t0).max(0.0) * d[0].hypot(d[1])
    }).sum()
}

/// A settings box comes from the original: where it covers the cube the piece prints as those
/// points do in the full print, a box clear of the cube leaves the piece's G-code as it is to the
/// byte, and a box edited on the original reaches the piece with nothing done to the test.
#[test]
fn a_pieces_settings_boxes_come_from_the_original() {
    let settings: &[(&str, &str)] = &[("sparse_infill_density", "15%"), ("sparse_infill_pattern", "grid")];
    let block = |paint: Option<fab_plate::SpatialPaint>| {
        let mut p = plate();
        p.parts.truncate(1);
        p.parts[0].mesh = cube([100.0, 100.0, 0.0], [40.0, 40.0, 30.0]);
        p.parts[0].paint = paint;
        p
    };
    let t = test("Middle", [120.0, 120.0, 15.0], [24.0, 24.0, 16.0], 0.0);
    let piece_of = |p: &Plate, id: u64| {
        let (e, rx) = engine();
        let input = whole(id, p, settings);
        let test_plate = pieces(&input, std::slice::from_ref(&t));
        let shift = test_plate.pieces.as_ref().unwrap().parts[0].shift;
        e.settle(1, test_plate);
        (settled(&rx, 1), shift)
    };
    let solid = &[("sparse_infill_density", "100%")];
    let (plain, shift) = piece_of(&block(None), 1);
    let (clear, _) = piece_of(&block(Some(settings_box([101.0, 101.0, 0.0], [106.0, 139.0, 30.0], solid))), 2);
    assert!(plain.gcode == clear.gcode, "a box clear of the cube changes nothing of the piece");
    let (x0, x1) = (120.0, 140.0);
    let half = settings_box([x0, 100.0, 0.0], [x1, 140.0, 30.0], solid);
    let (partly, _) = piece_of(&block(Some(half.clone())), 3);
    // Its middle layers, away from the skins of the cuts, on the side the box covers and the other.
    let in_piece = [shift[0] + x0, shift[0] + x1];
    let z = [5.0, 11.0];
    assert!(length(&partly, Role::InternalSolidInfill, in_piece, true, z) > 500.0, "the box's half prints solid");
    assert_eq!(length(&partly, Role::SparseInfill, in_piece, true, z), 0.0, "and holds no sparse infill");
    assert!(length(&partly, Role::SparseInfill, in_piece, false, z) > 50.0, "the other half keeps the sparse infill");
    assert_eq!(length(&partly, Role::InternalSolidInfill, in_piece, false, z), 0.0, "and no solid");
    // The same model points in the full print: as much solid per layer there as in the piece.
    let (e, rx) = engine();
    e.settle(1, whole(3, &block(Some(half)), settings));
    let full = settled(&rx, 1);
    let drop = 7.0;
    // Well inside the cube and the box: the walls of the cut take the piece's edge.
    let (lo, hi) = ([122.0, 111.0], [130.0, 129.0]);
    let piece_len = length_in(&partly, Role::InternalSolidInfill, [lo[0] + shift[0], lo[1] + shift[1]], [hi[0] + shift[0], hi[1] + shift[1]], z);
    let print_len = length_in(&full, Role::InternalSolidInfill, lo, hi, [z[0] + drop, z[1] + drop]);
    assert!(print_len > 100.0, "the box prints solid there: {print_len}");
    assert!((piece_len - print_len).abs() < 0.1 * print_len, "as much solid at the same points: {piece_len} in the piece, {print_len} in the print");
    // Moved on the original, the piece follows.
    let (moved, _) = piece_of(&block(Some(settings_box([125.0, 100.0, 0.0], [140.0, 140.0, 30.0], solid))), 4);
    assert!(moved.gcode != partly.gcode, "the piece follows the box");
}

/// The pieces of one job stand apart, whatever they are, and each prints as it does alone, apart
/// from where it stands; on the plate's fixed layers and on adaptive ones, where they share print
/// heights unevenly.
#[test]
fn test_prints_stand_apart_and_print_as_they_do_alone() {
    let mut vase = plate();
    vase.parts[0].mesh = turned_vase(30.0);
    let b = vase.parts[0].mesh.bounds();
    let c = [(b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0];
    let cases: [(Plate, &[(&str, &str)], [TestPrint; 2]); 2] = [
        (plate(), &[("enable_support", "1"), ("support_type", "tree(auto)")], [test("Cap", [100.0, 100.0, 8.0], [18.0, 18.0, 8.0], 15.0), test("Box", [205.0, 106.0, 3.0], [8.0, 8.0, 4.0], 0.0)]),
        (vase, &[("fab_adaptive_layers", "1"), ("layer_height", "0.2")], [test("Rim", [c[0], c[1], 25.0], [20.0, 20.0, 8.0], 0.0), test("Box", [205.0, 106.0, 3.0], [8.0, 8.0, 4.0], 0.0)]),
    ];
    for (plate, settings, tests) in cases {
        let input = whole(1, &plate, settings);
        let (e, rx) = engine();
        let both = pieces(&input, &tests);
        let shifts: Vec<[f64; 2]> = both.pieces.as_ref().unwrap().parts.iter().map(|p| p.shift).collect();
        e.settle(1, both);
        let r = settled(&rx, 1);
        for (o, t) in tests.iter().enumerate() {
            let alone_input = pieces(&input, std::slice::from_ref(t));
            let alone_shift = alone_input.pieces.as_ref().unwrap().parts[0].shift;
            let (ae, arx) = engine();
            ae.settle(1, alone_input);
            let alone = settled(&arx, 1);
            let d = printed(&r, o as u16, shifts[o]);
            assert!(d.len() > 20, "{}: {} moves", t.name, d.len());
            let own = printed(&alone, 0, alone_shift);
            // The G-code's third decimal falls apart with where it stands: nothing nearer counts.
            let near = |a: &(u8, [i64; 6]), b: &(u8, [i64; 6])| a.0 == b.0 && a.1.iter().zip(&b.1).all(|(x, y)| (x - y).abs() <= 1);
            let apart = |a: &[(u8, [i64; 6])], b: &[(u8, [i64; 6])]| a.iter().filter(|m| !b.iter().any(|n| near(m, n))).take(6).copied().collect::<Vec<_>>();
            let (here, there) = (apart(&d, &own), apart(&own, &d));
            assert!(d.len() == own.len() && here.is_empty() && there.is_empty(), "{} prints as it does alone: {} moves against {}, these only here {here:?}, these only alone {there:?}", t.name, d.len(), own.len());
        }
        // Their prints' boxes, brims and supports with them, never meet.
        let of = |o: u16| {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for m in r.layers.records().filter(|m| m.object == o && m.kind == MoveKind::Extrude && m.role != Role::PrimeTower && m.role != Role::Flush) {
                for q in [m.p0, m.p1] {
                    for k in 0..2 {
                        lo[k] = lo[k].min(q[k]);
                        hi[k] = hi[k].max(q[k]);
                    }
                }
            }
            (lo, hi)
        };
        let (a, b) = (of(0), of(1));
        assert!(a.1[0] < b.0[0] || b.1[0] < a.0[0] || a.1[1] < b.0[1] || b.1[1] < a.0[1], "they stand apart: {a:?} {b:?}");
    }
}

/// A cube over two parts is one piece: one test, one shift, the parts where they are to each other.
#[test]
fn a_cube_over_two_parts_is_one_block() {
    let mut p = plate();
    p.parts[0].mesh = cube([100.0, 100.0, 0.0], [10.0, 20.0, 6.0]);
    p.parts[1].mesh = cube([112.0, 100.0, 0.0], [10.0, 20.0, 6.0]);
    let third = Part { object: "third".into(), name: "third".into(), mesh: cube([200.0, 100.0, 0.0], [10.0, 10.0, 6.0]), ..p.parts[1].clone() };
    p.parts.push(third);
    let input = whole(1, &p, &[]);
    let tests = [test("Across", [111.0, 110.0, 3.0], [20.0, 10.0, 4.0], 0.0), test("Other", [205.0, 105.0, 3.0], [6.0, 6.0, 4.0], 0.0)];
    let plate_of = pieces(&input, &tests);
    let list = plate_of.pieces.clone().unwrap();
    assert_eq!(list.parts.iter().map(|q| q.test).collect::<Vec<_>>(), [0, 0, 1], "one test for what one cube reaches");
    assert_eq!(list.parts[0].shift, list.parts[1].shift, "one shift");
    let (e, rx) = engine();
    e.settle(1, plate_of);
    let r = settled(&rx, 1);
    let bounds = |o: u16| r.layers.records().filter(|m| m.object == o && is_model(m)).flat_map(|m| [m.p0[0], m.p1[0]]).fold((f32::MAX, f32::MIN), |b, x| (b.0.min(x), b.1.max(x)));
    let (a, b) = (bounds(0), bounds(1));
    assert!((b.0 - a.1) > 2.0 && (b.0 - a.1) < 3.5, "two parts 2 mm apart stay so: {a:?} {b:?}");
}

#[test]
fn test_prints_of_two_materials_change_tools_as_the_plate_does() {
    // Like plate::tests' tower cases: two filament slots on the same nozzle.
    let mut setup = job(&[("enable_prime_tower", "1"), ("brim_type", "no_brim"), ("skirt_loops", "0")]);
    setup.filament_map = vec![1, 1];
    for (filament, density) in setup.filaments.iter_mut().zip(["1.24", "1.27"]) {
        filament.values.set_vec("filament_density", vec![density.into()]);
    }
    let mut p = plate();
    p.parts[0].mesh = cube([100.0, 100.0, 0.0], [8.0, 8.0, 0.6]);
    p.parts[1].mesh = cube([108.0, 100.0, 0.0], [8.0, 8.0, 0.6]);
    // Part and move filament IDs are one-based; read_pieces' slots are zero-based.
    p.parts[0].filament = 1;
    p.parts[1].filament = 2;
    let input = LiveInput::ready(1, 1, setup, Arc::new(p), Arc::new(Vec::new()));
    let tests = [
        test("Across", [108.0, 104.0, 0.3], [8.0, 6.0, 0.6], 0.0),
        test("First only", [102.0, 104.0, 0.3], [3.0, 6.0, 0.6], 0.0),
    ];
    // Read the writer's layer boundaries, just as its tool-change tests do.
    let tools_by_layer = |r: &Settled| -> Vec<(f64, Vec<u8>)> {
        r.gcode.text().unwrap().split("; CHANGE_LAYER\n").skip(1).map(|layer| {
            let z = layer.lines().find_map(|line| line.strip_prefix("; Z_HEIGHT: "))
                .expect("each G-code layer states its height").parse().expect("numeric layer height");
            let tools = layer.lines().filter_map(|line| {
                line.split(';').next().expect("a G-code line").split_whitespace().next()
                    .and_then(|word| word.strip_prefix('T')).map(|tool| tool.parse().expect("numeric tool selection"))
            }).collect();
            (z, tools)
        }).collect()
    };
    let tower_moves = |r: &Settled| r.layers.records()
        .filter(|m| m.kind == MoveKind::Extrude && m.role == Role::PrimeTower && m.e > 0.0).count();
    let (e, rx) = engine();
    e.settle(1, input.clone());
    let full = settled(&rx, 1);
    let full_tools = tools_by_layer(&full);
    assert!(!full_tools.is_empty(), "the whole plate prints layers");
    assert!(full_tools.iter().flat_map(|(_, tools)| tools).any(|&t| t == 0)
        && full_tools.iter().flat_map(|(_, tools)| tools).any(|&t| t == 1), "the whole plate selects both tools: {full_tools:?}");
    assert!(tower_moves(&full) > 0, "the whole plate honours the enabled prime tower");
    for count in 1..=2 {
        let own = pieces(&input, &tests[..count]);
        let list = own.pieces.clone().expect("a test plate has pieces");
        assert_eq!(list.parts.iter().map(|part| part.test).collect::<Vec<_>>(),
            if count == 1 { vec![0, 0] } else { vec![0, 0, 1] }, "one cube reaches both materials; the second reaches only part 0");
        e.settle(count as u64 + 1, own);
        let r = settled(&rx, count as u64 + 1);
        let tools = tools_by_layer(&r);
        let read = read_pieces(r.layers.records(), &r.stats, &list);
        assert_eq!(tools, full_tools, "{count} pieces keep the whole plate's tool-change count, sequence and height on every layer");
        assert_eq!(read.pieces.len(), count, "all ticked cubes print in one job");
        assert!(read.pieces[0].filament_mm[0] > 0.0 && read.pieces[0].filament_mm[1] > 0.0,
            "the crossing cube reads filament on both slots: {:?}", read.pieces[0].filament_mm);
        if count == 2 {
            assert!(read.pieces[1].filament_mm[0] > 0.0, "the second cube prints slot 0");
            assert_eq!(read.pieces[1].filament_mm[1], 0.0, "the second cube uses no slot 1 filament");
        }
        for (layer, (z, _)) in tools.iter().enumerate() {
            for piece in 0..count {
                let mut slots = Vec::new();
                for m in r.layers.records().filter(|m| m.layer as usize == layer && is_model(m)) {
                    if list.parts.get(m.object as usize).is_some_and(|part| part.test == piece) && slots.last() != Some(&m.filament) {
                        slots.push(m.filament);
                    }
                }
                if piece == 0 {
                    assert!(slots.contains(&1) && slots.contains(&2) && slots.len() > 1,
                        "crossing piece changes material on layer {layer} at {z}: {slots:?}");
                } else {
                    assert_eq!(slots, [1], "second piece prints only slot 0 on layer {layer} at {z}");
                }
            }
            for role in [Role::PrimeTower, Role::Flush] {
                let has = |result: &Settled| result.layers.records().any(|m|
                    m.layer as usize == layer && m.role == role && m.kind == MoveKind::Extrude && m.e > 0.0);
                assert_eq!(has(&r), has(&full), "{role:?} handling matches the whole plate on layer {layer} at {z}");
            }
        }
        let mut prime_seconds = 0.0;
        let mut prime_grams = 0.0;
        for m in r.layers.records().filter(|m| (m.kind == MoveKind::Extrude && matches!(m.role, Role::PrimeTower | Role::Flush))
            || m.flags & makepad_fab_gcode::record::flags::PURGE != 0) {
            assert_eq!(m.object, u16::MAX, "prime/purge belongs to no piece: {m:?}");
            prime_seconds += m.t_end - m.t_start;
            if m.filament > 0 {
                let slot = m.filament as usize - 1;
                prime_grams += m.e as f64 * r.stats.filament_g[slot] / r.stats.filament_mm[slot];
            }
        }
        assert!(prime_seconds > 0.0 && prime_grams > 0.0, "the test job spends time and material priming: {prime_seconds} s, {prime_grams} g");
        assert!(read.shared_seconds + 1e-6 >= prime_seconds, "prime/purge time is shared: {} s for {prime_seconds} s", read.shared_seconds);
        assert!(read.shared_grams + 1e-6 >= prime_grams, "prime/purge material is shared: {} g for {prime_grams} g", read.shared_grams);
        for piece in &read.pieces {
            for (slot, &mm) in piece.filament_mm.iter().enumerate() {
                let expected: f64 = r.layers.records().filter(|m| m.filament as usize == slot + 1
                    && list.parts.get(m.object as usize).is_some_and(|part| part.test == piece.test)).map(|m| m.e as f64).sum();
                assert!((mm - expected).abs() < 1e-9, "piece {} slot {slot} counts only its own moves, excluding shared prime/purge", piece.test);
            }
        }
        assert!((read.pieces.iter().map(|piece| piece.seconds).sum::<f64>() + read.shared_seconds - read.total_seconds).abs() < 1e-6,
            "piece and shared times add up to the job");
        assert!((read.pieces.iter().map(|piece| piece.grams).sum::<f64>() + read.shared_grams - read.total_grams).abs() < 1e-6,
            "piece and shared grams add up to the job");
    }
}

/// Moving a cube cancels the slice of where it was, off the running revision.
#[test]
fn a_cube_edit_cancels_the_running_slice() {
    let (grew, first) = mpsc::channel::<()>();
    let (go, wait) = mpsc::channel::<()>();
    let held = Mutex::new(Some((grew, wait)));
    let (e, rx) = engine_seeing(move |ev| {
        if matches!(ev, LiveEvent::PartLayers { part: 0, .. }) {
            if let Some((grew, wait)) = held.lock().unwrap().take() {
                grew.send(()).unwrap();
                wait.recv().expect("the edit is in");
            }
        }
    });
    let input = whole(1, &plate(), &[]);
    let at = |z: f64| pieces(&input, &[test("Stem", [100.0, 100.0, z], [20.0, 20.0, 4.0], 0.0)]);
    e.set_input(at(6.0));
    first.recv().expect("the slice is running");
    let (old, _) = e.revisions();
    e.settle(2, at(7.0));
    go.send(()).unwrap();
    let r = loop {
        match rx.recv().expect("a settled slice") {
            LiveEvent::Toolpaths { revision, .. } => assert!(revision != old, "the moved cube's old slice goes no further"),
            LiveEvent::Settled { tag: 2, result, .. } => break result.expect("sliced"),
            LiveEvent::Failed { error, .. } => panic!("{error}"),
            _ => {}
        }
    };
    let (fe, frx) = engine();
    fe.settle(1, at(7.0));
    assert!(r.gcode == settled(&frx, 1).gcode, "what is left is a fresh engine's slice of the new cube");
}

/// An input held is in the caller's hands: nothing slices it, and it stands for its own identity
/// alone. Any other input slices as ever, whether its plate or only its settings differ, and a
/// part moved on top of the held one starts from it.
#[test]
fn a_held_input_slices_nothing_and_any_other_input_slices() {
    let plate = plate();
    const THIN: &[(&str, &str)] = &[("layer_height", "0.2")];
    const THICK: &[(&str, &str)] = &[("layer_height", "0.3")];
    let made = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let make = |id: u64, settings_id: u64, settings: &'static [(&'static str, &'static str)]| -> MakeInput {
        let (plate, made) = (plate.clone(), made.clone());
        Arc::new(move || {
            made.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            LiveInput::ready(id, settings_id, job(settings), Arc::new(plate.clone()), Arc::new(Vec::new()))
        })
    };
    let count = || made.load(std::sync::atomic::Ordering::SeqCst);
    let (e, rx) = engine();
    let parts = plate.parts.len();
    let slices = |rx: &mpsc::Receiver<LiveEvent>, revision: u64| loop {
        match rx.recv().expect("it slices") {
            LiveEvent::Toolpaths { revision: r, .. } if r >= revision => return,
            LiveEvent::Failed { error, .. } => panic!("{error}"),
            _ => {}
        }
    };
    let quiet = |e: &LiveEngine, rx: &mpsc::Receiver<LiveEvent>, what: &str| {
        e.wait_idle();
        for ev in rx.try_iter() {
            assert!(!matches!(ev, LiveEvent::Started { .. } | LiveEvent::Toolpaths { .. }), "{what} slices nothing");
        }
    };

    // Held: nothing is built or sliced, and only its own identity reads as the engine's input.
    let held = e.hold_input(1, 1, parts, make(1, 1, THIN));
    quiet(&e, &rx, "a held input");
    assert_eq!(count(), 0, "a held input is not built");
    assert_eq!(e.input_revision(1, 1), Some(held), "the held input is the engine's");
    assert_eq!(e.input_revision(1, 2), None, "other settings are not the held input");
    assert_eq!(e.input_revision(2, 1), None, "another plate is not the held input");

    // Only the settings differ: it slices, with those settings.
    let revision = e.set_input_with(1, 2, parts, make(1, 2, THICK));
    slices(&rx, revision);
    assert_eq!(count(), 1, "other settings build their input");
    assert_eq!(e.input_revision(1, 1), None, "the held input went");
    assert_eq!(e.input_revision(1, 2), Some(revision));
    e.wait_idle();
    let _ = rx.try_iter().count();

    // A settle of other settings from a held input is that settings' slice, not the held one's.
    e.hold_input(1, 1, parts, make(1, 1, THIN));
    quiet(&e, &rx, "a held input");
    e.settle_with(5, 1, 2, parts, make(1, 2, THICK));
    let thick_slice = settled(&rx, 5);
    let (fresh, fresh_rx) = engine();
    fresh.settle(1, LiveInput::ready(1, 2, job(THICK), Arc::new(plate.clone()), Arc::new(Vec::new())));
    assert!(thick_slice.gcode == settled(&fresh_rx, 1).gcode, "a settle from a held input is a fresh engine's slice");
    let (thin_engine, thin_rx) = engine();
    thin_engine.settle(1, LiveInput::ready(1, 1, job(THIN), Arc::new(plate.clone()), Arc::new(Vec::new())));
    assert!(thick_slice.gcode != settled(&thin_rx, 1).gcode, "the settings tell the slices apart");
    e.wait_idle();
    let _ = rx.try_iter().count();

    // Only the plate differs: it slices.
    e.hold_input(1, 1, parts, make(1, 1, THIN));
    quiet(&e, &rx, "a held input");
    let revision = e.set_input_with(2, 1, parts, make(2, 1, THIN));
    slices(&rx, revision);
    assert_eq!(e.input_revision(1, 1), None, "the held input went");
    e.wait_idle();
    let _ = rx.try_iter().count();

    // A part moved on top of the held input starts from it.
    let before = count();
    e.hold_input(1, 1, parts, make(1, 1, THIN));
    quiet(&e, &rx, "a held input");
    assert_eq!(count(), before, "a held input is not built");
    e.set_transform(0, [[1.0, 0.0, 0.0, 2.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]);
    e.wait_idle();
    assert_eq!(count(), before + 1, "a moved part builds the held input");
    assert!(rx.try_iter().any(|ev| matches!(ev, LiveEvent::Started { .. })), "a moved part slices");
    assert_eq!(e.input_revision(1, 1), None, "with a part moved on top, it is not the held input any more");

    // Cancelled, nothing is held.
    e.release();
    e.wait_idle();
    let _ = rx.try_iter().count();
    e.hold_input(1, 1, parts, make(1, 1, THIN));
    e.cancel();
    quiet(&e, &rx, "a cancelled input");
    e.set_transform(0, [[1.0, 0.0, 0.0, 2.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]);
    quiet(&e, &rx, "a part moved on nothing");
    assert_eq!(e.input_revision(1, 1), None, "a cancelled hold is gone");
}

/// A cube moved over layers already cut cuts nothing of the whole part again; a test plate keeps
/// the very plate it is cut from, so switching back slices nothing and writes the same G-code,
/// and an edited plate lets the old one go: its plans are earlier states then, which newer ones
/// age out.
#[test]
fn test_plates_keep_only_the_plate_they_are_cut_from() {
    let p = plate();
    let input = whole(1, &p, &[]);
    let t = |x: f64, z: f64| [test("Stem", [100.0 + x, 100.0, z], [20.0, 20.0, 4.0], 0.0)];
    {
        let (e, rx) = engine();
        e.settle(1, pieces(&input, &t(0.0, 6.0)));
        settled(&rx, 1);
        let cut = e.planes_cut();
        assert!(cut > 0, "the planes of its layers are cut");
        assert!(cut <= 25, "only those: {cut}");
        e.settle(2, pieces(&input, &t(1.5, 6.0)));
        settled(&rx, 2);
        assert_eq!(e.planes_cut(), cut, "moved aside, it cuts no plane again");
    }
    let (e, rx) = engine();
    e.settle(1, input.clone());
    let full = settled(&rx, 1);
    e.settle(2, pieces(&input, &t(0.0, 6.0)));
    settled(&rx, 2);
    assert_eq!(e.planes_cut(), 0, "the full print's cuts are its planes");
    for (i, x) in [1.0, 2.0, 3.0].iter().enumerate() {
        e.settle(3 + i as u64, pieces(&input, &t(*x, 6.0)));
        settled(&rx, 3 + i as u64);
    }
    let (planned, traced) = (e.shared.caches.lock().unwrap().plans.len(), e.traced_count());
    assert!(planned >= p.parts.len(), "the plate's plans are kept: {planned}");
    let keys = e.shared.caches.lock().unwrap().whole_plate.as_ref().map(|w| w.1.clone()).expect("the plate it is cut from");
    let work = e.part_work();
    e.settle(10, input.clone());
    let back = settled(&rx, 10);
    assert!(back.gcode == full.gcode, "the same G-code back");
    assert_eq!(e.traced_count(), traced, "switching back traces nothing");
    assert_eq!(e.part_work().iter().map(|w| w.1).sum::<u64>(), work.iter().map(|w| w.1).sum::<u64>(), "and plans nothing");
    // An edit of the plate: a test plate of the edit lets the old plate go.
    let mut edited = p.clone();
    edited.parts[1].mesh = cube([200.0, 100.0, 0.0], [16.0, 12.0, 6.0]);
    let next = whole(2, &edited, &[]);
    e.settle(11, pieces(&next, &t(0.0, 6.0)));
    settled(&rx, 11);
    e.settle(12, pieces(&next, &t(1.0, 6.0)));
    settled(&rx, 12);
    assert!(e.shared.caches.lock().unwrap().whole_plate.is_none(), "no superseded plate is held");
    for k in 0..=KEPT_STATES as u64 {
        e.settle(13 + k, pieces(&next, &t(2.25 + k as f64 * 0.5, 6.0)));
        settled(&rx, 13 + k);
    }
    let held = e.shared.caches.lock().unwrap().plans.iter().filter(|e| keys.contains(&e.0 .0)).count();
    assert_eq!(held, 0, "the old plate's plans age out as earlier states: {held} of them held past {} newer", KEPT_STATES + 1);
}

/// A whole print using the engine's existing live transform, with no piece source.
fn posed_whole(input: LiveInput, part: usize, xf: Affine) -> (Arc<Settled>, HashMap<usize, Arc<CutLayers>>) {
    let (e, rx) = engine();
    e.set_enabled(false);
    e.settle(1, input);
    e.touch(|s| {
        s.xf[part] = Some(xf);
        s.enabled = true;
        None
    });
    let mut cuts = HashMap::new();
    loop {
        match rx.recv().expect("the posed whole print finishes") {
            LiveEvent::PartCut { part, layers, stale, .. } if stale.is_empty() => { cuts.insert(part, layers); }
            LiveEvent::Settled { tag: 1, result, .. } => return (result.expect("posed whole sliced"), cuts),
            LiveEvent::Failed { error, .. } => panic!("{error}"),
            _ => {}
        }
    }
}

#[test]
fn posed_pieces_cut_the_posed_whole_and_drop_to_material() {
    let settings: &[(&str, &str)] = &[("initial_layer_print_height", "0.3"), ("layer_height", "0.2"), ("brim_type", "no_brim"), ("skirt_loops", "0")];
    let mut mesh = plate();
    mesh.parts.truncate(1);
    mesh.parts[0].mesh = cube([96.0, 96.0, 4.0], [8.0, 8.0, 4.0]);
    let (mut field, frames) = sdf_plate([150.0, 100.0, 0.0]);
    field.parts[1].field_bounds = Some(field.parts[1].mesh.bounds());
    let cases = [
        (whole(1, &mesh, settings), test3("Posed mesh", [100.0, 100.0, 6.0], [14.0, 14.0, 12.0], [10.0, 15.0, 20.0]), 0, Pt::default()),
        (LiveInput { frames: Some(Arc::new(frames)), ..whole(2, &field, settings) }, test3("Posed field", [146.0, 100.0, 3.0], [14.0, 28.0, 16.0], [12.0, -18.0, 25.0]), 1, Pt::from_mm(150.0, 100.0)),
    ];
    for (input, mut t, part, off) in cases {
        // A broad face at first material makes the first contour printable as well as nonempty.
        t.pose = Some(fab_model::eval::rotation_xyz([90.0, 0.0, 0.0]));
        let piece = pieces(&input, std::slice::from_ref(&t));
        let list = piece.pieces.clone().unwrap();
        let p = &list.parts[0];
        let xf = p.xf.unwrap();
        assert!(p.cube.bottom().abs() < 1e-12, "the posed cube stands on the bed");
        for (a, b) in t.cube.corners3().iter().zip(p.cube.corners3()) {
            for k in 0..3 {
                let q = xf[k][0] * a[0] + xf[k][1] * a[1] + xf[k][2] * a[2] + xf[k][3];
                assert!((q - b[k]).abs() < 1e-10, "the rigid map carries every cube corner");
            }
        }
        let (_, print) = posed_whole(input.clone(), part, xf);
        let params = Params::from_job(&prepare_job(&(input.setup)(None).unwrap()).unwrap(), 0);
        let print = &print[&part];
        let candidate: Vec<_> = print.iter().filter_map(|l| {
            let z = makepad_fab_toolpath::cut_z(&params, l.0, l.1);
            (z >= p.cube.bottom() && z < p.cube.top()).then(|| (l, z, intersection_ex(&l.2.regions, &p.cube.outline_at(z, off))))
        }).collect();
        let first = candidate.iter().position(|(_, _, r)| !r.is_empty()).expect("material in the cube");
        assert!(first > 0, "the posed cube starts below its material");
        let (e, rx) = engine();
        let (result, own) = settled_with_cuts(&e, &rx, 1, piece);
        let own = &own[&0];
        assert_eq!(own.len(), candidate.len() - first, "only leading empty planes are dropped");
        assert_eq!(e.planes_cut() as usize, candidate.len(), "only cube planes, each cut once");
        assert_eq!(own[0].1.to_bits(), params.first_layer_height.to_bits(), "material starts at first-layer height");
        for (i, (l, _, clipped)) in candidate[first..].iter().enumerate() {
            assert_eq!(*own[i].2.regions, **clipped, "{}: posed whole contours at layer {i}", t.name);
            if i > 0 { assert_eq!(own[i].1.to_bits(), l.1.to_bits(), "posed layer thickness"); }
        }
        let mut moves = 0;
        for m in result.layers.records().filter(is_model) {
            moves += 1;
            for q in [m.p0, m.p1] {
                let i = own.iter().position(|l| (l.0 - q[2] as f64).abs() < 1e-3).expect("a planar piece layer");
                let section = p.cube.section(candidate[first + i].1);
                let point = [q[0] as f64 - p.shift[0], q[1] as f64 - p.shift[1]];
                for (a, b) in section.iter().zip(section.iter().cycle().skip(1)) {
                    let d = [b[0] - a[0], b[1] - a[1]];
                    assert!(d[0] * (point[1] - a[1]) - d[1] * (point[0] - a[0]) >= -1e-3 * d[0].hypot(d[1]), "model moves stay in the posed section");
                }
            }
        }
        assert!(moves > 20, "a material piece");
        let read = read_pieces(result.layers.records(), &result.stats, &list);
        assert_eq!(read.pieces[0].layers.0, 0, "material is dropped to the first layer");
        assert!(read.pieces[0].filament_mm.iter().sum::<f64>() > 0.0, "posed moves retain their object's filament");
        let (lo, hi) = p.cube.footprint();
        for k in 0..2 {
            assert!(read.pieces[0].bed_min[k] >= lo[k] + p.shift[k] - 1e-3 && read.pieces[0].bed_max[k] <= hi[k] + p.shift[k] + 1e-3, "read bounds use the posed bed frame");
        }
    }
}

#[test]
fn posed_settings_follow_model_points_and_none_restores_bytes() {
    let settings: &[(&str, &str)] = &[("sparse_infill_density", "15%"), ("sparse_infill_pattern", "grid"), ("brim_type", "no_brim"), ("skirt_loops", "0")];
    let mut p = plate();
    p.parts.truncate(1);
    p.parts[0].mesh = cube([100.0, 100.0, 0.0], [24.0, 24.0, 20.0]);
    let input = whole(1, &p, settings);
    let mut t = test("Posed settings", [112.0, 112.0, 10.0], [16.0, 16.0, 12.0], 0.0);
    let (e, rx) = engine();
    let original = pieces(&input, std::slice::from_ref(&t));
    e.settle(1, original.clone());
    let as_printed = settled(&rx, 1);
    t.pose = Some(fab_model::eval::rotation_xyz([90.0, 0.0, 0.0]));
    let posed = pieces(&input, std::slice::from_ref(&t));
    assert_ne!(posed.plate_id, original.plate_id, "pose keys the plate");
    let piece = posed.pieces.as_ref().unwrap().parts[0].clone();
    e.settle(2, posed);
    let plain = settled(&rx, 2);
    let solid = &[("sparse_infill_density", "100%")];
    p.parts[0].paint = Some(settings_box([100.0, 100.0, 0.0], [103.0, 124.0, 20.0], solid));
    e.settle(3, pieces(&whole(2, &p, settings), std::slice::from_ref(&t)));
    assert!(plain.gcode == settled(&rx, 3).gcode, "an outside box leaves posed G-code byte-identical");
    // Original X stays X, original Y becomes posed Z, and original Z becomes -Y.
    p.parts[0].paint = Some(settings_box([112.0, 112.0, 0.0], [124.0, 124.0, 20.0], solid));
    let painted = whole(3, &p, settings);
    let (full, _) = posed_whole(painted.clone(), 0, piece.xf.unwrap());
    e.settle(4, pieces(&painted, std::slice::from_ref(&t)));
    let partly = settled(&rx, 4);
    let (lo, hi, z) = ([114.0, 109.0], [118.0, 115.0], [10.0, 13.0]);
    let shift = piece.shift;
    let in_piece = length_in(&partly, Role::InternalSolidInfill, [lo[0] + shift[0], lo[1] + shift[1]], [hi[0] + shift[0], hi[1] + shift[1]], z);
    let in_full = length_in(&full, Role::InternalSolidInfill, lo, hi, z);
    assert!(in_full > 50.0 && (in_piece - in_full).abs() < 0.1 * in_full, "same model points print solid: piece {in_piece}, full {in_full}");
    let crossing = |r: &Settled, shift: [f64; 2]| r.layers.records().filter(|m| {
        if m.kind != MoveKind::Extrude || m.role != Role::InternalSolidInfill || !(z[0]..z[1]).contains(&(m.p0[2] as f64)) { return false; }
        let (mut enter, mut leave) = (0.0f64, 1.0f64);
        for k in 0..2 {
            let a = m.p0[k] as f64 - shift[k];
            let d = (m.p1[k] - m.p0[k]) as f64;
            if d == 0.0 {
                if a < lo[k] || a > hi[k] { return false; }
            } else {
                let (u, v) = ((lo[k] - a) / d, (hi[k] - a) / d);
                enter = enter.max(u.min(v));
                leave = leave.min(u.max(v));
            }
        }
        enter < leave
    }).count();
    let count = crossing(&full, [0.0; 2]);
    assert!(count > 0, "the whole print has solid moves in the model window");
    assert_eq!(crossing(&partly, shift), count, "the posed piece has the same count of solid moves at those model points");
    assert_eq!(length_in(&partly, Role::SparseInfill, [lo[0] + shift[0], lo[1] + shift[1]], [hi[0] + shift[0], hi[1] + shift[1]], z), 0.0, "the box contains no sparse infill");
    assert!(length_in(&partly, Role::SparseInfill, [lo[0] + shift[0], lo[1] + shift[1]], [hi[0] + shift[0], hi[1] + shift[1]], [3.0, 6.0]) > 0.0, "below the rotated box stays sparse");
    t.pose = None;
    let back = pieces(&input, &[t]);
    assert_eq!(back.plate_id, original.plate_id, "None restores the plate key");
    e.settle(5, back);
    assert!(as_printed.gcode == settled(&rx, 5).gcode, "pose then None restores every byte");
}

#[test]
fn posed_and_as_printed_pieces_share_a_job_without_touching() {
    let input = whole(1, &plate(), &[("brim_type", "no_brim"), ("skirt_loops", "0")]);
    let mut cap = test("Posed cap", [100.0, 100.0, 8.0], [18.0, 18.0, 8.0], 0.0);
    cap.pose = Some(fab_model::eval::rotation_xyz([90.0, 0.0, 0.0]));
    let tests = [cap, test("As printed", [205.0, 106.0, 3.0], [8.0, 8.0, 4.0], 0.0)];
    let both = pieces(&input, &tests);
    let list = both.pieces.clone().unwrap();
    let (e, rx) = engine();
    e.settle(1, both);
    let result = settled(&rx, 1);
    for (i, t) in tests.iter().enumerate() {
        let alone = pieces(&input, std::slice::from_ref(t));
        let shift = alone.pieces.as_ref().unwrap().parts[0].shift;
        let (ae, arx) = engine();
        ae.settle(1, alone);
        let own = printed(&settled(&arx, 1), 0, shift);
        let joined = printed(&result, i as u16, list.parts[i].shift);
        let near = |a: &(u8, [i64; 6]), b: &(u8, [i64; 6])| a.0 == b.0 && a.1.iter().zip(&b.1).all(|(x, y)| (x - y).abs() <= 1);
        assert!(own.len() > 20 && own.len() == joined.len() && own.iter().all(|a| joined.iter().any(|b| near(a, b))) && joined.iter().all(|a| own.iter().any(|b| near(a, b))), "{} prints the same moves alone and together", t.name);
    }
    let reads = read_pieces(result.layers.records(), &result.stats, &list);
    let (a, b) = (&reads.pieces[0], &reads.pieces[1]);
    assert!(a.bed_max[0] < b.bed_min[0] || b.bed_max[0] < a.bed_min[0] || a.bed_max[1] < b.bed_min[1] || b.bed_max[1] < a.bed_min[1], "posed and as-printed material never touch");
}

#[test]
fn a_cubes_bottom_in_the_air_prints_from_its_lowest_material() {
    let settings: &[(&str, &str)] = &[("initial_layer_print_height", "0.3"), ("layer_height", "0.2"), ("brim_type", "no_brim"), ("skirt_loops", "0")];
    let mut p = plate();
    p.parts.truncate(1);
    p.parts[0].mesh = cube([96.0, 96.0, 10.0], [8.0, 8.0, 6.0]);
    let input = whole(1, &p, settings);
    let (e, rx) = engine();
    let (result, own) = settled_with_cuts(&e, &rx, 1, pieces(&input, &[test("Hanging", [100.0, 100.0, 8.0], [20.0, 20.0, 16.0], 0.0)]));
    let own = &own[&0];
    assert!((own[0].0 - 0.3).abs() < 1e-9, "its first layer is the first layer, not {} mm up", own[0].0);
    assert!(own.iter().all(|l| !l.2.regions.is_empty()), "no layer is empty");
    let tops = result.layers.records().filter(is_model).map(|m| m.p0[2] as f64);
    let (low, high) = tops.fold((f64::MAX, f64::MIN), |(a, b), z| (a.min(z), b.max(z)));
    assert!(low < 0.31 && high < 6.3, "the 6 mm of it, on the bed: {low}..{high}");
}

//! Each end-to-end check as its own test (they run side by side).

use crate::checks::*;
use fab_robot::math::*;

macro_rules! check {
    ($f:ident) => {
        #[test]
        fn $f() {
            let c = super::checks::$f();
            println!("CHECK {}: {}", stringify!($f), c.figures);
            assert!(c.pass, "{}: {}", stringify!($f), c.figures);
        }
    };
}

check!(live_deposit_holds_the_track);
check!(rigid_deposit_misses_the_track);
check!(unpreloaded_lost_motion_strays);
check!(pair_holds_within_rating);
check!(same_seed_same_bits);
check!(armed_loop_meets_no_compile);
check!(live_cut_trims_the_margin);
check!(rigid_cut_misses_the_margin);
check!(fdm_plan_time_is_the_cells);
check!(fdm_tile_holds_its_size);
check!(single_850_welds_on_the_track);
check!(single_850_trims_what_it_reaches);

#[test]
fn every_check_is_listed_once() {
    let names: Vec<&str> = all().map(|(n, _)| n).collect();
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "{names:?}");
    assert_eq!(CELL.len(), 12, "a cell check without its test here");
}

/// The profile at infinite jerk is the trapezoid; with jerk it is slower, its speed continuous
/// and its distance monotone, and it ends where the path does.
#[test]
fn profile_jerk_limits() {
    use fab_arms::path::{Motion, Profile, Waypoint};
    let w = |p: [f64; 3], speed: f64| Waypoint { p, z: [0.0, 0.0, -1.0], x: None, speed, on: true };
    let path = [w([0.0, 0.0, 0.0], 50.0), w([30.0, 0.0, 0.0], 50.0), w([30.0, 20.0, 0.0], 50.0), w([31.0, 20.0, 0.0], 50.0), w([60.0, 25.0, 0.0], 80.0)];
    let trap = Motion { accel: 500.0, jerk: f64::INFINITY, deviation: 0.05, turn: 0.6, lever: 0.0, rest: false };
    let a = Profile::new(&path, &trap);
    // A lone 30 mm segment from rest to rest at 50 mm/s, 500 mm/s²: 0.1 s up, 0.1 s down, 0.5 s cruise.
    let one = Profile::new(&path[..2], &trap);
    assert!((one.duration() - 0.7).abs() < 1e-9, "{}", one.duration());
    let b = Profile::new(&path, &Motion { jerk: 5000.0, ..trap });
    assert!(b.duration() > a.duration() + 0.05, "{} vs {}", b.duration(), a.duration());
    let (mut last_s, mut last_v) = (0.0, 0.0);
    let n = 4000;
    for k in 1..=n {
        let t = b.duration() * k as f64 / n as f64;
        let smp = b.at(&path, t);
        let i = smp.seg;
        let s = b.s[i - 1] + dist(path[i - 1].p, smp.p);
        assert!(s >= last_s - 1e-9, "distance went back at {t}");
        assert!((smp.speed - last_v).abs() < 5000.0 * b.duration() / n as f64 * 1.01 + 500.0 * b.duration() / n as f64 + 1e-6, "speed jumped at {t}: {last_v} to {}", smp.speed);
        (last_s, last_v) = (s, smp.speed);
    }
    assert!((last_s - b.length()).abs() < 1e-6 && last_v.abs() < 1e-6);
}

/// A tangent beam round a 7 mm ring turns the tool: on a 250 mm lever its flange swings far
/// more than the TCP moves, so the path speeds up that much more gently.
#[test]
fn profile_turning_tool_ramps_by_its_lever() {
    use fab_arms::path::{Motion, Profile, Waypoint};
    let path: Vec<Waypoint> = (0..=8)
        .map(|k| {
            let a = k as f64 * 0.1;
            Waypoint { p: [7.0 * a.cos(), 7.0 * a.sin(), 0.0], z: [-a.sin(), a.cos(), 0.0], x: Some([a.cos(), a.sin(), 0.0]), speed: 4.0, on: true }
        })
        .collect();
    let m = Motion { accel: 400.0, jerk: f64::INFINITY, deviation: 0.05, turn: 0.6, lever: 0.0, rest: false };
    let (bare, lever) = (Profile::new(&path, &m), Profile::new(&path, &Motion { lever: 250.0, ..m }));
    assert!(lever.duration() > bare.duration(), "{} vs {}", lever.duration(), bare.duration());
    let (vb, vl) = (bare.at(&path, 0.01).speed, lever.at(&path, 0.01).speed);
    // 10 ms in: at full 400 mm/s² the TCP is at its 4 mm/s; on the lever (k = 1 + 250/7) about 0.11 mm/s.
    assert!(vb > 3.9 && vl < 0.15 && vl > 0.0, "{vb} vs {vl}");
}

/// The same ring with the part turned under the tool so its axes stay put in the cell: no lever
/// swing slows it, only the turn axis' own limits; the turn runs from the first waypoint's to
/// the last's, within the axis' speed where that binds.
#[test]
fn profile_turning_part_holds_the_head() {
    use fab_arms::path::{Motion, Profile, Turning, Waypoint};
    let path: Vec<Waypoint> = (0..=8)
        .map(|k| {
            let a = k as f64 * 0.1;
            Waypoint { p: [7.0 * a.cos(), 7.0 * a.sin(), 0.0], z: [-a.sin(), a.cos(), 0.0], x: Some([a.cos(), a.sin(), 0.0]), speed: 4.0, on: true }
        })
        .collect();
    let turns: Vec<f64> = (0..=8).map(|k| -(k as f64) * 0.1).collect();
    let m = Motion { accel: 400.0, jerk: f64::INFINITY, deviation: 0.05, turn: 0.6, lever: 250.0, rest: false };
    let lever = Profile::new(&path, &m);
    let free = Profile::turning(&path, &m, Turning { turns: &turns, vmax: 1.6, accel: 4.8 });
    assert!(free.duration() < lever.duration(), "{} vs {}", free.duration(), lever.duration());
    let slow = Profile::turning(&path, &m, Turning { turns: &turns, vmax: 0.3, accel: 4.8 });
    let end = slow.duration();
    let turn = |t: f64| slow.turn_at(&path, &turns, t);
    assert!(turn(0.0) == 0.0 && (turn(end) + 0.8).abs() < 1e-9, "{} to {}", turn(0.0), turn(end));
    let rate = (1..100).map(|k| end * k as f64 / 100.0).map(|t| (turn(t + 1e-4) - turn(t - 1e-4)).abs() / 2e-4).fold(0.0f64, f64::max);
    assert!(rate > 0.25 && rate < 0.3 + 1e-6, "{rate} rad/s");
}

/// The one 850 laying and trimming the tile as the compare times it: cuts run with the table
/// turning under them where that is the faster, and the job is the faster for it; the work
/// that ran the wrist near its singularity leans clear of it, each within its tool's window.
#[test]
fn single_850_turns_under_cuts_and_leans_clear() {
    use crate::fdm::Part;
    use crate::lab::{Cell, Session, Setup};
    use fab_arms::route::{DedTrim, DedTrimOpts};
    let design = Part::Tile.design();
    let run = |coordinate: bool| {
        let setup = Setup { cell: Cell::Single850, max_time: 1.0e6, ..Setup::new(fab_arms::planner::LoopMode::Live, 1) };
        let mut s = Session::new(setup, &design, Box::new(DedTrim::new(&design, DedTrimOpts { trim: true, ..DedTrimOpts::default() })), None);
        s.planner.set_coordinate(coordinate);
        (s.dry_run(), s.planner.report.clone())
    };
    let ((turning, r), (between, still)) = (run(true), run(false));
    assert!(r.done && still.done && r.skipped.is_empty() && still.skipped.is_empty(), "{:?} {:?}", r.skipped, still.skipped);
    assert!(r.coordinated > 0 && still.coordinated == 0 && turning < between, "{} cuts turning: {turning:.1} s against {between:.1} s", r.coordinated);
    let [deposits, scans] = r.leaned;
    assert!(scans.0 > 0 && scans.1 <= 45f64.to_radians() + 1e-9 && deposits.1 <= 20f64.to_radians() + 1e-9, "{:?}", r.leaned);
}

/// Fab's slicer plans the tile for the 1 mm nozzle: two 0.6 mm layers of 1.15 mm lines, the
/// paths inside the part.
#[test]
fn fdm_plan_is_fabs_slice() {
    use crate::fdm::*;
    let cell = FdmCell::default();
    let e = cell.extruder();
    assert_eq!((e.nozzle, e.layer(), e.line()), (1.0, 0.6, 1.15));
    let p = plan(&crate::printer::h2d(), &Part::Tile.design(), &e).unwrap();
    assert_eq!(p.layers.len(), 2);
    for l in &p.layers {
        assert!((l.height - 0.6).abs() < 1e-9 && !l.paths.is_empty());
        for fp in &l.paths {
            assert!((fp.width - 1.15).abs() < 0.2, "width {}", fp.width);
            assert!(fp.pts.iter().all(|q| q[0].abs() < 4.0 && q[1].abs() < 4.0 && (q[2] - l.z).abs() < 1e-9));
        }
    }
    assert!((p.opts.min_layer_time - 12.0).abs() < 1e-9);
}

/// Every plastic cell reaches its bed tool down over the largest part (60 × 60 × 50 mm) from
/// its park, and its first mode there is at or above the note's class for its arm with 2–3 kg
/// (the head is under 1 kg): 15–30 Hz on the IRB 1100, 6–15 Hz on the 850.
#[test]
fn fdm_cells_reach_and_ring() {
    use crate::fdm::FdmCell;
    use fab_arms::route::{IK, W_MIN};
    use fab_robot::dynamics::first_mode;
    use fab_robot::kin::{ik, manipulability, IkGoal};
    use fab_robot::presets::FDM_ARMS;
    for (i, a) in FDM_ARMS.iter().enumerate() {
        let cell = FdmCell { arm: i as u8, ..FdmCell::default() };
        let c = cell.setup();
        let arm = &c.arms[0];
        let park = cell.park();
        assert!(manipulability(arm, &park) >= W_MIN, "{}: no park", a.id);
        for p in [[-30.0, -30.0, 0.0], [30.0, -30.0, 0.0], [30.0, 30.0, 0.0], [-30.0, 30.0, 0.0], [-30.0, -30.0, 50.0], [30.0, 30.0, 50.0]] {
            let g = IkGoal { p: c.plate.apply(p), z: [0.0, 0.0, -1.0], x: None };
            let q = ik(arm, &g, &park, &IK).unwrap_or_else(|m| panic!("{}: {p:?} out of reach ({:.2} mm)", a.id, m.err_mm));
            assert!(manipulability(arm, &q) >= W_MIN, "{}: {p:?} near a singularity", a.id);
        }
        let f1 = first_mode(arm, &park);
        let m = cell.motion();
        println!("FDM {}: first mode {f1:.1} Hz, accel {:.0} mm/s², jerk {:.0} mm/s³", a.id, m.accel, m.jerk);
        let (lo, hi) = match a.id {
            "irb1100" => (15.0, 40.0),
            "uf850_stream" | "uf850_own" => (6.0, 25.0),
            _ => (1.0, 60.0),
        };
        assert!(f1 > lo && f1 < hi, "{}: first mode {f1:.1} Hz", a.id);
    }
}

/// The tile's table: the H2D first at 1×, the five plastic arms and the two metal rows, each
/// timed with its limit named and its sheet; a tile is cooling-bound on every arm (two layers
/// at their shortest time), the weld-only row by its deposition; the same table twice.
#[test]
fn compare_table_weighs_every_option() {
    use crate::compare::*;
    use crate::fdm::Part;
    let t = table(Part::Tile, 0, 10);
    let groups: Vec<Group> = t.rows.iter().map(|r| r.group).collect();
    assert_eq!(groups, [[Group::Printer].as_slice(), &[Group::Plastic; 5], &[Group::Metal; 2]].concat());
    assert_eq!(t.ratio(&t.rows[0]), Some(1.0));
    for r in &t.rows {
        let time = *r.time.as_ref().unwrap_or_else(|e| panic!("{}: {e}; {}", r.name, r.notes.join(" ")));
        assert!(time > 0.0 && r.mode_hz > 1.0 && !r.sheet.is_empty(), "{}: {time} s, {} Hz, {} sheet rows", r.name, r.mode_hz, r.sheet.len());
        let (limit, saves) = r.limit().unwrap_or_else(|| panic!("{}: nothing limits it", r.name));
        assert!(saves > 0.0 && saves < time, "{}: lifting {} saves {saves} of {time} s", r.name, limit.label());
        println!("COMPARE {}: {time:.1} s, {:.2}× the H2D, {:.1} Hz, {} saves {saves:.1} s", r.name, t.ratio(r).unwrap(), r.mode_hz, limit.label());
        if r.group == Group::Metal {
            for n in &r.notes {
                println!("COMPARE   {n}");
            }
        }
    }
    for r in &t.rows[1..6] {
        assert_eq!(r.limit().map(|l| l.0), Some(Limit::Cooling), "{}", r.name);
    }
    assert_eq!(t.rows[6].name, WELD_ONLY);
    assert_eq!(t.rows[6].limit().map(|l| l.0), Some(Limit::Deposition));
    assert!(t.rows[6].sheet.iter().any(|s| s.1.contains("1.5 kW; 0.40 kg/h")), "the weld head's rate is on its sheet");
    let again = table(Part::Tile, 0, 10);
    for (a, b) in t.rows.iter().zip(&again.rows) {
        assert_eq!(a.time.as_ref().ok().map(|x| x.to_bits()), b.time.as_ref().ok().map(|x| x.to_bits()), "{} differs", a.name);
    }
}

/// Work skipped as out of reach is what a row shows in place of its time, by kind.
#[test]
fn unreached_work_is_named() {
    use crate::compare::unreached;
    use fab_arms::planner::Report;
    use fab_arms::route::TaskKind;
    let mut r = Report::default();
    assert_eq!(unreached(&r), None);
    r.unreachable = 3;
    assert_eq!(unreached(&r).as_deref(), Some("3 setpoints"));
    r.skipped = vec![TaskKind::Cut { z0: 0.0, z1: 2.0 }, TaskKind::Deposit { layer: 4, top: 5.0 }, TaskKind::Cut { z0: 2.0, z1: 4.0 }];
    assert_eq!(unreached(&r).as_deref(), Some("1 deposit, 2 cuts"));
    r.flags = vec![([4.0, -4.0, 1.0], fab_arms::route::CUT_UNREACHED)];
    assert_eq!(unreached(&r).as_deref(), Some("1 deposit, 2 cuts, 1 cut point"));
}

/// The cell loop's real-time path arms a dry run or a real build only once compiled; a
/// simulation arms nothing.
#[test]
fn realtime_path_arms_only_compiled() {
    use crate::realtime::RtPath;
    use fab_robot::safety::{Mode, RtReady};
    let rt = RtPath::mark(None);
    assert!(rt.arm_for(Mode::Sim).expect("a simulation needs nothing compiled").is_none());
    match rt.arm_for(Mode::Real) {
        Ok(Some(_)) => assert_eq!(rt.ready(), RtReady::Ready),
        Ok(None) => panic!("a real build armed nothing"),
        // Not compiled when asked (it may be by now: the compile runs on in the background).
        Err(e) => assert!(e.contains("not compiled ahead"), "{e}"),
    }
    println!("REALTIME {}", rt.line());
    assert!(rt.line().contains("lazy compiles on real-time threads"), "{}", rt.line());
}

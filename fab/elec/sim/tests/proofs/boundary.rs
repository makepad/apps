//! A boundary reads the same voltage as a PWL source, with only the exporter's events, and
//! feeding the next round preserves the RC's history in Exact and Fast.

use fab_elec_sim::analysis::op::operating_point;
use fab_elec_sim::analysis::tran::{Accepted, Stepper};
use fab_elec_sim::circuit::{Boundary, BoundaryKind};
use fab_elec_sim::dev::boundary::Table;
use fab_elec_sim::stamp::Tier;
use fab_elec_sim::{Record, SimCircuit, Sink, Solver, Source, Wave};
use std::sync::Arc;

#[test]
fn boundary_sources_follow_tables_across_rounds() {
    // 257 samples of a 1 kHz sine. Binary time spacing makes shifting the PWL by the coupling
    // delay exact. Only two of these points are export events.
    let dt = 1.0 / 262_144.0;
    let delay = 8.0 * dt;
    let t: Vec<f64> = (0..=256).map(|i| i as f64 * dt).collect();
    let v = t.iter().map(|&t| 0.25 + (std::f64::consts::TAU * 1e3 * t).sin()).collect();
    let table = Arc::new(Table { t, v, events: vec![64, 192], edges: vec![] });
    let first = Arc::new(Table { t: table.t[..=128].to_vec(), v: table.v[..=128].to_vec(), events: vec![64], edges: vec![] });
    let second = Arc::new(Table { t: table.t[128..].to_vec(), v: table.v[128..].to_vec(), events: vec![64], edges: vec![] });
    let current = Arc::new(Table { t: vec![0.0], v: vec![-1e-3], events: vec![], edges: vec![] });
    let split = table.t[128] + delay;
    let stop = table.t[256] + delay + 4.0 * dt;
    assert_eq!(table.value(-dt), table.v[0], "the table holds its first value before its start");
    assert_eq!(table.value(stop), table.v[256], "the table holds its last value after its end");
    assert!((table.value(10.5 * dt) - 0.5 * (table.v[10] + table.v[11])).abs() < 1e-15, "ordinary table points interpolate linearly");
    assert_eq!(table.next_event(0.0), table.t[64], "ordinary points are not events");
    assert_eq!(table.next_event(table.t[64]), table.t[192], "an event lookup is strictly after its argument");
    assert_eq!(table.next_event(table.t[192]), f64::INFINITY, "the last event has no successor");

    let mut circuit = SimCircuit::new();
    circuit.opts.reltol = 1e-4;
    let input = circuit.node("in");
    let load = circuit.node("load");
    // Put the current first to exercise both kinds in a single bank, with no branch for it.
    circuit.devices.boundary.push(Boundary { name: "ib".into(), node: load, kind: BoundaryKind::Current, v0: 2e-3 });
    circuit.devices.boundary.push(Boundary { name: "vb".into(), node: input, kind: BoundaryKind::Voltage, v0: 0.75 });
    circuit.r("load_r", "load", "0", 500.0);
    circuit.r("r", "in", "out", 1e3);
    circuit.c("c", "out", "0", 100e-9);
    circuit.v("pwl", "ref_in", "0", Source::wave(Wave::Pwl(table.t.iter().zip(&table.v).map(|(&t, &v)| (t + delay, v)).collect())));
    circuit.r("ref_r", "ref_in", "ref_out", 1e3);
    circuit.c("ref_c", "ref_out", "0", 100e-9);
    // Both RC branches share a solve and thus every accepted time and integration history.
    // A PWL announces every knot; an isolated boundary announces only its two events. Comparing
    // independently adaptive meshes would include integration error, not just source rounding.
    // 1e-11 V allows accumulated f64 rounding from the two RC equations' different row ordering;
    // it is far below the integration tolerances and cannot hide a waveform or history change.
    const ROUNDING: f64 = 1e-11;

    for (mode, res) in [("Exact", 0.0), ("Fast", 1.0 / 96e3)] {
        let run = |rounds: bool| {
            let mut solver = Solver::new(&circuit).expect("the boundary and reference circuits build");
            let out = solver.unknown("out").expect("the boundary capacitor has a node") as usize;
            let reference = solver.unknown("ref_out").expect("the reference capacitor has a node") as usize;
            let branch = solver.unknown("vb#branch").expect("a voltage boundary has a branch") as usize;
            assert!(solver.unknown("ib#branch").is_none(), "a current boundary has no branch unknown");
            operating_point(&mut solver, false).expect("unfed boundaries have a DC operating point");
            assert!((solver.x[input as usize] - 0.75).abs() < ROUNDING, "{mode}: an unfed voltage boundary uses v0");
            assert!((solver.x[load as usize] - 2e-3 * 500.0).abs() < ROUNDING, "{mode}: an unfed current boundary injects v0 from ground, v = i R");
            assert!(!solver.feed("missing", table.clone(), delay), "feeding a missing name returns false");
            assert!(!solver.feed("pwl", table.clone(), delay), "an independent source does not accept a boundary feed");
            let (bank, inst) = solver.find_device("vb").expect("the voltage boundary is found by name");
            let (_, ci) = solver.find_device("ib").expect("the current boundary is found by name");
            assert_eq!(solver.banks[bank].tier(inst), Tier::Timed, "an unfed boundary stays ready for its first feed");
            assert!(!solver.banks[bank].nonlinear(), "a boundary is linear");
            assert!(solver.feed("ib", current.clone(), delay), "the current boundary accepts its feed");
            assert!(solver.feed("vb", if rounds { first.clone() } else { table.clone() }, delay), "the voltage boundary accepts its first feed");
            assert_eq!(solver.banks[bank].next_break(0.0), table.t[64] + delay, "only the first event, shifted by the delay, is a breakpoint");
            assert_eq!(solver.banks[bank].next_break(table.t[64] + delay), if rounds { f64::INFINITY } else { table.t[192] + delay },
                "the reached event and all ordinary table points are skipped");
            assert!(solver.banks[bank].varies_after(table.t[64] + delay), "interpolation varies even when a table has no later event");

            let mut stepper = Stepper::start(&mut solver, false, 4.0 * dt).expect("fed boundaries have a DC operating point");
            stepper.set_res(&mut solver, res);
            assert!((solver.x[input as usize] - table.value(-delay)).abs() < ROUNDING, "{mode}: DC reads the fed table at minus delay");
            assert!((solver.x[load as usize] + 0.5).abs() < ROUNDING, "{mode}: a negative current feed gives v = i R at DC");
            let mut terminal = [0.0; 2];
            let xdot = vec![0.0; solver.x.len()];
            let cx = solver.base_cx();
            solver.banks[bank].terminal_currents(ci, &solver.x, &xdot, &cx, &mut terminal);
            assert_eq!(terminal, [1e-3, -1e-3], "current boundary terminal currents flow into node and ground terminals");
            solver.banks[bank].terminal_currents(inst, &solver.x, &xdot, &cx, &mut terminal);
            assert_eq!(terminal, [solver.x[branch], -solver.x[branch]], "voltage boundary terminal currents use its branch unknown");

            let mut record = Record::default();
            record.begin(&solver.unknowns);
            record.point(0.0, None, &solver.x);
            let mut emit = |s: &Solver, p: &Accepted| {
                assert!((s.x[out] - s.x[reference]).abs() < ROUNDING,
                    "{mode}, rounds {rounds}: RC boundary and PWL agree to {ROUNDING:e} V at accepted time {:e}: {} vs {}", p.t, s.x[out], s.x[reference]);
                assert!((s.x[load as usize] + 0.5).abs() < ROUNDING, "{mode}: the fed current boundary keeps v = i R at {:e}", p.t);
                record.point(p.t, None, &s.x);
            };
            stepper.run_to(&mut solver, split, &mut emit, None).expect("the RC runs to the round boundary");
            if rounds {
                let before = (solver.x.clone(), stepper.t(), stepper.restarts);
                assert!(solver.feed("vb", second.clone(), delay), "the continuing table replaces the first feed");
                stepper.breaks_changed(&solver);
                assert_eq!(solver.x, before.0, "a feed preserves the accepted solution");
                assert_eq!(stepper.t(), before.1, "a feed preserves time");
                assert_eq!(stepper.restarts, before.2, "a feed does not restart integration history");
                assert_eq!(solver.banks[bank].next_break(split), table.t[192] + delay, "the next round exposes its new event");
            }
            stepper.run_to(&mut solver, stop, &mut emit, None).expect("the RC runs through the continuing table");
            assert_eq!(stepper.restarts.change, 0, "{mode}: no feed causes a value-edit restart");
            assert_eq!(solver.banks[bank].next_break(stop), f64::INFINITY, "no ordinary or past point is a late breakpoint");
            assert!(!solver.banks[bank].varies_after(stop), "a table holds after its final point");
            record
        };

        let (whole, rounds) = (run(false), run(true));
        let (a, b) = (whole.v("out"), rounds.v("out"));
        let (mut i, mut j, mut shared) = (0, 0, 0);
        while i < whole.at.len() && j < rounds.at.len() {
            match whole.at[i].total_cmp(&rounds.at[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    assert!((a[i] - b[j]).abs() < ROUNDING,
                        "{mode}: two feeds and one whole feed agree to {ROUNDING:e} V at shared accepted time {:e}: {} vs {}", whole.at[i], a[i], b[j]);
                    shared += 1;
                    i += 1;
                    j += 1;
                }
            }
        }
        assert!(shared > table.t.len(), "{mode}: the feed comparison shares at least every PWL knot and the final point, got {shared}");
        assert_eq!(whole.at.last(), Some(&stop), "{mode}: the whole-table run finishes");
        assert_eq!(rounds.at.last(), Some(&stop), "{mode}: the two-round run finishes");
    }
}

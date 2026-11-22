use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::{lower, ModelLibs};
use fab_elec_core::ids::BoardId;
use fab_elec_sim::live::LiveCmd;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{Change, Options, Record, Sink};

#[test]
fn ledctl_live_currents_balance() {
    let design = fab_elec_fixtures::ledctl();
    let bench = ledctl_bench(10e3, [0.3; 8]);
    let lowered = lower(&design, BoardId(0), &bench, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "live LED controller lowers without diagnostics: {:?}", lowered.diags);
    let plan = lowered.plan;
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, ..LiveSpec::default() };
    assert_eq!(spec.reltol_at(spec.res), Options::default().reltol, "LED current proof runs in Exact at the bench's tolerance");
    let mut sim = LiveSim::new(lowered.circuit, plan.clone(), design.generation, spec)
        .expect("live LED controller operating point converges");
    // Acknowledge the initial publication so each advance can finish its whole Step.
    sim.values(1.0);
    let mut record = Record::default();
    let mut worst_residual = 0.0_f64;
    for publication in 1..=10 {
        let before = sim.t();
        assert!(sim.apply(LiveCmd::Step(3.7e-5)), "LED publication {publication}: Step keeps the worker alive");
        sim.advance(f64::INFINITY, None, &mut record)
            .unwrap_or_else(|diag| panic!("LED publication {publication}: live Step converges: {diag}"));
        let values = sim.values(1.0);
        assert!(!sim.running() && values.t >= before + 3.7e-5, "LED publication {publication}: Step reaches its target and pauses");
        assert_eq!(values.net_v.len(), plan.nets.len(), "LED publication {publication}: every net has a voltage");
        assert_eq!(values.branch_i.len(), plan.pin_net.len(), "LED publication {publication}: every pin has a current");
        assert_eq!(values.part_p.len(), plan.part_pins.len(), "LED publication {publication}: every component has a power");

        for (net, name) in plan.nets.iter().enumerate() {
            let (mut sum, mut largest) = (0.0_f64, 0.0_f64);
            for (&pin_net, &current) in plan.pin_net.iter().zip(&values.branch_i) {
                if pin_net == Some(net as u32) {
                    let current = f64::from(current);
                    sum += current;
                    largest = largest.max(current.abs());
                }
            }
            let residual = sum.abs();
            worst_residual = worst_residual.max(residual);
            let tolerance = 1e-9 + 1e-6 * largest;
            assert!(residual <= tolerance, "LED publication {publication}: net {name} obeys KCL: residual {residual:e}, tolerance {tolerance:e}, largest pin current {largest:e}");
        }
        for (part, &(first, count)) in plan.part_pins.iter().enumerate() {
            // Reconstruct power from the published f32 voltages and terminal currents.
            let terms: Vec<f32> = (first..first + count).filter_map(|pin| {
                plan.pin_net[pin as usize].map(|net| values.net_v[net as usize] * values.branch_i[pin as usize])
            }).collect();
            let expected: f32 = terms.iter().sum();
            let actual = values.part_p[part];
            // relative to the terms' magnitudes (a part whose terms cancel has a power near 0), with a
            // floor far below any visible glow (the threshold is 0.25 mW)
            let tolerance = 1e-6 * terms.iter().map(|t| t.abs()).sum::<f32>() + 1e-15;
            assert!((actual - expected).abs() <= tolerance, "LED publication {publication}: component {part} power equals the sum of pin voltage times current: {actual:e} vs {expected:e}, tolerance {tolerance:e}");
        }
    }
    eprintln!("ledctl live worst net KCL residual: {worst_residual:e} A");
}

/// The 10 % to 90 % rise of each rising edge of `node` after `from`, read from the recorded points by
/// linear interpolation (as a scope reads it), with the edge's 10 % time.
fn rises(record: &Record, node: &str, from: f64) -> Vec<(f64, f64)> {
    let (v, at) = (record.v(node), &record.at);
    let (vmin, vmax) = v.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    let (lo, hi) = (vmin + 0.1 * (vmax - vmin), vmin + 0.9 * (vmax - vmin));
    let cross = |i: usize, level: f64| at[i - 1] + (level - v[i - 1]) / (v[i] - v[i - 1]) * (at[i] - at[i - 1]);
    let mut out = Vec::new();
    let mut i = 1;
    while i < at.len() {
        if at[i] > from && v[i - 1] < lo && v[i] >= lo {
            let j = (i..at.len()).find(|&j| v[j] >= hi);
            let Some(j) = j else { break };
            out.push((cross(i, lo), cross(j, hi) - cross(i, lo)));
            i = j;
        }
        i += 1;
    }
    out
}

#[test]
fn ledctl_pin_edges_sample_alike_across_a_retune() {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let before = lower(&design, BoardId(0), &ledctl_bench(10e3, [0.3; 8]), &libs);
    let mut duty = [0.3; 8];
    duty[0] = 0.7;
    let after = lower(&design, BoardId(0), &ledctl_bench(10e3, duty), &libs);
    let changes = before.circuit.value_changes(&after.circuit).expect("a duty step is a value-only change");
    assert_eq!(changes.len(), 1, "a duty step on PA0 retunes one GPIO: {changes:?}");
    let pad = before.circuit.devices.gpio.iter().find(|p| p.name.ends_with("PA0.io")).expect("PA0 has a GPIO");
    let pad = before.circuit.nodes[pad.pad as usize].name.clone();
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(before.circuit, before.plan, 1, spec).expect("ledctl operating point converges");
    sim.values(1.0);
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    let run = |sim: &mut LiveSim, record: &mut Record, span: f64| {
        assert!(sim.apply(LiveCmd::Step(span)), "Step keeps the worker alive");
        while sim.running() {
            sim.advance(f64::INFINITY, None, record).unwrap_or_else(|d| panic!("ledctl live Step converges: {d}"));
            sim.values(1.0);
        }
    };
    run(&mut sim, &mut record, 4.25e-4);
    for change in changes {
        let Change::Pin(device, mode) = change else { panic!("a duty step changes only a GPIO's mode") };
        assert_eq!(device, "mcu.u#PA0.io", "the duty step retunes PA0");
        assert!(sim.apply(LiveCmd::PinMode { generation: 1, device, mode }), "the duty retune keeps the worker alive");
    }
    run(&mut sim, &mut record, 5.75e-4);
    assert_eq!(sim.stats().retuned, 1, "the duty step was a retune");
    assert!(record.at_time(&pad, 9e-4 + 6.5e-5) > 1.5, "PA0 runs at the new duty after the retune");
    // every rising edge after the first (which follows the operating point) is the same GPIO edge
    // into the same load: it must be sampled alike, before and after the retune
    let edges = rises(&record, &pad, 5e-5);
    assert!(edges.len() >= 9, "PA0 rises once per period: {edges:?}");
    let (lo, hi) = edges.iter().fold((f64::MAX, f64::MIN), |(lo, hi), e| (lo.min(e.1), hi.max(e.1)));
    assert!(hi - lo < 1e-12, "PA0's rising edges read the same rise before and after a same-edge retune: {edges:?}");
}

/// A live edit that deletes a conducting LED's resistor (lamps[0].r, at a fixed instant of its
/// on-phase) leaves the anode on one connection, carried at the forward voltage it had. The LED's
/// junction charge holds that net: the step after the Replace converges without the stepper's gmin
/// ladder, the run goes on for a millisecond of PWM with no rescue, the junction discharges
/// through its own forward current, which falls exponentially with the junction's voltage (a
/// millisecond on, nanoamps of the 5 mA it carried, the anode still about 2 V over the cathode's
/// PWM: an open LED holds its charge, as a real one does), and the lowering notes the open net where
/// it is.
#[test]
fn a_live_edit_that_opens_a_conducting_led_runs_on_with_a_note() {
    let design = fab_elec_fixtures::ledctl();
    let bench = ledctl_bench(10e3, [0.3; 8]);
    let libs = ModelLibs::generic();
    let lowered = lower(&design, BoardId(0), &bench, &libs);
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit, lowered.plan, 0, spec).expect("live LED controller operating point converges");
    sim.values(1.0);
    let mut record = Record::default();
    sim.apply(LiveCmd::Step(1.35e-4));
    sim.advance(f64::INFINITY, None, &mut record).expect("the design runs to the edit");
    sim.values(1.0);

    let mut edited = design.clone();
    edited.generation += 1;
    let unit = edited.units.iter_mut().find(|u| u.id == BoardId(0)).expect("ledctl has board 0");
    std::sync::Arc::make_mut(&mut unit.circuit).comps.retain(|c| c.path != "lamps[0].r");
    let le = lower(&edited, BoardId(0), &bench, &libs);
    let anode = "lamps[0].d.A";
    let notes: Vec<_> = le.diags.iter().filter(|d| d.net.is_some()).collect();
    assert!(notes.len() == 1 && notes[0].path == anode && notes[0].message.contains("one connection only (lamps[0].d A)"),
        "the open anode is noted on its net, and nothing else is: {:?}", le.diags);
    let (a, k) = (le.plan.nets.iter().position(|n| n == anode), le.plan.nets.iter().position(|n| n == "LED0"));
    let (a, k) = (a.expect("the anode net is simulated"), k.expect("LED0 is simulated"));
    sim.apply(LiveCmd::Replace { generation: 1, circuit: Box::new(le.circuit), plan: Box::new(le.plan) });
    let t_edit = sim.t();
    sim.values(1.0);
    sim.apply(LiveCmd::Step(1e-3));
    while sim.t() < t_edit + 1e-3 {
        let before = sim.t();
        sim.advance(f64::INFINITY, None, &mut record).unwrap_or_else(|d| panic!("the run goes on after the edit: {d}"));
        sim.values(1.0);
        assert!(sim.t() > before, "every advance moves time");
    }
    let rescues = &sim.stats().rescues;
    assert!(rescues.is_empty(), "the LED's junction charge carries the open anode: no step needs the ladder: {rescues:?}");
    let v = sim.values(1.0);
    let vd = f64::from(v.net_v[a] - v.net_v[k]);
    // ledctl's lamps are green: the current the card passes at what is left across the junction.
    let [(_, is), (_, n), ..] = libs.led_params(fab_elec_bench::input::LedColor::Green).expect("the generic library has green");
    let i = is * (vd / (n * 0.025852)).exp_m1();
    assert!(vd > 0.0 && i < 1e-8, "the open anode keeps its junction's charge, the LED passing under 10 nA of its 5 mA: {vd} V, {i} A");
}

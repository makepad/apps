//! Step control preserves the RC response across narrow PWM edges.

use fab_elec_sim::analysis::exact::ExactStop;
use fab_elec_sim::{run, Analysis, Control, Record, SimCircuit, Source, TranSpec, Wave};

/// Start at the zero-volt operating point and solve each linear input segment exactly.
fn rc_at(t: f64, tau: f64, corners: &[(f64, f64)]) -> f64 {
    let mut x = 0.0;
    for (&(t0, a), &(t1, v1)) in corners.iter().zip(corners.iter().skip(1)) {
        let b = (v1 - a) / (t1 - t0);
        let s = t.min(t1) - t0;
        // For input a + b*s, carry the exact endpoint into the next segment.
        x = a + b * (s - tau) + (x - a + b * tau) * (-s / tau).exp();
        if t <= t1 {
            return x;
        }
    }
    x
}

#[test]
fn pwm_rc_follows_closed_form_through_edges() -> Result<(), String> {
    let period = 1e-4;
    let ramp = 5e-9;
    let tstop = 300e-6;
    let mut corners = vec![(0.0, 0.0)];
    for cycle in 0..3 {
        let start = f64::from(cycle) * period;
        corners.extend([
            (start + ramp, 1.0),
            (start + 0.5 * period, 1.0),
            (start + 0.5 * period + ramp, 0.0),
            (f64::from(cycle + 1) * period, 0.0),
        ]);
    }
    let analysis = Analysis::Tran(TranSpec { tstop, max_step: None, uic: false });
    let defaults = SimCircuit::new().opts;
    // The bound is E3's 0.1 % of the 1 V full scale. Step control: 1 ohm keeps the charge floor's
    // per-step allowance trtol * chgtol / C (at most 5.6e-4 V) under it, and Fast runs at reltol
    // 1e-4, whose allowance trtol * reltol (7e-4) is too, so the bound measures the stepper through
    // the edges (Fast's default 1e-3 allows 7e-3 by definition, E3.Q1). The floor: 1 kOhm and
    // 0.5 pF put chgtol / C at 2e-2 V, which Fast's charge tolerance allows (a step's error reaches
    // it, measured 1.02x); Exact tightens the floor with reltol and meets the bound.
    // The last case charges from rest well above its charge floor, as an LED junction does through
    // its loop on a GPIO edge (117 ohm, 50 pF, reltol 1e-2, Fast only): its level term is nothing
    // there, so its allowance is the rate term, which grows as h^2 like the error. One provisional
    // retry must land; the model retry used to land on its third (ESIM-Q12).
    let cases = [
        (0.5e-9, 1.0, None), (5e-9, 1.0, None), (10e-6, 1.0, None), (0.5e-9, 1e3, None), (125e-12, 1.0, None),
        (117.0 * 50e-12, 117.0, Some(1e-2)),
    ];
    let mut failures = Vec::new();
    for (tau, ohms, from_rest) in cases {
        let farads = tau / ohms;
        let floor = defaults.chgtol / farads;
        for exact in [false, true] {
            if exact && from_rest.is_some() {
                continue;
            }
            let mode = if exact { "Exact" } else { "Fast" };
            let mut circuit = SimCircuit::new();
            circuit.opts.exact = exact;
            let control = ohms < 100.0;
            if control && !exact {
                circuit.opts.reltol = 1e-4;
            }
            if let Some(reltol) = from_rest {
                circuit.opts.reltol = reltol;
                circuit.opts.profile = true;
            }
            circuit.v("v1", "in", "0", Source::wave(Wave::Pulse {
                v1: 0.0, v2: 1.0, td: 0.0, tr: ramp, tf: ramp,
                pw: 0.5 * period - ramp, per: period,
            }));
            circuit.r("r1", "in", "out", ohms);
            circuit.c("c1", "out", "0", farads);

            let mut record = Record::default();
            let summary = run(&circuit, &analysis, &mut record, &Control::default())
                .map_err(|diag| format!("tau={tau:e} R={ohms}, {mode}: transient failed: {diag}"))?;
            let out = record.names.iter().position(|name| name == "out")
                .ok_or_else(|| format!("tau={tau:e} R={ohms}, {mode}: no output voltage recorded"))?;
            assert_eq!(record.at.len(), record.x.len(), "tau={tau:e} R={ohms}, {mode}: every time has a solution");
            assert_eq!(record.at.first(), Some(&0.0), "tau={tau:e} R={ohms}, {mode}: starts at the operating point");
            assert_eq!(record.at.last(), Some(&tstop), "tau={tau:e} R={ohms}, {mode}: reaches 300 us");

            let mut worst = (0.0_f64, 0.0_f64);
            for (&t, values) in record.at.iter().zip(&record.x) {
                assert!(t.is_finite() && t >= 0.0 && t <= tstop, "tau={tau:e} R={ohms}, {mode}: recorded time {t:e} is in the reference interval");
                let voltage = values.get(out)
                    .ok_or_else(|| format!("tau={tau:e} R={ohms}, {mode}: missing output voltage at t={t:e}"))?;
                let error = (voltage - rc_at(t, tau, &corners)).abs();
                assert!(error.is_finite(), "tau={tau:e} R={ohms}, {mode}: finite voltage error at t={t:e}");
                if error > worst.0 {
                    worst = (error, t);
                }
            }
            // the step lands on every corner, so the error at each ramp's end (the kink) is measured
            let mut missing = Vec::new();
            for cycle in 0..3 {
                let end = f64::from(cycle) * period + ramp;
                if !record.at.iter().any(|&t| (t - end).abs() <= 1e-15 * end) {
                    missing.push(end);
                }
            }
            let st = summary.stats;
            let exact_status = if exact {
                summary.exact.as_ref().map_or_else(
                    || ", Exact report missing".to_owned(),
                    |report| format!(", runs {}, stop {:?}", report.runs, report.stop),
                )
            } else {
                String::new()
            };
            println!(
                "tau={tau:e} R={ohms} {mode}: worst {:.3e} at {:.6e} s (chgtol/C {floor:.1e}), points {}, steps {}, rejected {} (newton {}, crossing {}, provisional {}, error {}), iterations {}{exact_status}",
                worst.0, worst.1, record.at.len(), st.steps, st.rejected, st.rejected_newton, st.rejected_crossing,
                st.rejected_provisional, st.rejected_error, st.iterations,
            );
            if exact {
                if let Some(report) = &summary.exact {
                    for entry in &report.trail {
                        println!("tau={tau:e} R={ohms} {mode}: {entry:?}");
                    }
                }
                // Exact stops on agreement, never on its reltol limit or a failed run, within:
                // - 4 runs for the 1 ohm ns poles: Fast at 1e-4 is already inside the target, so
                //   agreement is due at run 3 or 4 (ESIM-Q5; measured 3);
                // - 3 runs for the 10 us pole, which every step resolves (measured 3);
                // - 5 runs for the floor case: bound by chgtol / C, its first runs close less than
                //   a decade each (measured 0.019, 0.013, 1.6e-3, 1.6e-4).
                let max_runs = if ohms != 1.0 { 5 } else if tau >= 1e-6 { 3 } else { 4 };
                if !summary.exact.as_ref().is_some_and(|report| {
                    report.reached && report.stop == ExactStop::Reached && report.runs <= max_runs
                }) {
                    failures.push(format!(
                        "tau={tau:e} R={ohms} {mode}: must stop on agreement within {max_runs} runs: {:?}",
                        summary.exact,
                    ));
                }
            }
            if !missing.is_empty() {
                failures.push(format!("tau={tau:e} R={ohms} {mode}: no point on the rising ramps' ends at {missing:?}"));
            }
            // from rest at 1e-2: trtol * reltol of the 1 V full scale, by definition
            let bound = match from_rest {
                Some(reltol) => defaults.trtol * reltol,
                None if control || exact => 1e-3,
                None => 1.25 * floor,
            };
            if worst.0 > bound {
                failures.push(format!("tau={tau:e} R={ohms} {mode}: worst {:.3e} at {:.6e} s exceeds {bound:.1e}", worst.0, worst.1));
            }
            // a pole four times faster than the first step after a corner: the retry lands in one
            if tau == 125e-12 && st.rejected_provisional > 3 * 12 {
                failures.push(format!("tau={tau:e} R={ohms} {mode}: {} provisional rejections exceed 3 per corner over 12 corners", st.rejected_provisional));
            }
            if from_rest.is_some() {
                let corners = summary.profile.as_ref().map_or(&[][..], |p| p.corners.as_slice());
                let worst = corners.iter().max_by_key(|c| c.attempts);
                println!("tau={tau:e} R={ohms} {mode}: {} corners, most attempts {:?}", corners.len(), worst.map(|c| (c.t, c.attempts)));
                if corners.len() < 12 || worst.is_some_and(|c| c.attempts > 2) {
                    failures.push(format!("tau={tau:e} R={ohms} {mode}: a corner from rest takes at most one provisional rejection: {} corners, most {:?}", corners.len(), worst.map(|c| (c.t, c.attempts))));
                }
            }
        }
    }
    assert!(failures.is_empty(), "RC under 5 ns PWM edges against the closed form: {failures:#?}");
    Ok(())
}

#[test]
fn a_tolerance_that_needs_steps_under_the_old_step_floor_finishes() {
    let period = 1e-4;
    let ramp = 5e-9;
    let tstop = 3e-4;
    let tau = 0.5e-9;
    let mut corners = vec![(0.0, 0.0)];
    for cycle in 0..3 {
        let start = f64::from(cycle) * period;
        corners.extend([
            (start + ramp, 1.0),
            (start + 0.5 * period, 1.0),
            (start + 0.5 * period + ramp, 0.0),
            (f64::from(cycle + 1) * period, 0.0),
        ]);
    }
    // Exact's run at 1e-11: reltol and the charge floor both tightened 1e-8 from Fast's defaults
    // (exact.rs). After a floor-bound corner its first step is a few fs, under the old floor
    // 1e-9 * max_step (max_step defaults to tstop / 50), where the run used to fail.
    let mut circuit = SimCircuit::new();
    circuit.opts.exact = false;
    circuit.opts.reltol *= 1e-8;
    circuit.opts.chgtol *= 1e-8;
    circuit.v("v1", "in", "0", Source::wave(Wave::Pulse {
        v1: 0.0, v2: 1.0, td: 0.0, tr: ramp, tf: ramp,
        pw: 0.5 * period - ramp, per: period,
    }));
    circuit.r("r1", "in", "out", 1.0);
    circuit.c("c1", "out", "0", tau);

    let analysis = Analysis::Tran(TranSpec { tstop, max_step: None, uic: false });
    let mut record = Record::default();
    let summary = run(&circuit, &analysis, &mut record, &Control::default())
        .expect("a tight tolerance must finish at the representable step");
    let out = record.names.iter().position(|name| name == "out")
        .expect("output voltage recorded");
    let st = summary.stats;
    println!(
        "Fast reltol=1e-11: points {}, steps {}, floor_steps {}, rejected {} (newton {}, crossing {}, provisional {}, error {})",
        record.at.len(), st.steps, st.floor_steps, st.rejected, st.rejected_newton,
        st.rejected_crossing, st.rejected_provisional, st.rejected_error,
    );
    assert_eq!(record.at.last(), Some(&tstop), "reaches 300 us");
    let old_floor = 1e-9 * tstop / 50.0;
    let shortest = record.at.windows(2).map(|w| w[1] - w[0]).fold(f64::INFINITY, f64::min);
    assert!(shortest < old_floor, "the run steps under the old floor {old_floor:e} s (shortest {shortest:e} s), so it exercises the change");
    assert_eq!(record.at.len(), record.x.len(), "every time has a solution");
    for (&t, values) in record.at.iter().zip(&record.x) {
        let error = (values[out] - rc_at(t, tau, &corners)).abs();
        assert!(error <= 1e-3, "t={t:e}: voltage error {error:e} exceeds 1e-3");
    }
}

/// The 3.3 V PWM into a pad: 25 ohm and 5 pF, a 125 ps pole under 5 ns ramps. After a corner the
/// first step is many time constants long, and backward Euler damps the pole correctly; the step
/// control must step over it instead of cutting the step down to it, and trapezoidal must not
/// carry its remainder as a sawtooth along the flats.
#[test]
fn a_stiff_pole_after_a_corner_is_stepped_over() -> Result<(), String> {
    let period = 1e-4;
    let ramp = 5e-9;
    let tstop = 300e-6;
    let full_scale = 3.3;
    let ohms = 25.0;
    let farads = 5e-12;
    let tau = ohms * farads;
    let mut corners = vec![(0.0, 0.0)];
    for cycle in 0..3 {
        let start = f64::from(cycle) * period;
        corners.extend([
            (start + ramp, full_scale),
            (start + 0.5 * period, full_scale),
            (start + 0.5 * period + ramp, 0.0),
            (f64::from(cycle + 1) * period, 0.0),
        ]);
    }
    // the drive and the exact current into the capacitor at t
    let vin = |t: f64| {
        let k = corners.partition_point(|&(at, _)| at <= t).max(1) - 1;
        match corners.get(k + 1) {
            Some(&(t1, v1)) => corners[k].1 + (v1 - corners[k].1) * (t - corners[k].0) / (t1 - corners[k].0),
            None => corners[k].1,
        }
    };
    let analysis = Analysis::Tran(TranSpec { tstop, max_step: None, uic: false });
    // provisional plus error rejections as measured (Fast 4 and 0, Exact 23 over its runs), with
    // room for one corner's worth (3)
    for (exact, reltol, max_rejections) in [(false, 1e-3, 7), (false, 1e-2, 3), (true, 1e-3, 26)] {
        let mode = if exact { "Exact" } else { "Fast" };
        let mut circuit = SimCircuit::new();
        circuit.opts.exact = exact;
        circuit.opts.reltol = reltol;
        let tol = circuit.opts.clone();
        circuit.v("v1", "in", "0", Source::wave(Wave::Pulse {
            v1: 0.0, v2: full_scale, td: 0.0, tr: ramp, tf: ramp,
            pw: 0.5 * period - ramp, per: period,
        }));
        circuit.r("r1", "in", "out", ohms);
        circuit.c("c1", "out", "0", farads);

        let mut record = Record::default();
        let summary = run(&circuit, &analysis, &mut record, &Control::default())
            .map_err(|diag| format!("stiff pole {mode} reltol={reltol:e}: transient failed: {diag}"))?;
        let st = summary.stats;
        println!(
            "stiff pole {mode} reltol={reltol:e}: points {}, steps {}, rejected {} (newton {}, crossing {}, provisional {}, error {}), iterations {}",
            record.at.len(), st.steps, st.rejected, st.rejected_newton, st.rejected_crossing,
            st.rejected_provisional, st.rejected_error, st.iterations,
        );
        assert!(st.rejected_provisional + st.rejected_error <= max_rejections,
            "stiff pole {mode} reltol={reltol:e}: {} provisional plus {} error rejections exceed {max_rejections}",
            st.rejected_provisional, st.rejected_error);
        let out = record.names.iter().position(|name| name == "out")
            .ok_or_else(|| format!("stiff pole {mode} reltol={reltol:e}: no output voltage recorded"))?;
        assert_eq!(record.at.len(), record.x.len(), "stiff pole {mode} reltol={reltol:e}: every time has a solution");
        assert_eq!(record.at.first(), Some(&0.0), "stiff pole {mode} reltol={reltol:e}: starts at the operating point");
        assert_eq!(record.at.last(), Some(&tstop), "stiff pole {mode} reltol={reltol:e}: reaches three periods");

        let mut flat_samples = vec![0; corners.len() - 1];
        let mut first_samples = 0;
        let mut later_samples = 0;
        let mut last_corner = usize::MAX;
        let mut prev = 0.0;
        for (&t, values) in record.at.iter().zip(&record.x) {
            assert!(t.is_finite() && t >= 0.0 && t <= tstop,
                "stiff pole {mode} reltol={reltol:e}: recorded time {t:e} is in the reference interval");
            let voltage = values.get(out)
                .ok_or_else(|| format!("stiff pole {mode} reltol={reltol:e}: missing output voltage at t={t:e}"))?;
            let error = (voltage - rc_at(t, tau, &corners)).abs();
            assert!(error.is_finite(), "stiff pole {mode} reltol={reltol:e}: finite voltage error at t={t:e}");
            let corner = corners.partition_point(|&(at, _)| at <= t).max(1) - 1;
            let age = t - corners[corner].0;
            let first = age > 0.0 && corner != last_corner;
            if age > 0.0 {
                last_corner = corner;
            }
            if exact {
                assert!(error <= 1e-3 * full_scale,
                    "stiff pole Exact: error {error:e} V at t={t:e} exceeds 0.1 % of full scale");
            } else if first || (age > 0.0 && age <= 2e-9) {
                // the step's row allowance in volts on the capacitor, as the error test takes it:
                // trtol * max(reltol * |v| over the step, chgtol / C, (reltol * |i| + abstol) * h / C)
                let h = t - prev;
                let (v0, v1) = (rc_at(prev, tau, &corners), rc_at(t, tau, &corners));
                let (i0, i1) = ((vin(prev) - v0) / ohms, (vin(t) - v1) / ohms);
                let allowance = tol.trtol * (tol.reltol * v0.abs().max(v1.abs())).max(tol.chgtol / farads)
                    .max((tol.reltol * i0.abs().max(i1.abs()) + tol.abstol) * h / farads);
                if first {
                    first_samples += 1;
                    assert!(error <= allowance,
                        "stiff pole {mode} reltol={reltol:e}: first sample after the corner at {:e} s, at t={t:e}: error {error:e} V exceeds its row allowance {allowance:e} V",
                        corners[corner].0);
                } else {
                    assert!(error <= allowance,
                        "stiff pole {mode} reltol={reltol:e}: error {error:e} V at t={t:e}, {age:e} s after a corner, exceeds its row allowance {allowance:e} V");
                }
            } else {
                later_samples += 1;
                assert!(error <= 1e-3 * full_scale,
                    "stiff pole {mode} reltol={reltol:e}: error {error:e} V at t={t:e}, {age:e} s after a corner, exceeds 0.1 % of full scale");
            }
            for (i, segment) in corners.windows(2).enumerate() {
                if segment[0].1 == segment[1].1 && t >= segment[0].0 + 100e-9 && t <= segment[1].0 {
                    flat_samples[i] += 1;
                    // the steps after a stepped-over pole stay backward Euler: trapezoidal carrying
                    // its remainder rings at 5e-5 to 8e-5 of full scale here, the held steps leave
                    // 1.5e-6 to 2.2e-6
                    assert!(error <= 1e-5 * full_scale,
                        "stiff pole {mode} reltol={reltol:e}: flat error {error:e} V at t={t:e} exceeds 1e-5 of full scale after 100 ns, a sawtooth");
                }
            }
            prev = t;
        }
        if !exact {
            assert!(first_samples == corners.len() - 1 && later_samples > 0,
                "stiff pole {mode} reltol={reltol:e}: a first sample after each of the {} corners ({first_samples}) and samples later than 2 ns ({later_samples})",
                corners.len() - 1);
        }
        for (i, segment) in corners.windows(2).enumerate() {
            if segment[0].1 == segment[1].1 {
                assert!(flat_samples[i] > 0,
                    "stiff pole {mode} reltol={reltol:e}: no sample on the flat from {:?} after its first 100 ns", segment);
            }
        }
    }
    Ok(())
}

/// A supply rail's branch currents follow a dense reference after a load step: within 10 x reltol
/// of their full scale at 1e-4, 1e-3 and 1e-2, with the error at 1e-3 at most a third of the error
/// at 1e-2 (ESIM-Q9). Two currents: the source's branch, and the one through the resistor between
/// the two capacitor nodes (no branch unknown). A corner's first sample is exempt: a stiff pole
/// after a corner is stepped over and its first sample gets the row allowance.
#[test]
fn a_rail_current_follows_a_dense_reference_after_corners() {
    let (td, tr, tf, pw) = (1e-6, 5e-9, 5e-9, 5e-6 - 5e-9);
    let tstop = 11e-6;
    let corners = [td, td + tr, td + tr + pw, td + tr + pw + tf];
    // A cable into a regulator input (a 100 ns mode) and its output capacitance (about 300 ns),
    // with a 2 to 41 mA load step of 5 ns edges, one period.
    let currents = |reltol: f64, max_step: Option<f64>| {
        let mut c = SimCircuit::new();
        c.opts.reltol = reltol;
        c.v("vs", "src", "0", Source::dc(5.0));
        c.r("rs", "src", "a", 0.1);
        c.c("ca", "a", "0", 1e-6);
        c.r("rm", "a", "b", 0.05);
        c.c("cb", "b", "0", 6e-6);
        c.i("iload", "b", "0", Source::wave(Wave::Pulse { v1: 2e-3, v2: 41e-3, td, tr, tf, pw, per: 10e-6 }));
        let mut record = Record::default();
        run(&c, &Analysis::Tran(TranSpec { tstop, max_step, uic: false }), &mut record, &Control::default())
            .unwrap_or_else(|diag| panic!("reltol={reltol:e}: transient failed: {diag}"));
        let at = |name: &str| record.names.iter().position(|n| n == name).unwrap_or_else(|| panic!("no {name} recorded"));
        let (vs, a, b) = (at("vs#branch"), at("a"), at("b"));
        let i: Vec<[f64; 2]> = record.x.iter().map(|x| [x[vs], (x[a] - x[b]) / 0.05]).collect();
        (record.at, i)
    };
    let names = ["the source branch", "the 0.05 ohm resistor"];
    let (rt, ri) = currents(1e-8, Some(1e-9));
    let fs = [0, 1].map(|k| ri.iter().map(|i| i[k].abs()).fold(0.0, f64::max));
    let reference = |t: f64, k: usize| {
        let hi = rt.partition_point(|&s| s < t).clamp(1, rt.len() - 1);
        let f = (t - rt[hi - 1]) / (rt[hi] - rt[hi - 1]);
        ri[hi - 1][k] + f * (ri[hi][k] - ri[hi - 1][k])
    };
    let mut failures = Vec::new();
    let mut worst = [[(0.0f64, 0.0f64); 2]; 3];
    for (case, reltol) in [1e-4, 1e-3, 1e-2].into_iter().enumerate() {
        let (at, i) = currents(reltol, Some(10e-6));
        // each corner's first sample: the first point after it
        let first = corners.map(|corner| at.partition_point(|&t| t <= corner));
        for (p, &t) in at.iter().enumerate().filter(|(p, _)| !first.contains(p)) {
            for k in 0..2 {
                let e = ((i[p][k] - reference(t, k)) / fs[k]).abs();
                if e > worst[case][k].0 {
                    worst[case][k] = (e, t);
                }
            }
        }
        for k in 0..2 {
            let (e, t) = worst[case][k];
            println!("reltol={reltol:e} {}: worst {e:.3e} of full scale {:.3e} A at {t:e} s, points {}", names[k], fs[k], at.len());
            if e > 10.0 * reltol {
                failures.push(format!("reltol={reltol:e}: {} within 10 x reltol of full scale: {e:.3e} at {t:e} s", names[k]));
            }
        }
    }
    for k in 0..2 {
        let (tight, loose) = (worst[1][k].0, worst[2][k].0);
        if tight > loose / 3.0 {
            failures.push(format!("{}: the error at 1e-3 ({tight:.3e}) at most a third of the error at 1e-2 ({loose:.3e})", names[k]));
        }
    }
    assert!(failures.is_empty(), "rail currents after corners: {failures:#?}");
}

/// Two corners closer than the step a failed step is retried at are one instant: they are crossed,
/// not landed on. A 1 mA PWL step into 1 kOhm, coupled by 1 uF to another 1 kOhm, its corners
/// 5e-20 s apart (the retry step 1e-9 * max_step is 1e-17 s): a step to the second corner has
/// a0 C = 2e13 S, which rounds both nodes' 1e-3 S away, and either node's pivot cancels to zero.
/// That step failed singular at every retry and in the gmin ladder, all clamped to the same gap
/// (a boundary source's corner at an exporter's restart hit it first). Both nodes follow the
/// closed form, in Exact and in Fast: the step at the corner couples R I / 2 across, and the
/// capacitor charges with 2 R C.
#[test]
fn corners_closer_than_the_retry_step_are_one_instant() {
    let (t1, gap, tstop, max_step) = (1e-7, 5e-20, 4e-7, 1e-8);
    let (amps, ohms, farads) = (1e-3, 1e3, 1e-6);
    let mut failures = Vec::new();
    for exact in [true, false] {
        let mut c = SimCircuit::new();
        c.opts.exact = exact;
        c.i("i1", "0", "a", Source::wave(Wave::Pwl(vec![(0.0, 0.0), (t1, 0.0), (t1 + gap, amps), (tstop, amps)])));
        c.r("ra", "a", "0", ohms);
        c.c("cx", "a", "x", farads);
        c.r("rx", "x", "0", ohms);
        let analysis = Analysis::Tran(TranSpec { tstop, max_step: Some(max_step), uic: false });
        let mut record = Record::default();
        let mode = if exact { "Exact" } else { "Fast" };
        if let Err(d) = run(&c, &analysis, &mut record, &Control::default()) {
            failures.push(format!("{mode}: {}", d.message));
            continue;
        }
        assert_eq!(record.at.last(), Some(&tstop), "{mode}: reaches the stop");
        let tau = 2.0 * ohms * farads;
        let x_at = |t: f64| if t <= t1 { 0.0 } else { 0.5 * ohms * amps * (-(t - t1) / tau).exp() };
        let a_at = |t: f64| if t <= t1 { 0.0 } else { x_at(t) + ohms * amps * (1.0 - (-(t - t1) / tau).exp()) };
        for (node, want) in [("a", &a_at as &dyn Fn(f64) -> f64), ("x", &x_at)] {
            let k = record.names.iter().position(|n| n == node).expect("node recorded");
            let worst = record.at.iter().zip(&record.x).filter(|(&t, _)| t <= t1 || t >= t1 + gap).map(|(&t, x)| (x[k] - want(t)).abs()).fold(0.0, f64::max);
            if worst > 1e-3 {
                failures.push(format!("{mode} {node}: worst error {worst:.3e} V over 1e-3"));
            }
        }
    }
    assert!(failures.is_empty(), "corners 5e-20 s apart: {failures:#?}");
}

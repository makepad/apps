use fab_elec_sim::*;
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};

#[test]
fn thermal_noise_gives_kt_over_c() {
    let wave = Wave::Noise { seed: 1, sigma: 1.0, dt: 1.0, base: 1.0 };
    let same = wave.clone();
    let other = Wave::Noise { seed: 2, sigma: 1.0, dt: 1.0, base: 1.0 };
    assert_eq!(wave, same, "cloning noise must preserve waveform equality");
    let count = 200_000;
    let (mut sum, mut squares, mut different) = (0.0, 0.0, 0);
    for k in 0..count {
        let t = k as f64;
        let value = wave.at(t).expect("noise must have a value at every sample");
        let repeated = same.at(t).expect("the same seed must have a sample");
        assert_eq!(value.to_bits(), repeated.to_bits(), "same-seed reproducibility at sample {k}");
        let independent = other.at(t).expect("another seed must have a sample");
        different += usize::from(value.to_bits() != independent.to_bits());
        sum += value;
        squares += value * value;
    }
    let mean = sum / count as f64;
    let variance = squares / count as f64 - mean * mean;
    assert!(mean.abs() < 0.01, "standard normal mean: {mean:e}");
    assert!((variance - 1.0).abs() < 0.02, "standard normal variance: {variance:e}");
    assert!(different > 9 * count / 10, "distinct seeds must differ at most samples: {different}/{count}");
    let left = wave.at(1234.0).expect("left interpolation endpoint must exist");
    let right = wave.at(1235.0).expect("right interpolation endpoint must exist");
    let halfway = wave.at(1234.5).expect("half-step noise value must exist");
    assert!((halfway - 0.5 * (left + right)).abs() < 1e-12, "half-step noise must average its neighbours");
    assert_eq!(wave.at(-1.0), wave.at(0.0), "noise before zero must hold sample zero");
    assert_eq!(wave.next_break(0.0), f64::INFINITY, "noise must not schedule breakpoints");

    let mut variances = [0.0; 2];
    for (i, dt) in [1e-7, 5e-8].into_iter().enumerate() {
        let mut c = SimCircuit::new();
        c.r("r1", "out", "0", 1e6);
        c.c("c1", "out", "0", 1e-12);
        // The default charge floor (far above this circuit's charges) leaves the steps to max_step.
        let want = 1.380649e-23 * (c.temp_c + 273.15) / 1e-12;
        assert!(c.thermal_noise("r1", dt, 7), "a valid resistor must accept thermal noise at dt={dt:e}");
        let mut record = Record::default();
        let analysis = Analysis::Tran(TranSpec { tstop: 1e-3, max_step: Some(dt), uic: true });
        run(&c, &analysis, &mut record, &Control::default())
            .unwrap_or_else(|diag| panic!("thermal-noise transient failed at dt={dt:e}: {diag}"));
        assert_eq!(record.at.last().copied(), Some(1e-3), "thermal-noise transient must reach 1 ms at dt={dt:e}");

        // Discard ten RC time constants, then weight both moments by elapsed time.
        let start = record.at.partition_point(|&t| t < 1e-5);
        let times = &record.at[start..];
        let voltages = record.v("out");
        let values = &voltages[start..];
        assert!(times.len() >= 2, "thermal-noise transient needs samples after settling at dt={dt:e}");
        let duration = times[times.len() - 1] - times[0];
        assert!(duration > 0.0, "thermal-noise observation duration must be positive at dt={dt:e}");
        let mean = times.windows(2).zip(values.windows(2))
            .map(|(t, v)| 0.5 * (t[1] - t[0]) * (v[0] + v[1])).sum::<f64>() / duration;
        let variance = times.windows(2).zip(values.windows(2))
            .map(|(t, v)| 0.5 * (t[1] - t[0]) * ((v[0] - mean).powi(2) + (v[1] - mean).powi(2)))
            .sum::<f64>() / duration;
        assert!((variance / want - 1.0).abs() < 0.15, "kT/C variance at dt={dt:e}: {variance:e}, expected {want:e}");
        assert!(mean.abs() < 3.0 * want.sqrt() / 10.0, "thermal-noise mean at dt={dt:e}: {mean:e}, rms {}", want.sqrt());
        variances[i] = variance;
    }
    assert!((variances[1] / variances[0] - 1.0).abs() < 0.15, "halving dt must preserve kT/C variance: {variances:?}");
}

/// The noise a source makes at a longer spacing is the same noise, band-limited: a 48 kHz value
/// is the mean of the four 192 kHz draws it spans, and over a stretch both tile (1/300 s) the
/// values' integral is the same at 192, 48 and 44.1 kHz (whose values cover cells in part).
#[test]
fn noise_at_a_longer_spacing_is_the_same_noise_band_limited() {
    let fine_dt = 1.0 / 192_000.0;
    let fine = Wave::Noise { seed: 3, sigma: 1.0, dt: fine_dt, base: fine_dt };
    let at = |w: &Wave, t: f64| w.at(t).expect("noise has a value at every time");
    let fine_area: f64 = (0..640).map(|j| at(&fine, j as f64 * fine_dt)).sum::<f64>() * fine_dt;
    for rate in [48_000.0, 44_100.0] {
        let dt = 1.0 / rate;
        // the same density: sigma^2 dt
        let coarse = Wave::Noise { seed: 3, sigma: (fine_dt / dt).sqrt(), dt, base: fine_dt };
        let n = (rate / 300.0).round() as usize;
        let area: f64 = (0..n).map(|k| at(&coarse, k as f64 * dt)).sum::<f64>() * dt;
        assert!((area - fine_area).abs() < 1e-12 * fine_dt * 640.0, "{rate} Hz: the stretch's integral {area:e} is the 192 kHz one's {fine_area:e}");
        if rate == 48_000.0 {
            for k in 0..n {
                let mean = (0..4).map(|j| at(&fine, (4 * k + j) as f64 * fine_dt)).sum::<f64>() / 4.0;
                assert!((at(&coarse, k as f64 * dt) - mean).abs() < 1e-12, "48 kHz value {k} is the mean of the four 192 kHz draws it spans");
            }
        }
    }
}

#[test]
fn live_noise_draws_once_a_sample_period() {
    // A source lowered at 192 kHz, run at a rate above, at and below its own.
    let dt = 1.0 / 192_000.0;
    let mut circuit = SimCircuit::new();
    circuit.r("load", "out", "0", 1e6);
    assert!(circuit.thermal_noise("load", dt, 7), "the load has thermal current noise");
    // No capacitance: at a noise knot the voltage variance is sigma^2 R^2 = 2 k T R / dt.
    let exact_variance = 2.0 * 1.380649e-23 * (circuit.temp_c + 273.15) * 1e6 / dt;
    // The sample period alone bounds these runs, well below the instrument's bandwidth step.
    let spec = LiveSpec { rate: f64::INFINITY, trace: false, max_step: 1.0, ..LiveSpec::default() };
    // with no rate chosen a run falls to its finest noise spacing, else to its bandwidth step
    let close = |a: f64, b: f64| (a / b - 1.0).abs() < 1e-12;
    let sim = LiveSim::new(circuit.clone(), ValuePlan::default(), 1, spec.clone()).expect("the noisy load starts");
    assert!(close(sim.sample_rate(), 192e3), "no rate chosen: the noise source's own spacing, {} Hz", sim.sample_rate());
    let mut quiet = SimCircuit::new();
    quiet.r("load", "out", "0", 1e6);
    let sim = LiveSim::new(quiet, ValuePlan::default(), 1, LiveSpec { max_step: 1e-4, ..spec.clone() }).expect("the quiet load starts");
    assert!(close(sim.sample_rate(), 1e4), "no rate chosen and no noise: the bandwidth step, {} Hz", sim.sample_rate());

    let count = 4096;
    let mut densities = Vec::new();
    let mut fewest = usize::MAX;
    // each run's knot values, by knot
    let mut knots: Vec<std::collections::HashMap<u64, f64>> = Vec::new();
    // the last run is in Fast: the rate means the same in both modes
    for (rate, res) in [(384_000.0, 0.0), (192_000.0, 0.0), (48_000.0, 1e-4)] {
        let period = 1.0 / rate;
        let mut sim = LiveSim::new(circuit.clone(), ValuePlan::default(), 1, LiveSpec { sample_rate: rate, res, ..spec.clone() })
            .expect("the noisy load starts");
        assert_eq!(sim.step_cap(), period, "{rate} Hz: no step is longer than a sample period");
        let restarts = sim.restarts().change;
        sim.apply(LiveCmd::SetRes(if res > 0.0 { 0.0 } else { 1e-4 }));
        sim.apply(LiveCmd::SetRes(res));
        assert_eq!(sim.step_cap(), period, "{rate} Hz: the mode leaves the step bound alone");
        assert_eq!(sim.restarts().change, restarts, "{rate} Hz: the mode leaves the noise alone");

        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        sim.values(1.0);
        while sim.t() < count as f64 * period {
            sim.advance(count as f64 * period, None, &mut record).expect("the noisy load steps");
        }
        // With no stored charge these runs take the period, at the noise knots. Check the
        // phase too: interpolation points have a smaller, phase-dependent variance.
        assert!(record.at.iter().all(|t| (t / period - (t / period).round()).abs() < 1e-6),
            "{rate} Hz: accepted points lie on the noise knots");
        let values = record.v("out");
        assert!(values.len() >= count, "{rate} Hz: enough accepted noise samples");
        fewest = fewest.min(values.len());
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
        let want = exact_variance * dt / period;
        assert!((variance / want - 1.0).abs() < 0.125, "{rate} Hz: accepted-knot voltage variance {variance:e} matches thermal noise {want:e}");
        densities.push(variance * period);
        knots.push(record.at.iter().zip(&values).map(|(t, v)| ((t / period).round() as u64, *v)).collect());
    }
    // the source's draws stay at its own spacing: the 48 kHz run hears the 192 kHz run's noise,
    // each knot the mean of the four it spans (the 384 kHz run, finer than the source, draws its own)
    let (fine, coarse) = (&knots[1], &knots[2]);
    let mut compared = 0;
    for (&k, &v) in coarse {
        let Some(four) = (0..4).map(|j| fine.get(&(4 * k + j)).copied()).collect::<Option<Vec<f64>>>() else { continue };
        let mean = four.iter().sum::<f64>() / 4.0;
        assert!((v - mean).abs() <= 1e-9 * exact_variance.sqrt(), "48 kHz knot {k}: {v:e} is the mean of the 192 kHz run's four, {mean:e}");
        compared += 1;
    }
    assert!(compared >= count / 4 - 2, "the 48 kHz knots compared: {compared}");
    // The relative standard deviation of two independent Gaussian variance estimates is
    // approximately 2 / sqrt(N): allow four of those (the fixed seed makes runs reproducible).
    let tolerance = 8.0 / (fewest as f64).sqrt();
    assert!(densities.iter().all(|d| (d / densities[1] - 1.0).abs() < tolerance),
        "every rate keeps the source's density sigma^2 dt: {densities:?}, relative tolerance {tolerance}");
}

/// A noise source in a block on its own bound (steps of several sample periods: the control block
/// in rounds) reads at the bound's cell, the same noise band-limited at the same density, rather
/// than at whichever of its knots the steps end on. A small noise current on a 5 V node behind
/// 1 kOhm and 1 uF (1 ms), filtered once more by the same (so what is compared lies well under the
/// cell's rate), is stepped on a 4-period bound (83 us; truncation-error control shortens some
/// steps). The run is the run on the cell form given outright, and it follows a reference stepped
/// at a sixteenth of the noise's spacing: as large, and in step once the cell's lead is allowed
/// for (a cell's value is the mean of the draws it spans, placed at its start, half a cell ahead
/// of them; a draw is half a period ahead: 1.5 periods between). What stays apart is the band
/// limit (about 2 % of the noise, stepped finely) and the bound's steps across the cell's knots
/// (about 13 %); unrelated noise would be 140 % apart. Read at the steps' ends alone, the source
/// has about the steps' length over the period times its density: twice the noise, 180 % apart.
#[test]
fn a_noise_source_on_an_own_bound_reads_at_its_cell() {
    use fab_elec_sim::analysis::tran::{Accepted, Stepper};
    let period = 1.0 / 48_000.0;
    let (bound, settle, t_end, sigma) = (4.0 * period, 5e-3, 60e-3, 1e-6);
    let circuit = |noise: Wave| {
        let mut c = SimCircuit::new();
        c.v("vs", "s", "0", Source::dc(5.0));
        c.r("r", "s", "n", 1e3);
        c.c("c", "n", "0", 1e-6);
        // no DC value: the operating point is the 5 V, not a first draw's millivolt the runs then
        // forget
        c.i("inoise", "0", "n", Source { dc: Some(0.0), ..Source::wave(noise) });
        c.r("r2", "n", "m", 1e3);
        c.c("c2", "m", "0", 1e-6);
        c
    };
    let fine = circuit(Wave::Noise { seed: 11, sigma, dt: period, base: period });
    let cell = circuit(Wave::Noise { seed: 11, sigma: sigma * (period / bound).sqrt(), dt: bound, base: period });
    // the filtered noise (m's departure from 5 V) at each accepted point: on the own bound in Fast,
    // or the reference
    let run = |c: &SimCircuit, own: bool| {
        let mut s = Solver::new(c).expect("the circuit builds");
        let m = s.unknown("m").expect("the filtered node") as usize;
        let mut st = Stepper::start(&mut s, false, if own { bound } else { period / 16.0 }).expect("the operating point converges");
        if own {
            st.set_res(&mut s, period);
            st.set_own_bound(&mut s, bound, period);
        }
        let mut points = Vec::new();
        while st.t() < t_end {
            st.step(&mut s, t_end, &mut |s: &Solver, a: &Accepted| points.push((a.t, s.x[m] - 5.0))).expect("the transient runs");
        }
        points
    };
    let (reference, own, given) = (run(&fine, false), run(&fine, true), run(&cell, true));
    let at = |points: &[(f64, f64)], t: f64| {
        let hi = points.partition_point(|p| p.0 < t).clamp(1, points.len() - 1);
        let (a, b) = (points[hi - 1], points[hi]);
        a.1 + (t - a.0) / (b.0 - a.0) * (b.1 - a.1)
    };
    // all read a period apart (a run on the bound between its points as it steps, linearly)
    let grid: Vec<f64> = (0..).map(|k| settle + k as f64 * period).take_while(|&t| t <= t_end - 2.0 * period).collect();
    let rms = |f: &dyn Fn(f64) -> f64| (grid.iter().map(|&t| f(t).powi(2)).sum::<f64>() / grid.len() as f64).sqrt();
    let step = (t_end - settle) / own.iter().filter(|p| p.0 > settle).count() as f64;
    let (swing, want) = (rms(&|t| at(&own, t)), rms(&|t| at(&reference, t + 1.5 * period)));
    let (apart, off_form) = (rms(&|t| at(&own, t) - at(&reference, t + 1.5 * period)), rms(&|t| at(&own, t) - at(&given, t)));
    println!("steps of {:.2} periods on average: the filtered noise {swing:.4e} V rms against the reference's {want:.4e} V, apart by {apart:.4e} V rms, from the cell form's run by {off_form:.4e} V rms", step / period);
    assert!(step > 2.0 * period, "the run steps across the noise's knots: {:.2} periods a step on average", step / period);
    assert!(off_form <= 1e-9 * want, "the source reads as its cell form: apart from the run on that form by {off_form:.4e} V rms");
    assert!((swing / want - 1.0).abs() < 0.05, "the filtered noise is as large as the reference's: {swing:.4e} V rms against {want:.4e} V");
    assert!(apart < 0.2 * want, "the filtered noise follows the reference's: apart by {apart:.4e} V rms, the reference's {want:.4e} V");
}

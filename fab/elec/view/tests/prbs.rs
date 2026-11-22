use fab_elec_view::trace::eye::eye_metrics;
use fab_elec_instr::prbs::Prbs;
use std::f64::consts::PI;

#[test]
fn deterministic_prbs_channel_preserves_sequence_and_eye() {
    let source = Prbs::gigabit();
    let quiet = Prbs { noise: 0.0, ..source.clone() };
    let clean = Prbs { jitter: 0.0, ..quiet.clone() };
    let fast = Prbs { zeta: 0.9, wn: 40.0 * PI * source.bit_rate, ..clean.clone() };
    let ui = source.ui();
    let height = source.hi - source.lo;

    assert_eq!(ui, 1e-9, "gigabit unit interval is one nanosecond");
    assert!((source.ring_freq() / 0.9e9 - 1.0).abs() < 1e-15, "gigabit ringing frequency is 0.9 GHz");
    for order in [7, 9] {
        let sequence = Prbs { order, ..source.clone() };
        let period = (1usize << order) - 1;
        let bits: Vec<_> = (0..period + order as usize).map(|k| sequence.bit(k as u64)).collect();
        let first = &bits[..order as usize];
        for k in 1..period {
            assert_ne!(&bits[k..k + order as usize], first, "PRBS{order} has no earlier state repeat at {k}");
        }
        assert_eq!(&bits[period..], first, "PRBS{order} repeats its first state at its exact period");
        let ones = bits[..period].iter().filter(|&&bit| bit).count();
        assert_eq!(ones, 1 << (order - 1), "PRBS{order} has the maximal-sequence count of ones");
        assert_eq!(period - ones, (1 << (order - 1)) - 1, "PRBS{order} has the maximal-sequence count of zeros");
    }
    for (order, tap) in [(7, 6), (9, 5), (15, 14), (23, 18), (31, 28)] {
        let sequence = Prbs { order, ..clean.clone() };
        for k in 0..64 {
            assert_eq!(sequence.bit(k + u64::from(order)), sequence.bit(k + tap) ^ sequence.bit(k), "PRBS{order} follows its polynomial");
            assert_eq!(sequence.bit(k + (1u64 << order) - 1), sequence.bit(k), "PRBS{order} reduces bit indices modulo its period");
        }
    }

    let record = source.record(8192);
    assert_eq!(record, source.record(8192), "same seed gives identical record times and values");
    assert_ne!(record, Prbs { seed: source.seed + 2, ..source.clone() }.record(8192), "another seed changes the record");
    assert_eq!(source.member(0), Prbs { seed: source.seed + 1, ..source.clone() }, "member changes only its seed");
    assert_ne!(record, source.member(0).record(8192), "member zero differs from the base record");
    assert_eq!(source.record(0), (Vec::new(), Vec::new()), "empty record has no samples");
    assert_eq!(source.record(1).0, vec![0.0], "single-sample record starts at zero");
    assert_eq!(quiet.value(-ui), source.hi, "before zero the source holds bit zero's level");
    assert_eq!(source.edge_time(0), 0.0, "boundary zero has no jitter");

    // Include every polynomial and jitter large enough to reorder transitions.
    for signal in [quiet.clone(), Prbs { order: 9, ..quiet.clone() }, Prbs { order: 15, ..quiet.clone() },
        Prbs { order: 23, ..quiet.clone() }, Prbs { order: 31, ..quiet.clone() }, Prbs { jitter: 3.0 * ui, ..quiet.clone() }]
    {
        let (times, values) = signal.record(4096);
        assert_eq!(times.len(), 4096, "record returns the requested number of times");
        assert_eq!(values.len(), times.len(), "record has one value per time");
        assert_eq!(times[0], 0.0, "record starts at zero");
        assert!(times.windows(2).all(|pair| pair[1] > pair[0]), "record times strictly increase, including reordered jitter");
        for (&time, &value) in times.iter().zip(&values) {
            assert!((f64::from(value) - signal.value(time)).abs() < 1e-6, "PRBS{} record agrees with value at {time}", signal.order);
        }
        let last = times[times.len() - 1];
        let end = ((last + 4.0 * signal.jitter.abs()) / ui).ceil() as u64;
        for k in 1..=end {
            let edge = signal.edge_time(k);
            if signal.bit(k) != signal.bit(k - 1) && edge >= 0.0 && edge <= last {
                let at = times.partition_point(|&time| time < edge);
                assert_eq!(times[at].to_bits(), edge.to_bits(), "every transition is an exact record breakpoint");
            }
        }
    }

    let (times, values) = fast.record(6000);
    let last = times[times.len() - 1];
    let fine = (ui / 32.0).min(1.0 / (24.0 * fast.ring_freq()));
    let settling = 8.0 / (fast.zeta * fast.wn);
    let mut last_edge = 0.0;
    let mut boundary = 1;
    let mut saw_growth = false;
    for pair in times.windows(2) {
        while boundary as f64 * ui <= pair[0] {
            if fast.bit(boundary) != fast.bit(boundary - 1) {
                last_edge = boundary as f64 * ui;
            }
            boundary += 1;
        }
        let step = pair[1] - pair[0];
        assert!(step <= ui / 8.0, "record steps respect the coarse bound");
        if pair[0] - last_edge <= settling {
            assert!(step <= fine, "record uses fine steps throughout channel settling");
        } else {
            saw_growth |= step > fine;
        }
    }
    assert!(saw_growth, "settled runs grow beyond the fine step");
    for k in 0..=(last / ui - 0.5).floor() as u64 {
        let middle = (k as f64 + 0.5) * ui;
        let at = times.partition_point(|&time| time < middle);
        let nearest = if middle - times[at - 1] < times[at] - middle { at - 1 } else { at };
        let level = if fast.bit(k) { fast.hi } else { fast.lo };
        assert!((f64::from(values[nearest]) - level).abs() < 0.001, "fast channel settles to bit {k}'s level at its middle");
    }

    let a = clean.zeta * clean.wn * ui;
    let bound = height * (-a / 2.0).exp() / ((1.0 - (-a).exp()) * (1.0 - clean.zeta * clean.zeta).sqrt());
    let mut worst: f64 = 0.0;
    for k in 0..254 {
        let level = if clean.bit(k) { clean.hi } else { clean.lo };
        let deviation = (clean.value((k as f64 + 0.5) * ui) - level).abs();
        worst = worst.max(deviation);
        assert!(deviation < bound, "eye opening follows the closed-form envelope at bit {k}: {deviation} < {bound}");
    }
    assert!(worst > 0.0, "gigabit channel has nonzero intersymbol interference");
    for order in [7, 9, 15, 23, 31] {
        let signal = Prbs { order, ..clean.clone() };
        let period = (1u64 << order) - 1;
        for k in [40, 63, 95] {
            let early = signal.value((k as f64 + 0.5) * ui);
            let late = signal.value(((period * 1000 + k) as f64 + 0.5) * ui);
            assert!((early - late).abs() < 0.001, "PRBS{order} jump-ahead preserves the waveform at distant timestamps");
        }
    }

    let jittered = Prbs { jitter: 10e-12, ..clean };
    let variance = (1..=10_000).map(|k| (jittered.edge_time(k) - k as f64 * ui).powi(2)).sum::<f64>() / 10_000.0;
    let rms = variance.sqrt();
    assert!((rms / jittered.jitter - 1.0).abs() < 0.05, "transition jitter RMS is within 5% of 10 ps: {rms}");
}

#[test]
fn eye_metrics_recover_injected_jitter_and_noise() {
    let source = Prbs::gigabit();
    let clean = Prbs { jitter: 0.0, noise: 0.0, ..source.clone() };
    let fast = Prbs { zeta: 0.9, wn: 40.0 * PI * source.bit_rate, ..clean.clone() };
    let ui = source.ui();
    let amplitude = source.hi - source.lo;
    // Even the ringing channel's fine steps cover over 2000 crossings.
    let metrics = |signal: &Prbs| {
        let (times, values) = signal.record(131_072);
        eye_metrics(&times, &values, ui).expect("PRBS record has measurable eye levels and crossings")
    };

    let ideal = metrics(&fast);
    assert!(ideal.jitter_pp < 0.01 * ui, "fast channel crossing jitter is below 1% of a unit interval: {}", ideal.jitter_pp);
    assert!((ideal.height - amplitude).abs() < 0.01 * amplitude, "fast channel eye height is within 1% of the level separation: {}", ideal.height);
    assert!((ideal.width - ui).abs() < 0.01 * ui, "fast channel eye width is within 1% of a unit interval: {}", ideal.width);

    let jittered = metrics(&Prbs { jitter: 6e-12, ..fast.clone() });
    assert!((jittered.jitter_rms / 6e-12 - 1.0).abs() < 0.1, "crossing jitter RMS recovers 6 ps within 10%: {}", jittered.jitter_rms);
    let noisy = metrics(&Prbs { noise: 0.004, ..fast });
    assert!((noisy.height - (amplitude - 6.0 * 0.004)).abs() < 0.004, "eye height recovers the six-sigma noise penalty within 4 mV: {}", noisy.height);

    let ringing = metrics(&clean);
    let full = metrics(&source);
    assert!(ringing.jitter_pp > 0.0, "ringing channel has crossing jitter from intersymbol interference: {}", ringing.jitter_pp);
    assert!(ringing.height < amplitude, "ringing channel intersymbol interference reduces eye height: {}", ringing.height);
    assert!(full.jitter_rms > ringing.jitter_rms, "injected jitter and noise increase crossing jitter RMS: {} > {}", full.jitter_rms, ringing.jitter_rms);
    // Noise is compared on the same transitions: against the ringing's intersymbol spread (tens of mV), 4 mV
    // of noise lowers the height by a fraction of a millivolt, less than differently jittered records scatter.
    let noisy_ringing = metrics(&Prbs { noise: 0.004, ..clean.clone() });
    assert!(noisy_ringing.height < ringing.height, "noise on the same transitions reduces eye height: {} < {}", noisy_ringing.height, ringing.height);
    assert!(full.height > 0.0, "gigabit eye remains vertically open: {}", full.height);
    assert!(full.width > 0.0, "gigabit eye remains horizontally open: {}", full.width);

    let (times, values) = source.record(256);
    for invalid_ui in [0.0, -ui, f64::NAN] {
        assert!(eye_metrics(&times, &values, invalid_ui).is_none(), "invalid unit interval is rejected: {invalid_ui}");
    }
    let constant = vec![source.hi as f32; times.len()];
    assert!(eye_metrics(&times, &constant, ui).is_none(), "constant record has no measurable eye");
}

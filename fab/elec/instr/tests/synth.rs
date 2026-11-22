use fab_elec_instr::synth::{Spread, Synth, XTALK_DECAY, XTALK_RING};

#[test]
fn deterministic_board_variation_preserves_clean_sources() {
    let plain = Synth::pwm();
    let quiet = Synth { noise: 0.0, ..plain.clone() };
    let wobbled = Synth { wobble: 0.05, ..quiet.clone() };
    let repeated = wobbled.clone();
    let other_seed = Synth {
        seed: wobbled.seed + 1,
        ..wobbled.clone()
    };
    let runts = Synth { runt: 0.25, ..quiet.clone() };
    let coupled = Synth {
        xtalk: 0.1,
        xtalk_freq: 61_803.0,
        ..quiet.clone()
    };
    let spread = Spread {
        wn: 0.03,
        zeta: 0.2,
        height: 0.015,
    };
    let period = plain.period();
    let height = plain.hi - plain.lo;
    let before_falling = |cycle: usize| f64::from_bits((cycle as f64 * period + plain.duty * period).to_bits() - 1);

    assert_eq!(
        (plain.wobble, plain.runt, plain.xtalk, plain.xtalk_freq),
        (0.0, 0.0, 0.0, 0.0),
        "default source disables all new variation"
    );
    assert_eq!(
        plain.member(Spread::default(), 3),
        Synth {
            seed: plain.seed + 3 + 1,
            ..plain.clone()
        },
        "zero spread preserves the old member construction"
    );

    let levels: Vec<_> = (0..400).map(|cycle| wobbled.value(before_falling(cycle))).collect();
    let rms = (levels.iter().map(|&value| ((value - quiet.lo) / height - 1.0).powi(2)).sum::<f64>() / levels.len() as f64).sqrt();
    assert!((0.04..0.06).contains(&rms), "pulse height RMS follows wobble: {rms}");
    for cycle in (0..levels.len()).rev() {
        assert_eq!(
            repeated.value(before_falling(cycle)).to_bits(),
            levels[cycle].to_bits(),
            "pulse heights repeat regardless of call order"
        );
    }
    assert!(
        levels
            .iter()
            .enumerate()
            .any(|(cycle, value)| value.to_bits() != other_seed.value(before_falling(cycle)).to_bits()),
        "another seed changes pulse heights"
    );

    let mut runt_count = 0;
    for cycle in 0..2000 {
        let relative = (runts.value(before_falling(cycle)) - runts.lo) / height;
        if relative < 0.85 {
            runt_count += 1;
            assert!((0.55..=0.80).contains(&relative), "runt height stays between 55% and 80%: {relative}");
        }
    }
    let fraction = runt_count as f64 / 2000.0;
    assert!((0.2..0.3).contains(&fraction), "runt frequency follows its probability: {fraction}");

    let (times, _) = coupled.record(2500);
    let last = times[times.len() - 1];
    let phase = coupled.xtalk_phase();
    let spacing = 1.0 / (2.0 * coupled.xtalk_freq);
    let fine = (1.0 / (24.0 * coupled.ring_freq())).min(period / 64.0).min(1.0 / (12.0 * XTALK_RING));
    assert!(phase > 0.0 && phase < spacing, "clock phase lies inside one edge spacing");
    assert!(
        times.windows(2).all(|pair| pair[1] > pair[0]),
        "coupled breakpoints keep record times strictly increasing"
    );
    let mut edge_index = 0;
    let mut signs = [false; 2];
    loop {
        let edge = phase + edge_index as f64 * spacing;
        if edge > last {
            break;
        }
        let at = times.partition_point(|&time| time < edge);
        assert_eq!(times[at].to_bits(), edge.to_bits(), "record samples every coupled edge exactly");
        for pair in times[at..].windows(2).take_while(|pair| pair[0] - edge <= 4.0 * XTALK_DECAY) {
            assert!(pair[1] - pair[0] <= fine, "coupled burst steps stay within the fine bound for four decays");
        }
        let offset = edge % period;
        let falling = coupled.duty * period;
        if offset > 1e-6 && (offset - falling).abs() > 1e-6 && period - offset > 1e-6 && edge + 4.0 * XTALK_DECAY <= last {
            let level = if offset < falling { coupled.hi } else { coupled.lo };
            let mut peak: f64 = 0.0;
            for tap in 0..=128 {
                let deviation = coupled.value(edge + 4.0 * XTALK_DECAY * tap as f64 / 128.0) - level;
                if deviation.abs() > peak.abs() {
                    peak = deviation;
                }
            }
            let polarity = if edge_index % 2 == 0 { 1.0 } else { -1.0 };
            assert!(
                (peak / (polarity * coupled.xtalk) - 1.0).abs() < 0.02,
                "coupled peaks have the specified height and alternating polarity: {peak}"
            );
            signs[edge_index % 2] = true;
        }
        edge_index += 1;
    }
    assert!(signs.iter().all(|&seen| seen), "settled stretches cover both coupled edge polarities");

    // A power-of-two period makes this falling edge exactly equal to the clock phase.
    let aligned = Synth {
        freq: 65_536.0,
        xtalk_freq: 131_072.0,
        ..coupled.clone()
    };
    let aligned = Synth {
        duty: aligned.xtalk_phase() / aligned.period(),
        ..aligned
    };
    let (aligned_times, _) = aligned.record(800);
    let common_edge = aligned.duty * aligned.period();
    assert_eq!(common_edge, aligned.xtalk_phase(), "coincident edge setup shares an exact breakpoint");
    assert_eq!(
        aligned_times.iter().filter(|&&time| time == common_edge).count(),
        1,
        "coincident clock and PWM edges produce one sample"
    );
    assert!(
        aligned_times.windows(2).all(|pair| pair[1] > pair[0]),
        "coincident breakpoints keep record times strictly increasing"
    );

    let (plain_times, _) = plain.record(5000);
    let (wobbled_times, _) = wobbled.record(5000);
    assert!(
        plain_times.iter().zip(&wobbled_times).all(|(a, b)| a.to_bits() == b.to_bits()),
        "pulse variation without coupling preserves every record time bit"
    );

    let mut squares = [0.0; 3];
    for index in 0..400 {
        let member = quiet.member(spread, index);
        assert_eq!(member, quiet.member(spread, index), "member tolerances are deterministic per index");
        for (square, factor) in squares
            .iter_mut()
            .zip([member.wn / quiet.wn, member.zeta / quiet.zeta, (member.hi - member.lo) / height])
        {
            *square += factor.ln().powi(2);
        }
    }
    for ((name, setting), square) in [("wn", spread.wn), ("zeta", spread.zeta), ("height", spread.height)].into_iter().zip(squares) {
        let rms = (square / 400.0).sqrt();
        assert!((rms / setting - 1.0).abs() < 0.15, "member {name} log-factor RMS follows its spread: {rms}");
    }
}

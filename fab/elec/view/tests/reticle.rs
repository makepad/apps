//! The hover and answer geometry share the worker's closed-form PWM features.
use fab_elec_bench::measure_index::RecordIndex;
use fab_elec_view::instruments::{
    axis::si_label,
    reticle::{answer_geometry, reticle_label, snap, Geometry},
    scrub::measurement_rows,
    simfeed::{lane_features, Feature, FeatureKind},
};
use std::sync::Arc;

#[test]
fn pwm_reticle_and_answer_geometry() {
    const PERIOD: f64 = 100e-6;
    const DELAY: f64 = 10e-6;
    const RAMP: f64 = 2.5e-9; // Two ns from ten to ninety percent.
    const STEP: f64 = RAMP / 20.0;
    const TOP: f64 = 3.3;
    // Dense only on the ramps; sparse flat stretches keep this fixture small and exact.
    let mut t = vec![0.0];
    let mut y = vec![0.0f32];
    for cycle in 0..4 {
        let start = DELAY + cycle as f64 * PERIOD;
        for (offset, rising) in [(0.0, true), (PERIOD * 0.3, false)] {
            for step in 0..=20 {
                t.push(start + offset + step as f64 * STEP);
                let level = step as f64 / 20.0;
                y.push((TOP * if rising { level } else { 1.0 - level }) as f32);
            }
            let end = if rising { 30 } else { 70 };
            for step in 1..end {
                t.push(start + offset + step as f64 * 1e-6);
                y.push(if rising { TOP as f32 } else { 0.0 });
            }
        }
    }
    let rows = measurement_rows(&t, &y, 0.1);
    let rise_answer = rows.iter().find(|row| row.name == "rise").expect("PWM has a measured rise answer").value;
    let index = RecordIndex::new(Arc::from(t.as_slice()), Arc::from(y.as_slice()));
    let trigger = DELAY + PERIOD * 0.5;
    let features = lane_features(&t, &y, &index, trigger);
    let rise = features.iter().find(|f| f.kind == FeatureKind::Rise && f.t > 0.0).expect("post-trigger rise exists");
    assert!(features.windows(2).all(|pair| pair[0].t <= pair[1].t), "PWM feature times are ordered for binary search");
    assert_eq!(snap(&features, rise.t + 0.5e-9, 1e-9), Some(rise), "a rise within the radius snaps to its fifty-percent crossing");
    assert_eq!(snap(&features, rise.t + 2e-9, 1e-9), None, "no feature outside the radius snaps");
    assert_eq!(snap(&features, rise.t, 0.0), Some(rise), "an exact feature hit accepts a zero radius");
    assert_eq!(snap(&[], 0.0, 1.0), None, "an empty capture has no snap");
    assert_eq!(snap(&features, rise.t, -1.0), None, "a negative radius has no snap");
    let top = Feature { kind: FeatureKind::Top, t: 2.0, v: TOP, crossings: None };
    let edge = Feature { t: 0.0, ..*rise };
    assert_eq!(snap(&[edge, top], 1.0, 1.0), Some(&edge), "the earlier edge wins an equidistant level tie");
    let tied = [Feature { t: 0.0, ..top }, Feature { t: 2.0, ..edge }];
    assert_eq!(snap(&tied, 1.0, 1.0), Some(&tied[1]), "the later edge also wins an equidistant level tie");
    let tied = [Feature { t: 0.0, ..top }, edge];
    assert_eq!(snap(&tied, 0.0, 0.0), Some(&tied[1]), "coincident features retain the edge preference");
    let Some(Geometry::Period { t0, t1 }) = answer_geometry("freq", &features) else { panic!("frequency has period geometry") };
    assert!((t1 - t0 - PERIOD).abs() <= STEP, "frequency spans exactly one 100 us period within one edge sample");
    assert_eq!(t0, rise.t, "frequency chooses the first complete post-trigger cycle");
    let Some(Geometry::High { t0, t1 }) = answer_geometry("duty", &features) else { panic!("duty has high-span geometry") };
    assert!((t1 - t0 - PERIOD * 0.3).abs() <= STEP, "duty spans exactly 30 us within one edge sample");
    assert_eq!(t0, rise.t, "duty chooses the first complete post-trigger cycle");
    let Some(Geometry::Levels { top, base }) = answer_geometry("Vpp", &features) else { panic!("Vpp has level geometry") };
    assert!((top - TOP).abs() < 1e-5 && base.abs() < 1e-5, "Vpp marks the PWM top and base levels");
    assert!(features.iter().any(|f| f.kind == FeatureKind::Top && f.v == top)
        && features.iter().any(|f| f.kind == FeatureKind::Base && f.v == base), "Vpp uses the actual feature levels");
    let Some(Geometry::Edge { t10, t90, v10, v90 }) = answer_geometry("rise", &features) else { panic!("rise has crossing geometry") };
    assert!((t90 - t10 - rise_answer).abs() <= STEP, "rise geometry agrees with the measured rise answer");
    assert!((t90 - t10 - 2e-9).abs() <= STEP, "rise geometry spans the closed-form two ns transition");
    assert_eq!([t10, t90], [rise.crossings.unwrap()[0], rise.crossings.unwrap()[2]], "rise uses the first complete post-trigger ten/ninety crossings");
    assert!((v10 - TOP * 0.1).abs() < 1e-5 && (v90 - TOP * 0.9).abs() < 1e-5, "edge ticks sit at ten and ninety percent of base to top");
    assert_eq!([v10, v90], [base + (top - base) * 0.1, base + (top - base) * 0.9], "edge tick levels are derived from the top and base features");
    assert_eq!(answer_geometry("mean", &features), None, "an unsupported answer has no geometry");
    assert_eq!(answer_geometry("freq", &[]), None, "an empty capture has no answer geometry");
    let next_rise = features.iter().enumerate().filter(|(_, f)| f.kind == FeatureKind::Rise).nth(1).unwrap().0;
    assert_eq!(answer_geometry("duty", &features[..next_rise]), None, "a high pulse without the next rise is not a complete duty cycle");
    let shift = features.last().unwrap().t + PERIOD;
    let before: Vec<_> = features.iter().map(|f| Feature {
        t: f.t - shift, crossings: f.crossings.map(|times| times.map(|t| t - shift)), ..*f
    }).collect();
    let Some(Geometry::Period { t0, t1 }) = answer_geometry("freq", &before) else { panic!("pre-trigger fallback has period geometry") };
    assert_eq!(t0, before.iter().find(|f| f.kind == FeatureKind::Rise).unwrap().t, "without a post-trigger cycle frequency uses the first cycle");
    assert!((t1 - t0 - PERIOD).abs() <= STEP, "pre-trigger fallback still spans one complete period");
    let fall = features.iter().find(|f| f.kind == FeatureKind::Fall).unwrap();
    assert_eq!(reticle_label(Some(rise), 0.0, 0.0, "V"), "rise 2 ns", "rise label names its ten-to-ninety duration");
    assert_eq!(reticle_label(Some(fall), 0.0, 0.0, "V"), "fall 2 ns", "fall label uses a positive ninety-to-ten duration");
    let top = Feature { kind: FeatureKind::Top, t: 0.0, v: TOP, crossings: None };
    let base = Feature { kind: FeatureKind::Base, v: 0.0, ..top };
    assert_eq!(reticle_label(Some(&top), 0.0, 0.0, "V"), "top 3.3 V", "top label uses the answer's trimmed SI precision");
    assert_eq!(reticle_label(Some(&base), 0.0, 0.0, "V"), "base 0 V", "base label names the zero level");
    assert_eq!(reticle_label(None, 12.5e-6, 1.65, "V"), "t 12.5 µs  1.65 V", "free label shows trigger-relative time and pointer value");
    assert_eq!(reticle_label(None, -12.5e-6, -0.0033, "A"), format!("t {}  {}", si_label(-12.5e-6, "s"), si_label(-0.0033, "A")), "free labels keep SI prefixes, signs and the lane's unit");
}

//! Dwell histograms, sparse frame reuse and waveform-weighted persistence.

use fab_elec_view::instruments::phosphor::*;
use fab_elec_instr::synth::Synth;

/// Blends a frame of `n` whole waveforms over three columns, which must all blend alike (the picture's
/// waveform-weighted mean); None when nothing blends.
fn whole(persistence: &mut Persistence, n: u64, dt: f64, tau: f64) -> Option<Blend> {
    let mut out = Vec::new();
    if !persistence.frame(3, n, &[], dt, tau, &mut out) {
        return None;
    }
    assert!(out.len() == 3 && out.iter().all(|b| *b == out[0]), "whole waveforms blend every column alike: {out:?}");
    assert!((0..3).all(|c| persistence.weight(c) == persistence.weight(0)), "and leave every column the same weight");
    Some(out[0])
}

fn column_sum(hits: &Hits, col: u32) -> f64 {
    (0..hits.h).map(|row| hits.at(col, row) as f64).sum()
}

fn check_dirty(hits: &Hits) {
    for c in 0..hits.w {
        for r in 0..hits.h {
            if hits.at(c, r) != 0.0 {
                let d = hits.dirty;
                assert!(
                    c >= d.c0 && c < d.c1 && r >= d.r0 && r < d.r1,
                    "dirty rectangle misses nonzero texel ({c}, {r})"
                );
            }
        }
    }
}

#[test]
fn dwell_and_persistence_count_waveforms() {
    let map = PhosphorMap::screen(64, 48, 0.0, 0.6e-6, -0.5, 6.0);
    let mut hits = Hits::new(map.w, map.h);
    let mut raster = Raster::new();
    assert!(hits.dirty.is_empty(), "a new histogram has no dirty pixels");
    for seed in 1..=256 {
        let source = Synth { seed, ..Synth::pwm() };
        let (t, y) = source.record(256);
        assert!(
            t[t.len() - 1] >= map.t_left + map.t_px * map.w as f64,
            "the seeded acquisition must cross the full plot"
        );
        raster.add(&map, &t, &y, &mut hits);
    }
    assert_eq!(hits.waveforms, 256, "every seeded acquisition counts once");
    for c in 0..map.w {
        let sum = column_sum(&hits, c);
        assert!((sum / 256.0 - 1.0).abs() <= 1e-5, "noisy PWM column {c} holds {sum} counts instead of 256");
    }
    let row_of = |v: f64| ((map.v_top - v) / map.v_px) as u32;
    let above_top: f64 = (0..map.w)
        .flat_map(|c| (0..row_of(4.0)).map(move |r| (c, r)))
        .map(|(c, r)| hits.at(c, r) as f64)
        .sum();
    assert!(above_top > 0.0, "the overshoot must be represented above the flat top");
    assert!(hits.at(map.w - 1, row_of(3.3)) > 100.0, "the settled flat top must gather the dwell");
    check_dirty(&hits);
    let allocation = hits.data.as_ptr();
    hits.reset(map.w, map.h);
    assert_eq!(allocation, hits.data.as_ptr(), "same-size reset reuses the allocation");
    assert!(
        hits.data.iter().all(|v| *v == 0.0) && hits.dirty.is_empty() && hits.waveforms == 0,
        "reset must clear all written dwell and metadata"
    );

    let map = PhosphorMap::screen(4, 4, 0.0, 1.0, 0.0, 1.0);
    hits.reset(4, 4);
    raster.add(&map, &[0.0, 1.0], &[0.5, 0.5], &mut hits);
    for c in 0..4 {
        assert_eq!(
            (hits.at(c, 1), hits.at(c, 2), column_sum(&hits, c)),
            (0.5, 0.5, 1.0),
            "a flat beam splits once in column {c}"
        );
    }
    hits.reset(4, 4);
    raster.add(&map, &[0.0, 0.5, 0.5 + 1e-9, 1.0], &[0.1, 0.1, 0.9, 0.9], &mut hits);
    for c in 0..4 {
        assert!(
            (column_sum(&hits, c) - 1.0).abs() <= 1e-6,
            "a fast edge still contributes one count to column {c}"
        );
    }
    assert!(hits.data.iter().all(|v| *v >= 0.0 && *v <= 1.0), "edge joints must not count twice");
    hits.reset(4, 4);
    raster.add(&map, &[0.26, 0.74], &[1.0, 1.0], &mut hits);
    assert_eq!(
        (column_sum(&hits, 0), column_sum(&hits, 1), column_sum(&hits, 2), column_sum(&hits, 3)),
        (0.0, 1.0, 1.0, 0.0),
        "a clipped flat beam normalizes only the columns it crosses"
    );
    assert_eq!(hits.dirty, Dirty { c0: 1, c1: 3, r0: 0, r1: 1 }, "stroke marks only its written span");

    let mut other = Hits::new(4, 4);
    other.data[4] = 2.0;
    other.data[15] = 3.0;
    other.waveforms = 7;
    other.mark(1, 2, 0, 1);
    other.mark(3, 4, 3, 4);
    let expected: Vec<_> = hits.data.iter().zip(&other.data).map(|(a, b)| a + b).collect();
    let union = hits.dirty.union(other.dirty);
    assert_eq!(Dirty::default().union(union), union, "an empty rectangle is a union identity");
    assert_eq!(union.union(Dirty { c0: 8, c1: 7, r0: 0, r1: 1 }), union, "inverted rectangles are empty");
    hits.add(&other);
    assert_eq!(hits.data, expected, "worker frames add elementwise");
    assert_eq!(hits.waveforms, 8, "worker waveform counts add");
    assert_eq!(hits.dirty, union, "worker dirty rectangles union");
    check_dirty(&hits);
    hits.reset(4, 4);
    assert!(
        hits.data.iter().all(|v| *v == 0.0) && hits.dirty.is_empty(),
        "merged frame reset leaves a zero buffer and empty rectangle"
    );
    // Data outside another frame's rectangle is stale and must not be added.
    other.data[0] = 123.0;
    hits.add(&other);
    assert_eq!(hits.data[0], 0.0, "add must ignore stale data outside the other rectangle");
    hits.reset(8, 2);
    assert_eq!(
        (hits.w, hits.h, hits.data.len(), hits.waveforms),
        (8, 2, 16, 0),
        "resize updates dimensions and waveform count"
    );
    assert!(
        hits.data.iter().all(|v| *v == 0.0) && hits.dirty.is_empty(),
        "resize clears the whole buffer even at the same length"
    );

    let mut persistence = Persistence::default();
    let _ = whole(&mut persistence, 20, 0.1, 2.0);
    persistence.clear();
    assert_eq!(persistence.weight(0), 0.0, "clear forgets waveform weight");
    assert_eq!(
        whole(&mut persistence, 10, 0.1, 2.0),
        Some(Blend { keep: 0.0, scale: 0.1 }),
        "the first frame after clear replaces the picture"
    );
    assert_eq!(whole(&mut persistence, 0, 0.2, 2.0), None, "an empty frame draws no accumulation");
    assert_eq!(persistence.weight(0), 10.0, "empty frames retain pending decay");
    let blend = whole(&mut persistence, 5, 0.3, 2.0).unwrap();
    let kept = 10.0 * (-0.5f64 / 2.0).exp();
    assert_eq!(
        blend,
        Blend {
            keep: (kept / (kept + 5.0)) as f32,
            scale: (1.0 / (kept + 5.0)) as f32
        },
        "the next waveform frame must decay across both intervals"
    );
    assert!(
        (persistence.weight(0) - kept - 5.0).abs() < 1e-12,
        "weight must include pending decay and new waveforms"
    );
    assert_eq!(
        whole(&mut persistence, 4, 0.0, 0.0),
        Some(Blend { keep: 0.0, scale: 0.25 }),
        "zero tau replaces even at zero elapsed time"
    );

    persistence.clear();
    let (mut picture, mut counts, mut waveforms) = (0.0, 0.0, 0u64);
    for (n, count) in [(1, 1.0), (2000, 60.0), (3, 2.0), (57, 9.0)] {
        let blend = whole(&mut persistence, n, 0.1, f64::INFINITY).unwrap();
        picture = blend.keep as f64 * picture + blend.scale as f64 * count;
        counts += count;
        waveforms += n;
        let mean = counts / waveforms as f64;
        assert!(
            (picture - mean).abs() <= mean * 2e-7,
            "infinite persistence must be the waveform running mean: {picture} vs {mean}"
        );
        assert_eq!(
            persistence.weight(0),
            waveforms as f64,
            "infinite persistence keeps the exact total waveform count"
        );
    }

    persistence.clear();
    let tau = 0.7;
    let mut time = 0.0;
    let mut history = Vec::new();
    for i in 0..64 {
        let dt = 0.005 * (i % 7 + 1) as f64;
        let n = (i % 5 + 1) as u64;
        time += dt;
        history.push((time, n));
        let _ = whole(&mut persistence, n, dt, tau);
        let expected: f64 = history.iter().map(|(at, n)| *n as f64 * (-(time - at) / tau).exp()).sum();
        assert!(
            (persistence.weight(0) / expected - 1.0).abs() < 1e-12,
            "finite decay disagrees with the closed form at frame {i}"
        );
    }
    assert_eq!(grade_x(0.0, 100.0), 0.0, "no count gives no light");
    assert!(grade_x(10.0, 100.0) < grade_x(50.0, 100.0), "grading rises with counts");
    assert_eq!(grade_x(1e6, 100.0), 1.0, "grading saturates at full");
    assert_eq!(decay_keep(f64::INFINITY, 1.0 / 60.0), 1.0, "infinite decay keeps all weight");
    assert_eq!(decay_keep(0.0, 1.0 / 60.0), 0.0, "zero decay keeps no weight");
    assert!((decay_keep(0.5, 0.5) - (-1.0f64).exp()).abs() < 1e-15, "decay uses f64 exp(-dt / tau)");
}

#[test]
fn a_window_sweeps_its_settled_columns_and_each_column_decays_on_its_own() {
    // A window handed over while it draws: each publication takes the columns left of its pen, and
    // the parts are exactly the whole window's shares, each column once.
    let map = PhosphorMap::screen(16, 8, 0.0, 1.0, 0.0, 1.0);
    let (t, y): (Vec<f64>, Vec<f32>) = (0..=40).map(|k| (k as f64 / 40.0, (k as f32 * 0.37).sin() * 0.4 + 0.5)).unzip();
    let mut whole_window = Hits::new(map.w, map.h);
    Raster::new().add(&map, &t, &y, &mut whole_window);
    let mut stroke = Stroke::default();
    stroke.begin(&map, 0.0);
    let mut parts = vec![Hits::new(map.w, map.h), Hits::new(map.w, map.h), Hits::new(map.w, map.h)];
    for k in 1..t.len() {
        stroke.segment(t[k - 1], y[k - 1], t[k], y[k]);
        if k == 10 {
            stroke.sweep(&mut parts[0]);
            stroke.sweep(&mut parts[0]);
        }
        if k == 25 {
            stroke.sweep(&mut parts[1]);
        }
    }
    stroke.finish(&mut parts[2]);
    // the pen at sample 10 is at x = 4, at sample 25 at x = 10
    let runs: Vec<Vec<f32>> = parts.iter().map(|p| p.partial.clone()).collect();
    let ones = |a: usize, b: usize| (0..16).map(|c| if (a..b).contains(&c) { 1.0 } else { 0.0 }).collect::<Vec<f32>>();
    assert_eq!(runs, vec![ones(0, 4), ones(4, 10), ones(10, 16)], "each part takes the columns settled since the one before, once");
    assert_eq!(
        parts.iter().map(|p| (p.waveforms, p.partial_done, p.whole(), p.swept())).collect::<Vec<_>>(),
        vec![(0, 0, 0, true), (0, 0, 0, true), (1, 1, 0, true)],
        "the window completes in its last part and counts as swept, not whole"
    );
    let mut summed = Hits::new(map.w, map.h);
    for part in &parts {
        summed.add(part);
    }
    assert!(
        summed.data.iter().zip(&whole_window.data).all(|(a, b)| a.to_bits() == b.to_bits()),
        "the parts are the whole window's shares, bit for bit"
    );
    assert_eq!((summed.waveforms, summed.partial_done, summed.partial.clone(), summed.map), (1, 1, vec![1.0; 16], Some(map)), "summed: one window over every column");
    assert_eq!((whole_window.whole(), whole_window.partial.len()), (1, 0), "a window that never swept is whole");

    // Per column: a column a sweep has not reached keeps its picture, and decays over all the
    // time since its last waveform once one reaches it.
    let mut persistence = Persistence::default();
    let mut out = Vec::new();
    let (tau, keep) = (2.0, |dt: f64| (-dt / 2.0f64).exp());
    assert!(persistence.frame(3, 0, &[1.0, 1.0, 0.0], 0.1, tau, &mut out));
    assert_eq!(out, vec![Blend { keep: 0.0, scale: 1.0 }, Blend { keep: 0.0, scale: 1.0 }, Blend { keep: 1.0, scale: 0.0 }], "the swept columns replace, the rest keep");
    assert!(persistence.frame(3, 0, &[0.0, 0.0, 1.0], 0.2, tau, &mut out));
    assert_eq!(out[2], Blend { keep: 0.0, scale: 1.0 }, "the last column's first waveform replaces it");
    assert!(!persistence.frame(3, 0, &[], 0.05, tau, &mut out), "an empty frame blends nothing");
    assert!(persistence.frame(3, 1, &[], 0.25, tau, &mut out));
    let blend = |kept: f64| Blend { keep: (kept / (kept + 1.0)) as f32, scale: (1.0 / (kept + 1.0)) as f32 };
    assert_eq!(out, vec![blend(keep(0.5)), blend(keep(0.5)), blend(keep(0.3))], "each column decays over the time since its own last waveform");
    assert!((persistence.weight(0) - keep(0.5) - 1.0).abs() < 1e-12 && (persistence.weight(2) - keep(0.3) - 1.0).abs() < 1e-12);
    assert!(persistence.frame(3, 0, &[1.0, 0.0, 2.0], 0.0, f64::INFINITY, &mut out));
    let weights = [persistence.weight(0), persistence.weight(1), persistence.weight(2)];
    let want = [keep(0.5) + 2.0, keep(0.5) + 1.0, keep(0.3) + 3.0];
    assert!((0..3).all(|c| (weights[c] - want[c]).abs() < 1e-12), "partial counts add column by column: {weights:?}, expected {want:?}");
}

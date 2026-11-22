use fab_elec_sim::trace::{LogicEdge, LogicPage, SignalId, TraceStore, LOGIC_CHUNK};
use fab_elec_view::instruments::{
    phosphor::Hits,
    scope::ScopeSettings,
    scrub::{draw_logic_still, Still, StillScratch},
};

#[test]
fn logic_still_shows_timed_steps_and_column_bands() {
    let mut store = TraceStore::new(1, &[]);
    let mut closed = Vec::new();
    let (square, fast) = (SignalId(0), SignalId(1));
    for (id, entries, half_period) in [(square, 21, 0.5e-3), (fast, LOGIC_CHUNK, 5e-6)] {
        for i in 0..entries {
            store.push_logic(id, LogicEdge { t: i as f64 * half_period, high: i % 2 == 1 }, &mut closed);
        }
    }

    let settings = ScopeSettings { time_per_div: 1e-3, ..ScopeSettings::default() };
    let plot = (64, 32);
    let high = 3.3;
    let mut scratch = StillScratch::default();
    let mut hits = Hits::new(plot.0, plot.1);
    let trace = store.logic(square).expect("the recorded 1 kHz net");
    let (still, rows) = draw_logic_still(trace, 5e-3, high, &settings, plot, &mut scratch, &mut hits);
    assert!(matches!(still, Still::Drawn), "the 1 kHz trace draws");
    assert!(!hits.dirty.is_empty() && hits.waveforms == 1, "the square wave contributes one waveform");
    let value = |name| rows.iter().find(|row| row.name == name).map(|row| row.value);
    assert!(value("freq").is_some_and(|v| (v - 1000.0).abs() < 10.0), "the measured frequency is within 1% of 1 kHz");
    assert!(value("duty").is_some_and(|v| (v - 50.0).abs() < 0.5), "the square wave is high for half each cycle");
    assert!(value("width").is_some_and(|v| (v - 0.5e-3).abs() < 5e-6), "the high pulse lasts half a millisecond");

    let (still, _) = draw_logic_still(trace, -10e-3, high, &settings, plot, &mut scratch, &mut hits);
    assert!(matches!(still, Still::NoSamples) && hits.dirty.is_empty(), "the window before the first entry clears the picture");

    let trace = store.logic(fast).expect("the recorded 100 kHz net");
    let (still, rows) = draw_logic_still(trace, 5e-3, high, &settings, plot, &mut scratch, &mut hits);
    assert!(matches!(still, Still::Drawn), "the dense 100 kHz trace draws");
    let vpp = rows.iter().find(|row| row.name == "Vpp").expect("a dense trace reports Vpp");
    assert_eq!(vpp.value, f64::from(high), "dense strokes span both logic levels");
    assert!(vpp.n <= u64::from(3 * plot.0 + 2), "the dense raster uses at most three points per column plus its endpoints");
    assert!((0..plot.0).all(|col| hits.at(col, 10) > 0.0), "activity spans the space between levels in every dense column");

    assert!(store.page_logic(&LogicPage { id: fast, first: 0.0, chunk: None }), "the sealed chunk can be evicted");
    let trace = store.logic(fast).expect("the evicted trace keeps its summary");
    let last_edge = (LOGIC_CHUNK - 1) as f64 * 5e-6;
    let (still, _) = draw_logic_still(trace, last_edge, high, &settings, plot, &mut scratch, &mut hits);
    assert!(matches!(still, Still::Drawn) && hits.at(8, 10) > 0.0, "the unknown part of a spilled chunk draws busy");
    assert!(hits.at(56, 2) > 0.0 && hits.at(56, 10) == 0.0, "the last known high level holds past the spilled chunk");
}

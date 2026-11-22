//! A scope still from the record around the paused cursor, and the measurements shared with live captures.

use super::{
    phosphor::{Hits, Raster},
    scope::{Measurement, ScopeSettings},
    scope_host::volts,
    simfeed::least_swing,
};
use fab_elec_bench::{measure, measure_index::RecordIndex};
use std::sync::Arc;
use fab_elec_sim::trace::{LogicTrace, TraceStore, CHUNK, LOGIC_CHUNK};

/// Builds a record index and its rows in O(points + events log events); amplitude rows count samples.
pub fn measurement_rows(t: &[f64], y: &[f32], swing: f64) -> Vec<Measurement> {
    let Some(index) = record_index(t, y) else { return Vec::new() };
    indexed_rows(t, y, swing, &index)
}

// A still can contain vertical min/max strokes; only ordinary ordered records have an index.
pub(super) fn record_index(t: &[f64], y: &[f32]) -> Option<RecordIndex> {
    if t.len() < 2 || t.len() != y.len() || t.iter().any(|v| !v.is_finite())
        || y.iter().any(|v| !v.is_finite()) || !t.windows(2).all(|p| p[0] < p[1]) { return None; }
    Some(RecordIndex::new(Arc::from(t), Arc::from(y)))
}

/// Reuse the capture's index for its answers and features; duty readings are already percent.
pub(super) fn indexed_rows(t: &[f64], y: &[f32], swing: f64, index: &RecordIndex) -> Vec<Measurement> {
    let (from, to) = (t[0], t[t.len() - 1]);
    let read = |name: &str| index.reading(name, from, to);
    let vpp = read("pp");
    let edges = vpp.is_some_and(|vpp| vpp.value >= swing);
    let timing = |quantity: &str, name, unit| read(quantity).map(|r|
        Measurement { sigma: r.sigma, n: r.n as u64, ..Measurement::exact(0, name, r.value, unit) });
    let amplitude = |name, value: Option<f64>, unit| value.map(|value|
        Measurement { n: y.len() as u64, ..Measurement::exact(0, name, value, unit) });
    [
        amplitude("top", (index.top > index.base).then_some(index.top), "V"),
        amplitude("base", (index.top > index.base).then_some(index.base), "V"),
        timing("frequency", "freq", "Hz"),
        timing("duty", "duty", "%"),
        timing("width", "width", "s"),
        amplitude("Vpp", vpp.map(|r| r.value), "V"),
        amplitude("max", read("max").map(|r| r.value), "V"),
        amplitude("min", read("min").map(|r| r.value), "V"),
        timing("rise", "rise", "s"),
        measure::overshoot(t, y).map(|v| Measurement::exact(0, "overshoot", v * 100.0, "%")),
        amplitude("mean", read("mean").map(|r| r.value), "V"),
    ].into_iter().flatten().filter(|row| edges || matches!(row.name.as_str(), "Vpp" | "mean")).collect()
}

pub enum Still {
    Drawn,
    NoSamples,
}

/// Reused by one card; raw samples and min/max pairs make one waveform for the rasteriser.
#[derive(Default)]
pub struct StillScratch {
    raster: Raster,
    spectrum: super::spectrum::SpectrumStroke,
    t: Vec<f64>,
    y: Vec<f32>,
}

impl StillScratch {
    pub fn spectrum_measurements(&self) -> Option<super::spectrum::SpectrumMeasurements> { self.spectrum.measurements() }

    fn point(&mut self, time: f64, value: f32) {
        self.t.push(time);
        self.y.push(value);
    }

    // After rasterisation, give the measurement index a strictly increasing record.
    // The old level holds until the last representable instant before each step.
    fn measure_steps(&mut self, swing: f64) -> Vec<Measurement> {
        let (mut i, mut n) = (0, 0);
        while i < self.t.len() {
            let t = self.t[i];
            let old = self.y[i];
            i += 1;
            while i < self.t.len() && self.t[i] == t {
                i += 1;
            }
            let new = self.y[i - 1];
            if old != new && n > 0 && self.t[n - 1] < t.next_down() {
                self.t[n] = t.next_down();
                self.y[n] = old;
                n += 1;
            }
            self.t[n] = t;
            self.y[n] = new;
            n += 1;
        }
        self.t.truncate(n);
        self.y.truncate(n);
        measurement_rows(&self.t, &self.y, swing)
    }
}

/// Reads O(log(record) + points) times and values up to 16 * width points plus two neighbours;
/// dense windows read pyramid bins, partial chunks, chunks whose pyramid is not built yet (their
/// points, exact) and the open tail instead. Raster cost is
/// O(read points/bins + covered pixels); scratch/hits allocate only on growth, returned rows allocate.
pub fn draw_still(store: &TraceStore, row: usize, at: f64, settings: &ScopeSettings, _bracket: Option<(f64, f64)>, plot: (u32, u32), scratch: &mut StillScratch, hits: &mut Hits) -> (Still, Vec<Measurement>) {
    hits.reset(plot.0, plot.1);
    scratch.t.clear();
    scratch.y.clear();
    let window = settings.window();
    let (start, end) = (at - window.pre, at + window.post);
    let Some(signal) = store.signals.get(row) else { return (Still::NoSamples, Vec::new()) };
    if !at.is_finite() || !start.is_finite() || !end.is_finite() || end < start || store.is_empty() {
        return (Still::NoSamples, Vec::new());
    }
    let before = store.index_at(start).map(|i| i - store.base);
    if before.is_none() && store.index_at(at).is_none() {
        return (Still::NoSamples, Vec::new());
    }
    let Some(last) = store.index_at(end).map(|i| i - store.base) else { return (Still::NoSamples, Vec::new()) };
    // An exact hit at the left edge still needs its predecessor for the edge's line.
    let first = before.map_or(0, |i| if store.time.at(i) == start { i.saturating_sub(1) } else { i });
    let inside = before.map_or(0, |i| i + usize::from(store.time.at(i) < start));
    let dense = (last + 1).saturating_sub(inside) > 16 * plot.0 as usize;
    let stop = (last + 2).min(store.len());
    let map = settings.map(plot);
    let mut i = first;
    while i < stop {
        let c = i / CHUNK;
        if dense && i % CHUNK == 0 && c < signal.chunks.len() && signal.chunks[c].pyramid().is_some() && i >= inside && i + CHUNK <= last + 1 {
            // Pick the coarsest level whose bins fit a time pixel. Checking the bin endpoints
            // matters: solver steps are non-uniform, so sample count alone is not a width.
            let (mut level, mut bin) = (0, 4);
            while bin * 4 <= CHUNK {
                level += 1;
                bin *= 4;
            }
            while level > 0 && (0..CHUNK).step_by(bin).any(|b| store.time.at(i + b + bin - 1) - store.time.at(i + b) > map.t_px) {
                level -= 1;
                bin /= 4;
            }
            // Keep the exact endpoints so neighbouring raw segments reach this chunk.
            scratch.t.push(store.time.at(i) - at);
            scratch.y.push(signal.at(i));
            for (b, mm) in signal.chunks[c].level(level).unwrap_or_default().iter().enumerate() {
                let begin = i + b * bin;
                let (left, right) = (store.time.at(begin), store.time.at(begin + bin - 1));
                if right - left > map.t_px {
                    // Even four samples can span a wide gap; those sparse bins stay exact.
                    for p in begin..begin + bin {
                        scratch.t.push(store.time.at(p) - at);
                        scratch.y.push(signal.at(p));
                    }
                } else {
                    let t = (left - at) + (right - left) * 0.5;
                    scratch.t.extend_from_slice(&[t, t]);
                    scratch.y.extend_from_slice(&[mm.min, mm.max]);
                }
            }
            i += CHUNK;
            scratch.t.push(store.time.at(i - 1) - at);
            scratch.y.push(signal.at(i - 1));
        } else {
            scratch.t.push(store.time.at(i) - at);
            scratch.y.push(signal.at(i));
            i += 1;
        }
    }
    // Equal-time min/max pairs are vertical strokes. Rasterising them together keeps one
    // column's coverage normalised even when several bins land in it: this is one waveform.
    scratch.raster.add(&map, &scratch.t, &scratch.y, hits);
    hits.waveforms = u64::from(!hits.dirty.is_empty());
    let first = scratch.t.partition_point(|&t| t < -window.pre);
    let stop = scratch.t.partition_point(|&t| t <= window.post);
    let (t, y) = (&scratch.t[first..stop], &scratch.y[first..stop]);
    let rows = if dense {
        measure::min_max(y).map(|(lo, hi)| vec![
            Measurement::exact(0, "Vpp", hi - lo, "V"), Measurement::exact(0, "min", lo, "V"), Measurement::exact(0, "max", hi, "V"),
        ].into_iter().map(|mut row| { row.n = (last + 1 - inside) as u64; row }).collect()).unwrap_or_default()
    } else {
        measurement_rows(t, y, least_swing(volts(settings, plot)))
    };
    (Still::Drawn, rows)
}

/// Draws a logic trace in the cursor window, stepping between 0 V and `high` volts.
pub fn draw_logic_still(
    trace: &LogicTrace, at: f64, high: f32, settings: &ScopeSettings, plot: (u32, u32),
    scratch: &mut StillScratch, hits: &mut Hits,
) -> (Still, Vec<Measurement>) {
    hits.reset(plot.0, plot.1);
    scratch.t.clear();
    scratch.y.clear();
    let window = settings.window();
    let (start, end) = (at - window.pre, at + window.post);
    let map = settings.map(plot);
    let Some(first) = trace.first_time() else { return (Still::NoSamples, Vec::new()) };
    if !at.is_finite() || !high.is_finite() || !start.is_finite() || !end.is_finite() || !first.is_finite()
        || !map.v_top.is_finite() || !map.v_px.is_finite() || window.post < -window.pre || end < first {
        return (Still::NoSamples, Vec::new());
    }

    let mut i = trace.upto(start.next_down());
    let dense = trace.upto(end) - i > 16 * plot.0 as usize;
    let mut summarized = dense;
    // In a spilled chunk the first entry is available even when its interior is away.
    let Some(mut level) = trace.level_at(start).or_else(|| trace.get(i).map(|edge| edge.high)) else {
        return (Still::NoSamples, Vec::new());
    };
    let volts_at = |level| if level { high } else { 0.0 };
    let relative = |t: f64| (t - at).clamp(-window.pre, window.post);
    let column = |t: f64| (((t - map.t_left) / map.t_px) as u32).min(plot.0.saturating_sub(1));
    let begin = if first > start { relative(first) } else { -window.pre };
    scratch.point(begin, volts_at(level));
    for col in column(begin)..plot.0 {
        let incoming = level;
        let mark = scratch.t.len();
        let mut busy = false;
        while let Some(edge) = trace.get(i) {
            if edge.t > end || column(relative(edge.t)) > col {
                break;
            }
            if let Some((_, last)) = trace.away(i) {
                busy = true;
                summarized = true;
                if last > end || column(relative(last)) > col {
                    break;
                }
                i = (i / LOGIC_CHUNK + 1) * LOGIC_CHUNK;
                level = trace.get(i - 1).expect("the summary holds the last level").high;
            } else {
                if edge.high != level {
                    if dense {
                        busy = true;
                    } else if !busy {
                        let t = relative(edge.t);
                        scratch.point(t, volts_at(level));
                        scratch.point(t, volts_at(edge.high));
                    }
                    level = edge.high;
                }
                i += 1;
            }
        }
        if busy {
            // Anchor the incoming level so the stroke never slopes into a quiet column.
            scratch.t.truncate(mark);
            scratch.y.truncate(mark);
            let left = map.t_left + f64::from(col) * map.t_px;
            scratch.point(left.clamp(begin, window.post), volts_at(incoming));
            let centre = (left + 0.5 * map.t_px).clamp(begin, window.post);
            scratch.point(centre, volts_at(!level));
            scratch.point(centre, volts_at(level));
        } else if dense {
            let centre = map.t_left + (f64::from(col) + 0.5) * map.t_px;
            scratch.point(centre.clamp(begin, window.post), volts_at(level));
        }
    }
    scratch.point(window.post, volts_at(level));
    scratch.raster.add(&map, &scratch.t, &scratch.y, hits);
    hits.waveforms = u64::from(!hits.dirty.is_empty());
    let rows = if summarized {
        let high = f64::from(high);
        [("Vpp", high.abs()), ("min", high.min(0.0)), ("max", high.max(0.0))]
            .into_iter().map(|(name, v)| Measurement { n: scratch.t.len() as u64, ..Measurement::exact(0, name, v, "V") }).collect()
    } else {
        scratch.measure_steps(least_swing(volts(settings, plot)))
    };
    (Still::Drawn, rows)
}

/// Spectrum of the same paused capture, optionally restricted by its time bracket.
pub fn draw_spectrum_still(store: &TraceStore, row: usize, at: f64, settings: &ScopeSettings, bracket: Option<(f64, f64)>, plot: (u32, u32), scratch: &mut StillScratch, hits: &mut Hits) -> (Still, Vec<Measurement>) {
    hits.reset(plot.0, plot.1);
    let Some(signal) = store.signals.get(row) else { return (Still::NoSamples, Vec::new()) };
    let record = 10.0 * settings.time_per_div;
    // The cell and time picture use the same cursor-relative capture window.
    let start = -settings.window().pre;
    if !at.is_finite() || !(at + start).is_finite() || !record.is_finite() || record <= 0.0 || store.is_empty() { return (Still::NoSamples, Vec::new()); }
    scratch.spectrum.begin_gated(start, record, at, bracket, plot, settings.fft);
    if scratch.spectrum.result().gate_too_short { return (Still::Drawn, scratch.spectrum.result().rows()); }
    let (from, to) = bracket.unwrap_or((start, start + record));
    let Some(last) = store.index_at(at + to).map(|i| i - store.base) else {
        if bracket.is_none() { return (Still::NoSamples, Vec::new()); }
        scratch.spectrum.finish(hits);
        return (Still::Drawn, scratch.spectrum.result().rows());
    };
    let first = store.index_at(at + from).map_or(0, |i| i - store.base);
    let stop = (last + 2).min(store.len());
    scratch.spectrum.point(store.time.at(first), signal.at(first));
    for i in first + 1..stop { scratch.spectrum.segment(store.time.at(i - 1), signal.at(i - 1), store.time.at(i), signal.at(i)); }
    scratch.spectrum.finish(hits);
    (Still::Drawn, scratch.spectrum.result().rows())
}

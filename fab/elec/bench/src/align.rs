//! Monotone edge matching and one clock fit shared by every compared channel.

/// A directed edge in seconds relative to its capture's trigger.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    /// Trigger-relative time in seconds.
    pub t: f64,
    /// True for a rising edge.
    pub rising: bool,
}

/// Clock mapping: actual = offset + gain * expected.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Map {
    /// Actual clock offset in seconds.
    pub offset: f64,
    /// Clock gain, equal to one plus fractional skew.
    pub gain: f64,
}

/// A partition of both edge lists; paired indices are (expected, actual).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Matching {
    /// Monotone pairs of matching directions.
    pub pairs: Vec<(usize, usize)>,
    /// Unpaired expected indices.
    pub missing: Vec<usize>,
    /// Unpaired actual indices.
    pub extra: Vec<usize>,
}

/// Match sorted finite edges in one pass using a nonnegative window and positive gain.
pub fn match_edges(expected: &[Edge], actual: &[Edge], map: Map, window: f64) -> Matching {
    let mut out = Matching::default(); let (mut i, mut j) = (0, 0);
    while i < expected.len() && j < actual.len() {
        let p = map.offset + map.gain * expected[i].t;
        if actual[j].t < p - window { out.extra.push(j); j += 1; }
        else if actual[j].t > p + window { out.missing.push(i); i += 1; }
        else if actual[j].rising != expected[i].rising {
            if actual[j].t <= p { out.extra.push(j); j += 1; }
            else { out.missing.push(i); i += 1; }
        } else if actual.get(j + 1).is_some_and(|next| next.rising == expected[i].rising && (next.t - p).abs() < (actual[j].t - p).abs()) {
            out.extra.push(j); j += 1;
        } else { out.pairs.push((i, j)); i += 1; j += 1; }
    }
    out.missing.extend(i..expected.len()); out.extra.extend(j..actual.len()); out
}

fn rms(points: impl Iterator<Item = (f64, f64)>, map: Map) -> f64 {
    let mut squares = 0.0; let mut n = 0usize;
    for (e, a) in points { squares += (a - (map.offset + map.gain * e)).powi(2); n += 1; }
    if n == 0 { 0.0 } else { (squares / n as f64).sqrt() }
}
fn fit_points(points: impl Iterator<Item = (f64, f64)> + Clone, skew: bool) -> Option<(Map, f64)> {
    let (mut mean, mut delta, mut n) = (0.0, 0.0, 0usize);
    for (e, a) in points.clone() {
        n += 1; mean += (e - mean) / n as f64; delta += (a - e - delta) / n as f64;
    }
    if n == 0 { return None; }
    let (mut xx, mut xd) = (0.0, 0.0);
    if skew {
        for (e, a) in points.clone() { xx += (e - mean).powi(2); xd += (e - mean) * (a - e - delta); }
    }
    let gain = if xx > 0.0 { 1.0 + xd / xx } else { 1.0 };
    let map = Map { offset: delta - (gain - 1.0) * mean, gain };
    Some((map, rms(points, map)))
}

/// Least squares over pairs; without skew or distinct expected times only the offset is fitted.
pub fn fit(expected: &[Edge], actual: &[Edge], pairs: &[(usize, usize)], skew: bool) -> Option<(Map, f64)> {
    fit_points(pairs.iter().map(|&(e, a)| (expected[e].t, actual[a].t)), skew)
}

/// A finite search window from the shortest positive expected interval, at least twice tolerance.
pub fn search_window(expected: &[Edge], tolerance: f64) -> f64 {
    let shortest = expected.windows(2).map(|p| p[1].t - p[0].t).filter(|&dt| dt > 0.0).fold(f64::INFINITY, f64::min);
    if shortest.is_finite() { (0.45 * shortest).max(2.0 * tolerance) } else { 2.0 * tolerance }
}

/// One shared clock fit and the final partition for each channel.
#[derive(Debug)]
pub struct Alignment {
    /// Mapping from expected to actual trigger-relative seconds.
    pub map: Map,
    /// RMS residual of the final pairs, zero without pairs.
    pub rms: f64,
    /// Number of pairs across all channels.
    pub matched: usize,
    /// Partitions in input channel order.
    pub channels: Vec<Matching>,
}

/// Match, fit one map across channels, then rematch until stable or after four fit rounds.
pub fn align(channels: &[(&[Edge], &[Edge])], start: Map, tolerance: f64, skew: bool) -> Alignment {
    let matching = |map| channels.iter().map(|&(e, a)| match_edges(e, a, map, search_window(e, tolerance))).collect::<Vec<_>>();
    let mut map = start; let mut matched = matching(map);
    for _ in 0..4 {
        let points = channels.iter().zip(&matched).flat_map(|(&(e, a), m)| m.pairs.iter().map(move |&(i, j)| (e[i].t, a[j].t)));
        let Some((fitted, _)) = fit_points(points, skew) else { break };
        map = fitted;
        let next = matching(map);
        let stable = matched.iter().zip(&next).all(|(a, b)| a.pairs == b.pairs);
        matched = next;
        if stable { break; }
    }
    let points = channels.iter().zip(&matched).flat_map(|(&(e, a), m)| m.pairs.iter().map(move |&(i, j)| (e[i].t, a[j].t)));
    Alignment { map, rms: rms(points, map), matched: matched.iter().map(|m| m.pairs.len()).sum(), channels: matched }
}

/// Whether `a` beats `b`: more matched pairs, then a smaller residual.
pub fn better(a: &Alignment, b: &Alignment) -> bool {
    a.matched > b.matched || (a.matched == b.matched && a.rms < b.rms)
}

/// The most matches among candidates (matched, fitted map) that settled on another clock than `best`,
/// offsets more than `apart` from it: a candidate that converged onto the best clock is the same
/// answer, not a rival. Zero without one.
pub fn runner_up(candidates: &[(usize, Map)], best: Map, apart: f64) -> usize {
    candidates.iter().filter(|(_, map)| (map.offset - best.offset).abs() > apart).map(|&(matched, _)| matched).max().unwrap_or(0)
}

/// Try up to sixteen same-direction offsets on the first populated channel; returns the best
/// alignment and the runner-up's matched count (`runner_up`, apart by that channel's search window).
pub fn search_offset(channels: &[(&[Edge], &[Edge])], tolerance: f64, skew: bool) -> (Alignment, usize) {
    let mut best: Option<Alignment> = None; let mut seen = Vec::new();
    let Some(&(e, a)) = channels.iter().find(|(e, a)| !e.is_empty() && !a.is_empty()) else {
        return (align(channels, Map { offset: 0.0, gain: 1.0 }, tolerance, skew), 0);
    };
    for edge in a.iter().filter(|a| a.rising == e[0].rising).take(16) {
        let result = align(channels, Map { offset: edge.t - e[0].t, gain: 1.0 }, tolerance, skew);
        seen.push((result.matched, result.map));
        if best.as_ref().is_none_or(|best| better(&result, best)) { best = Some(result); }
    }
    let Some(best) = best else { return (align(channels, Map { offset: 0.0, gain: 1.0 }, tolerance, skew), 0) };
    let runner = runner_up(&seen, best.map, search_window(e, tolerance));
    (best, runner)
}

/// How an edge differs after the clock mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Expected edge has no partner.
    Missing,
    /// Actual edge has no partner.
    Extra,
    /// Actual edge arrives beyond positive tolerance.
    Late,
    /// Actual edge arrives beyond negative tolerance.
    Early,
}

/// One difference located in expected trigger-relative time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Difference {
    /// Expected time, including inverse-mapped actual extras.
    pub t: f64,
    /// Difference category.
    pub kind: Kind,
    /// Direction of the edge.
    pub rising: bool,
    /// Signed actual-clock residual for late/early; zero for missing/extra.
    pub dt: f64,
}

/// Report missing, extra and out-of-tolerance pairs in time order, allocating only the result.
pub fn differences(expected: &[Edge], actual: &[Edge], matching: &Matching, map: Map, tolerance: f64) -> Vec<Difference> {
    let mut out = Vec::new();
    for &i in &matching.missing { out.push(Difference { t: expected[i].t, kind: Kind::Missing, rising: expected[i].rising, dt: 0.0 }); }
    for &j in &matching.extra { out.push(Difference { t: (actual[j].t - map.offset) / map.gain, kind: Kind::Extra, rising: actual[j].rising, dt: 0.0 }); }
    for &(i, j) in &matching.pairs {
        let dt = actual[j].t - (map.offset + map.gain * expected[i].t);
        if dt.abs() > tolerance { out.push(Difference { t: expected[i].t, kind: if dt > 0.0 { Kind::Late } else { Kind::Early }, rising: expected[i].rising, dt }); }
    }
    out.sort_unstable_by(|a, b| a.t.total_cmp(&b.t).then_with(|| (a.kind as u8).cmp(&(b.kind as u8))).then_with(|| a.rising.cmp(&b.rising)).then_with(|| a.dt.total_cmp(&b.dt)));
    out
}

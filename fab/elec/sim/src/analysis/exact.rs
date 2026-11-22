//! Exact transient accuracy: repeat with tighter tolerances (reltol and the charge floor chgtol,
//! each a tenth of the run before) until successive runs agree (E3.Q1).
//!
//! Each comparison interpolates the newer run at the older run's times using up to three nearest
//! points in the newer run's segment between marks, including both endpoints. Three nodes give a
//! parabola, two a line, and one its value; a recorded time uses its value directly. The report
//! names the reason refinement stopped and records each completed comparison.
//!
//! The cost is the sum of the runs, each about 10^(1/3) times the previous for a second-order
//! method, with two recordings in memory. Only the reported run is replayed to the caller.
//! Failed or interrupted refinements count as runs; the last complete run keeps its summary
//! and error estimate. Without a pair of complete runs, the error estimate is infinity.

use super::{Analysis, Control, Sink, Summary, TranSpec};
use crate::circuit::SimCircuit;
use crate::newton::SimDiag;
use crate::stamp::{UnkKind, UnknownInfo};
use std::sync::atomic::Ordering;

/// E3's accuracy for Exact: 0.1 % of full scale.
pub const EXACT_TARGET: f64 = 1e-3;

#[derive(Clone, Debug, PartialEq)]
pub enum ExactStop {
    Reached,
    ReltolLimit,
    Stopped,
    RunFailed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactRun {
    /// The newer run's reltol and recorded point count.
    pub reltol: f64,
    pub points: usize,
    /// The largest difference as a fraction of full scale, its time and unknown.
    pub error: f64,
    pub at: f64,
    pub unknown: String,
    /// The newer run's interpolation node times at the largest difference (up to three).
    pub nodes: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactReport {
    /// Transient runs made (the first at the circuit's own reltol).
    pub runs: u32,
    /// The reltol of the run reported.
    pub reltol: f64,
    /// The estimated error of the run reported, as a fraction of full scale: the largest difference to
    /// the run before it.
    pub error: f64,
    /// Whether `error <= EXACT_TARGET`.
    pub reached: bool,
    /// Why refinement ended, including before the first comparison.
    pub stop: ExactStop,
    /// One entry per completed comparison, in run order.
    pub trail: Vec<ExactRun>,
}

struct Point {
    at: f64,
    x: Vec<f64>,
}

impl Point {
    fn value(&self, unknown: usize) -> f64 {
        self.x.get(unknown).copied().unwrap_or(f64::NAN)
    }
}

#[derive(Default)]
struct Recording {
    unknowns: Vec<UnknownInfo>,
    points: Vec<Point>,
    marks: Vec<f64>,
}

struct Comparison<'a> {
    error: f64,
    at: f64,
    unknown: &'a str,
    nodes: &'a [Point],
}

impl Sink for Recording {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.unknowns = unknowns.to_vec();
    }

    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        self.points.push(Point { at, x: x.to_vec() });
    }

    fn mark(&mut self, at: f64) {
        self.marks.push(at);
    }
}

impl Recording {
    fn replay(&self, sink: &mut dyn Sink) {
        sink.begin(&self.unknowns);
        let mut marks = self.marks.iter().peekable();
        for point in &self.points {
            sink.point(point.at, None, &point.x);
            while marks.peek().is_some_and(|&&at| at == point.at) {
                if let Some(&at) = marks.next() {
                    sink.mark(at);
                }
            }
        }
    }

    /// The three closest points within the surrounding marks form a contiguous slice, even on
    /// an uneven time grid. Each mark's point belongs to both neighbouring segments.
    /// At a recorded time, use the recorded value without interpolation roundoff.
    fn nearest(&self, at: f64) -> &[Point] {
        let k = self.points.partition_point(|p| p.at < at);
        if let Some(point) = self.points.get(k) {
            if point.at == at {
                return std::slice::from_ref(point);
            }
        }
        let mark = self.marks.partition_point(|&time| time < at);
        let start = mark.checked_sub(1).map_or(0, |i| {
            self.points.partition_point(|p| p.at < self.marks[i])
        });
        let end = self.marks.get(mark).map_or(self.points.len(), |&time| {
            self.points.partition_point(|p| p.at <= time)
        });
        let points = &self.points[start..end];
        let (mut lo, mut hi) = (k - start, k - start);
        for _ in 0..3 {
            let before = lo.checked_sub(1).and_then(|i| points.get(i));
            let after = points.get(hi);
            match (before, after) {
                (Some(a), Some(b)) if at - a.at <= b.at - at => lo -= 1,
                (_, Some(_)) => hi += 1,
                (Some(_), None) => lo -= 1,
                (None, None) => break,
            }
        }
        points.get(lo..hi).unwrap_or(&[])
    }

    fn error(&self, older: &Self, vntol: f64) -> Comparison<'_> {
        let voltages: Vec<usize> = self.unknowns.iter().enumerate()
            .filter_map(|(i, u)| (u.kind == UnkKind::Voltage).then_some(i)).collect();
        let mut full_scale = vntol.max(f64::MIN_POSITIVE);
        for point in &self.points {
            for &i in &voltages {
                let value = point.value(i);
                if !value.is_finite() {
                    return Comparison {
                        error: f64::INFINITY,
                        at: point.at,
                        unknown: &self.unknowns[i].name,
                        nodes: std::slice::from_ref(point),
                    };
                }
                full_scale = full_scale.max(value.abs());
            }
        }
        let mut difference = Comparison { error: 0.0, at: 0.0, unknown: "", nodes: &[] };
        for point in &older.points {
            let nearest = self.nearest(point.at);
            for &i in &voltages {
                let delta = (interpolate(nearest, point.at, i) - point.value(i)).abs();
                if !delta.is_finite() {
                    return Comparison {
                        error: f64::INFINITY,
                        at: point.at,
                        unknown: &self.unknowns[i].name,
                        nodes: nearest,
                    };
                }
                if delta > difference.error || difference.nodes.is_empty() {
                    difference = Comparison {
                        error: delta,
                        at: point.at,
                        unknown: &self.unknowns[i].name,
                        nodes: nearest,
                    };
                }
            }
        }
        difference.error /= full_scale;
        difference
    }
}

fn interpolate(points: &[Point], at: f64, unknown: usize) -> f64 {
    match points {
        [a] => a.value(unknown),
        [a, b] => {
            let fraction = (at - a.at) / (b.at - a.at);
            a.value(unknown) + fraction * (b.value(unknown) - a.value(unknown))
        }
        [a, b, c] => {
            // Ratio products avoid squaring small time intervals in the denominators.
            let wa = ((at - b.at) / (a.at - b.at)) * ((at - c.at) / (a.at - c.at));
            let wb = ((at - a.at) / (b.at - a.at)) * ((at - c.at) / (b.at - c.at));
            let wc = ((at - a.at) / (c.at - a.at)) * ((at - b.at) / (c.at - b.at));
            wa * a.value(unknown) + wb * b.value(unknown) + wc * c.value(unknown)
        }
        _ => f64::NAN,
    }
}

pub(super) fn tran(c: &SimCircuit, spec: &TranSpec, sink: &mut dyn Sink, ctl: &Control) -> Result<Summary, SimDiag> {
    let mut circuit = c.clone();
    circuit.opts.exact = false;
    let analysis = Analysis::Tran(spec.clone());
    let mut previous = Recording::default();
    let mut summary = super::run(&circuit, &analysis, &mut previous, ctl)?;
    let mut report = ExactReport {
        runs: 1,
        reltol: circuit.opts.reltol,
        error: f64::INFINITY,
        reached: false,
        stop: ExactStop::ReltolLimit,
        trail: Vec::new(),
    };
    report.stop = loop {
        if ctl.stop.as_ref().is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            break ExactStop::Stopped;
        }
        let scale = 10.0f64.powi(-(report.runs as i32));
        let next_reltol = c.opts.reltol * scale;
        if next_reltol < 1e-12 || !next_reltol.is_finite() {
            break ExactStop::ReltolLimit;
        }
        circuit.opts.reltol = next_reltol;
        // The charge floor tightens with reltol: left at its Fast value it bounds the error of a
        // small capacitor whatever reltol does (chgtol / C is 2 % of a volt at 0.5 pF), and two
        // runs that take the same steps agree without being right.
        circuit.opts.chgtol = c.opts.chgtol * scale;
        let mut newer = Recording::default();
        report.runs += 1;
        let next_summary = match super::run(&circuit, &analysis, &mut newer, ctl) {
            Ok(summary) => summary,
            Err(e) => break ExactStop::RunFailed(e.message.clone()),
        };
        let comparison = newer.error(&previous, circuit.opts.vntol);
        report.error = comparison.error;
        report.trail.push(ExactRun {
            reltol: next_reltol,
            points: newer.points.len(),
            error: comparison.error,
            at: comparison.at,
            unknown: comparison.unknown.to_owned(),
            nodes: comparison.nodes.iter().map(|point| point.at).collect(),
        });
        report.reltol = next_reltol;
        report.reached = report.error <= EXACT_TARGET;
        previous = newer;
        summary = next_summary;
        if report.reached {
            break ExactStop::Reached;
        }
    };
    previous.replay(sink);
    summary.exact = Some(report);
    Ok(summary)
}

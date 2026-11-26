//! Deterministic fitting of a freehand polyline in canvas coordinates.

#![forbid(unsafe_code)]

mod closed;
mod geometry;
mod open;

use geometry::{Frame, Point, Samples};

/// Canvas points in drawing order. Consecutive duplicates are allowed.
#[derive(Clone, Copy, Debug)]
pub struct Stroke<'a> {
    pub points: &'a [(f64, f64)],
}

/// Angles are in radians, measured from the positive x axis towards positive y.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Line {
        from: (f64, f64),
        to: (f64, f64),
    },
    Arrow {
        from: (f64, f64),
        to: (f64, f64),
    },
    /// Size gives full side lengths. The angle gives the direction of the first side.
    Rectangle {
        center: (f64, f64),
        size: (f64, f64),
        angle: f64,
    },
    /// Radii are ordered largest first. A circle has angle zero.
    Ellipse {
        center: (f64, f64),
        radii: (f64, f64),
        angle: f64,
    },
    /// Size gives the full horizontal and vertical diagonals.
    Diamond {
        center: (f64, f64),
        size: (f64, f64),
    },
    Triangle {
        corners: [(f64, f64); 3],
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Recognised {
    pub shape: Shape,
    /// Fit quality in 0..=1. An exact fit has score 1.
    pub score: f64,
}

/// Returns fits in decreasing score order, or an empty vector for unrecognised ink.
pub fn recognise(stroke: &Stroke<'_>) -> Vec<Recognised> {
    let Some(frame) = Frame::new(stroke.points.iter()) else { return Vec::new() };
    let Some(samples) = Samples::new(stroke.points, frame, true) else { return Vec::new() };
    if samples.closed {
        closed::recognise(&samples.points, frame)
    } else {
        open::recognise(&samples.points, frame, stroke.points)
    }
}

/// Places a separately drawn barb or V at the matching end of a line.
pub fn merge_arrow_head(line: &Shape, head: &Stroke<'_>) -> Option<Shape> {
    let Shape::Line { from, to } = line else { return None };
    let frame = Frame::new([from, to].into_iter().chain(head.points))?;
    let samples = Samples::new(head.points, frame, false)?;
    open::merge(&samples.points, frame, *from, *to)
}

// Use the same distance scale for ranking all fits to a stroke. The peak gate
// keeps a short excursion from disappearing in an otherwise small mean.
fn fit_score(points: &[Point], scale: f64, distance: impl Fn(Point) -> f64) -> Option<f64> {
    if scale <= 0.0 {
        return None;
    }
    let mut sum = 0.0;
    for &p in points {
        let error = distance(p) / scale;
        if error > 0.10 {
            return None;
        }
        sum += error;
    }
    let mean = sum / points.len() as f64;
    (mean <= 0.028).then(|| (1.0 - mean / 0.10).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests;

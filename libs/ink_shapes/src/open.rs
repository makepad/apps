use crate::{
    fit_score,
    geometry::{Frame, Point, Segment},
    Recognised, Shape,
};

fn shaft(points: &[Point]) -> Option<f64> {
    let (from, to) = (points[0], points[points.len() - 1]);
    let delta = to - from;
    let axis = delta.unit()?;
    let backwards = points.windows(2).map(|w| (w[0] - w[1]).dot(axis).max(0.0)).sum::<f64>();
    if backwards > 0.045 * delta.norm() {
        return None;
    }
    let chord = Segment::new(from, to);
    fit_score(points, delta.norm(), |p| chord.project(p).1.sqrt())
}

struct Head {
    barbs: [Segment; 2],
    reach: f64,
}

impl Head {
    fn fit(points: &[Point], tip: Point, direction: Point) -> Option<Self> {
        let length = direction.norm();
        let axis = direction.unit()?;
        let mut offsets = [Point::default(); 2];
        for &p in points {
            let d = p - tip;
            let side = usize::from(axis.cross(d) > 0.0);
            if d.norm2() > offsets[side].norm2() {
                offsets[side] = d;
            }
        }
        let reach = offsets[0].norm().max(offsets[1].norm());
        if reach < 0.045 * length || reach > 0.48 * length {
            return None;
        }
        let mut total = 0.0;
        for d in &mut offsets {
            let len = d.norm();
            // Samples close to the apex can fall on the other side of the shaft.
            if len < 0.20 * reach {
                *d = Point::default();
                continue;
            }
            if -d.dot(axis) < 0.25 * len || axis.cross(*d).abs() < 0.22 * len {
                return None;
            }
            total += len;
        }
        let travelled: f64 = points.windows(2).map(|w| (w[1] - w[0]).norm()).sum();
        if travelled > 2.5 * total {
            return None;
        }
        let head = Self { barbs: offsets.map(|d| Segment::new(tip, tip + d)), reach };
        fit_score(points, 2.5 * reach, |p| head.distance2(p).sqrt())?;
        Some(head)
    }

    fn distance2(&self, p: Point) -> f64 {
        self.barbs[0].project(p).1.min(self.barbs[1].project(p).1)
    }
}

fn hooked(points: &[Point], frame: Frame, reverse: bool, from: (f64, f64)) -> Option<Recognised> {
    let last = points.len() - 1;
    let tail = points[if reverse { last } else { 0 }];
    let mut tip_index = 0;
    let mut farthest = 0.0;
    for (i, &p) in points.iter().enumerate() {
        let distance = (p - tail).norm2();
        if distance > farthest {
            farthest = distance;
            tip_index = i;
        }
    }
    if tip_index == 0 || tip_index == last {
        return None;
    }
    let tip = points[tip_index];
    let before = &points[..=tip_index];
    let after = &points[tip_index..];
    let (stem, hook) = if reverse { (after, before) } else { (before, after) };
    let head = Head::fit(hook, tip, tip - tail)?;
    shaft(stem)?;
    let chord = Segment::new(tail, tip);
    let score = fit_score(points, (tip - tail).norm(), |p| {
        chord.project(p).1.min(head.distance2(p)).sqrt()
    })?;
    Some(Recognised { shape: Shape::Arrow { from, to: frame.world(tip)? }, score })
}

pub(super) fn recognise(points: &[Point], frame: Frame, raw: &[(f64, f64)]) -> Vec<Recognised> {
    let from = raw[0];
    let to = raw[raw.len() - 1];
    let arrow = [hooked(points, frame, false, from), hooked(points, frame, true, to)]
        .into_iter()
        .flatten()
        .max_by(|a, b| a.score.total_cmp(&b.score));
    if let Some(arrow) = arrow {
        return vec![arrow];
    }
    shaft(points)
        .map(|score| Recognised { shape: Shape::Line { from, to }, score })
        .into_iter()
        .collect()
}

pub(super) fn merge(points: &[Point], frame: Frame, a: (f64, f64), b: (f64, f64)) -> Option<Shape> {
    for (from, to) in [(a, b), (b, a)] {
        let end = frame.local(to);
        let direction = end - frame.local(from);
        let axis = direction.unit()?;
        // The V's apex projects furthest along the shaft, even across a small pen gap.
        let &tip = points.iter().max_by(|p, q| p.dot(axis).total_cmp(&q.dot(axis)))?;
        let gap = (tip - end).norm();
        if gap > 0.08 * direction.norm() {
            continue;
        }
        if let Some(head) = Head::fit(points, tip, direction) {
            if gap <= 0.45 * head.reach {
                return Some(Shape::Arrow { from, to });
            }
        }
    }
    None
}

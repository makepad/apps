use crate::{
    fit_score,
    geometry::{principal, Frame, Point, Segment},
    Recognised, Shape,
};

struct Oval {
    center: Point,
    axis: Point,
    major: f64,
    minor: f64,
}

impl Oval {
    fn from_loop(points: &[Point]) -> Option<Self> {
        // Integrate the enclosed area. Arc-length samples alone overrepresent
        // the flatter sides of an eccentric ellipse.
        let origin = points.iter().copied().fold(Point::default(), |s, p| s + p)
            * (1.0 / points.len() as f64);
        let edges = points.iter().copied().zip(points.iter().copied().cycle().skip(1));
        let mut center = Point::default();
        let (mut area2, mut unsigned) = (0.0, 0.0);
        for (p, q) in edges.clone() {
            let (p, q) = (p - origin, q - origin);
            let cross = p.cross(q);
            area2 += cross;
            unsigned += cross.abs();
            center = center + (p + q) * cross;
        }
        // A self-crossing scribble sweeps substantial area in both directions.
        if area2.abs() <= 1e-12 || area2.abs() < 0.80 * unsigned {
            return None;
        }
        center = origin + center * (1.0 / (3.0 * area2));
        let (mut xx, mut xy, mut yy) = (0.0, 0.0, 0.0);
        for (p, q) in edges {
            let (p, q) = (p - center, q - center);
            let cross = p.cross(q);
            xx += cross * (p.0 * p.0 + p.0 * q.0 + q.0 * q.0);
            yy += cross * (p.1 * p.1 + p.1 * q.1 + q.1 * q.1);
            xy += cross * (2.0 * (p.0 * p.1 + q.0 * q.1) + p.0 * q.1 + q.0 * p.1);
        }
        xx /= 6.0 * area2;
        xy /= 12.0 * area2;
        yy /= 6.0 * area2;
        let (axis, large) = principal(xx, xy, yy);
        let small = (xx * yy - xy * xy) / large;
        if !(small > 0.0 && large > 0.0) {
            return None;
        }
        Some(Self { center, axis, major: 2.0 * large.sqrt(), minor: 2.0 * small.sqrt() })
    }

    fn distance(&self, p: Point) -> f64 {
        let d = p - self.center;
        let q = Point(d.dot(self.axis).abs(), d.cross(self.axis).abs());
        // Start on the radial intersection, then remove tangential error.
        let mut u = Point(q.0 / self.major, q.1 / self.minor).unit().unwrap_or(Point(0.0, 1.0));
        for _ in 0..2 {
            let error = q - Point(self.major * u.0, self.minor * u.1);
            let tangent = Point(-self.major * u.1, self.minor * u.0);
            let step = (error.dot(tangent) / tangent.norm2()).clamp(-0.5, 0.5);
            u = (u + u.perp() * step) * (1.0 / (1.0 + step * step).sqrt());
        }
        (q - Point(self.major * u.0, self.minor * u.1)).norm()
    }

    fn has_corner(&self, points: &[Point]) -> bool {
        let radial = |p: Point| {
            let d = p - self.center;
            let radius =
                Point(d.dot(self.axis) / self.major, d.cross(self.axis) / self.minor).norm();
            d * (1.0 / radius)
        };
        let turn_cos = |a: Point, b: Point, c: Point| {
            let (u, v) = (b - a, c - b);
            u.dot(v) / (u.norm2() * v.norm2()).sqrt()
        };
        let n = points.len();
        (0..n).any(|i| {
            let (a, b, c) = (points[(i + n - 4) % n], points[i], points[(i + 4) % n]);
            let observed = turn_cos(a, b, c);
            if observed >= std::f64::consts::FRAC_1_SQRT_2 {
                return false;
            }
            // An eccentric oval turns sharply at its tips. Count a turn over
            // 45 degrees only if it exceeds the model's own turn by 30 degrees.
            let expected = turn_cos(radial(a), radial(b), radial(c)).clamp(-1.0, 1.0);
            let cos30 = 0.8660254037844386;
            expected > -cos30
                && observed < expected * cos30 - 0.5 * (1.0 - expected * expected).sqrt()
        })
    }

    fn candidate(mut self, points: &[Point], frame: Frame, scale: f64) -> Option<Recognised> {
        let angle = if self.minor >= 0.96 * self.major {
            self.major = 0.5 * (self.minor + self.major);
            self.minor = self.major;
            0.0
        } else {
            self.axis.angle()
        };
        let score = fit_score(points, scale, |p| self.distance(p))?;
        if self.has_corner(points) {
            return None;
        }
        let shape = Shape::Ellipse {
            center: frame.world(self.center)?,
            radii: frame.size(self.major, self.minor)?,
            angle,
        };
        Some(Recognised { shape, score })
    }
}

fn segments<const N: usize>(v: &[Point; N]) -> [Segment; N] {
    std::array::from_fn(|i| Segment::new(v[i], v[(i + 1) % N]))
}

fn polygon_score<const N: usize>(
    points: &[Point],
    vertices: &[Point; N],
    scale: f64,
) -> Option<f64> {
    let edges = segments(vertices);
    fit_score(points, scale, |p| {
        edges.iter().map(|edge| edge.project(p).1).fold(f64::INFINITY, f64::min).sqrt()
    })
}

fn furthest(points: &[Point], distance: impl Fn(Point) -> f64) -> Point {
    let mut best = points[0];
    let mut maximum = distance(best);
    for &p in &points[1..] {
        let d = distance(p);
        if d > maximum {
            best = p;
            maximum = d;
        }
    }
    best
}

// Seed corners at the largest departures from a chord. Splitting in geometric
// order keeps the same corner from being counted twice on a second lap.
fn corner_seeds(points: &[Point]) -> ([Point; 3], [Point; 4]) {
    let a = furthest(points, |p| (p - points[0]).norm2());
    let b = furthest(points, |p| (p - a).norm2());
    let base = Segment::new(a, b);
    let c = furthest(points, |p| base.project(p).1);
    let tri = [a, b, c];
    let winding = (b - a).cross(c - a).signum();
    let mut quad = [a, b, c, c];
    let mut maximum = 0.0;
    for i in 0..3 {
        let start = tri[i];
        let edge = tri[(i + 1) % 3] - start;
        let Some(axis) = edge.unit() else { continue };
        let outside = |p: Point| (p - start).cross(axis) * winding;
        let next = furthest(points, outside);
        let d = outside(next);
        if d > maximum {
            maximum = d;
            quad = [start, next, tri[(i + 1) % 3], tri[(i + 2) % 3]];
        }
    }
    (tri, quad)
}

#[derive(Clone, Copy, Default)]
struct Side {
    n: usize,
    sum: Point,
    xx: f64,
    xy: f64,
    yy: f64,
}

impl Side {
    fn add(&mut self, p: Point) {
        self.n += 1;
        self.sum = self.sum + p;
        self.xx += p.0 * p.0;
        self.xy += p.0 * p.1;
        self.yy += p.1 * p.1;
    }

    fn line(self) -> Option<(Point, Point)> {
        if self.n < 2 {
            return None;
        }
        let mean = self.sum * (1.0 / self.n as f64);
        let (axis, variance) = principal(
            self.xx - mean.0 * self.sum.0,
            self.xy - mean.0 * self.sum.1,
            self.yy - mean.1 * self.sum.1,
        );
        (variance > 0.0).then_some((mean, axis))
    }
}

fn fit_corners<const N: usize>(points: &[Point], seeds: &[Point; N]) -> Option<[Point; N]> {
    let edges = segments(seeds);
    let mut sides = [Side::default(); N];
    for &p in points {
        let mut best = (0, 0.0, f64::INFINITY);
        for (i, edge) in edges.iter().enumerate() {
            let (t, d) = edge.project(p);
            if d < best.2 {
                best = (i, t, d);
            }
        }
        // Exclude the rounded turn itself when estimating each side's direction.
        if (0.10..=0.90).contains(&best.1) {
            sides[best.0].add(p);
        }
    }
    let lines = sides.map(Side::line);
    let mut corners = *seeds;
    for i in 0..N {
        let prev = (i + N - 1) % N;
        let (a, u) = lines[prev]?;
        let (b, v) = lines[i]?;
        let turn = u.cross(v);
        if turn.abs() < 0.06 {
            return None;
        }
        let p = a + u * ((b - a).cross(v) / turn);
        let short = (seeds[prev] - seeds[i]).norm().min((seeds[(i + 1) % N] - seeds[i]).norm());
        if (p - seeds[i]).norm() > 0.4 * short {
            return None;
        }
        corners[i] = p;
    }
    Some(corners)
}

fn triangle(points: &[Point], seed: &[Point; 3], scale: f64, frame: Frame) -> Option<Recognised> {
    let v = fit_corners(points, seed)?;
    let score = polygon_score(points, &v, scale)?;
    let [a, b, c] = v.map(|p| frame.world(p));
    Some(Recognised { shape: Shape::Triangle { corners: [a?, b?, c?] }, score })
}

fn diamond(v: &[Point; 4]) -> Option<(Point, f64, f64)> {
    let right = furthest(v, |p| p.0);
    let left = furthest(v, |p| -p.0);
    let top = furthest(v, |p| -p.1);
    let bottom = furthest(v, |p| p.1);
    let w = right.0 - left.0;
    let h = bottom.1 - top.1;
    let center = Point((right.0 + left.0) * 0.5, (bottom.1 + top.1) * 0.5);
    if (top.0 - center.0).abs().max((bottom.0 - center.0).abs()) <= 0.08 * w
        && (right.1 - center.1).abs().max((left.1 - center.1).abs()) <= 0.08 * h
    {
        Some((center, w, h))
    } else {
        None
    }
}

fn quadrilateral(
    points: &[Point],
    seed: &[Point; 4],
    scale: f64,
    frame: Frame,
) -> Option<Recognised> {
    let v = fit_corners(points, seed)?;
    if let Some((center, w, h)) = diamond(&v) {
        let x = Point(w * 0.5, 0.0);
        let y = Point(0.0, h * 0.5);
        let score =
            polygon_score(points, &[center + x, center + y, center - x, center - y], scale)?;
        return Some(Recognised {
            shape: Shape::Diamond { center: frame.world(center)?, size: frame.size(w, h)? },
            score,
        });
    }
    let edges: [Point; 4] = std::array::from_fn(|i| v[(i + 1) % 4] - v[i]);
    for i in 0..4 {
        let (a, b) = (edges[i], edges[(i + 1) % 4]);
        if a.dot(b).abs() > 0.28 * a.norm() * b.norm() {
            return None;
        }
    }
    let mut a = (edges[0] - edges[2]) * 0.5;
    let mut b = (edges[1] - edges[3]) * 0.5;
    if b.norm2() > a.norm2() {
        std::mem::swap(&mut a, &mut b);
    }
    let aligned = b.perp() * a.dot(b.perp()).signum();
    let mut axis = (a * a.norm() + aligned * b.norm()).unit()?;
    // Snap both near-horizontal and near-vertical boxes to canvas axes (3 degrees).
    let angle = if axis.0.abs().min(axis.1.abs()) <= 0.052336 {
        if axis.0.abs() < axis.1.abs() {
            std::mem::swap(&mut a, &mut b);
        }
        axis = Point(1.0, 0.0);
        0.0
    } else {
        axis.angle()
    };
    let center = (v[0] + v[1] + v[2] + v[3]) * 0.25;
    let w = a.dot(axis).abs();
    let h = b.dot(axis.perp()).abs();
    let x = axis * (w * 0.5);
    let y = axis.perp() * (h * 0.5);
    let score = polygon_score(
        points,
        &[center + x + y, center - x + y, center - x - y, center + x - y],
        scale,
    )?;
    Some(Recognised {
        shape: Shape::Rectangle { center: frame.world(center)?, size: frame.size(w, h)?, angle },
        score,
    })
}

pub(super) fn recognise(points: &[Point], frame: Frame) -> Vec<Recognised> {
    let mut laps;
    let (oval, points) = if let Some(oval) = Oval::from_loop(points) {
        (oval, points)
    } else {
        // A reversal between laps cancels signed area and creates a cusp in
        // the drawing order. Restore the winding before measuring the turns.
        let halfway = points.len() / 2;
        if (points[halfway] - points[0]).norm() > 0.12 {
            return Vec::new();
        }
        laps = points.to_vec();
        laps[halfway..].reverse();
        let Some(oval) = Oval::from_loop(&laps) else { return Vec::new() };
        (oval, laps.as_slice())
    };
    let scale = 2.0 * oval.minor;
    let (tri, quad) = corner_seeds(points);
    let mut results: Vec<_> = [
        triangle(points, &tri, scale, frame),
        quadrilateral(points, &quad, scale, frame),
        oval.candidate(points, frame, scale),
    ]
    .into_iter()
    .flatten()
    .collect();
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
    results
}

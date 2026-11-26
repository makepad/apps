use super::*;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, TAU};

type XY = (f64, f64);

struct Pen(u64);

impl Pen {
    fn draw(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 32) as f64 / (u32::MAX as f64 + 1.0)
    }
}

fn polyline(vertices: &[XY], divisions: usize) -> Vec<XY> {
    let mut points = Vec::new();
    for w in vertices.windows(2) {
        for i in 0..divisions {
            // Uneven sampling models a pen slowing down near a turn.
            let t = (i as f64 / divisions as f64).powi(2);
            points.push((w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t));
        }
    }
    points.push(*vertices.last().unwrap());
    points
}

fn polygon(vertices: &[XY]) -> Vec<XY> {
    let mut v = vertices.to_vec();
    v.push(v[0]);
    let mut points = polyline(&v, 18);
    points.pop();
    points
}

fn oval(a: f64, b: f64) -> Vec<XY> {
    (0..96)
        .map(|i| {
            let t = TAU * i as f64 / 96.0;
            (a * t.cos(), b * t.sin())
        })
        .collect()
}

fn place((x, y): XY, angle: f64, seed: u64) -> XY {
    let (s, c) = angle.sin_cos();
    let scale = 0.4 + (seed % 11) as f64 * 0.26;
    ((x * c - y * s) * scale + 17.0 * seed as f64, (x * s + y * c) * scale - 29.5)
}

fn drawn(ideal: &[XY], closed: bool, angle: f64, seed: u64) -> Vec<XY> {
    let mut pen = Pen(seed);
    let n = ideal.len();
    let laps = if closed && seed % 4 >= 2 { 2 } else { 1 };
    let count = if closed { n * laps + 1 } else { n };
    let phase = if closed { (pen.draw() * n as f64) as usize } else { 0 };
    let mut points = Vec::new();
    for i in 0..count {
        let step = if laps == 2 && i >= n && seed % 8 >= 6 { 2 * n - i } else { i };
        let index = if seed % 2 == 0 { (phase + step) % n } else { (phase + n - 1 - step % n) % n };
        let (x, y) = ideal[index];
        let p = place((x + (pen.draw() - 0.5) * 1.2, y + (pen.draw() - 0.5) * 1.2), angle, seed);
        points.push(p);
        if i % 11 == 0 {
            points.push(p);
        }
    }
    points
}

fn best(points: &[XY], seed: u64) -> Shape {
    let fits = recognise(&Stroke { points });
    assert!(!fits.is_empty(), "no fit for variant {seed}");
    assert!(fits.iter().all(|c| (0.0..=1.0).contains(&c.score)));
    assert!(fits.windows(2).all(|w| w[0].score >= w[1].score));
    fits.into_iter().next().unwrap().shape
}

fn distance(a: XY, b: XY) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

#[test]
fn lines_preserve_the_drawn_endpoints() {
    let ideal = polyline(&[(-63.0, -5.0), (70.0, 9.0)], 72);
    for seed in 0..32 {
        let points = drawn(&ideal, false, 0.47 * seed as f64, seed);
        assert_eq!(
            best(&points, seed),
            Shape::Line { from: points[0], to: *points.last().unwrap() }
        );
    }
}

#[test]
fn hook_keeps_its_tip_in_either_drawing_direction() {
    for seed in 0..32 {
        let side = if seed % 4 < 2 { 1.0 } else { -1.0 };
        let ideal = polyline(&[(-55.0, 0.0), (55.0, 0.0), (39.0, side * 12.0)], 32);
        let angle = 0.47 * seed as f64;
        let points = drawn(&ideal, false, angle, seed);
        let shape = best(&points, seed);
        let Shape::Arrow { from, to } = shape else { panic!("variant {seed}: {shape:?}") };
        let tolerance =
            0.03 * distance(place((-55.0, 0.0), angle, seed), place((55.0, 0.0), angle, seed));
        assert!(distance(from, place((-55.0, 0.0), angle, seed)) < tolerance, "tail {seed}");
        assert!(distance(to, place((55.0, 0.0), angle, seed)) < tolerance, "tip {seed}");
    }
}

#[test]
fn separate_head_must_meet_an_endpoint_and_point_along_the_shaft() {
    for seed in 0..32 {
        let angle = seed as f64 * 0.3;
        let a = place((-60.0, 0.0), angle, seed);
        let b = place((60.0, 0.0), angle, seed);
        let line = Shape::Line { from: a, to: b };
        let sign = if seed % 4 < 2 { 1.0 } else { -1.0 };
        for offset in [60.0, 0.0] {
            let x = sign * offset;
            let v = [(x - sign * 16.0, -11.0), (x + sign, 1.0), (x - sign * 18.0, 14.0)];
            let points = drawn(&polyline(&v, 24), false, angle, seed);
            let actual = merge_arrow_head(&line, &Stroke { points: &points });
            let (from, to) = if sign > 0.0 { (a, b) } else { (b, a) };
            assert_eq!(
                actual,
                (offset > 0.0).then_some(Shape::Arrow { from, to }),
                "variant {seed}"
            );
        }
        let straight = drawn(&polyline(&[(44.0, 0.0), (60.0, 0.0)], 12), false, angle, seed);
        assert!(merge_arrow_head(&line, &Stroke { points: &straight }).is_none());
    }
}

#[test]
fn rectangles_fit_the_sides_and_snap_near_both_axes() {
    let ideal = polygon(&[(-65.0, -33.0), (65.0, -33.0), (65.0, 33.0), (-65.0, 33.0)]);
    for seed in 0..32 {
        let angle = [0.026, FRAC_PI_2 - 0.024, -0.36, 1.03][(seed / 4 % 4) as usize];
        let shape = best(&drawn(&ideal, true, angle, seed), seed);
        let Shape::Rectangle { center, size, angle: fitted } = shape else {
            panic!("variant {seed}: {shape:?}")
        };
        let scale = 0.4 + (seed % 11) as f64 * 0.26;
        assert!(distance(center, place((0.0, 0.0), angle, seed)) < 1.5 * scale, "center {seed}");
        let expected = if angle > 1.5 { (66.0, 130.0) } else { (130.0, 66.0) };
        assert!(
            distance(size, (expected.0 * scale, expected.1 * scale)) < 3.0 * scale,
            "size {seed}"
        );
        if angle < 0.0 || angle == 1.03 {
            assert!((fitted - angle).abs() < 0.025, "angle {seed}: {fitted}");
        } else {
            assert_eq!(fitted, 0.0, "snap {seed}");
        }
    }
}

#[test]
fn midpoint_corners_choose_a_diamond_including_the_rotated_square() {
    for seed in 0..32 {
        let h = if seed % 8 < 4 { 38.0 } else { 70.0 };
        let ideal = polygon(&[(-70.0, 0.0), (0.0, -h), (70.0, 0.0), (0.0, h)]);
        let points = drawn(&ideal, true, 0.0, seed);
        let shape = best(&points, seed);
        let Shape::Diamond { center, size } = shape else { panic!("variant {seed}: {shape:?}") };
        let scale = 0.4 + (seed % 11) as f64 * 0.26;
        assert!(distance(center, place((0.0, 0.0), 0.0, seed)) < 2.0 * scale);
        assert!(distance(size, (140.0 * scale, 2.0 * h * scale)) < 3.0 * scale);
    }
    let square = polygon(&[(-40.0, -40.0), (40.0, -40.0), (40.0, 40.0), (-40.0, 40.0)]);
    assert!(matches!(best(&drawn(&square, true, FRAC_PI_4, 0), 0), Shape::Diamond { .. }));
    assert!(matches!(best(&drawn(&square, true, 0.12, 0), 0), Shape::Rectangle { .. }));
}

#[test]
fn triangles_recover_three_distinct_corners() {
    let corners = [(-68.0, 30.0), (49.0, 39.0), (18.0, -52.0)];
    let ideal = polygon(&corners);
    for seed in 0..32 {
        let angle = seed as f64 * 0.43;
        let shape = best(&drawn(&ideal, true, angle, seed), seed);
        let Shape::Triangle { corners: fitted } = shape else {
            panic!("variant {seed}: {shape:?}")
        };
        let scale = 0.4 + (seed % 11) as f64 * 0.26;
        for corner in corners {
            assert!(
                fitted.iter().any(|&q| distance(q, place(corner, angle, seed)) < 2.0 * scale),
                "corner {corner:?} variant {seed}"
            );
        }
    }
}

#[test]
fn ellipses_recover_axes_and_snap_near_circles() {
    for seed in 0..32 {
        let b = [69.0, 36.0, 17.0, 58.0][(seed / 4 % 4) as usize];
        let angle = 0.78 - seed as f64 * 0.09;
        let points = drawn(&oval(70.0, b), true, angle, seed);
        let shape = best(&points, seed);
        let Shape::Ellipse { center, radii, angle: fitted } = shape else {
            panic!("variant {seed}: {shape:?}")
        };
        let scale = 0.4 + (seed % 11) as f64 * 0.26;
        assert!(distance(center, place((0.0, 0.0), angle, seed)) < 1.5 * scale);
        assert!(
            distance(radii, (70.0 * scale, b * scale)) < 2.2 * scale,
            "radii {seed}: {radii:?}"
        );
        if b == 69.0 {
            assert_eq!(radii.0, radii.1);
            assert_eq!(fitted, 0.0);
        } else {
            assert!((fitted - angle).sin().abs() < 0.03, "angle {seed}: {fitted}");
        }
    }
}

#[test]
fn incomplete_closure_and_dense_samples() {
    let mut points: Vec<_> = (0..8192)
        .map(|i| {
            let t = 0.973 * TAU * i as f64 / 8192.0;
            (1e6 + 60.0 * t.cos(), -1e6 + 27.0 * t.sin())
        })
        .collect();
    assert!(matches!(best(&points, 0), Shape::Ellipse { .. }));
    points.reverse();
    assert!(matches!(best(&points, 0), Shape::Ellipse { .. }));
}

#[test]
fn unstructured_ink_and_backtracking_stay_ink() {
    for seed in 0..32 {
        let mut pen = Pen(seed);
        let mut points: Vec<_> =
            (0..24).map(|_| (pen.draw() * 100.0, pen.draw() * 100.0)).collect();
        if seed % 2 == 0 {
            points.push(points[0]);
        }
        assert!(recognise(&Stroke { points: &points }).is_empty(), "scribble {seed}");
    }
    for n in [5, 6] {
        let vertices: Vec<_> = (0..n)
            .map(|i| {
                let t = TAU * i as f64 / n as f64;
                (60.0 * t.cos(), 60.0 * t.sin())
            })
            .collect();
        let points = drawn(&polygon(&vertices), true, 0.17, 3);
        assert!(recognise(&Stroke { points: &points }).is_empty(), "{n} sharp corners");
    }
    for points in [
        vec![(0.0, 0.0), (80.0, 0.0), (30.0, 0.0), (100.0, 0.0)],
        vec![(0.0, 0.0), (0.0, 70.0), (90.0, 70.0)],
        vec![(0.0, 0.0), (100.0, 100.0), (100.0, 0.0), (0.0, 100.0), (0.0, 0.0)],
    ] {
        assert!(recognise(&Stroke { points: &points }).is_empty(), "{points:?}");
    }
}

#[test]
fn degenerate_ink_and_invalid_merges() {
    let zero = Shape::Line { from: (0.0, 0.0), to: (0.0, 0.0) };
    let line = Shape::Line { from: (0.0, 0.0), to: (10.0, 0.0) };
    for points in [
        &[][..],
        &[(1.0, 1.0)],
        &[(1.0, 1.0); 30],
        &[(1.0, 1.0), (0.0, f64::NAN)],
        &[(1.0, 1.0), (f64::INFINITY, 0.0)],
        &[(0.0, f64::NEG_INFINITY), (1.0, 1.0)],
    ] {
        assert!(recognise(&Stroke { points }).is_empty());
        assert!(merge_arrow_head(&line, &Stroke { points }).is_none());
    }
    let head = Stroke { points: &[(8.0, 1.0), (10.0, 0.0), (8.0, -1.0)] };
    assert!(merge_arrow_head(&zero, &head).is_none());
    assert!(merge_arrow_head(&Shape::Arrow { from: (0.0, 0.0), to: (10.0, 0.0) }, &head).is_none());
}

#[test]
fn repeated_fit_has_identical_bits() {
    let points = drawn(&oval(75.0, 42.0), true, -0.72, 17);
    let run = || recognise(&Stroke { points: &points });
    let a = run();
    let b = run();
    assert_eq!(a, b);
    let bits = |r: &Recognised| {
        let Shape::Ellipse { center, radii, angle } = r.shape else { panic!("unexpected fit") };
        [center.0, center.1, radii.0, radii.1, angle, r.score].map(f64::to_bits)
    };
    assert_eq!(bits(&a[0]), bits(&b[0]));
}

//! Light painting: where a light goes so that it lands on a painted point.
//!
//! A viewer paints on an object lit by the map: at the painted point the
//! surface has the normal `n` and is seen along `view` (from the eye toward
//! the surface). Each mode answers with the direction in the map that the
//! light moves to:
//!
//! - Reflection: the mirrored ray. A light there is what the point reflects,
//!   so its highlight sits under the brush.
//! - Illumination: the normal. A light there faces the point square on, so
//!   that point is lit most.
//! - Rim: behind the object, across the view from the point. A light there
//!   grazes the point's edge from behind.
//!
//! A brush stroke is a run of such directions; `brush_points` thins it into
//! a stroke light's points, `brush_stroke` into a paint layer's stroke (plan
//! 9c), every point kept and its width following the brush.

use super::*;

/// How far behind the object a rim light sits: the angle its direction
/// makes with the plane across the view, toward the far side.
pub const RIM_BEHIND_DEG: f32 = 35.0;
/// The most points a stroke light keeps (`StrokeParams::points`).
pub const STROKE_MAX_POINTS: usize = 64;
/// The least angle between neighbouring points a brush keeps: closer ones
/// draw nothing new.
pub const BRUSH_SPACING_DEG: f32 = 1.0;

/// Which way a painted point is lit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaintMode {
    #[default]
    Reflection,
    Illumination,
    Rim,
}

impl PaintMode {
    pub const ALL: [PaintMode; 3] = [PaintMode::Reflection, PaintMode::Illumination, PaintMode::Rim];

    pub fn as_str(self) -> &'static str {
        match self {
            PaintMode::Reflection => "reflection",
            PaintMode::Illumination => "illumination",
            PaintMode::Rim => "rim",
        }
    }

    pub fn from_str(s: &str) -> Option<PaintMode> {
        PaintMode::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// The direction in the map a light moves to so that it lands, in `mode`,
/// on a point with the unit normal `normal` seen along the unit `view`.
pub fn paint_dir(mode: PaintMode, view: Vec3f, normal: Vec3f) -> Vec3f {
    match mode {
        PaintMode::Reflection => (view - normal * (2.0 * view.dot(normal))).normalize(),
        PaintMode::Illumination => normal.normalize(),
        PaintMode::Rim => {
            // The normal's part across the view; at the point facing the eye
            // square on there is none, and any direction across will do.
            let mut across = normal - view * normal.dot(view);
            if across.length() < 1.0e-4 {
                let side = if view.y.abs() < 0.9 { vec3f(0.0, 1.0, 0.0) } else { vec3f(1.0, 0.0, 0.0) };
                across = side - view * side.dot(view);
            }
            let behind = RIM_BEHIND_DEG.to_radians();
            (across.normalize() * behind.cos() + view * behind.sin()).normalize()
        }
    }
}

/// A brush's directions as a stroke light's points, (azimuth, elevation) in
/// degrees: neighbours closer than BRUSH_SPACING_DEG dropped (the last
/// direction always kept), and a longer run evenly resampled along its
/// length to STROKE_MAX_POINTS, both ends kept. The directions must already
/// be in the map's own frame.
pub fn brush_points(dirs: &[Vec3f]) -> Vec<[f32; 2]> {
    let mut kept: Vec<Vec3f> = Vec::new();
    let spacing = BRUSH_SPACING_DEG.to_radians();
    for &d in dirs {
        let d = d.normalize();
        if !(d.x.is_finite() && d.y.is_finite() && d.z.is_finite()) {
            continue;
        }
        match kept.last() {
            Some(&last) if angle(last, d) < spacing => {}
            _ => kept.push(d),
        }
    }
    if let (Some(&last), Some(&end)) = (kept.last(), dirs.last()) {
        let end = end.normalize();
        if kept.len() > 1 && end.x.is_finite() && angle(last, end) > 0.0 {
            *kept.last_mut().unwrap() = end;
        }
    }
    if kept.len() > STROKE_MAX_POINTS {
        kept = resample(&kept, STROKE_MAX_POINTS);
    }
    kept.into_iter()
        .map(|d| {
            let (az, el) = az_el_from_dir(d);
            [az, el]
        })
        .collect()
}

/// A paint brush's width at rest, as a multiple of its size: a slow brush
/// lays more paint, a fast one less (`brush_stroke`).
pub const BRUSH_REST_WIDTH: f32 = 1.4;
/// Its width at full speed.
pub const BRUSH_FAST_WIDTH: f32 = 0.5;
/// The brush speed, degrees between two reports of the pointer (a frame),
/// at which the width is halfway from rest to full speed.
pub const BRUSH_HALF_SPEED_DEG: f32 = 2.0;
/// How far along a stroke, as a share of its length, each end tapers to a tip.
pub const BRUSH_TAPER: f32 = 0.12;
/// A tip's width, as a multiple of the size there: thin, not nothing.
pub const BRUSH_TIP: f32 = 0.15;

/// A brush's directions (in the map's own frame, one per pointer report) as
/// a paint stroke's points and width profile (`PaintStroke::stroke`):
/// neighbours closer than BRUSH_SPACING_DEG dropped, the last one always
/// kept, and every other point kept, however long the stroke. Each point's
/// width multiplier follows the brush's speed over the reports it stands for
/// (BRUSH_REST_WIDTH at rest, falling toward BRUSH_FAST_WIDTH, halfway at
/// BRUSH_HALF_SPEED_DEG a report), smoothed over its neighbours, and both
/// ends taper to BRUSH_TIP over BRUSH_TAPER of the length. The profile has
/// one value per point; the points are about evenly spaced, so it reads
/// along the stroke as the renderer spreads it.
pub fn brush_stroke(dirs: &[Vec3f]) -> (Vec<[f32; 2]>, Vec<f32>) {
    let spacing = BRUSH_SPACING_DEG.to_radians();
    let mut kept: Vec<Vec3f> = Vec::new();
    // Per kept point: the summed angle and the count of the reports it stands for.
    let mut speed: Vec<(f32, f32)> = Vec::new();
    let mut prev: Option<Vec3f> = None;
    for &d in dirs {
        let d = d.normalize();
        if !(d.x.is_finite() && d.y.is_finite() && d.z.is_finite()) {
            continue;
        }
        let step = prev.map_or(0.0, |p| angle(p, d));
        prev = Some(d);
        match kept.last() {
            Some(&last) if angle(last, d) < spacing => {
                if let Some(s) = speed.last_mut() {
                    *s = (s.0 + step, s.1 + 1.0);
                }
            }
            _ => {
                kept.push(d);
                speed.push((step, 1.0));
            }
        }
    }
    if let (Some(last), Some(end)) = (kept.last_mut(), prev) {
        *last = end;
    }
    let n = kept.len();
    let half = BRUSH_HALF_SPEED_DEG.to_radians();
    let raw: Vec<f32> = speed.iter().map(|&(sum, count)| sum / count.max(1.0)).map(|v| BRUSH_FAST_WIDTH + (BRUSH_REST_WIDTH - BRUSH_FAST_WIDTH) * half / (half + v)).collect();
    // Smoothed over two points each side, so one late report makes no bead.
    let smooth: Vec<f32> = (0..n)
        .map(|i| {
            let (lo, hi) = (i.saturating_sub(2), (i + 3).min(n));
            raw[lo..hi].iter().sum::<f32>() / (hi - lo) as f32
        })
        .collect();
    let mut along = vec![0.0f32; n];
    for i in 1..n {
        along[i] = along[i - 1] + angle(kept[i - 1], kept[i]);
    }
    let total = along.last().copied().unwrap_or(0.0).max(1.0e-9);
    let profile = (0..n)
        .map(|i| {
            let t = along[i] / total;
            let end = (t.min(1.0 - t) / BRUSH_TAPER).clamp(0.0, 1.0);
            let taper = BRUSH_TIP + (1.0 - BRUSH_TIP) * end * end * (3.0 - 2.0 * end);
            smooth[i] * taper
        })
        .collect();
    let points = kept
        .into_iter()
        .map(|d| {
            let (az, el) = az_el_from_dir(d);
            [az, el]
        })
        .collect();
    (points, profile)
}

fn angle(a: Vec3f, b: Vec3f) -> f32 {
    a.dot(b).clamp(-1.0, 1.0).acos()
}

/// `n` points evenly spaced by angle along the polyline through `path`.
fn resample(path: &[Vec3f], n: usize) -> Vec<Vec3f> {
    let mut at = vec![0.0f32];
    for w in path.windows(2) {
        at.push(at.last().unwrap() + angle(w[0], w[1]));
    }
    let total = *at.last().unwrap();
    let mut out = Vec::with_capacity(n);
    let mut seg = 0;
    for i in 0..n {
        let s = total * i as f32 / (n - 1) as f32;
        while seg + 2 < path.len() && at[seg + 1] < s {
            seg += 1;
        }
        let span = (at[seg + 1] - at[seg]).max(1.0e-9);
        let t = ((s - at[seg]) / span).clamp(0.0, 1.0);
        out.push((path[seg] * (1.0 - t) + path[seg + 1] * t).normalize());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Vec3f, b: Vec3f) -> bool {
        (a - b).length() < 1.0e-4
    }

    #[test]
    fn a_reflected_light_is_what_the_point_mirrors() {
        // Looking down -z at a surface tilted 45 degrees up: the mirror shows
        // straight up.
        let view = vec3f(0.0, 0.0, -1.0);
        let n = vec3f(0.0, 1.0, 1.0).normalize();
        assert!(approx(paint_dir(PaintMode::Reflection, view, n), vec3f(0.0, 1.0, 0.0)));
        // Square on, it mirrors the eye's own direction back.
        assert!(approx(paint_dir(PaintMode::Reflection, view, vec3f(0.0, 0.0, 1.0)), vec3f(0.0, 0.0, 1.0)));
    }

    #[test]
    fn the_illumination_light_lights_the_point_most() {
        let n = vec3f(0.3, 0.8, 0.52).normalize();
        let l = paint_dir(PaintMode::Illumination, vec3f(0.0, 0.0, -1.0), n);
        assert!((l.dot(n) - 1.0).abs() < 1.0e-5);
    }

    #[test]
    fn a_rim_light_is_behind_and_across_from_the_point() {
        let view = vec3f(0.0, 0.0, -1.0);
        for n in [vec3f(1.0, 0.0, 0.2), vec3f(-0.3, 0.9, 0.4), vec3f(0.0, 0.0, 1.0)] {
            let n = n.normalize();
            let l = paint_dir(PaintMode::Rim, view, n);
            assert!((l.length() - 1.0).abs() < 1.0e-5);
            let behind = l.dot(view).asin().to_degrees();
            assert!((behind - RIM_BEHIND_DEG).abs() < 0.01, "{behind}");
            // On the point's side of the object, as seen by the eye.
            let across = n - view * n.dot(view);
            if across.length() > 1.0e-3 {
                assert!(l.dot(across) > 0.0);
            }
        }
    }

    #[test]
    fn paint_modes_round_trip_their_names() {
        for m in PaintMode::ALL {
            assert_eq!(PaintMode::from_str(m.as_str()), Some(m));
        }
        assert_eq!(PaintMode::from_str("nope"), None);
    }

    #[test]
    fn a_brush_drops_close_points_and_keeps_its_ends() {
        let dirs: Vec<Vec3f> = (0..=200).map(|i| dir_from_az_el(i as f32 * 0.1, 10.0)).collect();
        let points = brush_points(&dirs);
        // 20 degrees of azimuth at 10 elevation is about 19.7 degrees of arc.
        assert!(points.len() >= 18 && points.len() <= 21, "{}", points.len());
        assert!(points[0][0].abs() < 1.0e-3);
        assert!((points.last().unwrap()[0] - 20.0).abs() < 1.0e-3);
        assert!(points.iter().all(|p| (p[1] - 10.0).abs() < 0.01));
    }

    #[test]
    fn a_long_brush_is_resampled_to_the_stroke_limit() {
        let dirs: Vec<Vec3f> = (0..=300).map(|i| dir_from_az_el(i as f32, 0.0)).collect();
        let points = brush_points(&dirs);
        assert_eq!(points.len(), STROKE_MAX_POINTS);
        assert!(points[0][0].abs() < 1.0e-3);
        assert!((points.last().unwrap()[0] - 300.0).abs() < 1.0e-2);
        // Evenly spaced: every step about 300 / 63 degrees.
        for w in points.windows(2) {
            let step = (w[1][0] - w[0][0]).rem_euclid(360.0);
            assert!((step - 300.0 / 63.0).abs() < 0.05, "{step}");
        }
    }

    #[test]
    fn a_still_brush_is_one_point() {
        let d = dir_from_az_el(40.0, 20.0);
        assert_eq!(brush_points(&[d, d, d]).len(), 1);
        assert!(brush_points(&[]).is_empty());
    }
}

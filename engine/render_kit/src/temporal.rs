//! Time effects: which other times a layer is rendered at, and with what
//! weight. Motion blur is temporal supersampling across the shutter (exact:
//! a frame is a pure function of t); echo is the layer at earlier times.

use crate::stack::ResolvedEffect;

/// A 2D affine map `[a, b, c, d, tx, ty]`: `x' = a·x + c·y + tx`,
/// `y' = b·x + d·y + ty` (pixels), the layer's placement at one time.
pub type Affine2 = [f64; 6];

pub fn apply_affine(m: &Affine2, p: [f64; 2]) -> [f64; 2] {
    [m[0] * p[0] + m[2] * p[1] + m[4], m[1] * p[0] + m[3] * p[1] + m[5]]
}

pub fn invert_affine(m: &Affine2) -> Option<Affine2> {
    let det = m[0] * m[3] - m[1] * m[2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])])
}

/// `a` after `b`.
pub fn compose_affine(a: &Affine2, b: &Affine2) -> Affine2 {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

/// The largest distance any corner of `rect` (x, y, w, h in layer
/// pixels) travels between two placements.
pub fn travel(from: &Affine2, to: &Affine2, rect: [f64; 4]) -> f64 {
    let corners = [[rect[0], rect[1]], [rect[0] + rect[2], rect[1]], [rect[0], rect[1] + rect[3]], [rect[0] + rect[2], rect[1] + rect[3]]];
    corners
        .iter()
        .map(|p| {
            let (a, b) = (apply_affine(from, *p), apply_affine(to, *p));
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
        })
        .fold(0.0, f64::max)
}

/// The shutter of one output frame at `t`: open and close times.
pub fn shutter(effect: &ResolvedEffect, t: f64, host_fps: f64) -> (f64, f64) {
    let fps = if effect.num("fps") > 0.0 { effect.num("fps") } else { host_fps.max(1.0) };
    let frame = 1.0 / fps;
    let open = t + effect.num("phase") / 360.0 * frame;
    (open, open + effect.num("shutter_angle") / 360.0 * frame)
}

/// How many sub-frames motion blur takes: the author's `samples`, or, at
/// 0, enough that the layer moves at most `max_step` pixels between two
/// (when the host knows how far it moves), within the min/max bounds.
pub fn sample_count(effect: &ResolvedEffect, motion_px: Option<f64>) -> usize {
    let fixed = effect.num("samples").round() as usize;
    if fixed > 0 {
        return fixed;
    }
    let lo = effect.num("min_samples").round().max(1.0) as usize;
    let hi = (effect.num("max_samples").round() as usize).max(lo);
    match motion_px {
        Some(px) => ((px / effect.num("max_step").max(0.1)).ceil() as usize + 1).clamp(lo, hi),
        None => ((lo + hi) / 2).max(lo).min(16.max(lo)).min(hi),
    }
}

/// Sub-frame times and weights for motion blur: `n` samples at the
/// centres of `n` equal slices of the shutter (a box filter), equal
/// weights summing to 1. A closed shutter is the frame itself.
pub fn motion_blur_samples(open: f64, close: f64, n: usize) -> Vec<(f64, f64)> {
    let n = n.max(1);
    if close <= open || n == 1 {
        return vec![((open + close) * 0.5, 1.0)];
    }
    (0..n).map(|k| (open + (close - open) * (k as f64 + 0.5) / n as f64, 1.0 / n as f64)).collect()
}

/// Echo times and opacities, newest first: `t`, then `count` echoes each
/// `delay` further back at `decay` of the previous one's opacity.
pub fn echo_samples(effect: &ResolvedEffect, t: f64) -> Vec<(f64, f64)> {
    let count = effect.num("count").round().max(1.0) as usize;
    let delay = effect.num("delay");
    let decay = effect.num("decay");
    (0..=count).map(|k| (t - delay * k as f64, decay.powi(k as i32))).collect()
}

/// Groups of at most `width` inputs for a reduction tree over `n` samples:
/// the index ranges each pass of each level reads. Level 0 reads samples;
/// level k reads the outputs of level k - 1.
pub fn reduction_levels(n: usize, width: usize) -> Vec<Vec<std::ops::Range<usize>>> {
    let mut levels = Vec::new();
    let mut count = n;
    while count > 1 {
        let groups: Vec<_> = (0..count.div_ceil(width)).map(|g| g * width..((g + 1) * width).min(count)).collect();
        count = groups.len();
        levels.push(groups);
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Value;
    use crate::catalog::EffectKind;

    #[test]
    fn a_180_degree_shutter_is_centred_on_the_frame() {
        let blur = ResolvedEffect::new(EffectKind::MotionBlur);
        let (open, close) = shutter(&blur, 1.0, 25.0);
        assert!((open - (1.0 - 0.01)).abs() < 1e-12 && (close - 1.01).abs() < 1e-12);
        let samples = motion_blur_samples(open, close, 4);
        assert_eq!(samples.len(), 4);
        let mean: f64 = samples.iter().map(|(t, w)| t * w).sum();
        assert!((mean - 1.0).abs() < 1e-12, "{mean}");
        assert!((samples.iter().map(|s| s.1).sum::<f64>() - 1.0).abs() < 1e-12);
        assert_eq!(motion_blur_samples(1.0, 1.0, 8), vec![(1.0, 1.0)]);
    }

    #[test]
    fn samples_adapt_to_motion() {
        let blur = ResolvedEffect::new(EffectKind::MotionBlur);
        assert_eq!(sample_count(&blur, Some(0.0)), 4);
        assert_eq!(sample_count(&blur, Some(20.0)), 21);
        assert_eq!(sample_count(&blur, Some(5000.0)), 64);
        assert_eq!(sample_count(&blur.clone().with("samples", Value::Num(9.0)), Some(5000.0)), 9);
        assert_eq!(sample_count(&blur, None), 16);
    }

    #[test]
    fn affine_travel() {
        let a: Affine2 = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let b: Affine2 = [1.0, 0.0, 0.0, 1.0, 30.0, 40.0];
        assert_eq!(travel(&a, &b, [0.0, 0.0, 100.0, 100.0]), 50.0);
        let r: Affine2 = [0.0, 1.0, -1.0, 0.0, 0.0, 0.0];
        let inv = invert_affine(&r).unwrap();
        let id = compose_affine(&r, &inv);
        assert!(id.iter().zip([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]).all(|(x, y)| (x - y).abs() < 1e-12));
    }

    #[test]
    fn echoes_and_reduction() {
        let echo = ResolvedEffect::new(EffectKind::Echo).with("count", Value::Num(3.0)).with("delay", Value::Num(0.1)).with("decay", Value::Num(0.5));
        let s = echo_samples(&echo, 2.0);
        assert_eq!(s.len(), 4);
        assert!((s[3].0 - 1.7).abs() < 1e-12 && s[3].1 == 0.125);
        assert_eq!(reduction_levels(1, 8), Vec::<Vec<std::ops::Range<usize>>>::new());
        assert_eq!(reduction_levels(8, 8), vec![vec![0..8]]);
        assert_eq!(reduction_levels(20, 8), vec![vec![0..8, 8..16, 16..20], vec![0..3]]);
    }
}

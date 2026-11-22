//! Van Wijk & Nuij smooth zoom-and-pan path between two cameras.
//! `w` is the width of the world the view shows, `u` the distance travelled
//! along the straight line between the two centres.
use makepad_math::Vec2d as DVec2;

/// The arc's shape (van Wijk & Nuij's ρ): √2 trades pan for zoom evenly.
pub const HOP_RHO: f64 = 1.42;
/// The hop's shortest and longest durations, and the seconds per unit of
/// arc length in between.
pub const HOP_MIN_SECONDS: f64 = 0.7;
pub const HOP_MAX_SECONDS: f64 = 2.4;
pub const HOP_SECONDS_PER_LENGTH: f64 = 0.24;

/// Van Wijk & Nuij's path between two cameras, in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HopPath {
    from: DVec2,
    to: DVec2,
    view_width: f64,
    w0: f64,
    w1: f64,
    /// The exact landing zoom (`view_width / w1` loses an ulp).
    to_zoom: f64,
    /// The world distance between the centres.
    u1: f64,
    r0: f64,
    /// The arc length: the total in the path's own units.
    length: f64,
    /// A pure zoom (the centres coincide): the width grows or shrinks
    /// exponentially instead of along the hyperbola.
    pure_zoom: bool,
}

impl HopPath {
    /// The path from the camera (`from` at `from_zoom`) to (`to` at
    /// `to_zoom`), for a map `view_width` wide; `None` when the inputs or
    /// the arc are not finite (nothing is installed then).
    pub fn new(
        view_width: f64,
        from: DVec2,
        from_zoom: f64,
        to: DVec2,
        to_zoom: f64,
    ) -> Option<Self> {
        Self::between(view_width, from, from_zoom, to, to_zoom)
    }

    /// The same path in an arbitrary positive world scale. Callers that
    /// have a zoom floor apply it before constructing the path.
    pub fn between(
        view_width: f64,
        from: DVec2,
        from_zoom: f64,
        to: DVec2,
        to_zoom: f64,
    ) -> Option<Self> {
        if !from_zoom.is_finite() || from_zoom <= 0.0 || !to_zoom.is_finite() || to_zoom <= 0.0 {
            return None;
        }
        let view_width = view_width.max(1.0);
        let w0 = view_width / from_zoom;
        let w1 = view_width / to_zoom;
        let u1 = (to - from).length();
        if ![w0, w1, u1, from.x, from.y, to.x, to.y]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        let rho = HOP_RHO;
        if u1 < 1e-6 * w0.max(w1) {
            let length = (w1 / w0).ln().abs() / rho;
            return length.is_finite().then_some(Self {
                from,
                to,
                view_width,
                w0,
                w1,
                to_zoom,
                u1: 0.0,
                r0: 0.0,
                length,
                pure_zoom: true,
            });
        }
        // r_i = ln(−b_i + √(b_i² + 1)) = −asinh(b_i): the closed form cancels
        // catastrophically for large b (a deep start zoom far from the
        // target gave −∞); asinh does not
        let r = |i: f64, w: f64| {
            let b = (w1 * w1 - w0 * w0 + i * rho.powi(4) * u1 * u1) / (2.0 * w * rho * rho * u1);
            -b.asinh()
        };
        let r0 = r(1.0, w0);
        let r1 = r(-1.0, w1);
        let length = (r1 - r0) / rho;
        (r0.is_finite() && r1.is_finite() && length.is_finite() && length >= 0.0).then_some(Self {
            from,
            to,
            view_width,
            w0,
            w1,
            to_zoom,
            u1,
            r0,
            length,
            pure_zoom: false,
        })
    }
    /// The arc's length in the path's units (screen widths of motion,
    /// roughly): the duration derives from it.
    pub fn length(&self) -> f64 {
        self.length
    }
    /// The camera at arc position `s` in `0..=length`: (centre, zoom).
    pub fn sample(&self, s: f64) -> (DVec2, f64) {
        let s = s.clamp(0.0, self.length);
        if self.length <= 0.0 {
            return (self.to, self.view_width / self.w1);
        }
        let rho = HOP_RHO;
        if self.pure_zoom {
            let k = if self.w1 >= self.w0 { 1.0 } else { -1.0 };
            let w = self.w0 * (k * rho * s).exp();
            return (self.from, self.view_width / w);
        }
        let w = self.w0 * self.r0.cosh() / (rho * s + self.r0).cosh();
        let u = self.w0 / (rho * rho) * self.r0.cosh() * (rho * s + self.r0).tanh()
            - self.w0 / (rho * rho) * self.r0.sinh();
        let f = (u / self.u1).clamp(0.0, 1.0);
        (self.from + (self.to - self.from) * f, self.view_width / w)
    }
    /// The exact endpoint (the last frame lands here, never on a sample).
    pub fn end(&self) -> (DVec2, f64) {
        (self.to, self.to_zoom)
    }
}

/// The duration a path of `length` takes: the floor plus a share per unit
/// of arc, capped; a near hit takes at least 0.7 seconds, the farthest 2.4.
pub fn hop_duration(length: f64) -> f64 {
    (HOP_MIN_SECONDS + HOP_SECONDS_PER_LENGTH * length.max(0.0))
        .clamp(HOP_MIN_SECONDS, HOP_MAX_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_math::dvec2;

    fn zoom_samples(path: &HopPath, n: usize) -> Vec<f64> {
        (0..=n)
            .map(|i| path.sample(path.length() * i as f64 / n as f64).1)
            .collect()
    }
    /// The zoom first falls (or holds), then rises (or holds): one valley.
    fn assert_out_then_in(zooms: &[f64]) {
        let valley = zooms
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        for w in zooms[..=valley].windows(2) {
            assert!(
                w[1] <= w[0] * (1.0 + 1e-9),
                "zoom rose before the valley: {zooms:?}"
            );
        }
        for w in zooms[valley..].windows(2) {
            assert!(
                w[1] >= w[0] * (1.0 - 1e-9),
                "zoom fell after the valley: {zooms:?}"
            );
        }
    }

    #[test]
    fn a_far_hop_climbs_pans_and_descends_onto_the_exact_endpoint() {
        let path =
            HopPath::new(1000.0, dvec2(0.0, 0.0), 4.0, dvec2(20_000.0, 5_000.0), 8.0).unwrap();
        let (c0, z0) = path.sample(0.0);
        assert!((c0 - dvec2(0.0, 0.0)).length() < 1e-6 && (z0 - 4.0).abs() < 1e-9);
        let (c1, z1) = path.end();
        assert_eq!(c1, dvec2(20_000.0, 5_000.0));
        assert!((z1 - 8.0).abs() < 1e-12);
        let (cs, zs) = path.sample(path.length());
        assert!((cs - c1).length() < 1e-6 * 20_000.0, "{cs:?}");
        assert!((zs - z1).abs() < 1e-6, "{zs}");
        let zooms = zoom_samples(&path, 64);
        assert_out_then_in(&zooms);
        let lowest = zooms.iter().cloned().fold(f64::INFINITY, f64::min);
        // high enough that both sites would fit: the width shown at the top
        // of the arc exceeds the distance between them
        assert!(1000.0 / lowest > 20_000.0, "top width {}", 1000.0 / lowest);
        // the centre travels monotonically along the straight line
        let mut last = 0.0;
        for i in 0..=64 {
            let (c, _) = path.sample(path.length() * i as f64 / 64.0);
            let f = c.x / 20_000.0;
            assert!(f >= last - 1e-12 && f <= 1.0 + 1e-12);
            assert!((c.y - 5_000.0 * f).abs() < 1e-6);
            last = f;
        }
        assert_eq!(
            hop_duration(path.length()),
            (0.7 + 0.24 * path.length()).min(2.4)
        );
    }

    #[test]
    fn a_near_hop_is_shallow_and_short() {
        let far = HopPath::new(1000.0, dvec2(0.0, 0.0), 1.0, dvec2(5_000_000.0, 0.0), 1.0).unwrap();
        let near = HopPath::new(1000.0, dvec2(0.0, 0.0), 1.0, dvec2(300.0, 0.0), 1.0).unwrap();
        assert!(near.length() < far.length());
        assert!(hop_duration(near.length()) < hop_duration(far.length()));
        assert!(hop_duration(near.length()) >= HOP_MIN_SECONDS);
        assert_eq!(hop_duration(far.length()), HOP_MAX_SECONDS);
        let lowest = zoom_samples(&near, 32)
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        assert!(lowest > 0.5, "a near hop barely rises: {lowest}");
        assert_out_then_in(&zoom_samples(&near, 32));
    }

    #[test]
    fn a_pure_zoom_is_exponential_and_monotone_both_ways() {
        for (a, b) in [(1.0, 100.0), (100.0, 1.0), (3.0, 3.0)] {
            let path = HopPath::new(800.0, dvec2(5.0, 5.0), a, dvec2(5.0, 5.0), b).unwrap();
            assert!(path.pure_zoom);
            let zooms = zoom_samples(&path, 16);
            for w in zooms.windows(2) {
                if b > a {
                    assert!(w[1] >= w[0]);
                } else {
                    assert!(w[1] <= w[0]);
                }
            }
            assert!((zooms[0] - a).abs() < 1e-9 && (path.end().1 - b).abs() < 1e-9);
            assert!((path.sample(path.length()).1 - b).abs() < 1e-9 * b);
            for i in 0..=16 {
                assert_eq!(
                    path.sample(path.length() * i as f64 / 16.0).0,
                    dvec2(5.0, 5.0)
                );
            }
        }
        assert_eq!(
            HopPath::new(800.0, dvec2(0.0, 0.0), 3.0, dvec2(0.0, 0.0), 3.0)
                .unwrap()
                .length(),
            0.0
        );
    }

    /// The reviewer's inputs: a deep start zoom far from a shallow target
    /// cancelled catastrophically in `ln(√(b²+1) − b)`; `−asinh(b)` stays
    /// finite and the arc still has one valley. Non-finite inputs install
    /// nothing.
    #[test]
    fn a_deep_far_hop_stays_finite_and_non_finite_inputs_are_refused() {
        let path = HopPath::new(
            1000.0,
            dvec2(0.0, 0.0),
            1_000_000.0,
            dvec2(100_000.0, 0.0),
            4.0,
        )
        .expect("a finite arc");
        assert!(path.r0.is_finite() && path.length().is_finite() && path.length() > 0.0);
        let zooms = zoom_samples(&path, 128);
        assert!(zooms.iter().all(|z| z.is_finite() && *z > 0.0), "{zooms:?}");
        assert_out_then_in(&zooms);
        for i in 0..=128 {
            let (c, _) = path.sample(path.length() * i as f64 / 128.0);
            assert!(c.x.is_finite() && c.y.is_finite());
        }
        let (c, z) = path.sample(path.length());
        assert!(
            (c.x - 100_000.0).abs() < 1e-3 && (z - 4.0).abs() < 1e-6,
            "{c:?} {z}"
        );
        assert!(HopPath::new(1000.0, dvec2(f64::NAN, 0.0), 1.0, dvec2(1.0, 0.0), 1.0).is_none());
        assert!(
            HopPath::new(1000.0, dvec2(0.0, 0.0), f64::INFINITY, dvec2(1.0, 0.0), 1.0).is_none()
        );
        assert!(
            HopPath::new(1000.0, dvec2(0.0, 0.0), 1.0, dvec2(f64::INFINITY, 0.0), 1.0).is_none()
        );
    }
}

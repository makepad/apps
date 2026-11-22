//! Rigid 3D transforms: how a unit (a board, a cable, an off-board part) sits in its parent frame.

use crate::geom::{Angle, Nm};

// Within 100 microdegrees of gimbal lock, f64 quaternion error amplified by 1/cos(ry)
// cannot reliably recover separate rx and rz on the grid. Keep their stable combination
// in rx instead, with rz folded to zero.
const LOCK_BAND: i32 = 100;

/// A rigid transform: rotate by the unit quaternion `q = [w, x, y, z]`, then translate by `t`
/// (nm, parent frame). Canonical form (E4 Q4): `q` of unit length with `w >= 0` and no negative
/// zero; `new` and the writer both produce it, so equal poses hash equal.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Xform3 {
    pub t: [Nm; 3],
    pub q: [f64; 4],
}

impl Default for Xform3 {
    fn default() -> Self {
        Xform3::IDENTITY
    }
}

fn canonical_q(q: [f64; 4]) -> [f64; 4] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if !(n > 0.0) || !n.is_finite() {
        return [1.0, 0.0, 0.0, 0.0];
    }
    let s = if q[0] < 0.0 { -1.0 / n } else { 1.0 / n };
    let mut r = [q[0] * s, q[1] * s, q[2] * s, q[3] * s];
    for v in &mut r {
        if *v == 0.0 {
            *v = 0.0;
        }
    }
    r
}

fn qmul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0] * b[0] - a[1] * b[1] - a[2] * b[2] - a[3] * b[3],
        a[0] * b[1] + a[1] * b[0] + a[2] * b[3] - a[3] * b[2],
        a[0] * b[2] - a[1] * b[3] + a[2] * b[0] + a[3] * b[1],
        a[0] * b[3] + a[1] * b[2] - a[2] * b[1] + a[3] * b[0],
    ]
}

fn half_sin_cos(udeg: i32) -> (f64, f64) {
    // The input counts half-microdegrees of the half angle: reduce before converting
    // to f64, so quadrant boundaries and their 45-degree midpoints are exact.
    let h = udeg.rem_euclid(720_000_000);
    let quadrant = h / 180_000_000;
    let r = h % 180_000_000;
    let a = r.min(180_000_000 - r);
    let (s, c) = if a == 0 {
        (0.0, 1.0)
    } else if a == 90_000_000 {
        (std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2)
    } else {
        (a as f64 * (std::f64::consts::PI / 360_000_000.0)).sin_cos()
    };
    let (s, c) = if r > 90_000_000 { (c, s) } else { (s, c) };
    match quadrant {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

fn round_turn(radians: f64) -> i32 {
    let udeg = ((radians.to_degrees() * 1_000_000.0).round() as i64).rem_euclid(360_000_000);
    if udeg > 180_000_000 { (udeg - 360_000_000) as i32 } else { udeg as i32 }
}

impl Xform3 {
    pub const IDENTITY: Xform3 = Xform3 { t: [0; 3], q: [1.0, 0.0, 0.0, 0.0] };

    /// A transform in canonical form.
    pub fn new(t: [Nm; 3], q: [f64; 4]) -> Xform3 {
        Xform3 { t, q: canonical_q(q) }
    }
    pub fn translation(t: [Nm; 3]) -> Xform3 {
        Xform3 { t, q: Self::IDENTITY.q }
    }
    /// A rotation by `a` about the axis `(x, y, z)` (need not be unit), then `t`.
    pub fn axis_angle(t: [Nm; 3], axis: [f64; 3], a: Angle) -> Xform3 {
        let n = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        let h = a.radians() * 0.5;
        let s = if n > 0.0 { h.sin() / n } else { 0.0 };
        Xform3::new(t, [h.cos(), axis[0] * s, axis[1] * s, axis[2] * s])
    }
    /// `[rx, ry, rz]` in microdegrees: turn about the parent's X, then its Y, then its Z
    /// (R = Rz * Ry * Rx), then translate by `t`.
    pub fn from_euler(t: [Nm; 3], udeg: [i32; 3]) -> Xform3 {
        let (sx, cx) = half_sin_cos(udeg[0]);
        let (sy, cy) = half_sin_cos(udeg[1]);
        let (sz, cz) = half_sin_cos(udeg[2]);
        Xform3::new(t, qmul(qmul([cz, 0.0, 0.0, sz], [cy, 0.0, sy, 0.0]), [cx, sx, 0.0, 0.0]))
    }
    /// The canonical Euler angles of `q`, rounded to whole microdegrees:
    /// rx in (-180, 180], ry in [-90, 90], rz in (-180, 180] degrees. Within 100
    /// microdegrees of |ry| = 90, rz is zero and rx carries the stable combined turn.
    pub fn euler(&self) -> [i32; 3] {
        let [rx, ry, rz] = self.euler_radians();
        [round_turn(rx), (ry.to_degrees() * 1_000_000.0).round() as i32, round_turn(rz)]
    }
    fn euler_radians(&self) -> [f64; 3] {
        let [w, x, y, z] = canonical_q(self.q);
        // A = cos(ry/2) + sin(ry/2), B = cos(ry/2) - sin(ry/2).
        // Their ratio avoids the loss of precision of asin(sin(ry)) near a lock.
        let a = (w + y).hypot(x - z);
        let b = (w - y).hypot(x + z);
        let ry = 2.0 * (a - b).atan2(a + b);
        let ry_udeg = (ry.to_degrees() * 1_000_000.0).round() as i32;
        let minus = (x - z).atan2(w + y); // (rx - rz) / 2
        let plus = (x + z).atan2(w - y); // (rx + rz) / 2
        if 90_000_000 - ry_udeg.abs() <= LOCK_BAND {
            [2.0 * if ry_udeg > 0 { minus } else { plus }, ry, 0.0]
        } else {
            [plus + minus, ry, plus - minus]
        }
    }
    /// This pose with its rotation snapped to the grid: `from_euler(self.t, self.euler())`.
    pub fn snapped(&self) -> Xform3 {
        Xform3::from_euler(self.t, self.euler())
    }
    /// The board turned over about its X axis (bottom side up), then `t`.
    pub fn flipped_over(t: [Nm; 3]) -> Xform3 {
        Xform3::new(t, [0.0, 1.0, 0.0, 0.0])
    }
    /// The canonical form of this transform.
    pub fn canonical(&self) -> Xform3 {
        Xform3::new(self.t, self.q)
    }
    /// Rotate a direction (no translation).
    pub fn rotate(&self, v: [f64; 3]) -> [f64; 3] {
        let q = self.q;
        let p = qmul(qmul(q, [0.0, v[0], v[1], v[2]]), [q[0], -q[1], -q[2], -q[3]]);
        [p[1], p[2], p[3]]
    }
    /// Map a point in this frame to the parent frame, in floating nm.
    pub fn apply_f(&self, p: [f64; 3]) -> [f64; 3] {
        let r = self.rotate(p);
        [r[0] + self.t[0] as f64, r[1] + self.t[1] as f64, r[2] + self.t[2] as f64]
    }
    /// Map a point in this frame to the parent frame, rounded to nm.
    pub fn apply(&self, p: [Nm; 3]) -> [Nm; 3] {
        let r = self.apply_f([p[0] as f64, p[1] as f64, p[2] as f64]);
        [r[0].round() as Nm, r[1].round() as Nm, r[2].round() as Nm]
    }
    /// `self` after `child`: a point in the child's frame mapped through the child, then self.
    pub fn then(&self, child: &Xform3) -> Xform3 {
        Xform3::new(self.apply(child.t), qmul(self.q, child.q))
    }
    /// The inverse transform.
    pub fn inverse(&self) -> Xform3 {
        let qi = [self.q[0], -self.q[1], -self.q[2], -self.q[3]];
        let inv = Xform3 { t: [0; 3], q: qi };
        let t = inv.apply_f([-self.t[0] as f64, -self.t[1] as f64, -self.t[2] as f64]);
        Xform3::new([t[0].round() as Nm, t[1].round() as Nm, t[2].round() as Nm], qi)
    }
}

impl crate::canon::Canon for Xform3 {
    fn canon(&self, w: &mut crate::canon::Writer) {
        self.t.canon(w);
        canonical_q(self.q).canon(w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::Canon;
    use crate::geom::mm;
    use crate::sch::SchPt;

    #[test]
    fn euler_quadrants_are_exact() {
        for (h, expected) in [(0, (0.0, 1.0)), (180_000_000, (1.0, 0.0)), (360_000_000, (0.0, -1.0)), (540_000_000, (-1.0, 0.0))] {
            assert_eq!(half_sin_cos(h), expected, "half-angle quadrant {h} is exact");
            assert_eq!(half_sin_cos(h - 720_000_000), expected, "negative half-angle quadrant {h} reduces exactly");
            let (s, c) = half_sin_cos(h + 90_000_000);
            assert_eq!(s.abs(), c.abs(), "the half-angle quadrant midpoint {h} is symmetric");
        }
        for h in [1, 12_345_679, 89_999_999] {
            let (s, c) = half_sin_cos(h);
            assert_eq!(half_sin_cos(180_000_000 - h), (c, s), "the upper half-quadrant uses the complement at {h}");
        }
        let flip = Xform3::from_euler([0; 3], [180_000_000, 0, 0]);
        assert_eq!(flip, Xform3::flipped_over([0; 3]), "Euler's X half-turn is the exact board flip");
        assert_eq!(flip.q.map(f64::to_bits), Xform3::flipped_over([0; 3]).q.map(f64::to_bits), "the flip agrees bit for bit, including zero signs");
        let t = [mm(3.0), mm(-2.0), 17];
        let identity = Xform3::from_euler(t, [0; 3]);
        assert_eq!(identity, Xform3::translation(t), "zero Euler angles preserve the translation and identity rotation");
        assert_eq!(identity.q.map(f64::to_bits), Xform3::IDENTITY.q.map(f64::to_bits), "identity has the exact quaternion bits");
        for axis in 0..3 {
            let mut e = [0; 3];
            e[axis] = 90_000_000;
            let q = Xform3::from_euler([0; 3], e).q;
            assert_eq!(q[0], q[axis + 1], "axis {axis}'s quarter-turn components are equal");
            for other in 0..3 {
                if other != axis {
                    assert_eq!(q[other + 1], 0.0, "axis {axis}'s quarter-turn has no component on {other}");
                }
            }
        }
    }

    #[test]
    fn euler_grid_round_trips() {
        let t = [mm(1.0), mm(-3.0), 7];
        let check = |e: [i32; 3], canonical: bool| {
            let x = Xform3::from_euler(t, e);
            let got = x.euler();
            if canonical {
                assert_eq!(got, e, "canonical Euler grid triple {e:?} round-trips");
            }
            assert!((-180_000_000..=180_000_000).contains(&got[0]) && got[0] != -180_000_000, "rx is canonical for {e:?}: {got:?}");
            assert!((-90_000_000..=90_000_000).contains(&got[1]), "ry is canonical for {e:?}: {got:?}");
            assert!((-180_000_000..=180_000_000).contains(&got[2]) && got[2] != -180_000_000, "rz is canonical for {e:?}: {got:?}");
            let snapped = x.snapped();
            assert_eq!(snapped.t, t, "snapping preserves translation for {e:?}");
            assert_eq!(snapped.snapped(), snapped, "snapping is idempotent for {e:?}");
            assert_eq!(snapped.euler(), got, "the canonical angles stay fixed for {e:?}");
            if 90_000_000 - got[1].abs() <= LOCK_BAND {
                assert_eq!(got[2], 0, "the lock band folds rz to zero for {e:?}");
            } else {
                for (axis, raw) in x.euler_radians().into_iter().enumerate() {
                    let udeg = raw.to_degrees() * 1_000_000.0;
                    let error = (udeg - got[axis] as f64 + 180_000_000.0).rem_euclid(360_000_000.0) - 180_000_000.0;
                    assert!(error.abs() < 0.1, "outside the lock band axis {axis} of {e:?} has a 5x rounding margin: error {error} udeg");
                }
            }
            (x, snapped)
        };
        for rx in [-90_000_000, 0, 90_000_000, 180_000_000] {
            for ry in [-90_000_000, 0, 90_000_000, 180_000_000] {
                for rz in [-90_000_000, 0, 90_000_000, 180_000_000] {
                    let canonical = ry != 180_000_000 && (ry == 0 || rz == 0);
                    let (x, snapped) = check([rx, ry, rz], canonical);
                    // Noncanonical quarter-turn triples may choose the opposite quaternion
                    // sign at w = 0; compare the reconstructed rotations up to that sign.
                    let same = x.q.iter().zip(snapped.q).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
                    let opposite = x.q.iter().zip(snapped.q).map(|(a, b)| (a + b).abs()).fold(0.0, f64::max);
                    assert!(same.min(opposite) < 1e-15, "quarter turns reconstruct the same pose: {:?}", [rx, ry, rz]);
                }
            }
        }
        for rx in (-179..=180).step_by(7).map(|d| d * 1_000_000).chain([-179_999_999, -123_456_789, -1, 1, 123_456_789, 180_000_000]) {
            for sign in [-1, 1] {
                for k in [0, 1, 2, 3, 100, 101, 1000] {
                    let ry = sign * (90_000_000 - k);
                    check([rx, ry, 0], true);
                    for rz in [-60_000_000, 37_123_456, 60_000_000] {
                        let (x, snapped) = check([rx, ry, rz], k > LOCK_BAND);
                        if k <= LOCK_BAND {
                            let v = [2.0 / 3.0, -1.0 / 3.0, 2.0 / 3.0];
                            let a = x.rotate(v);
                            let b = snapped.rotate(v);
                            let distance = a.into_iter().zip(b).map(|(a, b)| (a - b) * (a - b)).sum::<f64>().sqrt();
                            assert!(distance < 2e-6, "folding the band triple {:?} moves a unit vector by {distance}", [rx, ry, rz]);
                        }
                    }
                }
            }
        }
        let mut seed = 0x53a9_7c21u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed
        };
        for _ in 0..512 {
            let rx = (next() % 360_000_000) as i32 - 179_999_999;
            let ry = (next() % 180_000_001) as i32 - 90_000_000;
            let rz = (next() % 360_000_000) as i32 - 179_999_999;
            let rz = if 90_000_000 - ry.abs() <= LOCK_BAND { 0 } else { rz };
            check([rx, ry, rz], true);
        }
    }

    #[test]
    fn euler_rotation_matches_axis_angle_composition() {
        for e in [[0, 0, 0], [90_000_000, -90_000_000, 180_000_000], [12_345_678, -45_678_901, 98_765_432], [-171_234_567, 89_999_999, -5_432_101]] {
            let rx = Xform3::axis_angle([0; 3], [1.0, 0.0, 0.0], Angle::from_udeg(e[0] as i64));
            let ry = Xform3::axis_angle([0; 3], [0.0, 1.0, 0.0], Angle::from_udeg(e[1] as i64));
            let rz = Xform3::axis_angle([0; 3], [0.0, 0.0, 1.0], Angle::from_udeg(e[2] as i64));
            let expected = rz.then(&ry).then(&rx);
            let got = Xform3::from_euler([17, -23, 41], e);
            for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [2.0, -3.0, 4.0]] {
                for (a, b) in got.rotate(v).into_iter().zip(expected.rotate(v)) {
                    assert!((a - b).abs() < 1e-9, "Euler {e:?} rotates {v:?} in the order Rz * Ry * Rx");
                }
            }
        }
    }

    /// Canonical form, composition, inverse; and the schematic grid beside it.
    #[test]
    fn poses_and_grid() {
        let a = Xform3::new([mm(1.0), 0, 0], [-2.0, 0.0, 0.0, -0.0]);
        assert_eq!(a.q, [1.0, 0.0, 0.0, 0.0], "unit length, w >= 0, no negative zero");
        assert_eq!(a.content_hash(), Xform3::translation([mm(1.0), 0, 0]).content_hash(), "equal poses hash equal");
        let turn = Xform3::axis_angle([0, mm(2.0), 0], [0.0, 0.0, 1.0], Angle::QUARTER);
        assert_eq!(turn.apply([mm(1.0), 0, 0]), [0, mm(3.0), 0], "quarter turn about Z, then translate");
        let both = turn.then(&a);
        assert_eq!(both.apply([0, 0, 0]), turn.apply(a.apply([0, 0, 0])), "composition maps through the child first");
        let back = both.inverse().then(&both);
        assert_eq!(back.apply([mm(5.0), mm(-3.0), mm(1.6)]), [mm(5.0), mm(-3.0), mm(1.6)], "inverse undoes");
        let flip = Xform3::flipped_over([0, 0, 0]);
        assert_eq!(flip.apply([mm(1.0), mm(2.0), mm(1.6)]), [mm(1.0), mm(-2.0), mm(-1.6)], "turned over about X");
        let p = SchPt::new(2, -1);
        assert_eq!(p.to_p(), crate::geom::P::new(2 * SchPt::GRID, -SchPt::GRID), "grid to nm");
        assert_eq!(SchPt::snap(crate::geom::P::new(2 * SchPt::GRID + 100, -SchPt::GRID - 100)), p, "snap");
        assert_eq!(p.rotated_quarters(1), SchPt::new(1, 2), "quarter turn on the grid");
    }
}

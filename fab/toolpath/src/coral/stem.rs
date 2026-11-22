//! A stem's layer is one open turn of a spiral: it runs a little past its start and a line width
//! inside it, so it stands like a tube but is never a closed loop. How stiff a stem is comes from
//! the section that turn actually prints, not from a ring formula: at small radii a ring
//! over-promises.

use std::f64::consts::TAU;

/// Points round a turn.
pub const N: usize = 96;
/// The radius step (mm) of the table a stem is sized from.
const STEP: f64 = 0.05;
/// The widest radius (mm) the table holds; past it a turn's stiffness goes as the cube.
const TABLE_MAX: f64 = 12.0;

/// How a stem's turn is drawn: its line width (mm), how far past its start it runs (rad), and how
/// much it is gently lobed.
#[derive(Clone, Copy, Debug)]
pub struct Turn {
    pub width: f64,
    pub overlap: f64,
    pub lobe: f64,
}

impl Turn {
    /// One turn and `overlap` round `c` from angle `th`, its end a line width inside its start,
    /// lobed three times with phase `ph`: the turn's middle runs at radius `r`. The lobes swell
    /// the turn but not its step inwards, so where it runs past its start it lies exactly a
    /// line width inside it, beads touching and not overlapping (C5).
    pub fn points(&self, c: (f64, f64), r: f64, th: f64, ph: f64, overlap: f64) -> Vec<(f64, f64)> {
        (0..N).map(|i| {
            let a = (TAU + overlap) * i as f64 / (N - 1) as f64;
            let rr = (r + self.width / 2.0) * (1.0 + self.lobe * (3.0 * (th + a) + ph).sin()) - self.width * a / TAU;
            (c.0 + rr * (th + a).cos(), c.1 + rr * (th + a).sin())
        }).collect()
    }
}

/// The least second moment of area (mm4) of open lines `t` (mm) wide: each segment a thin strip
/// along it.
pub fn imin<'a>(lines: impl IntoIterator<Item = &'a [(f64, f64)]>, t: f64) -> f64 {
    let (mut a, mut sx, mut sy, mut ixx, mut iyy, mut ixy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for line in lines {
        for s in line.windows(2) {
            let ((x1, y1), (x2, y2)) = (s[0], s[1]);
            let w = t * (x2 - x1).hypot(y2 - y1);
            a += w;
            sx += w * (x1 + x2) / 2.0;
            sy += w * (y1 + y2) / 2.0;
            iyy += w * (x1 * x1 + x1 * x2 + x2 * x2) / 3.0;
            ixx += w * (y1 * y1 + y1 * y2 + y2 * y2) / 3.0;
            ixy += w * (2.0 * x1 * y1 + x1 * y2 + x2 * y1 + 2.0 * x2 * y2) / 6.0;
        }
    }
    if a == 0.0 {
        return 0.0;
    }
    let (cx, cy) = (sx / a, sy / a);
    let (ixx, iyy, ixy) = (ixx - a * cy * cy, iyy - a * cx * cx, ixy - a * cx * cy);
    (ixx + iyy) / 2.0 - ((ixx - iyy) / 2.0).hypot(ixy)
}

/// How stiff a column has to be. A column whose highest contact is `top` up, printed at its tip,
/// wobbles drag / E · ∫ (top - s)² / I(s) ds; with I(s) = c · top · (top - s)² and c = drag /
/// (E · wobble) that is exactly the wobble allowed.
#[derive(Clone, Copy, Debug)]
pub struct Stiffness {
    c: f64,
}

impl Stiffness {
    /// The nozzle's side push `drag` (N), the material's modulus (N/mm2), the wobble allowed (mm).
    pub fn new(drag: f64, modulus: f64, wobble: f64) -> Self {
        Stiffness { c: drag / (modulus * wobble) }
    }

    /// The I (mm4) a column standing to `top` needs at height `z` (mm).
    pub fn need(&self, z: f64, top: f64) -> f64 {
        let up = (top - z).max(0.0);
        self.c * top.max(0.0) * up * up
    }

    /// Where (mm) a column to `top` needs no more than `i` (mm4).
    pub fn below(&self, i: f64, top: f64) -> f64 {
        if top <= 0.0 { 0.0 } else { top - (i / (self.c * top)).sqrt() }
    }
}

/// The printed turn's Imin at radii a step apart, for sizing stems by what they print.
#[derive(Clone, Debug)]
pub struct Sizes {
    r: Vec<f64>,
    i: Vec<f64>,
}

impl Sizes {
    /// From `min` (mm), the thinnest stem, up.
    pub fn new(turn: &Turn, min: f64) -> Self {
        let n = ((TABLE_MAX - min) / STEP + 1e-9).floor() as usize + 1;
        let r: Vec<f64> = (0..n).map(|k| min + STEP * k as f64).collect();
        let i = r.iter().map(|&r| imin([&turn.points((0.0, 0.0), r, 0.0, 0.0, turn.overlap)[..]], turn.width)).collect();
        Sizes { r, i }
    }

    /// The thinnest stem (mm).
    pub fn min(&self) -> f64 {
        self.r[0]
    }

    /// The I (mm4) the thinnest stem prints.
    pub fn least(&self) -> f64 {
        self.i[0]
    }

    /// The radius (mm) whose printed turn is `need` (mm4) stiff: between the table's steps by
    /// line, past its end as the cube, and never under the thinnest.
    pub fn radius(&self, need: f64) -> f64 {
        let n = self.i.len() - 1;
        if need <= self.i[0] {
            return self.r[0];
        }
        if need >= self.i[n] {
            return self.r[n] * (need / self.i[n]).cbrt();
        }
        let j = self.i.partition_point(|&i| i < need) - 1;
        self.r[j] + STEP * (need - self.i[j]) / (self.i[j + 1] - self.i[j])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURN: Turn = Turn { width: 0.45, overlap: 0.9, lobe: 0.06 };

    /// A turn is open, its end inside its start; a wider turn is stiffer, and its stiffness lies
    /// near a thin ring's, a little over for the overlap.
    #[test]
    fn a_turn_is_open_and_stiffer_as_it_widens() {
        let s = Sizes::new(&TURN, 1.0);
        assert!(s.i.windows(2).all(|w| w[1] > w[0]), "the table grows");
        let pts = TURN.points((0.0, 0.0), 3.0, 0.0, 0.0, 0.9);
        let (a, b) = (pts[0], pts[N - 1]);
        assert!(b.0.hypot(b.1) < a.0.hypot(a.1) - 0.3, "its end lies inside its start: {a:?} {b:?}");
        for r in [1.5, 3.0, 6.0] {
            let i = imin([&TURN.points((0.0, 0.0), r, 0.0, 0.0, 0.9)[..]], TURN.width);
            let ring = std::f64::consts::PI * r * r * r * TURN.width;
            assert!(i > 0.8 * ring && i < ring * (1.0 + 0.9 / TAU) * 1.2, "r {r}: I {i}, a ring {ring}");
            assert!((s.radius(i) - r).abs() < 0.01, "r {r}: sized back to {}", s.radius(i));
        }
        assert_eq!(s.radius(0.0), 1.0);
    }
}

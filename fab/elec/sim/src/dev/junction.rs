//! What every pn junction shares: thermal voltage, the energy gap, temperature scaling of the
//! saturation current and the junction potential, the depletion charge, and junction limiting.

/// Boltzmann's constant over the electron charge, V/K.
pub const K_Q: f64 = 8.617333262e-5;
pub const BOLTZMANN: f64 = 1.380649e-23;
pub const CHARGE: f64 = 1.602176634e-19;

pub fn vt(temp: f64) -> f64 {
    K_Q * temp
}

/// The silicon energy gap at `temp` kelvin (eV).
pub fn egap(temp: f64) -> f64 {
    1.16 - 7.02e-4 * temp * temp / (temp + 1108.0)
}

/// A saturation current at `temp` from its value at `tnom`, with gap `eg` (eV), temperature
/// exponent `xti` and emission coefficient `n`.
pub fn is_at(is: f64, temp: f64, tnom: f64, eg: f64, xti: f64, n: f64) -> f64 {
    let ratio = temp / tnom;
    is * ((ratio - 1.0) * eg / (n * vt(temp))).exp() * ratio.powf(xti / n)
}

/// The junction potential at `temp` from its value `pb` at `tnom`.
pub fn pb_at(pb: f64, temp: f64, tnom: f64) -> f64 {
    let ratio = temp / tnom;
    pb * ratio - 3.0 * vt(temp) * ratio.ln() - (ratio * egap(tnom) - egap(temp))
}

/// A zero-bias junction capacitance at `temp`, given the potentials at both temperatures.
pub fn cj_at(cj: f64, m: f64, temp: f64, tnom: f64, pb_nom: f64, pb_t: f64) -> f64 {
    cj * (1.0 + m * (4e-4 * (temp - tnom) - (pb_t - pb_nom) / pb_nom))
}

/// Depletion charge and capacitance of a junction at `v`: the power law below `fc * pb`, its
/// tangent-continued quadratic above (charge and capacitance continuous at the corner).
pub fn depletion(v: f64, cj0: f64, pb: f64, m: f64, fc: f64) -> (f64, f64) {
    if cj0 == 0.0 {
        return (0.0, 0.0);
    }
    let corner = fc * pb;
    if v < corner {
        let arg = 1.0 - v / pb;
        let s = arg.powf(-m);
        (cj0 * pb * (1.0 - arg * s) / (1.0 - m), cj0 * s)
    } else {
        let f1 = pb * (1.0 - (1.0 - fc).powf(1.0 - m)) / (1.0 - m);
        let f2 = (1.0 - fc).powf(1.0 + m);
        let f3 = 1.0 - fc * (1.0 + m);
        let q = cj0 * (f1 + (f3 * (v - corner) + 0.5 * m / pb * (v * v - corner * corner)) / f2);
        let c = cj0 * (f3 + m * v / pb) / f2;
        (q, c)
    }
}

/// `depletion` for one junction with its constants worked out once: the same arithmetic, so the
/// same bits (the device proofs compare them), without the forward branch's two `powf` at every
/// evaluation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Depletion {
    cj0: f64,
    pb: f64,
    m: f64,
    corner: f64,
    f1: f64,
    f2: f64,
    f3: f64,
    /// `0.5 m / pb`, as the quadratic's coefficient groups it.
    k: f64,
}

impl Depletion {
    pub fn new(cj0: f64, pb: f64, m: f64, fc: f64) -> Depletion {
        Depletion {
            cj0,
            pb,
            m,
            corner: fc * pb,
            f1: pb * (1.0 - (1.0 - fc).powf(1.0 - m)) / (1.0 - m),
            f2: (1.0 - fc).powf(1.0 + m),
            f3: 1.0 - fc * (1.0 + m),
            k: 0.5 * m / pb,
        }
    }
    /// Charge and capacitance at `v`.
    pub fn at(&self, v: f64) -> (f64, f64) {
        let Depletion { cj0, pb, m, corner, f1, f2, f3, k } = *self;
        if cj0 == 0.0 {
            return (0.0, 0.0);
        }
        if v < corner {
            let arg = 1.0 - v / pb;
            let s = arg.powf(-m);
            (cj0 * pb * (1.0 - arg * s) / (1.0 - m), cj0 * s)
        } else {
            let q = cj0 * (f1 + (f3 * (v - corner) + k * (v * v - corner * corner)) / f2);
            let c = cj0 * (f3 + m * v / pb) / f2;
            (q, c)
        }
    }
}

/// The voltage above which a junction's step is limited: `n Vt ln(n Vt / (sqrt 2 Is))`.
pub fn vcrit(nvt: f64, is: f64) -> f64 {
    nvt * (nvt / (std::f64::consts::SQRT_2 * is)).ln()
}

/// Junction limiting: above `vcrit` a step in voltage becomes the equal step in current
/// (logarithmic growth), so the exponential cannot overflow. Returns the limited voltage and
/// whether it limited.
pub fn pnjlim(vnew: f64, vold: f64, nvt: f64, vcrit: f64) -> (f64, bool) {
    if vnew > vcrit && (vnew - vold).abs() > 2.0 * nvt {
        if vold > 0.0 {
            let arg = 1.0 + (vnew - vold) / nvt;
            if arg > 0.0 {
                (vold + nvt * arg.ln(), true)
            } else {
                (vcrit, true)
            }
        } else {
            (nvt * (vnew / nvt).ln(), true)
        }
    } else {
        (vnew, false)
    }
}

/// `exp(x)`, continued linearly above `x = 80` so a wild Newton step cannot overflow.
pub fn exp_lin(x: f64) -> (f64, f64) {
    const X_MAX: f64 = 80.0;
    if x < X_MAX {
        let e = x.exp();
        (e, e)
    } else {
        let e = X_MAX.exp();
        (e * (1.0 + x - X_MAX), e)
    }
}

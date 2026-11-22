//! Small-signal AC (E3 1.8): linearise at the operating point, then one complex solve of
//! `(G + jwC) X = B` per frequency, on the same layout and ordering as the real solves.

use super::op::operating_point;
use super::Sink;
use crate::newton::{DiagKind, SimDiag, Solver};
use crate::num::C64;

#[derive(Clone, Debug, PartialEq)]
pub enum AcSweep {
    /// Points per decade.
    Dec { n: u32, fstart: f64, fstop: f64 },
    /// Points per octave.
    Oct { n: u32, fstart: f64, fstop: f64 },
    /// `n` points, both ends included.
    Lin { n: u32, fstart: f64, fstop: f64 },
    /// Exactly these frequencies.
    List(Vec<f64>),
}

impl AcSweep {
    pub fn freqs(&self) -> Vec<f64> {
        let log = |per: f64, base: f64, a: f64, b: f64| {
            let mut out = Vec::new();
            let mut k = 0;
            loop {
                let f = a * base.powf(k as f64 / per);
                if f > b * (1.0 + 1e-9) {
                    break;
                }
                out.push(f);
                k += 1;
            }
            out
        };
        match *self {
            AcSweep::Dec { n, fstart, fstop } => log(n as f64, 10.0, fstart, fstop),
            AcSweep::Oct { n, fstart, fstop } => log(n as f64, 2.0, fstart, fstop),
            AcSweep::Lin { n, fstart, fstop } => {
                if n <= 1 {
                    vec![fstart]
                } else {
                    (0..n).map(|k| fstart + (fstop - fstart) * k as f64 / (n - 1) as f64).collect()
                }
            }
            AcSweep::List(ref v) => v.clone(),
        }
    }
}

pub fn ac(s: &mut Solver, sweep: &AcSweep, sink: &mut dyn Sink) -> Result<usize, SimDiag> {
    operating_point(s, false)?;
    // the linearisation at the operating point (operating_point ends with a load there)
    let g = s.g.clone();
    let c = s.c.clone();
    let mut rhs = vec![C64::ZERO_C; s.n + 1];
    for b in s.banks.iter() {
        b.ac_rhs(&mut rhs);
    }
    rhs[0] = C64::ZERO_C;
    let mut lu = s.layout.lu::<C64>();
    let mut vals = vec![C64::ZERO_C; g.len()];
    let mut x = vec![C64::ZERO_C; s.n + 1];
    let mut first = true;
    let mut points = 0;
    for f in sweep.freqs() {
        let w = 2.0 * std::f64::consts::PI * f;
        for i in 0..vals.len() {
            vals[i] = C64::new(g[i], w * c[i]);
        }
        for b in s.banks.iter() {
            b.ac_stamp(w, &mut vals);
        }
        let r = if first { lu.factor(&vals) } else { lu.refactor(&vals) };
        if let Err(e) = r {
            let name = s.unknowns[e.col as usize + 1].name.clone();
            return Err(SimDiag { kind: DiagKind::Singular, message: format!("ac at {f:e} Hz: singular at {name}"), at: vec![name], time: None, partial: None });
        }
        first = false;
        x.copy_from_slice(&rhs);
        lu.solve(&mut x[1..]);
        x[0] = C64::ZERO_C;
        sink.ac_point(f, &x);
        points += 1;
    }
    Ok(points)
}

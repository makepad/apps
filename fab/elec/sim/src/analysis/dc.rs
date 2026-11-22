//! DC sweeps (E3 1.8): one or two nested sweeps of a device value; each point continues from
//! the last, and falls back to the full operating-point aids when continuation fails.

use super::op::operating_point;
use super::Sink;
use crate::newton::{Extras, SimDiag, Solver};

/// Sweeps `key` of device `device` over `values` ("dc" of a source, "r" of a resistor ...).
#[derive(Clone, Debug, PartialEq)]
pub struct DcSweep {
    pub device: String,
    pub key: String,
    pub values: Vec<f64>,
}

impl DcSweep {
    /// `from` to `to` (inclusive) in steps of `step`.
    pub fn lin(device: &str, key: &str, from: f64, to: f64, step: f64) -> DcSweep {
        let n = ((to - from) / step).round() as i64;
        let values = (0..=n.max(0)).map(|k| from + k as f64 * step).collect();
        DcSweep { device: device.into(), key: key.into(), values }
    }
}

pub fn dc(s: &mut Solver, sweeps: &[DcSweep], sink: &mut dyn Sink) -> Result<usize, SimDiag> {
    for sw in sweeps {
        if s.find_device(&sw.device).is_none() {
            return Err(SimDiag::setup(format!("dc sweep: no device named {}", sw.device)));
        }
    }
    let mut points = 0;
    match sweeps {
        [] => {}
        [a] => sweep_one(s, a, None, sink, &mut points)?,
        [outer, inner, ..] => {
            for &v in &outer.values {
                set(s, outer, v)?;
                sweep_one(s, inner, Some(v), sink, &mut points)?;
            }
        }
    }
    Ok(points)
}

fn set(s: &mut Solver, sw: &DcSweep, v: f64) -> Result<(), SimDiag> {
    if s.set_param(&sw.device, &sw.key, v) {
        Ok(())
    } else {
        Err(SimDiag::setup(format!("dc sweep: {} has no value {}", sw.device, sw.key)))
    }
}

fn sweep_one(s: &mut Solver, sw: &DcSweep, outer: Option<f64>, sink: &mut dyn Sink, points: &mut usize) -> Result<(), SimDiag> {
    let cx = s.base_cx();
    let itl2 = s.opts.itl2;
    for (k, &v) in sw.values.iter().enumerate() {
        set(s, sw, v)?;
        let cont = k > 0 && s.newton(&cx, false, None, &Extras::default(), itl2).is_ok();
        if !cont {
            operating_point(s, false).map_err(|mut d| {
                d.message = format!("{} = {v:e}: {}", sw.device, d.message);
                d
            })?;
        }
        sink.point(v, outer, &s.x);
        *points += 1;
    }
    Ok(())
}

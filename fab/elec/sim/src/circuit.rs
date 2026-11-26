//! `SimCircuit`: the solver's input, plain data (E3 1.3).
//!
//! Node 0 is ground. Every device carries its name (a component path such as `reg.c_out`), so a
//! diagnostic names the design's part, and live mode carries state over by name. Until the C2
//! freeze names stand in for `PinRef` / `CompRef` / `NetRef`; the builder methods here are what
//! the lowering from `&Design` will call.

use crate::expr::Expr;
use std::collections::HashMap;

/// A node index into `SimCircuit::nodes`; 0 is ground.
pub type Node = u32;
pub const GND: Node = 0;

#[derive(Clone, Debug, PartialEq)]
pub struct NodeInfo {
    pub name: String,
    /// Initial condition: held during the initial operating point, the start value under `uic`.
    pub ic: Option<f64>,
    /// A starting guess for the operating point, released once it converged with it.
    pub nodeset: Option<f64>,
    /// Declared a supply by the design (a power net, or a net on a power-output port) or by the
    /// code that built the circuit: a round plan may take its consumers' currents a round late
    /// there (`analysis::round`). It is the design's intent, never a value.
    pub supply: bool,
    /// The design declares that the circuit's outputs read this node. The flag records intent, never a value.
    pub heard: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// Trapezoidal, order 2 (the default).
    Trap,
    /// Backward differentiation, orders 1 to `max_order` (at most 6).
    Gear,
    /// Backward Euler, order 1.
    Euler,
}

/// Solver options under the classic simulator names, since vendor netlists set them by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub reltol: f64,
    /// Exact (opt-in): a transient is repeated with reltol ten times tighter until two successive
    /// runs agree within `EXACT_TARGET` of full scale (global error control, E3.Q1); Fast (false)
    /// trusts the per-step tolerances.
    pub exact: bool,
    /// Profile (opt-in, for timing series): the solver times device evaluation, factorisations
    /// and solves, and lists every rejected step (`Summary::profile`). Off, it costs nothing.
    pub profile: bool,
    pub vntol: f64,
    pub abstol: f64,
    pub chgtol: f64,
    pub gmin: f64,
    /// Newton iterations after which the DC operating point moves to its next aid.
    pub itl1: u32,
    /// The same for a DC sweep point.
    pub itl2: u32,
    /// The same for a time step (the step is then cut).
    pub itl4: u32,
    pub trtol: f64,
    /// Model nominal temperature, Celsius.
    pub tnom: f64,
    pub method: Method,
    pub max_order: u8,
    /// Force the dense (true) or sparse (false) linear solver; `None` picks by size.
    pub force_dense: Option<bool>,
    /// Nets (or `name#branch` unknowns) that cut the circuit into blocks the solver loads one by
    /// one (`blocks`, ESIMRT C2): rails and mixing sums, which many parts of the circuit touch.
    /// Names the circuit lacks are skipped. Empty: every load evaluates every device at once.
    pub border: Vec<String>,
    /// The three-tier load (`tiers`, ESIMRT S3): a transient load folds the linear devices and
    /// evaluates only the others, and a refactor visits only the columns that changed. Off, every
    /// load evaluates every device and every refactor compares every column (the reference).
    pub tiers: bool,
    /// Fast's device bypass (SPICE's): under a time resolution in effect (`Stepper::set_res`), a
    /// nonlinear instance whose controlling voltages moved by less than a tenth of Newton's
    /// tolerance since its last evaluation keeps that evaluation's stamps and carries its current
    /// and charge along their slopes. Exact never bypasses. Off: every iteration evaluates all.
    /// Read when the solver is built.
    pub bypass: bool,
    /// Fast's single pass (`Bank::settle`), under the bypass: a transient Newton accepts its
    /// solved point without the confirming iteration when every nonlinear device would keep its
    /// stamps there. 0 off; 1 the exact grade (every device inside its bypass band: the point
    /// today's Fast returns, less a no-op iteration); 2 also evaluates the devices outside it at
    /// the point and accepts when each agrees with the line the solve used, by Newton's own
    /// device test. Exact never uses it, nor does Fast with the bypass off. Read at each solve.
    pub single_pass: u8,
    /// The host's compiler for sparse LU plans (`sparse::compiled`): Fast compiles each LU's
    /// refactor and solve on the host's pool and runs them with the same bits. None (the
    /// default, and wherever the host has no code generation) keeps them interpreted.
    pub plan_jit: Option<crate::sparse::compiled::PlanJit>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            reltol: 1e-3,
            exact: false,
            profile: false,
            vntol: 1e-6,
            abstol: 1e-12,
            chgtol: 1e-14,
            gmin: 1e-12,
            itl1: 100,
            itl2: 50,
            itl4: 10,
            trtol: 7.0,
            tnom: 27.0,
            method: Method::Trap,
            max_order: 2,
            force_dense: None,
            border: Vec::new(),
            tiers: true,
            bypass: true,
            single_pass: 2,
            plan_jit: None,
        }
    }
}

/// A time-domain waveform of an independent source.
#[derive(Clone, Debug, PartialEq)]
pub enum Wave {
    /// Constant at the DC value.
    None,
    Pulse { v1: f64, v2: f64, td: f64, tr: f64, tf: f64, pw: f64, per: f64 },
    /// `vo + va * exp(-(t - td) * theta) * sin(2 pi freq (t - td) + phase)`, phase in degrees.
    Sin { vo: f64, va: f64, freq: f64, td: f64, theta: f64, phase: f64 },
    Exp { v1: f64, v2: f64, td1: f64, tau1: f64, td2: f64, tau2: f64 },
    /// Piecewise linear (time, value), times increasing; held at the ends.
    Pwl(Vec<(f64, f64)>),
    /// Single-frequency FM: `vo + va sin(2 pi fc t + mdi sin(2 pi fs t))`.
    Sffm { vo: f64, va: f64, fc: f64, mdi: f64, fs: f64 },
    /// AM: `va (vo + sin(2 pi fm (t - td))) sin(2 pi fc (t - td))`.
    Am { va: f64, vo: f64, fm: f64, fc: f64, td: f64 },
    /// Band-limited white noise: one seeded sequence of standard normal draws a `base` apart,
    /// each held over its cell, and the wave's values at `t = k * dt` (k = 0, 1, 2, ...,
    /// linearly interpolated; before zero, value 0) the sequence's means over `[k dt, (k + 1)
    /// dt)`, by how much of each cell they cover, times `sigma sqrt(dt / base)`: each value
    /// deviates by `sigma` when `dt` is a whole number of cells (a little less else), and at
    /// `dt == base` the values are the draws. The low-frequency two-sided spectral density is
    /// `sigma^2 * dt`, rolling off above `1 / dt`; the same sequence at a longer `dt` and the
    /// same density is the same noise, band-limited. A run should not step longer than `dt`:
    /// crossing several values skips them.
    Noise { seed: u64, sigma: f64, dt: f64, base: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    /// The DC value; when absent the waveform's value at t = 0.
    pub dc: Option<f64>,
    pub ac_mag: f64,
    /// Degrees.
    pub ac_phase: f64,
    pub wave: Wave,
    /// A pending switch: for `t > at` the source reads as the inner one. A lowering never sets it.
    pub then: Option<Box<(f64, Source)>>,
}

/// When a source retune takes effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Retuned {
    /// The source changed at once; the caller must cut the integration history.
    Now,
    /// The running period finishes at this breakpoint; no history cut is needed before it.
    At(f64),
}

impl Source {
    pub fn dc(v: f64) -> Source {
        Source { dc: Some(v), ac_mag: 0.0, ac_phase: 0.0, wave: Wave::None, then: None }
    }
    pub fn ac(dc: f64, mag: f64, phase_deg: f64) -> Source {
        Source { dc: Some(dc), ac_mag: mag, ac_phase: phase_deg, wave: Wave::None, then: None }
    }
    pub fn wave(wave: Wave) -> Source {
        Source { dc: None, ac_mag: 0.0, ac_phase: 0.0, wave, then: None }
    }
    /// The largest magnitude the source reaches (`Wave::peak`), its DC value and a pending
    /// switch's source included.
    pub fn peak(&self) -> f64 {
        let then = self.then.as_ref().map_or(0.0, |p| p.1.peak());
        self.dc.map_or(0.0, f64::abs).max(self.wave.peak()).max(then)
    }
    pub fn value_dc(&self) -> f64 {
        match (self.dc, &self.wave) {
            (Some(v), _) => v,
            (None, w) => w.at(0.0).unwrap_or(0.0),
        }
    }
    pub fn value_at(&self, t: f64) -> f64 {
        if let Some(pending) = &self.then {
            if t > pending.0 {
                return pending.1.value_at(t);
            }
        }
        match self.wave.at(t) {
            Some(v) => v,
            None => self.dc.unwrap_or(0.0),
        }
    }
    /// The value at `t` as a run whose steps cross cells of `cell` seconds reads it: a noise
    /// source whose values are closer than that is read at the cell (`Wave::Noise` with `dt = cell`
    /// and `sigma sqrt(dt / cell)`: the same noise at the same density, band-limited, its values
    /// the means of the draws each cell spans, as a run at a longer period hears it), so a step
    /// across a cell integrates what its values carry instead of reading every few and skipping
    /// the rest. Any other source, and any `cell` not longer than the noise's spacing, as
    /// `value_at`. O(cell / base + 2) draws a noise value.
    pub fn value_in(&self, t: f64, cell: f64) -> f64 {
        if let Some(pending) = &self.then {
            if t > pending.0 {
                return pending.1.value_in(t, cell);
            }
        }
        match self.wave {
            Wave::Noise { seed, sigma, dt, base } if cell > dt => {
                let wave = Wave::Noise { seed, sigma: sigma * (dt / cell).sqrt(), dt: cell, base };
                wave.at(t).unwrap_or_else(|| self.dc.unwrap_or(0.0))
            }
            _ => self.value_at(t),
        }
    }
    /// The next waveform corner strictly after `t`.
    pub fn next_break(&self, t: f64) -> f64 {
        if let Some(pending) = &self.then {
            // a switch within rounding of `t` has been reached: the corners after it are the new source's
            if pending.0 - t <= 1e-13 * t.abs().max(1e-300) {
                return pending.1.next_break(t);
            }
            return self.wave.next_break(t).min(pending.0);
        }
        self.wave.next_break(t)
    }

    /// Whether the value can change after `t` (conservative: true when unsure).
    pub fn varies_after(&self, t: f64) -> bool {
        // a pending switch changes the value just after its time, as `value_at` reads it
        if let Some(pending) = &self.then {
            if t > pending.0 {
                return pending.1.varies_after(t);
            }
            return true;
        }
        self.wave.varies_after(t)
    }

    /// Retunes between steps, finishing a running periodic pulse or sine before switching.
    /// A second retune before that switch replaces it at the same time. The new periodic
    /// waveform's origin moves to the switch time; other waveforms keep their own origin.
    pub fn retune(&mut self, t: f64, mut new: Source) -> Retuned {
        new.then = None;
        if let Some(pending) = self.then.as_mut() {
            if t < pending.0 {
                new.wave.shift_periodic_origin(pending.0);
                pending.1 = new;
                return Retuned::At(pending.0);
            }
        }
        if let Some(pending) = self.then.take() {
            *self = pending.1;
        }
        if let Some((td, per)) = self.wave.period() {
            let rounding = 1e-13 * t.abs().max(1e-300);
            let at = if td - t > rounding {
                td
            } else {
                // Start at the rounding threshold, then skip a boundary that is not after it.
                let k = ((t - td + rounding) / per).floor().max(0.0);
                let at = td + k * per;
                if at - t > rounding { at } else { td + (k + 1.0) * per }
            };
            new.wave.shift_periodic_origin(at);
            self.then = Some(Box::new((at, new)));
            Retuned::At(at)
        } else {
            new.wave.shift_periodic_origin(t);
            *self = new;
            Retuned::Now
        }
    }
}

/// Johnson-Nyquist current noise with sigma = sqrt(2 k T / (r dt)), where T is
/// `temp_c + 273.15` kelvin and `dt` is the run's largest step.
/// Returns `None` unless resistance `r` and sample spacing `dt` are finite and positive.
pub fn thermal_noise_source(r: f64, temp_c: f64, dt: f64, seed: u64) -> Option<Source> {
    if !r.is_finite() || r <= 0.0 || !dt.is_finite() || dt <= 0.0 {
        return None;
    }
    let sigma = (2.0 * 1.380649e-23 * (temp_c + 273.15) / (r * dt)).sqrt();
    Some(Source::wave(Wave::Noise { seed, sigma, dt, base: dt }))
}

impl Wave {
    /// The largest magnitude the wave reaches, an upper bound: a sine's amplitude as undamped,
    /// a noise eight of its deviations.
    pub fn peak(&self) -> f64 {
        match self {
            Wave::None => 0.0,
            Wave::Pulse { v1, v2, .. } | Wave::Exp { v1, v2, .. } => v1.abs().max(v2.abs()),
            Wave::Sin { vo, va, .. } | Wave::Sffm { vo, va, .. } => vo.abs() + va.abs(),
            Wave::Am { va, vo, .. } => va.abs() * (vo.abs() + 1.0),
            Wave::Pwl(points) => points.iter().fold(0.0, |a, p| a.max(p.1.abs())),
            Wave::Noise { sigma, .. } => 8.0 * sigma.abs(),
        }
    }

    fn period(&self) -> Option<(f64, f64)> {
        match *self {
            Wave::Pulse { td, per, .. } if per > 0.0 && per.is_finite() => Some((td, per)),
            Wave::Sin { td, freq, .. } if freq > 0.0 && freq.is_finite() => Some((td, 1.0 / freq)),
            _ => None,
        }
    }

    fn shift_periodic_origin(&mut self, at: f64) {
        if self.period().is_some() {
            match self {
                Wave::Pulse { td, .. } | Wave::Sin { td, .. } => *td += at,
                _ => {}
            }
        }
    }

    pub fn at(&self, t: f64) -> Option<f64> {
        use std::f64::consts::PI;
        Some(match *self {
            Wave::None => return None,
            Wave::Pulse { v1, v2, td, tr, tf, pw, per } => {
                if t <= td {
                    v1
                } else {
                    let mut tau = t - td;
                    if per > 0.0 && per.is_finite() && tau > per {
                        // (0, per]: a corner at a period boundary reads its left value
                        let k = (tau / per).ceil() - 1.0;
                        tau -= k * per;
                    }
                    if tau < tr {
                        v1 + (v2 - v1) * tau / tr
                    } else if tau <= tr + pw {
                        v2
                    } else if tau < tr + pw + tf {
                        v2 + (v1 - v2) * (tau - tr - pw) / tf
                    } else {
                        v1
                    }
                }
            }
            Wave::Sin { vo, va, freq, td, theta, phase } => {
                let ph = phase * PI / 180.0;
                if t < td {
                    vo + va * ph.sin()
                } else {
                    let s = t - td;
                    vo + va * (-s * theta).exp() * (2.0 * PI * freq * s + ph).sin()
                }
            }
            Wave::Exp { v1, v2, td1, tau1, td2, tau2 } => {
                let mut v = v1;
                if t > td1 {
                    v += (v2 - v1) * (1.0 - (-(t - td1) / tau1).exp());
                }
                if t > td2 {
                    v += (v1 - v2) * (1.0 - (-(t - td2) / tau2).exp());
                }
                v
            }
            Wave::Pwl(ref pts) => {
                if pts.is_empty() {
                    return Some(0.0);
                }
                if t <= pts[0].0 {
                    return Some(pts[0].1);
                }
                let i = pts.partition_point(|p| p.0 < t);
                if i >= pts.len() {
                    pts[pts.len() - 1].1
                } else {
                    let (t0, v0) = pts[i - 1];
                    let (t1, v1) = pts[i];
                    if t1 > t0 {
                        v0 + (v1 - v0) * (t - t0) / (t1 - t0)
                    } else {
                        v1
                    }
                }
            }
            Wave::Sffm { vo, va, fc, mdi, fs } => vo + va * (2.0 * PI * fc * t + mdi * (2.0 * PI * fs * t).sin()).sin(),
            Wave::Am { va, vo, fm, fc, td } => {
                if t < td {
                    0.0
                } else {
                    let s = t - td;
                    va * (vo + (2.0 * PI * fm * s).sin()) * (2.0 * PI * fc * s).sin()
                }
            }
            Wave::Noise { seed, sigma, dt, base } => {
                let position = t.max(0.0) / dt;
                let whole = position.floor();
                let k = whole as u64;
                let fraction = position - whole;
                let a = noise_value(seed, dt, base, k);
                let b = noise_value(seed, dt, base, k.saturating_add(1));
                sigma * (a + fraction * (b - a))
            }
        })
    }

    /// Whether the value can change after `t` (conservative: true when unsure).
    pub fn varies_after(&self, t: f64) -> bool {
        match *self {
            Wave::None => false,
            Wave::Pulse { v1, v2, .. } => v1 != v2 && self.next_break(t).is_finite(),
            Wave::Pwl(_) => self.next_break(t).is_finite(),
            Wave::Sin { va, .. } | Wave::Sffm { va, .. } | Wave::Am { va, .. } => va != 0.0,
            Wave::Exp { v1, v2, .. } => v1 != v2,
            Wave::Noise { sigma, .. } => sigma != 0.0,
        }
    }

    pub fn next_break(&self, t: f64) -> f64 {
        // strictly after t by more than rounding, so no step is ever empty
        let after = |b: f64| b - t > 1e-13 * t.abs().max(1e-300);
        match *self {
            Wave::Pulse { td, tr, tf, pw, per, .. } => {
                let corners = [0.0, tr, tr + pw, tr + pw + tf];
                if !(per > 0.0 && per.is_finite()) {
                    return corners.iter().map(|c| td + c).find(|&b| after(b)).unwrap_or(f64::INFINITY);
                }
                let k0 = if t > td { ((t - td) / per).floor() } else { 0.0 };
                for k in [k0, k0 + 1.0] {
                    for c in corners {
                        let b = td + k * per + c;
                        if after(b) {
                            return b;
                        }
                    }
                }
                f64::INFINITY
            }
            Wave::Sin { td, .. } | Wave::Am { td, .. } => {
                if after(td) {
                    td
                } else {
                    f64::INFINITY
                }
            }
            Wave::Exp { td1, td2, .. } => {
                if after(td1) {
                    td1
                } else if after(td2) {
                    td2
                } else {
                    f64::INFINITY
                }
            }
            Wave::Pwl(ref pts) => {
                let i = pts.partition_point(|p| !after(p.0));
                if i < pts.len() {
                    pts[i].0
                } else {
                    f64::INFINITY
                }
            }
            Wave::None | Wave::Sffm { .. } | Wave::Noise { .. } => f64::INFINITY,
        }
    }
}

/// Value `k` of the unit noise of `seed` held a `dt` apart (`Wave::Noise`): the mean of its
/// draws a `base` apart over `[k dt, (k + 1) dt)`, each by how much of its cell it covers, times
/// `sqrt(dt / base)`; the draw itself when `dt == base`. O(dt / base + 2) draws.
fn noise_value(seed: u64, dt: f64, base: f64, k: u64) -> f64 {
    if dt == base || !(base > 0.0 && base.is_finite()) {
        return noise_sample(seed, k);
    }
    let cells = dt / base;
    // an edge a whole number of cells in, but for rounding, is that cell's edge
    let snap = |x: f64| {
        let r = x.round();
        if (x - r).abs() <= 64.0 * f64::EPSILON * r.max(1.0) { r } else { x }
    };
    let (from, to) = (snap(k as f64 * cells), snap((k as f64 + 1.0) * cells));
    let (mut j, mut sum) = (from.floor(), 0.0);
    while j < to {
        sum += ((j + 1.0).min(to) - j.max(from)) * noise_sample(seed, j as u64);
        j += 1.0;
    }
    sum / cells.sqrt()
}

/// Sample `k` of the standard normal sequence of `seed` (splitmix64 and Box-Muller).
fn noise_sample(seed: u64, k: u64) -> f64 {
    let a = splitmix64(seed ^ splitmix64(k));
    let b = splitmix64(a);
    let scale = 1.0 / ((1_u64 << 53) as f64);
    // The radial uniform is in (0, 1], so its logarithm is always finite.
    let u1 = ((a >> 11) as f64 + 1.0) * scale;
    let u2 = (b >> 11) as f64 * scale;
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// One splitmix64 step: a well-mixed 64-bit value of `x`.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Res {
    pub name: String,
    pub a: Node,
    pub b: Node,
    pub r: f64,
    pub tc1: f64,
    pub tc2: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cap {
    pub name: String,
    pub a: Node,
    pub b: Node,
    pub c: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ind {
    pub name: String,
    pub a: Node,
    pub b: Node,
    pub l: f64,
    /// Initial current under `uic`.
    pub ic: Option<f64>,
}

/// A transmission line (T, or O with an `Ltra` card) from port 1 (p1, n1) to port 2 (p2, n2):
/// per metre resistance `r`, inductance `l`, conductance `g` and capacitance `c`, `len` metres.
/// With `model`, the card's values replace these at setup.
#[derive(Clone, Debug, PartialEq)]
pub struct Tline {
    pub name: String,
    pub p1: Node,
    pub n1: Node,
    pub p2: Node,
    pub n2: Node,
    pub model: Option<String>,
    pub r: f64,
    pub l: f64,
    pub g: f64,
    pub c: f64,
    pub len: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mutual {
    pub name: String,
    pub l1: String,
    pub l2: String,
    pub k: f64,
}

/// An independent source. A voltage source holds `v(p) - v(n)`; its branch current flows from
/// `p` through the source to `n`. A current source drives its current from `p` through the
/// source to `n` (out of `p`, into `n`).
#[derive(Clone, Debug, PartialEq)]
pub struct Indep {
    pub name: String,
    pub p: Node,
    pub n: Node,
    pub src: Source,
}

/// A source at a cut between blocks, fed the exporting block's accepted points.
#[derive(Clone, Debug, PartialEq)]
pub struct Boundary {
    pub name: String,
    pub node: Node,
    pub kind: BoundaryKind,
    /// The value before the first table is fed.
    pub v0: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoundaryKind {
    /// Holds the node's voltage against ground, with a branch current into the source.
    Voltage,
    /// Injects current from ground into the node.
    Current,
    /// The holder's port at the voltage boundary `of` (an index into the same circuit's
    /// boundaries): a one-port standing in for the circuit that holds the node, driven by the
    /// boundary's branch current less this source's value (the same current as the holder has
    /// counted it), so the node moves at once by what the holder will do a round later. Its
    /// impedance is `r + rb / (1 + s tau) + s l`. At a declared supply, `second` adds
    /// `rb2 / (1 + s tau2)`. The parameters start at zero, where the boundary holds its value.
    /// It stamps nothing of its own.
    Port { of: u32, second: bool },
    /// Corrects voltage boundary `of` (an index into this circuit's boundaries) by `g` times
    /// its own node's voltage less the delayed table of that node as the far block read it.
    /// The gain defaults to zero. Without a table the correction is inactive.
    Echo { of: u32 },
    /// Table of the dimensionless gain for echo instance `echo`. It owns no unknowns or stamps.
    EchoGain { echo: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CtlKind {
    /// E: `v(p, n) = gain * v(cp, cn)`.
    Vcvs,
    /// G: current `gain * v(cp, cn)` from p through the source to n.
    Vccs,
    /// F: current `gain * i(ctrl)` from p through the source to n.
    Cccs,
    /// H: `v(p, n) = gain * i(ctrl)`.
    Ccvs,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Controlled {
    pub name: String,
    pub kind: CtlKind,
    pub p: Node,
    pub n: Node,
    pub cp: Node,
    pub cn: Node,
    /// The voltage source (or inductor) whose branch current controls F and H.
    pub ctrl: String,
    pub gain: f64,
}

/// What a behavioural element's expression gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BKind {
    /// `v(p, n) = expr` (`B... V=expr`).
    V,
    /// A current `expr` from p through the source to n (`B... I=expr`).
    I,
    /// A resistance `expr` between p and n (`R... {expr}` reading the circuit).
    R,
    /// A capacitance `expr` between p and n (`C... {expr}` reading the circuit), in charge form:
    /// the charge on p is `expr * v(p, n)`, so the charge returns with the state.
    C,
    /// An inductance `expr` from p to n (`L... {expr}` reading the circuit), in flux form: it
    /// claims a branch current `i` and the flux `expr * i`, so the flux returns with the state.
    L,
}

/// A behavioural element: a source (B) whose voltage or current, or a resistor, capacitor or
/// inductor whose resistance, capacitance or inductance, is an expression of node voltages, branch currents, the time and the temperature (the dialect
/// of `expr`).
#[derive(Clone, Debug, PartialEq)]
pub struct Behavioural {
    pub name: String,
    pub kind: BKind,
    pub p: Node,
    pub n: Node,
    pub expr: Expr,
    /// The nodes the expression reads, by name.
    pub reads: Vec<(String, Node)>,
}

/// A voltage-controlled switch (S).
#[derive(Clone, Debug, PartialEq)]
pub struct Switch {
    pub name: String,
    pub p: Node,
    pub n: Node,
    pub cp: Node,
    pub cn: Node,
    /// A current-controlled switch (W): the voltage source or inductor whose current controls it
    /// (`cp` and `cn` are then ground).
    pub ctrl: Option<String>,
    pub model: String,
    /// Starts closed.
    pub on: bool,
}

/// A timing cell (`dev::cell`, Logic mode): a threshold inverter, the pair of hysteretic switches
/// a Schmitt section lowers to, with the one-node RC network on its input taken out of the solve
/// and run in closed form. The output goes high through `up` from `vdd` and low through `down` to
/// `vss`; both switches read the input against `vss` as their models say (`down` closes as it
/// rises into its band, `up`, whose control is vss minus the input, as it falls into its own).
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub name: String,
    pub out: Node,
    pub vdd: Node,
    pub vss: Node,
    /// Its input node, which the cell sets to its form's value (what a probe or a listener there
    /// reads), and that node's name (a hand-over's, by name across lowerings).
    pub inp: Node,
    pub input: String,
    /// The switches' names and `Sw` models.
    pub down: (String, String),
    pub up: (String, String),
    /// The input network's capacitors to fixed nodes (rails), by name: (name, farads, whether its
    /// `a` end is on the input). Their sum is its capacitance.
    pub caps: Vec<(String, f64, bool)>,
    /// Its resistors to outside nodes, by name: (name, node, ohms). Each outside node is read at
    /// the last accepted point and loaded as the resistor loads it. One may be a chain of several
    /// through nodes only they touch, named by its first.
    pub res: Vec<(String, Node, f64)>,
    /// The resistors `res` are made of, by name, each entry's from the input on: (name, the entry
    /// of `res` it is in, ohms, whether its `a` end is the one nearer the input); a resistor with
    /// both ends on a chain's inner node carries nothing and is in none (`u32::MAX`). A value edit
    /// of one sets its share of its entry (`dev::cell::CellBank::set_absorbed`).
    pub parts: Vec<(String, u32, f64, bool)>,
    /// A chain's inner nodes, which the cell sets where the chain's current puts them (what a probe
    /// there reads): (node, its entry of `res`, how many of the entry's resistors lie between it
    /// and the input).
    pub inner: Vec<(Node, u32, u32)>,
    /// The input's voltage at the start, and whether the output starts high.
    pub v0: f64,
    pub high: bool,
    /// Added to every flip: what the closed form leaves out (a calibration's), seconds.
    pub delay: f64,
}

impl Cell {
    /// Sets a part it absorbed by name: a resistor's ohms (its entry of `res` follows) or a
    /// capacitor's farads; false when it absorbed no part of that name. The caller keeps the value
    /// one the part can take.
    pub fn set_part(&mut self, part: &str, v: f64) -> bool {
        if let Some(k) = self.parts.iter().position(|p| p.0 == part) {
            self.parts[k].2 = v;
            let entry = self.parts[k].1;
            let ohms: f64 = self.parts.iter().filter(|p| p.1 == entry).map(|p| p.2).sum();
            if let Some(r) = self.res.get_mut(entry as usize) {
                r.2 = ohms;
            }
            true
        } else if let Some(k) = self.caps.iter_mut().find(|k| k.0 == part) {
            k.1 = v;
            true
        } else {
            false
        }
    }

    /// The share of entry `entry`'s ohms that lies between the input and an inner node `between`
    /// resistors along it (0 at the input, 1 at the entry's outside node).
    pub fn share(&self, entry: u32, between: u32) -> f64 {
        let along: Vec<f64> = self.parts.iter().filter(|p| p.1 == entry).map(|p| p.2).collect();
        let total: f64 = along.iter().sum();
        along.iter().take(between as usize).sum::<f64>() / total
    }

    /// The terminals it reads out: its output's three (`dev::cell::CellBank::terminals`), then
    /// two (`a`, `b`) for each part it absorbed, the resistors in `parts` order, then the
    /// capacitors.
    pub fn terminals(&self) -> usize {
        3 + 2 * (self.parts.len() + self.caps.len())
    }

    /// The terminal of the cell that terminal `terminal` (0 `a`, 1 `b`) of the absorbed part
    /// `part` reads as; None for a part it did not absorb.
    pub fn terminal_of(&self, part: &str, terminal: u32) -> Option<u32> {
        let k = self.parts.iter().position(|p| p.0 == part).or_else(|| self.caps.iter().position(|k| k.0 == part).map(|k| self.parts.len() + k))?;
        (terminal < 2).then(|| 3 + 2 * k as u32 + terminal)
    }
}

/// A GPIO input's pull resistor (`GpioParams::r_pull`) to its high or low rail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PinPull {
    None,
    Up,
    Down,
}

/// What a GPIO pad does: a written mode takes effect over `GpioParams::t_edge` (each side's
/// conductance ramps on its own), except a PWM written over a PWM, which is a preload at the next
/// period start that keeps the period grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PinMode {
    OutHigh,
    OutLow,
    /// High impedance input with its pull.
    Input(PinPull),
    /// Pulls the pad low when asserted, otherwise leaves only the pull.
    OpenDrain { low: bool, pull: PinPull },
    /// High on each period's first `high` seconds, with ramps starting at each edge.
    Pwm { period: f64, high: f64, origin: f64 },
}

/// A GPIO pad's electrical parameters, ohms and seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpioParams {
    /// The high-side driver's on resistance, to vdd.
    pub r_on_h: f64,
    /// The low-side driver's on resistance, to vss.
    pub r_on_l: f64,
    /// The pull-up or pull-down resistance.
    pub r_pull: f64,
    /// The pad's leakage to vss, always present: a pad is never an open circuit.
    pub r_leak: f64,
    /// A mode change's ramp time.
    pub t_edge: f64,
    /// The input's rising threshold, a fraction of the pad's supply (vdd above vss). The pad's
    /// currents do not depend on it: it is what a co-simulated process watches (`CoSimPin::vih`).
    pub vih: f64,
    /// The input's falling threshold, a fraction of the supply.
    pub vil: f64,
}

/// An MCU pad between its supply rails: rail conductances set by its mode, a leakage, finite
/// edges and voltage watches (`dev::gpio`).
#[derive(Clone, Debug, PartialEq)]
pub struct Gpio {
    pub name: String,
    pub pad: Node,
    pub vdd: Node,
    pub vss: Node,
    pub params: GpioParams,
    pub mode: PinMode,
}

/// Which crossings of a pad watch's level count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PinEdge {
    Rising,
    Falling,
    Both,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diode {
    pub name: String,
    pub a: Node,
    pub k: Node,
    pub model: String,
    pub area: f64,
    pub off: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bjt {
    pub name: String,
    pub c: Node,
    pub b: Node,
    pub e: Node,
    pub s: Node,
    pub model: String,
    pub area: f64,
    pub off: bool,
}

/// A three-terminal JFET (J), in drain, gate, source order.
#[derive(Clone, Debug, PartialEq)]
pub struct Jfet {
    pub name: String,
    pub d: Node,
    pub g: Node,
    pub s: Node,
    pub model: String,
    pub area: f64,
    pub off: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reg {
    pub name: String,
    pub vin: Node,
    pub gnd: Node,
    pub vout: Node,
    pub en: Node,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mos {
    pub name: String,
    pub d: Node,
    pub g: Node,
    pub s: Node,
    pub b: Node,
    pub model: String,
    pub w: f64,
    pub l: f64,
    /// Drain and source diffusion areas and perimeters, m^2 and m.
    pub ad: f64,
    pub as_: f64,
    pub pd: f64,
    pub ps: f64,
    pub off: bool,
    /// Devices in parallel (`m`): scales every current, charge and conductance.
    pub mult: f64,
    /// Drain and source diffusion squares: with the card's `rsh` and no `rd` / `rs`, the series
    /// resistances are `rsh * nrd` and `rsh * nrs`.
    pub nrd: f64,
    pub nrs: f64,
    /// The device's own temperature, degrees C (`temp`); absent takes the circuit's.
    pub temp: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelKind {
    Diode,
    Npn,
    Pnp,
    Nmos,
    Pmos,
    Njf,
    Pjf,
    Sw,
    /// A current-controlled switch's card (`CSW`): `it`, `ih`, `ron`, `roff` and the band `iband`.
    Csw,
    /// A transmission line's card (`LTRA`): `r`, `l`, `g`, `c` per metre and `len`.
    Ltra,
    Reg,
}

/// A `.model` card: parameters by their classic names; each device parses its own with defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelCard {
    pub name: String,
    pub kind: ModelKind,
    pub params: Vec<(String, f64)>,
}

impl ModelCard {
    pub fn get(&self, key: &str) -> Option<f64> {
        self.params.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| *v)
    }
    pub fn or(&self, key: &str, default: f64) -> f64 {
        self.get(key).unwrap_or(default)
    }
}

/// The device instances, one vector per type (each becomes one bank in the solver).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Banks {
    pub r: Vec<Res>,
    pub c: Vec<Cap>,
    pub l: Vec<Ind>,
    pub k: Vec<Mutual>,
    pub v: Vec<Indep>,
    pub i: Vec<Indep>,
    pub boundary: Vec<Boundary>,
    pub ctl: Vec<Controlled>,
    pub b: Vec<Behavioural>,
    pub sw: Vec<Switch>,
    pub gpio: Vec<Gpio>,
    pub d: Vec<Diode>,
    pub q: Vec<Bjt>,
    pub reg: Vec<Reg>,
    pub m: Vec<Mos>,
    pub j: Vec<Jfet>,
    pub o: Vec<Tline>,
    pub cell: Vec<Cell>,
}

impl Banks {
    // Every new device field holding a Node must be added to both walks below.
    /// Visits every stored node, including controls and behavioural expression reads.
    pub fn for_each_node_mut(&mut self, mut f: impl FnMut(&mut Node)) {
        for d in &mut self.r {
            f(&mut d.a);
            f(&mut d.b);
        }
        for d in &mut self.c {
            f(&mut d.a);
            f(&mut d.b);
        }
        for d in &mut self.l {
            f(&mut d.a);
            f(&mut d.b);
        }
        // Mutual inductors (k) carry device names, not nodes.
        for d in self.v.iter_mut().chain(&mut self.i) {
            f(&mut d.p);
            f(&mut d.n);
        }
        for d in &mut self.boundary {
            f(&mut d.node);
        }
        for d in &mut self.ctl {
            f(&mut d.p);
            f(&mut d.n);
            f(&mut d.cp);
            f(&mut d.cn);
        }
        for d in &mut self.b {
            f(&mut d.p);
            f(&mut d.n);
            for (_, node) in &mut d.reads {
                f(node);
            }
        }
        for d in &mut self.sw {
            f(&mut d.p);
            f(&mut d.n);
            f(&mut d.cp);
            f(&mut d.cn);
        }
        for d in &mut self.gpio {
            f(&mut d.pad);
            f(&mut d.vdd);
            f(&mut d.vss);
        }
        for d in &mut self.d {
            f(&mut d.a);
            f(&mut d.k);
        }
        for d in &mut self.q {
            f(&mut d.c);
            f(&mut d.b);
            f(&mut d.e);
            f(&mut d.s);
        }
        for d in &mut self.reg {
            f(&mut d.vin);
            f(&mut d.gnd);
            f(&mut d.vout);
            f(&mut d.en);
        }
        for d in &mut self.m {
            f(&mut d.d);
            f(&mut d.g);
            f(&mut d.s);
            f(&mut d.b);
        }
        for d in &mut self.j {
            f(&mut d.d);
            f(&mut d.g);
            f(&mut d.s);
        }
        for d in &mut self.o {
            f(&mut d.p1);
            f(&mut d.n1);
            f(&mut d.p2);
            f(&mut d.n2);
        }
        for d in &mut self.cell {
            f(&mut d.out);
            f(&mut d.vdd);
            f(&mut d.vss);
            f(&mut d.inp);
            for (node, _, _) in &mut d.inner {
                f(node);
            }
            for (_, node, _) in &mut d.res {
                f(node);
            }
        }
    }

    /// Visits the same nodes, in the same order, as `for_each_node_mut`.
    pub fn for_each_node(&self, mut f: impl FnMut(Node)) {
        for d in &self.r {
            f(d.a);
            f(d.b);
        }
        for d in &self.c {
            f(d.a);
            f(d.b);
        }
        for d in &self.l {
            f(d.a);
            f(d.b);
        }
        // Mutual inductors (k) carry device names, not nodes.
        for d in self.v.iter().chain(&self.i) {
            f(d.p);
            f(d.n);
        }
        for d in &self.boundary {
            f(d.node);
        }
        for d in &self.ctl {
            f(d.p);
            f(d.n);
            f(d.cp);
            f(d.cn);
        }
        for d in &self.b {
            f(d.p);
            f(d.n);
            for &(_, node) in &d.reads {
                f(node);
            }
        }
        for d in &self.sw {
            f(d.p);
            f(d.n);
            f(d.cp);
            f(d.cn);
        }
        for d in &self.gpio {
            f(d.pad);
            f(d.vdd);
            f(d.vss);
        }
        for d in &self.d {
            f(d.a);
            f(d.k);
        }
        for d in &self.q {
            f(d.c);
            f(d.b);
            f(d.e);
            f(d.s);
        }
        for d in &self.reg {
            f(d.vin);
            f(d.gnd);
            f(d.vout);
            f(d.en);
        }
        for d in &self.m {
            f(d.d);
            f(d.g);
            f(d.s);
            f(d.b);
        }
        for d in &self.j {
            f(d.d);
            f(d.g);
            f(d.s);
        }
        for d in &self.o {
            f(d.p1);
            f(d.n1);
            f(d.p2);
            f(d.n2);
        }
        for d in &self.cell {
            f(d.out);
            f(d.vdd);
            f(d.vss);
            f(d.inp);
            for &(node, _, _) in &d.inner {
                f(node);
            }
            for &(_, node, _) in &d.res {
                f(node);
            }
        }
    }

    /// Keeps the devices whose names pass `keep`, preserving order within each list.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.r.retain(|d| keep(&d.name));
        self.c.retain(|d| keep(&d.name));
        self.l.retain(|d| keep(&d.name));
        self.k.retain(|d| keep(&d.name));
        self.v.retain(|d| keep(&d.name));
        self.i.retain(|d| keep(&d.name));
        self.boundary.retain(|d| keep(&d.name));
        self.ctl.retain(|d| keep(&d.name));
        self.b.retain(|d| keep(&d.name));
        self.sw.retain(|d| keep(&d.name));
        self.gpio.retain(|d| keep(&d.name));
        self.d.retain(|d| keep(&d.name));
        self.q.retain(|d| keep(&d.name));
        self.reg.retain(|d| keep(&d.name));
        self.m.retain(|d| keep(&d.name));
        self.j.retain(|d| keep(&d.name));
        self.o.retain(|d| keep(&d.name));
        self.cell.retain(|d| keep(&d.name));
    }
}

/// One value-only difference between two lowerings of the same circuit.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// An independent source's waveform (`LiveCmd::Retune`).
    Source(String, Source),
    /// A GPIO's mode (`LiveCmd::PinMode`).
    Pin(String, PinMode),
    /// A resistor's, capacitor's or inductor's value (`LiveCmd::SetParam` with key `value`).
    Value(String, f64),
}

#[derive(Clone, Debug)]
pub struct SimCircuit {
    pub nodes: Vec<NodeInfo>,
    pub devices: Banks,
    pub models: Vec<ModelCard>,
    pub opts: Options,
    /// Circuit temperature, Celsius.
    pub temp_c: f64,
    index: HashMap<String, Node>,
}

impl Default for SimCircuit {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for SimCircuit {
    fn eq(&self, o: &Self) -> bool {
        self.nodes == o.nodes && self.devices == o.devices && self.models == o.models && self.opts == o.opts && self.temp_c == o.temp_c
    }
}

impl SimCircuit {
    /// The largest magnitude an independent voltage source of the circuit reaches (`Source::peak`),
    /// volts: what its nodes are held to, bar what its parts make of it.
    pub fn source_peak(&self) -> f64 {
        self.devices.v.iter().fold(0.0, |a, v| a.max(v.src.peak()))
    }

    pub fn new() -> Self {
        let mut index = HashMap::new();
        index.insert("0".to_string(), GND);
        SimCircuit {
            nodes: vec![NodeInfo { name: "0".into(), ic: None, nodeset: None, supply: false, heard: false }],
            devices: Banks::default(),
            models: Vec::new(),
            opts: Options::default(),
            temp_c: 27.0,
            index,
        }
    }

    /// Keeps the named devices over only their used nodes plus ground, in old node order.
    /// Node metadata, models, options and temperature are preserved. References by name
    /// (`Mutual::l1`/`l2`, `Controlled::ctrl`, `Switch::ctrl` and names in behavioural
    /// expressions and reads) are unchanged; the caller decides whether both ends are kept.
    pub fn subcircuit(&self, keep: impl FnMut(&str) -> bool) -> SimCircuit {
        let mut devices = self.devices.clone();
        devices.retain(keep);
        let mut used = vec![false; self.nodes.len()];
        used[GND as usize] = true;
        devices.for_each_node(|node| used[node as usize] = true);

        let mut nodes = Vec::new();
        let mut index = HashMap::new();
        let mut remap = vec![GND; self.nodes.len()];
        for (old, info) in self.nodes.iter().enumerate() {
            if used[old] {
                let node = nodes.len() as Node;
                remap[old] = node;
                index.insert(info.name.clone(), node);
                nodes.push(info.clone());
            }
        }
        devices.for_each_node_mut(|node| *node = remap[*node as usize]);
        SimCircuit { nodes, devices, models: self.models.clone(), opts: self.opts.clone(), temp_c: self.temp_c, index }
    }

    /// The differences a host can send as value commands instead of a Replace, each with its new
    /// value: an independent source's waveform, a GPIO's mode, and a resistor's (finite, non-zero),
    /// capacitor's or inductor's (one no mutual couples) value. A resistor's thermal noise source
    /// (`#noise`) follows its value in a live run with its draws kept, so its new density is not
    /// listed. A node's heard flag (`NodeInfo::heard`) is what the plan weighs, not the circuit:
    /// a probe that only adds one keeps the run, and the run keeps its plan's heard nets until a
    /// change reaches its planner. Empty when equal; `None` when anything else differs (a node, a
    /// model, a device added, moved or renamed, another field). O(devices).
    pub fn value_changes(&self, new: &SimCircuit) -> Option<Vec<Change>> {
        let (a, b) = (&self.devices, &new.devices);
        let same_nodes = self.nodes.len() == new.nodes.len()
            && self.nodes.iter().zip(&new.nodes).all(|(old, new)| NodeInfo { heard: new.heard, ..old.clone() } == *new);
        if !same_nodes || self.models != new.models || self.opts != new.opts || self.temp_c != new.temp_c
            || a.k != b.k || a.ctl != b.ctl || a.b != b.b || a.sw != b.sw
            || a.d != b.d || a.q != b.q || a.reg != b.reg || a.m != b.m || a.j != b.j || a.o != b.o || a.boundary != b.boundary || a.cell != b.cell
            || a.r.len() != b.r.len() || a.c.len() != b.c.len() || a.l.len() != b.l.len()
            || a.v.len() != b.v.len() || a.i.len() != b.i.len() || a.gpio.len() != b.gpio.len()
        {
            return None;
        }
        let mut changes = Vec::new();
        let mut value = |name: &str, old: f64, new: f64, ok: bool| {
            if old.to_bits() == new.to_bits() {
                return Some(());
            }
            ok.then(|| changes.push(Change::Value(name.to_owned(), new)))
        };
        for (old, new) in a.r.iter().zip(&b.r) {
            if (&old.name, old.a, old.b, old.tc1, old.tc2) != (&new.name, new.a, new.b, new.tc1, new.tc2) {
                return None;
            }
            value(&new.name, old.r, new.r, new.r.is_finite() && new.r != 0.0 && old.r.is_finite() && old.r != 0.0)?;
        }
        for (old, new) in a.c.iter().zip(&b.c) {
            if (&old.name, old.a, old.b) != (&new.name, new.a, new.b) {
                return None;
            }
            value(&new.name, old.c, new.c, new.c.is_finite())?;
        }
        for (old, new) in a.l.iter().zip(&b.l) {
            if (&old.name, old.a, old.b, old.ic) != (&new.name, new.a, new.b, new.ic) {
                return None;
            }
            let coupled = a.k.iter().any(|k| k.l1 == new.name || k.l2 == new.name);
            value(&new.name, old.l, new.l, new.l.is_finite() && !coupled)?;
        }
        for (old, new) in a.v.iter().zip(&b.v).chain(a.i.iter().zip(&b.i)) {
            if old.name != new.name || old.p != new.p || old.n != new.n {
                return None;
            }
            let follows = |r: &str| changes.iter().any(|c| matches!(c, Change::Value(name, _) if name == r));
            if old.src != new.src && !new.name.strip_suffix("#noise").is_some_and(follows) {
                changes.push(Change::Source(new.name.clone(), new.src.clone()));
            }
        }
        for (old, new) in a.gpio.iter().zip(&b.gpio) {
            if old.name != new.name || old.pad != new.pad || old.vdd != new.vdd || old.vss != new.vss || old.params != new.params {
                return None;
            }
            if old.mode != new.mode {
                changes.push(Change::Pin(new.name.clone(), new.mode));
            }
        }
        Some(changes)
    }

    /// The node of that name, created on first use. "0" and "gnd" are ground.
    pub fn node(&mut self, name: &str) -> Node {
        if name.eq_ignore_ascii_case("gnd") {
            return GND;
        }
        if let Some(&n) = self.index.get(name) {
            return n;
        }
        let n = self.nodes.len() as Node;
        self.nodes.push(NodeInfo { name: name.to_string(), ic: None, nodeset: None, supply: false, heard: false });
        self.index.insert(name.to_string(), n);
        n
    }

    pub fn find_node(&self, name: &str) -> Option<Node> {
        if name.eq_ignore_ascii_case("gnd") {
            return Some(GND);
        }
        self.index.get(name).copied()
    }

    pub fn set_ic(&mut self, node: &str, v: f64) {
        let n = self.node(node);
        self.nodes[n as usize].ic = Some(v);
    }

    pub fn set_nodeset(&mut self, node: &str, v: f64) {
        let n = self.node(node);
        self.nodes[n as usize].nodeset = Some(v);
    }

    /// Declares the node a supply (`NodeInfo::supply`); ground is no supply.
    pub fn declare_supply(&mut self, node: &str) {
        let n = self.node(node);
        if n != GND {
            self.nodes[n as usize].supply = true;
        }
    }

    /// Declares a voltage node heard by the circuit's outputs (`NodeInfo::heard`). Ground is never heard.
    pub fn declare_heard(&mut self, node: &str) {
        let n = self.node(node);
        if n != GND {
            self.nodes[n as usize].heard = true;
        }
    }

    pub fn model(&mut self, name: &str, kind: ModelKind, params: &[(&str, f64)]) {
        self.models.retain(|m| m.name != name);
        self.models.push(ModelCard { name: name.into(), kind, params: params.iter().map(|(k, v)| (k.to_string(), *v)).collect() });
    }

    pub fn find_model(&self, name: &str) -> Option<&ModelCard> {
        self.models.iter().find(|m| m.name.eq_ignore_ascii_case(name))
    }

    pub fn r(&mut self, name: &str, a: &str, b: &str, r: f64) {
        let (a, b) = (self.node(a), self.node(b));
        self.devices.r.push(Res { name: name.into(), a, b, r, tc1: 0.0, tc2: 0.0 });
    }

    /// Adds Johnson-Nyquist current noise across a resistor; `dt` should be the run's largest step.
    pub fn thermal_noise(&mut self, resistor: &str, dt: f64, seed: u64) -> bool {
        let Some(res) = self.devices.r.iter().find(|res| res.name == resistor) else {
            return false;
        };
        let Some(src) = thermal_noise_source(res.r, self.temp_c, dt, seed) else {
            return false;
        };
        self.devices.i.push(Indep {
            name: format!("{resistor}#noise"),
            p: res.a,
            n: res.b,
            src,
        });
        true
    }

    pub fn c(&mut self, name: &str, a: &str, b: &str, c: f64) {
        let (a, b) = (self.node(a), self.node(b));
        self.devices.c.push(Cap { name: name.into(), a, b, c });
    }

    pub fn l(&mut self, name: &str, a: &str, b: &str, l: f64) {
        let (a, b) = (self.node(a), self.node(b));
        self.devices.l.push(Ind { name: name.into(), a, b, l, ic: None });
    }

    /// T: a lossless line of characteristic impedance `z0` and delay `td` (one metre of
    /// l = td z0 and c = td / z0).
    pub fn tline(&mut self, name: &str, p1: &str, n1: &str, p2: &str, n2: &str, z0: f64, td: f64) {
        let [p1, n1, p2, n2] = [p1, n1, p2, n2].map(|n| self.node(n));
        self.devices.o.push(Tline { name: name.into(), p1, n1, p2, n2, model: None, r: 0.0, l: td * z0, g: 0.0, c: td / z0, len: 1.0 });
    }

    /// O: a line whose `Ltra` card gives r, l, g, c per metre and len.
    pub fn ltra(&mut self, name: &str, p1: &str, n1: &str, p2: &str, n2: &str, model: &str) {
        let [p1, n1, p2, n2] = [p1, n1, p2, n2].map(|n| self.node(n));
        self.devices.o.push(Tline { name: name.into(), p1, n1, p2, n2, model: Some(model.into()), r: 0.0, l: 0.0, g: 0.0, c: 0.0, len: 1.0 });
    }

    pub fn k(&mut self, name: &str, l1: &str, l2: &str, k: f64) {
        self.devices.k.push(Mutual { name: name.into(), l1: l1.into(), l2: l2.into(), k });
    }

    pub fn v(&mut self, name: &str, p: &str, n: &str, src: Source) {
        let (p, n) = (self.node(p), self.node(n));
        self.devices.v.push(Indep { name: name.into(), p, n, src });
    }

    pub fn i(&mut self, name: &str, p: &str, n: &str, src: Source) {
        let (p, n) = (self.node(p), self.node(n));
        self.devices.i.push(Indep { name: name.into(), p, n, src });
    }

    /// E or G: controlled by `v(cp, cn)`.
    pub fn vctl(&mut self, name: &str, kind: CtlKind, p: &str, n: &str, cp: &str, cn: &str, gain: f64) {
        let (p, n, cp, cn) = (self.node(p), self.node(n), self.node(cp), self.node(cn));
        self.devices.ctl.push(Controlled { name: name.into(), kind, p, n, cp, cn, ctrl: String::new(), gain });
    }

    /// F or H: controlled by the branch current of `ctrl`.
    pub fn ictl(&mut self, name: &str, kind: CtlKind, p: &str, n: &str, ctrl: &str, gain: f64) {
        let (p, n) = (self.node(p), self.node(n));
        self.devices.ctl.push(Controlled { name: name.into(), kind, p, n, cp: GND, cn: GND, ctrl: ctrl.into(), gain });
    }

    /// B, or a behavioural R, C or L: a voltage, current, resistance, capacitance or inductance
    /// given by `expr`. Each node the
    /// expression reads is a node of the circuit (named here, it exists); a node read and
    /// connected to nothing has no DC path.
    pub fn b(&mut self, name: &str, kind: BKind, p: &str, n: &str, expr: Expr) {
        let (p, n) = (self.node(p), self.node(n));
        let names: Vec<String> = expr.nodes().map(str::to_string).collect();
        let reads = names.into_iter().map(|s| {
            let node = self.node(&s);
            (s, node)
        }).collect();
        self.devices.b.push(Behavioural { name: name.into(), kind, p, n, expr, reads });
    }

    pub fn sw(&mut self, name: &str, p: &str, n: &str, cp: &str, cn: &str, model: &str, on: bool) {
        let (p, n, cp, cn) = (self.node(p), self.node(n), self.node(cp), self.node(cn));
        self.devices.sw.push(Switch { name: name.into(), p, n, cp, cn, ctrl: None, model: model.into(), on });
    }

    /// W: a switch between p and n controlled by the current through `ctrl` (a voltage source or
    /// an inductor), with a `Csw` model.
    pub fn csw(&mut self, name: &str, p: &str, n: &str, ctrl: &str, model: &str, on: bool) {
        let (p, n) = (self.node(p), self.node(n));
        self.devices.sw.push(Switch { name: name.into(), p, n, cp: GND, cn: GND, ctrl: Some(ctrl.into()), model: model.into(), on });
    }

    pub fn gpio(&mut self, name: &str, pad: &str, vdd: &str, vss: &str, params: GpioParams, mode: PinMode) {
        let (pad, vdd, vss) = (self.node(pad), self.node(vdd), self.node(vss));
        self.devices.gpio.push(Gpio { name: name.into(), pad, vdd, vss, params, mode });
    }

    /// A timing cell (`Cell`); its nodes by index, as `SimCircuit::node` made them.
    pub fn cell(&mut self, cell: Cell) {
        self.devices.cell.push(cell);
    }

    pub fn d(&mut self, name: &str, a: &str, k: &str, model: &str) {
        let (a, k) = (self.node(a), self.node(k));
        self.devices.d.push(Diode { name: name.into(), a, k, model: model.into(), area: 1.0, off: false });
    }

    pub fn q(&mut self, name: &str, c: &str, b: &str, e: &str, model: &str) {
        let (c, b, e) = (self.node(c), self.node(b), self.node(e));
        self.devices.q.push(Bjt { name: name.into(), c, b, e, s: GND, model: model.into(), area: 1.0, off: false });
    }

    pub fn q4(&mut self, name: &str, c: &str, b: &str, e: &str, s: &str, model: &str) {
        let (c, b, e, s) = (self.node(c), self.node(b), self.node(e), self.node(s));
        self.devices.q.push(Bjt { name: name.into(), c, b, e, s, model: model.into(), area: 1.0, off: false });
    }

    pub fn jfet(&mut self, name: &str, d: &str, g: &str, s: &str, model: &str, area: f64) {
        let (d, g, s) = (self.node(d), self.node(g), self.node(s));
        self.devices.j.push(Jfet { name: name.into(), d, g, s, model: model.into(), area, off: false });
    }

    pub fn reg(&mut self, name: &str, vin: &str, gnd: &str, vout: &str, en: &str, model: &str) {
        let (vin, gnd, vout, en) = (self.node(vin), self.node(gnd), self.node(vout), self.node(en));
        self.devices.reg.push(Reg { name: name.into(), vin, gnd, vout, en, model: model.into() });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn m(&mut self, name: &str, d: &str, g: &str, s: &str, b: &str, model: &str, w: f64, l: f64) {
        let (d, g, s, b) = (self.node(d), self.node(g), self.node(s), self.node(b));
        self.devices.m.push(Mos {
            name: name.into(), d, g, s, b, model: model.into(), w, l, ad: 0.0, as_: 0.0, pd: 0.0, ps: 0.0, off: false,
            mult: 1.0, nrd: 1.0, nrs: 1.0, temp: None,
        });
    }
}

//! MCU pads with rail conductances, finite edges, PWM preload and voltage watches.

use crate::circuit::{Gpio, GpioParams, PinEdge, PinMode, PinPull, Retuned};
use crate::stamp::{Bank, EvalCx, Stamps, Topology, Unk};

#[derive(Clone, Copy)]
struct Ramp {
    t: f64,
    gh: f64,
    gl: f64,
}

#[derive(Clone, Copy, Default)]
struct Runtime {
    ramp: Option<Ramp>,
    pending: Option<(f64, PinMode)>,
    accepted: Option<(f64, f64)>,
    /// The conductances last stamped (`gh`, `gl` with the leak) and the time and analysis they
    /// are for: they depend only on those and on the pad's mode, ramp, pending write and
    /// parameters, whose every change drops it, so the iterations of one time point reuse them.
    memo: Option<Memo>,
}

#[derive(Clone, Copy)]
struct Memo {
    t: f64,
    tran: bool,
    gh: f64,
    gl: f64,
}

#[derive(Clone)]
struct Watch {
    inst: usize,
    id: u32,
    level: f64,
    edge: PinEdge,
}

#[derive(Clone)]
pub struct GpioBank {
    pub inst: Vec<Gpio>,
    runtime: Vec<Runtime>,
    slots: Vec<u32>,
    watches: Vec<Watch>,
    hits: Vec<(u32, f64)>,
    /// The time resolution (`Bank::set_res`): an edge's ramp no longer than it ends with no corner.
    res: f64,
}

fn valid_mode(mode: PinMode) -> bool {
    match mode {
        PinMode::Pwm { period, high, origin } => {
            period.is_finite() && high.is_finite() && origin.is_finite() && high > 0.0 && high < period
        }
        _ => true,
    }
}

fn after(b: f64, t: f64) -> bool {
    b - t > 1e-13 * t.abs().max(1e-300)
}

fn period_start(t: f64, origin: f64, period: f64) -> f64 {
    if after(origin, t) {
        return origin;
    }
    let rounding = 1e-13 * t.abs().max(1e-300);
    let k = ((t - origin + rounding) / period).floor().max(0.0);
    let at = origin + k * period;
    if after(at, t) { at } else { origin + (k + 1.0) * period }
}

/// The next PWM corner strictly after `t`: each edge's start and, when its ramp is longer than
/// `res`, its end.
fn pwm_break(mode: PinMode, t_edge: f64, res: f64, t: f64) -> f64 {
    pwm_corner(mode, t_edge, t, |end, e| !end || e > res)
}

/// The next PWM corner strictly after `t` that `pwm_break` leaves out: an edge's end when its ramp
/// is no longer than `res`.
fn pwm_hidden(mode: PinMode, t_edge: f64, res: f64, t: f64) -> f64 {
    pwm_corner(mode, t_edge, t, |end, e| end && e <= res)
}

/// The next PWM corner strictly after `t` that `keep` (is it an edge's end, the ramp's length)
/// takes.
fn pwm_corner(mode: PinMode, t_edge: f64, t: f64, keep: impl Fn(bool, f64) -> bool) -> f64 {
    let PinMode::Pwm { period, high, origin } = mode else { return f64::INFINITY };
    let e = t_edge.min(high).min(period - high);
    let rounding = 1e-13 * t.abs().max(1e-300);
    let k0 = ((t - origin + rounding) / period).floor().max(0.0);
    for k in [k0, k0 + 1.0] {
        for (corner, end) in [(0.0, false), (e, true), (high, false), (high + e, true)] {
            if !keep(end, e) {
                continue;
            }
            let at = origin + k * period + corner;
            if after(at, t) {
                return at;
            }
        }
    }
    f64::INFINITY
}

fn target(mode: PinMode, p: GpioParams, t: f64) -> (f64, f64) {
    let (pull, low) = match mode {
        PinMode::OutHigh => return (1.0 / p.r_on_h, 0.0),
        PinMode::OutLow => return (0.0, 1.0 / p.r_on_l),
        PinMode::Input(pull) => (pull, false),
        PinMode::OpenDrain { low, pull } => (pull, low),
        PinMode::Pwm { period, high, origin } => {
            let e = p.t_edge.min(high).min(period - high);
            let tau = (t - origin).rem_euclid(period);
            let h = if t < origin {
                0.0
            } else if tau < high {
                (tau / e).clamp(0.0, 1.0)
            } else {
                (high / e).clamp(0.0, 1.0) * (1.0 - ((tau - high) / e).clamp(0.0, 1.0))
            };
            return (h / p.r_on_h, (1.0 - h) / p.r_on_l);
        }
    };
    let gh = if pull == PinPull::Up { 1.0 / p.r_pull } else { 0.0 };
    let gl = if pull == PinPull::Down { 1.0 / p.r_pull } else { 0.0 };
    (gh, gl + if low { 1.0 / p.r_on_l } else { 0.0 })
}

fn crossing(w: &Watch, v0: f64, v1: f64) -> Option<f64> {
    let rising = v0 < w.level && v1 >= w.level && w.edge != PinEdge::Falling;
    let falling = v0 > w.level && v1 <= w.level && w.edge != PinEdge::Rising;
    if rising || falling { Some((w.level - v0) / (v1 - v0)) } else { None }
}

fn encode_mode(mode: PinMode) -> [f64; 4] {
    match mode {
        PinMode::OutHigh => [0.0, 0.0, 0.0, 0.0],
        PinMode::OutLow => [1.0, 0.0, 0.0, 0.0],
        PinMode::Input(pull) => [2.0, pull as u8 as f64, 0.0, 0.0],
        PinMode::OpenDrain { low, pull } => [3.0, pull as u8 as f64, low as u8 as f64, 0.0],
        PinMode::Pwm { period, high, origin } => [4.0, period, high, origin],
    }
}

fn decode_mode(s: &[f64]) -> Option<PinMode> {
    let &[tag, a, b, c] = s else { return None };
    let mode = match tag {
        0.0 => PinMode::OutHigh,
        1.0 => PinMode::OutLow,
        2.0 | 3.0 => {
            let pull = match a {
                0.0 => PinPull::None,
                1.0 => PinPull::Up,
                2.0 => PinPull::Down,
                _ => return None,
            };
            if tag == 2.0 {
                PinMode::Input(pull)
            } else if b == 0.0 || b == 1.0 {
                PinMode::OpenDrain { low: b == 1.0, pull }
            } else {
                return None;
            }
        }
        4.0 => PinMode::Pwm { period: a, high: b, origin: c },
        _ => return None,
    };
    if valid_mode(mode) { Some(mode) } else { None }
}

impl GpioBank {
    pub fn new(inst: Vec<Gpio>) -> Self {
        let runtime = vec![Runtime::default(); inst.len()];
        let slots = Vec::with_capacity(8 * inst.len());
        Self { inst, runtime, slots, watches: Vec::new(), hits: Vec::new(), res: 0.0 }
    }

    // Evaluating a candidate never commits a preload: rejected steps can go back in time.
    fn conductances(&self, i: usize, t: f64, tran: bool) -> (f64, f64) {
        let pin = &self.inst[i];
        let rt = self.runtime[i];
        if let Some((at, mode)) = rt.pending {
            if t >= at {
                return target(mode, pin.params, t);
            }
        }
        let (gh, gl) = target(pin.mode, pin.params, t);
        if tran {
            if let Some(ramp) = rt.ramp {
                let s = ((t - ramp.t) / pin.params.t_edge).clamp(0.0, 1.0);
                // each side's transistor (or pull) ramps its own conductance, as a driver's gates
                // swing: an edge between two drives reads 10-90 % in about 0.8 t_edge when
                // r_on_h == r_on_l, and a release to a pull leaves only the pull at the ramp's end
                return ((1.0 - s) * ramp.gh + s * gh, (1.0 - s) * ramp.gl + s * gl);
            }
        }
        (gh, gl)
    }

    fn settle(&mut self, i: usize, t: f64) {
        if let Some((at, mode)) = self.runtime[i].pending {
            if t >= at {
                self.inst[i].mode = mode;
                self.runtime[i].pending = None;
                self.runtime[i].ramp = None;
                self.runtime[i].memo = None;
            }
        }
    }
    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let (gh, gl) = match self.runtime[i].memo {
                Some(m) if m.t.to_bits() == cx.t.to_bits() && m.tran == cx.tran => (m.gh, m.gl),
                _ => {
                    let (gh, gl) = self.conductances(i, cx.t, cx.tran);
                    let gl = gl + 1.0 / self.inst[i].params.r_leak;
                    self.runtime[i].memo = Some(Memo { t: cx.t, tran: cx.tran, gh, gl });
                    (gh, gl)
                }
            };
            let pin = &self.inst[i];
            st.g4(&self.slots[i * 8..i * 8 + 4], gh);
            st.g4(&self.slots[i * 8 + 4..i * 8 + 8], gl);
            st.i2(pin.pad, pin.vdd, gh * (x[pin.pad as usize] - x[pin.vdd as usize]));
            st.i2(pin.pad, pin.vss, gl * (x[pin.pad as usize] - x[pin.vss as usize]));
        }
    }
}

impl Bank for GpioBank {
    fn kind(&self) -> &'static str {
        "GPIO"
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        let pin = &self.inst[i];
        out.extend_from_slice(&[pin.pad, pin.vdd, pin.vss]);
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.slots[i * 8..(i + 1) * 8]);
    }
    fn terminals(&self) -> usize {
        3
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], cx: &EvalCx, out: &mut [f64]) {
        let pin = &self.inst[inst];
        let (gh, gl) = self.conductances(inst, cx.t, cx.tran);
        let ih = gh * (x[pin.pad as usize] - x[pin.vdd as usize]);
        let il = (gl + 1.0 / pin.params.r_leak) * (x[pin.pad as usize] - x[pin.vss as usize]);
        out[..3].copy_from_slice(&[ih + il, -ih, -il]);
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        self.slots.clear();
        for pin in &self.inst {
            let p = pin.params;
            for (key, value) in [("r_on_h", p.r_on_h), ("r_on_l", p.r_on_l), ("r_pull", p.r_pull), ("r_leak", p.r_leak), ("t_edge", p.t_edge)] {
                if !value.is_finite() || value <= 0.0 {
                    return Err(format!("{}: {key} must be finite and positive", pin.name));
                }
            }
            if !valid_mode(pin.mode) {
                return Err(format!("{}: PWM requires finite period, high and origin with 0 < high < period", pin.name));
            }
            self.slots.extend_from_slice(&topo.slot4(pin.pad, pin.vdd));
            self.slots.extend_from_slice(&topo.slot4(pin.pad, pin.vss));
        }
        for rt in &mut self.runtime {
            rt.accepted = None;
            rt.memo = None;
        }
        Ok(())
    }
    fn slots(&mut self) -> &mut [u32] {
        &mut self.slots
    }
    fn eval(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps) {
        self.eval_iter(x, cx, st, 0..self.inst.len());
    }
    fn eval_some(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: &[u32]) {
        self.eval_iter(x, cx, st, insts.iter().map(|&i| i as usize));
    }
    fn iter_state(&self, out: &mut Vec<f64>) {
        for (pin, rt) in self.inst.iter().zip(&self.runtime) {
            out.extend_from_slice(&encode_mode(pin.mode));
            if let Some(ramp) = rt.ramp {
                out.extend_from_slice(&[1.0, ramp.t, ramp.gh, ramp.gl]);
            } else {
                out.extend_from_slice(&[0.0; 4]);
            }
            if let Some((at, mode)) = rt.pending {
                out.extend_from_slice(&[1.0, at]);
                out.extend_from_slice(&encode_mode(mode));
            } else {
                out.extend_from_slice(&[0.0; 6]);
            }
        }
    }
    // PWM, ramps and pending writes all have breaks, so the default `varies_after` applies.
    fn next_break(&self, t: f64) -> f64 {
        let mut next = f64::INFINITY;
        for (pin, rt) in self.inst.iter().zip(&self.runtime) {
            if let Some((at, mode)) = rt.pending {
                if !after(at, t) {
                    next = next.min(pwm_break(mode, pin.params.t_edge, self.res, t));
                    continue;
                }
                next = next.min(at);
            }
            if let Some(ramp) = rt.ramp {
                let end = ramp.t + pin.params.t_edge;
                if pin.params.t_edge > self.res && after(end, t) {
                    next = next.min(end);
                }
            }
            next = next.min(pwm_break(pin.mode, pin.params.t_edge, self.res, t));
        }
        next
    }
    fn set_res(&mut self, res: f64) {
        self.res = res;
    }
    fn hidden_break(&self, t: f64) -> f64 {
        if !(self.res > 0.0) {
            return f64::INFINITY;
        }
        let mut next = f64::INFINITY;
        for (pin, rt) in self.inst.iter().zip(&self.runtime) {
            if let Some((at, mode)) = rt.pending {
                if !after(at, t) {
                    next = next.min(pwm_hidden(mode, pin.params.t_edge, self.res, t));
                    continue;
                }
            }
            if let Some(ramp) = rt.ramp {
                let end = ramp.t + pin.params.t_edge;
                if pin.params.t_edge <= self.res && after(end, t) {
                    next = next.min(end);
                }
            }
            next = next.min(pwm_hidden(pin.mode, pin.params.t_edge, self.res, t));
        }
        next
    }
    fn watch_moved(&self, x0: &[f64], x1: &[f64]) -> bool {
        self.watches.iter().any(|w| {
            let pin = &self.inst[w.inst];
            let v0 = x0[pin.pad as usize] - x0[pin.vss as usize];
            let v1 = x1[pin.pad as usize] - x1[pin.vss as usize];
            (v1 - v0).abs() > 1e-3 * w.level.abs().max(1e-3)
        })
    }
    fn crossing(&self, x0: &[f64], x1: &[f64]) -> Option<f64> {
        let mut first: Option<f64> = None;
        for w in &self.watches {
            let pin = &self.inst[w.inst];
            let v0 = x0[pin.pad as usize] - x0[pin.vss as usize];
            let v1 = x1[pin.pad as usize] - x1[pin.vss as usize];
            if let Some(f) = crossing(w, v0, v1) {
                first = Some(first.map_or(f, |old| old.min(f)));
            }
        }
        first
    }
    fn accept(&mut self, x: &[f64], cx: &EvalCx) {
        let start = self.hits.len();
        for w in &self.watches {
            let pin = &self.inst[w.inst];
            let v = x[pin.pad as usize] - x[pin.vss as usize];
            if let Some((t0, v0)) = self.runtime[w.inst].accepted {
                if let Some(f) = crossing(w, v0, v) {
                    self.hits.push((w.id, t0 + f * (cx.t - t0)));
                }
            }
        }
        // Each batch lies after earlier accepted batches; sort only this step's hits in place.
        self.hits[start..].sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
        for i in 0..self.inst.len() {
            let pin = &self.inst[i];
            self.runtime[i].accepted = Some((cx.t, x[pin.pad as usize] - x[pin.vss as usize]));
            self.settle(i, cx.t);
        }
    }
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        if !v.is_finite() || v <= 0.0 {
            return false;
        }
        let Some(pin) = self.inst.get_mut(inst) else { return false };
        self.runtime[inst].memo = None;
        match key {
            "r_on_h" => pin.params.r_on_h = v,
            "r_on_l" => pin.params.r_on_l = v,
            "r_pull" => pin.params.r_pull = v,
            "r_leak" => pin.params.r_leak = v,
            "t_edge" => pin.params.t_edge = v,
            _ => return false,
        }
        true
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        let p = &self.inst.get(inst)?.params;
        Some(match key {
            "r_on_h" => p.r_on_h,
            "r_on_l" => p.r_on_l,
            "r_pull" => p.r_pull,
            "r_leak" => p.r_leak,
            "t_edge" => p.t_edge,
            _ => return None,
        })
    }
    fn pin_write(&mut self, inst: usize, t: f64, mode: PinMode) -> Option<Retuned> {
        if inst >= self.inst.len() || !t.is_finite() || !valid_mode(mode) {
            return None;
        }
        self.settle(inst, t);
        self.runtime[inst].memo = None;
        if let (PinMode::Pwm { period, origin, .. }, PinMode::Pwm { period: new_period, high, .. }) = (self.inst[inst].mode, mode) {
            let at = self.runtime[inst].pending.map_or_else(|| period_start(t, origin, period), |pending| pending.0);
            self.runtime[inst].pending = Some((at, PinMode::Pwm { period: new_period, high, origin: at }));
            return Some(Retuned::At(at));
        }
        let (gh, gl) = self.conductances(inst, t, true);
        self.inst[inst].mode = mode;
        self.runtime[inst].pending = None;
        self.runtime[inst].ramp = Some(Ramp { t, gh, gl });
        Some(Retuned::Now)
    }
    fn pin_mode(&self, inst: usize) -> Option<PinMode> {
        self.inst.get(inst).map(|g| g.mode)
    }
    fn pin_watch(&mut self, inst: usize, id: u32, level: f64, edge: PinEdge) -> bool {
        if inst >= self.inst.len() || !level.is_finite() {
            return false;
        }
        self.watches.push(Watch { inst, id, level, edge });
        true
    }
    fn clear_watches(&mut self) {
        self.watches.clear();
    }
    fn pin_hits(&mut self, out: &mut Vec<(u32, f64)>) {
        out.extend(self.hits.drain(..));
    }
    fn state(&self, inst: usize) -> Vec<f64> {
        // Four mode values, four ramp values and six preload values.
        let rt = self.runtime[inst];
        let mut s = [0.0; 14];
        s[..4].copy_from_slice(&encode_mode(self.inst[inst].mode));
        if let Some(ramp) = rt.ramp {
            s[4..8].copy_from_slice(&[1.0, ramp.t, ramp.gh, ramp.gl]);
        }
        if let Some((at, mode)) = rt.pending {
            s[8] = 1.0;
            s[9] = at;
            s[10..14].copy_from_slice(&encode_mode(mode));
        }
        s.to_vec()
    }
    fn set_state(&mut self, inst: usize, s: &[f64]) {
        if inst >= self.inst.len() || s.len() != 14 {
            return;
        }
        let Some(mode) = decode_mode(&s[..4]) else { return };
        let ramp = match s[4] {
            0.0 => None,
            1.0 if s[5..8].iter().all(|v| v.is_finite()) && s[6] >= 0.0 && s[7] >= 0.0 => Some(Ramp { t: s[5], gh: s[6], gl: s[7] }),
            _ => return,
        };
        let pending = match s[8] {
            0.0 => None,
            1.0 if s[9].is_finite() => {
                let Some(new @ PinMode::Pwm { origin, .. }) = decode_mode(&s[10..14]) else { return };
                if origin != s[9] || !matches!(mode, PinMode::Pwm { .. }) {
                    return;
                }
                Some((s[9], new))
            }
            _ => return,
        };
        self.inst[inst].mode = mode;
        self.runtime[inst] = Runtime { ramp, pending, accepted: None, memo: None };
    }
}

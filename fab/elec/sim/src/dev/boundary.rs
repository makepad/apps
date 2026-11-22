//! Sources at block boundaries, reading an exporter's accepted points one round at a time.

use crate::circuit::{Boundary, BoundaryKind, GND};
use crate::stamp::{Bank, EvalCx, Stamps, Tier, Topology, Unk};
use std::sync::Arc;

/// Accepted points: `t` is strictly increasing, `v` has the same length, and `events` holds
/// ascending indices into `t`. Ordinary points interpolate; only events are breakpoints.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub t: Vec<f64>,
    pub v: Vec<f64>,
    pub events: Vec<u32>,
    /// The exporter's own crossings, an ascending subset of `events`.
    pub edges: Vec<u32>,
}

impl Table {
    /// Linear interpolation, holding the endpoint values outside the table (zero if empty).
    pub fn value(&self, t: f64) -> f64 {
        if self.t.is_empty() {
            return 0.0;
        }
        if t <= self.t[0] {
            return self.v[0];
        }
        let i = self.t.partition_point(|&at| at < t);
        if i == self.t.len() {
            self.v[i - 1]
        } else {
            self.v[i - 1] + (self.v[i] - self.v[i - 1]) * (t - self.t[i - 1]) / (self.t[i] - self.t[i - 1])
        }
    }

    /// `value` at `t`, searching on from `*at` (the index the last call for an earlier time left):
    /// times read in increasing order cost O(1) each, amortised. A later call for an earlier time
    /// searches from the start. Moves `*at` to the index it found.
    pub fn value_from(&self, t: f64, at: &mut usize) -> f64 {
        if self.t.is_empty() {
            return 0.0;
        }
        if t <= self.t[0] {
            *at = 0;
            return self.v[0];
        }
        // the first index whose time is at least `t` (`value`'s partition point), from `*at` when
        // the time before it is still under `t`
        let mut i = if *at >= 1 && *at <= self.t.len() && self.t[*at - 1] < t { *at } else { 1 };
        while i < self.t.len() && self.t[i] < t {
            i += 1;
        }
        *at = i;
        if i == self.t.len() {
            self.v[i - 1]
        } else {
            self.v[i - 1] + (self.v[i] - self.v[i - 1]) * (t - self.t[i - 1]) / (self.t[i] - self.t[i - 1])
        }
    }

    /// The first event time strictly after `t`, or infinity when no event remains.
    pub fn next_event(&self, t: f64) -> f64 {
        let i = self.events.partition_point(|&i| self.t[i as usize] <= t);
        self.events.get(i).map_or(f64::INFINITY, |&i| self.t[i as usize])
    }

    /// The integral over `[a, b]`, the table held at its ends outside it.
    fn integral(&self, a: f64, b: f64) -> f64 {
        let n = self.t.len();
        if n == 0 || !(b > a) {
            return 0.0;
        }
        let mut sum = 0.0;
        if a < self.t[0] {
            sum += (b.min(self.t[0]) - a) * self.v[0];
        }
        if b > self.t[n - 1] {
            sum += (b - a.max(self.t[n - 1])) * self.v[n - 1];
        }
        for i in self.t.partition_point(|&t| t <= a).saturating_sub(1)..n - 1 {
            let (ta, tb) = (self.t[i], self.t[i + 1]);
            if ta >= b {
                break;
            }
            let (lo, hi) = (ta.max(a), tb.min(b));
            if hi > lo {
                // shares of the segment, not a slope: a jump's segment is one float wide
                let at = |t: f64| self.v[i] + (self.v[i + 1] - self.v[i]) * ((t - ta) / (tb - ta));
                sum += 0.5 * (at(lo) + at(hi)) * (hi - lo);
            }
        }
        sum
    }

    /// The table as a reader stepping on the grid of period `h` takes it under a time resolution,
    /// reading it `delay` late, in a round from `t0` to `end` (the reader's times). A jump the
    /// exporter's solver made (one of its crossings, `edges`, or a value that changes from a point
    /// to the next float, as after a landed corner) is no instant for the reader to land on: it
    /// reaches the reader as the line through the grid points' means over their own sample. A jump
    /// at `t` counts `1/2 + (x - t) / h` of itself at the grid point `x` within half a sample of it,
    /// so the reader sees a ramp a sample wide centred on the jump, which keeps its area and its
    /// time (A-K1). Between the grid points next to a jump's the table is that line; elsewhere it is
    /// as it was.
    ///
    /// The exporter is final through reader time `known >= end`. The round takes jumps through
    /// `known` and holds the table from there for its means. At a grid-aligned `t0`, `taken`
    /// supplies the value the reader already took when a zone starts there or includes that knot.
    /// Without it the value is reconstructed from the table. When `t0` is among a zone's mean
    /// knots, the next knot carries the difference between the mean now known and the value used
    /// at `t0`. A jump with another event (a kink) among the points its line replaces stays as
    /// it was. Returns None when nothing is split, else the table and the jumps split.
    pub fn split(&self, h: f64, delay: f64, t0: f64, taken: Option<f64>, end: f64, known: f64) -> Option<(Table, u32)> {
        let n = self.t.len();
        if !(h > 0.0) || n < 2 {
            return None;
        }
        // A jump at the round's start (an exporter's restart there, as at a plan's start) is past
        // splitting: the reader took the value before it, and takes the one after from its next
        // step, as at a landing.
        if let Some(i) = (0..n - 1).find(|&i| (self.t[i] + delay - t0).abs() <= 1e-9 * h && self.t[i + 1] <= self.t[i].next_up() && self.v[i] != self.v[i + 1]) {
            let mut after = self.clone();
            after.v[i] = after.v[i + 1];
            return after.split(h, delay, t0, taken, end, known).or(Some((after, 0)));
        }
        let jump = |i: usize| self.edges.binary_search(&(i as u32)).is_ok() || (i + 1 < n && self.t[i + 1] <= self.t[i].next_up());
        let jumps: Vec<f64> = (0..n).filter(|&i| jump(i)).map(|i| self.t[i] + delay).filter(|&at| at <= known).collect();
        if jumps.is_empty() {
            return None;
        }
        // grid indices whose sample meets a jump's first half sample, merged where they touch
        let merge = |zones: &mut Vec<(i64, i64, u32)>| {
            zones.sort_unstable_by_key(|z| z.0);
            let mut out: Vec<(i64, i64, u32)> = Vec::with_capacity(zones.len());
            for &z in zones.iter() {
                match out.last_mut() {
                    Some(o) if z.0 <= o.1 + 1 => {
                        o.1 = o.1.max(z.1);
                        o.2 += z.2;
                    }
                    _ => out.push(z),
                }
            }
            *zones = out;
        };
        let mut zones: Vec<(i64, i64, u32)> = jumps.iter().map(|&at| {
            let q = at / h;
            ((q - 0.5).floor() as i64 + 1, (q + 1.0).ceil() as i64 - 1, 1)
        }).collect();
        merge(&mut zones);
        let k0 = (t0 / h).round();
        let k0 = ((t0 / h - k0).abs() < 1e-6).then_some(k0 as i64);
        // the grid point after the round's start takes its carry: a knot of the line
        if let Some(k0) = k0 {
            if let Some(z) = zones.iter_mut().find(|z| z.0 <= k0 && k0 <= z.1) {
                z.1 = z.1.max(k0 + 1);
                merge(&mut zones);
            }
        }
        let grid = |k: i64| k as f64 * h - delay;
        let event = |i: usize| self.events.binary_search(&(i as u32)).is_ok();
        zones.retain(|&(lo, hi, _)| {
            let (a, b) = (grid(lo - 1), grid(hi + 1));
            let from = self.t.partition_point(|&t| t <= a);
            (from..n).take_while(|&i| self.t[i] < b).all(|i| !event(i) || jump(i) || (i > 0 && jump(i - 1) && self.t[i] <= self.t[i - 1].next_up()))
        });
        if zones.is_empty() {
            return None;
        }
        // the mean over the sample at `x` of the table held from `c` (reader times)
        let mean = |x: f64, c: f64| {
            let (a, b, c) = (x - 0.5 * h - delay, x + 0.5 * h - delay, c - delay);
            if c >= b {
                self.integral(a, b) / h
            } else if c <= a {
                self.value(c)
            } else {
                (self.integral(a, c) + (b - c) * self.value(c)) / h
            }
        };
        // Without a supplied read, reconstruct it from the table held at the round's start.
        let start = k0.filter(|&k0| zones.iter().any(|z| z.0 <= k0 && k0 <= z.1)).map(|k0| {
            let used = taken.unwrap_or_else(|| if jumps.iter().any(|&at| at > t0 - h && at <= t0) { mean(t0, t0) } else { self.value(t0 - delay) });
            (k0, used, mean(t0, known) - used)
        });
        let mut out = Table { t: Vec::with_capacity(n + 4 * zones.len()), v: Vec::with_capacity(n + 4 * zones.len()), events: Vec::new(), edges: Vec::new() };
        let keep = |out: &mut Table, i: usize| {
            let at = out.t.len() as u32;
            if event(i) {
                out.events.push(at);
            }
            if self.edges.binary_search(&(i as u32)).is_ok() {
                out.edges.push(at);
            }
            out.t.push(self.t[i]);
            out.v.push(self.v[i]);
        };
        let knot = |out: &mut Table, t: f64, v: f64| {
            if out.t.last().is_none_or(|&last| t > last) {
                out.t.push(t);
                out.v.push(v);
            }
        };
        let (mut i, mut split) = (0, 0);
        for &(lo, hi, count) in &zones {
            let (a, b) = (grid(lo - 1), grid(hi + 1));
            while i < n && self.t[i] <= a {
                keep(&mut out, i);
                i += 1;
            }
            knot(&mut out, a, self.value(a));
            if let Some(v) = taken.filter(|_| k0 == Some(lo - 1)) {
                // The left knot may already have been copied from the exporter.
                *out.v.last_mut().unwrap() = v;
            }
            for k in lo..=hi {
                let x = k as f64 * h;
                let v = match start {
                    Some((k0, used, _)) if k == k0 => used,
                    Some((k0, _, carry)) if k == k0 + 1 => mean(x, known) + carry,
                    _ => mean(x, if x <= t0 { t0 } else { known }),
                };
                knot(&mut out, grid(k), v);
            }
            while i < n && self.t[i] < b {
                i += 1;
            }
            if !(i < n && self.t[i] == b) {
                knot(&mut out, b, self.value(b));
            }
            split += count;
        }
        while i < n {
            keep(&mut out, i);
            i += 1;
        }
        Some((out, split))
    }
}

/// A voltage boundary's holder port (`BoundaryKind::Port`): the port's instance, its memory's
/// unknown `w`, and its impedance `r + rb / (1 + s tau) + s l`. With `e` the branch current less
/// the port's value, the branch row reads `v(node) - value - r e - w - l de/dt = 0` and the memory's
/// `tau dw/dt + w - rb e = 0`: all zero, the boundary holds its value and `w` stays at zero.
#[derive(Clone, Copy, Debug)]
pub struct Port {
    pub own: u32,
    pub w: Unk,
    pub r: f64,
    pub rb: f64,
    pub tau: f64,
    pub l: f64,
}

/// Terminals are node, ground for every kind. A current boundary injects into the node, so
/// its node terminal current is negative; a voltage boundary's branch flows into the source; a
/// port carries none.
#[derive(Clone)]
pub struct BoundaryBank {
    pub inst: Vec<Boundary>,
    /// Zero for a current boundary, which has no branch unknown.
    br: Vec<Unk>,
    /// Per voltage boundary, its port, if it has one.
    ports: Vec<Option<Port>>,
    /// Eight slots per instance: a voltage boundary's four, then its port's (branch, branch),
    /// (branch, w), (w, w), (w, branch); unused slots are the trash cell.
    slots: Vec<u32>,
    tables: Vec<Option<(Arc<Table>, f64)>>,
}

const SLOTS: usize = 8;

impl BoundaryBank {
    pub fn new(inst: Vec<Boundary>) -> Self {
        let tables = vec![None; inst.len()];
        BoundaryBank { inst, br: Vec::new(), ports: Vec::new(), slots: Vec::new(), tables }
    }

    fn value(&self, i: usize, cx: &EvalCx) -> f64 {
        let t = if cx.tran { cx.t } else { 0.0 };
        self.tables[i].as_ref().map_or(self.inst[i].v0, |(table, delay)| table.value(t - delay)) * cx.src_scale
    }

    /// The port of voltage boundary `i`, if it has one. O(1).
    pub fn port(&self, i: usize) -> Option<Port> {
        self.ports.get(i).copied().flatten()
    }

    /// The tables that make breakpoints: a port's is its own branch current as the holder counts
    /// it, which moves where the node it stands beside does, and lands nothing of its own.
    fn breaking(&self) -> impl Iterator<Item = &(Arc<Table>, f64)> {
        self.tables.iter().zip(&self.inst).filter(|(_, b)| !matches!(b.kind, BoundaryKind::Port { .. })).filter_map(|(t, _)| t.as_ref())
    }

    fn eval_iter(&mut self, x: &[f64], cx: &EvalCx, st: &mut Stamps, insts: impl Iterator<Item = usize>) {
        for i in insts {
            let b = &self.inst[i];
            match b.kind {
                BoundaryKind::Voltage => {
                    let value = self.value(i, cx);
                    let s = &self.slots[i * SLOTS..(i + 1) * SLOTS];
                    let br = self.br[i] as usize;
                    st.g[s[0] as usize] += 1.0;
                    st.g[s[1] as usize] -= 1.0;
                    st.g[s[2] as usize] += 1.0;
                    st.g[s[3] as usize] -= 1.0;
                    st.i2(b.node, GND, x[br]);
                    st.f[br] += x[b.node as usize] - x[GND as usize] - value;
                    if let Some(p) = self.ports[i] {
                        let (w, e) = (p.w as usize, x[br] - self.value(p.own as usize, cx));
                        st.f[br] -= p.r * e + x[w];
                        st.g[s[4] as usize] -= p.r;
                        st.g[s[5] as usize] -= 1.0;
                        st.q[br] -= p.l * e;
                        st.c[s[4] as usize] -= p.l;
                        st.f[w] += x[w] - p.rb * e;
                        st.g[s[6] as usize] += 1.0;
                        st.g[s[7] as usize] -= p.rb;
                        st.q[w] += p.tau * x[w];
                        st.c[s[6] as usize] += p.tau;
                    }
                }
                BoundaryKind::Current => st.i2(GND, b.node, self.value(i, cx)),
                BoundaryKind::Port { .. } => {}
            }
        }
    }
}

impl Bank for BoundaryBank {
    fn terminals(&self) -> usize {
        2
    }
    fn terminal_currents(&self, inst: usize, x: &[f64], _xdot: &[f64], cx: &EvalCx, out: &mut [f64]) {
        let i = match self.inst[inst].kind {
            BoundaryKind::Voltage => x[self.br[inst] as usize],
            BoundaryKind::Current => -self.value(inst, cx),
            BoundaryKind::Port { .. } => 0.0,
        };
        out[..2].copy_from_slice(&[i, -i]);
    }
    fn kind(&self) -> &'static str {
        "boundary"
    }
    fn tier(&self, _i: usize) -> Tier {
        // A first feed can arrive after the tier plan was built, without a value edit.
        Tier::Timed
    }
    fn len(&self) -> usize {
        self.inst.len()
    }
    fn inst_name(&self, i: usize) -> &str {
        &self.inst[i].name
    }
    fn inst_unks(&self, i: usize, out: &mut Vec<Unk>) {
        out.extend_from_slice(&[self.inst[i].node, GND]);
        if self.inst[i].kind == BoundaryKind::Voltage {
            out.push(self.br[i]);
            if let Some(p) = self.ports[i] {
                out.push(p.w);
            }
        }
    }
    fn inst_slots(&self, i: usize, out: &mut Vec<u32>) {
        if self.inst[i].kind == BoundaryKind::Voltage {
            let n = if self.ports[i].is_some() { SLOTS } else { 4 };
            out.extend_from_slice(&self.slots[i * SLOTS..i * SLOTS + n]);
        }
    }
    fn setup(&mut self, topo: &mut Topology) -> Result<(), String> {
        self.ports = vec![None; self.inst.len()];
        for (own, b) in self.inst.iter().enumerate() {
            if let BoundaryKind::Port { of } = b.kind {
                let ok = self.inst.get(of as usize).is_some_and(|v| v.kind == BoundaryKind::Voltage);
                if !ok || self.ports[of as usize].is_some() {
                    return Err(format!("{}: a port belongs to one voltage boundary of its own", b.name));
                }
                let w = topo.internal(&b.name, "w");
                self.ports[of as usize] = Some(Port { own: own as u32, w, r: 0.0, rb: 0.0, tau: 0.0, l: 0.0 });
            }
        }
        for (i, b) in self.inst.iter().enumerate() {
            if b.kind == BoundaryKind::Voltage {
                let br = topo.branch(&b.name);
                self.br.push(br);
                self.slots.extend_from_slice(&[topo.slot(b.node, br), topo.slot(GND, br), topo.slot(br, b.node), topo.slot(br, GND)]);
                match self.ports[i] {
                    Some(p) => self.slots.extend_from_slice(&[topo.slot(br, br), topo.slot(br, p.w), topo.slot(p.w, p.w), topo.slot(p.w, br)]),
                    None => self.slots.extend_from_slice(&[0; 4]),
                }
            } else {
                self.br.push(0);
                self.slots.extend_from_slice(&[0; SLOTS]);
            }
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
    fn next_break(&self, t: f64) -> f64 {
        self.breaking().map(|(table, delay)| {
            // Compare shifted times directly: subtracting then adding the delay can round an
            // event back onto `t`, which is not a breakpoint strictly after it.
            let i = table.events.partition_point(|&i| table.t[i as usize] + delay <= t);
            table.events.get(i).map_or(f64::INFINITY, |&i| table.t[i as usize] + delay)
        }).fold(f64::INFINITY, f64::min)
    }
    fn edge_break_at(&self, t: f64) -> bool {
        self.breaking().any(|(table, delay)| table.edges.iter().any(|&i| table.t[i as usize] + delay == t))
    }
    fn plain_break_at(&self, t: f64) -> bool {
        self.breaking().any(|(table, delay)| {
            table.events.iter().any(|&i| table.t[i as usize] + delay == t && !table.edges.contains(&i))
        })
    }
    fn jump_step(&self, t: f64) -> f64 {
        self.breaking().map(|(table, delay)| {
            // the jump's two points at `t`, then the exporter's next
            let i = table.t.partition_point(|&at| at + delay < t);
            let jumps = i + 2 < table.t.len() && table.t[i] + delay == t && table.t[i + 1] <= table.t[i].next_up() && table.v[i] != table.v[i + 1];
            if jumps { table.t[i + 2] + delay - t } else { f64::INFINITY }
        }).fold(f64::INFINITY, f64::min)
    }
    fn varies_after(&self, t: f64) -> bool {
        // Interpolation still varies after the last event, up to the last accepted point.
        self.tables.iter().flatten().any(|(table, delay)| table.t.last().is_some_and(|&last| last + delay > t))
    }
    fn feed(&mut self, inst: usize, table: Arc<Table>, delay: f64) -> bool {
        self.tables[inst] = Some((table, delay));
        true
    }
    /// A port's impedance (`r`, `rb`, `tau`, `l`) and its value before a table (`v0`), on the
    /// port's own instance.
    fn set_param(&mut self, inst: usize, key: &str, v: f64) -> bool {
        let BoundaryKind::Port { of } = self.inst[inst].kind else { return false };
        if key == "v0" {
            self.inst[inst].v0 = v;
            return true;
        }
        let Some(p) = self.ports.get_mut(of as usize).and_then(Option::as_mut) else { return false };
        match key {
            "r" => p.r = v,
            "rb" => p.rb = v,
            "tau" => p.tau = v,
            "l" => p.l = v,
            _ => return false,
        }
        true
    }
    fn param(&self, inst: usize, key: &str) -> Option<f64> {
        let BoundaryKind::Port { of } = self.inst[inst].kind else { return None };
        if key == "v0" {
            return Some(self.inst[inst].v0);
        }
        let p = self.port(of as usize)?;
        match key {
            "r" => Some(p.r),
            "rb" => Some(p.rb),
            "tau" => Some(p.tau),
            "l" => Some(p.l),
            _ => None,
        }
    }
}

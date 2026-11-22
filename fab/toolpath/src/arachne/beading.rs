//! Distributed, outer, widening and limited bead strategies.

use super::Config;

#[derive(Clone, Debug, Default)]
pub struct Beading {
    pub widths: Vec<f64>,
    pub locations: Vec<f64>,
}

pub struct Strategy {
    pub(crate) cap: usize,
    outer: f64,
    inner: f64,
    min_width: f64,
    min_feature: f64,
    split: f64,
    add: f64,
    distribution: usize,
    transitions: Vec<f64>,
}

impl Strategy {
    /// Cache every count threshold once for an island.
    pub fn new(cfg: &Config) -> Option<Self> {
        if cfg.loops == 0 { return None; }
        let cap = cfg.loops.checked_mul(2)?;
        let mut s = Self { cap, outer: cfg.s_outer, inner: cfg.s_inner,
            min_width: if cfg.widen { cfg.min_bead_width } else { 0.0 }, min_feature: cfg.min_feature_size,
            split: (2.0 * cfg.min_bead_width / cfg.w_outer - 1.0).clamp(0.01, 0.99),
            add: (cfg.min_bead_width / cfg.w_inner).clamp(0.01, 0.99),
            distribution: cfg.distribution_count, transitions: Vec::new() };
        s.transitions.try_reserve(cap + 1).ok()?;
        for n in 0..=cap {
            let t = if n == 0 { s.min_feature }
                else if n == cap { s.nominal(n + 1) - 0.01 }
                else if n == 1 { (1.0 + s.split) * s.outer }
                else { 2.0 * s.outer + (n as f64 - 2.0 + s.threshold(n)) * s.inner };
            if !t.is_finite() || t <= 0.0 || s.transitions.last().is_some_and(|v| *v >= t) { return None; }
            s.transitions.push(t);
        }
        Some(s)
    }

    fn threshold(&self, n: usize) -> f64 { if n % 2 == 0 { self.add } else { self.split } }
    fn nominal(&self, n: usize) -> f64 {
        match n { 0 => 0.0, 1 => self.outer, _ => 2.0 * self.outer + (n - 2) as f64 * self.inner }
    }

    pub fn optimal_count(&self, t: f64) -> usize {
        if !t.is_finite() || t < 0.0 { return 0; }
        if t < self.inner { return usize::from(t >= self.min_feature); }
        if t < 0.5 * self.outer { return 0; }
        if t <= 2.0 * self.outer { return if t > self.transitions[1] { 2 } else { 1 }; }
        if t >= self.transitions[self.cap] { return self.cap + 1; }
        let q = (t - 2.0 * self.outer) / self.inner;
        let n = (q.floor() as usize).min(self.cap);
        (2 + n + usize::from(q - n as f64 >= self.threshold(n))).min(self.cap)
    }

    pub fn transition_thickness(&self, n: usize) -> f64 {
        self.transitions.get(n).copied().unwrap_or(f64::INFINITY)
    }

    pub fn transition_anchor(&self, n: usize) -> f64 { 1.0 - self.threshold(n) }

    fn distributed(&self, t: f64, n: usize, widths: &mut Vec<f64>) {
        if n == 0 { return; }
        if n <= 2 { widths.extend(std::iter::repeat(t / n as f64).take(n)); return; }
        let first = widths.len();
        let m = (n - 1) as f64 * 0.5;
        let k = self.distribution.saturating_sub(1).max(1) as f64;
        let weight = |i: usize| (1.0 - ((i as f64 - m) / k).powi(2)).max(0.0);
        let sum: f64 = (0..n).map(weight).sum();
        let surplus = t - n as f64 * self.inner;
        for i in 0..n { widths.push((self.inner + surplus * weight(i) / sum).max(0.0)); }
        let actual: f64 = widths[first..].iter().sum();
        // Propagated layouts can be forced below their feasible thickness.
        if actual > t + 1e-12 {
            for w in &mut widths[first..] { *w *= t / actual; }
        } else if let Some(w) = widths.last_mut() { *w += t - actual; }
    }

    fn redistribute(&self, t: f64, n: usize, widths: &mut Vec<f64>) {
        match n {
            0 => (),
            1 => widths.push(t),
            2 => widths.extend_from_slice(&[t * 0.5, t * 0.5]),
            _ => {
                let outer = (t * 0.5).min(self.outer);
                widths.push(outer);
                self.distributed((t - 2.0 * outer).max(0.0), n - 2, widths);
                widths.push(outer);
            }
        }
    }

    pub(crate) fn compute_into(&self, t: f64, n: usize, b: &mut Beading) {
        b.widths.clear(); b.locations.clear();
        if !t.is_finite() || t <= 0.0 || n == 0 { return; }
        let n = n.min(self.cap + 1);
        if n > self.cap {
            self.redistribute(self.nominal(self.cap), self.cap, &mut b.widths);
            let half = self.cap / 2;
            let side: f64 = b.widths[..half].iter().sum();
            b.widths.insert(half, 0.0); b.widths.insert(half + 1, 0.0);
            let mut x = 0.0;
            for i in 0..b.widths.len() {
                if i == half { b.locations.push(side); }
                else if i == half + 1 { b.locations.push(t - side); x = t - side; }
                else { let w = b.widths[i]; b.locations.push(x + w * 0.5); x += w; }
            }
        } else {
            self.redistribute(t, n, &mut b.widths);
            if n == 1 && t < self.inner {
                b.widths[0] = if t >= self.min_feature { t.max(self.min_width) } else { 0.0 };
            }
            let mut x = 0.0;
            for &w in &b.widths { b.locations.push(x + w * 0.5); x += w; }
            if n == 1 { b.locations[0] = t * 0.5; }
            if n == self.cap { b.widths.insert(n / 2, 0.0); b.locations.insert(n / 2, t * 0.5); }
        }
    }
}

//! Rational oscillators projected directly onto a machine's tick grid.

/// An oscillator off the tick grid: frequency `num/den` Hz, with edge `n >= 0`
/// at `start + ceil(n * tick_hz * den / num)`. Unrepresentable results panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock { pub num: u64, pub den: u64, pub start: u64, pub tick_hz: u64 }

impl Clock {
    /// Creates a clock; panics on a zero numerator, denominator or tick frequency.
    pub fn new(num: u64, den: u64, start: u64, tick_hz: u64) -> Clock {
        assert!(num != 0 && den != 0 && tick_hz != 0, "clock rates must be nonzero");
        Clock { num, den, start, tick_hz }
    }

    fn period(&self) -> u128 { u128::from(self.tick_hz) * u128::from(self.den) }

    /// The tick of edge `n`, calculated without accumulated rounding error.
    pub fn edge(&self, n: u64) -> u64 {
        // The same ceiling in u64 when the product fits: integer division rounds alike.
        if let Some(product) = self.tick_hz.checked_mul(self.den).and_then(|period| n.checked_mul(period)) {
            let offset = product / self.num + u64::from(product % self.num != 0);
            return self.start.checked_add(offset).expect("clock edge overflow");
        }
        let product = u128::from(n).checked_mul(self.period()).expect("clock edge overflow");
        let num = u128::from(self.num);
        let offset = product / num + u128::from(product % num != 0);
        self.start.checked_add(u64::try_from(offset).expect("clock edge overflow"))
            .expect("clock edge overflow")
    }

    /// Counts edges at or before `tick`, including edge zero at `start`.
    pub fn edges_through(&self, tick: u64) -> u64 {
        if tick < self.start { return 0; }
        // The same quotient in u64 when every term fits: integer division rounds alike.
        if let (Some(scaled), Some(period)) = ((tick - self.start).checked_mul(self.num), self.tick_hz.checked_mul(self.den)) {
            if let Some(count) = (scaled / period).checked_add(1) { return count; }
        }
        let count = u128::from(tick - self.start) * u128::from(self.num) / self.period() + 1;
        u64::try_from(count).expect("clock edge count overflow")
    }

    /// Counts edges in `[from, to)`; reversed and empty intervals contain none.
    pub fn edges_in(&self, from: u64, to: u64) -> u64 {
        if from >= to { return 0; }
        let before = |tick: u64| tick.checked_sub(1).map_or(0, |t| self.edges_through(t));
        before(to) - before(from)
    }

    /// The first edge at or after `tick`, as `(edge index, edge tick)`.
    pub fn next_edge(&self, tick: u64) -> (u64, u64) {
        let n = tick.checked_sub(1).map_or(0, |t| self.edges_through(t));
        (n, self.edge(n))
    }
}

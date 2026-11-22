//! An event histogram on a canonical power-of-two grid, independent of arrival order.

#[derive(Clone, Debug, PartialEq)]
pub struct Distribution {
    pub k: i32,
    /// Global bin index of the minimum, not its value in measurement units.
    pub start: f64,
    pub counts: [u64; 64],
    pub total: u64,
    pub nonfinite: u64,
}

impl Default for Distribution {
    fn default() -> Self { Self { k: -40, start: 0.0, counts: [0; 64], total: 0, nonfinite: 0 } }
}

impl Distribution {
    pub fn width(&self) -> f64 { 2.0_f64.powi(self.k) }
    pub fn lo(&self) -> f64 { self.start * self.width() }
    pub fn bin_range(&self, i: usize) -> (f64, f64) {
        let lo = (self.start + i as f64) * self.width();
        (lo, lo + self.width())
    }

    pub fn push(&mut self, value: f32) {
        if !value.is_finite() { self.nonfinite += 1; return; }
        let value = f64::from(value);
        let mut bin = (value * 2.0_f64.powi(-self.k)).floor();
        if self.total == 0 { self.start = bin; }
        while !self.fits(bin) {
            self.double();
            bin = (value * 2.0_f64.powi(-self.k)).floor();
        }
        if bin < self.start {
            let shift = (self.start - bin) as usize;
            self.counts.copy_within(..64 - shift, shift);
            self.counts[..shift].fill(0);
            self.start = bin;
        }
        self.counts[(bin - self.start) as usize] += 1;
        self.total += 1;
    }

    fn fits(&self, bin: f64) -> bool {
        let offset = bin - self.start;
        if offset >= 0.0 { return offset < 64.0; }
        // Only occupied bins constrain a lower minimum. Including the empty tail here
        // would make the final level depend on the order in which values arrived.
        let shift = -offset;
        shift < 64.0 && self.counts[64 - shift as usize..].iter().all(|&count| count == 0)
    }

    fn double(&mut self) {
        let start = (self.start * 0.5).floor();
        let mut counts = [0; 64];
        for (i, &count) in self.counts.iter().enumerate() {
            if count == 0 { continue; }
            let bin = ((self.start + i as f64) * 0.5).floor();
            counts[(bin - start) as usize] += count;
        }
        self.k += 1;
        self.start = start;
        self.counts = counts;
    }
}

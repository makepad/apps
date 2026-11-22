/// The first, last and extreme samples in one non-empty pixel column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    /// First sample's fractional position, measured in columns.
    pub x0: f32,
    /// Last sample's fractional position, measured in columns.
    pub x1: f32,
    /// First sample's value.
    pub y0: f32,
    /// Last sample's value.
    pub y1: f32,
    /// Smallest sample value.
    pub lo: f32,
    /// Largest sample value.
    pub hi: f32,
    /// Number of samples, always at least one.
    pub count: u32,
}

/// A trace window reduced to its non-empty columns, in time order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Columns {
    /// Inclusive window start, in seconds.
    pub t0: f64,
    /// Exclusive window end, in seconds.
    pub t1: f64,
    /// Number of pixel columns across the window.
    pub cols: u32,
    pub cols_data: Vec<Column>,
    /// Last preceding sample, as (fractional column, value).
    pub before: Option<(f32, f32)>,
    /// First following sample, as (fractional column, value).
    pub after: Option<(f32, f32)>,
}

impl Columns {
    /// Reduces a trace using its chunk pyramids for dense windows.
    pub fn reduce_trace(store: &fab_elec_sim::trace::TraceStore, signal: usize, t0: f64, t1: f64, cols: u32) -> Self {
        let mut out = Self::default();
        out.reduce_trace_into(store, signal, t0, t1, cols);
        out
    }

    /// Fills an existing buffer without discarding its capacity.
    pub fn reduce_trace_into(&mut self, store: &fab_elec_sim::trace::TraceStore, signal: usize, t0: f64, t1: f64, cols: u32) {
        self.t0 = t0;
        self.t1 = t1;
        self.cols = cols;
        self.cols_data.clear();
        self.reduce_trace_suffix(store, signal, 0);
    }

    /// Refills from the old last visible column. Returns false if the prefix needs a full reduction.
    pub(super) fn reduce_trace_suffix(&mut self, store: &fab_elec_sim::trace::TraceStore, signal: usize, old_len: usize) -> bool {
        use super::store::{partition_time, range_minmax, time_index};
        use fab_elec_sim::trace::CHUNK;

        self.before = None;
        self.after = None;
        let Some(signal) = store.signals.get(signal) else {
            self.cols_data.clear();
            return true;
        };
        let (t0, t1, cols) = (self.t0, self.t1, self.cols);
        let time = &store.time;
        let duration = t1 - t0;
        if time.len() != signal.chunks.len() * CHUNK + signal.open.len()
            || cols == 0
            || !t0.is_finite()
            || !t1.is_finite()
            || !duration.is_finite()
            || duration <= 0.0
        {
            self.cols_data.clear();
            return true;
        }
        let start = time_index(time, t0);
        let end = time_index(time, t1);
        let position = |time: f64| (time - t0) / duration * f64::from(cols);
        if start > 0 {
            self.before = Some((position(time.at(start - 1)) as f32, signal.at(start - 1)));
        }
        if end < time.len() {
            self.after = Some((position(time.at(end)) as f32, signal.at(end)));
        }
        if start == end || (end <= old_len && !self.cols_data.is_empty()) {
            return true;
        }
        let sample_position = |t: f64| {
            let x = position(t);
            let index = (x.floor() as u32).min(cols - 1);
            let mut stored_x = x as f32;
            // Rounding to f32 must not move an interior sample into the next column.
            if f64::from(stored_x) >= f64::from(index) + 1.0 {
                stored_x = f32::from_bits(stored_x.to_bits() - 1);
            }
            (index, stored_x)
        };
        let sparse_limit = 8 * u64::from(cols);
        let sparse = (end - start) as u64 <= sparse_limit;
        let (first_sample, first_column) = if let Some((_, prefix)) = self.cols_data.split_last() {
            // Only a switch to the pyramid can change an earlier all-NaN column's payload.
            let was_sparse = (old_len - start) as u64 <= sparse_limit;
            let changes_nan = |c: &Column| {
                c.lo.is_nan() && (c.lo.to_bits() != c.y0.to_bits() || c.hi.to_bits() != c.y0.to_bits())
            };
            if !sparse && was_sparse && prefix.iter().any(changes_nan) {
                return false;
            }
            self.cols_data.truncate(prefix.len());
            let (c, _) = sample_position(time.at(old_len - 1));
            let a = partition_time(time, |t| position(t) < f64::from(c)).max(start);
            (a, c)
        } else {
            (start, 0)
        };
        if sparse {
            let mut previous = None;
            for i in first_sample..end {
                let (index, stored_x) = sample_position(time.at(i));
                let value = signal.at(i);
                if previous == Some(index) {
                    let column = self.cols_data.last_mut().unwrap();
                    column.x1 = stored_x;
                    column.y1 = value;
                    column.lo = column.lo.min(value);
                    column.hi = column.hi.max(value);
                    column.count += 1;
                } else {
                    self.cols_data.push(Column {
                        x0: stored_x,
                        x1: stored_x,
                        y0: value,
                        y1: value,
                        lo: value,
                        hi: value,
                        count: 1,
                    });
                    previous = Some(index);
                }
            }
            return true;
        }

        let mut a = first_sample;
        for c in first_column..cols {
            if a == end {
                break;
            }
            let b = if c == cols - 1 {
                end
            } else {
                partition_time(time, |t| position(t) < f64::from(c + 1)).clamp(a, end)
            };
            if a < b {
                let extremes = range_minmax(signal, a, b).unwrap();
                let y0 = signal.at(a);
                // An all-NaN column keeps its first sample's payload.
                let (lo, hi) = if extremes.min > extremes.max { (y0, y0) } else { (extremes.min, extremes.max) };
                self.cols_data.push(Column {
                    x0: sample_position(time.at(a)).1,
                    x1: sample_position(time.at(b - 1)).1,
                    y0,
                    y1: signal.at(b - 1),
                    lo,
                    hi,
                    count: (b - a) as u32,
                });
            }
            a = b;
        }
        true
    }

    /// Reduces increasing times and matching values in O(log n + visible samples).
    pub fn reduce(t: &[f64], y: &[f32], t0: f64, t1: f64, cols: u32) -> Self {
        let mut out = Self::default();
        out.reduce_into(t, y, t0, t1, cols);
        out
    }

    /// Mean samples per pixel column, including empty columns.
    pub fn density(&self) -> f32 {
        if self.cols == 0 {
            return 0.0;
        }
        let samples: u64 = self.cols_data.iter().map(|c| u64::from(c.count)).sum();
        (samples as f64 / f64::from(self.cols)) as f32
    }

    /// Reduce samples `t`, `y` over `t0..t1` into `cols` min/max columns, refilling this buffer
    /// without discarding its capacity (the workers' sparks and the scope's reference spark).
    pub fn reduce_into(&mut self, t: &[f64], y: &[f32], t0: f64, t1: f64, cols: u32) {
        self.t0 = t0;
        self.t1 = t1;
        self.cols = cols;
        self.cols_data.clear();
        self.before = None;
        self.after = None;
        let duration = t1 - t0;
        if t.len() != y.len() || cols == 0 || !t0.is_finite() || !t1.is_finite()
            || !duration.is_finite() || duration <= 0.0
        {
            return;
        }
        let start = t.partition_point(|&time| time < t0);
        let end = t.partition_point(|&time| time < t1);
        let position = |time: f64| (time - t0) / duration * f64::from(cols);
        if start > 0 {
            self.before = Some((position(t[start - 1]) as f32, y[start - 1]));
        }
        if end < t.len() {
            self.after = Some((position(t[end]) as f32, y[end]));
        }
        let mut previous = None;
        for i in start..end {
            let x = position(t[i]);
            let index = (x.floor() as u32).min(cols - 1);
            let mut stored_x = x as f32;
            // Rounding to f32 must not move an interior sample into the next column.
            if f64::from(stored_x) >= f64::from(index) + 1.0 {
                stored_x = f32::from_bits(stored_x.to_bits() - 1);
            }
            if previous == Some(index) {
                let column = self.cols_data.last_mut().unwrap();
                column.x1 = stored_x;
                column.y1 = y[i];
                column.lo = column.lo.min(y[i]);
                column.hi = column.hi.max(y[i]);
                column.count += 1;
            } else {
                self.cols_data.push(Column {
                    x0: stored_x,
                    x1: stored_x,
                    y0: y[i],
                    y1: y[i],
                    lo: y[i],
                    hi: y[i],
                    count: 1,
                });
                previous = Some(index);
            }
        }
    }
}

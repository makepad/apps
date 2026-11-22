//! A scalar field over a board's plane, published per solve as `Arc<HeatField>`: the board's
//! temperature for the heat overlay. ESIM's board thermal solve uses dissipated power per device
//! from the circuit sim, solving over the copper and stackup on a worker with a new Arc per solve.
//! It comes after the co-sim; until then the board bench's `--heat-test` is the only producer.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeatField {
    /// Grid size in cells, x then y.
    pub w: u32,
    pub h: u32,
    /// The board-plane box the grid covers, mm `[min_x, min_y, max_x, max_y]`, y up: cell (i, j)
    /// spans `min + (i, j) * size / (w, h)` and its value is the field at the cell's centre.
    pub bounds_mm: [f64; 4],
    /// The simulated time the field is of, seconds; 0 for a steady state.
    pub t: f64,
    /// The lowering solved for, `ValueIndex::generation`; a view drops a field of another generation.
    pub generation: u64,
    /// Row-major, `w * h` values, `values[j * w + i]`; row 0 is the `min_y` edge of `bounds_mm`
    /// in board coordinates (y up); NaN means no data (off the board or outside the solve).
    pub values: Vec<f32>,
    /// The values at the cold and hot ends of the ramp `[lo, hi]`.
    pub range: [f32; 2],
    /// What the field is, for the legend ("Board temperature", "Test heat field (synthetic)").
    pub label: String,
    /// The values' unit ("°C").
    pub unit: String,
}

impl HeatField {
    /// Whether the nonempty grid matches its values, with finite, ordered bounds and a finite range.
    pub fn is_valid(&self) -> bool {
        self.w > 0 && self.h > 0
            && (self.w as usize).checked_mul(self.h as usize) == Some(self.values.len())
            && self.bounds_mm.iter().all(|v| v.is_finite())
            && self.bounds_mm[2] > self.bounds_mm[0] && self.bounds_mm[3] > self.bounds_mm[1]
            && self.range.iter().all(|v| v.is_finite())
    }

    /// The value's position clamped to 0..1; None for NaN (no data), 0 for a flat/inverted range.
    pub fn ramp(&self, v: f32) -> Option<f32> {
        if v.is_nan() {
            return None;
        }
        let [lo, hi] = self.range;
        if !(hi > lo) {
            return Some(0.0);
        }
        let x = (v as f64 - lo as f64) / (hi as f64 - lo as f64);
        Some(if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) as f32 })
    }

    /// The finite min/max, skipping NaN; None if none: what a producer sets `range` from.
    pub fn span(values: &[f32]) -> Option<[f32; 2]> {
        let mut finite = values.iter().copied().filter(|v| v.is_finite());
        let first = finite.next()?;
        Some(finite.fold([first, first], |[lo, hi], v| [lo.min(v), hi.max(v)]))
    }
}

#[cfg(test)]
mod tests {
    use super::HeatField;

    #[test]
    fn span_and_ramp() {
        let mut field = HeatField {
            w: 2, h: 2, bounds_mm: [0.0, 0.0, 2.0, 2.0],
            values: vec![f32::NAN, 35.0, 20.0, f32::NAN], range: [20.0, 35.0],
            ..HeatField::default()
        };
        assert!(field.is_valid(), "NaN cells must not invalidate a field");
        assert_eq!(HeatField::span(&field.values), Some([20.0, 35.0]), "span must skip NaN cells");
        assert_eq!(HeatField::span(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY]), None, "span must require finite values");
        assert_eq!(HeatField::span(&[]), None, "an empty span must be absent");
        assert_eq!(field.ramp(f32::NAN), None, "NaN must have no ramp position");
        assert_eq!(field.ramp(10.0), Some(0.0), "ramp must clamp below the range");
        assert_eq!(field.ramp(40.0), Some(1.0), "ramp must clamp above the range");
        field.range = [20.0, 20.0];
        assert_eq!(field.ramp(30.0), Some(0.0), "a flat range must read zero");
        field.range = [35.0, 20.0];
        assert_eq!(field.ramp(30.0), Some(0.0), "an inverted range must read zero");
    }
}

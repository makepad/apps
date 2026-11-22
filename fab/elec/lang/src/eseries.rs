//! Standard preferred-number mantissas and selection by ratio.

/// A preferred-number series, named for its number of values per decade.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Series {
    /// Three values per decade.
    E3,
    /// Six values per decade.
    E6,
    /// Twelve values per decade.
    E12,
    /// Twenty-four values per decade.
    E24,
    /// Forty-eight values per decade.
    E48,
    /// Ninety-six values per decade.
    E96,
    /// One hundred ninety-two values per decade.
    E192,
}

impl Series {
    /// Recognise an exact series name, from `"E3"` through `"E192"`.
    pub fn of_name(name: &str) -> Option<Series> {
        match name {
            "E3" => Some(Series::E3),
            "E6" => Some(Series::E6),
            "E12" => Some(Series::E12),
            "E24" => Some(Series::E24),
            "E48" => Some(Series::E48),
            "E96" => Some(Series::E96),
            "E192" => Some(Series::E192),
            _ => None,
        }
    }

    /// The standard mantissas in increasing order: two digits through E24,
    /// three digits for E48, E96 and E192.
    pub fn values(self) -> &'static [u16] {
        match self {
            Series::E3 => &[10, 22, 47],
            Series::E6 => &[10, 15, 22, 33, 47, 68],
            Series::E12 => &[10, 12, 15, 18, 22, 27, 33, 39, 47, 56, 68, 82],
            Series::E24 => &[
                10, 11, 12, 13, 15, 16, 18, 20, 22, 24, 27, 30, 33, 36, 39, 43, 47, 51, 56,
                62, 68, 75, 82, 91,
            ],
            Series::E48 => &[
                100, 105, 110, 115, 121, 127, 133, 140, 147, 154, 162, 169, 178, 187, 196,
                205, 215, 226, 237, 249, 261, 274, 287, 301, 316, 332, 348, 365, 383, 402,
                422, 442, 464, 487, 511, 536, 562, 590, 619, 649, 681, 715, 750, 787, 825,
                866, 909, 953,
            ],
            Series::E96 => &[
                100, 102, 105, 107, 110, 113, 115, 118, 121, 124, 127, 130, 133, 137, 140,
                143, 147, 150, 154, 158, 162, 165, 169, 174, 178, 182, 187, 191, 196, 200,
                205, 210, 215, 221, 226, 232, 237, 243, 249, 255, 261, 267, 274, 280, 287,
                294, 301, 309, 316, 324, 332, 340, 348, 357, 365, 374, 383, 392, 402, 412,
                422, 432, 442, 453, 464, 475, 487, 499, 511, 523, 536, 549, 562, 576, 590,
                604, 619, 634, 649, 665, 681, 698, 715, 732, 750, 768, 787, 806, 825, 845,
                866, 887, 909, 931, 953, 976,
            ],
            Series::E192 => &[
                100, 101, 102, 104, 105, 106, 107, 109, 110, 111, 113, 114, 115, 117, 118,
                120, 121, 123, 124, 126, 127, 129, 130, 132, 133, 135, 137, 138, 140, 142,
                143, 145, 147, 149, 150, 152, 154, 156, 158, 160, 162, 164, 165, 167, 169,
                172, 174, 176, 178, 180, 182, 184, 187, 189, 191, 193, 196, 198, 200, 203,
                205, 208, 210, 213, 215, 218, 221, 223, 226, 229, 232, 234, 237, 240, 243,
                246, 249, 252, 255, 258, 261, 264, 267, 271, 274, 277, 280, 284, 287, 291,
                294, 298, 301, 305, 309, 312, 316, 320, 324, 328, 332, 336, 340, 344, 348,
                352, 357, 361, 365, 370, 374, 379, 383, 388, 392, 397, 402, 407, 412, 417,
                422, 427, 432, 437, 442, 448, 453, 459, 464, 470, 475, 481, 487, 493, 499,
                505, 511, 517, 523, 530, 536, 542, 549, 556, 562, 569, 576, 583, 590, 597,
                604, 612, 619, 626, 634, 642, 649, 657, 665, 673, 681, 690, 698, 706, 715,
                723, 732, 741, 750, 759, 768, 777, 787, 796, 806, 816, 825, 835, 845, 856,
                866, 876, 887, 898, 909, 920, 931, 942, 953, 965, 976, 988,
            ],
        }
    }
}

/// The series value nearest `v` in logarithmic distance, with ties going higher.
/// Nonpositive or nonfinite inputs are returned unchanged. Decimal mantissas and
/// exponents are parsed together so the result has one decimal-to-binary rounding.
pub fn nearest(v: f64, s: Series) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return v;
    }
    let values = s.values();
    let digits = if values[0] == 10 { 1 } else { 2 };
    let exponent = v.log10().floor() as i32 - digits;
    let mut best = v;
    let mut best_distance = f64::INFINITY;
    for exponent in exponent - 1..=exponent + 1 {
        for &mantissa in values {
            let candidate = format!("{mantissa}e{exponent}")
                .parse::<f64>()
                .expect("integer mantissa and exponent form a decimal");
            if candidate <= 0.0 || !candidate.is_finite() {
                continue;
            }
            let distance = (candidate / v).ln().abs();
            if distance < best_distance || (distance == best_distance && candidate > best) {
                best = candidate;
                best_distance = distance;
            }
        }
    }
    best
}

//! Integer-only division, square root and floating point rounding operations.
//! Arithmetic NaNs keep their sign and payload and are quieted (the first
//! operand wins for binary operations). Invalid operations use the module
//! format's positive default NaN. Absolute value, like the IEEE sign operation,
//! only clears the sign bit.

/// The binary arithmetic NaN rule, shared with exec and emitted back ends.
/// Check operands in order, then `invalid` (computed by the caller for its
/// operation). A non-NaN, valid pair returns None and may use hardware arithmetic.
pub fn binary_nan_f32(a: f32, b: f32, invalid: bool) -> Option<f32> {
    for bits in [a.to_bits(), b.to_bits()] {
        if bits & 0x7FFF_FFFF > 0x7F80_0000 {
            return Some(f32::from_bits(bits | 0x0040_0000));
        }
    }
    if invalid { Some(f32::from_bits(0x7FC0_0000)) } else { None }
}

/// The f64 counterpart of [`binary_nan_f32`], including first-operand priority.
pub fn binary_nan_f64(a: f64, b: f64, invalid: bool) -> Option<f64> {
    for bits in [a.to_bits(), b.to_bits()] {
        if bits & 0x7FFF_FFFF_FFFF_FFFF > 0x7FF0_0000_0000_0000 {
            return Some(f64::from_bits(bits | 0x0008_0000_0000_0000));
        }
    }
    if invalid { Some(f64::from_bits(0x7FF8_0000_0000_0000)) } else { None }
}

/// Integer-only IEEE binary32 division, round to nearest, ties to even.
/// Gradual underflow is rounded once, from an exact quotient and remainder.
pub fn div_f32(a: f32, b: f32) -> f32 {
    let ab = a.to_bits();
    let bb = b.to_bits();
    let sign = (ab ^ bb) & 0x8000_0000;
    let am = ab & 0x7FFF_FFFF;
    let bm = bb & 0x7FFF_FFFF;
    if let Some(nan) = binary_nan_f32(a, b,
        (am == 0 && bm == 0) || (am == 0x7F80_0000 && bm == 0x7F80_0000)) {
        return nan;
    }
    if am == 0x7F80_0000 || bm == 0 { return f32::from_bits(sign | 0x7F80_0000); }
    if am == 0 || bm == 0x7F80_0000 { return f32::from_bits(sign); }

    let normalize = |mag: u32| {
        let mut exponent = (mag >> 23) as i32 - 127;
        let mut mantissa = mag & 0x007F_FFFF;
        if mag < 0x0080_0000 {
            let shift = mantissa.leading_zeros() - 8;
            mantissa <<= shift;
            exponent = -126 - shift as i32;
        } else {
            mantissa |= 0x0080_0000;
        }
        (mantissa, exponent)
    };
    let (mut numerator, ae) = normalize(am);
    let (denominator, be) = normalize(bm);
    let mut exponent = ae - be;
    if numerator < denominator {
        numerator <<= 1;
        exponent -= 1;
    }
    if exponent > 127 { return f32::from_bits(sign | 0x7F80_0000); }
    if exponent < -150 { return f32::from_bits(sign); }
    // Both mantissas have 24 bits. Restoring division needs at most 25 bits
    // for its remainder and is directly portable to a 32-bit integer ISA.
    let mut quotient = 1u32;
    let mut remainder = numerator - denominator;
    for _ in 0..23 {
        remainder <<= 1;
        quotient <<= 1;
        if remainder >= denominator {
            remainder -= denominator;
            quotient |= 1;
        }
    }
    if exponent < -126 {
        let shift = (-126 - exponent) as u32;
        let tail = quotient & ((1 << shift) - 1);
        let half = 1 << (shift - 1);
        quotient >>= shift;
        if tail > half || (tail == half && (remainder != 0 || quotient & 1 != 0)) {
            quotient += 1;
        }
        // A carry into bit 23 is the smallest normal, without a separate case.
        return f32::from_bits(sign | quotient);
    }
    let twice = remainder << 1;
    if twice > denominator || (twice == denominator && quotient & 1 != 0) {
        quotient += 1;
    }
    // A mantissa carry increments the exponent, including overflow to infinity.
    let magnitude = (((exponent + 127) as u32) << 23) + (quotient - 0x0080_0000);
    f32::from_bits(sign | magnitude)
}

#[derive(Clone, Copy)]
enum Rounding {
    Floor,
    Ceil,
    Trunc,
    Away,
}

fn integral(bits: u64, fraction: u32, bias: i32, mode: Rounding) -> u64 {
    let infinity = (bias as u64 * 2 + 1) << fraction;
    let sign_bit = infinity + (1u64 << fraction);
    let sign = bits & sign_bit;
    let mag = bits & (sign_bit - 1);
    if mag >= infinity {
        return if mag > infinity { bits | (1u64 << (fraction - 1)) } else { bits };
    }
    if mag == 0 {
        return bits;
    }
    let exponent = (mag >> fraction) as i32 - bias;
    if exponent >= fraction as i32 {
        return bits;
    }
    if exponent < 0 {
        let up = match mode {
            Rounding::Floor => sign != 0,
            Rounding::Ceil => sign == 0,
            Rounding::Trunc => false,
            Rounding::Away => exponent == -1,
        };
        return sign | if up { (bias as u64) << fraction } else { 0 };
    }
    let unit = 1u64 << (fraction - exponent as u32);
    let tail = mag & (unit - 1);
    let up = match mode {
        Rounding::Floor => sign != 0 && tail != 0,
        Rounding::Ceil => sign == 0 && tail != 0,
        Rounding::Trunc => false,
        Rounding::Away => tail >= unit / 2,
    };
    sign | ((mag & !(unit - 1)) + if up { unit } else { 0 })
}

// Restoring square root: the remainder is exact, including all sticky bits.
fn isqrt(mut n: u128) -> (u128, u128) {
    let mut bit = 1u128 << ((127 - n.leading_zeros()) & !1);
    let mut root = 0;
    while bit != 0 {
        if n >= root + bit {
            n -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    (root, n)
}

fn sqrt(bits: u64, fraction: u32, bias: i32) -> u64 {
    let hidden = 1u64 << fraction;
    let infinity = (bias as u64 * 2 + 1) << fraction;
    let sign = infinity + hidden;
    let mag = bits & (sign - 1);
    let quiet = hidden >> 1;
    if mag > infinity {
        return bits | quiet;
    }
    if mag == 0 {
        return bits;
    }
    if bits & sign != 0 {
        return infinity | quiet;
    }
    if mag == infinity {
        return bits;
    }
    let mut exponent = (mag >> fraction) as i32 - bias;
    let mut mantissa = mag & (hidden - 1);
    if mag < hidden {
        let shift = mantissa.leading_zeros() - (63 - fraction);
        mantissa <<= shift;
        exponent = 1 - bias - shift as i32;
    } else {
        mantissa |= hidden;
    }
    if exponent & 1 != 0 {
        mantissa <<= 1;
        exponent -= 1;
    }
    let (mut root, remainder) = isqrt((mantissa as u128) << fraction);
    // (root + 1/2)^2 = root^2 + root + 1/4. An integer
    // radicand cannot tie, so remainder > root rounds upward.
    if remainder > root {
        root += 1;
    }
    (((exponent / 2 + bias) as u64) << fraction) + (root as u64 - hidden)
}

pub fn sqrt_f32(x: f32) -> f32 { f32::from_bits(sqrt(x.to_bits() as u64, 23, 127) as u32) }
pub fn floor_f32(x: f32) -> f32 { f32::from_bits(integral(x.to_bits() as u64, 23, 127, Rounding::Floor) as u32) }
pub fn ceil_f32(x: f32) -> f32 { f32::from_bits(integral(x.to_bits() as u64, 23, 127, Rounding::Ceil) as u32) }
pub fn trunc_f32(x: f32) -> f32 { f32::from_bits(integral(x.to_bits() as u64, 23, 127, Rounding::Trunc) as u32) }
pub fn round_f32(x: f32) -> f32 { f32::from_bits(integral(x.to_bits() as u64, 23, 127, Rounding::Away) as u32) }
pub fn abs_f32(x: f32) -> f32 { f32::from_bits(x.to_bits() & 0x7FFF_FFFF) }
pub fn sqrt_f64(x: f64) -> f64 { f64::from_bits(sqrt(x.to_bits(), 52, 1023)) }
pub fn floor_f64(x: f64) -> f64 { f64::from_bits(integral(x.to_bits(), 52, 1023, Rounding::Floor)) }
pub fn ceil_f64(x: f64) -> f64 { f64::from_bits(integral(x.to_bits(), 52, 1023, Rounding::Ceil)) }
pub fn trunc_f64(x: f64) -> f64 { f64::from_bits(integral(x.to_bits(), 52, 1023, Rounding::Trunc)) }
pub fn round_f64(x: f64) -> f64 { f64::from_bits(integral(x.to_bits(), 52, 1023, Rounding::Away)) }
pub fn abs_f64(x: f64) -> f64 { f64::from_bits(x.to_bits() & 0x7FFF_FFFF_FFFF_FFFF) }

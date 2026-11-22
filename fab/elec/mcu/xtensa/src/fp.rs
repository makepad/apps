//! Single-precision arithmetic for the Operations on pp. 328, 487–488,
//! 518, 523, 628, and conversions pp. 383, 432–434, 580, 633, 636, 643.
//! No host floating-point arithmetic, NaN payloads, or intermediate rounding.

use crate::insn::Op;

// PDF field diagrams pp. 108–109. Bits 31:12 are MBZ in both registers;
// FCR ignores 11:7, FSR ignores 6:0. Trap enables are FCR bits 6:2.
pub(crate) const FCR_MASK: u32 = 0x7f;
pub(crate) const FSR_MASK: u32 = 0xf80;
pub(crate) const I: u32 = 1 << 7;
pub(crate) const U: u32 = 1 << 8;
const O: u32 = 1 << 9;
pub(crate) const V: u32 = 1 << 11;
const SIGN: u32 = 0x8000_0000;
const INF: u32 = 0x7f80_0000;
const FRACTION: u32 = 0x007f_ffff;
/// ISA Summary p. 109: only Underflow is undetermined; the rounded result and
/// other flags are delivered. The CPU records this separately from FSR's bits.
pub(crate) const TININESS_UNDEFINED: u32 = 1 << 31;

#[derive(Clone, Copy)]
pub(crate) enum Issue {
    Nan,
    Signaling,
    Trap,
    UnsignedNegative,
    Tininess,
}

/// Also identifies every instruction that references coprocessor-0 state
/// (§4.4.4 pp. 149–151, Coprocessor Group p. 323, RUR/WUR pp. 587, 652).
pub(crate) fn reason(op: Op, issue: Issue) -> Option<&'static str> {
    use Op::*;
    macro_rules! why {
        ($name:literal) => {
            Some(match issue {
                Issue::Nan => concat!($name, ": NaN result bits not specified by ISA Summary"),
                Issue::Signaling => concat!($name, ": signaling NaN encoding not specified by ISA Summary"),
                Issue::Trap => concat!($name, ": enabled FP trap behavior is implementation-dependent"),
                Issue::UnsignedNegative => concat!($name, ": negative signed-range result has unspecified Invalid flag"),
                Issue::Tininess => concat!($name, ": Underflow for a result rounding up to 2^-126 needs IEEE754's tininess after rounding, which the ISA Summary names but does not define"),
            })
        };
    }
    match op {
        AbsS => why!("ABS.S"), AddS => why!("ADD.S"), AddexpS => why!("ADDEXP.S"),
        AddexpmS => why!("ADDEXPM.S"), CeilS => why!("CEIL.S"), ConstS => why!("CONST.S"),
        Div0S => why!("DIV0.S"), DivnS => why!("DIVN.S"), FloatS => why!("FLOAT.S"),
        FloorS => why!("FLOOR.S"), Lsi => why!("LSI"), Lsip => why!("LSIP"),
        Lsx => why!("LSX"), Lsxp => why!("LSXP"), MaddS => why!("MADD.S"),
        MaddnS => why!("MADDN.S"), MkdadjS => why!("MKDADJ.S"), MksadjS => why!("MKSADJ.S"),
        MovS => why!("MOV.S"), MoveqzS => why!("MOVEQZ.S"), MovfS => why!("MOVF.S"),
        MovgezS => why!("MOVGEZ.S"), MovltzS => why!("MOVLTZ.S"), MovnezS => why!("MOVNEZ.S"),
        MovtS => why!("MOVT.S"), MsubS => why!("MSUB.S"), MulS => why!("MUL.S"),
        NegS => why!("NEG.S"), Nexp01S => why!("NEXP01.S"), OeqS => why!("OEQ.S"),
        OleS => why!("OLE.S"), OltS => why!("OLT.S"), Recip0S => why!("RECIP0.S"),
        Rfr => why!("RFR"), RoundS => why!("ROUND.S"), Rsqrt0S => why!("RSQRT0.S"),
        RurFcr => why!("RUR.FCR"), RurFsr => why!("RUR.FSR"), Sqrt0S => why!("SQRT0.S"),
        Ssi => why!("SSI"), Ssip => why!("SSIP"), Ssx => why!("SSX"), Ssxp => why!("SSXP"),
        SubS => why!("SUB.S"), TruncS => why!("TRUNC.S"), UeqS => why!("UEQ.S"),
        UfloatS => why!("UFLOAT.S"), UleS => why!("ULE.S"), UltS => why!("ULT.S"),
        UnS => why!("UN.S"), UtruncS => why!("UTRUNC.S"), Wfr => why!("WFR"),
        WurFcr => why!("WUR.FCR"), WurFsr => why!("WUR.FSR"),
        _ => None,
    }
}

pub(crate) fn is_fp(op: Op) -> bool { reason(op, Issue::Nan).is_some() }

pub(crate) fn is_nan(bits: u32) -> bool { bits & !SIGN > INF }

// §4.3.11.2 pp. 107–108: finite value = (-1)^sign * significand * 2^exponent.
fn parts(bits: u32) -> (u64, i32) {
    let exponent = (bits >> 23) & 255;
    if exponent == 0 { ((bits & FRACTION) as u64, -149) }
    else { (((bits & FRACTION) | (1 << 23)) as u64, exponent as i32 - 150) }
}

/// An exact nonnegative multiple of 2^-298. The smallest f32 product is
/// 2^-298, the largest is less than 2^256. Nine limbs cover all 554 bits
/// plus the carry from adding an f32. Keeping every bit also handles
/// cancellation and halfway cases that a rounded f64 intermediate can lose.
struct Magnitude([u64; 9]);

impl Magnitude {
    fn new(significand: u64, exponent: i32) -> Self {
        let shift = (exponent + 298) as usize;
        let mut limbs = [0; 9];
        limbs[shift / 64] = significand << (shift % 64);
        if shift % 64 != 0 && shift / 64 + 1 < limbs.len() {
            limbs[shift / 64 + 1] = significand >> (64 - shift % 64);
        }
        Self(limbs)
    }

    fn float(bits: u32) -> Self {
        let (m, e) = parts(bits);
        Self::new(m, e)
    }

    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }

    fn add(&mut self, other: &Self) {
        let mut carry = 0u128;
        for (a, b) in self.0.iter_mut().zip(other.0) {
            let sum = *a as u128 + b as u128 + carry;
            *a = sum as u64;
            carry = sum >> 64;
        }
    }

    fn sub(&mut self, other: &Self) {
        let mut borrow = false;
        for (a, b) in self.0.iter_mut().zip(other.0) {
            let (difference, first) = a.overflowing_sub(b);
            let (difference, second) = difference.overflowing_sub(u64::from(borrow));
            *a = difference;
            borrow = first || second;
        }
    }

    fn bit(&self, n: u32) -> bool { self.0[n as usize / 64] & (1 << (n % 64)) != 0 }

    fn below(&self, n: u32) -> bool {
        let word = n as usize / 64;
        self.0[..word].iter().any(|x| *x != 0)
            || (n % 64 != 0 && self.0[word] & ((1u64 << (n % 64)) - 1) != 0)
    }

    fn round(&self, negative: bool, mode: u32) -> (u32, u32) {
        let sign = if negative { SIGN } else { 0 };
        let Some(word) = self.0.iter().rposition(|x| *x != 0) else { return (sign, 0); };
        let highest = word as u32 * 64 + 63 - self.0[word].leading_zeros();
        // 24 significant bits for normals; subnormals have fixed quantum 2^-149.
        let mut cut = highest.saturating_sub(23).max(149);
        let mut q = 0;
        for n in 0..24 { if self.bit(cut + n) { q |= 1 << n; } }
        let inexact = self.below(cut);
        let halfway = self.bit(cut - 1);
        let lower = self.below(cut - 1);
        if increment(mode, negative, q & 1 != 0, halfway, lower) { q += 1; }
        if q == 1 << 24 { q >>= 1; cut += 1; }
        if cut >= 403 {
            // Overflow rounds to infinity or the maximum finite magnitude
            // according to RM (§4.3.11.2–4, pp. 108–110).
            let infinity = mode == 0 || (mode == 2 && !negative) || (mode == 3 && negative);
            return (sign | if infinity { INF } else { INF - 1 }, O | I);
        }
        let bits = sign | if q < 1 << 23 { q } else { ((cut - 148) << 23) | (q & FRACTION) };
        // Table 48 note 1 p. 109: Underflow is IEEE754's "tininess" after rounding with "loss of
        // accuracy" meaning inexact. A result is taken as tiny here when its rounded value is
        // subnormal. The Summary does not define "after rounding" beyond naming IEEE754, so
        // where it could also mean rounding to 24 bits with an unbounded exponent, the flag is
        // left undetermined. That differs only for an inexact value in [2^-127, 2^-126): rounded
        // at its own 24-bit quantum (bit 148) it stays below 2^-126 unless it carries out.
        let tiny = bits & INF == 0;
        let undetermined = inexact && highest == 171 && {
            let mut q = 0u32;
            for n in 0..24 { if self.bit(148 + n) { q |= 1 << n; } }
            let carries = q == (1 << 24) - 1 && increment(mode, negative, true, self.bit(147), self.below(147));
            !carries != tiny
        };
        // p. 109: never present an undetermined Underflow as a determined flag.
        let flags = if inexact { I | if tiny && !undetermined { U } else { 0 } } else { 0 }
            | if undetermined { TININESS_UNDEFINED } else { 0 };
        (bits, flags)
    }
}

// Table 47 p. 108: nearest/even, zero, +infinity, -infinity. The IEEE754
// nearest tie rule applies to the single rounding in the cited Operations.
fn increment(mode: u32, negative: bool, odd: bool, half: bool, low: bool) -> bool {
    match mode {
        0 => half && (low || odd),
        1 => false,
        2 => !negative && (half || low),
        _ => negative && (half || low),
    }
}

fn sum(mut a: Magnitude, an: bool, mut b: Magnitude, bn: bool, mode: u32) -> (u32, u32) {
    if an == bn { a.add(&b); a.round(an, mode) }
    else {
        match a.cmp(&b) {
            std::cmp::Ordering::Greater => { a.sub(&b); a.round(an, mode) }
            std::cmp::Ordering::Less => { b.sub(&a); b.round(bn, mode) }
            std::cmp::Ordering::Equal => (if mode == 3 { SIGN } else { 0 }, 0),
        }
    }
}

pub(crate) fn arithmetic(op: Op, r: u32, s: u32, t: u32, mode: u32) -> Result<(u32, u32), Issue> {
    use Op::*;
    let fused = matches!(op, MaddS | MsubS | MaddnS);
    // The Summary names IEEE754 but gives no payload/sign selection rule
    // for arithmetic NaN results, including invalid operations (pp. 108–110).
    if is_nan(s) || is_nan(t) || (fused && is_nan(r)) { return Err(Issue::Nan); }
    let mode = if op == MaddnS { 0 } else { mode };
    let answer = if matches!(op, AddS | SubS) {
        let t = t ^ if op == SubS { SIGN } else { 0 };
        if s & !SIGN == INF || t & !SIGN == INF {
            if s & !SIGN == INF && t & !SIGN == INF && (s ^ t) & SIGN != 0 {
                return Err(Issue::Nan);
            }
            (if s & !SIGN == INF { s } else { t }, 0)
        } else {
            sum(Magnitude::float(s), s & SIGN != 0, Magnitude::float(t), t & SIGN != 0, mode)
        }
    } else {
        let sign = (s ^ t ^ if op == MsubS { SIGN } else { 0 }) & SIGN;
        let infinity = s & !SIGN == INF || t & !SIGN == INF;
        if infinity && (s & !SIGN == 0 || t & !SIGN == 0) { return Err(Issue::Nan); }
        if infinity {
            if fused && r & !SIGN == INF && (r ^ sign) & SIGN != 0 { return Err(Issue::Nan); }
            (sign | INF, 0)
        } else if fused && r & !SIGN == INF { (r, 0) }
        else {
            let (sm, se) = parts(s);
            let (tm, te) = parts(t);
            let product = Magnitude::new(sm * tm, se + te);
            if fused { sum(product, sign != 0, Magnitude::float(r), r & SIGN != 0, mode) }
            else { product.round(sign != 0, mode) }
        }
    };
    // MADDN.S p. 488 explicitly leaves all flags unchanged, so an undetermined Underflow
    // does not matter there.
    if op == MaddnS { return Ok((answer.0, 0)); }
    // p. 109: tininess uncertainty affects only Underflow, not the result.
    // Observation and enabled-trap checks belong to the CPU's FSR state.
    Ok(answer)
}

pub(crate) fn from_integer(value: u32, unsigned: bool, scale: u8, mode: u32) -> (u32, u32) {
    let negative = !unsigned && value & SIGN != 0;
    let magnitude = if negative { value.wrapping_neg() } else { value };
    // Scaling by 2^-t is exact here: every nonzero input stays normal
    // (pp. 432, 636), so one rounding of this exact rational is equivalent,
    // and Underflow cannot arise.
    let (bits, flags) = Magnitude::new(magnitude as u64, -(scale as i32)).round(negative, mode);
    debug_assert!(flags & TININESS_UNDEFINED == 0, "an integer conversion is never tiny");
    (bits, flags & !TININESS_UNDEFINED)
}

pub(crate) fn to_integer(op: Op, value: u32, scale: u8) -> Result<(u32, u32), Issue> {
    use Op::*;
    let unsigned = op == UtruncS;
    let negative = value & SIGN != 0;
    let upper = if unsigned { u32::MAX } else { i32::MAX as u32 };
    // Conversion pages explicitly specify NaN saturation, unlike arithmetic.
    if is_nan(value) { return Ok((upper, V)); }
    if value & !SIGN == INF { return Ok((if negative { SIGN } else { upper }, V)); }
    let (m, e) = parts(value);
    let e = e + scale as i32;
    let mode = match op { RoundS => 0, FloorS => 3, CeilS => 2, _ => 1 };
    let (q, inexact) = if e >= 0 {
        if e > 32 { return Ok((if negative { SIGN } else { upper }, V)); }
        (m << e, false)
    } else {
        let shift = (-e) as u32;
        let q = if shift < 64 { m >> shift } else { 0 };
        let half = shift <= 64 && m & (1u64 << (shift - 1)) != 0;
        let low = if shift > 64 { m != 0 } else { m & ((1u64 << (shift - 1)) - 1) != 0 };
        (q + u64::from(increment(mode, negative, q & 1 != 0, half, low)), half || low)
    };
    // UTRUNC.S p. 643 explicitly gives negative inputs the TRUNC.S result,
    // including its signed range, instead of clamping all negatives to zero.
    let limit = if negative { 1u64 << 31 } else { upper as u64 };
    if q > limit { return Ok((if negative { SIGN } else { upper }, V)); }
    // p. 643 specifies the same ANSWER as TRUNC.S for negative inputs,
    // but not whether V follows signed or unsigned representability. Those
    // disagree for a nonzero signed-range result. Both agree for zero and
    // signed overflow, which remain modeled. Do not guess the missing flag.
    if unsigned && negative && q != 0 { return Err(Issue::UnsignedNegative); }
    Ok((if negative { (q as u32).wrapping_neg() } else { q as u32 }, if inexact { I } else { 0 }))
}

pub(crate) fn compare(op: Op, s: u32, t: u32) -> Result<(bool, u32), Issue> {
    use Op::*;
    if is_nan(s) || is_nan(t) {
        // OLE/OLT pp. 550–551 raise V for EVERY NaN. The other five
        // compare pages distinguish signaling NaNs, but neither those pages
        // nor §4.3.11 defines their bit encoding. Do not guess bit 22's sense.
        if matches!(op, OleS | OltS) { return Ok((false, V)); }
        return Err(Issue::Signaling);
    }
    let equal = s == t || (s & !SIGN == 0 && t & !SIGN == 0);
    let less = !equal && if (s ^ t) & SIGN != 0 { s & SIGN != 0 }
        else if s & SIGN != 0 { s > t } else { s < t };
    Ok((match op {
        OeqS | UeqS => equal,
        OltS | UltS => less,
        OleS | UleS => less || equal,
        _ => false, // UN.S; neither operand is a NaN here.
    }, 0))
}

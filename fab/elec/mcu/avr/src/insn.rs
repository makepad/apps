//! The AVRe+ subset used by microsplash, with checked operands.
//!
//! Y/Z loads and stores without displacement share their encoding with LDD/STD
//! at displacement zero. Decoding always returns LDD/STD for those encodings.
//! Auto-update loads/stores that overlap either pointer register, and LPM Z+
//! into r30/r31, have undefined hardware results and are deliberately rejected.

pub type Reg = u8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ptr { X, XInc, XDec, Y, YInc, YDec, Z, ZInc, ZDec }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Insn {
    Add(Reg, Reg), Adc(Reg, Reg), Sub(Reg, Reg), Sbc(Reg, Reg),
    And(Reg, Reg), Or(Reg, Reg), Eor(Reg, Reg), Mov(Reg, Reg),
    Cp(Reg, Reg), Cpc(Reg, Reg), Cpse(Reg, Reg), Mul(Reg, Reg),
    Muls(Reg, Reg), Mulsu(Reg, Reg), Fmul(Reg, Reg), Fmuls(Reg, Reg), Fmulsu(Reg, Reg),
    Movw(Reg, Reg),
    Subi(Reg, u8), Sbci(Reg, u8), Andi(Reg, u8), Ori(Reg, u8), Cpi(Reg, u8), Ldi(Reg, u8),
    Adiw(Reg, u8), Sbiw(Reg, u8),
    Com(Reg), Neg(Reg), Swap(Reg), Inc(Reg), Dec(Reg), Asr(Reg), Lsr(Reg), Ror(Reg),
    Bset(u8), Bclr(u8), Bst(Reg, u8), Bld(Reg, u8), Sbrc(Reg, u8), Sbrs(Reg, u8),
    Sbic(u8, u8), Sbis(u8, u8), Sbi(u8, u8), Cbi(u8, u8),
    In(Reg, u8), Out(u8, Reg),
    Brbs(u8, i8), Brbc(u8, i8), Rjmp(i16), Rcall(i16), Jmp(u32), Call(u32),
    Ijmp, Icall, Ret, Reti,
    Ld(Reg, Ptr), St(Ptr, Reg), Ldd(Reg, bool, u8), Std(bool, u8, Reg),
    Lds(Reg, u16), Sts(u16, Reg), Lpm, LpmZ(Reg), LpmZInc(Reg), Push(Reg), Pop(Reg),
    Nop, Sleep, Wdr, Break,
}

pub fn words(i: &Insn) -> usize {
    match i { Insn::Jmp(_) | Insn::Call(_) | Insn::Lds(..) | Insn::Sts(..) => 2, _ => 1 }
}

/// ATmega328P (AVRe+) clocks: (base, conditional extra). The emitter will use
/// this same function as the oracle. Base is for a branch not taken or no skip.
/// A taken branch adds extra; a taken skip adds extra for a one-word instruction,
/// and the caller must add one more clock when the skipped instruction is two words.
pub fn cycles(i: &Insn) -> (u8, u8) {
    use Insn::*;
    match i {
        Call(_) | Ret | Reti => (4,0),
        Rcall(_) | Icall | Jmp(_) | Lpm | LpmZ(_) | LpmZInc(_) => (3,0),
        Rjmp(_) | Ijmp | Ld(..) | St(..) | Ldd(..) | Std(..) | Lds(..) | Sts(..) |
        Push(_) | Pop(_) | Adiw(..) | Sbiw(..) | Mul(..) | Muls(..) | Mulsu(..) |
        Fmul(..) | Fmuls(..) | Fmulsu(..) | Sbi(..) | Cbi(..) => (2,0),
        Brbs(..) | Brbc(..) | Cpse(..) | Sbrc(..) | Sbrs(..) | Sbic(..) | Sbis(..) => (1,1),
        Add(..) | Adc(..) | Sub(..) | Sbc(..) | And(..) | Or(..) | Eor(..) |
        Mov(..) | Cp(..) | Cpc(..) | Movw(..) | Subi(..) | Sbci(..) | Andi(..) |
        Ori(..) | Cpi(..) | Ldi(..) | Com(_) | Neg(_) | Swap(_) | Inc(_) | Dec(_) |
        Asr(_) | Lsr(_) | Ror(_) | Bset(_) | Bclr(_) | Bst(..) | Bld(..) |
        In(..) | Out(..) | Nop | Sleep | Wdr | Break => (1,0),
    }
}

fn range(name: &str, value: u32, lo: u32, hi: u32) -> Result<u16, String> {
    if (lo..=hi).contains(&value) { Ok(value as u16) }
    else { Err(format!("{name} operand {value} outside {lo}..={hi}")) }
}

fn reg(name: &str, r: Reg) -> Result<u16, String> { range(name, r as u32, 0, 31) }
fn bit(b: u8) -> Result<u16, String> { range("bit", b as u32, 0, 7) }

pub(crate) fn pointer_base(p: Ptr) -> usize {
    match p {
        Ptr::X | Ptr::XInc | Ptr::XDec => 26,
        Ptr::Y | Ptr::YInc | Ptr::YDec => 28,
        Ptr::Z | Ptr::ZInc | Ptr::ZDec => 30,
    }
}

fn overlap(r: Reg, p: Ptr) -> bool {
    !matches!(p, Ptr::X | Ptr::Y | Ptr::Z) && r as usize / 2 == pointer_base(p) / 2
}

pub fn encode(i: &Insn) -> Result<([u16; 2], usize), String> {
    use Insn::*;
    let mut second = 0;
    let first = match *i {
        Add(d,r) | Adc(d,r) | Sub(d,r) | Sbc(d,r) | And(d,r) | Or(d,r) |
        Eor(d,r) | Mov(d,r) | Cp(d,r) | Cpc(d,r) | Cpse(d,r) | Mul(d,r) => {
            let d = reg("d", d)?;
            let r = reg("r", r)?;
            let op = match i {
                Add(..) => 0x0c00, Adc(..) => 0x1c00, Sub(..) => 0x1800, Sbc(..) => 0x0800,
                And(..) => 0x2000, Or(..) => 0x2800, Eor(..) => 0x2400, Mov(..) => 0x2c00,
                Cp(..) => 0x1400, Cpc(..) => 0x0400, Cpse(..) => 0x1000, _ => 0x9c00,
            };
            op | d << 4 | (r & 15) | (r & 16) << 5
        }
        Muls(d,r) => {
            let d = range("d", d as u32, 16, 31)? - 16;
            let r = range("r", r as u32, 16, 31)? - 16;
            0x0200 | d << 4 | r
        }
        Mulsu(d,r) | Fmul(d,r) | Fmuls(d,r) | Fmulsu(d,r) => {
            let d = range("d", d as u32, 16, 23)? - 16;
            let r = range("r", r as u32, 16, 23)? - 16;
            let op = match i { Mulsu(..) => 0x0300, Fmul(..) => 0x0308,
                Fmuls(..) => 0x0380, _ => 0x0388 };
            op | d << 4 | r
        }
        Movw(d,r) => {
            let d = reg("d", d)?;
            let r = reg("r", r)?;
            if d & 1 != 0 { return Err(format!("d operand {d} must be even")); }
            if r & 1 != 0 { return Err(format!("r operand {r} must be even")); }
            0x0100 | (d / 2) << 4 | r / 2
        }
        Subi(d,k) | Sbci(d,k) | Andi(d,k) | Ori(d,k) | Cpi(d,k) | Ldi(d,k) => {
            let d = range("d", d as u32, 16, 31)? - 16;
            let k = k as u16;
            let op = match i { Subi(..) => 0x5000, Sbci(..) => 0x4000,
                Andi(..) => 0x7000, Ori(..) => 0x6000, Cpi(..) => 0x3000, _ => 0xe000 };
            op | (k & 0xf0) << 4 | d << 4 | (k & 15)
        }
        Adiw(d,k) | Sbiw(d,k) => {
            if !matches!(d, 24 | 26 | 28 | 30) {
                return Err(format!("d operand {d} must be one of 24, 26, 28, 30"));
            }
            let k = range("k", k as u32, 0, 63)?;
            (if matches!(i, Adiw(..)) { 0x9600 } else { 0x9700 }) |
                ((d as u16 - 24) / 2) << 4 | (k & 0x30) << 2 | (k & 15)
        }
        Com(d) | Neg(d) | Swap(d) | Inc(d) | Dec(d) | Asr(d) | Lsr(d) | Ror(d) |
        Push(d) | Pop(d) | LpmZ(d) | LpmZInc(d) => {
            let d = reg("d/r", d)?;
            if matches!(i, LpmZInc(_)) && d >= 30 {
                return Err(format!("d operand {d} overlaps Z in LPM Z+ (undefined)"));
            }
            let op = match i { Com(_) => 0x9400, Neg(_) => 0x9401, Swap(_) => 0x9402,
                Inc(_) => 0x9403, Dec(_) => 0x940a, Asr(_) => 0x9405, Lsr(_) => 0x9406,
                Ror(_) => 0x9407, Push(_) => 0x920f, Pop(_) => 0x900f,
                LpmZ(_) => 0x9004, _ => 0x9005 };
            op | d << 4
        }
        Bset(b) | Bclr(b) => (if matches!(i, Bset(_)) { 0x9408 } else { 0x9488 }) | bit(b)? << 4,
        Bst(d,b) | Bld(d,b) | Sbrc(d,b) | Sbrs(d,b) => {
            let op = match i { Bst(..) => 0xfa00, Bld(..) => 0xf800,
                Sbrc(..) => 0xfc00, _ => 0xfe00 };
            op | reg("d/r", d)? << 4 | bit(b)?
        }
        Sbic(a,b) | Sbis(a,b) | Sbi(a,b) | Cbi(a,b) => {
            let op = match i { Sbic(..) => 0x9900, Sbis(..) => 0x9b00,
                Sbi(..) => 0x9a00, _ => 0x9800 };
            op | range("I/O address", a as u32, 0, 31)? << 3 | bit(b)?
        }
        In(d,a) | Out(a,d) => {
            let a = range("I/O address", a as u32, 0, 63)?;
            (if matches!(i, In(..)) { 0xb000 } else { 0xb800 }) |
                reg("d/r", d)? << 4 | (a & 0x30) << 5 | (a & 15)
        }
        Brbs(b,k) | Brbc(b,k) => {
            if !(-64..=63).contains(&k) { return Err(format!("k operand {k} outside -64..=63")); }
            (if matches!(i, Brbs(..)) { 0xf000 } else { 0xf400 }) |
                ((k as u16) & 0x7f) << 3 | bit(b)?
        }
        Rjmp(k) | Rcall(k) => {
            if !(-2048..=2047).contains(&k) { return Err(format!("k operand {k} outside -2048..=2047")); }
            (if matches!(i, Rjmp(_)) { 0xc000 } else { 0xd000 }) | ((k as u16) & 0xfff)
        }
        Jmp(k) | Call(k) => {
            if k >= 1 << 22 { return Err(format!("word address operand {k} exceeds 22 bits")); }
            second = k as u16;
            (if matches!(i, Jmp(_)) { 0x940c } else { 0x940e }) |
                ((k >> 13) as u16 & 0x1f0) | ((k >> 16) as u16 & 1)
        }
        Ld(r,p) | St(p,r) => {
            reg("d/r", r)?;
            if overlap(r,p) { return Err(format!("register operand r{r} overlaps {p:?} (undefined)")); }
            let op = match p { Ptr::X => 0x900c, Ptr::XInc => 0x900d, Ptr::XDec => 0x900e,
                Ptr::Y => 0x8008, Ptr::YInc => 0x9009, Ptr::YDec => 0x900a,
                Ptr::Z => 0x8000, Ptr::ZInc => 0x9001, Ptr::ZDec => 0x9002 };
            op | (if matches!(i, St(..)) { 0x0200 } else { 0 }) | (r as u16) << 4
        }
        Ldd(r,y,q) | Std(y,q,r) => {
            let q = range("q", q as u32, 0, 63)?;
            (if matches!(i, Ldd(..)) { 0x8000 } else { 0x8200 }) |
                reg("d/r", r)? << 4 | (y as u16) << 3 |
                (q & 7) | (q & 0x18) << 7 | (q & 0x20) << 8
        }
        Lds(d,a) | Sts(a,d) => {
            second = a;
            (if matches!(i, Lds(..)) { 0x9000 } else { 0x9200 }) | reg("d/r", d)? << 4
        }
        Ijmp => 0x9409, Icall => 0x9509, Ret => 0x9508, Reti => 0x9518,
        Lpm => 0x95c8, Nop => 0, Sleep => 0x9588, Wdr => 0x95a8, Break => 0x9598,
    };
    Ok(([first, second], words(i)))
}

/// Decode without allocation, rejecting reserved and undefined operand forms.
pub fn decode(w: &[u16]) -> Option<(Insn, usize)> {
    use Insn::*;
    let a = *w.first()?;
    let d = ((a >> 4) & 31) as u8;
    let r = ((a & 15) | ((a >> 5) & 16)) as u8;
    let b = (a & 7) as u8;
    let simple = match a {
        0 => Some(Nop), 0x9409 => Some(Ijmp), 0x9509 => Some(Icall),
        0x9508 => Some(Ret), 0x9518 => Some(Reti), 0x95c8 => Some(Lpm),
        0x9588 => Some(Sleep), 0x95a8 => Some(Wdr), 0x9598 => Some(Break), _ => None,
    };
    if let Some(i) = simple { return Some((i,1)); }
    let pair = match a & 0xfc00 {
        0x0c00 => Some(Add(d,r)), 0x1c00 => Some(Adc(d,r)), 0x1800 => Some(Sub(d,r)),
        0x0800 => Some(Sbc(d,r)), 0x2000 => Some(And(d,r)), 0x2800 => Some(Or(d,r)),
        0x2400 => Some(Eor(d,r)), 0x2c00 => Some(Mov(d,r)), 0x1400 => Some(Cp(d,r)),
        0x0400 => Some(Cpc(d,r)), 0x1000 => Some(Cpse(d,r)), 0x9c00 => Some(Mul(d,r)),
        _ => None,
    };
    if let Some(i) = pair { return Some((i,1)); }
    let k = ((a & 15) | ((a >> 4) & 0xf0)) as u8;
    let upper = 16 + ((a >> 4) & 15) as u8;
    let rel = ((a << 4) as i16) >> 4;
    let immediate = match a & 0xf000 {
        0x3000 => Some(Cpi(upper,k)), 0x4000 => Some(Sbci(upper,k)),
        0x5000 => Some(Subi(upper,k)), 0x6000 => Some(Ori(upper,k)),
        0x7000 => Some(Andi(upper,k)), 0xe000 => Some(Ldi(upper,k)),
        0xc000 => Some(Rjmp(rel)), 0xd000 => Some(Rcall(rel)), _ => None,
    };
    if let Some(i) = immediate { return Some((i,1)); }
    if a & 0xd000 == 0x8000 {
        let q = ((a & 7) | ((a >> 7) & 0x18) | ((a >> 8) & 0x20)) as u8;
        let y = a & 8 != 0;
        return Some((if a & 0x200 == 0 { Ldd(d,y,q) } else { Std(y,q,d) },1));
    }
    if a & 0xf800 == 0xb000 || a & 0xf800 == 0xb800 {
        let io = ((a & 15) | ((a >> 5) & 0x30)) as u8;
        return Some((if a & 0x800 == 0 { In(d,io) } else { Out(io,d) },1));
    }
    if a & 0xfc00 == 0xf000 || a & 0xfc00 == 0xf400 {
        let k = (((a >> 3) as u8) << 1) as i8 >> 1;
        return Some((if a & 0x400 == 0 { Brbs(b,k) } else { Brbc(b,k) },1));
    }
    let i = match a & 0xff00 {
        0x0100 => Some(Movw(((a >> 4) & 15) as u8 * 2, (a & 15) as u8 * 2)),
        0x0200 => Some(Muls(upper,16 + (a & 15) as u8)),
        0x9600 | 0x9700 => {
            let d = 24 + ((a >> 4) & 3) as u8 * 2;
            let k = ((a & 15) | ((a >> 2) & 0x30)) as u8;
            Some(if a & 0x100 == 0 { Adiw(d,k) } else { Sbiw(d,k) })
        }
        0x9800 => Some(Cbi(((a >> 3) & 31) as u8,b)),
        0x9900 => Some(Sbic(((a >> 3) & 31) as u8,b)),
        0x9a00 => Some(Sbi(((a >> 3) & 31) as u8,b)),
        0x9b00 => Some(Sbis(((a >> 3) & 31) as u8,b)), _ => None,
    };
    if let Some(i) = i { return Some((i,1)); }
    let md = 16 + ((a >> 4) & 7) as u8;
    let mr = 16 + (a & 7) as u8;
    let i = match a & 0xff88 {
        0x0300 => Some(Mulsu(md,mr)), 0x0308 => Some(Fmul(md,mr)),
        0x0380 => Some(Fmuls(md,mr)), 0x0388 => Some(Fmulsu(md,mr)), _ => None,
    };
    if let Some(i) = i { return Some((i,1)); }
    if a & 0xfe0e == 0x940c || a & 0xfe0e == 0x940e {
        let k = *w.get(1)? as u32 | ((a as u32 & 1) << 16) | ((a as u32 & 0x1f0) << 13);
        return Some((if a & 2 == 0 { Jmp(k) } else { Call(k) },2));
    }
    if a & 0xff8f == 0x9408 || a & 0xff8f == 0x9488 {
        let b = ((a >> 4) & 7) as u8;
        return Some((if a & 0x80 == 0 { Bset(b) } else { Bclr(b) },1));
    }
    let i = match a & 0xfe08 {
        0xf800 => Some(Bld(d,b)), 0xfa00 => Some(Bst(d,b)),
        0xfc00 => Some(Sbrc(d,b)), 0xfe00 => Some(Sbrs(d,b)), _ => None,
    };
    if let Some(i) = i { return Some((i,1)); }
    let i = match a & 0xfe0f {
        0x9400 => Com(d), 0x9401 => Neg(d), 0x9402 => Swap(d), 0x9403 => Inc(d),
        0x940a => Dec(d), 0x9405 => Asr(d), 0x9406 => Lsr(d), 0x9407 => Ror(d),
        0x920f => Push(d), 0x900f => Pop(d), 0x9004 => LpmZ(d),
        0x9005 if d < 30 => LpmZInc(d),
        0x9000 => return Some((Lds(d,*w.get(1)?),2)),
        0x9200 => return Some((Sts(*w.get(1)?,d),2)),
        op => {
            let p = match op & !0x0200 {
                0x900c => Ptr::X, 0x900d => Ptr::XInc, 0x900e => Ptr::XDec,
                0x9009 => Ptr::YInc, 0x900a => Ptr::YDec,
                0x9001 => Ptr::ZInc, 0x9002 => Ptr::ZDec, _ => return None,
            };
            if overlap(d,p) { return None; }
            if op & 0x200 == 0 { Ld(d,p) } else { St(p,d) }
        }
    };
    Some((i,1))
}

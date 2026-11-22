//! Little-endian 16/24-bit instruction decoding from ISA chapters 8 and 9.
//!
//! No assembler, disassembler, emulator, or generated opcode source is used.
//! `ar`, `as`, `at` in the syntax below mean AR indexed by the named encoding
//! field; they do not imply a common destination position across instructions.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `ALL4 bt, bs` (pp. 337–338); bs must be a multiple of four.
    All4,
    /// `ALL8 bt, bs` (p. 338); bs must be a multiple of eight.
    All8,
    /// `ANDB br, bs, bt` (pp. 339–340).
    Andb,
    /// `ANDBC br, bs, bt` (p. 340).
    Andbc,
    /// `ANY4 bt, bs` (p. 341); bs must be a multiple of four.
    Any4,
    /// `ANY8 bt, bs` (pp. 341–342); bs must be a multiple of eight.
    Any8,
    /// `BF bs, label` (pp. 353–354).
    Bf,
    /// `BT bs, label` (pp. 371–372).
    Bt,
    /// `MOVF ar, as, bt` (pp. 501–502).
    Movf,
    /// `MOVT ar, as, bt` (pp. 514–515).
    Movt,
    /// `ORB br, bs, bt` (pp. 552–553).
    Orb,
    /// `ORBC br, bs, bt` (p. 553).
    Orbc,
    /// `XORB br, bs, bt` (pp. 652–653).
    Xorb,
    /// `ABS.S fr, fs` (pp. 325–326).
    AbsS,
    /// `ADD.S fr, fs, ft` (pp. 328–329).
    AddS,
    /// `ADDEXP.S fr, fs` (pp. 329–330).
    AddexpS,
    /// `ADDEXPM.S fr, fs` (pp. 331–332).
    AddexpmS,
    /// `CEIL.S ar, fs, 0..15` (p. 383); t is the scale exponent.
    CeilS,
    /// `CONST.S fr, 0..15` (pp. 386–387); s is the table index.
    ConstS,
    /// `DIV0.S fr, fs` (pp. 406–407); unspecified approximation.
    Div0S,
    /// `DIVN.S fr, fs, ft` (pp. 408–409); unspecified exponent interpretation.
    DivnS,
    /// `FLOAT.S fr, as, 0..15` (pp. 431–432); t is the scale exponent.
    FloatS,
    /// `FLOOR.S ar, fs, 0..15` (pp. 433–434); t is the scale exponent.
    FloorS,
    /// `LSI ft, as, 0..1020` (pp. 478–479).
    Lsi,
    /// `LSIP ft, as, 0..1020` (pp. 480–481).
    Lsip,
    /// `LSX fr, as, at` (pp. 482–483).
    Lsx,
    /// `LSXP fr, as, at` (pp. 483–484).
    Lsxp,
    /// `MADD.S fr, fs, ft` (pp. 486–487); one final rounding.
    MaddS,
    /// `MADDN.S fr, fs, ft` (p. 488); nearest, one rounding, no flags.
    MaddnS,
    /// `MKDADJ.S fr, fs` (pp. 493–494); unspecified divide_adjust helper.
    MkdadjS,
    /// `MKSADJ.S fr, fs` (pp. 494–495); unspecified square_root_adjust helper.
    MksadjS,
    /// `MOV.S fr, fs` (p. 498); non-arithmetic copy.
    MovS,
    /// `MOVEQZ.S fr, fs, at` (pp. 500–501).
    MoveqzS,
    /// `MOVF.S fr, fs, bt` (p. 503).
    MovfS,
    /// `MOVGEZ.S fr, fs, at` (pp. 505–506).
    MovgezS,
    /// `MOVLTZ.S fr, fs, at` (p. 510).
    MovltzS,
    /// `MOVNEZ.S fr, fs, at` (pp. 512–513).
    MovnezS,
    /// `MOVT.S fr, fs, bt` (p. 516).
    MovtS,
    /// `MSUB.S fr, fs, ft` (pp. 517–518); subtract the unrounded product.
    MsubS,
    /// `MUL.S fr, fs, ft` (pp. 522–523).
    MulS,
    /// `NEG.S fr, fs` (p. 541).
    NegS,
    /// `NEXP01.S fr, fs` (pp. 542–543).
    Nexp01S,
    /// `OEQ.S br, fs, ft` (pp. 547–548).
    OeqS,
    /// `OLE.S br, fs, ft` (pp. 549–550).
    OleS,
    /// `OLT.S br, fs, ft` (pp. 550–551).
    OltS,
    /// `RECIP0.S fr, fs` (pp. 560–561); unspecified approximation.
    Recip0S,
    /// `RFR ar, fs` (pp. 573–574).
    Rfr,
    /// `ROUND.S ar, fs, 0..15` (p. 580); t is the scale exponent.
    RoundS,
    /// `RSQRT0.S fr, fs` (p. 584); unspecified approximation.
    Rsqrt0S,
    /// `RUR.FCR ar` (pp. 317, 586–587); user register 232 in st.
    RurFcr,
    /// `RUR.FSR ar` (pp. 317, 586–587); user register 233 in st.
    RurFsr,
    /// `SQRT0.S fr, fs` (pp. 612–613); unspecified approximation.
    Sqrt0S,
    /// `SSI ft, as, 0..1020` (pp. 619–620).
    Ssi,
    /// `SSIP ft, as, 0..1020` (pp. 620–621).
    Ssip,
    /// `SSX fr, as, at` (p. 624).
    Ssx,
    /// `SSXP fr, as, at` (p. 625).
    Ssxp,
    /// `SUB.S fr, fs, ft` (pp. 628–629).
    SubS,
    /// `TRUNC.S ar, fs, 0..15` (pp. 632–633); t is the scale exponent.
    TruncS,
    /// `UEQ.S br, fs, ft` (pp. 634–635).
    UeqS,
    /// `UFLOAT.S fr, as, 0..15` (p. 636); t is the scale exponent.
    UfloatS,
    /// `ULE.S br, fs, ft` (pp. 637–638).
    UleS,
    /// `ULT.S br, fs, ft` (p. 639).
    UltS,
    /// `UN.S br, fs, ft` (pp. 641–642).
    UnS,
    /// `UTRUNC.S ar, fs, 0..15` (pp. 642–643); t is the scale exponent.
    UtruncS,
    /// `WFR fr, as` (pp. 646–647).
    Wfr,
    /// `WUR.FCR at` (pp. 317, 651–652); user register 232 in sr.
    WurFcr,
    /// `WUR.FSR at` (pp. 317, 651–652); user register 233 in sr.
    WurFsr,
    /// `ABS ar, at` (p. 324; the summary on p. 67 incorrectly says as).
    Abs,
    /// `MIN ar, as, at` (pp. 491–492), signed comparison.
    Min,
    /// `MAX ar, as, at` (p. 489), signed comparison.
    Max,
    /// `MINU ar, as, at` (pp. 491–492), unsigned comparison.
    Minu,
    /// `MAXU ar, as, at` (pp. 489–490), unsigned comparison.
    Maxu,
    /// `ADD ar, as, at`.
    Add,
    /// `ADD.N ar, as, at`.
    AddN,
    /// `ADDI at, as, imm8`.
    Addi,
    /// `ADDI.N ar, as, -1|1..15`; t encodes the immediate.
    AddiN,
    /// `ADDMI at, as, imm8<<8`.
    Addmi,
    /// `ADDX2 ar, as, at`.
    Addx2,
    /// `ADDX4 ar, as, at`.
    Addx4,
    /// `ADDX8 ar, as, at`.
    Addx8,
    /// `SUB ar, as, at`.
    Sub,
    /// `SUBX2 ar, as, at`.
    Subx2,
    /// `SUBX4 ar, as, at`.
    Subx4,
    /// `SUBX8 ar, as, at`.
    Subx8,
    /// `NEG ar, at`.
    Neg,
    /// `SALT ar, as, at` (signed; instruction diagram p. 599).
    Salt,
    /// `SALTU ar, as, at` (unsigned; instruction diagram p. 600).
    Saltu,
    /// `AND ar, as, at`.
    And,
    /// `OR ar, as, at` (also the assembler's MOV pseudo-instruction, p. 496).
    Or,
    /// `XOR ar, as, at`.
    Xor,
    /// `BALL as, at, label`.
    Ball,
    /// `BANY as, at, label`.
    Bany,
    /// `BNALL as, at, label`.
    Bnall,
    /// `BNONE as, at, label`.
    Bnone,
    /// `BBC as, at, label` (bit number is AR[t] & 31).
    Bbc,
    /// `BBS as, at, label` (bit number is AR[t] & 31).
    Bbs,
    /// `BBCI as, bbi, label`; bbi = ((r & 1)<<4) | t.
    Bbci,
    /// `BBSI as, bbi, label`; bbi = ((r & 1)<<4) | t.
    Bbsi,
    /// `BEQ as, at, label`.
    Beq,
    /// `BNE as, at, label`.
    Bne,
    /// `BGE as, at, label`.
    Bge,
    /// `BLT as, at, label`.
    Blt,
    /// `BGEU as, at, label`.
    Bgeu,
    /// `BLTU as, at, label`.
    Bltu,
    /// `BEQI as, b4const, label`; r selects the table constant.
    Beqi,
    /// `BNEI as, b4const, label`; r selects the table constant.
    Bnei,
    /// `BGEI as, b4const, label`; r selects the table constant.
    Bgei,
    /// `BLTI as, b4const, label`; r selects the table constant.
    Blti,
    /// `BGEUI as, b4constu, label`; r selects the table constant.
    Bgeui,
    /// `BLTUI as, b4constu, label`; r selects the table constant.
    Bltui,
    /// `BEQZ as, label`.
    Beqz,
    /// `BNEZ as, label`.
    Bnez,
    /// `BGEZ as, label`.
    Bgez,
    /// `BLTZ as, label`.
    Bltz,
    /// `BEQZ.N as, label` (forward offset).
    BeqzN,
    /// `BNEZ.N as, label` (forward offset).
    BnezN,
    /// `MOVI at, imm12`; s contains the high immediate nibble.
    Movi,
    /// `MOVI.N as, -32..95`.
    MoviN,
    /// `MOV.N at, as`.
    MovN,
    /// `MOVEQZ ar, as, at`.
    Moveqz,
    /// `MOVNEZ ar, as, at`.
    Movnez,
    /// `MOVGEZ ar, as, at`.
    Movgez,
    /// `MOVLTZ ar, as, at`.
    Movltz,
    /// `EXTUI ar, at, shiftimm, maskimm` (p. 429).
    Extui,
    /// `SLL ar, as`.
    Sll,
    /// `SRL ar, at`.
    Srl,
    /// `SRA ar, at`.
    Sra,
    /// `SRC ar, as, at`.
    Src,
    /// `SLLI ar, as, 1..31`; imm is sa, the funnel-right amount in the
    /// operation on p. 611 (32 minus the assembler's left shift amount).
    Slli,
    /// `SRLI ar, at, 0..15`; s encodes the shift amount.
    Srli,
    /// `SRAI ar, at, 0..31`; s encodes the low four amount bits.
    Srai,
    /// `SSL as`.
    Ssl,
    /// `SSR as`.
    Ssr,
    /// `SSA8L as`.
    Ssa8l,
    /// `SSA8B as` (available even in a little-endian processor).
    Ssa8b,
    /// `SSAI 0..31`; sa = (t<<4) | s.
    Ssai,
    /// `L8UI at, as, offset`.
    L8ui,
    /// `L16UI at, as, offset`.
    L16ui,
    /// `L16SI at, as, offset`.
    L16si,
    /// `L32I at, as, offset`.
    L32i,
    /// `L32I.N at, as, offset`; r is the unscaled immediate nibble.
    L32iN,
    /// `L32R at, label`; imm is the always-negative byte displacement.
    L32r,
    /// `S8I at, as, offset`.
    S8i,
    /// `S16I at, as, offset`.
    S16i,
    /// `S32I at, as, offset`.
    S32i,
    /// `S32I.N at, as, offset`; r is the unscaled immediate nibble.
    S32iN,
    /// `J label` (CALL format, n=0).
    J,
    /// `JX as` (CALLX format, no t field).
    Jx,
    /// `CALL0 label` (CALL format, n stored in t).
    Call0,
    /// `CALLX0 as` (CALLX format, no t field).
    Callx0,
    /// `RET` (CALLX format).
    Ret,
    /// `RET.N` (RRRN format).
    RetN,
    /// `MULL ar, as, at`.
    Mull,
    /// `MULUH ar, as, at` (requires Config::mul32_high).
    Muluh,
    /// `MULSH ar, as, at` (requires Config::mul32_high).
    Mulsh,
    /// `QUOS ar, as, at` (requires Config::div32).
    Quos,
    /// `QUOU ar, as, at` (requires Config::div32).
    Quou,
    /// `REMS ar, as, at` (requires Config::div32).
    Rems,
    /// `REMU ar, as, at` (requires Config::div32).
    Remu,
    /// `ISYNC`.
    Isync,
    /// `RSYNC`.
    Rsync,
    /// `ESYNC`.
    Esync,
    /// `DSYNC`.
    Dsync,
    /// `FSYNC`; s contains 8 | imm3, PDF p. 434.
    Fsync,
    /// `MEMW`.
    Memw,
    /// `EXTW`.
    Extw,
    /// `EXCW` (Exception Option 2, pp. 128, 428–429).
    Excw,
    /// `NOP`.
    Nop,
    /// `NOP.N`.
    NopN,
    /// `ILL` (CALLX format).
    Ill,
    /// `ILL.N`.
    IllN,
    /// `BREAK s, t`.
    Break,
    /// `BREAK.N s`; all 16 immediates are allowed by p. 371.
    BreakN,
    /// `SYSCALL`.
    Syscall,
    /// `RSR at, sr`; imm is sr and r=s=0 (RSR format).
    Rsr,
    /// Thread Pointer user register 231, pp. 255, 316.
    RurThreadptr,
    /// Thread Pointer user register 231, pp. 255, 316.
    WurThreadptr,
    /// `WSR at, sr`; imm is sr and r=s=0 (RSR format).
    Wsr,
    /// `XSR at, sr`; imm is sr and r=s=0 (RSR format).
    Xsr,
    /// `RSIL at, level`; s encodes the level.
    Rsil,
    /// `WAITI level`; s encodes the level.
    Waiti,
    /// `RFE`.
    Rfe,
    /// `RFDE` (decodes, execution is unmodeled).
    Rfde,
    /// `RFDO` (decodes, execution is unmodeled).
    Rfdo,
    /// `RFDD` (decodes, execution is unmodeled; s is 0 or 1, p. 667).
    Rfdd,
    /// `RFI level` (decodes, execution is unmodeled; level is s/imm).
    Rfi,
    /// `CALL4 label` (windowed, n stored in t).
    Call4,
    /// `CALL8 label` (windowed, n stored in t).
    Call8,
    /// `CALL12 label` (windowed, n stored in t).
    Call12,
    /// `CALLX4 as` (windowed, CALLX format, no t field).
    Callx4,
    /// `CALLX8 as` (windowed, CALLX format, no t field).
    Callx8,
    /// `CALLX12 as` (windowed, CALLX format, no t field).
    Callx12,
    /// `ENTRY as, imm12<<3` (windowed, BRI12 format).
    Entry,
    /// `RETW` (windowed, CALLX format).
    Retw,
    /// `RETW.N` (windowed).
    RetwN,
    /// `MOVSP at, as` (windowed).
    Movsp,
    /// `ROTW -8..7` (windowed; t encodes the signed immediate).
    Rotw,
    /// `L32E at, as, -64..-4` (windowed; r encodes the offset).
    L32e,
    /// `S32E at, as, -64..-4` (windowed; r encodes the offset).
    S32e,
    /// `RFWO` (windowed).
    Rfwo,
    /// `RFWU` (windowed).
    Rfwu,
}

/// Decoded chapter-9 fields and chapter-8 immediate.
///
/// r/s/t retain the named format fields, including fixed opcode nibbles and
/// immediate pieces. Absent fields are zero; CALL stores n in t. CALLX has
/// no t field. Narrow instructions retain their own operations. `imm` is
/// sign-extended/scaled as used by the operation; for immediate branches it
/// is B4CONST/B4CONSTU, for special/user-register operations the register number,
/// and for EXTUI it is shiftimm while `r` holds the encoded mask field op2
/// (mask width minus one). SLLI uses the operation's funnel-right sa (p. 611).
/// FP conversions put the scale exponent t in imm; CONST.S puts its index s
/// there. RUR retains r/s/t; WUR follows RSR's absent-r/s convention.
///
/// The extra `bits` field is necessary: the requested immediate-branch
/// convention otherwise loses imm8 (pp. 350–351, 355–362), and replacing
/// EXTUI's r with its mask otherwise loses its destination (p. 429).
/// It holds only the instruction's low 16 or 24 bits, and is consulted for
/// those missing operands. Construct instructions through `decode` so the
/// encoding and decoded fields remain consistent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    pub op: Op,
    pub r: u8,
    pub s: u8,
    pub t: u8,
    pub imm: i32,
    pub bits: u32,
}

fn sext(value: u32, width: u32) -> i32 {
    ((value << (32 - width)) as i32) >> (32 - width)
}

// Tables 15 and 16, pp. 64–65. The unsigned table's first TWO entries differ.
const B4CONST: [i32; 16] = [-1, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256];
const B4CONSTU: [i32; 16] = [32768, 65536, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 32, 64, 128, 256];

fn branch_offset(offset: i32) -> Result<i32, ()> {
    // §3.8.5 p. 62 reserves -3..=0 for the wide conditional branches.
    // Narrow branches explicitly allow unsigned 0..63 (pp. 353, 368).
    if (-3..=0).contains(&offset) { Err(()) } else { Ok(offset) }
}

fn special_access(op: Op, sr: i32) -> bool {
    use Op::*;
    match sr {
        0..=4 | 12 | 16 | 17 | 32..=35 | 72 | 73 | 177 | 209 | 224 | 228 | 230..=232 | 234 | 238 | 240..=242 => true,
        // Tables 172–174, pp. 303–304: neither alias permits XSR.
        // RSR.INTCLEAR is legal with an undefined result.
        226 | 227 => op != Xsr,
        // Table 184, pp. 310–311: WSR reserved, XSR not legal.
        235 => op == Rsr,
        _ => false,
    }
}

/// Decode one little-endian instruction, ignoring bits above its length.
///
/// This is a structural decoder: optional high multiply/divide instructions
/// decode here and are checked against Config by the CPU. Illegal register
/// accesses and encodings of excluded options are rejected here. Opcode maps:
/// Tables 198–259, pp. 661–680; formats: PDF diagrams pp. 656–659.
pub fn decode(bits: u32) -> Result<(Insn, u32), ()> {
    use Op::*;
    let bits = bits & 0x00ff_ffff;
    let op0 = bits & 15;
    let t = ((bits >> 4) & 15) as u8;
    let s = ((bits >> 8) & 15) as u8;
    let r = ((bits >> 12) & 15) as u8;
    let op1 = (bits >> 16) & 15;
    let op2 = bits >> 20;
    let imm8 = bits >> 16;
    let mut i = Insn { op: Ill, r, s, t, imm: 0, bits };
    let length = if op0 >= 8 { 2 } else { 3 };
    if length == 2 {
        i.bits &= 0xffff;
    }
    i.op = match op0 {
        0 => match (op1, op2) {
            (0, 0) => match r {
                0 => {
                    // CALLX divides the would-be t nibble into m and n.
                    i.t = 0;
                    match (t >> 2, t & 3) {
                        (0, 0) if s == 0 => Ill,
                        (2, 0) if s == 0 => Ret,
                        (2, 1) if s == 0 => Retw,
                        (2, 2) => Jx,
                        (3, 0) => Callx0,
                        (3, 1) => Callx4,
                        (3, 2) => Callx8,
                        (3, 3) => Callx12,
                        _ => return Err(()),
                    }
                }
                1 => Movsp,
                2 => match (s, t) {
                    (0, 0) => Isync,
                    (0, 1) => Rsync,
                    (0, 2) => Esync,
                    (0, 3) => Dsync,
                    (0, 8) => Excw,
                    (0, 12) => Memw,
                    (0, 13) => Extw,
                    (0, 15) => Nop,
                    (8..=15, 0) => { i.imm = (s & 7) as i32; Fsync }
                    _ => return Err(()),
                },
                3 => match (s, t) {
                    (0, 0) => Rfe,
                    (2, 0) => Rfde,
                    (4, 0) => Rfwo,
                    (5, 0) => Rfwu,
                    // RFI's level range is configuration-dependent (pp.
                    // 571–572); levels 0/1 are undefined. The brief requires
                    // a named stop for RFI, so retain all 4-bit levels here.
                    (_, 1) => { i.imm = s as i32; Rfi }
                    _ => return Err(()),
                },
                4 => Break,
                5 if s == 0 && t == 0 => Syscall,
                6 => { i.imm = s as i32; Rsil }
                7 if t == 0 => { i.imm = s as i32; Waiti }
                8 => Any4,
                9 => All4,
                10 => Any8,
                11 => All8,
                _ => return Err(()),
            },
            (0, 1) => And,
            (0, 2) => Or,
            (0, 3) => Xor,
            (0, 4) => match (r, t) {
                (0, 0) => Ssr,
                (1, 0) => Ssl,
                (2, 0) => Ssa8l,
                (3, 0) => Ssa8b,
                // Table 213 p. 665 says t=0; p. 618's diagram and prose
                // explicitly use bit 4 for sa[4]. Follow the instruction.
                (4, 0..=1) => { i.imm = (s | (t << 4)) as i32; Ssai }
                (8, _) if s == 0 => { i.imm = sext(t as u32, 4); Rotw }
                _ => return Err(()),
            },
            (0, 6) if s == 0 => Neg,
            (0, 6) if s == 1 => Abs,
            (0, 8) => Add,
            (0, 9) => Addx2,
            (0, 10) => Addx4,
            (0, 11) => Addx8,
            (0, 12) => Sub,
            (0, 13) => Subx2,
            (0, 14) => Subx4,
            (0, 15) => Subx8,
            (1, 0..=1) => {
                i.imm = (t as u32 | ((op2 & 1) << 4)) as i32;
                // sa=0 is undefined (p. 611); choose rejection.
                if i.imm == 0 { return Err(()); }
                Slli
            }
            (1, 2..=3) => { i.imm = (s as u32 | ((op2 & 1) << 4)) as i32; Srai }
            (1, 4) => { i.imm = s as i32; Srli }
            (1, 6) => {
                i.imm = ((bits >> 8) & 255) as i32;
                i.r = 0; i.s = 0;
                Xsr
            }
            (1, 8) => Src,
            (1, 9) if s == 0 => Srl,
            (1, 10) if t == 0 => Sll,
            (1, 11) if s == 0 => Sra,
            (1, 15) if r == 14 && s == 0 && t == 0 => Rfdo,
            (1, 15) if r == 14 && s <= 1 && t == 1 => Rfdd,
            (2, 0) => Andb,
            (2, 1) => Andbc,
            (2, 2) => Orb,
            (2, 3) => Orbc,
            (2, 4) => Xorb,
            // Table 220 p. 668 labels both entries SALT. The instruction
            // diagrams pp. 599–600 distinguish signed 7 from unsigned 6.
            (2, 6) => Saltu,
            (2, 7) => Salt,
            (2, 8) => Mull,
            (2, 10) => Muluh,
            (2, 11) => Mulsh,
            (2, 12) => Quou,
            (2, 13) => Quos,
            (2, 14) => Remu,
            (2, 15) => Rems,
            (3, 0..=1) => {
                i.imm = ((bits >> 8) & 255) as i32;
                i.r = 0; i.s = 0;
                if op2 == 0 { Rsr } else { Wsr }
            }
            // MIN/MAX instruction diagrams pp. 489–492; RST3 p. 668.
            (3, 4) => Min,
            (3, 5) => Max,
            (3, 6) => Minu,
            (3, 7) => Maxu,
            (3, 8) => Moveqz,
            (3, 9) => Movnez,
            (3, 10) => Movltz,
            (3, 11) => Movgez,
            // MOVF's diagram p. 501 duplicates MOVEQZ (op2=8). Use
            // Table 221 p. 668's unambiguous MOVF/MOVT entries C/D.
            (3, 12) => Movf,
            (3, 13) => Movt,
            (3, 14) => {
                i.imm = (16 * s as i32) + t as i32; // RUR st, p. 587.
                match i.imm { 231 => RurThreadptr, 232 => RurFcr, 233 => RurFsr, _ => return Err(()) }
            }
            // WUR's diagram p. 651 incorrectly repeats RUR's op2=E;
            // Table 221 p. 668 gives F. Its RSR layout and Operation agree.
            (3, 15) => {
                i.imm = ((bits >> 8) & 255) as i32;
                i.r = 0; i.s = 0;
                match i.imm { 231 => WurThreadptr, 232 => WurFcr, 233 => WurFsr, _ => return Err(()) }
            }
            (4..=5, _) => {
                i.imm = (s as u32 | ((op1 & 1) << 4)) as i32;
                if i.imm as u32 + op2 > 31 { return Err(()); } // p. 429
                i.r = op2 as u8; // encoded mask field, NOT destination
                Extui
            }
            (9, 0) => { i.imm = ((r as i32) - 16) * 4; L32e }
            (9, 4) => { i.imm = ((r as i32) - 16) * 4; S32e }
            // Table 222 calls these LSXU/SSXU. This is the current FP
            // option: use LSXP/SSXP and their post-increment Operations
            // (pp. 483–484, 625), not the FP2000 option's update Operations.
            (8, 0) => Lsx,
            (8, 1) => Lsxp,
            (8, 4) => Ssx,
            (8, 5) => Ssxp,
            (10, 0) => AddS,
            (10, 1) => SubS,
            (10, 2) => MulS,
            (10, 4) => MaddS,
            (10, 5) => MsubS,
            (10, 6) => MaddnS,
            (10, 7) => DivnS,
            (10, 8..=14) => {
                i.imm = t as i32;
                match op2 {
                    8 => RoundS, 9 => TruncS, 10 => FloorS, 11 => CeilS,
                    12 => FloatS, 13 => UfloatS, _ => UtruncS,
                }
            }
            (10, 15) => match t {
                0 => MovS, 1 => AbsS,
                3 => { i.imm = s as i32; ConstS }
                // WFR p. 647 prints t=2, colliding with CVTD.S p. 388.
                // Table 228 p. 671 assigns WFR t=5; use that unique slot.
                4 => Rfr, 5 => Wfr, 6 => NegS, 7 => Div0S,
                8 => Recip0S, 9 => Sqrt0S, 10 => Rsqrt0S, 11 => Nexp01S,
                12 => MksadjS, 13 => MkdadjS, 14 => AddexpS, 15 => AddexpmS,
                _ => return Err(()), // CVTD.S needs DoublePrecision.
            },
            (11, 1) => UnS,
            (11, 2) => OeqS,
            (11, 3) => UeqS,
            (11, 4) => OltS,
            (11, 5) => UltS,
            (11, 6) => OleS,
            (11, 7) => UleS,
            (11, 8) => MoveqzS,
            (11, 9) => MovnezS,
            (11, 10) => MovltzS,
            (11, 11) => MovgezS,
            // MOVF.S p. 503 similarly duplicates MOVEQZ.S. Table 229
            // p. 671 distinguishes them; MOVT.S p. 516 also confirms D.
            (11, 12) => MovfS,
            (11, 13) => MovtS,
            _ => return Err(()),
        },
        1 => {
            // p. 461's heading says RI6, but its 16-bit literal field and
            // the RI16 diagram on p. 657 agree on this layout.
            i.r = 0; i.s = 0;
            i.imm = (((bits >> 8) as i32) - 65536) * 4;
            L32r
        }
        2 => match r {
            0 => { i.imm = imm8 as i32; L8ui }
            1 => { i.imm = (imm8 * 2) as i32; L16ui }
            2 => { i.imm = (imm8 * 4) as i32; L32i }
            4 => { i.imm = imm8 as i32; S8i }
            5 => { i.imm = (imm8 * 2) as i32; S16i }
            6 => { i.imm = (imm8 * 4) as i32; S32i }
            9 => { i.imm = (imm8 * 2) as i32; L16si }
            10 => { i.imm = sext(((s as u32) << 8) | imm8, 12); Movi }
            12 => { i.imm = sext(imm8, 8); Addi }
            13 => { i.imm = sext(imm8, 8) * 256; Addmi }
            _ => return Err(()),
        },
        3 => {
            i.imm = (imm8 * 4) as i32;
            // Table 237 p. 674 uses FP2000's LSIU/SSIU names. Current
            // instruction diagrams pp. 480, 620 give LSIP/SSIP here.
            match r { 0 => Lsi, 4 => Ssi, 8 => Lsip, 12 => Ssip, _ => return Err(()) }
        }
        5 => {
            i.r = 0; i.s = 0; i.t = t & 3;
            i.imm = sext(bits >> 6, 18) * 4;
            match i.t { 0 => Call0, 1 => Call4, 2 => Call8, _ => Call12 }
        }
        6 => {
            let n = t & 3;
            let m = t >> 2;
            i.t = 0;
            match n {
                0 => {
                    i.r = 0; i.s = 0;
                    i.imm = sext(bits >> 6, 18);
                    J
                }
                1 => {
                    i.r = 0;
                    i.imm = branch_offset(sext(bits >> 12, 12))?;
                    match m { 0 => Beqz, 1 => Bnez, 2 => Bltz, _ => Bgez }
                }
                2 => {
                    branch_offset(sext(imm8, 8))?;
                    i.imm = B4CONST[r as usize];
                    match m { 0 => Beqi, 1 => Bnei, 2 => Blti, _ => Bgei }
                }
                3 if m == 0 => {
                    i.r = 0;
                    i.imm = ((bits >> 12) * 8) as i32;
                    Entry
                }
                3 if m >= 2 => {
                    branch_offset(sext(imm8, 8))?;
                    i.imm = B4CONSTU[r as usize];
                    if m == 2 { Bltui } else { Bgeui }
                }
                3 if m == 1 && r <= 1 => {
                    // BF/BT use RRI8, not the BRI8 sub-fields (pp. 353, 372).
                    i.t = t;
                    i.imm = branch_offset(sext(imm8, 8))?;
                    if r == 0 { Bf } else { Bt }
                }
                _ => return Err(()), // Loop excluded.
            }
        }
        7 => {
            i.imm = branch_offset(sext(imm8, 8))?;
            match r {
                0 => Bnone, 1 => Beq, 2 => Blt, 3 => Bltu,
                4 => Ball, 5 => Bbc, 6..=7 => Bbci,
                8 => Bany, 9 => Bne, 10 => Bge, 11 => Bgeu,
                12 => Bnall, 13 => Bbs, 14..=15 => Bbsi,
                _ => unreachable!(),
            }
        }
        8 => { i.imm = (r as i32) * 4; L32iN }
        9 => { i.imm = (r as i32) * 4; S32iN }
        10 => AddN,
        11 => { i.imm = if t == 0 { -1 } else { t as i32 }; AddiN }
        12 => {
            // Table 25 p. 83 calls narrow branches RI16. The individual
            // instruction on p. 352 and the RI6 diagram p. 659 take priority.
            i.r = 0; i.t = 0;
            if t & 8 == 0 {
                let value = ((t & 7) << 4) | r;
                i.imm = if value >= 96 { value as i32 - 128 } else { value as i32 };
                MoviN
            } else {
                i.imm = (((t & 3) << 4) | r) as i32;
                if t & 4 == 0 { BeqzN } else { BnezN }
            }
        }
        13 => match (r, s, t) {
            (0, _, _) => MovN,
            (15, 0, 0) => RetN,
            (15, 0, 1) => RetwN,
            // Table 258 p. 680 says s=0, contradicting the immediate field
            // and syntax on p. 371. Follow the instruction's diagram.
            (15, _, 2) => BreakN,
            (15, 0, 3) => NopN,
            (15, 0, 6) => IllN,
            _ => return Err(()),
        },
        _ => return Err(()),
    };
    if matches!(i.op, Rsr | Wsr | Xsr) && !special_access(i.op, i.imm) {
        return Err(());
    }
    Ok((i, length))
}

/// Address referenced by a PC-relative instruction, modulo 2^32.
/// There are no Loop operations in this core. Register-indirect jumps/calls
/// have no PC-relative target and return None.
pub fn target(insn: &Insn, pc: u32) -> Option<u32> {
    use Op::*;
    let (base, offset) = match insn.op {
        L32r => (pc.wrapping_add(3) & !3, insn.imm), // pp. 461–462
        Call0 | Call4 | Call8 | Call12 => ((pc & !3).wrapping_add(4), insn.imm),
        Beqi | Bnei | Bgei | Blti | Bgeui | Bltui =>
            (pc.wrapping_add(4), sext((insn.bits >> 16) & 255, 8)),
        J | Ball | Bany | Bnall | Bnone | Bbc | Bbs | Bbci | Bbsi
        | Beq | Bne | Bge | Blt | Bgeu | Bltu | Beqz | Bnez | Bgez | Bltz
        | BeqzN | BnezN | Bf | Bt => (pc.wrapping_add(4), insn.imm),
        _ => return None,
    };
    Some(base.wrapping_add(offset as u32))
}

use fab_elec_mcu_avr::{
    asm::{Asm, Target},
    insn::{cycles, decode, encode, words, Insn, Ptr, Reg},
    sim::{Cpu, Event, Stop, FLASH_WORDS, IDLE_WAKE_CYCLES},
};
use Insn::*;

fn canonical(i: Insn) -> Insn {
    match i {
        Ld(r,Ptr::Y) => Ldd(r,true,0), Ld(r,Ptr::Z) => Ldd(r,false,0),
        St(Ptr::Y,r) => Std(true,0,r), St(Ptr::Z,r) => Std(false,0,r), _ => i,
    }
}

fn round_trip(i: Insn) {
    let (encoded,n) = encode(&i).unwrap();
    assert_eq!(words(&i),n,"{i:?}");
    assert_eq!(decode(&encoded[..n]),Some((canonical(i),n)),"{i:?}");
    if n == 2 { assert_eq!(decode(&encoded[..1]),None,"short {i:?}"); }
}

#[test]
fn encodings() {
    let pairs: [fn(Reg,Reg) -> Insn; 12] = [Add,Adc,Sub,Sbc,And,Or,Eor,Mov,Cp,Cpc,Cpse,Mul];
    for op in pairs {
        for d in [0,1,15,16,30,31] {
            for r in [0,1,15,16,30,31] { round_trip(op(d,r)); }
        }
        assert!(encode(&op(32,0)).is_err());
        assert!(encode(&op(0,32)).is_err());
    }
    for d in [16,17,23,31] { for r in [16,22,31] { round_trip(Muls(d,r)); } }
    let restricted: [fn(Reg,Reg) -> Insn; 4] = [Mulsu,Fmul,Fmuls,Fmulsu];
    for op in restricted {
        for d in [16,19,23] { for r in [16,20,23] { round_trip(op(d,r)); } }
        for (d,r) in [(15,16),(16,15),(24,16),(16,24)] { assert!(encode(&op(d,r)).is_err()); }
    }
    for d in [0,2,16,30] { for r in [0,2,16,30] { round_trip(Movw(d,r)); } }
    let immediates: [fn(Reg,u8) -> Insn; 6] = [Subi,Sbci,Andi,Ori,Cpi,Ldi];
    for op in immediates {
        for d in [16,23,31] { for k in [0,1,0x7f,0xff] { round_trip(op(d,k)); } }
        assert!(encode(&op(15,0)).is_err()); assert!(encode(&op(32,0)).is_err());
    }
    for d in [24,26,28,30] { for k in [0,1,31,63] { round_trip(Adiw(d,k)); round_trip(Sbiw(d,k)); } }
    let singles: [fn(Reg) -> Insn; 12] = [Com,Neg,Swap,Inc,Dec,Asr,Lsr,Ror,Push,Pop,LpmZ,LpmZInc];
    for op in singles {
        for d in [0,1,16,29,31] {
            if !matches!(op(d),LpmZInc(31)) { round_trip(op(d)); }
        }
        assert!(encode(&op(32)).is_err());
    }
    let bits: [fn(Reg,u8) -> Insn; 4] = [Bst,Bld,Sbrc,Sbrs];
    for op in bits {
        for d in [0,17,31] { for b in [0,3,7] { round_trip(op(d,b)); } }
        assert!(encode(&op(32,0)).is_err()); assert!(encode(&op(0,8)).is_err());
    }
    let io_bits: [fn(u8,u8) -> Insn; 4] = [Sbic,Sbis,Sbi,Cbi];
    for op in io_bits {
        for a in [0,15,31] { for b in [0,4,7] { round_trip(op(a,b)); } }
        assert!(encode(&op(32,0)).is_err()); assert!(encode(&op(0,8)).is_err());
    }
    for b in [0,3,7] {
        round_trip(Bset(b)); round_trip(Bclr(b));
        for k in [-64,-1,0,1,63] { round_trip(Brbs(b,k)); round_trip(Brbc(b,k)); }
    }
    for k in [-2048,-1,0,1,2047] { round_trip(Rjmp(k)); round_trip(Rcall(k)); }
    for k in [0,0x1234,0xffff,0x10000,0x2aaaaa,0x3fffff] { round_trip(Jmp(k)); round_trip(Call(k)); }
    for r in [0,15,31] { for a in [0,31,32,63] { round_trip(In(r,a)); round_trip(Out(a,r)); } }
    for p in [Ptr::X,Ptr::XInc,Ptr::XDec,Ptr::Y,Ptr::YInc,Ptr::YDec,Ptr::Z,Ptr::ZInc,Ptr::ZDec] {
        for r in [0,15,25,26,27,28,29,30,31] {
            let bad = match p {
                Ptr::XInc | Ptr::XDec => r == 26 || r == 27,
                Ptr::YInc | Ptr::YDec => r == 28 || r == 29,
                Ptr::ZInc | Ptr::ZDec => r == 30 || r == 31, _ => false,
            };
            if bad {
                assert!(encode(&Ld(r,p)).is_err()); assert!(encode(&St(p,r)).is_err());
                // Form the otherwise valid encoding by replacing r0's field.
                for i in [Ld(0,p),St(p,0)] {
                    let w = encode(&i).unwrap().0[0] | (r as u16) << 4;
                    assert_eq!(decode(&[w]),None,"undefined {p:?}, r{r}");
                }
            } else { round_trip(Ld(r,p)); round_trip(St(p,r)); }
        }
    }
    for r in [0,16,31] {
        for y in [false,true] { for q in [0,1,7,8,31,32,63] { round_trip(Ldd(r,y,q)); round_trip(Std(y,q,r)); } }
        for a in [0,0x100,0x8ff,0xffff] { round_trip(Lds(r,a)); round_trip(Sts(a,r)); }
    }
    for i in [Ijmp,Icall,Ret,Reti,Lpm,Nop,Sleep,Wdr,Break] { round_trip(i); }

    for (i,w,n) in [
        (Ldi(16,0xff),[0xef0f,0],1), (Rjmp(-1),[0xcfff,0],1),
        (Out(5,16),[0xb905,0],1), (Sbi(4,5),[0x9a25,0],1),
        (Call(0x1234),[0x940e,0x1234],2), (Ret,[0x9508,0],1),
        (Adiw(24,1),[0x9601,0],1), (Ld(0,Ptr::XInc),[0x900d,0],1),
        (Std(true,5,3),[0x823d,0],1), (LpmZInc(1),[0x9015,0],1),
    ] { assert_eq!(encode(&i).unwrap(),(w,n),"manual {i:?}"); }

    for i in [Muls(15,16),Muls(16,32),Movw(1,0),Movw(0,31),Movw(32,0),
        Adiw(22,1),Adiw(25,1),Sbiw(31,1),Adiw(24,64),Sbiw(24,255),
        Bset(8),Bclr(255),Brbs(8,0),Brbc(8,0),Brbs(0,-65),Brbc(0,64),
        Rjmp(-2049),Rjmp(2048),Rcall(-2049),Rcall(2048),Jmp(0x400000),Call(u32::MAX),
        In(0,64),Out(64,0),In(32,0),Out(0,32),Ld(32,Ptr::X),St(Ptr::Z,32),
        Ldd(32,false,0),Ldd(0,true,64),Std(false,64,0),Std(true,0,32),
        Lds(32,0),Sts(0,32),LpmZInc(30),LpmZInc(31)] {
        assert!(encode(&i).is_err(),"invalid {i:?}");
    }
    assert_eq!(decode(&[]),None);
    for w in [0xffff,0x95e8,0x95f8,0x9006,0x9007,0x95d8,0x9419,0x9519,0x91e5,0x91f5] {
        assert_eq!(decode(&[w,0]),None,"unsupported/undefined {w:04x}");
    }
    for first in 0..=u16::MAX {
        let source = [first,0xa55a];
        if let Some((i,n)) = decode(&source) {
            let (encoded,m) = encode(&i).unwrap();
            assert_eq!(m,n,"length {first:04x}");
            assert_eq!(&encoded[..m],&source[..n],"canonical {first:04x}: {i:?}");
        }
    }
}

fn program(insns: &[Insn]) -> Vec<u16> {
    let mut a = Asm::new();
    for i in insns { a.emit(*i); }
    a.finish().unwrap()
}

#[test]
fn cycle_table_matches_oracle() {
    // Independent AVRe+ manual counts, with valid memory/stack operands. Execute
    // exactly one instruction in each fresh CPU, including terminal instructions.
    let fixed: &[(&[Insn],u8)] = &[
        (&[Add(16,17),Adc(16,17),Sub(16,17),Sbc(16,17),And(16,17),Or(16,17),
            Eor(16,17),Mov(16,17),Cp(16,17),Cpc(16,17),Movw(18,16),
            Subi(16,1),Sbci(16,1),Andi(16,1),Ori(16,1),Cpi(16,1),Ldi(16,1),
            Com(16),Neg(16),Swap(16),Inc(16),Dec(16),Asr(16),Lsr(16),Ror(16),
            Bset(0),Bclr(0),Bst(16,0),Bld(16,0),In(16,0x1e),Out(0x1e,16),
            Nop,Sleep,Wdr,Break],1),
        (&[Mul(16,17),Muls(16,17),Mulsu(16,17),Fmul(16,17),Fmuls(16,17),Fmulsu(16,17),
            Adiw(24,1),Sbiw(24,1),Sbi(5,0),Cbi(5,0),Rjmp(0),Ijmp,
            Ld(16,Ptr::X),Ld(16,Ptr::XInc),Ld(16,Ptr::XDec),
            Ld(16,Ptr::Y),Ld(16,Ptr::YInc),Ld(16,Ptr::YDec),
            Ld(16,Ptr::Z),Ld(16,Ptr::ZInc),Ld(16,Ptr::ZDec),
            St(Ptr::X,16),St(Ptr::XInc,16),St(Ptr::XDec,16),
            St(Ptr::Y,16),St(Ptr::YInc,16),St(Ptr::YDec,16),
            St(Ptr::Z,16),St(Ptr::ZInc,16),St(Ptr::ZDec,16),
            Ldd(16,true,3),Ldd(16,false,3),Std(true,3,16),Std(false,3,16),
            Lds(16,0x100),Sts(0x100,16),Push(16),Pop(16)],2),
        (&[Rcall(0),Jmp(0),Icall,Lpm,LpmZ(16),LpmZInc(16)],3),
        (&[Call(0),Ret,Reti],4),
    ];
    for &(insns,expected) in fixed {
        for &insn in insns {
            assert_eq!(cycles(&insn),(expected,0),"manual {insn:?}");
            let mut cpu = Cpu::new(&program(&[insn]));
            cpu.sp = 0x8fd;
            for lo in [26,28,30] { cpu.r[lo] = 1; cpu.r[lo+1] = 1; }
            let before = cpu.cycles;
            let result = cpu.step();
            assert_eq!(result,match insn {
                Sleep => Err(Stop::Sleep), Break => Err(Stop::Break), _ => Ok(()),
            },"{insn:?}");
            assert_eq!(cpu.cycles-before,cycles(&insn).0 as u64,"oracle {insn:?}");
        }
    }
    for bit in 0..8 {
        for insn in [Brbs(bit,1),Brbc(bit,1)] {
            for taken in [false,true] {
                let mut cpu = Cpu::new(&program(&[insn,Nop,Nop]));
                let set = taken == matches!(insn,Brbs(..));
                cpu.sreg = if set { 1 << bit } else { 0 };
                let before = cpu.cycles;
                cpu.step().unwrap();
                assert_eq!(cycles(&insn),(1,1),"manual {insn:?}");
                let (base,extra) = cycles(&insn);
                assert_eq!(cpu.cycles-before,(base + if taken { extra } else { 0 }) as u64);
                assert_eq!(cpu.pc,if taken { 2 } else { 1 });
            }
        }
    }
    for skip in [Cpse(0,1),Sbrc(0,0),Sbrs(0,0),Sbic(3,0),Sbis(3,0)] {
        for following in [Nop,Lds(2,0x100),Sts(0x100,2),Jmp(9),Call(9)] {
            for taken in [false,true] {
                let mut cpu = Cpu::new(&program(&[skip,following,Nop]));
                match skip {
                    Cpse(..) => cpu.r[1] = (!taken) as u8,
                    Sbrc(..) => cpu.r[0] = (!taken) as u8,
                    Sbrs(..) => cpu.r[0] = taken as u8,
                    Sbic(..) => cpu.set_pin('B',0,!taken),
                    Sbis(..) => cpu.set_pin('B',0,taken), _ => unreachable!(),
                }
                let before = cpu.cycles;
                cpu.step().unwrap();
                let skipped = if taken { words(&following) as u32 } else { 0 };
                assert_eq!(cycles(&skip),(1,1),"manual {skip:?}");
                let (base,extra) = cycles(&skip);
                let delta = base as u64 + if taken { extra as u64 + u64::from(skipped == 2) } else { 0 };
                assert_eq!(cpu.pc,1+skipped,"skip PC {skip:?} {following:?} {taken}");
                assert_eq!(cpu.cycles-before,1+skipped as u64,"manual cycles {skip:?} {following:?} {taken}");
                assert_eq!(cpu.cycles-before,delta,"cycle table {skip:?} {following:?} {taken}");
            }
        }
    }
}

// Independent reference: the manual's per-bit Boolean equations, rather than
// the simulator's wide arithmetic carry/borrow comparisons.
fn arithmetic_reference(a: u8, b: u8, old: u8, subtract: bool, with_carry: bool) -> (u8,u8) {
    let carry = if with_carry { old & 1 } else { 0 };
    let result = if subtract { (a as i32 - b as i32 - carry as i32) as u8 }
        else { (a as u32 + b as u32 + carry as u32) as u8 };
    let abit = |n: u32| (a >> n) & 1 != 0;
    let bbit = |n: u32| (b >> n) & 1 != 0;
    let rbit = |n: u32| (result >> n) & 1 != 0;
    let carry_at = |n| if subtract {
        (!abit(n) && bbit(n)) || (bbit(n) && rbit(n)) || (rbit(n) && !abit(n))
    } else {
        (abit(n) && bbit(n)) || (bbit(n) && !rbit(n)) || (!rbit(n) && abit(n))
    };
    let v = if subtract {
        (abit(7) && !bbit(7) && !rbit(7)) || (!abit(7) && bbit(7) && rbit(7))
    } else {
        (abit(7) && bbit(7) && !rbit(7)) || (!abit(7) && !bbit(7) && rbit(7))
    };
    let z = result == 0 && (!(subtract && with_carry) || old & 2 != 0);
    let flags = (old & 0xc0) | (carry_at(3) as u8) << 5 | ((rbit(7) ^ v) as u8) << 4 |
        (v as u8) << 3 | (rbit(7) as u8) << 2 | (z as u8) << 1 | carry_at(7) as u8;
    (result,flags)
}

fn check_arithmetic() {
    let ops = [Add(16,17),Adc(16,17),Sub(16,17),Sbc(16,17),Cp(16,17),Cpc(16,17)];
    let mut cpu = Cpu::new(&program(&ops));
    for (op,i) in ops.into_iter().enumerate() {
        for carry in 0..=1 {
            for old_z in 0..=1 {
                let old = 0xfc | carry | (old_z << 1);
                for a in 0..=255u8 { for b in 0..=255u8 {
                    cpu.pc = op as u32; cpu.sreg = old; cpu.r[16] = a; cpu.r[17] = b;
                    let expected = arithmetic_reference(a,b,old,op >= 2,op & 1 != 0);
                    cpu.step().unwrap();
                    assert_eq!(cpu.sreg,expected.1,"flags {i:?} {a:02x} {b:02x} old={old:02x}");
                    assert_eq!(cpu.r[16],if op >= 4 { a } else { expected.0 },"result {i:?}");
                    assert_eq!(cpu.r[17],b,"source {i:?}");
                } }
            }
        }
    }
    assert_eq!(cpu.cycles,6*4*256*256,"one cycle per arithmetic instruction");

    let unary = [Neg(16),Com(16),Asr(16),Lsr(16),Ror(16),Inc(16),Dec(16),Swap(16)];
    let mut cpu = Cpu::new(&program(&unary));
    for (op,i) in unary.into_iter().enumerate() {
        for a in 0..=255u8 { for old in [0u8,0xff,0x80,0x41] {
            cpu.pc = op as u32; cpu.r[16] = a; cpu.sreg = old;
            let (r,c,h,v,mask) = match i {
                Neg(_) => {
                    let r = (0i16 - a as i16) as u8;
                    (r,r != 0,(r & 8) != 0 || (a & 8) != 0,r == 128,0x3f)
                }
                Com(_) => (255-a,true,false,false,0x1f),
                Inc(_) => ((a as u16+1) as u8,false,false,a == 127,0x1e),
                Dec(_) => ((a as i16-1) as u8,false,false,a == 128,0x1e),
                Swap(_) => ((a % 16)*16 + a/16,false,false,false,0),
                _ => {
                    let top = match i { Asr(_) => a/128, Ror(_) => old & 1, _ => 0 };
                    let r = a/2 + top*128;
                    (r,a & 1 != 0,false,(top != 0) ^ (a & 1 != 0),0x1f)
                }
            };
            let n = r >= 128;
            let flags = (h as u8)*32 + ((n ^ v) as u8)*16 + (v as u8)*8 +
                (n as u8)*4 + ((r == 0) as u8)*2 + c as u8;
            cpu.step().unwrap();
            assert_eq!((cpu.r[16],cpu.sreg),(r,(old & !mask) | (flags & mask)),"{i:?} {a} {old}");
        } }
    }

    let ops = [Mul(16,17),Muls(16,17),Mulsu(16,17),Fmul(16,17),Fmuls(16,17),Fmulsu(16,17)];
    let mut cpu = Cpu::new(&program(&ops));
    for (op,i) in ops.into_iter().enumerate() {
        for a in [0,1,2,63,127,128,129,192,254,255u8] {
            for b in [0,1,2,63,127,128,129,192,254,255u8] {
                for old in [0u8,0xff] {
                    let left = if matches!(op,1 | 2 | 4 | 5) && a >= 128 { a as i32-256 } else { a as i32 };
                    let right = if matches!(op,1 | 4) && b >= 128 { b as i32-256 } else { b as i32 };
                    let wide = (left*right) & 0xffff;
                    let result = (if op >= 3 { wide*2 } else { wide }) as u16;
                    let flags = (old & 0xfc) | ((result == 0) as u8)*2 | ((wide & 0x8000 != 0) as u8);
                    cpu.pc = op as u32; cpu.r[16] = a; cpu.r[17] = b; cpu.sreg = old;
                    let before = cpu.cycles; cpu.step().unwrap();
                    assert_eq!(cpu.cycles-before,2,"{i:?} cycles");
                    assert_eq!((u16::from_le_bytes([cpu.r[0],cpu.r[1]]),cpu.sreg),(result,flags),"{i:?} {a} {b}");
                }
            }
        }
    }

    let mut cpu = Cpu::new(&[0]);
    for d in [24,26,28,30] { for k in [0,1,31,63u8] { for sub in [false,true] {
        let i = if sub { Sbiw(d,k) } else { Adiw(d,k) };
        cpu.flash[0] = encode(&i).unwrap().0[0];
        for a in [0,1,63,255,256,0x7fff,0x8000,0xffc0,0xffffu16] { for old in [0u8,0xff] {
            let result = (if sub { a as i32 - k as i32 } else { a as i32 + k as i32 }) as u16;
            let a15 = (a >> 15) as u8; let r15 = (result >> 15) as u8;
            let v = if sub { a15 & (r15 ^ 1) } else { (a15 ^ 1) & r15 };
            let c = if sub { r15 & (a15 ^ 1) } else { (r15 ^ 1) & a15 };
            let flags = (old & 0xe0) | (r15 ^ v) << 4 | v << 3 | r15 << 2 | ((result == 0) as u8) << 1 | c;
            cpu.pc = 0; cpu.r[d as usize] = a as u8; cpu.r[d as usize+1] = (a >> 8) as u8; cpu.sreg = old;
            let before = cpu.cycles; cpu.step().unwrap();
            assert_eq!(cpu.cycles-before,2,"{i:?} cycles");
            assert_eq!((u16::from_le_bytes([cpu.r[d as usize],cpu.r[d as usize+1]]),cpu.sreg),(result,flags),"{i:?} {a}");
        } }
    } } }
}

fn check_assembler() {
    let mut a = Asm::new();
    let near = a.label(); let far = a.label();
    a.jump(Target::Far,near);
    a.bind(near);
    a.jump(Target::FarCall,near);
    a.jump(Target::Far,far);
    a.ldi_label(16,17,far);
    a.jump(Target::FarCall,far);
    a.org(4096);
    a.bind(far);
    a.word(0x1234);
    a.jump(Target::Far,near);
    let w = a.finish().unwrap();
    assert_eq!(decode(&w),Some((Rjmp(0),1)),"near far jump");
    assert_eq!(decode(&w[1..]),Some((Rcall(-1),1)),"near far call");
    assert_eq!(decode(&w[2..]),Some((Jmp(4096),2)),"far jump");
    assert_eq!(decode(&w[4..]),Some((Ldi(16,0),1)),"label low after relaxation");
    assert_eq!(decode(&w[5..]),Some((Ldi(17,16),1)),"label high after relaxation");
    assert_eq!(decode(&w[6..]),Some((Call(4096),2)),"far call");
    assert!(w[8..4096].iter().all(|w| *w == 0xffff),"org padding");
    assert_eq!(w[4096],0x1234,"org address stays fixed");
    assert_eq!(decode(&w[4097..]),Some((Jmp(1),2)),"backward far jump");
    assert_eq!(Asm::bytes(&[0x1234,0xabcd]),[0x34,0x12,0xcd,0xab],"flash byte order");

    // A grows on the first pass; that pushes B's destination beyond its short
    // range on the second pass. There is no org between B and its destination.
    let mut a = Asm::new();
    let b_dest = a.label(); let a_dest = a.label();
    a.jump(Target::Far,b_dest); // B: initial displacement 2047.
    a.jump(Target::Far,a_dest); // A: initial displacement 2049.
    for _ in 0..2046 { a.word(0); }
    a.bind(b_dest);
    a.ldi_label(16,17,b_dest);
    a.word(0);
    a.bind(a_dest);
    let w = a.finish().unwrap();
    assert_eq!(decode(&w),Some((Jmp(2050),2)),"second relaxation pass");
    assert_eq!(decode(&w[2..]),Some((Jmp(2053),2)),"first relaxation pass");
    assert_eq!(decode(&w[2050..]),Some((Ldi(16,2),1)),"moved label low");
    assert_eq!(decode(&w[2051..]),Some((Ldi(17,8),1)),"moved label high");

    for (target,gap) in [(Target::Brbs(1),64u32),(Target::Brbc(1),64),(Target::Rjmp,2048),(Target::Rcall,2048)] {
        let mut a = Asm::new(); let l = a.label(); a.jump(target,l); a.org(gap+1); a.bind(l);
        let e = a.finish().unwrap_err();
        assert!(e.contains("L0") && e.contains(&format!("distance {gap}")),"{e}");
    }
    let mut a = Asm::new(); let l = a.label(); a.bind(l); a.org(64); a.jump(Target::Brbc(1),l);
    assert!(a.finish().unwrap_err().contains("distance -65"));
    for (target,gap) in [(Target::Brbs(7),63u32),(Target::Brbc(0),63),(Target::Rjmp,2047),(Target::Rcall,2047)] {
        let mut a = Asm::new(); let l = a.label(); a.jump(target,l); a.org(gap+1); a.bind(l);
        assert!(a.finish().is_ok(),"positive boundary {target:?}");
    }
    let mut a = Asm::new(); let l = a.label(); a.jump(Target::Jmp,l);
    assert!(a.finish().unwrap_err().contains("unbound label L0"));
    let mut a = Asm::new(); let l = a.label(); a.ldi_label(16,17,l);
    assert!(a.finish().unwrap_err().contains("unbound label L0"));
    let mut a = Asm::new(); a.emit(Ldi(0,0)); a.emit(Sbi(33,8));
    assert!(a.finish().unwrap_err().contains("d operand 0"),"first error retained");
    let mut a = Asm::new(); a.word(0); a.org(0);
    assert!(a.finish().unwrap_err().contains("org"));
    let mut a = Asm::new(); let l = a.label(); a.jump(Target::Far,l); a.org(1); a.org(4096); a.bind(l);
    assert!(a.finish().unwrap_err().contains("relaxed word address 2"),"org overrun after growth");
}

fn check_control_and_memory() {
    let mut a = Asm::new(); let again = a.label();
    a.emit(Ldi(16,17)); a.bind(again); a.emit(Dec(16)); a.jump(Target::Brbc(1),again); a.emit(Break);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    assert_eq!(cpu.run(1000),Stop::Break);
    // LDI + 17 DEC + 16 taken BRNE + 1 untaken BRNE + BREAK.
    assert_eq!(cpu.cycles,1+17+16*2+1+1,"counted delay");
    assert_eq!(cpu.r[16],0);

    let mut a = Asm::new();
    let sub = a.label(); let sub2 = a.label(); let done = a.label(); let table = a.label();
    a.emit(Ldi(16,0xa5)); a.emit(Ldi(17,0x3c)); a.emit(Push(16));
    let return_pc = a.here()+2;
    a.jump(Target::Call,sub); a.emit(Pop(19));
    a.ldi_label(30,31,sub2); a.emit(Icall); a.jump(Target::Rcall,sub2);
    a.ldi_label(30,31,table); a.emit(Add(30,30)); a.emit(Adc(31,31));
    a.emit(Lpm); a.emit(LpmZInc(21)); a.emit(LpmZ(22));
    a.ldi_label(30,31,done); a.emit(Ijmp);
    a.bind(done); a.emit(Break);
    a.bind(sub); a.emit(Push(17)); a.emit(Ldi(17,0x5a)); a.emit(Pop(18)); a.emit(Ret);
    a.bind(sub2); a.emit(Inc(20)); a.emit(Ret);
    a.bind(table); a.word(0xbeef);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    for _ in 0..4 { cpu.step().unwrap(); }
    assert_eq!(cpu.sp,0x8fc,"stack after PUSH and CALL");
    assert_eq!(cpu.read(0x8ff),0xa5);
    assert_eq!(cpu.read(0x8fe),return_pc as u8,"return PC low pushed first");
    assert_eq!(cpu.read(0x8fd),(return_pc >> 8) as u8,"return PC high pushed second");
    assert_eq!(cpu.run(1000),Stop::Break);
    assert_eq!(cpu.cycles,55,"CALL/ICALL/RCALL/RET, stack, LPM and IJMP timing");
    assert_eq!(cpu.sp,0x8ff,"stack round trip");
    assert_eq!((cpu.r[17],cpu.r[18],cpu.r[19],cpu.r[20]),(0x5a,0x3c,0xa5,2));
    assert_eq!((cpu.r[0],cpu.r[21],cpu.r[22]),(0xef,0xef,0xbe),"LPM byte addresses");

    let mut cpu = Cpu::new(&program(&[Brbs(0,1),Nop,Brbc(0,-2)]));
    cpu.sreg = 1; cpu.step().unwrap(); assert_eq!((cpu.pc,cpu.cycles),(2,2));
    cpu.step().unwrap(); assert_eq!((cpu.pc,cpu.cycles),(3,3));

    // Load/store addressing and data-space aliases, using only legal pointer
    // destinations. Every memory instruction below takes two clocks.
    let mut cpu = Cpu::new(&program(&[
        St(Ptr::XInc,16),Ld(17,Ptr::XDec),St(Ptr::X,17),Ld(18,Ptr::X),
        St(Ptr::YInc,16),Ld(19,Ptr::YDec),St(Ptr::ZInc,16),Ld(20,Ptr::ZDec),
        Std(true,63,16),Ldd(21,true,63),Std(false,7,16),Ldd(22,false,7),
        Sts(0x8ff,16),Lds(23,0x8ff),Sts(0,16),Lds(24,0),
    ]));
    cpu.r[16] = 0xa7; cpu.r[26] = 0; cpu.r[27] = 1;
    cpu.r[28] = 0x40; cpu.r[29] = 1; cpu.r[30] = 0x80; cpu.r[31] = 1;
    for _ in 0..16 { cpu.step().unwrap(); }
    assert_eq!(cpu.cycles,32);
    for d in 17..=24 { assert_eq!(cpu.r[d],0xa7,"loaded r{d}"); }
    assert_eq!((cpu.r[26],cpu.r[28],cpu.r[30]),(0,0x40,0x80),"pointer updates");
    assert_eq!(cpu.r[0],0xa7,"register data alias");

    let mut cpu = Cpu::new(&program(&[
        Ldi(16,0xab),Mov(0,16),Movw(2,0),Swap(16),Andi(16,0xf0),Ori(16,5),
        And(16,0),Or(16,0),Eor(16,0),Bset(6),Bld(17,7),Bst(16,0),Bclr(0),
        Subi(17,1),Sbci(17,0),Cpi(17,0x7f),Out(0x3f,0),In(18,0x3f),Wdr,Break,
    ]));
    assert_eq!(cpu.run(100),Stop::Break);
    assert_eq!(cpu.cycles,20);
    assert_eq!((cpu.r[0],cpu.r[2],cpu.r[16],cpu.r[17],cpu.r[18]),(0xab,0xab,0,0x7f,0xab));
    assert_eq!(cpu.sreg,0xab,"SREG alias");
    cpu.write(0x5d,0x34); cpu.write(0x5e,0x08);
    assert_eq!((cpu.sp,cpu.read(0x5d),cpu.read(0x5e)),(0x834,0x34,8));
}

fn check_gpio() {
    let mut cpu = Cpu::new(&program(&[
        Sbi(5,0),Sbi(4,0),Sbi(3,0),Cbi(4,0),
        Sbi(5,1),Sbi(3,0),Cbi(3,1),
        Break,
    ]));
    assert_eq!(cpu.run(100),Stop::Break);
    assert_eq!(cpu.events,vec![
        Event::Pin { cycle: 2, port: 'B', bit: 0, high: true }, // input pull-up
        Event::Pin { cycle: 6, port: 'B', bit: 0, high: false }, // PIN toggle
        Event::Pin { cycle: 10, port: 'B', bit: 1, high: true },
        Event::Pin { cycle: 12, port: 'B', bit: 0, high: true }, // only the SBI bit toggles
    ]);
    for (p,base) in [('B',0x23),('C',0x26),('D',0x29)] {
        cpu.write(base+2,0); cpu.write(base+1,0);
        cpu.set_pin(p,3,true); assert_eq!(cpu.read(base) & 8,8,"external high");
        let before = cpu.events.len();
        cpu.write(base+1,8); // direction change drives PORT's low value
        assert_eq!(cpu.events[before],Event::Pin { cycle: cpu.cycles, port: p, bit: 3, high: false });
        cpu.set_pin(p,3,false); cpu.set_pin(p,3,true);
        assert_eq!(cpu.events.len(),before+1,"external input ignored while driving output");
        cpu.write(base+1,0); assert_eq!(cpu.read(base) & 8,8,"external level survives output mode");
    }
}

fn usart(cpu: &mut Cpu, control: u8) {
    cpu.write(0xc0,2); // U2X0: 8 clocks per baud tick.
    cpu.write(0xc1,control);
    cpu.write(0xc2,6); // 8N1.
    cpu.write(0xc4,16); cpu.write(0xc5,0);
}

#[test]
fn usart_polling_and_interrupt_streams() {
    let mut a = Asm::new(); let poll = a.label(); let drain = a.label();
    a.emit(Ldi(16,b'A')); a.emit(Ldi(17,3));
    a.bind(poll); a.emit(Lds(18,0xc0)); a.emit(Sbrs(18,5)); a.jump(Target::Rjmp,poll);
    a.emit(Sts(0xc6,16)); a.emit(Inc(16)); a.emit(Dec(17)); a.jump(Target::Brbc(1),poll);
    a.bind(drain); a.emit(Lds(18,0xc0)); a.emit(Sbrs(18,6)); a.jump(Target::Rjmp,drain);
    a.emit(Break);
    let mut cpu = Cpu::new(&a.finish().unwrap()); usart(&mut cpu,8);
    assert_eq!(cpu.run(5000),Stop::Break);
    // Two LDIs + LDS + SBRS (skips one word) + STS = 8. Each frame
    // thereafter is exactly (start + 8 data + stop) * 8 * (16+1) = 1360.
    assert_eq!(cpu.events,vec![
        Event::Tx { cycle: 8, byte: b'A' },
        Event::Tx { cycle: 1368, byte: b'B' },
        Event::Tx { cycle: 2728, byte: b'C' },
        Event::TxDone { cycle: 4088 },
    ]);
    assert_eq!(cpu.read(0xc0),0x62,"TXC and UDRE set; RXC remains clear");

    let mut a = Asm::new(); let isr = a.label(); let done = a.label();
    a.emit(Bset(7)); a.emit(Sleep); a.emit(Rjmp(-2));
    a.org(38); a.jump(Target::Rjmp,isr);
    a.org(40); a.emit(Break); // Stop on the final TX-complete interrupt.
    a.org(64); a.bind(isr);
    a.emit(LpmZInc(16)); a.emit(Sts(0xc6,16)); a.emit(Dec(18));
    a.jump(Target::Brbc(1),done);
    a.emit(Ldi(16,0x48)); a.emit(Sts(0xc1,16)); // Disable UDRE after byte 3.
    a.bind(done); a.emit(Reti);
    a.org(128); a.word(0x4241); a.word(0x0043);
    let mut cpu = Cpu::new(&a.finish().unwrap()); usart(&mut cpu,0x68);
    cpu.r[18] = 3; cpu.r[30] = 0; cpu.r[31] = 1; // Flash byte address 256.
    // SEI 1 + SLEEP 1 + idle wake 4 + entry 4 + vector RJMP 2 + LPM 3 + STS 2.
    for _ in 0..6 { cpu.step().unwrap(); }
    assert_eq!(cpu.cycles,17,"first ISR write");
    // DEC 1 + BRNE 2 + RETI 4 + main RJMP 2 + entry 4 + vector 2 + LPM 3 + STS 2.
    for _ in 0..8 { cpu.step().unwrap(); }
    assert_eq!(cpu.cycles,37,"second ISR write fills UDR without starting a frame");
    assert_eq!(cpu.read(0xc0) & 0x20,0);
    for _ in 0..4 { cpu.step().unwrap(); } // DEC, BRNE, RETI, SLEEP: 8 clocks.
    assert_eq!(cpu.cycles,45);
    cpu.step().unwrap();
    assert_eq!((cpu.pc,cpu.cycles),(38,1385),"UDRE wake at 1377 + wake 4 + entry 4");
    assert_eq!(cpu.run(5000),Stop::Break);
    assert_eq!(cpu.events,vec![
        Event::Tx { cycle: 17, byte: b'A' },
        Event::Tx { cycle: 1377, byte: b'B' },
        Event::Tx { cycle: 2737, byte: b'C' },
        Event::TxDone { cycle: 4097 },
    ]);
    assert_eq!(cpu.cycles,4106,"last frame + idle wake 4 + entry 4 + BREAK 1");
    assert_eq!((cpu.r[30],cpu.r[31],cpu.r[18]),(3,1,0),"three flash bytes consumed");
    assert_eq!(cpu.read(0xc0) & 0x40,0,"TX interrupt acknowledges TXC");
}

#[test]
fn usart_frame_boundaries_and_overrun() {
    let mut cpu = Cpu::new(&program(&[Sts(0xc6,16),Sts(0xc6,17),Sts(0xc6,18)]));
    usart(&mut cpu,8); cpu.r[16] = b'A'; cpu.r[17] = b'B'; cpu.r[18] = b'C';
    cpu.step().unwrap(); assert_eq!(cpu.read(0xc0) & 0x20,0x20,"idle shifter accepts first byte");
    cpu.step().unwrap(); assert_eq!(cpu.read(0xc0) & 0x20,0,"second byte occupies UDR");
    assert_eq!(cpu.step(),Err(Stop::UdrOverrun { pc: 4 }));
    assert_eq!(cpu.cycles,6,"overrun at the offending STS completion");
    assert_eq!(cpu.events,vec![Event::Tx { cycle: 2, byte: b'A' }]);

    // All valid asynchronous frame lengths, both baud divisors, and all parity
    // bit patterns (even reserved UPM=01 adds the requested parity clock).
    for (size,data) in [(0,5),(1,6),(2,7),(3,8),(7,9)] {
        for parity in 0..4 {
            for stops in 1..=2 {
                for double in [false,true] {
                    let mut cpu = Cpu::new(&program(&[Nop,Call(0)]));
                    cpu.write(0xc0,if double { 2 } else { 0 });
                    cpu.write(0xc1,8 | (size & 4));
                    let format = ((size & 3) << 1) | (parity << 4) | ((stops-1) << 3);
                    cpu.write(0xc2,format);
                    cpu.write(0xc4,0); cpu.write(0xc5,0);
                    assert_eq!(cpu.read(0xc2),format);
                    cpu.write(0xc6,0x55);
                    let end = (1 + data + u64::from(parity != 0) + stops as u64)
                        * if double { 8 } else { 16 };
                    // NOPs put a four-clock CALL across the exact frame end.
                    for _ in 0..end-1 { cpu.pc = 0; cpu.step().unwrap(); }
                    assert_eq!(cpu.read(0xc0) & 0xc0,0,"frame not complete a clock early");
                    cpu.pc = 1; cpu.step().unwrap();
                    assert_eq!(cpu.cycles,end+3);
                    assert_eq!(cpu.events,vec![Event::Tx { cycle: 0, byte: 0x55 },Event::TxDone { cycle: end }]);
                    cpu.write(0xc0,if double { 2 } else { 0 });
                    assert_ne!(cpu.read(0xc0) & 0x40,0,"zero does not acknowledge TXC");
                    cpu.write(0xc6,0x33);
                    assert_ne!(cpu.read(0xc0) & 0x40,0,"starting another frame does not acknowledge TXC");
                    cpu.write(0xc0,0xc0);
                    assert_eq!(cpu.read(0xc0),0x20,"TXC W1C and RXC read-only");
                }
            }
        }
    }

    // A buffered transfer inside a long instruction.
    let mut cpu = Cpu::new(&program(&[Nop,Call(0)])); usart(&mut cpu,8);
    cpu.write(0xc6,b'A'); cpu.write(0xc6,b'B');
    for _ in 0..1359 { cpu.pc = 0; cpu.step().unwrap(); }
    cpu.pc = 1; cpu.step().unwrap();
    assert_eq!(cpu.cycles,1363);
    assert_eq!(cpu.events,vec![Event::Tx { cycle: 0, byte: b'A' },Event::Tx { cycle: 1360, byte: b'B' }]);
    assert_eq!(cpu.read(0xc0),0x22,"transfer frees UDR without setting TXC");

    // STS completes on the same clock that the buffered byte enters the shifter:
    // UDRE must already be high, so the third write is legal at that boundary.
    let mut cpu = Cpu::new(&program(&[Nop,Sts(0xc6,16),Sleep]));
    usart(&mut cpu,0x48); cpu.write(0xc4,0); cpu.sreg = 128; cpu.r[16] = b'C';
    cpu.write(0xc6,b'A'); cpu.write(0xc6,b'B');
    for _ in 0..78 { cpu.pc = 0; cpu.step().unwrap(); }
    cpu.pc = 1; cpu.step().unwrap(); assert_eq!(cpu.cycles,80);
    assert_eq!(cpu.read(0xc0) & 0x60,0,"third byte occupies the newly freed buffer");
    cpu.step().unwrap(); cpu.step().unwrap();
    assert_eq!(cpu.events,vec![Event::Tx { cycle: 0, byte: b'A' },
        Event::Tx { cycle: 80, byte: b'B' },Event::Tx { cycle: 160, byte: b'C' },
        Event::TxDone { cycle: 240 }]);
    assert_eq!((cpu.pc,cpu.cycles),(40,248));

    let mut cpu = Cpu::new(&program(&[Sleep])); usart(&mut cpu,0x48); cpu.sreg = 128;
    cpu.write(0xc5,0xf1); cpu.write(0xc4,0);
    assert_eq!((cpu.read(0xc5),cpu.read(0xc4)),(1,0),"UBRR is twelve bits");
    cpu.write(0xc6,b'A'); cpu.step().unwrap(); cpu.step().unwrap();
    assert_eq!(cpu.events,vec![Event::Tx { cycle: 0, byte: b'A' },Event::TxDone { cycle: 20560 }]);
    assert_eq!((cpu.pc,cpu.cycles),(40,20568),"10 * 8 * (256+1), then idle wake and entry");
}

#[test]
fn usart_sleep_and_interrupt_priority() {
    assert_eq!(IDLE_WAKE_CYCLES,4,"ATmega328P idle wake latency");
    let mut cpu = Cpu::new(&program(&[Bset(7),Sleep,Nop])); usart(&mut cpu,0x28);
    cpu.step().unwrap(); cpu.step().unwrap(); cpu.step().unwrap();
    assert_eq!((cpu.pc,cpu.cycles),(38,10),"already-empty UDR wakes immediately after SLEEP");
    assert_eq!(cpu.read(0xc0) & 0x20,0x20,"UDRE remains pending after entry");

    for (global,control) in [(false,0x28),(true,8),(true,0x88)] {
        let mut cpu = Cpu::new(&program(&[Sleep])); usart(&mut cpu,control);
        cpu.sreg = if global { 128 } else { 0 };
        assert_eq!(cpu.step(),Err(Stop::Sleep),"I clear or no modeled enabled source");
    }

    let mut cpu = Cpu::new(&program(&[Sleep])); usart(&mut cpu,0x48); cpu.sreg = 128;
    cpu.write(0xc6,b'A'); cpu.write(0xc6,b'B');
    cpu.step().unwrap();
    assert_eq!(cpu.run(1358),Stop::Cycles); assert_eq!(cpu.cycles,1359);
    assert_eq!(cpu.read(0xc0) & 0x60,0);
    assert_eq!(cpu.run(1),Stop::Cycles); assert_eq!(cpu.cycles,1360);
    assert_eq!(cpu.read(0xc0) & 0x60,0x20,"buffer transferred at the exact deadline");
    assert_eq!(cpu.run(1359),Stop::Cycles); assert_eq!(cpu.cycles,2719);
    assert_eq!(cpu.read(0xc0) & 0x40,0);
    cpu.step().unwrap();
    assert_eq!((cpu.pc,cpu.cycles),(40,2728),"only the idle frame end wakes TXC");
    assert_eq!(cpu.events,vec![Event::Tx { cycle: 0, byte: b'A' },
        Event::Tx { cycle: 1360, byte: b'B' },Event::TxDone { cycle: 2720 }]);
    assert_eq!(cpu.read(0xc0) & 0x40,0,"TXC acknowledged on vector entry");

    for (top,vector,at) in [(39,22,48),(1999,40,1368)] {
        let mut cpu = Cpu::new(&program(&[Sleep])); usart(&mut cpu,0x48); cpu.sreg = 128;
        cpu.write(0xc6,b'A'); timer(&mut cpu,top,0,1,2);
        cpu.step().unwrap(); cpu.step().unwrap();
        assert_eq!((cpu.pc,cpu.cycles),(vector,at),"earliest of timer and USART wakes");
    }

    // Pending during SEI's deferral: both sources can wake, COMPA wins.
    let mut cpu = Cpu::new(&program(&[Bset(7),Sleep])); usart(&mut cpu,0x28);
    timer(&mut cpu,1,0,1,2);
    cpu.step().unwrap(); cpu.step().unwrap(); cpu.step().unwrap();
    assert_eq!((cpu.pc,cpu.cycles),(22,10),"COMPA precedes UDRE on idle wake");
    assert_eq!(cpu.read(0xc0) & 0x20,0x20);

    // All four pending: COMPA, OVF, UDRE, TX. Stop Timer1 before servicing so
    // new matches cannot mask the remaining priorities.
    let mut cpu = Cpu::new(&program(&[Rjmp(-1)])); usart(&mut cpu,0x68);
    cpu.write(0xc6,b'A'); assert_eq!(cpu.run(1360),Stop::Cycles);
    timer(&mut cpu,0,0xfffe,1,3);
    cpu.step().unwrap(); cpu.step().unwrap(); cpu.write(0x81,0);
    for vector in [22,26,38,40] {
        cpu.sreg = 128; let before = cpu.cycles; cpu.step().unwrap();
        assert_eq!((cpu.pc,cpu.cycles-before),(vector,4),"interrupt priority");
        if vector == 38 { cpu.write(0xc1,0x48); } // Level remains high until disabled.
    }
    assert_eq!(cpu.read(0x36) & 3,0); assert_eq!(cpu.read(0xc0) & 0x40,0);
}

fn write16(cpu: &mut Cpu, address: u16, value: u16) {
    cpu.write(address+1,(value >> 8) as u8); cpu.write(address,value as u8);
}

fn timer(cpu: &mut Cpu, top: u16, count: u16, mode: u8, mask: u8) {
    write16(cpu,0x88,top); write16(cpu,0x84,count);
    cpu.write(0x36,3); cpu.write(0x6f,mask); cpu.write(0x81,mode);
}

#[test]
fn prescaler_reset_hold_and_flag_acknowledgement() {
    let mut cpu = Cpu::new(&program(&[Nop,Out(0x23,16)]));
    for _ in 0..5 { cpu.pc = 0; cpu.step().unwrap(); }
    cpu.r[16] = 1; cpu.pc = 1; cpu.step().unwrap(); // PSRSYNC at cycle 6.
    let reset = cpu.cycles;
    assert_eq!(cpu.read(0x43),0,"PSRSYNC clears itself without TSM");
    timer(&mut cpu,0xffff,0,2,0); // Normal, /8, counter zero at reset.
    for _ in 0..25 {
        cpu.pc = 0; cpu.step().unwrap();
        let count = u16::from_le_bytes([cpu.read(0x84),cpu.read(0x85)]);
        assert_eq!(count as u64,(cpu.cycles-reset)/8,"tick phase since PSRSYNC");
    }
    cpu.write(0x43,0x81); write16(&mut cpu,0x84,0);
    for _ in 0..19 { cpu.pc = 0; cpu.step().unwrap(); }
    assert_eq!((cpu.read(0x43),cpu.read(0x84)),(0x81,0),"TSM holds prescaler in reset");
    cpu.write(0x43,0); let release = cpu.cycles;
    for _ in 0..17 {
        cpu.pc = 0; cpu.step().unwrap();
        assert_eq!(cpu.read(0x84) as u64,(cpu.cycles-release)/8,"first tick eight clocks after TSM release");
    }

    // Generate all three flags, stop the clock, then acknowledge through OUT/SBI/CBI.
    let mut cpu = Cpu::new(&program(&[Nop,Nop,Nop,Out(0x16,16),Cbi(0x16,1),Sbi(0x16,1),Out(0x16,17)]));
    timer(&mut cpu,0,0xfffe,1,0);
    for _ in 0..3 { cpu.step().unwrap(); }
    cpu.write(0x81,0); assert_eq!(cpu.read(0x36),7);
    cpu.r[16] = 0; cpu.r[17] = 1;
    cpu.step().unwrap(); assert_eq!(cpu.read(0x36),7,"OUT zero leaves flags");
    cpu.step().unwrap(); assert_eq!(cpu.read(0x36),7,"CBI writes zero to only its flag");
    cpu.step().unwrap(); assert_eq!(cpu.read(0x36),5,"SBI clears only OCF1A");
    cpu.step().unwrap(); assert_eq!(cpu.read(0x36),4,"OUT one clears TOV1, leaving OCF1B");
    cpu.write(0x36,4); assert_eq!(cpu.read(0x36),0,"OCF1B is independently W1C");
}

#[test]
fn reset_flags_and_watchdog_timed_writes() {
    let mut cpu = Cpu::new(&program(&[Out(0x34,16),Out(0x34,17),In(18,0x34)]));
    assert_eq!(cpu.read(0x54),2,"external-reset EXTRF");
    // MCUSR is write-ZERO-to-clear in the datasheet, unlike TIFR1.
    cpu.r[16] = 0xff; cpu.r[17] = 0;
    cpu.step().unwrap(); assert_eq!(cpu.read(0x54),2,"ones preserve MCUSR flags");
    cpu.step().unwrap(); cpu.step().unwrap();
    assert_eq!(cpu.r[18],0,"zero clears EXTRF");
    cpu.write(0x54,0xff); assert_eq!(cpu.read(0x54),0,"software cannot set reset flags");

    // WDP needs an earlier unlock; WDCE alone does not open one. WDIE remains
    // independently writable and WDIF is not set by software (watchdog is inert).
    let mut cpu = Cpu::new(&program(&[Nop]));
    cpu.write(0x60,0xf7); assert_eq!(cpu.read(0x60),0x40);
    cpu.write(0x60,0x27); assert_eq!(cpu.read(0x60),0);
    cpu.step().unwrap();

    // STS unlock completes at 2; second STS completes at 4+gap. Cycle 6 is
    // the fourth and last permitted clock, while cycle 7 must leave WDE/WDP.
    for gap in 0..=3 {
        let mut code = vec![Sts(0x60,16)];
        for _ in 0..gap { code.push(Nop); }
        code.push(Sts(0x60,17));
        let mut cpu = Cpu::new(&program(&code)); cpu.r[16] = 0x18; cpu.r[17] = 0x25;
        cpu.step().unwrap(); assert_eq!(cpu.read(0x60),0x18,"WDCE+WDE opens window");
        for _ in 0..gap { cpu.step().unwrap(); }
        let result = cpu.step();
        assert_eq!(cpu.cycles,4+gap as u64);
        if gap <= 2 {
            assert_eq!(result,Ok(()),"disable on clock {}",gap+2);
            assert_eq!(cpu.read(0x60),0x25,"WDE cleared, all four WDP bits writable");
            cpu.write(0x60,0); assert_eq!(cpu.read(0x60),0x25,"prescaler protected again");
        } else {
            assert_eq!(result,Err(Stop::Watchdog { pc: 2+gap as u32 }));
            assert_eq!(cpu.read(0x60),8,"late write preserves enabled WDE and old WDP");
        }
    }

    let mut cpu = Cpu::new(&program(&[Sts(0x60,16),Nop,Nop,Nop,Nop,Sts(0x60,17)]));
    cpu.r[16] = 0x18; cpu.r[17] = 8;
    cpu.step().unwrap();
    for _ in 0..3 { cpu.step().unwrap(); }
    assert_eq!(cpu.read(0x60),0x18,"WDCE set before fourth clock");
    cpu.step().unwrap(); assert_eq!(cpu.read(0x60),8,"WDCE clears after four clocks");
    assert_eq!(cpu.step(),Err(Stop::Watchdog { pc: 6 }),"next enable write fails loudly");

    let mut cpu = Cpu::new(&program(&[Sts(0x60,16),Sts(0x60,17)]));
    cpu.r[16] = 0x18; cpu.r[17] = 0x2b;
    cpu.step().unwrap();
    assert_eq!(cpu.step(),Err(Stop::Watchdog { pc: 2 }),"valid sequence must still disable WDE");
    assert_eq!(cpu.read(0x60),0x2b,"configuration stored before stop");
    let mut cpu = Cpu::new(&program(&[Sts(0x60,16)])); cpu.r[16] = 8;
    assert_eq!(cpu.step(),Err(Stop::Watchdog { pc: 0 }),"WDE enable needs no unlock");
}

fn check_timer() {
    let mut a = Asm::new(); let main = a.label(); let isr = a.label(); let again = a.label();
    a.jump(Target::Rjmp,main);
    a.org(22); a.jump(Target::Rjmp,isr);
    a.org(26); a.emit(Reti);
    a.org(64); a.bind(main);
    a.emit(Sbi(4,5)); // PB5 output, initially low.
    a.emit(Ldi(16,0)); a.emit(Sts(0x89,16)); a.emit(Ldi(16,249)); a.emit(Sts(0x88,16));
    a.emit(Ldi(16,2)); a.emit(Sts(0x6f,16));
    a.emit(Ldi(16,0x0b)); a.emit(Sts(0x81,16)); // CTC, divide by 64.
    a.emit(Bset(7)); a.bind(again); a.emit(Sleep); a.jump(Target::Rjmp,again);
    a.bind(isr); a.emit(Sbi(3,5)); a.emit(Reti);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    assert_eq!(cpu.run(160_020),Stop::Cycles);
    assert_eq!(cpu.cycles,160_020,"sleep clips the remaining cycle budget");
    assert_eq!(cpu.events.len(),10);
    for (n,event) in cpu.events.iter().enumerate() {
        // Prescaler first edge at 64, equality at 15936, OCF1A at 16000;
        // wake 4 + entry 4 + vector RJMP 2 + SBI 2 = 12 clocks.
        assert_eq!(*event,Event::Pin { cycle: 16_012 + 16_000*n as u64,
            port: 'B', bit: 5, high: n % 2 == 0 });
    }
    assert_eq!(cpu.sp,0x8ff,"sleep/RETI stack balance");

    // One coherent counter read even if the low byte rolls over in between.
    let mut cpu = Cpu::new(&program(&[Nop,Nop,Nop,Nop]));
    timer(&mut cpu,0x1234,0x12ff,1,0);
    assert_eq!(cpu.read(0x84),0xff);
    cpu.step().unwrap();
    assert_eq!(cpu.read(0x85),0x12,"TCNT high latched by earlier low read");
    assert_eq!((cpu.read(0x84),cpu.read(0x85)),(0,0x13));
    // OCR reads bypass TEMP. A partial OCR write shares TEMP with TCNT reads.
    cpu.write(0x89,0xab);
    assert_eq!(cpu.read(0x89),0x12,"high write not committed yet");
    assert_eq!(cpu.read(0x88),0x34);
    cpu.write(0x84,0x56);
    assert_eq!((cpu.read(0x84),cpu.read(0x85)),(0x56,0xab),"OCR read leaves TEMP alone");
    cpu.write(0x89,0xcd); cpu.read(0x84); cpu.write(0x88,0xef);
    assert_eq!((cpu.read(0x88),cpu.read(0x89)),(0xef,0xab),"shared TEMP read overwrites partial high write");

    for (cs,div) in [(1,1u64),(2,8),(3,64),(4,256),(5,1024)] {
        let mut cpu = Cpu::new(&program(&[Rjmp(-1)]));
        timer(&mut cpu,3,0,8|cs,0);
        assert_eq!(cpu.run(4*div),Stop::Cycles);
        assert_eq!(cpu.cycles,4*div);
        assert_eq!(cpu.read(0x36) & 3,2,"CTC compare prescaler {div}");
        assert_eq!((cpu.read(0x84),cpu.read(0x85)),(0,0),"CTC clear");
        cpu.write(0x36,1); assert_eq!(cpu.read(0x36) & 2,2,"W1C leaves other flag");
        cpu.write(0x36,2); assert_eq!(cpu.read(0x36) & 3,0,"W1C acknowledges compare");
    }
    let mut cpu = Cpu::new(&program(&[Nop; 8]));
    timer(&mut cpu,0,0xfffe,1,0);
    cpu.step().unwrap(); assert_eq!(cpu.read(0x36),0);
    cpu.step().unwrap(); assert_eq!(cpu.read(0x36),1,"normal overflow at MAX wrap");
    cpu.step().unwrap(); assert_eq!(cpu.read(0x36),7,"normal A/B compare one clock after equality");
    cpu.write(0x36,7); assert_eq!(cpu.read(0x36),0);
    // Changing CS does not reset the free-running synchronous prescaler.
    cpu.write(0x81,0); cpu.step().unwrap(); cpu.write(0x81,2);
    for _ in 0..4 { cpu.step().unwrap(); }
    assert_eq!(cpu.read(0x84),2,"prescaler phase retained across stop/start");

    let mut cpu = Cpu::new(&program(&[Nop,Sbic(0x16,1),Nop]));
    timer(&mut cpu,1,0,1,0);
    cpu.step().unwrap(); cpu.step().unwrap();
    assert_eq!((cpu.pc,cpu.cycles),(2,2),"I/O skip samples the flag on its first clock");

    // Lowering CTC TOP below the current count must miss the match and wrap.
    let mut cpu = Cpu::new(&program(&[Nop; 4]));
    timer(&mut cpu,2,0xfffe,9,0);
    cpu.step().unwrap(); cpu.step().unwrap();
    assert_eq!(cpu.read(0x36),1,"CTC can overflow when TOP was missed");
    // A CPU TCNT write inhibits compare for one timer clock.
    timer(&mut cpu,2,2,9,0); cpu.step().unwrap();
    assert_eq!(cpu.read(0x36),0,"TCNT write inhibits immediate compare");
    assert_eq!(cpu.read(0x84),3);

    // SEI and RETI each permit one instruction before an already pending IRQ.
    let mut a = Asm::new();
    a.emit(Bset(7)); a.emit(Inc(16)); a.emit(Inc(17)); a.emit(Break);
    a.org(22); a.emit(Reti); a.org(26); a.emit(Reti);
    let mut cpu = Cpu::new(&a.finish().unwrap());
    timer(&mut cpu,0,0xfffe,1,3);
    cpu.step().unwrap(); cpu.step().unwrap(); // overflow pending, next PC=2.
    assert_eq!(cpu.r[16],1,"instruction following SEI");
    cpu.step().unwrap(); // OVF entry; compare becomes pending during entry.
    assert_eq!((cpu.pc,cpu.cycles,cpu.sreg & 128),(26,6,0));
    assert_eq!(cpu.sp,0x8fd);
    cpu.step().unwrap(); // RETI to PC=2.
    assert_eq!((cpu.pc,cpu.cycles,cpu.sreg & 128),(2,10,128));
    cpu.step().unwrap(); assert_eq!(cpu.r[17],1,"instruction following RETI");
    cpu.step().unwrap(); assert_eq!((cpu.pc,cpu.cycles),(22,15),"pending COMPA");

    // Both flags pending: COMPA has priority over OVF.
    let mut cpu = Cpu::new(&program(&[Nop; 4]));
    timer(&mut cpu,0,0xfffe,1,3);
    for _ in 0..3 { cpu.step().unwrap(); }
    cpu.sreg = 128; cpu.step().unwrap();
    assert_eq!(cpu.pc,22,"vector priority");
    assert_eq!(cpu.read(0x36) & 1,1,"other interrupt stays pending");

    for (top,count,mode,mask) in [(4,0,9,0),(4,0,9,1),(4,0,0,3)] {
        let mut cpu = Cpu::new(&program(&[Sleep])); cpu.sreg = 128;
        timer(&mut cpu,top,count,mode,mask);
        assert_eq!(cpu.run(100),Stop::Sleep,"no timer wake source");
    }
    let mut cpu = Cpu::new(&program(&[Sleep]));
    timer(&mut cpu,4,0,9,2);
    assert_eq!(cpu.run(100),Stop::Sleep,"I clear");
}

fn check_stops() {
    let mut cpu = Cpu::new(&[0xffff]);
    assert_eq!(cpu.step(),Err(Stop::BadInsn { pc: 0 }));
    assert_eq!(cpu.cycles,0);
    cpu.pc = FLASH_WORDS as u32;
    assert_eq!(cpu.step(),Err(Stop::BadAddress { pc: FLASH_WORDS as u32, addr: FLASH_WORDS as u32 }));
    let mut cpu = Cpu::new(&program(&[Lds(0,0x900)]));
    assert_eq!(cpu.step(),Err(Stop::BadAddress { pc: 0, addr: 0x900 }));
    let mut cpu = Cpu::new(&program(&[Sts(0xffff,0)]));
    assert_eq!(cpu.step(),Err(Stop::BadAddress { pc: 0, addr: 0xffff }));
    let mut cpu = Cpu::new(&program(&[Push(0)])); cpu.sp = 0x900;
    assert_eq!(cpu.step(),Err(Stop::BadAddress { pc: 0, addr: 0x900 }));
    let mut cpu = Cpu::new(&program(&[Pop(0)]));
    assert_eq!(cpu.step(),Err(Stop::BadAddress { pc: 0, addr: 0x900 }));
    let mut cpu = Cpu::new(&program(&[Lpm])); cpu.r[31] = 0x80;
    assert_eq!(cpu.step(),Err(Stop::BadAddress { pc: 0, addr: 0x8000 }));
    let mut cpu = Cpu::new(&program(&[Nop])); cpu.read(0xffff);
    assert_eq!(cpu.step(),Err(Stop::BadAddress { pc: 0, addr: 0xffff }));
    let mut cpu = Cpu::new(&program(&[Nop])); cpu.write(0x900,1);
    assert_eq!(cpu.run(1),Stop::BadAddress { pc: 0, addr: 0x900 });
    let mut cpu = Cpu::new(&program(&[Rjmp(-1)]));
    assert_eq!(cpu.run(0),Stop::Cycles); assert_eq!(cpu.cycles,0);
    assert_eq!(cpu.run(3),Stop::Cycles); assert_eq!(cpu.cycles,4,"stop at instruction boundary");
    assert_eq!(cpu.run(2),Stop::Cycles); assert_eq!(cpu.cycles,6,"relative cycle budget");
}

#[test]
fn oracle() {
    check_arithmetic();
    check_assembler();
    check_control_and_memory();
    check_gpio();
    check_timer();
    check_stops();
}

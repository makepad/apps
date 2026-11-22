//! Label assembly with monotone relaxation of relative jumps and calls.
//!
//! `here()` is the current, provisional word address (all unresolved `Far`
//! instructions start short). Use labels or `finish_relocated()` for addresses
//! that must survive relaxation. `org` is an absolute placement constraint:
//! growing instructions consume its padding; growing past it is an error,
//! never silent relocation.

use crate::insn::{encode, Insn, Reg};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Brbs(u8), Brbc(u8), Rjmp, Rcall, Jmp, Call, Far, FarCall,
}

enum Item {
    Encoded([u16; 2], usize),
    Jump { target: Target, label: Label, long: bool },
    LdiLabel(Reg, Reg, Label),
    Org(u32),
}

/// Item boundaries before and after relaxation, including the end of the image.
#[derive(Debug)]
pub struct Relocation {
    provisional: Vec<u32>,
    positions: Vec<u32>,
}

impl Relocation {
    /// Map a provisional word address to the first item starting there, or the end.
    /// Panics with the address when it is not an item boundary.
    pub fn at(&self, p: u32) -> u32 {
        let index = self.provisional.partition_point(|&start| start < p);
        assert_eq!(self.provisional.get(index), Some(&p), "word address {p} is not an item boundary");
        self.positions[index]
    }
}

#[derive(Default)]
pub struct Asm {
    items: Vec<Item>,
    provisional: Vec<u32>,
    labels: Vec<Option<usize>>,
    here: u32,
    relaxable: usize,
    error: Option<String>,
}

impl Asm {
    pub fn new() -> Asm { Self::default() }

    pub fn label(&mut self) -> Label {
        let id = u32::try_from(self.labels.len()).expect("too many labels");
        self.labels.push(None);
        Label(id)
    }

    pub fn bind(&mut self, l: Label) {
        match self.labels.get_mut(l.0 as usize) {
            Some(slot) => {
                assert!(slot.is_none(), "label L{} bound twice", l.0);
                *slot = Some(self.items.len());
            }
            None => self.fail(format!("unknown label L{}", l.0)),
        }
    }

    pub fn here(&self) -> u32 { self.here }

    /// Number of Far/FarCall items appended so far.
    pub fn relaxable(&self) -> usize { self.relaxable }

    fn fail(&mut self, message: String) {
        if self.error.is_none() { self.error = Some(message); }
    }

    fn append(&mut self, item: Item, size: u32) {
        if let Some(next) = self.here.checked_add(size) {
            if matches!(item, Item::Jump { target: Target::Far | Target::FarCall, .. }) {
                self.relaxable += 1;
            }
            self.provisional.push(self.here);
            self.items.push(item);
            self.here = next;
        } else { self.fail("word address overflow".into()); }
    }

    pub fn emit(&mut self, i: Insn) {
        match encode(&i) {
            Ok((w,n)) => self.append(Item::Encoded(w,n), n as u32),
            Err(e) => self.fail(e),
        }
    }

    pub fn jump(&mut self, t: Target, l: Label) {
        let probe = match t {
            Target::Brbs(b) => Insn::Brbs(b,0), Target::Brbc(b) => Insn::Brbc(b,0),
            _ => Insn::Nop,
        };
        if let Err(e) = encode(&probe) { self.fail(e); }
        let long = matches!(t, Target::Jmp | Target::Call);
        self.append(Item::Jump { target: t, label: l, long }, if long { 2 } else { 1 });
    }

    /// Materialize the final word address, not the byte address used by LPM.
    pub fn ldi_label(&mut self, lo: Reg, hi: Reg, l: Label) {
        for r in [lo,hi] {
            if let Err(e) = encode(&Insn::Ldi(r,0)) { self.fail(e); }
        }
        self.append(Item::LdiLabel(lo,hi,l), 2);
    }

    pub fn word(&mut self, w: u16) { self.append(Item::Encoded([w,0],1), 1); }

    pub fn org(&mut self, word_addr: u32) {
        if word_addr < self.here {
            self.fail(format!("org {word_addr} is behind current word address {}", self.here));
        } else {
            self.append(Item::Org(word_addr), word_addr - self.here);
        }
    }

    fn positions(&self) -> Result<Vec<u32>, String> {
        let mut pos = Vec::with_capacity(self.items.len() + 1);
        let mut pc = 0u32;
        for item in &self.items {
            pos.push(pc);
            let n = match item {
                Item::Encoded(_,n) => *n as u32,
                Item::Jump { long, .. } => if *long { 2 } else { 1 },
                Item::LdiLabel(..) => 2,
                Item::Org(addr) => {
                    if pc > *addr {
                        return Err(format!("org {addr} is behind relaxed word address {pc}"));
                    }
                    *addr - pc
                }
            };
            pc = pc.checked_add(n).ok_or("word address overflow")?;
        }
        pos.push(pc);
        Ok(pos)
    }

    fn address(labels: &[Option<usize>], positions: &[u32], l: Label) -> Result<u32, String> {
        let index = labels.get(l.0 as usize).copied().flatten()
            .ok_or_else(|| format!("unbound label L{}", l.0))?;
        Ok(positions[index])
    }

    pub fn finish(self) -> Result<Vec<u16>, String> {
        self.finish_relocated().map(|(words, _)| words)
    }

    /// Finish relaxation and retain the mapping from provisional item boundaries.
    pub fn finish_relocated(mut self) -> Result<(Vec<u16>, Relocation), String> {
        if let Some(e) = self.error.take() { return Err(e); }
        let positions = loop {
            let positions = self.positions()?;
            let mut grew = false;
            // Every decision in a pass uses the same layout. A grown item never
            // shrinks, even when an org absorbs another item's growth.
            for (index,item) in self.items.iter_mut().enumerate() {
                if let Item::Jump { target: Target::Far | Target::FarCall, label, long } = item {
                    let dest = Self::address(&self.labels, &positions, *label)?;
                    let distance = dest as i64 - (positions[index] as i64 + 1);
                    if !*long && !(-2048..=2047).contains(&distance) {
                        *long = true;
                        grew = true;
                    }
                }
            }
            if !grew { break positions; }
        };
        let mut output = Vec::with_capacity(*positions.last().unwrap() as usize);
        for (index,item) in self.items.iter().enumerate() {
            match *item {
                Item::Encoded(w,n) => output.extend_from_slice(&w[..n]),
                Item::Org(addr) => output.resize(addr as usize, 0xffff),
                Item::LdiLabel(lo,hi,label) => {
                    let dest = Self::address(&self.labels, &positions, label)?;
                    if dest > 0xffff {
                        return Err(format!("label L{} word address {dest} exceeds 16-bit LDI pair", label.0));
                    }
                    for i in [Insn::Ldi(lo,dest as u8), Insn::Ldi(hi,(dest >> 8) as u8)] {
                        output.push(encode(&i)?.0[0]);
                    }
                }
                Item::Jump { target, label, long } => {
                    let dest = Self::address(&self.labels, &positions, label)?;
                    let distance = dest as i64 - (positions[index] as i64 + 1);
                    let bound = if matches!(target, Target::Brbs(_) | Target::Brbc(_)) { 64 } else { 2048 };
                    if !long && !(-bound..bound).contains(&distance) {
                        return Err(format!("label L{} distance {distance} words outside {}..={} for {target:?}",
                            label.0, -bound, bound - 1));
                    }
                    let i = match target {
                        Target::Brbs(b) => Insn::Brbs(b,distance as i8),
                        Target::Brbc(b) => Insn::Brbc(b,distance as i8),
                        Target::Rjmp => Insn::Rjmp(distance as i16),
                        Target::Rcall => Insn::Rcall(distance as i16),
                        Target::Jmp => Insn::Jmp(dest), Target::Call => Insn::Call(dest),
                        Target::Far => if long { Insn::Jmp(dest) } else { Insn::Rjmp(distance as i16) },
                        Target::FarCall => if long { Insn::Call(dest) } else { Insn::Rcall(distance as i16) },
                    };
                    let (w,n) = encode(&i).map_err(|e| format!("label L{}: {e}", label.0))?;
                    output.extend_from_slice(&w[..n]);
                }
            }
        }
        self.provisional.push(self.here);
        Ok((output, Relocation { provisional: self.provisional, positions }))
    }

    pub fn bytes(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
}

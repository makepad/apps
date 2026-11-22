//! Two-pass byte and label assembler, based on MicroPython's py/asmbase.h contract.
//!
//! The MIT License (MIT)
//!
//! Copyright (c) 2016 Damien P. George
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
//! THE SOFTWARE.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Compute,
    Emit,
}

pub struct AsmBase {
    pass: Pass,
    offset: u32,
    computed_size: u32,
    labels: Vec<Option<u32>>,
    code: Vec<u8>,
}

impl AsmBase {
    pub fn new(labels: usize) -> Self {
        Self {
            pass: Pass::Compute,
            offset: 0,
            computed_size: 0,
            labels: vec![None; labels],
            code: Vec::new(),
        }
    }

    pub fn start_pass(&mut self, pass: Pass) {
        match pass {
            Pass::Compute => {
                self.labels.fill(None);
                self.computed_size = 0;
            }
            Pass::Emit => {
                assert_eq!(self.pass, Pass::Compute, "Emit must follow Compute");
                self.computed_size = self.offset;
            }
        }
        self.pass = pass;
        self.offset = 0;
        self.code.clear();
    }

    pub fn pass(&self) -> Pass { self.pass }
    pub fn offset(&self) -> u32 { self.offset }

    pub fn label_offset(&self, label: usize) -> Option<u32> {
        self.labels[label]
    }

    pub fn label_assign(&mut self, label: usize) {
        match self.pass {
            Pass::Compute => self.labels[label] = Some(self.offset),
            Pass::Emit => assert_eq!(
                self.labels[label], Some(self.offset),
                "label {label}: Emit offset differs from Compute"
            ),
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).expect("assembler write exceeds u32");
        self.offset = self.offset.checked_add(len).expect("assembler offset exceeds u32");
        if self.pass == Pass::Emit {
            assert!(self.offset <= self.computed_size, "Emit exceeds Compute size");
            self.code.extend_from_slice(bytes);
        }
    }

    /// Reserve `len` zero bytes and return their starting offset.
    pub fn reserve(&mut self, len: u32) -> u32 {
        let start = self.offset;
        self.offset = self.offset.checked_add(len).expect("reserve offset exceeds u32");
        if self.pass == Pass::Emit {
            assert!(self.offset <= self.computed_size, "Emit exceeds Compute size");
            self.code.resize(self.offset as usize, 0);
        }
        start
    }

    /// Overwrite bytes already written in Emit; do nothing in Compute.
    pub fn patch(&mut self, offset: u32, bytes: &[u8]) {
        if self.pass == Pass::Emit {
            let len = u32::try_from(bytes.len()).expect("patch length exceeds u32");
            let end = offset.checked_add(len).expect("patch offset exceeds u32");
            assert!(end <= self.offset, "patch must overwrite already written bytes");
            self.code[offset as usize..end as usize].copy_from_slice(bytes);
        }
    }

    /// Pad with zeros to a nonzero power-of-two byte alignment.
    pub fn align(&mut self, n: u32) {
        assert!(n.is_power_of_two(), "align n must be a nonzero power of two");
        let padding = self.offset.wrapping_neg() & (n - 1);
        // No allocation is needed during Compute, even for large alignments.
        if self.pass == Pass::Compute {
            self.offset = self.offset.checked_add(padding).expect("align offset exceeds u32");
        } else {
            for _ in 0..padding { self.write(&[0]); }
        }
    }

    /// Write the low `bytesize` bytes in target (little-endian) order.
    pub fn data(&mut self, bytesize: usize, value: u64) {
        assert!((1..=8).contains(&bytesize), "data bytesize must be in 1..=8");
        self.write(&value.to_le_bytes()[..bytesize]);
    }

    pub fn code(&self) -> &[u8] { &self.code }
}

impl AsMut<AsmBase> for AsmBase {
    fn as_mut(&mut self) -> &mut AsmBase { self }
}

/// Generate the same program twice. The closure must reset any external state
/// that affects layout on each invocation. Forward references resolve in Emit.
pub fn assemble<A: AsMut<AsmBase>>(asm: &mut A, mut f: impl FnMut(&mut A)) -> Vec<u8> {
    asm.as_mut().start_pass(Pass::Compute);
    f(asm);
    let size = asm.as_mut().offset();
    asm.as_mut().start_pass(Pass::Emit);
    f(asm);
    assert_eq!(asm.as_mut().offset(), size, "Emit size differs from Compute");
    asm.as_mut().code().to_vec()
}

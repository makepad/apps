//! What a firmware image tells a co-simulated device about itself ([`FwDebug`], one per image, made
//! by its emitter), and how a machine reports its pins' use ([`PinUse`]). The device reads values
//! only from the machine (registers, RAM through the layout); nothing here evaluates source or IR.

use crate::{Owner, StepMap};

/// How firmware uses a pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinUse {
    Input,
    PullUp,
    Output,
    Pwm,
    /// A peripheral owns the pad (a UART's TX or RX, say).
    Peripheral,
}

/// A source statement of the image: its first address (None when it emitted no code), its byte
/// span, line and column (1-based) in the firmware document, and the body that owns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FwStmt {
    pub addr: Option<u32>,
    pub span: (u32, u32),
    pub line: u32,
    pub col: u32,
    pub owner: Owner,
}

/// A source condition of the image (an `if`'s, a `while`'s, the left side of a short-circuit
/// `&&` or `||`): the address of the conditional branch that tests it, its byte span and 1-based
/// line in the firmware document, and the body that owns it. Inlined code has a copy per call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FwCond {
    pub pc: u32,
    pub span: (u32, u32),
    pub line: u32,
    pub owner: Owner,
    /// The branch is taken when the condition is true (false: when it is false).
    pub taken_true: bool,
}

/// A handler (module entry, setup included): its name and byte span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FwHandler {
    pub name: String,
    pub span: (u32, u32),
}

/// A shared helper, indexed by function number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FwFunc {
    pub name: String,
    pub span: (u32, u32),
}

/// A program variable's value type, as the compiler laid it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FwTy {
    I32,
    F32,
    Bool,
    /// An array of `len` scalars of one type, a word each.
    Array(FwScalar, u32),
    /// Another layout, shown as its words.
    Words(u32),
}

/// An array's element type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FwScalar {
    I32,
    F32,
    Bool,
}

/// A variable's value read from the chip's RAM.
#[derive(Clone, Debug, PartialEq)]
pub enum FwValue {
    I32(i32),
    F32(f32),
    Bool(bool),
    Words(Vec<u32>),
    /// The machine could not read its bytes.
    Unread,
}

/// A program variable: its name, type and data address (little-endian words from there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FwVar {
    pub name: String,
    /// The declaration's byte span in the firmware document, or (0, 0) if absent.
    pub span: (u32, u32),
    pub ty: FwTy,
    pub addr: u32,
}

impl FwVar {
    /// Its size in bytes.
    pub fn bytes(&self) -> usize {
        match self.ty {
            FwTy::I32 | FwTy::F32 | FwTy::Bool => 4,
            FwTy::Array(_, n) | FwTy::Words(n) => n as usize * 4,
        }
    }

    /// Its value from its bytes.
    pub fn decode(&self, bytes: &[u8]) -> FwValue {
        let word = |k: usize| u32::from_le_bytes([bytes[4 * k], bytes[4 * k + 1], bytes[4 * k + 2], bytes[4 * k + 3]]);
        if bytes.len() < self.bytes() {
            return FwValue::Unread;
        }
        match self.ty {
            FwTy::I32 => FwValue::I32(word(0) as i32),
            FwTy::F32 => FwValue::F32(f32::from_bits(word(0))),
            FwTy::Bool => FwValue::Bool(word(0) != 0),
            FwTy::Array(_, n) | FwTy::Words(n) => FwValue::Words((0..n as usize).map(word).collect()),
        }
    }
}

/// An unobserved image's fault counter: a saturating u16 count followed by its last u16 site,
/// both little-endian, at a machine data address. The code is the watch protocol's fault code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FwFaultCell {
    pub code: u8,
    pub addr: u32,
}

/// One image's table, shared by every state the device publishes (one `Arc` per image).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FwDebug {
    /// The id the image's Hello frame carries.
    pub image_id: u32,
    /// The firmware document revision the image was built from (the host's).
    pub source_rev: u64,
    /// Sample rate in Hz from this program's `mic(...)` declaration, when present.
    pub mic_rate: Option<u32>,
    /// Where stepping and the observations stop; None: the image has no usable map.
    pub map: Option<StepMap>,
    /// The statements, indexed by statement id.
    pub stmts: Vec<FwStmt>,
    /// (address, statement ids beginning there, outermost first), ascending by address.
    pub starts: Vec<(u32, Vec<u32>)>,
    /// The source conditions by their branch's address, ascending; one per branch.
    pub conds: Vec<FwCond>,
    /// The handlers, indexed by handler number.
    pub handlers: Vec<FwHandler>,
    pub funcs: Vec<FwFunc>,
    /// Each port site's byte span (what a watch frame's `site` names), by site number.
    pub sites: Vec<(u32, u32)>,
    /// The program's variables.
    pub vars: Vec<FwVar>,
    /// RAM fault cells when watch is off; empty when faults use the stream.
    pub fault_cells: Vec<FwFaultCell>,
    /// The runtime's named routines by start address, ascending; it ends where the first handler's
    /// code begins (`runtime_end`).
    pub runtime: Vec<(u32, &'static str)>,
    pub runtime_end: u32,
}

impl FwDebug {
    pub fn owner_name(&self, owner: Owner) -> Option<&str> {
        match owner {
            Owner::Handler(k) => self.handlers.get(k as usize).map(|h| h.name.as_str()),
            Owner::Func(k) => self.funcs.get(k as usize).map(|f| f.name.as_str()),
        }
    }

    /// The statement ids that begin at `addr`, outermost first.
    pub fn starting_at(&self, addr: u32) -> &[u32] {
        match self.starts.binary_search_by_key(&addr, |s| s.0) {
            Ok(k) => &self.starts[k].1,
            Err(_) => &[],
        }
    }

    /// The condition whose branch is at `pc`: its index in `conds`.
    pub fn cond_at(&self, pc: u32) -> Option<u32> {
        self.conds.binary_search_by_key(&pc, |c| c.pc).ok().map(|k| k as u32)
    }

    /// The runtime routine holding `pc`, if `pc` is in the runtime.
    pub fn routine(&self, pc: u32) -> Option<&'static str> {
        if pc >= self.runtime_end {
            return None;
        }
        let k = self.runtime.partition_point(|r| r.0 <= pc);
        k.checked_sub(1).map(|k| self.runtime[k].1)
    }
}

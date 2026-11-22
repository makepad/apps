//! Phase 6a: verified AIR to CALL0 handlers. No FSR reads or trap enables.
//! The caller enables CP0 and sets FCR=0 (nearest/even, traps disabled).
//! a0 returns, a1 is a 16-byte-aligned stack, a2/a3 are state/shared bases.
//! Ports take arguments/results in a2..a9 and the AIR site in a10; a11 is
//! the call target. Ports run to completion and preserve a1 and a12..a15.
//! Within a handler a12/a13 hold state/shared; a14/a15 are saved if allocated.
use fab_elec_mcu_emit32::{asm::Pass, bounds::bounds, control::{steps, pure, reads, Instruction, Step, Analysis, BitField, CountedLoop}, literals::{Literal, LoopLiterals}, regalloc::{self, Interval, Location}, xtensa::{assemble_relaxed, cc, ccz, enc, Asm, Reg}};
use fab_elec_mcu_rt::{format::*, module::{Buffer, DeclArg, Elem, Module}, ports};

use crate::{chip, float, insns::{self, Count}};
use crate::radio::kernel::{scalar_access, KernelRegion, PortCost, ProvenAccess, ScalarAccess};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Options {
    /// Word-aligned link address of the returned bytes (including literals).
    pub base: u32,
    /// (port number, CALL0 routine address). Required only for used ports.
    pub ports: Vec<(u16, u32)>,
    pub sites: Vec<(u32, u32)>,
    /// Ascending (AIR code index, statement ID), as emitted by the compiler.
    pub marks: Vec<(u32, u32)>,
    /// Word-aligned data addresses, in RAM or read-only flash, in module
    /// buffer-number order.
    pub buffers: Vec<u32>,
}

impl Default for Options {
    fn default() -> Self {
        Self { base: crate::SLOT_BASE, ports: Vec::new(), sites: Vec::new(), marks: Vec::new(), buffers: Vec::new() }
    }
}

/// One kernel port's contribution to the path selected for an entry's bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortCharge {
    pub port: u16,
    pub calls: u64,
    /// Complete call allowances, including CALLX0, summed across charged sites.
    pub insns: u64,
    /// Instructions in the region walks within those allowances.
    pub walk: u64,
}

impl PortCharge {
    pub fn name(&self) -> &'static str { port_def(self.port).expect("verified kernel port").name }
}

#[derive(Clone, Debug)]
pub struct EntryReport {
    pub name: String,
    pub kind: EntryKind,
    /// Entry offset into Code::bytes; add Code::base to call it.
    pub offset: u32,
    pub code_bytes: u32,
    pub stack_bytes: u32,
    pub slot_bytes: u32,
    /// Final emitted instruction bound, including wrapper/frame clear and
    /// port allowances. Handlers retain their self-feeding exclusions;
    /// exports count raw copy/fill loops at each call's constant maximum.
    pub max_insns: u64,
    /// Kernel port charges in port-number order. Empty for app handlers.
    pub port_charges: Vec<PortCharge>,
    /// Scalar kernel accesses proved at build time, ordered by absolute instruction address.
    pub proven_accesses: Vec<ProvenAccess>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind { Handler, Export { params: Vec<u32>, result: u32 } }

#[derive(Clone, Copy)]
pub(crate) enum Domain<'a> {
    App,
    Kernel { state: u32, costs: &'a [(u16, PortCost)], regions: &'a [KernelRegion], inline_constants: bool },
}

fn port_def(n: u16) -> Option<&'static ports::PortDef> {
    if n >= ports::KERNEL_BASE { ports::KERNEL_PORTS.get((n - ports::KERNEL_BASE) as usize) }
    else { ports::PORTS.get(n as usize) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortLabel {
    pub port: u16,
    pub name: &'static str,
    pub address: u32,
}

#[derive(Clone, Debug)]
pub struct Code {
    pub base: u32,
    pub bytes: Vec<u8>,
    pub entries: Vec<EntryReport>,
    pub ports: Vec<PortLabel>,
    pub map: SourceMap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    Entry { entry: usize, code: Option<u32>, site: Option<u32>, mark: Option<u32> },
    Outside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpSpan {
    pub start: u32,
    pub end: u32,
    pub entry: usize,
    pub code: u32,
    pub site: Option<u32>,
    pub mark: Option<u32>,
}

/// Absolute byte PCs. Literal tables and inter-handler padding are Outside.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMap {
    pub entries: Vec<(u32, u32)>,
    pub ops: Vec<OpSpan>,
    /// Repeated loop ranges, from the body label through EndLoop's tail.
    /// `code` identifies the original OP_LOOP; nested ranges may overlap.
    pub loops: Vec<OpSpan>,
    /// (Byte address, statement id), ascending: first emitted op at or after each mark in its
    /// entry, outermost first at one address. A mark with no following emitted op has no address.
    pub stmt_starts: Vec<(u32, u32)>,
}

impl SourceMap {
    pub fn find(&self, pc: u32) -> Where {
        let Some(entry) = self.entries.iter().position(|&(a, b)| (a..b).contains(&pc)) else {
            return Where::Outside;
        };
        let at = self.ops.partition_point(|s| s.end <= pc);
        let op = self.ops.get(at).filter(|s| s.start <= pc && s.entry == entry);
        Where::Entry { entry, code: op.map(|s| s.code), site: op.and_then(|s| s.site), mark: op.and_then(|s| s.mark) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal { pub entry: usize, pub code: u32, pub message: String }

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}

fn error(m: &Module<'_>, k: usize, at: u32, reason: impl std::fmt::Display) -> Refusal {
    let name = m.entries().get(k).map_or("<module>", |e| m.string(e.name));
    Refusal { entry: k, code: at, message: format!("entry `{name}`, code {at}: {reason}") }
}

pub fn build_handlers(m: &Module<'_>, o: &Options) -> Result<Code, Refusal> {
    if !m.funcs().is_empty() { return Err(crate::image::refusal(crate::image::FUNCTIONS_UNSUPPORTED)); }
    build_domain(m, o, Domain::App)
}

pub(crate) fn build_domain(m: &Module<'_>, o: &Options, domain: Domain<'_>) -> Result<Code, Refusal> {
    if matches!(domain, Domain::App) != m.kernel_lines().is_none() {
        return Err(error(m, 0, 0, "module domain does not match the builder"));
    }
    if o.base & 3 != 0 { return Err(error(m, 0, 0, "code base must be word aligned")); }
    if o.buffers.len() != m.buffers().len() || m.buffers().iter().zip(&o.buffers).any(|(b, &address)|
        address & 3 != 0 || u64::from(address) + b.words() as u64 * 4 > u64::from(u32::MAX))
    { return Err(error(m, 0, 0, "every buffer needs a word-aligned allocation within 32-bit addresses")); }
    if o.marks.windows(2).any(|w| w[0].0 > w[1].0) {
        return Err(error(m, 0, 0, "statement marks must be in AIR code order"));
    }
    for (n, &(port, address)) in o.ports.iter().enumerate() {
        if port_def(port).is_none() || (port >= ports::KERNEL_BASE) != matches!(domain, Domain::Kernel { .. })
            || o.ports[..n].iter().any(|p| p.0 == port) {
            return Err(error(m, 0, 0, "unknown or duplicate port address"));
        }
        if address & 3 != 0 { return Err(error(m, 0, 0, "port routine address must be word aligned")); }
    }
    let mut out = Code { base: o.base, bytes: Vec::new(), entries: Vec::new(), ports: Vec::new(), map: SourceMap::default() };
    for k in 0..m.entries().len() {
        let plan = Plan::new(m, k, o, domain)?;
        if u64::from(plan.bytes) + u64::from(m.state_words()) * 4 > u64::from(chip::profile().ram_words) * 4 {
            return Err(error(m, k, m.entries()[k].body + 1, "state and handler stack exceed the S3's 384 KiB module RAM budget"));
        }
        while out.bytes.len() & 3 != 0 { out.bytes.push(0); }
        let offset = u32::try_from(out.bytes.len()).map_err(|_| error(m, k, 0, "code size exceeds 32 bits"))?;
        let link = o.base.checked_add(offset).ok_or_else(|| error(m, k, 0, "code address overflows"))?;
        let mut asm = Asm::new(plan.steps.len() * 64 + 128, 11);
        // A counting-only traversal checks reach before the checked assembler
        // sees any real displacements. 120 KiB bounds every J and L32R in a
        // handler, including its table (ISA pp. 450–451, 461–462).
        asm.base.start_pass(Pass::Compute);
        let count = Emitter::new(&mut asm, &plan, o, domain).emit();
        let size = u64::from(asm.base.offset()) + u64::from(count.literals) * 4 + 4;
        if size > 120 * 1024 {
            return Err(error(m, k, m.entries()[k].body + 1,
                "handler and literals exceed the 120 KiB branch/literal reach limit"));
        }
        if u64::from(link) + size > u64::from(u32::MAX) {
            return Err(error(m, k, m.entries()[k].body + 1, "code address overflows"));
        }
        let mut emission = count;
        let bytes = assemble_relaxed(&mut asm, |a| { emission = Emitter::new(a, &plan, o, domain).emit(); });
        let (max_insns, port_charges) = match domain {
            Domain::App => insns::bound(&bytes, &emission.counts, plan.bytes).map(|total| (total, Vec::new())),
            Domain::Kernel { costs, .. } => insns::attributed_bound(&bytes, &emission.counts, plan.bytes).map(|(total, counts)| {
                let charges = counts.into_iter().map(|(port, count)| {
                    let (_, cost) = costs.iter().find(|c| c.0 == port).expect("emitted kernel port cost");
                    let walk = count.calls.checked_mul(cost.walk).expect("region walks fit within charged port instructions");
                    PortCharge { port, calls: count.calls, insns: count.insns, walk }
                }).collect();
                (total, charges)
            }),
        }.ok_or_else(|| error(m, k, m.entries()[k].body + 1, "handler instruction bound exceeds u64"))?;
        let entry_offset = offset + emission.entry;
        let end = link + bytes.len() as u32;
        out.map.entries.push((o.base + entry_offset, end));
        let entry = &m.entries()[k];
        // Exclude a preceding entry's terminal mark at this entry's header.
        let first = o.marks.partition_point(|&(at, _)| at < entry.body + 1);
        let end = o.marks.partition_point(|&(at, _)| at <= entry.code + entry.len);
        let marks = &o.marks[first..end];
        for &(start, end, i) in &emission.spans {
            let before = marks.partition_point(|&(at, _)| at <= i.at);
            let mark = before.checked_sub(1).map(|n| marks[n].1);
            out.map.ops.push(OpSpan { start: link + start, end: link + end, entry: k,
                code: i.at, site: (i.op == OP_PORT).then(|| i.args[1]), mark });
            if i.op == OP_PORT {
                let port = i.args[0] as u16;
                if !out.ports.iter().any(|p| p.port == port) {
                    out.ports.push(PortLabel { port, name: port_def(port).expect("verified port").name,
                        address: o.ports.iter().find(|p| p.0 == port).unwrap().1 });
                }
            }
        }
        for &(start, end, i) in &emission.loops {
            let before = marks.partition_point(|&(at, _)| at <= i.at);
            out.map.loops.push(OpSpan { start: link + start, end: link + end, entry: k,
                code: i.at, site: None, mark: before.checked_sub(1).map(|n| marks[n].1) });
        }
        for &(at, id) in marks {
            let next = emission.spans.partition_point(|&(_, _, i)| i.at < at);
            if let Some(&(start, _, _)) = emission.spans.get(next) {
                out.map.stmt_starts.push((link + start, id));
            }
        }
        let kind = match domain {
            Domain::App => EntryKind::Handler,
            Domain::Kernel { state, .. } => {
                let decl = m.decls().iter().find(|d| matches!(d.args.first(), Some(DeclArg::Entry(e)) if *e as usize == k))
                    .ok_or_else(|| error(m, k, 0, "entry has no export declaration"))?;
                let [DeclArg::Entry(_), DeclArg::Int(n)] = decl.args.as_slice() else { unreachable!("verified export"); };
                let name = m.string(decl.name);
                let slot = |suffix: &str| -> Result<u32, Refusal> {
                    let name = format!("{name}#{suffix}");
                    let var = m.vars().iter().find(|v| m.string(v.name) == name)
                        .ok_or_else(|| error(m, k, 0, "export slot is missing"))?;
                    var.offset.checked_mul(4).and_then(|offset| state.checked_add(offset))
                        .ok_or_else(|| error(m, k, 0, "export slot address overflows"))
                };
                EntryKind::Export { params: (0..*n).map(|p| slot(&format!("p{p}"))).collect::<Result<_, _>>()?, result: slot("r")? }
            }
        };
        out.entries.push(EntryReport { name: m.string(m.entries()[k].name).to_owned(), kind, offset: entry_offset,
            code_bytes: bytes.len() as u32 - emission.entry, stack_bytes: plan.bytes, slot_bytes: plan.slot_bytes,
            max_insns, port_charges, proven_accesses: emission.proven_accesses.into_iter()
                .map(|access| ProvenAccess { pc: link + access.pc, ..access }).collect() });
        out.bytes.extend(bytes);
    }
    out.map.stmt_starts.sort_by_key(|s| s.0);
    out.ports.sort_by_key(|p| p.port);
    Ok(out)
}

/// Plan frames before emitting, so the image builder can report the entire
/// SRAM map even when the largest frame alone would exceed the pool.
pub(crate) fn max_stack_bytes(m: &Module<'_>, o: &Options) -> Result<u32, Refusal> {
    let mut largest = 0;
    for k in 0..m.entries().len() { largest = largest.max(Plan::new(m, k, o, Domain::App)?.bytes); }
    Ok(largest)
}

pub(crate) fn uses_port(m: &Module<'_>, port: u16) -> bool {
    (0..m.entries().len()).any(|k| steps(m, k).iter().any(|step|
        matches!(step, Step::Op(i) if i.op == OP_PORT && i.args[0] == u32::from(port))))
}

// a8/a11 are the only scratch inside the allocated body. a0's return address
// is saved before allocation starts. Float helpers clobber the same
// registers as ports. Restrict even their input/output endpoints to preserved registers or
// spills: argument/result moves then cannot overwrite another live operand.
const VOLATILE: &[Reg] = &[4, 5, 6, 7, 9, 10, 0, 2, 3];
const PRESERVED: &[Reg] = &[14, 15];

fn clobbers(i: Instruction<'_>) -> bool {
    i.op == OP_PORT || i.op == OP_CMP_F
        || (i.op == OP_UN && !matches!(i.sub, UN_NEG_I | UN_NOT_B | UN_BITS_FI | UN_BITS_IF))
        || (i.op == OP_BIN && matches!(i.sub, BIN_ADD_F | BIN_SUB_F | BIN_MUL_F | BIN_DIV_F | BIN_MIN_F | BIN_MAX_F))
}

#[derive(Clone, Copy)]
enum Immediate { Add(i32), Shift(u8) }

// The remaining source and the chosen immediate form.
fn immediate(i: Instruction<'_>, constants: &[Option<u32>]) -> Option<(u32, Immediate)> {
    if i.op != OP_BIN { return None; }
    let a = i.args;
    if matches!(i.sub, BIN_SHL_I | BIN_SHR_I | BIN_SHR_UI) {
        return constants[a[2] as usize].map(|c| (a[1], Immediate::Shift((c & 31) as u8)));
    }
    for (constant, source) in [(2, 1), (1, 2)] {
        let Some(c) = constants[a[constant] as usize] else { continue; };
        if i.sub == BIN_ADD_I && (-128..=127).contains(&(c as i32)) {
            return Some((a[source], Immediate::Add(c as i32)));
        }
    }
    None
}

#[derive(Clone, Copy)]
enum CompareOperand { Value(u32), Immediate(i32) }
#[derive(Clone, Copy)]
struct Compare { cond: u8, left: u32, right: CompareOperand }

fn compare(i: Instruction<'_>, constants: &[Option<u32>]) -> Compare {
    let a = i.args;
    for (left, right, sub) in [(a[1], a[2], i.sub), (a[2], a[1], match i.sub {
        CMP_LT => CMP_GT, CMP_LE => CMP_GE, CMP_GT => CMP_LT, CMP_GE => CMP_LE, sub => sub,
    })] {
        let Some(c) = constants[right as usize] else { continue; };
        let c = c as i32;
        let (cond, constant) = match sub {
            CMP_LT => (cc::LT, Some(c)), CMP_GE => (cc::GE, Some(c)),
            CMP_EQ => (cc::EQ, Some(c)), CMP_NE => (cc::NE, Some(c)),
            CMP_GT => (cc::GE, c.checked_add(1)), _ => (cc::LT, c.checked_add(1)),
        };
        if let Some(c) = constant.filter(|&c| c == 0 || enc::b4const(c).is_some()) {
            return Compare { cond, left, right: CompareOperand::Immediate(c) };
        }
    }
    let (cond, left, right) = match i.sub {
        CMP_LT => (cc::LT, a[1], a[2]), CMP_GE => (cc::GE, a[1], a[2]),
        CMP_LE => (cc::GE, a[2], a[1]), CMP_GT => (cc::LT, a[2], a[1]),
        CMP_EQ => (cc::EQ, a[1], a[2]), _ => (cc::NE, a[1], a[2]),
    };
    Compare { cond, left, right: CompareOperand::Value(right) }
}

#[derive(Clone, Copy, Default)]
struct Live { start: Option<u32>, end: u32, weight: u64 }
impl Live {
    fn access(&mut self, start: u32, end: u32, weight: u64) {
        self.start = Some(self.start.map_or(start, |at| at.min(start)));
        self.end = self.end.max(end);
        self.weight = self.weight.saturating_add(weight);
    }
}

struct Plan<'a> {
    steps: Vec<Step<'a>>,
    control: Analysis<'a>,
    bitfields: Vec<Option<BitField>>,
    buffers: Vec<Buffer>,
    values: Vec<Option<Location>>,
    constants: Vec<Option<u32>>,
    /// AIR instruction index to its proved address and scalar port operation.
    inline_accesses: BTreeMap<u32, (u32, ScalarAccess)>,
    bounds: Vec<Option<u32>>,
    literals: LoopLiterals,
    literal_locations: Vec<Location>,
    vars: Vec<Option<Location>>,
    frame: u32,
    counters: Vec<(u32, Location)>,
    saved: Vec<Reg>,
    zero_regs: Vec<Reg>,
    spill_base: u32,
    bytes: u32,
    slot_bytes: u32,
}

impl<'a> Plan<'a> {
    fn new(m: &Module<'a>, k: usize, o: &Options, domain: Domain<'_>) -> Result<Self, Refusal> {
        let e = &m.entries()[k];
        let steps = steps(m, k);
        let mut control = Analysis::new(m, k, &steps);
        let bounds = bounds(m, k, &steps, &control);
        let mut literal_uses = vec![Vec::new(); steps.len()];
        // ISA Summary p. 429: EXTUI extracts a zero-extended field. shiftimm
        // is 0..31 (bits 16, 11:8), maskimm is 1..16 (op2 = width - 1).
        // shift + width > 32 is undefined/reserved; Analysis leaves that
        // shift intact. Wider masks retain their ordinary AND operand.
        let bitfields: Vec<_> = control.bitfields.iter().map(|field|
            field.filter(|f| f.width <= 16)).collect();
        for field in bitfields.iter().flatten() {
            if let Some(n) = field.shift_step { control.omit[n] = true; }
        }
        let constants = control.constants.clone();
        let mut inline_accesses = BTreeMap::new();
        let nv = e.n_vals as usize;
        let nc = nv + e.n_vars as usize;
        let mut lives = vec![Live::default(); nc];
        let mut needed = vec![false; nv];
        let mut loops = Vec::<(u32, u32)>::new();
        let mut counters = Vec::new();
        let mut calls = Vec::new();
        let mut branches = 0usize;
        for (n, step) in steps.iter().enumerate() {
            // Validate all original operations, even those made redundant by
            // the control-flow plan. Structural markers omitted with a head
            // or transfer must not alter the retained scope stacks.
            if control.omit[n] && !matches!(step, Step::Op(_)) { continue; }
            let i = match *step {
                Step::Op(i) => i,
                Step::EndLoop => { loops.pop(); continue; }
                Step::EndIf => { branches -= 1; continue; }
                Step::Else => continue,
            };
            let a = i.args;
            let f64_use = i.op == OP_CONST_D || i.op == OP_CMP_D
                || (pure(i) && m.ty(k, a[0], false) == TY_F64)
                || reads(i).iter().any(|&v| m.ty(k, v, false) == TY_F64)
                || (matches!(i.op, OP_GET | OP_SET) && m.ty(k, a[usize::from(i.op == OP_GET)], true) == TY_F64);
            if f64_use {
                return Err(error(m, k, i.at, "f64 is not available on the ESP32-S3 yet"));
            }
            if matches!(i.op, OP_LOAD | OP_STORE) && i.sub < REGION_BUF_BASE {
                let (base, extent) = if i.op == OP_LOAD { (a[1], a[2]) } else { (a[0], a[1]) };
                if u64::from(base) + u64::from(extent) > u64::from(u32::MAX / 4) {
                    return Err(error(m, k, i.at, "region byte offset exceeds 32 bits"));
                }
            }
            if i.op == OP_PORT {
                let name = port_def(a[0] as u16).expect("verified port").name;
                if !o.ports.iter().any(|p| u32::from(p.0) == a[0]) {
                    let site = o.sites.get(a[1] as usize).map_or_else(|| format!("site {}", a[1]),
                        |(line, col)| format!("line {line}:{col}"));
                    return Err(error(m, k, i.at, format!("no CALL0 routine supplied for `{name}` at {site}")));
                }
                if a[2] > 8 || a[3] > 8 {
                    return Err(error(m, k, i.at, "port arity exceeds a2..a9"));
                }
                if let Domain::Kernel { regions, inline_constants, .. } = domain {
                    // Verified value IDs have one definition, visible at each use.
                    let constant = scalar_access(a[0] as u16).and_then(|access|
                        constants[a[4] as usize].map(|address| (address, access)));
                    if let Some((address, access)) = constant {
                        access.check(address, regions).map_err(|reason| error(m, k, i.at, format!(
                            "{name} at {address:#010x}, width {}, class {:?}: {reason}", access.width, access.class)))?;
                        if inline_constants { inline_accesses.insert(i.at, (address, access)); }
                    }
                }
            }
            if control.omit[n] { continue; }
            if matches!(i.op, OP_LOAD | OP_STORE) {
                let (base, extent, index) = if i.op == OP_LOAD { (a[1], a[2], a[3]) } else { (a[0], a[1], a[2]) };
                let max = if i.sub >= REGION_BUF_BASE {
                    let k = if i.sub == REGION_BUF_LONG { a[4] } else { i.sub - REGION_BUF_BASE } as usize;
                    let buffer = m.buffers()[k];
                    let base = base.min(buffer.len - 1);
                    literal_uses[n].push((Literal::BufferBase { buffer: k as u32, element: base },
                        o.buffers[k] + base * buffer.elem.bytes()));
                    // At the last element the offset cannot affect the address.
                    if base == buffer.len - 1 { None } else { Some(buffer.len - 1 - base) }
                } else { Some(extent.saturating_sub(1)) };
                if let Some(max) = max {
                    if index != NONE && !bounds[index as usize].is_some_and(|b| b <= max) {
                        literal_uses[n].push((Literal::Clamp(max), max));
                    }
                }
            }
            let weight = 8u64.saturating_pow(loops.len() as u32);
            let inline = i.op == OP_PORT && inline_accesses.contains_key(&i.at);
            let operands = if inline {
                // Only the data operand of a store needs a location.
                a[5..4 + a[2] as usize].to_vec()
            } else if let Some(field) = bitfields[n] {
                vec![field.source]
            } else if let Some(cmp) = control.fused[n] {
                let c = compare(cmp, &constants);
                match c.right { CompareOperand::Value(v) => vec![c.left, v],
                    CompareOperand::Immediate(_) => vec![c.left] }
            } else if let Some((source, _)) = immediate(i, &constants) {
                // x + x still reads x when one occurrence is an immediate.
                vec![source]
            } else { reads(i) };
            for v in operands {
                needed[v as usize] = true;
                let id = control.aliases[v as usize].map_or(v as usize, |var| nv + var as usize);
                let live = &mut lives[id];
                let def = live.start.expect("verified definition before use");
                let until = loops.iter().filter(|(start, _)| def < *start)
                    .map(|(_, end)| end - 1).max().unwrap_or(i.at).max(i.at);
                live.access(def, until, weight);
            }
            if pure(i) { lives[a[0] as usize].access(i.at, i.at, 0); }
            if i.op == OP_PORT {
                for &v in &a[4 + a[2] as usize..] { lives[v as usize].access(i.at, i.at, 0); }
            }
            if matches!(i.op, OP_GET | OP_SET) {
                let v = a[usize::from(i.op == OP_GET)] as usize;
                let live = &mut lives[nv + v];
                // A first unconditional SET defines the initial value. Other
                // vars may be read before any SET (including a skipped branch
                // or loop), so their zero definition lives from handler entry.
                // Extending to the outer loop's end preserves loop-carried vars.
                let first = if live.start.is_none() && (i.op == OP_GET || !loops.is_empty() || branches != 0) {
                    0
                } else { i.at };
                let end = loops.first().map_or(i.at, |&(_, end)| end - 1);
                live.access(first, end, weight);
            }
            if !inline && clobbers(i) { calls.push(i.at); }
            if i.op == OP_IF && control.transfers[n].is_none() { branches += 1; }
            if i.op == OP_LOOP {
                if let Some(c) = control.counted[n] {
                    lives[nv + c.var as usize].access(i.at, i.end - 1, weight.saturating_mul(16));
                }
                if control.counted[n].is_none_or(|c| c.needs_counter) {
                    counters.push(i.at);
                    lives.push(Live { start: Some(i.at), end: i.end - 1,
                        // Two counter uses per iteration: test and decrement.
                        weight: weight.saturating_mul(16), });
                }
                loops.push((i.at, i.end));
            }
        }
        // Folded-only constants emit no definition and consume no location.
        for v in 0..nv {
            if constants[v].is_some() && !needed[v] { lives[v].start = None; }
        }
        let mut ids = Vec::new();
        let mut intervals = Vec::new();
        for (id, live) in lives.iter().enumerate() {
            if let Some(start) = live.start {
                let call = calls.partition_point(|&at| at < start);
                ids.push(id);
                intervals.push(Interval { start, end: live.end, weight: live.weight,
                    crosses_call: calls.get(call).is_some_and(|&at| at <= live.end),
                    rematerialize: constants.get(id).copied().flatten() });
            }
        }
        let literals = LoopLiterals::new(&steps, &literal_uses, &calls);
        let allocation = regalloc::allocate(&intervals, VOLATILE, PRESERVED);
        let literal_locations = regalloc::fill(&intervals, &allocation.locations, &literals.intervals, VOLATILE, PRESERVED);
        let mut locations = vec![None; lives.len()];
        for (id, location) in ids.into_iter().zip(allocation.locations) { locations[id] = Some(location); }
        for (v, var) in control.aliases.iter().enumerate() {
            if let Some(var) = var { locations[v] = locations[nv + *var as usize]; }
        }
        let saved: Vec<_> = PRESERVED.iter().copied().filter(|&reg|
            locations.contains(&Some(Location::Register(reg))) || literal_locations.contains(&Location::Register(reg))).collect();
        let zero_regs = (nv..nc).filter(|&v| lives[v].start == Some(0)).filter_map(|v|
            match locations[v] { Some(Location::Register(reg)) => Some(reg), _ => None }).collect();
        let spill_base = if saved.len() == 2 { 32 } else { 16 };
        let slots = allocation.spill_slots as u64 * 4;
        let frame = u64::from(spill_base) + slots;
        let bytes = (frame + u64::from(e.frame_words) * 4 + 15) & !15;
        let bytes = u32::try_from(bytes).map_err(|_| error(m, k, e.body + 1, "handler frame byte size exceeds 32 bits"))?;
        let counters = counters.into_iter().enumerate().map(|(n, at)| (at, locations[nc + n].unwrap())).collect();
        Ok(Self { steps, control, bitfields, buffers: m.buffers().to_vec(), values: locations[..nv].to_vec(), constants,
            inline_accesses, bounds, literals, literal_locations,
            vars: locations[nv..nc].to_vec(), frame: frame as u32, counters, saved, zero_regs,
            spill_base, bytes, slot_bytes: slots as u32 })
    }
}

/// Small target-specific primitives shared with the emitted fmath ports.
/// a11 is reserved for addresses, constants and immediate shift amounts.
pub(crate) struct Body<'a> {
    pub(crate) a: &'a mut Asm,
    next: usize,
    literals: u32,
    counts: Vec<Count>,
}

impl Body<'_> {
    // The enclosing op counts the first visit; add only repeated visits here.
    pub(crate) fn repeat_extra(&mut self, start: u32, end: u32, times: u64) {
        self.counts.push(Count::Span { start, end, times });
    }
    pub(crate) fn label(&mut self) -> usize { let n = self.next; self.next += 1; n }
    pub(crate) fn bind(&mut self, n: usize) { self.a.base.label_assign(n); }
    pub(crate) fn jump(&mut self, n: usize) { self.a.j_label(n); }
    pub(crate) fn branch(&mut self, cond: u8, a: Reg, b: Reg, n: usize) { self.a.bcc_reg_reg_label(cond, a, b, n); }
    pub(crate) fn zero(&mut self, cond: u8, a: Reg, n: usize) { self.a.bccz_reg_label(cond, a, n); }
    pub(crate) fn imm(&mut self, r: Reg, value: u32) {
        if !(-2048..=2047).contains(&(value as i32)) { self.literals += 1; }
        self.a.mov_reg_i32_optimised(r, value);
    }
    pub(crate) fn shl(&mut self, dst: Reg, src: Reg, amount: u32) {
        self.imm(11, amount); self.a.ssl(11); self.a.sll(dst, src);
    }
    pub(crate) fn shr(&mut self, dst: Reg, src: Reg, amount: u32) {
        self.imm(11, amount); self.a.ssr(11); self.a.srl(dst, src);
    }
    fn load(&mut self, dst: Reg, base: Reg, offset: u32) {
        if offset <= 1020 { self.a.l32i(dst, base, offset); }
        else {
            // Use the destination for the address, preserving the other
            // scratch operand even when both sources live in distant slots.
            assert_ne!(dst, base, "large load needs a distinct address base");
            self.imm(dst, offset); self.a.add_n(dst, base, dst); self.a.l32i(dst, dst, 0);
        }
    }
    fn store(&mut self, src: Reg, base: Reg, offset: u32) {
        if offset <= 1020 { self.a.s32i(src, base, offset); }
        else {
            let scratch = if src == 11 { 8 } else { 11 };
            assert_ne!(scratch, base, "large store needs a distinct address base");
            self.imm(scratch, offset); self.a.add_n(scratch, base, scratch); self.a.s32i(src, scratch, 0);
        }
    }
    // rt::exec: division by zero is 0, remainder by zero is the dividend;
    // MIN/-1 wraps to MIN (remainder zero). No divide exception reaches the CPU.
    // Every branch is forward; unlike float div/sqrt, this needs no repeated
    // span in the emitted instruction bound (QUOS/REMS each retire once).
    fn divide(&mut self, dst: Reg, dividend: Reg, divisor: Reg, remainder: bool) {
        let zero = self.label(); let normal = self.label(); let done = self.label();
        self.zero(ccz::EQ, divisor, zero);
        // BNEI's -1 immediate (ISA Summary Table 15 p. 64, pp. 365–366) avoids
        // needing a third temporary when both inputs are in a8/a11. NEG
        // wraps MIN to MIN; all other x/-1 cases have the same result.
        self.a.bcci_reg_label(cc::NE, divisor, -1, normal);
        if remainder { self.imm(dst, 0); } else { self.a.neg(dst, dividend); }
        self.jump(done);
        self.bind(normal);
        if remainder { self.a.rems(dst, dividend, divisor); } else { self.a.quos(dst, dividend, divisor); }
        self.jump(done); self.bind(zero);
        if !remainder { self.imm(dst, 0); }
        else if dst != dividend { self.a.mov_n(dst, dividend); }
        self.bind(done);
    }
    fn compare(&mut self, cond: u8, dst: Reg, x: Reg, y: Reg) {
        if dst != x && dst != y { self.a.setcc_reg_reg_reg(cond, dst, x, y); }
        else {
            // Read both inputs before writing a spilled result over one of
            // them. The assembler's compact setcc requires three registers.
            let yes = self.label(); let done = self.label();
            self.branch(cond, x, y, yes); self.imm(dst, 0); self.jump(done);
            self.bind(yes); self.imm(dst, 1); self.bind(done);
        }
    }
}

struct Emission<'a> { entry: u32, literals: u32, spans: Vec<(u32, u32, Instruction<'a>)>, loops: Vec<(u32, u32, Instruction<'a>)>, counts: Vec<Count>, proven_accesses: Vec<ProvenAccess> }
struct Loop<'a> { body: usize, next: usize, exit: usize, counter: Option<Location>, counted: Option<CountedLoop>, start: u32, instruction: Instruction<'a> }
struct Branch { otherwise: usize, end: usize }
struct Emitter<'a, 'p, 'm> { b: Body<'a>, plan: &'p Plan<'m>, options: &'p Options, domain: Domain<'p> }

/// Fixed wrapper work, including RET_N: 17 instructions plus
/// at most four saves/restores for a14/a15 and eleven zero-initialized allocated
/// vars = 32. All wrapper save offsets (0..16) fit one access; imm is one
/// MOVI.N, MOVI or L32R. Relaxation makes the first clear test one branch:
/// it skips only the four instructions of the clearing loop.
/// Each cleared word adds four instructions (store, two addi, backward BCCZ).
/// Clearing starts at 16, or 32 when both preserved registers need save slots,
/// so `stack_bytes - 16` remains an upper bound for clear work in either case.
/// The emitted body bound excludes this wrapper and its clearing loop.
/// The caller's original a14/a15 values are restored before returning.
pub(crate) const ENTRY_FIXED_INSNS: u64 = 32;

impl<'a, 'p, 'm> Emitter<'a, 'p, 'm> {
    fn new(a: &'a mut Asm, plan: &'p Plan<'m>, options: &'p Options, domain: Domain<'p>) -> Self {
        Self { b: Body { a, next: 0, literals: 0, counts: Vec::new() }, plan, options, domain }
    }
    fn move_reg(&mut self, dst: Reg, src: Reg) {
        if dst != src { self.b.a.mov_n(dst, src); }
    }
    fn source(&mut self, location: Location, scratch: Reg) -> Reg {
        match location {
            Location::Register(reg) => reg,
            Location::Spill(slot) => {
                self.b.load(scratch, 1, self.plan.spill_base + slot as u32 * 4); scratch
            }
            Location::Rematerialize(value) => { self.b.imm(scratch, value); scratch }
        }
    }
    fn destination(location: Location, scratch: Reg) -> Reg {
        match location { Location::Register(reg) => reg, Location::Spill(_) => scratch,
            Location::Rematerialize(_) => unreachable!("literal has no destination") }
    }
    fn write(&mut self, location: Location, src: Reg) {
        match location {
            Location::Register(reg) => self.move_reg(reg, src),
            Location::Spill(slot) => self.b.store(src, 1, self.plan.spill_base + slot as u32 * 4),
            Location::Rematerialize(_) => unreachable!("literal has no storage"),
        }
    }
    fn value(&mut self, v: u32, scratch: Reg) -> Reg {
        self.source(self.plan.values[v as usize].expect("materialized operand"), scratch)
    }
    fn destination_value(&self, v: u32, scratch: Reg) -> Reg {
        Self::destination(self.plan.values[v as usize].expect("materialized result"), scratch)
    }
    fn result(&mut self, v: u32, src: Reg) {
        self.write(self.plan.values[v as usize].expect("materialized result"), src);
    }
    fn fixed_value(&mut self, v: u32, dst: Reg) {
        let src = self.value(v, dst); self.move_reg(dst, src);
    }
    fn branch_constant(&mut self, cond: u8, src: Reg, constant: i32, label: usize) {
        if constant == 0 {
            let cond = match cond { cc::EQ => ccz::EQ, cc::NE => ccz::NE,
                cc::LT => ccz::LT, cc::GE => ccz::GE, _ => unreachable!("signed compare") };
            self.b.zero(cond, src, label);
        } else if enc::b4const(constant).is_some() {
            self.b.a.bcci_reg_label(cond, src, constant, label);
        } else {
            self.b.imm(11, constant as u32); self.b.branch(cond, src, 11, label);
        }
    }
    fn condition(&mut self, n: usize, condition: u32, when_true: bool, label: usize) {
        if let Some(i) = self.plan.control.fused[n] {
            let c = compare(i, &self.plan.constants);
            let cond = if when_true { c.cond } else { c.cond ^ 8 };
            let x = self.value(c.left, 8);
            match c.right {
                CompareOperand::Value(v) => { let y = self.value(v, 11); self.b.branch(cond, x, y, label); }
                CompareOperand::Immediate(c) => self.branch_constant(cond, x, c, label),
            }
        } else {
            let src = self.value(condition, 8);
            self.b.zero(if when_true { ccz::NE } else { ccz::EQ }, src, label);
        }
    }
    fn emit(mut self) -> Emission<'m> {
        self.b.a.literal_table();
        // Padding is skipped with the table and never executed.
        self.b.a.base.align(4);
        let entry = self.b.a.base.offset();
        self.b.imm(11, self.plan.bytes); self.b.a.sub(1, 1, 11);
        self.b.store(0, 1, 0); self.b.store(12, 1, 4); self.b.store(13, 1, 8);
        for (n, &reg) in self.plan.saved.iter().enumerate() { self.b.store(reg, 1, 12 + n as u32 * 4); }
        self.b.a.mov_n(12, 2); self.b.a.mov_n(13, 3);
        // Fresh registers and frame, as in exec::run. Clearing the contiguous
        // slot area also avoids relying on a port result's previous contents.
        self.b.imm(2, 0); self.b.a.addi(3, 1, self.plan.spill_base as i32);
        self.b.imm(4, (self.plan.bytes - self.plan.spill_base) / 4);
        let clear = self.b.label(); let cleared = self.b.label();
        self.b.zero(ccz::EQ, 4, cleared); self.b.bind(clear);
        self.b.store(2, 3, 0); self.b.a.addi(3, 3, 4); self.b.a.addi(4, 4, -1);
        self.b.zero(ccz::NE, 4, clear); self.b.bind(cleared);
        for &reg in &self.plan.zero_regs { self.b.imm(reg, 0); }
        let mut loops = Vec::<Loop<'m>>::new();
        let mut branches = Vec::<Branch>::new();
        let mut skipped = 0;
        let mut spans = Vec::new();
        let mut loop_spans = Vec::new();
        let mut proven_accesses = Vec::new();
        let plan = self.plan;
        for (n, &step) in plan.steps.iter().enumerate() {
            if skipped != 0 {
                match step { Step::Op(i) if i.op == OP_LOOP => skipped += 1,
                    Step::EndLoop => skipped -= 1, _ => {} }
                continue;
            }
            if plan.control.omit[n] { continue; }
            let start = self.b.a.base.offset();
            let inline = match step {
                Step::Op(i) if i.op == OP_PORT => plan.inline_accesses.get(&i.at),
                _ => None,
            };
            match step {
                Step::Else => {
                    let b = branches.last().unwrap(); self.b.jump(b.end); self.b.bind(b.otherwise);
                }
                Step::EndIf => self.b.bind(branches.pop().unwrap().end),
                Step::EndLoop => {
                    let l = loops.pop().unwrap(); self.b.bind(l.next);
                    if let Some(location) = l.counter {
                        let counter = self.source(location, 8); self.b.zero(ccz::EQ, counter, l.exit);
                        self.b.a.addi(counter, counter, -1); self.write(location, counter);
                    }
                    if let Some(c) = l.counted {
                        let location = plan.vars[c.var as usize].unwrap();
                        let var = self.source(location, 8);
                        self.b.a.addi(var, var, 1); self.write(location, var);
                        self.branch_constant(cc::LT, var, c.limit, l.body);
                    } else { self.b.jump(l.body); }
                    self.b.bind(l.exit);
                    loop_spans.push((l.start, self.b.a.base.offset(), l.instruction));
                }
                Step::Op(i) => {
                    match i.op {
                        OP_IF => {
                            if let Some(transfer) = plan.control.transfers[n] {
                                let l = &loops[loops.len() - 1 - transfer.args[0] as usize];
                                let target = if transfer.op == OP_BREAK { l.exit } else { l.next };
                                self.condition(n, i.args[0], true, target);
                            } else {
                                let b = Branch { otherwise: self.b.label(), end: self.b.label() };
                                self.condition(n, i.args[0], false, b.otherwise); branches.push(b);
                            }
                        }
                        OP_LOOP => {
                            let counted = plan.control.counted[n];
                            if i.args[0] == 0 || counted.is_some_and(|c| c.trips == 0) { skipped = 1; continue; }
                            // These definitions precede the repeated body label.
                            // A literal holds a register only where one is free
                            // across its whole loop; otherwise it is emitted at
                            // each access.
                            for (literal, &location) in plan.literals.intervals.iter().zip(&plan.literal_locations) {
                                if literal.start == i.at {
                                    if let Location::Register(reg) = location {
                                        self.b.imm(reg, literal.rematerialize.unwrap());
                                    }
                                }
                            }
                            let mut l = Loop { body: self.b.label(), next: self.b.label(), exit: self.b.label(),
                                counter: self.plan.counters.iter().find(|&&(at, _)| at == i.at).map(|&(_, loc)| loc), counted,
                                start: 0, instruction: i };
                            if let Some(location) = l.counter {
                                let counter = Self::destination(location, 8);
                                self.b.imm(counter, i.args[0] - 1); self.write(location, counter);
                            }
                            self.b.bind(l.body); l.start = self.b.a.base.offset(); loops.push(l);
                        }
                        OP_BREAK | OP_CONTINUE => {
                            let l = &loops[loops.len() - 1 - i.args[0] as usize];
                            self.b.jump(if i.op == OP_BREAK { l.exit } else { l.next });
                        }
                        OP_PORT => if let Some(&(address, access)) = inline {
                            let value = if access.write { self.value(i.args[5], 8) }
                                else { self.destination_value(i.args[5], 8) };
                            self.b.imm(11, address);
                            proven_accesses.push(ProvenAccess { pc: self.b.a.base.offset(), address,
                                width: access.width, class: access.class, write: access.write });
                            (access.instruction)(self.b.a, value, 11, 0);
                            if !access.write { self.result(i.args[5], value); }
                        } else { self.op(n, i); },
                        _ => self.op(n, i),
                    }
                    let end = self.b.a.base.offset();
                    if end > start { spans.push((start, end, i)); }
                }
            }
            self.b.counts.push(Count::Span { start, end: self.b.a.base.offset(), times: 1 });
            // Emit structure after its own instructions: LOOP's preheader is
            // outside the repetition, EndLoop's tail inside it, and ELSE's
            // jump over the other arm belongs only to the then path.
            match step {
                Step::Else => self.b.counts.push(Count::Else),
                Step::EndIf => self.b.counts.push(Count::EndIf),
                Step::EndLoop => self.b.counts.push(Count::EndLoop),
                Step::Op(i) => match i.op {
                    OP_LOOP => {
                        let cap = u64::from(i.args[0]);
                        let trips = plan.control.counted[n].map_or(cap, |c| c.trips.min(cap));
                        self.b.counts.push(Count::Loop(trips));
                    }
                    OP_IF if plan.control.transfers[n].is_none() => self.b.counts.push(Count::If),
                    OP_PORT if inline.is_none() => self.b.counts.push(match self.domain {
                        Domain::App => Count::Port,
                        Domain::Kernel { costs, .. } => {
                            let port = i.args[0] as u16;
                            let max = if matches!(port, ports::COPY_RAW | ports::FILL_RAW | ports::PACK_COPY | ports::PACK_FILL) {
                                plan.constants[i.args[7] as usize].expect("verified constant raw bound")
                            } else { 0 };
                            let cost = costs.iter().find(|c| c.0 == port).expect("emitted kernel port cost").1;
                            Count::PortCost { port, cost: cost.bound(max) }
                        }
                    }),
                    _ => {}
                },
            }
        }
        for (n, &reg) in self.plan.saved.iter().enumerate() { self.b.load(reg, 1, 12 + n as u32 * 4); }
        self.b.load(0, 1, 0); self.b.load(12, 1, 4); self.b.load(13, 1, 8);
        self.b.imm(11, self.plan.bytes); self.b.a.add_n(1, 1, 11); self.b.a.ret_n();
        Emission { entry, literals: self.b.literals, spans, loops: loop_spans, counts: self.b.counts, proven_accesses }
    }
    fn literal(&mut self, n: usize, value: u32, scratch: Reg) -> Reg {
        let location = self.plan.literals.uses[n].iter().find_map(|&id| {
            (self.plan.literals.intervals[id].rematerialize == Some(value))
                .then_some(self.plan.literal_locations[id])
        }).unwrap_or(Location::Rematerialize(value));
        self.source(location, scratch)
    }
    fn clamped_index(&mut self, n: usize, index: u32, max: u32) -> Reg {
        let src = self.value(index, 8);
        // MINU compares u32 bits, including negative i32 offsets. No signed
        // comparison or source-level range hint can justify this elision.
        if self.plan.bounds[index as usize].is_some_and(|b| b <= max) { src }
        else {
            let bound = self.literal(n, max, 11);
            self.b.a.minu(8, src, bound); 8
        }
    }
    fn address(&mut self, n: usize, region: u32, base: u32, extent: u32, index: u32) {
        let offset = base * 4 + if region == REGION_FRAME { self.plan.frame } else { 0 };
        let region = match region { REGION_STATE => 12, REGION_SHARED => 13, _ => 1 };
        if index != NONE {
            let index = self.clamped_index(n, index, extent.saturating_sub(1));
            self.b.imm(11, offset); self.b.a.add_n(11, 11, region);
            self.b.a.addx4(8, index, 11);
        } else {
            self.b.imm(8, offset); self.b.a.add_n(8, 8, region);
        }
    }
    fn buffer_address(&mut self, n: usize, i: Instruction<'_>, base: u32, index: u32) -> (Elem, Reg) {
        let k = if i.sub == REGION_BUF_LONG { i.args[4] } else { i.sub - REGION_BUF_BASE } as usize;
        let buffer = self.plan.buffers[k];
        let last = buffer.len - 1; // verified buffers are nonempty
        let base = base.min(last);
        let address = self.options.buffers[k] + base * buffer.elem.bytes();
        if index != NONE && base < last {
            // min(base + off, last), without ever adding two unchecked u32s.
            // This also treats negative i32 offsets as their unsigned bits.
            let index = self.clamped_index(n, index, last - base);
            let base = self.literal(n, address, 11);
            match buffer.elem {
                Elem::Word => self.b.a.addx4(8, index, base),
                Elem::U8 => self.b.a.add_n(8, index, base),
                // ADDX2: ISA Summary p. 335; RRR p. 656, RST0 Table 200 p. 662.
                Elem::U16 | Elem::I16 => self.b.a.addx2(8, index, base),
            }
            (buffer.elem, 8)
        } else { (buffer.elem, self.literal(n, address, 8)) }
    }
    fn op(&mut self, n: usize, i: Instruction<'_>) {
        let a = i.args;
        if clobbers(i) && i.op != OP_PORT {
            // Like ports, float helpers have a fixed a2/a3 -> a2 ABI.
            // No volatile interval touches this instruction, so marshal
            // directly without adding scratch copies around spilled inputs.
            self.fixed_value(a[1], 2);
            if i.op != OP_UN { self.fixed_value(a[2], 3); }
            match i.op {
                OP_UN => float::unary(&mut self.b, i.sub),
                OP_BIN => float::binary(&mut self.b, i.sub),
                OP_CMP_F => float::compare(&mut self.b, i.sub),
                _ => unreachable!("float helper opcode"),
            }
            self.result(a[0], 2);
            return;
        }
        match i.op {
            OP_CONST => {
                if let Some(Location::Register(dst)) = self.plan.values[a[0] as usize] {
                    self.b.imm(dst, a[1]);
                }
            }
            OP_GET => {
                let src = self.source(self.plan.vars[a[1] as usize].unwrap(), 8);
                self.result(a[0], src);
            }
            OP_SET => {
                let src = self.value(a[1], 8);
                self.write(self.plan.vars[a[0] as usize].unwrap(), src);
            }
            OP_UN => {
                let dst = self.destination_value(a[0], 8);
                let src = self.value(a[1], 8);
                match i.sub {
                    UN_NEG_I => self.b.a.neg(dst, src),
                    UN_NOT_B => { self.b.imm(11, 0); self.b.compare(cc::EQ, dst, src, 11); }
                    UN_BITS_FI | UN_BITS_IF => self.move_reg(dst, src),
                    _ => unreachable!("non-clobbering unary operation"),
                }
                self.result(a[0], dst);
            }
            OP_BIN => {
                let dst = self.destination_value(a[0], 8);
                if let Some(field) = self.plan.bitfields[n] {
                    let src = self.value(field.source, 8);
                    self.b.a.extui(dst, src, field.shift, field.width);
                } else if let Some((v, form)) = immediate(i, &self.plan.constants) {
                    let src = self.value(v, 8);
                    match form {
                        Immediate::Add(c) => self.b.a.addi(dst, src, c),
                        Immediate::Shift(0) => self.move_reg(dst, src),
                        Immediate::Shift(n) => match i.sub {
                            BIN_SHL_I => self.b.a.slli(dst, src, n),
                            BIN_SHR_I => self.b.a.srai(dst, src, n),
                            _ if n < 16 => self.b.a.srli(dst, src, n),
                            _ => self.b.a.extui(dst, src, n, 32 - n),
                        },
                    }
                } else {
                    let x = self.value(a[1], 8); let y = self.value(a[2], 11);
                    match i.sub {
                        BIN_ADD_I => self.b.a.add_n(dst, x, y), BIN_SUB_I => self.b.a.sub(dst, x, y),
                        BIN_MUL_I => self.b.a.mull(dst, x, y),
                        BIN_DIV_I | BIN_REM_I => {
                            // QUOS/REMS: ISA Summary pp. 556–557, 561–562;
                            // RRR format p. 656, RST2 encodings p. 667.
                            // Only 0 and -1 need rt::exec's exceptional path.
                            if self.plan.constants[a[2] as usize].is_some_and(|c| c != 0 && c != u32::MAX) {
                                if i.sub == BIN_REM_I { self.b.a.rems(dst, x, y); }
                                else { self.b.a.quos(dst, x, y); }
                            } else { self.b.divide(dst, x, y, i.sub == BIN_REM_I); }
                        }
                        BIN_AND_I | BIN_AND_B => self.b.a.and(dst, x, y),
                        BIN_OR_I | BIN_OR_B => self.b.a.or(dst, x, y), BIN_XOR_I => self.b.a.xor(dst, x, y),
                        BIN_SHL_I | BIN_SHR_I | BIN_SHR_UI => {
                            self.b.a.extui(11, y, 0, 5); // Preserve x in a8 and any allocated shift operand.
                            if i.sub == BIN_SHL_I { self.b.a.ssl(11); self.b.a.sll(dst, x); }
                            else { self.b.a.ssr(11); if i.sub == BIN_SHR_I { self.b.a.sra(dst, x); } else { self.b.a.srl(dst, x); } }
                        }
                        _ => unreachable!("non-clobbering binary operation"),
                    }
                }
                self.result(a[0], dst);
            }
            OP_CMP_I => {
                let x = self.value(a[1], 8); let y = self.value(a[2], 11);
                let dst = self.destination_value(a[0], 8);
                let (cond, x, y) = match i.sub {
                    CMP_LT => (cc::LT, x, y), CMP_LE => (cc::GE, y, x),
                    CMP_GT => (cc::LT, y, x), CMP_GE => (cc::GE, x, y),
                    CMP_EQ => (cc::EQ, x, y), _ => (cc::NE, x, y),
                };
                if cond == cc::LT { self.b.a.salt(dst, x, y); }
                else { self.b.compare(cond, dst, x, y); }
                self.result(a[0], dst);
            }
            OP_SEL => {
                let condition = self.value(a[1], 8);
                let dst = self.destination_value(a[0], 8);
                if dst == condition {
                    let no = self.b.label(); let done = self.b.label();
                    self.b.zero(ccz::EQ, condition, no);
                    self.fixed_value(a[2], dst); self.b.jump(done);
                    self.b.bind(no); self.fixed_value(a[3], dst); self.b.bind(done);
                } else {
                    let no = self.value(a[3], 11); self.move_reg(dst, no);
                    let yes = self.value(a[2], 11); self.b.a.movnez(dst, yes, condition);
                }
                self.result(a[0], dst);
            }
            OP_WRAP => {
                let dst = self.destination_value(a[0], 8);
                let src = self.value(a[1], 8);
                if a[2] == 0 { self.b.imm(dst, 0); }
                else if a[2].is_power_of_two() {
                    self.b.imm(11, a[2] - 1); self.b.a.and(dst, src, 11);
                } else {
                    self.b.imm(11, a[2]); self.b.divide(dst, src, 11, true);
                    let done = self.b.label(); self.b.zero(ccz::GE, dst, done);
                    self.b.a.add_n(dst, dst, 11); self.b.bind(done);
                }
                self.result(a[0], dst);
            }
            OP_FMA => {
                let x = self.value(a[1], 8); let y = self.value(a[2], 11);
                let dst = self.destination_value(a[0], 8);
                self.b.a.mull(dst, x, y);
                let z = self.value(a[3], 11);
                self.b.a.add_n(dst, dst, z); self.result(a[0], dst);
            }
            OP_LOAD => {
                let dst = self.destination_value(a[0], 8);
                if i.sub >= REGION_BUF_BASE {
                    let (elem, address) = self.buffer_address(n, i, a[1], a[3]); load_element(self.b.a, elem, dst, address);
                } else { self.address(n, i.sub, a[1], a[2], a[3]); self.b.load(dst, 8, 0); }
                self.result(a[0], dst);
            }
            OP_STORE => {
                if i.sub >= REGION_BUF_BASE {
                    let (elem, address) = self.buffer_address(n, i, a[0], a[2]); let src = self.value(a[3], 11);
                    store_element(self.b.a, elem, src, address);
                } else if i.sub != REGION_SHARED {
                    self.address(n, i.sub, a[0], a[1], a[2]); let src = self.value(a[3], 11); self.b.store(src, 8, 0);
                }
            }
            OP_PORT => {
                for n in 0..a[2] { self.fixed_value(a[4 + n as usize], 2 + n as u8); }
                self.b.imm(10, a[1]);
                let target = self.options.ports.iter().find(|p| u32::from(p.0) == a[0]).unwrap().1;
                self.b.imm(11, target); self.b.a.callx0(11);
                for n in 0..a[3] { self.result(a[4 + a[2] as usize + n as usize], 2 + n as u8); }
            }
            _ => unreachable!("verified f32/i32 opcode"),
        }
    }
}

pub(crate) fn load_element(a: &mut Asm, elem: Elem, dst: Reg, base: Reg) {
    // ESP32-S3 TRM v1.8 §4.3.3.1 p.404: DCache-mapped flash permits
    // single/double-byte reads; datasheet v2.2 §4.1.2.2 p.39 lists 8/16/32/128-bit
    // data-space flash reads. L16UI therefore serves image indexing and
    // image-to-surface copy just as it serves SRAM U16 loads.
    match elem {
        Elem::Word => a.l32i(dst, base, 0),
        Elem::U8 => a.l8ui(dst, base, 0),
        Elem::U16 => a.l16ui(dst, base, 0),
        // L16SI, RRI8 with op0=2/r=9, zero scaled offset: ISA Summary
        // pp. 451–452, format p. 656 and LSAI Table 233 p. 672.
        Elem::I16 => a.op24(enc::rri8(2, 9, base, dst, 0)),
    }
}

pub(crate) fn store_element(a: &mut Asm, elem: Elem, src: Reg, base: Reg) {
    match elem {
        Elem::Word => a.s32i(src, base, 0),
        Elem::U8 => a.s8i(src, base, 0),
        Elem::U16 | Elem::I16 => a.s16i(src, base, 0),
    }
}

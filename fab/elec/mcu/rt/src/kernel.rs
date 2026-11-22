//! Executable reference for masked, run-to-completion kernel ports.
//! Addresses are 32-bit device addresses, never host pointers. Every complete
//! access is checked before mutation; a fault leaves memory and IRQ state alone.
use alloc::{format, vec::Vec};
use crate::{exec::{self, Exit, PortCall, Ports}, module::{DeclArg, Module}, ports, sched::{Fault, Hal}};
pub use crate::KernelLines;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelFault {
    Misaligned, OutOfRegion, CopyBound, FillBound,
    /// A blocking export would wait inside a critical section (depth > 0).
    BlockInCritical { export: u16 },
}

impl KernelFault {
    pub fn name(self) -> &'static str {
        match self {
            Self::Misaligned => "misaligned", Self::OutOfRegion => "out of region",
            Self::CopyBound => "copy bound", Self::FillBound => "fill bound",
            Self::BlockInCritical { .. } => "blocking export in a critical section",
        }
    }
}

/// The kind assigned to an address range in the kernel memory map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind { Kernel, Pack, Device }

/// The set of regions a checked memory port may access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessClass { Kernel, Pack, Device }

impl AccessClass {
    pub fn reaches(self, kind: RegionKind) -> bool {
        match self {
            Self::Kernel => kind == RegionKind::Kernel,
            Self::Pack => matches!(kind, RegionKind::Kernel | RegionKind::Pack),
            Self::Device => kind == RegionKind::Device,
        }
    }
}

pub fn access_class(port: u16) -> Option<AccessClass> {
    match port {
        ports::LOAD8U | ports::LOAD16U | ports::LOAD32 | ports::STORE8 | ports::STORE16 |
        ports::STORE32 | ports::COPY_RAW | ports::FILL_RAW | ports::NOW64 => Some(AccessClass::Kernel),
        ports::PACK_LOAD32 | ports::PACK_STORE32 | ports::PACK_STORE8 |
        ports::PACK_COPY | ports::PACK_FILL => Some(AccessClass::Pack),
        ports::DEV_LOAD32 | ports::DEV_STORE32 => Some(AccessClass::Device),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct Region { base: u32, bytes: Vec<u8>, writable: bool, kind: RegionKind }

#[derive(Clone, Debug, Default)]
pub struct KernelMemory { regions: Vec<Region> }

impl KernelMemory {
    pub fn new() -> Self { Self::default() }

    /// Regions cannot overlap or wrap the device's address space. Read-only
    /// regions permit loads and copy sources; every mutation requires writable.
    pub fn add_region(&mut self, base: u32, bytes: Vec<u8>, writable: bool, kind: RegionKind) -> Result<(), KernelFault> {
        let end = u64::from(base).checked_add(bytes.len() as u64).ok_or(KernelFault::OutOfRegion)?;
        if end > 1u64 << 32 || self.regions.iter().any(|r| {
            !bytes.is_empty() && !r.bytes.is_empty() && u64::from(base) < u64::from(r.base) + r.bytes.len() as u64 && u64::from(r.base) < end
        }) { return Err(KernelFault::OutOfRegion); }
        self.regions.push(Region { base, bytes, writable, kind });
        Ok(())
    }

    fn locate(&self, addr: u32, len: usize, class: Option<AccessClass>, write: bool) -> Result<(usize, usize), KernelFault> {
        self.regions.iter().enumerate().find_map(|(k, r)| {
            let off = addr.checked_sub(r.base)? as usize;
            off.checked_add(len).filter(|end| *end <= r.bytes.len()).map(|_| (k, off))
        }).filter(|&(k, _)| {
            let region = &self.regions[k];
            class.is_none_or(|c| c.reaches(region.kind)) && (!write || region.writable)
        }).ok_or(KernelFault::OutOfRegion)
    }

    pub fn bytes(&self, addr: u32, len: usize) -> Result<&[u8], KernelFault> {
        self.read_bytes(addr, len, None)
    }

    fn read_bytes(&self, addr: u32, len: usize, class: Option<AccessClass>) -> Result<&[u8], KernelFault> {
        let (k, off) = self.locate(addr, len, class, false)?;
        Ok(&self.regions[k].bytes[off..off + len])
    }

    pub fn bytes_mut(&mut self, addr: u32, len: usize) -> Result<&mut [u8], KernelFault> {
        self.write_bytes(addr, len, None)
    }

    fn write_bytes(&mut self, addr: u32, len: usize, class: Option<AccessClass>) -> Result<&mut [u8], KernelFault> {
        let (k, off) = self.locate(addr, len, class, true)?;
        Ok(&mut self.regions[k].bytes[off..off + len])
    }

    fn aligned(addr: u32, align: u32) -> Result<(), KernelFault> {
        if addr & (align - 1) != 0 { Err(KernelFault::Misaligned) } else { Ok(()) }
    }

    fn load(&self, class: AccessClass, addr: u32, width: usize) -> Result<u32, KernelFault> {
        Self::aligned(addr, width as u32)?;
        let mut word = [0; 4];
        word[..width].copy_from_slice(self.read_bytes(addr, width, Some(class))?);
        Ok(u32::from_le_bytes(word))
    }

    fn store(&mut self, class: AccessClass, addr: u32, width: usize, value: u32) -> Result<(), KernelFault> {
        Self::aligned(addr, width as u32)?;
        self.write_bytes(addr, width, Some(class))?.copy_from_slice(&value.to_le_bytes()[..width]);
        Ok(())
    }

    fn copy(&mut self, class: AccessClass, dst: u32, src: u32, n: u32, max: u32) -> Result<(), KernelFault> {
        if max > i32::MAX as u32 || n > max { return Err(KernelFault::CopyBound); }
        if n == 0 { return Ok(()); }
        let n = n as usize;
        let (s, so) = self.locate(src, n, Some(class), false)?;
        let (d, doff) = self.locate(dst, n, Some(class), true)?;
        if s == d {
            self.regions[s].bytes.copy_within(so..so + n, doff);
        } else {
            // Both ranges were checked before the first write. Separate
            // regions cannot overlap, so no temporary allocation is needed.
            for i in 0..n {
                let byte = self.regions[s].bytes[so + i];
                self.regions[d].bytes[doff + i] = byte;
            }
        }
        Ok(())
    }

    fn fill(&mut self, class: AccessClass, dst: u32, byte: u32, n: u32, max: u32) -> Result<(), KernelFault> {
        if max > i32::MAX as u32 || n > max { return Err(KernelFault::FillBound); }
        if n != 0 { self.write_bytes(dst, n as usize, Some(class))?.fill(byte as u8); }
        Ok(())
    }
}

/// A host reference using the same Hal::fault channel as scheduler ports.
/// `clock` is a settable device time snapshot; it also timestamps faults.
/// The first fatal fault suspends execution, matching the chip's nonreturning
/// ILL. A report goes through Hal::fault and the export carries on.
pub struct KernelPorts<'a> {
    pub memory: KernelMemory,
    pub intenable: u32,
    pub lines: KernelLines,
    pub clock: u64,
    pub raised: Vec<u32>,
    /// Last fatal fault, also reported through Hal::fault. Clear before a new
    /// call when reusing this reference after a fatal fault.
    pub fault: Option<Fault>,
    /// Last requested radio stop. Clear before a new call when reusing this
    /// reference after a stop. The export suspends without calling Hal::fault.
    pub stop: Option<u8>,
    pub hal: &'a mut dyn Hal,
}

impl<'a> KernelPorts<'a> {
    pub fn new(memory: KernelMemory, lines: KernelLines, hal: &'a mut dyn Hal) -> Self {
        Self { memory, intenable: 0, lines, clock: 0, raised: Vec::new(), fault: None, stop: None, hal }
    }

    fn fail(&mut self, fault: Fault) -> PortCall {
        self.hal.fault(self.clock, fault);
        self.fault = Some(fault);
        PortCall::Suspend
    }
}

impl Ports for KernelPorts<'_> {
    fn call(&mut self, port: u16, site: u32, _: u64, args: &[u32], rets: &mut [u32]) -> PortCall {
        rets.fill(0);
        let arg = |k| args.get(k).copied().unwrap_or(0);
        let class = || access_class(port).expect("memory port has an access class");
        let result = match port {
            ports::LOAD8U | ports::LOAD16U | ports::LOAD32 | ports::PACK_LOAD32 | ports::DEV_LOAD32 => {
                let width = match port { ports::LOAD8U => 1, ports::LOAD16U => 2, _ => 4 };
                self.memory.load(class(), arg(0), width).map(|value| { if let Some(r) = rets.first_mut() { *r = value; } })
            }
            ports::STORE8 | ports::STORE16 | ports::STORE32 | ports::PACK_STORE32 | ports::PACK_STORE8 | ports::DEV_STORE32 => {
                let width = match port { ports::STORE8 | ports::PACK_STORE8 => 1, ports::STORE16 => 2, _ => 4 };
                self.memory.store(class(), arg(0), width, arg(1))
            }
            ports::COPY_RAW | ports::PACK_COPY => self.memory.copy(class(), arg(0), arg(1), arg(2), arg(3)),
            ports::FILL_RAW | ports::PACK_FILL => self.memory.fill(class(), arg(0), arg(1), arg(2), arg(3)),
            ports::CLZ => {
                if let Some(r) = rets.first_mut() { *r = arg(0).leading_zeros(); }
                Ok(())
            }
            ports::IRQ_ENABLE => { self.intenable |= arg(0) & !self.lines.protected; Ok(()) }
            ports::IRQ_DISABLE => { self.intenable &= !(arg(0) & !self.lines.protected); Ok(()) }
            ports::IRQ_RAISE => {
                let line = arg(0);
                // The verifier admits only constant listed lines.
                if line < 32 && self.lines.raise & (1 << line) != 0 { self.raised.push(line); }
                Ok(())
            }
            ports::NOW64 => KernelMemory::aligned(arg(0), 4).and_then(|_| {
                // Check all eight bytes before writing either word.
                self.memory.write_bytes(arg(0), 8, Some(class()))?.copy_from_slice(&self.clock.to_le_bytes());
                Ok(())
            }),
            ports::KERNEL_LOOP_CAP => return self.fail(Fault::LoopCap { site }),
            ports::RADIO_STOP => {
                self.stop = Some(arg(0) as u8);
                return PortCall::Suspend;
            }
            ports::KERNEL_REPORT => {
                self.hal.fault(self.clock, Fault::KernelReport { code: arg(0) as u8, site, arg: arg(1) as i32 });
                Ok(())
            }
            _ => panic!("port {port} is not a verified kernel port"),
        };
        if let Err(kind) = result { return self.fail(Fault::Kernel { kind, site }); }
        PortCall::Done
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallError {
    NotKernel, UnknownExport, Arguments { expected: usize, found: usize }, State, Scratch, Slot,
    /// The export suspended on a fault or a radio stop. [`KernelPorts::stop`] holds the requested stop cause.
    Faulted,
}

/// Call a verified export through its fixed state slots. The caller initializes
/// state from Module::state_init once, and owns state/scratch across calls.
/// Calls of a module must be serialized, just like the masked device thunks.
pub fn call_export(module: &Module<'_>, name: &str, args: &[i32], state: &mut [u32], scratch: &mut [u32], ports: &mut dyn Ports) -> Result<i32, CallError> {
    if module.kernel_lines().is_none() { return Err(CallError::NotKernel); }
    let decl = module.decls().iter().find(|d| d.form == ports::FORM_EXPORT && module.string(d.name) == name).ok_or(CallError::UnknownExport)?;
    let [DeclArg::Entry(entry), DeclArg::Int(n)] = decl.args.as_slice() else { return Err(CallError::UnknownExport); };
    if args.len() != *n as usize { return Err(CallError::Arguments { expected: *n as usize, found: args.len() }); }
    if state.len() < module.state_words() as usize { return Err(CallError::State); }
    if scratch.len() < module.scratch_words(*entry as usize) { return Err(CallError::Scratch); }
    let slot = |name: &str| module.vars().iter().find(|v| module.string(v.name) == name).map(|v| v.offset as usize).ok_or(CallError::Slot);
    let result = slot(&format!("{name}#r"))?;
    let mut params = [0; 6];
    for (p, off) in params.iter_mut().take(args.len()).enumerate() { *off = slot(&format!("{name}#p{p}"))?; }
    for (arg, off) in args.iter().zip(params) { state[off] = *arg as u32; }
    match exec::run(module, *entry as usize, state, scratch, ports) {
        Exit::Done { .. } => Ok(state[result] as i32),
        Exit::Suspended { .. } => Err(CallError::Faulted),
    }
}

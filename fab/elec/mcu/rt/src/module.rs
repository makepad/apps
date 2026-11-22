//! Decode once, verify every operand and bound, then lend CODE to the runtime.
use crate::{format::*, ports, pwm, serial, watch, DisplayProfile, KernelLines, Profile};
use alloc::{format, string::String, vec::Vec};
use core::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    HeaderLength { found: usize },
    Magic { found: u32 },
    FormatVersion { found: u32 },
    PortVersion { found: u32 },
    Profile { found: u32, expected: u32 },
    ModuleLength { declared: u32, found: usize },
    Reserved { at: usize, found: u32 },
    Directory { count: u32, words: usize },
    UnknownSection { kind: u32 },
    DuplicateSection { kind: u32 },
    MissingSection { kind: u32 },
    SectionRange { kind: u32, offset: u32, len: u32 },
    SectionOverlap { first: u32, second: u32 },
    SectionLength { kind: u32, expected: usize, found: usize },
    StringLength { string: u32, bytes: u32, remaining: usize },
    Utf8 { string: u32, valid_up_to: usize },
    StringPadding { string: u32, word: u32 },
    StringId { section: u32, row: u32, id: u32 },
    VarRange { var: u32, offset: u32, words: u32, state: u32 },
    DeclForm { decl: u32, form: u32 },
    DomainForm { decl: u32, form: u32, kernel: bool },
    DomainPort { entry: u32, at: u32, site: u32, port: u32, kernel: bool },
    KernelBound { entry: u32, at: u32, site: u32, port: u32 },
    KernelRaise { entry: u32, at: u32, site: u32, raise: u32 },
    KernelReport { entry: u32, at: u32, site: u32 },
    KernelStop { entry: u32, at: u32, site: u32 },
    ExportArgs { decl: u32 },
    ExportSlot { decl: u32, param: Option<u32> },
    DuplicateExport { decl: u32 },
    KernelEntry { entry: u32 },
    DeclArguments { decl: u32, form: u32 },
    DeclArgTag { decl: u32, arg: u32, tag: u32 },
    DeclEntry { decl: u32, entry: u32 },
    DeclHandle { decl: u32, found: i32, expected: i32 },
    DisplayUnsupported { decl: u32 },
    DuplicateDisplay { decl: u32 },
    MotionUnsupported { decl: u32 },
    DuplicateMotion { decl: u32 },
    MotionUndeclared { entry: u32, at: u32, site: u32, port: u32 },
    TouchUnsupported { decl: u32 },
    DuplicateTouch { decl: u32 },
    TouchPeriod { decl: u32, period: i32 },
    TouchUndeclared { entry: u32, at: u32, site: u32, port: u32 },
    DuplicateEth { decl: u32 },
    EthCount { decl: u32, count: i32 },
    EthUndeclared { entry: u32, at: u32, site: u32, port: u32 },
    /// A `net` declaration's arguments are outside its rules (kind 0 or 1, slots >= 2, send_bytes >= 1).
    NetDecl { decl: u32, kind: i32, slots: i32, send_bytes: i32 },
    NetUndeclared { entry: u32, at: u32, site: u32, port: u32 },
    /// A constant connection that no `net` declaration has.
    NetHandle { entry: u32, at: u32, site: u32, port: u32, c: i32 },
    AudioDecl { decl: u32, reason: String },
    AudioPort { entry: u32, at: u32, site: u32, port: u32, reason: &'static str },
    DisplaySize { width: u32, height: u32 },
    ImageSize { image: u32, width: u32, height: u32 },
    ImageIndex { decl: u32, image: i32 },
    ImageDimensions { decl: u32, image: i32, width: i32, height: i32 },
    BufferDecl { buffer: u32, decl: u32 },
    BufferElem { buffer: u32, decl: u32, found: u32, expected: Elem },
    BufferLength { buffer: u32, decl: u32, found: u32, expected: u32 },
    BufferFlags { buffer: u32, flags: u32 },
    BufferWritable { buffer: u32, decl: u32, writable: bool },
    BufferOrder { buffer: u32, decl: u32, expected: u32 },
    BufferMissing { decl: u32 },
    BufferBytes { needed: u64, limit: u32 },
    BufferFaultRam { words: u32, available: u32 },
    PinRange { decl: u32, pin: i32, pins: u32 },
    DuplicatePin { decl: u32, pin: i32 },
    AnalogPin { decl: u32, pin: i32 },
    AnalogOnly { decl: u32, pin: i32 },
    PwmPin { decl: u32, pin: i32 },
    PwmFrequency { decl: u32, pin: i32, asked_mhz: i64 },
    PwmClock { decl: u32, pin: i32, asked_mhz: u32 },
    PwmShared { decl: u32, pin: i32, asked_mhz: u32, other_decl: u32, other_pin: u32, other_mhz: u64 },
    SerialLinks { decl: u32, links: u32 },
    SerialBaud { decl: u32, asked: i64, nearest_milli: u64 },
    DuplicateEntry { decl: u32, entry: u16 },
    EntryRange { entry: u32, offset: u32, len: u32 },
    ProgramHeader { entry: u32, at: u32 },
    TypeCode { entry: u32, at: u32, index: u32, variable: bool, found: u8 },
    TypePadding { entry: u32, at: u32, found: u32 },
    WideFlag { entry: u32, found: u32, expected: bool },
    F64Unsupported { entry: u32 },
    BlockLength { entry: u32, at: u32, len: u32, end: u32 },
    Instruction { entry: u32, at: u32, word: u32 },
    SubOp { entry: u32, at: u32, opcode: u32, sub: u32 },
    OperandRange { entry: u32, at: u32, index: u32, variable: bool },
    ValueUndefined { entry: u32, at: u32, value: u32 },
    ValueRedefined { entry: u32, at: u32, value: u32 },
    TypeMismatch { entry: u32, at: u32, index: u32, found: u8, expected: u8 },
    BoolConstant { entry: u32, at: u32, bits: u32 },
    WrapLength { entry: u32, at: u32, len: u32 },
    Region { entry: u32, at: u32, region: u32 },
    BufferRegion { entry: u32, at: u32, buffer: u32 },
    BufferReadOnly { entry: u32, at: u32, buffer: u32 },
    MemoryRange { entry: u32, at: u32, region: u32, base: u32, extent: u32, size: u32 },
    SharedWrite { entry: u32, at: u32 },
    WideMemory { entry: u32, at: u32, value: u32 },
    LoopDepth { entry: u32, at: u32, depth: u32, loops: u32 },
    Port { entry: u32, at: u32, port: u32 },
    PortArity { entry: u32, at: u32, port: u32, args: u32, rets: u32 },
    Site { entry: u32, at: u32, site: u32, sites: u32 },
    /// More port call sites than the watch stream numbers below its reserved site values.
    Sites { sites: u32, limit: u32 },
    /// A pin port whose pin is a constant that is not declared for it
    /// (`pin_write`: an output; `pin_read`: an input or an output;
    /// `pwm_duty`: a PWM output; `adc_read`: an analog input;
    /// `shift_out`: outputs for both data and clock). `param` is the argument index.
    PinRole { entry: u32, at: u32, site: u32, port: u32, param: u32, pin: i32 },
    /// A constant shift bit count outside 1..=32.
    ShiftBits { entry: u32, at: u32, site: u32, bits: i32 },
    /// An EEPROM port on a chip with no EEPROM.
    EepromUnsupported { entry: u32, at: u32, site: u32, port: u32 },
    /// A constant EEPROM address outside the chip's capacity in bytes.
    EepromAddr { entry: u32, at: u32, site: u32, port: u32, addr: i32, size: u32 },
    BufferArgument { entry: u32, at: u32, site: u32, port: u32, param: u32 },
    CopyKinds { entry: u32, at: u32, site: u32, dst: u32, src: u32, dst_elem: Elem, src_elem: Elem },
    CallTarget { entry: u32, at: u32, func: u32 },
    CallArity { entry: u32, at: u32, func: u32, args: u32, rets: u32 },
    CallCycle { func: u32 },
    FrameMismatch { entry: u32, found: u32, expected: u32 },
    CostHighWord { entry: u32, high: u32 },
    CostUnderstated { entry: u32, declared: u64, computed: u64 },
    CostLimit { entry: u32, declared: u64, limit: u64 },
    Ram { state: u32, scratch: usize, fifo: u32, limit: u32 },
    SizeOverflow { section: u32, index: u32 },
    Allocation { elements: usize },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // What a person can meet from a correct compiler says it in words;
        // the rest is a damaged or foreign module.
        match *self {
            LoadError::DomainForm { form, kernel, .. } => write!(f, "form {form} is not allowed in {} module", if kernel { "a kernel" } else { "an app" }),
            LoadError::DomainPort { port, kernel, .. } => write!(f, "port {port} is not allowed in {} module", if kernel { "a kernel" } else { "an app" }),
            LoadError::KernelBound { port, .. } => write!(f, "`{}` max must be a compile-time constant >= 0", ports::KERNEL_PORTS[(port - u32::from(ports::KERNEL_BASE)) as usize].name),
            LoadError::KernelRaise { raise, .. } => write!(f, "`irq_raise` line must be a compile-time constant in KernelLines::raise ({raise:#010x})"),
            LoadError::Sites { sites, limit } => write!(f, "{sites} port call sites; the watch stream numbers at most {limit}"),
            LoadError::KernelReport { .. } => f.write_str("`report` code must be a compile-time constant in the kernel report block (96..=127)"),
            LoadError::KernelStop { .. } => f.write_str("`radio_stop` cause must be a compile-time constant in the kernel report block (96..=127)"),
            LoadError::ExportArgs { .. } => f.write_str("export needs [Entry, Int(n)] with 0 <= n <= 6"),
            LoadError::ExportSlot { param: Some(n), .. } => write!(f, "export needs a unique one-word i32 state slot NAME#p{n}"),
            LoadError::ExportSlot { param: None, .. } => f.write_str("export needs a unique one-word i32 state slot NAME#r"),
            LoadError::DuplicateExport { .. } => f.write_str("two exports cannot share a name"),
            LoadError::KernelEntry { .. } => f.write_str("a kernel module may only have export entries, with no setup entry"),
            LoadError::Profile { found, expected } => write!(f, "the module is for chip profile {found}, this chip is {expected}"),
            LoadError::DisplayUnsupported { decl } => write!(f, "display declaration {decl} needs a board with a display"),
            LoadError::DuplicateDisplay { .. } => f.write_str("at most one display may be declared"),
            LoadError::MotionUnsupported { decl } => write!(f, "motion declaration {decl} needs a board with motion input"),
            LoadError::DuplicateMotion { .. } => f.write_str("at most one motion may be declared"),
            LoadError::MotionUndeclared { port, .. } => write!(f, "`{}` needs a motion declaration", ports::PORTS[port as usize].name),
            LoadError::TouchUnsupported { .. } => f.write_str("touch declaration needs a board with touch input"),
            LoadError::DuplicateTouch { .. } => f.write_str("at most one touch may be declared"),
            LoadError::TouchPeriod { period, .. } => write!(f, "touch period must be 1..=300000 us; period is {period}"),
            LoadError::TouchUndeclared { port, .. } => write!(f, "`{}` needs a touch declaration", ports::PORTS[port as usize].name),
            LoadError::DuplicateEth { .. } => f.write_str("at most one eth may be declared: the board has one USB link"),
            LoadError::EthCount { count, .. } => write!(f, "eth needs at least one receive buffer; count is {count}"),
            LoadError::EthUndeclared { port, .. } => write!(f, "`{}` needs an eth declaration: declare it with `let name = eth(count, fn() {{ ... }})`", ports::PORTS[port as usize].name),
            LoadError::NetDecl { kind, slots, send_bytes, .. } => if !(0..=1).contains(&kind) {
                write!(f, "net's kind must be 0 (WebSocket) or 1 (TCP stream); kind is {kind}")
            } else if slots < 2 {
                write!(f, "net needs at least two receive slots (one being read, one arriving); slots is {slots}")
            } else {
                write!(f, "net needs a send queue of at least one byte; send_bytes is {send_bytes}")
            },
            LoadError::NetUndeclared { port, .. } => write!(f, "`{}` needs a net declaration: declare it with `let name = net(kind, slots, send_bytes, fn() {{ ... }})`", ports::PORTS[port as usize].name),
            LoadError::NetHandle { port, c, .. } => write!(f, "`{}` names connection {c}, which no net declaration has", ports::PORTS[port as usize].name),
            LoadError::AudioDecl { ref reason, .. } => f.write_str(reason),
            LoadError::AudioPort { port, reason, .. } => write!(f, "`{}` {reason}", ports::PORTS[port as usize].name),
            LoadError::DisplaySize { width, height } => write!(f, "the display's {width} by {height} size must be non-zero, even and fit the framebuffer"),
            LoadError::BufferDecl { buffer, decl } => write!(f, "buffer {buffer} names declaration {decl}, which is not a buffer form"),
            LoadError::BufferElem { buffer, decl, found, expected } => write!(f, "buffer {buffer} has element code {found}; declaration {decl} needs {expected:?}"),
            LoadError::BufferLength { buffer, decl, found, expected } => write!(f, "buffer {buffer} has {found} elements; declaration {decl} needs {expected}"),
            LoadError::BufferFlags { buffer, flags } => write!(f, "buffer {buffer} has unknown flag bits in {flags}"),
            LoadError::BufferWritable { buffer, decl, writable } => write!(f, "buffer {buffer} of declaration {decl} is marked {}; images are read-only and every other buffer form is writable", if writable { "writable" } else { "read-only" }),
            LoadError::BufferOrder { buffer, decl, expected } => write!(f, "buffer {buffer} names declaration {decl} out of order; expected declaration {expected}"),
            LoadError::BufferMissing { decl } => write!(f, "buffer declaration {decl} has no buffer row"),
            LoadError::BufferBytes { needed, limit } => write!(f, "the program's buffers need {needed} bytes; this board has {limit}"),
            LoadError::BufferFaultRam { words, available } => write!(f, "buffer range fault tracking needs {words} words of RAM; this board has {available} left after state, scratch and serial buffers"),
            LoadError::BufferRegion { buffer, .. } => write!(f, "memory region names buffer {buffer}, which does not exist"),
            LoadError::BufferReadOnly { buffer, .. } => write!(f, "store writes read-only buffer {buffer}"),
            LoadError::BufferArgument { port, param, .. } => write!(f, "port {port} argument {param} must be a constant naming a buffer of the required kind and writability"),
            LoadError::CopyKinds { dst, src, dst_elem, src_elem, .. } => write!(f, "copy needs buffers of the same kind: buffer {dst} is {dst_elem:?}, buffer {src} is {src_elem:?}"),
            LoadError::PinRange { pin, pins, .. } => write!(f, "pin {pin} is not on this chip (its pins are 0..{pins})"),
            LoadError::DuplicatePin { pin, .. } => write!(f, "pin {pin} is declared twice"),
            LoadError::AnalogPin { pin, .. } => write!(f, "pin {pin} has no analog input on this chip"),
            LoadError::AnalogOnly { pin, .. } => write!(f, "pin {pin} is an analog-only pin: declare it with `analog(..)`"),
            LoadError::PwmPin { pin, .. } => write!(f, "pin {pin} has no PWM output on this chip"),
            LoadError::PwmFrequency { pin, asked_mhz, .. } => write!(f, "no PWM setting near {asked_mhz} mHz for pin {pin}"),
            LoadError::PwmClock { pin, .. } => write!(f, "pin {pin} is on the device clock's timer"),
            LoadError::PwmShared { pin, asked_mhz, other_mhz, .. } => write!(f, "pin {pin} asks {asked_mhz} mHz but its timer already runs at {other_mhz} mHz"),
            LoadError::SerialLinks { decl, links } => write!(f, "serial declaration {decl} exceeds the chip's {links} serial links"),
            LoadError::SerialBaud { asked, nearest_milli, .. } => write!(f, "no serial setting near {asked} baud; nearest is {nearest_milli} milli-baud"),
            LoadError::F64Unsupported { .. } => f.write_str("this chip has no 64-bit floats (f64)"),
            LoadError::CostLimit { declared, limit, .. } => write!(f, "a handler may run {declared} operations; this chip allows {limit}"),
            LoadError::Ram { state, scratch, fifo, limit } if fifo > 0 => write!(f, "the program needs {} words of RAM ({state} of state, {scratch} for the largest handler, {fifo} for serial receive buffers); this chip has {limit}", state as u128 + scratch as u128 + fifo as u128),
            LoadError::Ram { state, scratch, limit, .. } => write!(f, "the program needs {} words of RAM ({state} of state, {scratch} for the largest handler); this chip has {limit}", state as u128 + scratch as u128),
            LoadError::PinRole { port, pin, .. } if port == ports::PIN_WRITE as u32 => write!(f, "pin {pin} is written but not declared as an output"),
            LoadError::PinRole { port, pin, .. } if port == ports::PWM_DUTY as u32 => write!(f, "pin {pin} gets a duty but is not declared as PWM"),
            LoadError::PinRole { port, pin, .. } if port == ports::ADC_READ as u32 => write!(f, "pin {pin} is read but not declared as an analog input"),
            LoadError::PinRole { port, param, pin, .. } if port == ports::SHIFT_OUT as u32 => write!(f, "`shift_out` drives pin {pin} as its {} pin, which is not declared as an output", if param == 0 { "data" } else { "clock" }),
            LoadError::PinRole { pin, .. } => write!(f, "pin {pin} is read but not declared as an input or output"),
            LoadError::ShiftBits { bits, .. } => write!(f, "`shift_out` sends 1 to 32 bits; this call sends {bits}"),
            LoadError::EepromUnsupported { port, .. } => write!(f, "`{}` is unavailable: this chip has no EEPROM", ports::PORTS[port as usize].name),
            LoadError::EepromAddr { port, addr, size, .. } => write!(f, "`{}` address {addr} is outside 0..{size} ({size} bytes of EEPROM)", ports::PORTS[port as usize].name),
            _ => write!(f, "invalid microsplash module: {self:?}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: u32,
    pub code: u32,
    pub len: u32,
    pub cost: u64,
    pub n_vals: u32,
    pub n_vars: u32,
    pub frame_words: u32,
    pub wide: bool,
    /// Number of simultaneously active blocks, including the body block.
    pub depth: u32,
    /// Index in the CODE section, as are statement error offsets.
    pub body: u32,
}

/// A shared function; its program uses the module's total frame.
#[derive(Debug, Clone)]
pub struct Func<'a> {
    pub program: Entry,
    pub params: &'a [u32],
    pub results: &'a [u32],
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeclArg { Int(i32), Float(f32), Entry(u16) }

#[derive(Debug, Clone, PartialEq)]
pub struct Decl { pub form: u32, pub handle: i32, pub name: u32, pub args: Vec<DeclArg> }

#[derive(Debug, Clone, PartialEq)]
pub struct Var { pub name: u32, pub sig: u32, pub offset: u32, pub words: u32 }

/// Buffer element codes, identical to the compute core's `ir::Elem`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Elem { Word = 0, U8 = 1, U16 = 2, I16 = 3 }

impl Elem {
    pub fn bytes(self) -> u32 {
        match self { Self::Word => 4, Self::U8 => 1, Self::U16 | Self::I16 => 2 }
    }
}

/// A verified BUFS row. Element i occupies bytes `i * elem.bytes()` onward,
/// in little-endian order within its word allocation, as on both chips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffer { pub decl: u32, pub elem: Elem, pub len: u32, pub writable: bool }

impl Buffer {
    pub fn words(self) -> usize { (u64::from(self.len) * u64::from(self.elem.bytes())).div_ceil(4) as usize }

    pub(crate) fn index(self, base: u32, off: u32) -> usize {
        (u64::from(base) + u64::from(off)).min(u64::from(self.len.saturating_sub(1))) as usize
    }

    pub(crate) fn load(self, words: &[u32], index: usize) -> u32 {
        if index >= self.len as usize { return 0; }
        let per_word = 4 / self.elem.bytes() as usize;
        let shift = (index % per_word) * self.elem.bytes() as usize * 8;
        let bits = words.get(index / per_word).copied().unwrap_or(0) >> shift;
        match self.elem {
            Elem::Word => bits,
            Elem::U8 => bits & 0xff,
            Elem::U16 => bits & 0xffff,
            Elem::I16 => bits as i16 as i32 as u32,
        }
    }

    pub(crate) fn store(self, words: &mut [u32], index: usize, value: u32) {
        if !self.writable || index >= self.len as usize { return; }
        let per_word = 4 / self.elem.bytes() as usize;
        let shift = (index % per_word) * self.elem.bytes() as usize * 8;
        let mask = match self.elem { Elem::Word => u32::MAX, Elem::U8 => 0xff, Elem::U16 | Elem::I16 => 0xffff };
        if let Some(word) = words.get_mut(index / per_word) {
            *word = (*word & !(mask << shift)) | ((value & mask) << shift);
        }
    }
}

fn buffer_elem(form: u32) -> Option<Elem> {
    Some(match form {
        ports::FORM_BYTES => Elem::U8,
        ports::FORM_SAMPLES => Elem::I16,
        ports::FORM_WORDS => Elem::Word,
        ports::FORM_SURFACE | ports::FORM_IMAGE => Elem::U16,
        _ => return None,
    })
}

fn buffer_length(form: u32, args: &[DeclArg]) -> Option<u32> {
    match (form, args) {
        (ports::FORM_BYTES | ports::FORM_SAMPLES | ports::FORM_WORDS, [DeclArg::Int(n)]) if *n > 0 => Some(*n as u32),
        (ports::FORM_SURFACE, [DeclArg::Int(w), DeclArg::Int(h)]) |
        (ports::FORM_IMAGE, [DeclArg::Int(_), DeclArg::Int(w), DeclArg::Int(h)]) if *w > 0 && *h > 0 =>
            w.checked_mul(*h).filter(|n| *n > 0).map(|n| n as u32),
        _ => None,
    }
}

/// The two-dimensional extent of a verified U16 buffer declaration.
pub fn buffer_dimensions(decl: &Decl) -> Option<(u32, u32)> {
    match (decl.form, decl.args.as_slice()) {
        (ports::FORM_SURFACE, [DeclArg::Int(w), DeclArg::Int(h)]) |
        (ports::FORM_IMAGE, [DeclArg::Int(_), DeclArg::Int(w), DeclArg::Int(h)]) => Some((*w as u32, *h as u32)),
        _ => None,
    }
}

/// An image borrowed from IMAGES, with row-major RGB565 pixels packed high half first.
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    pub width: u32,
    pub height: u32,
    pixels: &'a [u32],
}

impl Image<'_> {
    /// The RGB565 pixel at (x, y), or None outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<u16> {
        if x >= self.width || y >= self.height { return None; }
        // The loader proved that the pixel count fits usize.
        let index = y as usize * self.width as usize + x as usize;
        Some((self.pixels[index / 2] >> if index % 2 == 0 { 16 } else { 0 }) as u16)
    }
}

/// One `net(kind, slots, send_bytes, fn)` declaration, as verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetDecl {
    pub kind: i32,
    pub slots: u32,
    pub send_bytes: u32,
    pub entry: u16,
}

#[derive(Debug)]
pub struct Module<'a> {
    kernel_lines: Option<KernelLines>,
    state: &'a [u32],
    shared: &'a [u32],
    code: &'a [u32],
    sites: u32,
    strings: Vec<String>,
    entries: Vec<Entry>,
    funcs: Vec<Func<'a>>,
    func_order: Vec<u32>,
    decls: Vec<Decl>,
    pwm: Vec<pwm::PwmOut>,
    serial: Vec<serial::SerialOut>,
    display: Option<DisplayProfile>,
    motion: Option<crate::MotionProfile>,
    touch: bool,
    images: Vec<Image<'a>>,
    buffers: Vec<Buffer>,
    vars: Vec<Var>,
    scratch: Vec<usize>,
}

impl<'a> Module<'a> {
    /// The policy with which this module was verified; None for app modules.
    pub fn kernel_lines(&self) -> Option<KernelLines> { self.kernel_lines }
    pub fn state_words(&self) -> u32 { self.state.len() as u32 }
    pub fn state_init(&self) -> &'a [u32] { self.state }
    pub fn shared(&self) -> &'a [u32] { self.shared }
    pub fn n_sites(&self) -> u32 { self.sites }
    pub fn string(&self, id: u32) -> &str { self.strings.get(id as usize).map_or("", String::as_str) }
    pub fn funcs(&self) -> &[Func<'a>] { &self.funcs }
    /// Callees before callers; suitable for placement and static-chain accounting.
    pub fn func_order(&self) -> &[u32] { &self.func_order }
    pub fn func_ty(&self, func: usize, index: u32, variable: bool) -> u8 {
        program_ty(self.code, &self.funcs[func].program, index, variable).expect("type index outside verified function")
    }
    pub fn entries(&self) -> &[Entry] { &self.entries }
    pub fn entry(&self, name: &str) -> Option<usize> { self.entries.iter().position(|e| self.string(e.name) == name) }
    pub fn decls(&self) -> &[Decl] { &self.decls }
    pub fn pwm(&self) -> &[pwm::PwmOut] { &self.pwm }
    pub fn serial(&self) -> &[serial::SerialOut] { &self.serial }
    /// The verified panel dimensions, only when the module declares a display.
    pub fn display(&self) -> Option<DisplayProfile> { self.display }
    /// Verified raw motion scales, only when motion is declared.
    pub fn motion(&self) -> Option<crate::MotionProfile> { self.motion }
    /// True only when a touch declaration has been verified for this board.
    pub fn touch(&self) -> bool { self.touch }
    /// The verified `net` declarations, in connection order.
    pub fn net(&self) -> Vec<NetDecl> {
        self.decls.iter().filter(|d| d.form == ports::FORM_NET).map(|d| match d.args.as_slice() {
            [DeclArg::Int(kind), DeclArg::Int(slots), DeclArg::Int(send_bytes), DeclArg::Entry(entry)] =>
                NetDecl { kind: *kind, slots: *slots as u32, send_bytes: *send_bytes as u32, entry: *entry },
            _ => unreachable!("verified net declaration"),
        }).collect()
    }
    pub fn images(&self) -> &[Image<'a>] { &self.images }
    pub fn buffers(&self) -> &[Buffer] { &self.buffers }
    pub fn vars(&self) -> &[Var] { &self.vars }
    pub fn code(&self) -> &'a [u32] { self.code }
    /// Scratch words required by `exec::run` and `exec::resume`.
    pub fn scratch_words(&self, k: usize) -> usize { self.scratch.get(k).copied().map_or(0, |n| n) }

    /// A verified entry's value or variable type. Indices must be in range.
    pub fn ty(&self, entry: usize, index: u32, variable: bool) -> u8 {
        let e = &self.entries[entry];
        let start = e.code as usize + PROGRAM_WORDS
            + if variable { e.n_vals.div_ceil(4) as usize } else { 0 };
        Types { words: &self.code[start..e.body as usize], count: if variable { e.n_vars } else { e.n_vals } }
            .get(index).expect("type index outside verified entry")
    }
}

fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), LoadError> {
    v.try_reserve(n).map_err(|_| LoadError::Allocation { elements: n })
}

fn push<T>(v: &mut Vec<T>, x: T) -> Result<(), LoadError> {
    reserve(v, 1)?;
    v.push(x);
    Ok(())
}

fn size(n: u32, words: usize, section: u32) -> Result<usize, LoadError> {
    (n as usize).checked_mul(words).ok_or(LoadError::SizeOverflow { section, index: n })
}

fn exact(section: u32, expected: usize, found: usize) -> Result<(), LoadError> {
    if expected == found { Ok(()) } else { Err(LoadError::SectionLength { kind: section, expected, found }) }
}

struct Reader<'a> { words: &'a [u32], pc: usize, kind: u32 }

impl<'a> Reader<'a> {
    fn new(words: &'a [u32], kind: u32) -> Self { Self { words, pc: 0, kind } }
    fn take(&mut self, n: usize) -> Result<&'a [u32], LoadError> {
        let end = self.pc.checked_add(n).ok_or(LoadError::SizeOverflow { section: self.kind, index: self.pc as u32 })?;
        let out = self.words.get(self.pc..end).ok_or(LoadError::SectionLength { kind: self.kind, expected: end, found: self.words.len() })?;
        self.pc = end;
        Ok(out)
    }
    fn finish(&self) -> Result<(), LoadError> { exact(self.kind, self.pc, self.words.len()) }
}

fn string_id(id: u32, count: u32, section: u32, row: u32) -> Result<(), LoadError> {
    if id < count { Ok(()) } else { Err(LoadError::StringId { section, row, id }) }
}

// Table settings win over free TOPs. Of the table settings, choose the
// nearest frequency (ties go to the smaller prescaler). A free TOP uses
// the first prescaler whose rounded TOP fits, preserving level resolution.
fn pwm_setting(timer: u8, table: &pwm::PwmTimer, clock_hz: u32, asked_mhz: u32) -> Option<pwm::PwmSetting> {
    let asked = asked_mhz as u64;
    if asked == 0 { return None; }
    let best = table.prescalers.iter().map(|n| pwm::PwmSetting { timer, mode: table.mode, prescale: *n, top: table.top })
        .filter(|s| s.period_cycles() != 0)
        .min_by_key(|s| (s.real_mhz(clock_hz).abs_diff(asked), s.prescale));
    if let Some(s) = best.filter(|s| pwm::matches(s.real_mhz(clock_hz), asked)) { return Some(s); }
    let f = clock_hz as u64 * 1000;
    for &n in table.prescalers {
        if n == 0 { continue; }
        // n <= u16::MAX and asked <= i32::MAX at the call site; even
        // twice their product plus f fits comfortably in u64.
        let scaled = n as u64 * asked;
        let top = match table.mode {
            pwm::PwmMode::PhaseCorrect => (f + scaled) / (2 * scaled),
            pwm::PwmMode::Fast => {
                let Some(top) = ((f + scaled / 2) / scaled).checked_sub(1) else { continue };
                top
            }
        };
        if (3..=table.free_top as u64).contains(&top) {
            let s = pwm::PwmSetting { timer, mode: table.mode, prescale: n, top: top as u16 };
            return pwm::matches(s.real_mhz(clock_hz), asked).then_some(s);
        }
    }
    None
}

pub fn load<'a>(words: &'a [u32], profile: &Profile) -> Result<Module<'a>, LoadError> {
    load_domain(words, profile, None)
}

/// Kernel privilege is selected by this API alone, never by module contents.
pub fn load_kernel<'a>(words: &'a [u32], profile: &Profile, lines: &KernelLines) -> Result<Module<'a>, LoadError> {
    load_domain(words, profile, Some(*lines))
}

fn load_domain<'a>(words: &'a [u32], profile: &Profile, lines: Option<KernelLines>) -> Result<Module<'a>, LoadError> {
    // Fixed-index reads below follow an exact-size check, take(), or
    // chunks_exact(). Counts from the file never directly size an allocation.
    if words.len() < HEADER_WORDS { return Err(LoadError::HeaderLength { found: words.len() }); }
    if words[H_MAGIC] != MAGIC { return Err(LoadError::Magic { found: words[H_MAGIC] }); }
    if !matches!(words[H_VERSION], FORMAT_VERSION | FUNCTION_VERSION) { return Err(LoadError::FormatVersion { found: words[H_VERSION] }); }
    if words[H_PORT_VERSION] != ports::PORT_TABLE_VERSION { return Err(LoadError::PortVersion { found: words[H_PORT_VERSION] }); }
    if words[H_WORDS] as usize != words.len() { return Err(LoadError::ModuleLength { declared: words[H_WORDS], found: words.len() }); }
    if words[H_PROFILE] != 0 && words[H_PROFILE] != profile.id { return Err(LoadError::Profile { found: words[H_PROFILE], expected: profile.id }); }
    if let Some(DisplayProfile { width, height }) = profile.display {
        let pixels = width as u64 * height as u64;
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 || pixels > (isize::MAX as u64 / 2) {
            return Err(LoadError::DisplaySize { width, height });
        }
    }
    for at in [H_RESERVED0, H_RESERVED1] {
        if words[at] != 0 { return Err(LoadError::Reserved { at, found: words[at] }); }
    }
    let count = words[H_SECTIONS];
    let dir_end = size(count, DIRECTORY_WORDS, 0)?.checked_add(HEADER_WORDS).ok_or(LoadError::Directory { count, words: words.len() })?;
    let directory = words.get(HEADER_WORDS..dir_end).ok_or(LoadError::Directory { count, words: words.len() })?;
    let mut sections = [None; SEC_BUFS as usize];
    let mut ranges = [None; SEC_BUFS as usize];
    for d in directory.chunks_exact(DIRECTORY_WORDS) {
        let (kind, offset, len) = (d[0], d[1], d[2]);
        if !(SEC_LAYOUT..=SEC_BUFS).contains(&kind) { return Err(LoadError::UnknownSection { kind }); }
        let k = (kind - 1) as usize;
        if sections[k].is_some() { return Err(LoadError::DuplicateSection { kind }); }
        let start = offset as usize;
        let end = start.checked_add(len as usize).ok_or(LoadError::SectionRange { kind, offset, len })?;
        if start < dir_end { return Err(LoadError::SectionRange { kind, offset, len }); }
        let section = words.get(start..end).ok_or(LoadError::SectionRange { kind, offset, len })?;
        for (j, range) in ranges.iter().enumerate() {
            if let Some((s, e)) = *range {
                if start < end && s < e && start < e && s < end { return Err(LoadError::SectionOverlap { first: j as u32 + 1, second: kind }); }
            }
        }
        sections[k] = Some(section);
        ranges[k] = Some((start, end));
    }
    let section = |kind: u32| -> Result<&'a [u32], LoadError> {
        sections.get((kind - 1) as usize).copied().flatten().ok_or(LoadError::MissingSection { kind })
    };
    for kind in SEC_LAYOUT..=SEC_CODE { section(kind)?; }
    let layout = section(SEC_LAYOUT)?;
    exact(SEC_LAYOUT, LAYOUT_WORDS, layout.len())?;
    let limit = u32::from(crate::watch::SITE_RADIO_SCHEDULER);
    if layout[L_SITES] > limit { return Err(LoadError::Sites { sites: layout[L_SITES], limit }); }
    if words[H_VERSION] == FORMAT_VERSION && (layout[L_FUNCS] != 0 || sections[(SEC_FUNCS - 1) as usize].is_some()) {
        return Err(LoadError::FormatVersion { found: words[H_VERSION] });
    }
    let func_data = if layout[L_FUNCS] != 0 { section(SEC_FUNCS)? } else { sections[(SEC_FUNCS - 1) as usize].unwrap_or(&[]) };
    exact(SEC_FUNCS, size(layout[L_FUNCS], FUNC_WORDS, SEC_FUNCS)?, func_data.len())?;
    let state = section(SEC_STATE_INIT)?;
    let shared = section(SEC_SHARED)?;
    exact(SEC_STATE_INIT, layout[L_STATE_WORDS] as usize, state.len())?;
    exact(SEC_SHARED, layout[L_SHARED_WORDS] as usize, shared.len())?;
    let nstrings = layout[L_STRINGS];
    let nentries = layout[L_ENTRIES];
    let mut strings = Vec::new();
    let mut r = Reader::new(section(SEC_STRINGS)?, SEC_STRINGS);
    for string in 0..nstrings {
        let bytes = r.take(1)?[0];
        let packed = (bytes as usize / 4).checked_add(usize::from(bytes % 4 != 0)).ok_or(LoadError::SizeOverflow { section: SEC_STRINGS, index: string })?;
        if packed > r.words.len() - r.pc { return Err(LoadError::StringLength { string, bytes, remaining: r.words.len() - r.pc }); }
        let data = r.take(packed)?;
        let mut raw = Vec::new();
        reserve(&mut raw, bytes as usize)?;
        let mut left = bytes as usize;
        for w in data {
            let bs = w.to_le_bytes();
            let n = left.min(4);
            raw.extend_from_slice(&bs[..n]);
            if bs[n..].iter().any(|b| *b != 0) { return Err(LoadError::StringPadding { string, word: *w }); }
            left -= n;
        }
        let text = String::from_utf8(raw).map_err(|e| LoadError::Utf8 { string, valid_up_to: e.utf8_error().valid_up_to() })?;
        push(&mut strings, text)?;
    }
    r.finish()?;
    let mut vars = Vec::new();
    let data = section(SEC_VARS)?;
    exact(SEC_VARS, size(layout[L_VARS], VAR_WORDS, SEC_VARS)?, data.len())?;
    for (k, v) in data.chunks_exact(VAR_WORDS).enumerate() {
        string_id(v[0], nstrings, SEC_VARS, k as u32)?;
        string_id(v[1], nstrings, SEC_VARS, k as u32)?;
        if v[2].checked_add(v[3]).is_none_or(|end| end > layout[L_STATE_WORDS]) {
            return Err(LoadError::VarRange { var: k as u32, offset: v[2], words: v[3], state: layout[L_STATE_WORDS] });
        }
        push(&mut vars, Var { name: v[0], sig: v[1], offset: v[2], words: v[3] })?;
    }
    let mut images = Vec::new();
    if let Some(data) = sections[(SEC_IMAGES - 1) as usize] {
        let mut r = Reader::new(data, SEC_IMAGES);
        let count = r.take(1)?[0];
        for image in 0..count {
            let dimensions = r.take(2)?;
            let (width, height) = (dimensions[0], dimensions[1]);
            if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
                return Err(LoadError::ImageSize { image, width, height });
            }
            let count = (width as usize).checked_mul(height as usize).ok_or(LoadError::SizeOverflow { section: SEC_IMAGES, index: image })?;
            let pixels = r.take(count.div_ceil(2))?;
            // Even dimensions leave no partial word to check for zero padding.
            push(&mut images, Image { width, height, pixels })?;
        }
        r.finish()?;
    }
    let mut display = None;
    let mut eth = false;
    let mut motion = false;
    let mut touch = false;
    let mut speaker_rate = None;
    let mut mic_rate = None;
    let mut decls = Vec::new();
    let mut pwm: Vec<pwm::PwmOut> = Vec::new();
    let mut serial: Vec<serial::SerialOut> = Vec::new();
    let mut pins = Vec::new();
    let mut bound_entries = Vec::new();
    let mut timer = 0i32;
    let mut buffer = 0i32;
    let mut net = 0i32;
    let mut r = Reader::new(section(SEC_DECLS)?, SEC_DECLS);
    for k in 0..layout[L_DECLS] {
        let h = r.take(DECL_HEADER_WORDS)?;
        let (form, handle, name, nargs) = (h[0], h[1] as i32, h[2], h[3]);
        string_id(name, nstrings, SEC_DECLS, k)?;
        if lines.is_some() && form < ports::FORM_EXPORT || lines.is_none() && form >= ports::FORM_EXPORT {
            return Err(LoadError::DomainForm { decl: k, form, kernel: lines.is_some() });
        }
        if if lines.is_some() { form != ports::FORM_EXPORT } else { form >= ports::FORMS } {
            return Err(LoadError::DeclForm { decl: k, form });
        }
        if form == ports::FORM_ETH && eth { return Err(LoadError::DuplicateEth { decl: k }); }
        if form == ports::FORM_MOTION && motion { return Err(LoadError::DuplicateMotion { decl: k }); }
        if form == ports::FORM_TOUCH && touch { return Err(LoadError::DuplicateTouch { decl: k }); }
        let mut args = Vec::new();
        for (a, pair) in r.take(size(nargs, 2, SEC_DECLS)?)?.chunks_exact(2).enumerate() {
            let arg = match pair[0] {
                ARG_INT => DeclArg::Int(pair[1] as i32),
                ARG_FLOAT => DeclArg::Float(f32::from_bits(pair[1])),
                ARG_ENTRY => {
                    if pair[1] >= nentries || pair[1] > u16::MAX as u32 { return Err(LoadError::DeclEntry { decl: k, entry: pair[1] }); }
                    DeclArg::Entry(pair[1] as u16)
                }
                tag => return Err(LoadError::DeclArgTag { decl: k, arg: a as u32, tag }),
            };
            push(&mut args, arg)?;
        }
        match (form, args.as_slice()) {
            (ports::FORM_EXPORT, [DeclArg::Entry(entry), DeclArg::Int(n)]) if (0..=6).contains(n) => {
                if decls.iter().any(|d: &Decl| strings[d.name as usize] == strings[name as usize]) {
                    return Err(LoadError::DuplicateExport { decl: k });
                }
                if bound_entries.contains(entry) { return Err(LoadError::DuplicateEntry { decl: k, entry: *entry }); }
                for param in (0..*n as u32).map(Some).chain(core::iter::once(None)) {
                    let slot = match param {
                        Some(p) => format!("{}#p{p}", strings[name as usize]),
                        None => format!("{}#r", strings[name as usize]),
                    };
                    let bad = || LoadError::ExportSlot { decl: k, param };
                    let mut found = vars.iter().filter(|v| strings[v.name as usize] == slot);
                    let v = found.next().ok_or_else(bad)?;
                    if found.next().is_some() || strings[v.sig as usize] != "i32" || v.words != 1 {
                        return Err(bad());
                    }
                    // A hand-built stream must not alias one gate slot with
                    // another slot or with user state.
                    if vars.iter().any(|other| !core::ptr::eq(v, other) && other.offset <= v.offset && u64::from(v.offset) < u64::from(other.offset) + u64::from(other.words)) {
                        return Err(bad());
                    }
                }
                push(&mut bound_entries, *entry)?;
            }
            (ports::FORM_EXPORT, _) => return Err(LoadError::ExportArgs { decl: k }),
            (ports::FORM_BYTES | ports::FORM_SAMPLES | ports::FORM_WORDS | ports::FORM_SURFACE, _) => {
                if buffer_length(form, &args).is_none() { return Err(LoadError::DeclArguments { decl: k, form }); }
                if handle != buffer { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: buffer }); }
                buffer = buffer.checked_add(1).ok_or(LoadError::SizeOverflow { section: SEC_DECLS, index: k })?;
            }
            (ports::FORM_DISPLAY, []) => {
                if display.is_some() { return Err(LoadError::DuplicateDisplay { decl: k }); }
                let panel = profile.display.ok_or(LoadError::DisplayUnsupported { decl: k })?;
                if handle != 0 { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: 0 }); }
                display = Some(panel);
            }
            (ports::FORM_IMAGE, [DeclArg::Int(index), DeclArg::Int(width), DeclArg::Int(height)]) => {
                if handle != buffer { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: buffer }); }
                let image = usize::try_from(*index).ok().and_then(|index| images.get(index))
                    .ok_or(LoadError::ImageIndex { decl: k, image: *index })?;
                if *width <= 0 || *height <= 0 || *width as u32 != image.width || *height as u32 != image.height {
                    return Err(LoadError::ImageDimensions { decl: k, image: *index, width: *width, height: *height });
                }
                buffer = buffer.checked_add(1).ok_or(LoadError::SizeOverflow { section: SEC_DECLS, index: k })?;
            }
            (ports::FORM_OUTPUT | ports::FORM_ANALOG, [DeclArg::Int(pin)]) |
            (ports::FORM_INPUT, [DeclArg::Int(pin), DeclArg::Int(0..=2)]) => {
                if *pin < 0 || *pin as u32 >= profile.pins { return Err(LoadError::PinRange { decl: k, pin: *pin, pins: profile.pins }); }
                if handle != *pin { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: *pin }); }
                if pins.contains(pin) { return Err(LoadError::DuplicatePin { decl: k, pin: *pin }); }
                let analog = profile.analog.get(*pin as u32);
                if form == ports::FORM_ANALOG {
                    if analog.is_none() { return Err(LoadError::AnalogPin { decl: k, pin: *pin }); }
                } else if analog.is_some_and(|p| !p.digital) {
                    return Err(LoadError::AnalogOnly { decl: k, pin: *pin });
                }
                push(&mut pins, *pin)?;
            }
            (ports::FORM_PWM, [DeclArg::Int(pin), DeclArg::Int(mhz)]) => {
                if *pin < 0 || *pin as u32 >= profile.pins { return Err(LoadError::PinRange { decl: k, pin: *pin, pins: profile.pins }); }
                if handle != *pin { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: *pin }); }
                if pins.contains(pin) { return Err(LoadError::DuplicatePin { decl: k, pin: *pin }); }
                if profile.analog.get(*pin as u32).is_some_and(|p| !p.digital) { return Err(LoadError::AnalogOnly { decl: k, pin: *pin }); }
                let output = profile.pwm.pins.iter().find(|p| p.pin == *pin as u32).ok_or(LoadError::PwmPin { decl: k, pin: *pin })?;
                let frequency_error = || LoadError::PwmFrequency { decl: k, pin: *pin, asked_mhz: *mhz as i64 };
                if *mhz <= 0 { return Err(frequency_error()); }
                let table = profile.pwm.timers.get(output.timer as usize).ok_or_else(frequency_error)?;
                let asked_mhz = *mhz as u32;
                let fixed = if table.clock {
                    let prescale = table.prescalers.first().copied().ok_or_else(frequency_error)?;
                    let s = pwm::PwmSetting { timer: output.timer, mode: table.mode, prescale, top: table.top };
                    if !pwm::matches(s.real_mhz(profile.clock_hz), asked_mhz as u64) {
                        return Err(LoadError::PwmClock { decl: k, pin: *pin, asked_mhz });
                    }
                    Some(s)
                } else { None };
                let setting = if let Some(other) = pwm.iter().find(|p| p.setting.timer == output.timer) {
                    if !pwm::matches(other.real_mhz, asked_mhz as u64) {
                        return Err(LoadError::PwmShared { decl: k, pin: *pin, asked_mhz, other_decl: other.decl, other_pin: other.pin, other_mhz: other.real_mhz });
                    }
                    other.setting
                } else {
                    fixed.or_else(|| pwm_setting(output.timer, table, profile.clock_hz, asked_mhz)).ok_or_else(frequency_error)?
                };
                push(&mut pwm, pwm::PwmOut { decl: k, pin: *pin as u32, asked_mhz, real_mhz: setting.real_mhz(profile.clock_hz), setting })?;
                push(&mut pins, *pin)?;
            }
            (ports::FORM_MOTION, [DeclArg::Int(period), DeclArg::Entry(entry)]) => {
                if profile.motion.is_none() { return Err(LoadError::MotionUnsupported { decl: k }); }
                if *period <= 0 { return Err(LoadError::DeclArguments { decl: k, form }); }
                if handle != 0 { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: 0 }); }
                if bound_entries.contains(entry) { return Err(LoadError::DuplicateEntry { decl: k, entry: *entry }); }
                push(&mut bound_entries, *entry)?;
                motion = true;
            }
            (ports::FORM_TOUCH, [DeclArg::Int(period), DeclArg::Entry(entry)]) => {
                if !profile.touch { return Err(LoadError::TouchUnsupported { decl: k }); }
                if !(1..=300_000).contains(period) { return Err(LoadError::TouchPeriod { decl: k, period: *period }); }
                if handle != 0 { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: 0 }); }
                if bound_entries.contains(entry) { return Err(LoadError::DuplicateEntry { decl: k, entry: *entry }); }
                push(&mut bound_entries, *entry)?;
                touch = true;
            }
            (ports::FORM_SPEAKER | ports::FORM_MIC, args) => {
                let (name, duration, step, max, previous, other) = if form == ports::FORM_SPEAKER {
                    ("speaker", "queue_ms", 5, ports::SPEAKER_MAX_MS, &mut speaker_rate, mic_rate)
                } else {
                    ("mic", "history_ms", 20, ports::MIC_MAX_MS, &mut mic_rate, speaker_rate)
                };
                let bad = |reason| LoadError::AudioDecl { decl: k, reason };
                let [DeclArg::Int(rate), DeclArg::Int(ms), DeclArg::Entry(entry)] = args else {
                    return Err(bad(format!("{name} needs (integer rate_hz, integer {duration}, fn() {{ ... }})")));
                };
                if previous.is_some() { return Err(bad(format!("at most one {name} may be declared"))); }
                let audio = profile.audio.ok_or_else(|| bad(format!("no {name} on this chip")))?;
                if form == ports::FORM_MIC && !audio.mic { return Err(bad("no mic input on this board".into())); }
                if *rate <= 0 || !audio.rates.contains(&(*rate as u32)) {
                    return Err(bad(format!("{name} cannot clock {rate} Hz on this board. Supported rates in Hz are {:?}", audio.rates)));
                }
                if let Some(other) = other {
                    if *rate != other { return Err(bad(format!("speaker and mic must use the same rate, got {other} and {rate} Hz"))); }
                }
                if !(20..=max).contains(ms) || *ms % step != 0 {
                    return Err(bad(format!("{name} {duration} must be 20..={max} ms in multiples of {step}, got {ms}")));
                }
                if handle != 0 { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: 0 }); }
                if bound_entries.contains(entry) { return Err(LoadError::DuplicateEntry { decl: k, entry: *entry }); }
                push(&mut bound_entries, *entry)?;
                *previous = Some(*rate);
            }
            (ports::FORM_NET, [DeclArg::Int(kind), DeclArg::Int(slots), DeclArg::Int(send_bytes), DeclArg::Entry(entry)]) => {
                if !(0..=1).contains(kind) || *slots < 2 || *send_bytes < 1 {
                    return Err(LoadError::NetDecl { decl: k, kind: *kind, slots: *slots, send_bytes: *send_bytes });
                }
                if handle != net { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: net }); }
                net = net.checked_add(1).filter(|n| *n <= 256).ok_or(LoadError::SizeOverflow { section: SEC_DECLS, index: k })?;
                if bound_entries.contains(entry) { return Err(LoadError::DuplicateEntry { decl: k, entry: *entry }); }
                push(&mut bound_entries, *entry)?;
            }
            (ports::FORM_ETH, [DeclArg::Int(count), DeclArg::Entry(entry)]) => {
                if *count < 1 { return Err(LoadError::EthCount { decl: k, count: *count }); }
                if handle != 0 { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: 0 }); }
                if bound_entries.contains(entry) { return Err(LoadError::DuplicateEntry { decl: k, entry: *entry }); }
                push(&mut bound_entries, *entry)?;
                eth = true;
            }
            (ports::FORM_SERIAL, [DeclArg::Int(baud), DeclArg::Entry(entry)]) => {
                let ordinal = i32::try_from(serial.len()).map_err(|_| LoadError::SizeOverflow { section: SEC_DECLS, index: k })?;
                if handle != ordinal { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: ordinal }); }
                if bound_entries.contains(entry) { return Err(LoadError::DuplicateEntry { decl: k, entry: *entry }); }
                let link = ordinal as u32;
                if link >= profile.serial.links { return Err(LoadError::SerialLinks { decl: k, links: profile.serial.links }); }
                if *baud <= 0 { return Err(LoadError::SerialBaud { decl: k, asked: *baud as i64, nearest_milli: 0 }); }
                let setting = serial::resolve(profile.serial, profile.clock_hz, link, *baud as u32)
                    .map_err(|nearest_milli| LoadError::SerialBaud { decl: k, asked: *baud as i64, nearest_milli })?;
                push(&mut serial, serial::SerialOut { decl: k, link, entry: *entry, setting, fifo: 0 })?;
                push(&mut bound_entries, *entry)?;
            }
            (ports::FORM_EVERY | ports::FORM_AFTER, [DeclArg::Int(period), DeclArg::Entry(entry)]) => {
                if *period < 0 || (form == ports::FORM_EVERY && *period == 0) { return Err(LoadError::DeclArguments { decl: k, form }); }
                if handle != timer { return Err(LoadError::DeclHandle { decl: k, found: handle, expected: timer }); }
                timer = timer.checked_add(1).ok_or(LoadError::SizeOverflow { section: SEC_DECLS, index: k })?;
                if bound_entries.contains(entry) { return Err(LoadError::DuplicateEntry { decl: k, entry: *entry }); }
                push(&mut bound_entries, *entry)?;
            }
            _ => return Err(LoadError::DeclArguments { decl: k, form }),
        }
        push(&mut decls, Decl { form, handle, name, args })?;
    }
    r.finish()?;
    let mut buffers = Vec::new();
    let data = sections[(SEC_BUFS - 1) as usize].unwrap_or(&[]);
    exact(SEC_BUFS, data.len() / BUF_WORDS * BUF_WORDS, data.len())?;
    let mut expected = decls.iter().enumerate().filter(|(_, d)| buffer_elem(d.form).is_some());
    let mut buffer_bytes = 0u64;
    for (k, row) in data.chunks_exact(BUF_WORDS).enumerate() {
        let buffer = k as u32;
        let decl = row[0];
        let d = decls.get(decl as usize).ok_or(LoadError::BufferDecl { buffer, decl })?;
        let elem = buffer_elem(d.form).ok_or(LoadError::BufferDecl { buffer, decl })?;
        let next = expected.next().map_or(NONE, |(k, _)| k as u32);
        if next != decl { return Err(LoadError::BufferOrder { buffer, decl, expected: next }); }
        if row[1] != elem as u32 { return Err(LoadError::BufferElem { buffer, decl, found: row[1], expected: elem }); }
        let len = buffer_length(d.form, &d.args).ok_or(LoadError::DeclArguments { decl, form: d.form })?;
        if row[2] != len { return Err(LoadError::BufferLength { buffer, decl, found: row[2], expected: len }); }
        if row[3] & !1 != 0 { return Err(LoadError::BufferFlags { buffer, flags: row[3] }); }
        let writable = row[3] & 1 != 0;
        // The flag must match the form: images are read-only, every other buffer is writable.
        if writable == (d.form == ports::FORM_IMAGE) { return Err(LoadError::BufferWritable { buffer, decl, writable }); }
        if writable {
            let bytes = (u64::from(len) * u64::from(elem.bytes())).div_ceil(4) * 4;
            buffer_bytes = buffer_bytes.checked_add(bytes).ok_or(LoadError::SizeOverflow { section: SEC_BUFS, index: buffer })?;
        }
        push(&mut buffers, Buffer { decl, elem, len, writable })?;
    }
    if let Some((decl, _)) = expected.next() { return Err(LoadError::BufferMissing { decl: decl as u32 }); }
    if buffer_bytes > u64::from(profile.buffer_bytes) { return Err(LoadError::BufferBytes { needed: buffer_bytes, limit: profile.buffer_bytes }); }
    for (k, buffer) in buffers.iter().enumerate() {
        if buffer.words() > isize::MAX as usize / 4 { return Err(LoadError::SizeOverflow { section: SEC_BUFS, index: k as u32 }); }
    }
    let code = section(SEC_CODE)?;
    let data = section(SEC_ENTRIES)?;
    exact(SEC_ENTRIES, size(nentries, ENTRY_WORDS, SEC_ENTRIES)?, data.len())?;
    let mut entries = Vec::new();
    for (k, e) in data.chunks_exact(ENTRY_WORDS).enumerate() {
        string_id(e[0], nstrings, SEC_ENTRIES, k as u32)?;
        if lines.is_some() && (strings[e[0] as usize] == crate::SETUP || !decls.iter().any(|d| {
            matches!(d.args.first(), Some(DeclArg::Entry(entry)) if usize::from(*entry) == k) && strings[d.name as usize] == strings[e[0] as usize]
        })) { return Err(LoadError::KernelEntry { entry: k as u32 }); }
        if e[1].checked_add(e[2]).is_none_or(|end| end as usize > code.len()) { return Err(LoadError::EntryRange { entry: k as u32, offset: e[1], len: e[2] }); }
        if e[4] != 0 { return Err(LoadError::CostHighWord { entry: k as u32, high: e[4] }); }
        push(&mut entries, Entry { name: e[0], code: e[1], len: e[2], cost: e[3] as u64 | (e[4] as u64) << 32, n_vals: 0, n_vars: 0, frame_words: 0, wide: false, depth: 0, body: 0 })?;
    }
    let mut funcs = Vec::new();
    for (k, row) in func_data.chunks_exact(FUNC_WORDS).enumerate() {
        string_id(row[0], nstrings, SEC_FUNCS, k as u32)?;
        let mut program = Entry { name: row[0], code: row[1], len: row[2], cost: u64::from(row[5]),
            n_vals: 0, n_vars: 0, frame_words: 0, wide: false, depth: 0, body: 0 };
        let (params, results) = program_header(code, k as u32, &mut program, row[3], row[4], profile)?;
        push(&mut funcs, Func { program, params, results })?;
    }
    for (k, entry) in entries.iter_mut().enumerate() {
        program_header(code, k as u32, entry, 0, 0, profile)?;
    }
    if let Some(first) = funcs.first() {
        let expected = first.program.frame_words;
        for (k, e) in entries.iter().chain(funcs.iter().map(|f| &f.program)).enumerate() {
            if e.frame_words != expected { return Err(LoadError::FrameMismatch { entry: k as u32, found: e.frame_words, expected }); }
        }
    }
    // Verify operands before traversing any call edges. This pass deliberately
    // does not follow calls or trust their costs (a cycle is still possible).
    let mut graph = Vec::new();
    let mut writes_eeprom = false;
    for k in 0..funcs.len() {
        let f = &funcs[k];
        let mut e = f.program.clone();
        let checked = verify(code, k as u32, &mut e, f.params, f.results, &funcs, None, layout, profile, &decls, &buffers, lines.as_ref(), 1, false)?;
        writes_eeprom |= checked.writes_eeprom;
        funcs[k].program = e;
        push(&mut graph, checked.calls)?;
    }
    let func_order = call_order(&graph)?;
    let mut costs = Vec::new();
    reserve(&mut costs, funcs.len())?;
    costs.resize(funcs.len(), 0);
    for &k in &func_order {
        let f = &funcs[k as usize];
        let mut e = f.program.clone();
        let checked = verify(code, k, &mut e, f.params, f.results, &funcs, Some(&costs), layout, profile, &decls, &buffers, lines.as_ref(), 1, true)?;
        costs[k as usize] = checked.cost;
    }
    for (k, e) in entries.iter_mut().enumerate() {
        writes_eeprom |= verify(code, k as u32, e, &[], &[], &funcs, Some(&costs), layout, profile, &decls, &buffers, lines.as_ref(), 1, true)?.writes_eeprom;
    }
    // EEPROM waits are a profile cost in addition to AIR's function costs.
    // Discover writes in every body, then propagate waits through the DAG.
    if writes_eeprom && profile.eeprom_access_ops() > 1 {
        for &k in &func_order {
            let f = &funcs[k as usize];
            let mut e = f.program.clone();
            costs[k as usize] = verify(code, k, &mut e, f.params, f.results, &funcs, Some(&costs), layout, profile, &decls, &buffers, lines.as_ref(), profile.eeprom_access_ops(), false)?.cost;
        }
        for (k, entry) in entries.iter_mut().enumerate() {
            verify(code, k as u32, entry, &[], &[], &funcs, Some(&costs), layout, profile, &decls, &buffers, lines.as_ref(), profile.eeprom_access_ops(), true)?;
        }
    }
    let mut scratch = Vec::new();
    let mut max_scratch = 0;
    for (k, entry) in entries.iter().enumerate() {
        let n = crate::exec::scratch_size(entry, &funcs).ok_or(LoadError::SizeOverflow { section: SEC_CODE, index: k as u32 })?;
        max_scratch = max_scratch.max(n);
        push(&mut scratch, n)?;
    }
    // Any entry, including setup and timers, can hold the CPU while bytes
    // arrive. Only verified costs may size the receive buffers.
    let wcet_ns = entries.iter().map(|e| e.cost).max().map_or(0, |cost| cost as u128 * profile.op_ns as u128);
    let mut fifo = 0u32;
    for output in &mut serial {
        let decl = output.decl;
        let overflow = || LoadError::SizeOverflow { section: SEC_DECLS, index: decl };
        let byte_ns = output.setting.byte_ns(profile.clock_hz, profile.serial.frame_bits) as u128;
        if byte_ns == 0 { return Err(overflow()); }
        output.fifo = u32::try_from(wcet_ns.div_ceil(byte_ns) + 2).map_err(|_| overflow())?;
        fifo = fifo.checked_add(output.fifo).ok_or_else(overflow)?;
    }
    if state.len() as u128 + max_scratch as u128 + fifo as u128 > profile.ram_words as u128 {
        return Err(LoadError::Ram { state: layout[L_STATE_WORDS], scratch: max_scratch, fifo, limit: profile.ram_words });
    }
    if !buffers.is_empty() {
        let words = layout[L_SITES].div_ceil(32);
        let available = profile.ram_words - (state.len() + max_scratch) as u32 - fifo;
        if words > available { return Err(LoadError::BufferFaultRam { words, available }); }
    }
    Ok(Module { kernel_lines: lines, state, shared, code, sites: layout[L_SITES], strings, entries, funcs, func_order, decls, pwm, serial, display, motion: profile.motion.filter(|_| motion), touch, images, buffers, vars, scratch })
}

#[derive(Clone, Copy)]
struct Types<'a> { words: &'a [u32], count: u32 }

impl Types<'_> {
    fn get(&self, index: u32) -> Option<u8> {
        if index >= self.count { return None; }
        self.words.get((index / 4) as usize).map(|w| (w >> ((index % 4) * 8)) as u8)
    }
}

fn type_table<'a>(code: &'a [u32], pc: &mut u32, end: u32, count: u32, entry: u32, variable: bool) -> Result<(Types<'a>, bool), LoadError> {
    let at = *pc;
    let n = count / 4 + u32::from(count % 4 != 0);
    let words = take_code(code, pc, n, end, entry, at)?;
    let mut wide = false;
    for (k, w) in words.iter().enumerate() {
        for (b, t) in w.to_le_bytes().iter().copied().enumerate() {
            let index = k as u64 * 4 + b as u64;
            if index < count as u64 {
                if t > TY_F64 { return Err(LoadError::TypeCode { entry, at: at + k as u32, index: index as u32, variable, found: t }); }
                wide |= t == TY_F64;
            } else if t != 0 {
                return Err(LoadError::TypePadding { entry, at: at + k as u32, found: *w });
            }
        }
    }
    Ok((Types { words, count }, wide))
}

fn take_code<'a>(code: &'a [u32], pc: &mut u32, n: u32, end: u32, entry: u32, at: u32) -> Result<&'a [u32], LoadError> {
    let next = pc.checked_add(n).filter(|next| *next <= end).ok_or(LoadError::BlockLength { entry, at, len: n, end })?;
    let words = code.get(*pc as usize..next as usize).ok_or(LoadError::BlockLength { entry, at, len: n, end })?;
    *pc = next;
    Ok(words)
}

fn block_end(code: &[u32], at: u32, end: u32, entry: u32) -> Result<u32, LoadError> {
    let mut pc = at;
    let n = take_code(code, &mut pc, 1, end, entry, at)?[0];
    pc.checked_add(n).filter(|next| *next <= end).ok_or(LoadError::BlockLength { entry, at, len: n, end })
}

struct Verify<'a> {
    entry: u32,
    at: u32,
    vals: Types<'a>,
    vars: Types<'a>,
    // 0: never defined; 1: visible; 2: defined in a finished scope.
    flags: Vec<u8>,
    scope: Vec<u32>,
    state: u32,
    shared: u32,
    frame: u32,
    sites: u32,
    buffers: &'a [Buffer],
}

impl Verify<'_> {
    fn ty(&self, index: u32, variable: bool) -> Result<u8, LoadError> {
        let ts = if variable { self.vars } else { self.vals };
        ts.get(index).ok_or(LoadError::OperandRange { entry: self.entry, at: self.at, index, variable })
    }
    fn type_is(&self, index: u32, found: u8, expected: u8) -> Result<(), LoadError> {
        if found == expected { Ok(()) } else { Err(LoadError::TypeMismatch { entry: self.entry, at: self.at, index, found, expected }) }
    }
    fn used(&self, index: u32, expected: Option<u8>) -> Result<u8, LoadError> {
        let t = self.ty(index, false)?;
        if self.flags.get(index as usize) != Some(&1) { return Err(LoadError::ValueUndefined { entry: self.entry, at: self.at, value: index }); }
        if let Some(want) = expected { self.type_is(index, t, want)?; }
        Ok(t)
    }
    fn fresh(&self, index: u32) -> Result<u8, LoadError> {
        let t = self.ty(index, false)?;
        if self.flags.get(index as usize) != Some(&0) { return Err(LoadError::ValueRedefined { entry: self.entry, at: self.at, value: index }); }
        Ok(t)
    }
    fn define(&mut self, index: u32, want: u8) -> Result<(), LoadError> {
        self.type_is(index, self.fresh(index)?, want)?;
        push(&mut self.scope, index)?;
        if let Some(flag) = self.flags.get_mut(index as usize) { *flag = 1; }
        Ok(())
    }
    fn access(&self, region: u32, buffer: Option<u32>, base: u32, extent: u32, off: u32, write: bool) -> Result<(), LoadError> {
        if off != NONE { self.used(off, Some(TY_I32))?; }
        if let Some(buffer) = buffer {
            let b = self.buffers.get(buffer as usize).ok_or(LoadError::BufferRegion { entry: self.entry, at: self.at, buffer })?;
            if write && !b.writable { return Err(LoadError::BufferReadOnly { entry: self.entry, at: self.at, buffer }); }
            // Like ir::buf_index, buffers clamp base + offset to their actual
            // length, independent of the static extent (also for constant bases).
            return Ok(());
        }
        let size = match region {
            REGION_STATE => self.state,
            REGION_SHARED => self.shared,
            REGION_FRAME => self.frame,
            _ => return Err(LoadError::Region { entry: self.entry, at: self.at, region }),
        };
        if extent == 0 || base.checked_add(extent).is_none_or(|end| end > size) { return Err(LoadError::MemoryRange { entry: self.entry, at: self.at, region, base, extent, size }); }
        if write && region == REGION_SHARED { return Err(LoadError::SharedWrite { entry: self.entry, at: self.at }); }
        Ok(())
    }
    fn narrow(&self, index: u32, ty: u8) -> Result<(), LoadError> {
        if ty == TY_F64 { Err(LoadError::WideMemory { entry: self.entry, at: self.at, value: index }) } else { Ok(()) }
    }
    fn sub_error(&self, opcode: u32, sub: u32) -> LoadError { LoadError::SubOp { entry: self.entry, at: self.at, opcode, sub } }
}

fn un_types(sub: u32) -> Option<(u8, u8)> {
    Some(match sub {
        UN_NEG_F..=UN_ROUND_F => (TY_F32, TY_F32),
        UN_F2I | UN_BITS_FI => (TY_F32, TY_I32),
        UN_I2F | UN_BITS_IF => (TY_I32, TY_F32),
        UN_NEG_I => (TY_I32, TY_I32),
        UN_NOT_B => (TY_BOOL, TY_BOOL),
        UN_NEG_D..=UN_ROUND_D => (TY_F64, TY_F64),
        UN_F2D => (TY_F32, TY_F64),
        UN_D2F => (TY_F64, TY_F32),
        UN_D2I | UN_HI_D | UN_LO_D => (TY_F64, TY_I32),
        UN_I2D => (TY_I32, TY_F64),
        _ => return None,
    })
}

fn bin_types(sub: u32) -> Option<(u8, u8)> {
    Some(match sub {
        BIN_ADD_F..=BIN_MAX_F => (TY_F32, TY_F32),
        BIN_ADD_I..=BIN_SHR_UI => (TY_I32, TY_I32),
        BIN_AND_B | BIN_OR_B => (TY_BOOL, TY_BOOL),
        BIN_ADD_D..=BIN_MAX_D => (TY_F64, TY_F64),
        BIN_MAKE_D => (TY_I32, TY_F64),
        _ => return None,
    })
}

enum Finish {
    Root,
    Loop(u32),
    Then { otherwise: u32, end: u32 },
    Else { then_cost: u64 },
}

struct Block {
    end: u32,
    mark: usize,
    loops: u32,
    cost: u64,
    finish: Finish,
}

fn program_header<'a>(code: &'a [u32], k: u32, e: &mut Entry, nparams: u32, nresults: u32, profile: &Profile) -> Result<(&'a [u32], &'a [u32]), LoadError> {
    let end = e.code.checked_add(e.len).ok_or(LoadError::EntryRange { entry: k, offset: e.code, len: e.len })?;
    let mut pc = e.code;
    let h = take_code(code, &mut pc, PROGRAM_WORDS as u32, end, k, e.code).map_err(|_| LoadError::ProgramHeader { entry: k, at: e.code })?;
    e.n_vals = h[P_VALS];
    e.n_vars = h[P_VARS];
    e.frame_words = h[P_FRAME];
    let (vals, vwide) = type_table(code, &mut pc, end, e.n_vals, k, false)?;
    let (_, rwide) = type_table(code, &mut pc, end, e.n_vars, k, true)?;
    e.wide = vwide || rwide;
    if h[P_WIDE] != u32::from(e.wide) { return Err(LoadError::WideFlag { entry: k, found: h[P_WIDE], expected: e.wide }); }
    if e.wide && !profile.f64 { return Err(LoadError::F64Unsupported { entry: k }); }
    let params = take_code(code, &mut pc, nparams, end, k, e.code)?;
    let results = take_code(code, &mut pc, nresults, end, k, e.code)?;
    for &index in params.iter().chain(results) {
        if vals.get(index).is_none() { return Err(LoadError::OperandRange { entry: k, at: pc, index, variable: false }); }
    }
    e.body = pc;
    Ok((params, results))
}

struct Verified { writes_eeprom: bool, cost: u64, calls: Vec<u32> }

fn verify(code: &[u32], k: u32, e: &mut Entry, params: &[u32], results: &[u32], funcs: &[Func<'_>], costs: Option<&[u64]>, layout: &[u32], profile: &Profile, decls: &[Decl], buffers: &[Buffer], lines: Option<&KernelLines>, eeprom_ops: u64, check_cost: bool) -> Result<Verified, LoadError> {
    let end = e.code + e.len;
    let mut pc = e.code + PROGRAM_WORDS as u32;
    let (vals, _) = type_table(code, &mut pc, end, e.n_vals, k, false)?;
    let (vars, _) = type_table(code, &mut pc, end, e.n_vars, k, true)?;
    pc = e.body;
    let mut calls = Vec::new();
    let body_end = block_end(code, pc, end, k)?;
    if body_end != end { return Err(LoadError::BlockLength { entry: k, at: pc, len: body_end - pc - 1, end }); }
    pc += 1; // block_end proved the length word exists.
    let mut flags = Vec::new();
    reserve(&mut flags, e.n_vals as usize)?;
    flags.resize(e.n_vals as usize, 0);
    // The i32 constants by value index: pin constants are checked against
    // declarations; buffer arguments must be constants naming buffer rows.
    let mut konst: Vec<Option<i32>> = Vec::new();
    reserve(&mut konst, e.n_vals as usize)?;
    konst.resize(e.n_vals as usize, None);
    let mut v = Verify { entry: k, at: pc, vals, vars, flags, scope: Vec::new(), state: layout[L_STATE_WORDS], shared: layout[L_SHARED_WORDS], frame: e.frame_words, sites: layout[L_SITES], buffers };
    for &param in params { v.define(param, v.ty(param, false)?)?; }
    let mut stack = Vec::new();
    push(&mut stack, Block { end, mark: 0, loops: 0, cost: 0, finish: Finish::Root })?;
    let mut computed = 0;
    let mut writes_eeprom = false;
    e.depth = 1;
    while let Some(top) = stack.last() {
        let (limit, loops) = (top.end, top.loops);
        if pc == limit {
            let Some(done) = stack.pop() else { break };
            if matches!(done.finish, Finish::Root) {
                for &result in results { v.used(result, None)?; }
            }
            for index in v.scope.drain(done.mark..) {
                if let Some(flag) = v.flags.get_mut(index as usize) { *flag = 2; }
            }
            let cost = match done.finish {
                Finish::Root => { computed = done.cost; break; }
                Finish::Loop(cap) => (cap as u64).saturating_mul(done.cost.saturating_add(2)),
                Finish::Then { otherwise, end } => {
                    pc = otherwise + 1;
                    push(&mut stack, Block { end, mark: v.scope.len(), loops, cost: 0, finish: Finish::Else { then_cost: done.cost } })?;
                    continue;
                }
                Finish::Else { then_cost } => then_cost.max(done.cost).saturating_add(1),
            };
            if let Some(parent) = stack.last_mut() { parent.cost = parent.cost.saturating_add(cost); }
            continue;
        }
        v.at = pc;
        let at = pc;
        let word = take_code(code, &mut pc, 1, limit, k, at)?[0];
        let op = word & OP_MASK;
        let sub = word >> SUB_SHIFT;
        if word & !INSTRUCTION_MASK != 0 { return Err(LoadError::Instruction { entry: k, at, word }); }
        let operands = match op {
            OP_CONST | OP_GET | OP_UN | OP_SET => 2,
            OP_CONST_D | OP_BIN | OP_CMP_F | OP_CMP_I | OP_CMP_D | OP_WRAP | OP_CALL => 3,
            OP_LOAD | OP_STORE => 4 + u32::from(sub == REGION_BUF_LONG),
            OP_SEL | OP_FMA | OP_PORT => 4,
            OP_IF | OP_LOOP | OP_BREAK | OP_CONTINUE => 1,
            _ => return Err(LoadError::Instruction { entry: k, at, word }),
        };
        if !matches!(op, OP_UN | OP_BIN | OP_CMP_F | OP_CMP_I | OP_CMP_D | OP_LOAD | OP_STORE | OP_FMA) && sub != 0 { return Err(v.sub_error(op, sub)); }
        let a = take_code(code, &mut pc, operands, limit, k, at)?;
        let mut op_cost = 1;
        let buffer = if matches!(op, OP_LOAD | OP_STORE) && sub >= REGION_BUF_BASE {
            Some(if sub == REGION_BUF_LONG { a[4] } else { sub - REGION_BUF_BASE })
        } else { None };
        if (OP_CONST..=OP_FMA).contains(&op) {
            let dst = a[0];
            let ty = v.fresh(dst)?;
            let want = match op {
                OP_CONST => {
                    v.narrow(dst, ty)?;
                    if ty == TY_BOOL && a[1] > 1 { return Err(LoadError::BoolConstant { entry: k, at, bits: a[1] }); }
                    if ty == TY_I32 {
                        if let Some(c) = konst.get_mut(dst as usize) { *c = Some(a[1] as i32); }
                    }
                    ty
                }
                OP_CONST_D => TY_F64,
                OP_GET => v.ty(a[1], true)?,
                OP_UN => {
                    let (inp, out) = un_types(sub).ok_or_else(|| v.sub_error(op, sub))?;
                    v.used(a[1], Some(inp))?;
                    out
                }
                OP_BIN => {
                    let (inp, out) = bin_types(sub).ok_or_else(|| v.sub_error(op, sub))?;
                    v.used(a[1], Some(inp))?;
                    v.used(a[2], Some(inp))?;
                    out
                }
                OP_CMP_F | OP_CMP_I | OP_CMP_D => {
                    if sub > CMP_NE { return Err(v.sub_error(op, sub)); }
                    let inp = if op == OP_CMP_F { TY_F32 } else if op == OP_CMP_D { TY_F64 } else {
                        let t = v.used(a[1], None)?;
                        if t != TY_BOOL { v.type_is(a[1], t, TY_I32)?; }
                        t
                    };
                    v.used(a[1], Some(inp))?;
                    v.used(a[2], Some(inp))?;
                    TY_BOOL
                }
                OP_SEL => {
                    v.used(a[1], Some(TY_BOOL))?;
                    let t = v.used(a[2], None)?;
                    v.used(a[3], Some(t))?;
                    t
                }
                OP_WRAP => {
                    v.used(a[1], Some(TY_I32))?;
                    if a[2] == 0 { return Err(LoadError::WrapLength { entry: k, at, len: a[2] }); }
                    TY_I32
                }
                OP_LOAD => {
                    v.access(sub, buffer, a[1], a[2], a[3], false)?;
                    if buffer.is_some_and(|k| buffers[k as usize].elem != Elem::Word) { v.type_is(dst, ty, TY_I32)?; }
                    v.narrow(dst, ty)?;
                    ty
                }
                OP_FMA => {
                    if sub != FMA_MUL_ADD_I { return Err(v.sub_error(op, sub)); }
                    for index in &a[1..] { v.used(*index, Some(TY_I32))?; }
                    TY_I32
                }
                _ => return Err(LoadError::Instruction { entry: k, at, word }),
            };
            v.define(dst, want)?;
        } else {
            match op {
                OP_SET => { v.used(a[1], Some(v.ty(a[0], true)?))?; }
                OP_STORE => {
                    v.narrow(a[3], v.used(a[3], None)?)?;
                    v.access(sub, buffer, a[0], a[1], a[2], true)?;
                    if buffer.is_some_and(|k| buffers[k as usize].elem != Elem::Word) { v.used(a[3], Some(TY_I32))?; }
                }
                OP_IF => {
                    v.used(a[0], Some(TY_BOOL))?;
                    let then_end = block_end(code, pc, limit, k)?;
                    let else_end = block_end(code, then_end, limit, k)?;
                    push(&mut stack, Block { end: then_end, mark: v.scope.len(), loops, cost: 0, finish: Finish::Then { otherwise: then_end, end: else_end } })?;
                    pc += 1;
                }
                OP_LOOP => {
                    let body_end = block_end(code, pc, limit, k)?;
                    let nested = loops.checked_add(1).ok_or(LoadError::SizeOverflow { section: SEC_CODE, index: at })?;
                    push(&mut stack, Block { end: body_end, mark: v.scope.len(), loops: nested, cost: 0, finish: Finish::Loop(a[0]) })?;
                    pc += 1;
                }
                OP_BREAK | OP_CONTINUE => {
                    if a[0] >= loops { return Err(LoadError::LoopDepth { entry: k, at, depth: a[0], loops }); }
                }
                OP_CALL => {
                    let f = funcs.get(a[0] as usize).ok_or(LoadError::CallTarget { entry: k, at, func: a[0] })?;
                    if a[1] as usize != f.params.len() || a[2] as usize != f.results.len() {
                        return Err(LoadError::CallArity { entry: k, at, func: a[0], args: a[1], rets: a[2] });
                    }
                    let args = take_code(code, &mut pc, a[1], limit, k, at)?;
                    for (&arg, &param) in args.iter().zip(f.params) {
                        v.used(arg, program_ty(code, &f.program, param, false))?;
                    }
                    let rets = take_code(code, &mut pc, a[2], limit, k, at)?;
                    for (&ret, &result) in rets.iter().zip(f.results) {
                        v.define(ret, program_ty(code, &f.program, result, false).expect("verified signature"))?;
                    }
                    push(&mut calls, a[0])?;
                    op_cost = costs.map_or(0, |c| c[a[0] as usize]).saturating_add(4 + u64::from(a[1]));
                }
                OP_PORT => {
                    let port = a[0];
                    if lines.is_some() != (port >= u32::from(ports::KERNEL_BASE)) {
                        return Err(LoadError::DomainPort { entry: k, at, site: a[1], port, kernel: lines.is_some() });
                    }
                    let sig = if lines.is_some() {
                        ports::KERNEL_PORTS.get((port - u32::from(ports::KERNEL_BASE)) as usize)
                    } else { ports::PORTS.get(port as usize) }.ok_or(LoadError::Port { entry: k, at, port })?;
                    if a[2] as usize != sig.params.len() || a[3] as usize != sig.rets.len() { return Err(LoadError::PortArity { entry: k, at, port, args: a[2], rets: a[3] }); }
                    if a[1] >= v.sites { return Err(LoadError::Site { entry: k, at, site: a[1], sites: v.sites }); }
                    let args = take_code(code, &mut pc, a[2], limit, k, at)?;
                    for (index, ty) in args.iter().zip(sig.params) { v.used(*index, Some(*ty))?; }
                    if let Some(lines) = lines {
                        if matches!(port as u16, ports::COPY_RAW | ports::FILL_RAW | ports::PACK_COPY | ports::PACK_FILL)
                            && konst[args[3] as usize].is_none_or(|n| n < 0) {
                            return Err(LoadError::KernelBound { entry: k, at, site: a[1], port });
                        }
                        if port == u32::from(ports::IRQ_RAISE) && !konst[args[0] as usize].is_some_and(|n| {
                            (0..32).contains(&n) && lines.raise & (1u32 << n) != 0
                        }) { return Err(LoadError::KernelRaise { entry: k, at, site: a[1], raise: lines.raise }); }
                        if matches!(port as u16, ports::KERNEL_REPORT | ports::RADIO_STOP)
                            && !konst[args[0] as usize].is_some_and(|n| u8::try_from(n)
                                .is_ok_and(|code| watch::FAULT_KERNEL_REPORT.contains(&code))) {
                            return Err(if port == u32::from(ports::RADIO_STOP) {
                                LoadError::KernelStop { entry: k, at, site: a[1] }
                            } else {
                                LoadError::KernelReport { entry: k, at, site: a[1] }
                            });
                        }
                    }
                    if (port == ports::ACCEL as u32 || port == ports::GYRO as u32)
                        && !decls.iter().any(|d| d.form == ports::FORM_MOTION)
                    { return Err(LoadError::MotionUndeclared { entry: k, at, site: a[1], port }); }
                    if (u32::from(ports::TOUCH_X)..=u32::from(ports::TOUCH_DOWN)).contains(&port)
                        && !decls.iter().any(|d| d.form == ports::FORM_TOUCH)
                    { return Err(LoadError::TouchUndeclared { entry: k, at, site: a[1], port }); }
                    if (u32::from(ports::NET_LINK)..=u32::from(ports::NET_KIND)).contains(&port) {
                        let declared = decls.iter().filter(|d| d.form == ports::FORM_NET).count() as i32;
                        if declared == 0 { return Err(LoadError::NetUndeclared { entry: k, at, site: a[1], port }); }
                        if port >= u32::from(ports::NET_OPEN) {
                            if let Some(c) = konst[args[0] as usize].filter(|c| !(0..declared).contains(c)) {
                                return Err(LoadError::NetHandle { entry: k, at, site: a[1], port, c });
                            }
                        }
                    }
                    let audio_error = |reason| LoadError::AudioPort { entry: k, at, site: a[1], port, reason };
                    match port as u16 {
                        ports::PLAY..=ports::SPEAKER_ON if !decls.iter().any(|d| d.form == ports::FORM_SPEAKER) =>
                            return Err(audio_error("needs a speaker declaration")),
                        ports::RECORD..=ports::MIC_REWIND if !decls.iter().any(|d| d.form == ports::FORM_MIC) =>
                            return Err(audio_error("needs a mic declaration")),
                        ports::SPEAKER_ON if konst[args[0] as usize].is_some_and(|n| !(0..=1).contains(&n)) =>
                            return Err(audio_error("needs level 0 or 1")),
                        ports::MIC_REWIND if konst[args[0] as usize].is_some_and(|n| n < 0) =>
                            return Err(audio_error("needs a nonnegative sample count")),
                        _ => {}
                    }
                    let audio_buffer = matches!(port as u16, ports::PLAY | ports::RECORD);
                    let net_buffer = port == u32::from(ports::NET_SEND) || port == u32::from(ports::NET_RECV);
                    let eth_port = (ports::ETH_SEND as u32..=ports::ETH_MAC as u32).contains(&port);
                    if eth_port && !decls.iter().any(|d| d.form == ports::FORM_ETH) {
                        return Err(LoadError::EthUndeclared { entry: k, at, site: a[1], port });
                    }
                    if eth_port || net_buffer || audio_buffer || port == ports::COPY as u32 || port == ports::FILL as u32 || port == ports::PRESENT as u32 {
                        let argument = |param: usize, writes: bool, elem: Option<Elem>| -> Result<u32, LoadError> {
                            let bad = || LoadError::BufferArgument { entry: k, at, site: a[1], port, param: param as u32 };
                            let index = konst[args[param] as usize].filter(|n| *n >= 0).ok_or_else(bad)? as u32;
                            let b = buffers.get(index as usize).ok_or_else(bad)?;
                            if writes && !b.writable || elem.is_some_and(|elem| elem != b.elem) { return Err(bad()); }
                            Ok(index)
                        };
                        if audio_buffer {
                            let index = argument(0, port == u32::from(ports::RECORD), Some(Elem::I16))?;
                            let len = i64::from(buffers[index as usize].len);
                            let off = konst[args[1] as usize].unwrap_or(0);
                            let n = konst[args[2] as usize].unwrap_or(0);
                            if off < 0 || n < 0 || i64::from(off) + i64::from(n) > len {
                                return Err(audio_error("sample range is outside the buffer"));
                            }
                        } else if eth_port {
                            argument(0, port != ports::ETH_SEND as u32, Some(Elem::U8))?;
                        } else if net_buffer {
                            argument(1, port == u32::from(ports::NET_RECV), Some(Elem::U8))?;
                        } else if port == ports::PRESENT as u32 {
                            argument(1, false, Some(Elem::U16))?;
                        } else {
                            let dst = argument(0, true, None)?;
                            if port == ports::COPY as u32 {
                                let src = argument(2, false, None)?;
                                let (dst_elem, src_elem) = (buffers[dst as usize].elem, buffers[src as usize].elem);
                                if dst_elem != src_elem { return Err(LoadError::CopyKinds { entry: k, at, site: a[1], dst, src, dst_elem, src_elem }); }
                            }
                        }
                    }
                    if port == ports::PIN_WRITE as u32 || port == ports::PIN_READ as u32 || port == ports::PWM_DUTY as u32 || port == ports::ADC_READ as u32 || port == ports::SHIFT_OUT as u32 {
                        let pins = if port == ports::SHIFT_OUT as u32 { 2 } else { 1 };
                        for (param, arg) in args[..pins].iter().enumerate() {
                            let Some(pin) = konst[*arg as usize] else { continue; };
                            let ok = decls.iter().any(|d| d.handle == pin && if port == ports::PWM_DUTY as u32 {
                                d.form == ports::FORM_PWM
                            } else if port == ports::ADC_READ as u32 {
                                d.form == ports::FORM_ANALOG
                            } else {
                                d.form == ports::FORM_OUTPUT || (d.form == ports::FORM_INPUT && port == ports::PIN_READ as u32)
                            });
                            if !ok { return Err(LoadError::PinRole { entry: k, at, site: a[1], port, param: param as u32, pin }); }
                        }
                    }
                    if port == ports::SHIFT_OUT as u32 {
                        if let Some(bits) = konst[args[3] as usize] {
                            if !(1..=32).contains(&bits) { return Err(LoadError::ShiftBits { entry: k, at, site: a[1], bits }); }
                        }
                    }
                    if port == ports::EEPROM_READ as u32 || port == ports::EEPROM_WRITE as u32 {
                        if profile.eeprom == 0 { return Err(LoadError::EepromUnsupported { entry: k, at, site: a[1], port }); }
                        if let Some(addr) = konst[args[0] as usize] {
                            if addr < 0 || addr as u32 >= profile.eeprom {
                                return Err(LoadError::EepromAddr { entry: k, at, site: a[1], port, addr, size: profile.eeprom });
                            }
                        }
                        writes_eeprom |= port == ports::EEPROM_WRITE as u32;
                        op_cost = eeprom_ops;
                    }
                    let rets = take_code(code, &mut pc, a[3], limit, k, at)?;
                    for (index, ty) in rets.iter().zip(sig.rets) { v.define(*index, *ty)?; }
                }
                _ => return Err(LoadError::Instruction { entry: k, at, word }),
            }
        }
        if matches!(op, OP_IF | OP_LOOP) {
            let depth = u32::try_from(stack.len()).map_err(|_| LoadError::SizeOverflow { section: SEC_CODE, index: at })?;
            e.depth = e.depth.max(depth);
        } else if let Some(top) = stack.last_mut() {
            top.cost = top.cost.saturating_add(op_cost);
        }
    }
    if check_cost && computed > e.cost { return Err(LoadError::CostUnderstated { entry: k, declared: e.cost, computed }); }
    if check_cost && e.cost > profile.max_entry_ops { return Err(LoadError::CostLimit { entry: k, declared: e.cost, limit: profile.max_entry_ops }); }
    Ok(Verified { writes_eeprom, cost: computed, calls })
}

fn program_ty(code: &[u32], e: &Entry, index: u32, variable: bool) -> Option<u8> {
    let start = e.code as usize + PROGRAM_WORDS + if variable { e.n_vals.div_ceil(4) as usize } else { 0 };
    Types { words: code.get(start..e.body as usize)?, count: if variable { e.n_vars } else { e.n_vals } }.get(index)
}

// Iterative DFS: even a hostile chain cannot exhaust the host's call stack.
fn call_order(graph: &[Vec<u32>]) -> Result<Vec<u32>, LoadError> {
    let mut colors = Vec::new();
    reserve(&mut colors, graph.len())?;
    colors.resize(graph.len(), 0u8);
    let mut order = Vec::new();
    let mut stack = Vec::new();
    for root in 0..graph.len() {
        if colors[root] != 0 { continue; }
        colors[root] = 1;
        push(&mut stack, (root, 0usize))?;
        while let Some((node, next)) = stack.last_mut() {
            if let Some(&child) = graph[*node].get(*next) {
                *next += 1;
                match colors[child as usize] {
                    1 => return Err(LoadError::CallCycle { func: child }),
                    0 => { colors[child as usize] = 1; push(&mut stack, (child as usize, 0))?; }
                    _ => {}
                }
            } else {
                let node = *node;
                colors[node] = 2;
                push(&mut order, node as u32)?;
                stack.pop();
            }
        }
    }
    Ok(order)
}

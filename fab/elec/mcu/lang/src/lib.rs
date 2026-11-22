//! microsplash: firmware written in Splash. The Splash compute subset (typed,
//! alloc-free, bounded) with a firmware prelude, compiled on the host into a
//! module of flat AIR that the microsplash runtime
//! (`fab-elec-mcu-rt`) verifies and runs: in the circuit simulator,
//! on a chip's interpreter or JIT, or ahead of time into a chip's image.
//!
//! ```text
//! let led  = output(PA0)
//! var on   = false
//! let tick = every(ms(500), fn() {
//!     on = !on
//!     write(led, if on { high } else { low })
//! })
//! ```
//!
//! - Declarations (`output`, `input`, `analog`, `every`, `after`, `pwm`, `serial`, `eth`, `net`, `motion`, `touch`, `speaker`, `mic`) are top-level
//!   constants: every pin, its role, timer and serial link are known before
//!   anything runs.
//! - Handlers (`fn setup()` and each `every` / `after` / `serial` / `eth` / `net` / `motion` / `touch` / `speaker` / `mic` function) run to
//!   completion, one at a time, over the program's `var`s.
//! - Every port call names its site: the user's source line that made it.
use fab_elec_mcu_rt as rt;
use makepad_script_compute::ir::{Elem, Op, PortBuf, PortSig, Stmt, Ty};
use makepad_script_compute::lower::event::{lower_events, BufForm, DeclForm, DeclHandle, EventLowered, EventSpec};
use makepad_script_compute::parse::{Expr, ExprKind, Item, Span};
use makepad_script_compute::{parse, ShaderError};
use rt::module::LoadError;

mod assets;
pub mod convert;
mod glyphs;
mod debug;
mod diagnostic;
mod kernel;
pub mod encode;

pub use fab_elec_mcu_emu::Owner;
pub use debug::{debug_table, ChipMap};
pub use diagnostic::Diagnostic;
pub use rt::Profile;
pub use kernel::{compile_kernel, KERNEL_FORMS, KERNEL_PORTS};
pub use glyphs::{glyph_atlas, glyph_atlas_size, render_glyph_atlas};

const PRELUDE: &str = include_str!("prelude.splash");

/// The ports, in the runtime's order ([`rt::ports::PORTS`]).
pub const PORTS: &[PortSig] = &[
    PortSig { bufs: &[], name: "pin_write", params: &[Ty::I32, Ty::I32], rets: &[] },
    PortSig { bufs: &[], name: "pin_read", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "clock_us", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "clock_ms", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "timer_stop", params: &[Ty::I32], rets: &[] },
    PortSig { bufs: &[], name: "timer_start", params: &[Ty::I32], rets: &[] },
    PortSig { bufs: &[], name: "timer_period", params: &[Ty::I32, Ty::I32], rets: &[] },
    PortSig { bufs: &[], name: "loop_cap", params: &[], rets: &[] },
    PortSig { bufs: &[], name: "pwm_duty", params: &[Ty::I32, Ty::I32], rets: &[] },
    PortSig { bufs: &[], name: "serial_byte", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[PortBuf { param: 1, elem: Some(Elem::U16), writes: false }], name: "present", params: &[Ty::I32, Ty::I32, Ty::I32, Ty::I32, Ty::I32, Ty::I32, Ty::I32, Ty::I32], rets: &[] },
    PortSig { bufs: &[], name: "fill_rect", params: &[Ty::I32, Ty::I32, Ty::I32, Ty::I32, Ty::I32, Ty::I32], rets: &[] },
    PortSig { bufs: &[PortBuf { param: 0, elem: None, writes: true }, PortBuf { param: 2, elem: None, writes: false }], name: "copy", params: &[Ty::I32; 5], rets: &[] },
    PortSig { bufs: &[PortBuf { param: 0, elem: None, writes: true }], name: "fill", params: &[Ty::I32; 4], rets: &[] },
    PortSig { bufs: &[], name: "adc_read", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[PortBuf { param: 0, elem: Some(Elem::U8), writes: false }], name: "eth_send", params: &[Ty::I32; 3], rets: &[Ty::I32] },
    PortSig { bufs: &[PortBuf { param: 0, elem: Some(Elem::U8), writes: true }], name: "eth_recv", params: &[Ty::I32; 2], rets: &[Ty::I32] },
    PortSig { bufs: &[PortBuf { param: 0, elem: Some(Elem::U8), writes: true }], name: "eth_mac", params: &[Ty::I32; 2], rets: &[] },
    PortSig { bufs: &[], name: "shift_out", params: &[Ty::I32; 4], rets: &[] },
    PortSig { bufs: &[], name: "accel", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "gyro", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "touch_x", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "touch_y", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "touch_down", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "eeprom_read", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "eeprom_write", params: &[Ty::I32; 2], rets: &[] },
    PortSig { bufs: &[], name: "probe", params: &[Ty::I32; 2], rets: &[] },
    PortSig { bufs: &[], name: "net_link", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_link_error", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_link_detail", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_open", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_close", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_state", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_epoch", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_error", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_detail", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[PortBuf { param: 1, elem: Some(Elem::U8), writes: false }], name: "net_send", params: &[Ty::I32; 5], rets: &[Ty::I32] },
    PortSig { bufs: &[PortBuf { param: 1, elem: Some(Elem::U8), writes: true }], name: "net_recv", params: &[Ty::I32; 4], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_left", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "net_kind", params: &[Ty::I32], rets: &[Ty::I32] },
    PortSig { bufs: &[PortBuf { param: 0, elem: Some(Elem::I16), writes: false }], name: "play", params: &[Ty::I32; 3], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "speaker_free", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "speaker_queued", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "speaker_on", params: &[Ty::I32], rets: &[] },
    PortSig { bufs: &[PortBuf { param: 0, elem: Some(Elem::I16), writes: true }], name: "record", params: &[Ty::I32; 3], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "mic_ready", params: &[], rets: &[Ty::I32] },
    PortSig { bufs: &[], name: "mic_rewind", params: &[Ty::I32], rets: &[Ty::I32] },
];

/// The declaration forms, in the runtime's order (`rt::ports::FORM_*`).
pub const FORMS: &[DeclForm] = &[
    DeclForm { buffer: None, name: "output", handle: DeclHandle::Arg(0), class: 0 },
    DeclForm { buffer: None, name: "input", handle: DeclHandle::Arg(0), class: 0 },
    DeclForm { buffer: None, name: "every", handle: DeclHandle::Ordinal, class: 1 },
    DeclForm { buffer: None, name: "after", handle: DeclHandle::Ordinal, class: 1 },
    DeclForm { buffer: None, name: "pwm", handle: DeclHandle::Arg(0), class: 0 },
    DeclForm { buffer: None, name: "serial", handle: DeclHandle::Ordinal, class: 2 },
    DeclForm { buffer: None, name: "display", handle: DeclHandle::Ordinal, class: 3 },
    DeclForm { buffer: Some(BufForm { elem: Elem::U16, len: &[1, 2], writable: false }), name: "image", handle: DeclHandle::Ordinal, class: 5 },
    DeclForm { buffer: Some(BufForm { elem: Elem::U8, len: &[0], writable: true }), name: "bytes", handle: DeclHandle::Ordinal, class: 5 },
    DeclForm { buffer: Some(BufForm { elem: Elem::I16, len: &[0], writable: true }), name: "samples", handle: DeclHandle::Ordinal, class: 5 },
    DeclForm { buffer: Some(BufForm { elem: Elem::Word, len: &[0], writable: true }), name: "words", handle: DeclHandle::Ordinal, class: 5 },
    DeclForm { buffer: Some(BufForm { elem: Elem::U16, len: &[0, 1], writable: true }), name: "surface", handle: DeclHandle::Ordinal, class: 5 },
    DeclForm { buffer: None, name: "analog", handle: DeclHandle::Arg(0), class: 0 },
    DeclForm { buffer: None, name: "eth", handle: DeclHandle::Ordinal, class: 6 },
    DeclForm { buffer: None, name: "motion", handle: DeclHandle::Ordinal, class: 7 },
    DeclForm { buffer: None, name: "touch", handle: DeclHandle::Ordinal, class: 8 },
    DeclForm { buffer: None, name: "net", handle: DeclHandle::Ordinal, class: 9 },
    DeclForm { buffer: None, name: "speaker", handle: DeclHandle::Ordinal, class: 10 },
    DeclForm { buffer: None, name: "mic", handle: DeclHandle::Ordinal, class: 11 },
];

pub use rt::SETUP;

/// A port site as the user sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site {
    pub start: u32,
    pub end: u32,
    /// 1-based.
    pub line: u32,
    pub col: u32,
}

/// A source statement's index in `Compiled::stmts`.
pub type StmtId = u32;

/// A user statement, recorded once per inlined copy. The table is built once per compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StmtInfo {
    /// The statement's byte span and 1-based line/column in the firmware source.
    pub site: Site,
    /// The entry or innermost shared function containing this copy.
    pub owner: Owner,
}

/// A source condition's index in `Compiled::conds`.
pub type CondId = u32;

/// A source condition the program branches on (an `if`'s, a `while`'s, or the left side of a `&&`
/// or `||` whose right side runs only on its value), recorded once per inlined copy, as a
/// statement is. The table is built once per compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CondInfo {
    /// The condition's byte span and 1-based line/column in the firmware source.
    pub site: Site,
    /// The entry or innermost shared function containing this copy.
    pub owner: Owner,
    /// The module's `if` at the condition tests the condition itself; false: its negation (a
    /// `while`'s exit).
    pub tests: bool,
}

#[derive(Debug)]
pub enum CompileError {
    Consts(Vec<ConstError>),
    Source(Vec<ShaderError>),
}

/// A bench override error, attributed to its named firmware parameter by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstError {
    pub name: String,
    pub message: String,
    /// Byte span of the firmware's top-level `let`, when present.
    pub at: Option<(u32, u32)>,
}

/// A compiled firmware program.
pub struct Compiled {
    /// The module the runtime loads (verified against the profile).
    pub module: Vec<u32>,
    /// The names of the module's images, in its order (a chip that keeps them in a file bundle
    /// files them under these).
    pub images: Vec<String>,
    /// The lowered program: the entries' AIR (the reference `ir::run_ports`
    /// runs them), declarations and state layout.
    pub lowered: EventLowered,
    /// Port sites by index: what a port call's `site` names.
    pub sites: Vec<Site>,
    /// Source statements indexed by `StmtId`, built once per compile and shared by views.
    pub stmts: std::sync::Arc<[StmtInfo]>,
    /// (Module code index, statement ID), by code index; statements that begin at the same
    /// index (a caller and the helper inlined into it) all appear, outermost first.
    pub marks: Vec<(u32, u32)>,
    /// Source conditions indexed by `CondId`, built once per compile and shared by views.
    pub conds: std::sync::Arc<[CondInfo]>,
    /// (Module code index, condition ID), by code index: the condition's `if` is the instruction
    /// at the index, unless optimisation removed it (then the index holds whatever followed, and
    /// an emitter finds no `if` there). A condition that takes no branch has none.
    pub cond_marks: Vec<(u32, u32)>,
    /// Resolved PWM outputs, in declaration order.
    pub pwm: Vec<rt::pwm::PwmOut>,
    /// Resolved receive links, in declaration order.
    pub serial: Vec<rt::serial::SerialOut>,
    /// Actual-frequency notes: PWM outputs, then serial links, each in declaration order,
    /// followed by EEPROM wait accounting when the module writes.
    pub notes: Vec<String>,
}

fn site(src: &str, start: usize, end: usize) -> Site {
    let e = ShaderError::new(start, end, String::new());
    let (line, col) = e.line_col(src);
    Site { start: start as u32, end: end as u32, line: line as u32, col: col as u32 }
}

fn hz_text(mhz: u64) -> String {
    let tenths = mhz / 100 + u64::from(mhz % 100 >= 50);
    if tenths % 10 == 0 { (tenths / 10).to_string() } else { format!("{}.{}", tenths / 10, tenths % 10) }
}

fn ordinal_text(n: u64) -> String {
    match n {
        1 => "first".to_string(),
        2 => "second".to_string(),
        3 => "third".to_string(),
        _ => {
            let suffix = if (11..=13).contains(&(n % 100)) { "th" } else {
                match n % 10 { 1 => "st", 2 => "nd", 3 => "rd", _ => "th" }
            };
            format!("{n}{suffix}")
        }
    }
}

fn pin_name(pin: u32, pins: &[(&str, u32)]) -> String {
    pins.iter().find(|(_, p)| *p == pin).map_or_else(|| pin.to_string(), |(n, _)| (*n).to_string())
}

fn pwm_pin(profile: &Profile, pin: u32) -> Option<(&rt::pwm::PwmPin, &rt::pwm::PwmTimer)> {
    let output = profile.pwm.pins.iter().find(|p| p.pin == pin)?;
    Some((output, profile.pwm.timers.get(output.timer as usize)?))
}

/// A refusal of the runtime's verifier, at the source it names.
fn load_error(e: &LoadError, l: &EventLowered, profile: &Profile, pins: &[(&str, u32)]) -> ShaderError {
    let named = |pin: i32| match pins.iter().find(|(_, p)| *p as i64 == pin as i64) {
        Some((n, _)) => format!("pin {} ({})", pin, n),
        None => format!("pin {}", pin),
    };
    let at = |span: Option<Span>, m: String| match span {
        Some(s) => ShaderError::new(s.start, s.end, m),
        None => ShaderError::new(0, 1, m),
    };
    let pwm_names = |non_clock: bool| {
        let mut available: Vec<_> = profile.pwm.pins.iter().filter(|p| !non_clock || profile.pwm.timers.get(p.timer as usize).is_some_and(|t| !t.clock)).collect();
        available.sort_by_key(|p| p.pin);
        available.iter().map(|p| pin_name(p.pin, pins)).collect::<Vec<_>>().join(", ")
    };
    match *e {
        LoadError::DomainPort { site, .. } | LoadError::KernelBound { site, .. }
        | LoadError::KernelRaise { site, .. } | LoadError::KernelReport { site, .. }
        | LoadError::KernelStop { site, .. } => at(l.sites.get(site as usize).copied(), e.to_string()),
        LoadError::DomainForm { decl, .. } | LoadError::ExportArgs { decl }
        | LoadError::ExportSlot { decl, .. } | LoadError::DuplicateExport { decl } =>
            at(l.decls.get(decl as usize).map(|d| d.span), e.to_string()),
        LoadError::KernelEntry { entry } => at(l.entries.get(entry as usize).map(|e| e.span), e.to_string()),
        LoadError::PinRole { site, port, param, pin, .. } => {
            let is_pwm = l.decls.iter().any(|d| d.form as u32 == rt::ports::FORM_PWM && d.handle == pin);
            let is_analog = l.decls.iter().any(|d| d.form as u32 == rt::ports::FORM_ANALOG && d.handle == pin);
            let m = if port == rt::ports::SHIFT_OUT as u32 {
                format!("`shift_out` drives {} as its {} pin, which is not declared as an output: declare it with `let name = output(pin)`", named(pin), if param == 0 { "data" } else { "clock" })
            } else if port == rt::ports::ADC_READ as u32 {
                format!("`analog_read` reads {}, which is not declared as an analog input: declare it with `let name = analog(pin)`", named(pin))
            } else if port == rt::ports::PIN_READ as u32 && is_analog {
                format!("`read` reads {}, an analog input: read it with `analog_read(..)`", named(pin))
            } else if port == rt::ports::PIN_WRITE as u32 && is_analog {
                format!("`write` drives {}, an analog input: declare it with `output(..)` to drive it", named(pin))
            } else if port == rt::ports::PWM_DUTY as u32 {
                format!("`duty` drives {}, which is not declared as PWM: declare it with `let name = pwm(pin, hz(..))`", named(pin))
            } else if port == rt::ports::PIN_WRITE as u32 && is_pwm {
                format!("`write` drives {}, a PWM output: set its level with `duty(..)`", named(pin))
            } else if port == rt::ports::PIN_READ as u32 && is_pwm {
                format!("`read` reads {}, a PWM output: only `input(..)` and `output(..)` pins can be read", named(pin))
            } else if port == rt::ports::PIN_WRITE as u32 {
                format!("`write` drives {}, which is not declared as an output: declare it with `let name = output(..)`", named(pin))
            } else {
                format!("`read` reads {}, which is not declared: declare it with `input(..)` or `output(..)`", named(pin))
            };
            at(l.sites.get(site as usize).copied(), m)
        }
        LoadError::ShiftBits { site, .. } | LoadError::EepromUnsupported { site, .. }
        | LoadError::EepromAddr { site, .. } => at(l.sites.get(site as usize).copied(), e.to_string()),
        LoadError::AnalogPin { decl, pin } => {
            let available = profile.analog.pins.iter().map(|p| pin_name(p.pin, pins)).collect::<Vec<_>>().join(", ");
            let m = if available.is_empty() {
                "this chip has no analog inputs".to_string()
            } else {
                format!("{} has no analog input on this chip ({} have)", named(pin), available)
            };
            at(l.decls.get(decl as usize).map(|d| d.span), m)
        }
        LoadError::AnalogOnly { decl, pin } => at(l.decls.get(decl as usize).map(|d| d.span),
            format!("{} is an analog-only pin: declare it with `analog(..)`", named(pin))),
        LoadError::PwmPin { decl, pin } => {
            let available = pwm_names(false);
            let m = if available.is_empty() {
                format!("pin {} has no PWM output on this chip", pin_name(pin as u32, pins))
            } else {
                format!("pin {} has no PWM output on this chip ({} have)", pin_name(pin as u32, pins), available)
            };
            at(l.decls.get(decl as usize).map(|d| d.span), m)
        }
        LoadError::PwmClock { decl, pin, .. } => {
            let m = pwm_pin(profile, pin as u32).and_then(|(output, timer)| {
                let prescale = *timer.prescalers.first()?;
                let setting = rt::pwm::PwmSetting { timer: output.timer, mode: timer.mode, prescale, top: timer.top };
                Some(format!("pin {} ({}) is on {}, the device clock: it runs at {} Hz only; {} take other frequencies", pin_name(pin as u32, pins), output.output, timer.name, hz_text(setting.real_mhz(profile.clock_hz)), pwm_names(true)))
            }).unwrap_or_else(|| e.to_string());
            at(l.decls.get(decl as usize).map(|d| d.span), m)
        }
        LoadError::PwmShared { decl, pin, asked_mhz, other_pin, other_mhz, .. } => {
            let m = pwm_pin(profile, pin as u32).zip(pwm_pin(profile, other_pin)).map(|((output, timer), (other, _))| {
                format!("pin {} ({}) asks {} Hz but pin {} ({}), on the same {}, runs at {} Hz", pin_name(pin as u32, pins), output.output, hz_text(asked_mhz as u64), pin_name(other_pin, pins), other.output, timer.name, hz_text(other_mhz))
            }).unwrap_or_else(|| e.to_string());
            at(l.decls.get(decl as usize).map(|d| d.span), m)
        }
        LoadError::PwmFrequency { decl, pin, asked_mhz } => {
            let m = if asked_mhz <= 0 {
                "a PWM frequency must be above 0 Hz".to_string()
            } else {
                pwm_pin(profile, pin as u32).map(|(output, timer)| {
                    let setting = |prescale, top| rt::pwm::PwmSetting { timer: output.timer, mode: timer.mode, prescale, top };
                    let frequencies = timer.prescalers.iter().map(|n| hz_text(setting(*n, timer.top).real_mhz(profile.clock_hz))).collect::<Vec<_>>().join(", ");
                    let mut m = format!("{} has no setting near {} Hz; it has {}", timer.name, hz_text(asked_mhz as u64), frequencies);
                    if timer.free_top > 0 {
                        if let Some((&first, &last)) = timer.prescalers.first().zip(timer.prescalers.last()) {
                            m.push_str(&format!(", and through {} {} to {} Hz", timer.free_top_reg, hz_text(setting(last, timer.free_top).real_mhz(profile.clock_hz)), hz_text(setting(first, 3).real_mhz(profile.clock_hz))));
                        }
                    }
                    m
                }).unwrap_or_else(|| e.to_string())
            };
            at(l.decls.get(decl as usize).map(|d| d.span), m)
        }
        LoadError::SerialLinks { decl, links } => {
            let d = l.decls.get(decl as usize);
            let name = d.map_or("", |d| d.name.as_str());
            let ordinal = d.map_or(links as u64 + 1, |d| d.handle as u64 + 1);
            let m = format!("this chip has {links} serial link{}: `{name}` would be the {}", if links == 1 { "" } else { "s" }, ordinal_text(ordinal));
            at(d.map(|d| d.span), m)
        }
        LoadError::SerialBaud { decl, asked, nearest_milli } => {
            let m = if asked <= 0 {
                "a baud rate must be above 0".to_string()
            } else {
                // Resolve without the tolerance filter to identify the nearest
                // setting's oversampling and its own receiver tolerance.
                let table = rt::serial::SerialTable { tolerance_permille: [u32::MAX; 2], ..*profile.serial };
                let nearest = rt::serial::resolve(&table, profile.clock_hz, 0, asked as u32).ok();
                let tolerance = profile.serial.tolerance_permille[usize::from(nearest.is_some_and(|s| s.u2x))];
                let requested = asked as u64 * 1000;
                let off = (nearest_milli.abs_diff(requested) as u128 * 1000 + requested as u128 / 2) / requested as u128;
                format!("the chip's UART cannot run {asked} baud closely enough: the nearest is {} baud ({}.{} % off; a receiver allows {}.{} %)", hz_text(nearest_milli), off / 10, off % 10, tolerance / 10, tolerance % 10)
            };
            at(l.decls.get(decl as usize).map(|d| d.span), m)
        }
        LoadError::Ram { fifo, .. } if fifo > 0 => at(l.decls.iter().rfind(|d| d.form as u32 == rt::ports::FORM_SERIAL).map(|d| d.span), format!("this does not fit the chip: {}", e)),
        LoadError::EthUndeclared { site, .. } | LoadError::MotionUndeclared { site, .. }
        | LoadError::TouchUndeclared { site, .. } | LoadError::NetUndeclared { site, .. }
        | LoadError::NetHandle { site, .. } | LoadError::AudioPort { site, .. } => at(l.sites.get(site as usize).copied(), e.to_string()),
        LoadError::NetDecl { decl, .. } | LoadError::AudioDecl { decl, .. } => at(l.decls.get(decl as usize).map(|d| d.span), e.to_string()),
        LoadError::TouchUnsupported { decl } | LoadError::DuplicateTouch { decl } | LoadError::TouchPeriod { decl, .. } =>
            at(l.decls.get(decl as usize).map(|d| d.span), e.to_string()),
        LoadError::MotionUnsupported { decl } | LoadError::DuplicateMotion { decl } =>
            at(l.decls.get(decl as usize).map(|d| d.span), e.to_string()),
        LoadError::DuplicateEth { decl } | LoadError::EthCount { decl, .. } =>
            at(l.decls.get(decl as usize).map(|d| d.span), e.to_string()),
        LoadError::BufferArgument { site, .. } | LoadError::CopyKinds { site, .. } =>
            at(l.sites.get(site as usize).copied(), e.to_string()),
        LoadError::BufferDecl { decl, .. } | LoadError::BufferElem { decl, .. }
        | LoadError::BufferLength { decl, .. } | LoadError::BufferOrder { decl, .. }
        | LoadError::BufferMissing { decl } | LoadError::BufferWritable { decl, .. } => at(l.decls.get(decl as usize).map(|d| d.span), e.to_string()),
        LoadError::BufferBytes { .. } | LoadError::BufferFaultRam { .. } =>
            at(l.buffers.last().and_then(|b| l.decls.get(b.decl as usize)).map(|d| d.span), e.to_string()),
        LoadError::PinRange { decl, .. } | LoadError::DuplicatePin { decl, .. }
        | LoadError::DisplayUnsupported { decl } | LoadError::DuplicateDisplay { decl }
        | LoadError::ImageIndex { decl, .. } | LoadError::ImageDimensions { decl, .. } => at(l.decls.get(decl as usize).map(|d| d.span), format!("this does not fit the chip: {}", e)),
        LoadError::CostLimit { entry, .. } => at(l.entries.get(entry as usize).map(|x| x.span), format!("this does not fit the chip: {}", e)),
        _ => at(None, format!("the program does not fit this chip: {}", e)),
    }
}

/// Compiles `source` for a chip whose package pins are `pins` (name and
/// pin number, both made constants the program can name: `PA0`, `LED0`).
/// `assets` holds the firmware folder's files by name: pictures become images, the others are
/// named in a note.
pub fn compile(source: &str, profile: &Profile, pins: &[(&str, u32)], assets: &[(&str, &[u8])]) -> Result<Compiled, Vec<ShaderError>> {
    match compile_with(source, profile, pins, &[], assets) {
        Ok(compiled) => Ok(compiled),
        Err(CompileError::Source(errors)) => Err(errors),
        Err(CompileError::Consts(_)) => unreachable!("no bench overrides"),
    }
}

fn override_consts(items: &mut [Item], consts: &[(&str, f64)]) -> Result<(), CompileError> {
    let mut seen = std::collections::HashSet::new();
    let mut errors = Vec::new();
    let mut replacements = Vec::new();
    for &(name, number) in consts {
        let found = items.iter().enumerate().find_map(|(k, item)| match item {
            Item::Let { name: own, value, span, .. } if own == name => Some((k, value, span)),
            _ => None,
        });
        let at = found.map(|(_, _, span)| (span.start as u32, span.end as u32));
        let mut refuse = |message| errors.push(ConstError { name: name.to_owned(), message, at });
        if !seen.insert(name) {
            refuse(format!("the bench sets `{name}` more than once"));
            continue;
        }
        let Some((k, value, _)) = found else {
            refuse(format!("the bench sets `{name}`, but the firmware has no top-level `let {name}`"));
            continue;
        };
        // The compute parser's flag is is_integer (true for a fraction/exponent-free literal).
        let integer = match &value.kind {
            ExprKind::Num(_, integer) => Some(*integer),
            ExprKind::Neg(inner) => match &inner.kind {
                ExprKind::Num(_, integer) => Some(*integer),
                _ => None,
            },
            _ => None,
        };
        let Some(integer) = integer else {
            refuse(format!("`{name}` is set by the bench, so it must be a literal"));
            continue;
        };
        if !number.is_finite() {
            refuse(format!("the bench value for `{name}` must be finite"));
        } else if integer && (number.fract() != 0.0 || number < i32::MIN as f64 || number > i32::MAX as f64) {
            refuse(format!("the bench value for `{name}` must be an integer in the i32 range"));
        } else {
            replacements.push((k, number, integer));
        }
    }
    if !errors.is_empty() { return Err(CompileError::Consts(errors)); }
    for (k, number, integer) in replacements {
        let Item::Let { value, .. } = &mut items[k] else { unreachable!("validated let") };
        value.kind = if number.is_sign_negative() {
            ExprKind::Neg(Box::new(Expr { kind: ExprKind::Num(-number, integer), span: value.span }))
        } else {
            ExprKind::Num(number, integer)
        };
    }
    Ok(())
}

// Lowering proves each buffer argument is a constant handle. The pairwise
// kind constraint of copy belongs to microsplash, outside PortSig.
fn check_copies(lowered: &EventLowered) -> Result<(), ShaderError> {
    if lowered.buffers.is_empty() { return Ok(()); }
    for entry in &lowered.entries {
        let mut constants = vec![None; entry.program.vals.len()];
        let mut blocks = vec![entry.program.body.as_slice()];
        while let Some(block) = blocks.pop() {
            for statement in block {
                match statement {
                    Stmt::Def(v, Op::ConstI(n)) => constants[v.0 as usize] = Some(*n),
                    Stmt::If(_, yes, no) => { blocks.push(no); blocks.push(yes); }
                    Stmt::Loop { body, .. } => blocks.push(body),
                    _ => {}
                }
            }
        }
        let mut blocks = vec![entry.program.body.as_slice()];
        while let Some(block) = blocks.pop() {
            for statement in block {
                match statement {
                    Stmt::Port { port, site, args, .. } if *port == rt::ports::COPY => {
                        let buffer = |param: usize| constants[args[param].0 as usize]
                            .and_then(|k| usize::try_from(k).ok()).and_then(|k| lowered.buffers.get(k));
                        if let Some((dst, src)) = buffer(0).zip(buffer(2)) {
                            if dst.elem != src.elem {
                                let span = lowered.sites[*site as usize];
                                return Err(ShaderError::new(span.start, span.end, format!(
                                    "copy needs buffers of the same kind: `{}` is {:?}, `{}` is {:?}",
                                    lowered.decls[dst.decl as usize].name, dst.elem,
                                    lowered.decls[src.decl as usize].name, src.elem,
                                )));
                            }
                        }
                    }
                    Stmt::If(_, yes, no) => { blocks.push(no); blocks.push(yes); }
                    Stmt::Loop { body, .. } => blocks.push(body),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

// The flat event AIR accepted by encode has no host/function calls. Mirror
// the verifier's capped loops and worst branch, replacing only EEPROM port
// costs. Recompute the whole branch cost: waits can change which arm wins.
fn eeprom_costs(lowered: &mut EventLowered, profile: &Profile) -> bool {
    fn writes(block: &[Stmt]) -> bool {
        block.iter().any(|stmt| match stmt {
            Stmt::Port { port, .. } => *port == rt::ports::EEPROM_WRITE,
            Stmt::If(_, yes, no) => writes(yes) || writes(no),
            Stmt::Loop { body, .. } => writes(body),
            _ => false,
        })
    }
    fn cost(block: &[Stmt], funcs: &[makepad_script_compute::ir::Program], eeprom_ops: u64) -> u64 {
        block.iter().fold(0u64, |sum, stmt| sum.saturating_add(match stmt {
            Stmt::Mark(_) => 0,
            Stmt::If(_, yes, no) => cost(yes, funcs, eeprom_ops).max(cost(no, funcs, eeprom_ops)).saturating_add(1),
            Stmt::Loop { cap, body } => u64::from(*cap).saturating_mul(cost(body, funcs, eeprom_ops).saturating_add(2)),
            Stmt::Call { f, args, .. } => cost(&funcs[*f as usize].body, funcs, eeprom_ops).saturating_add(4 + args.len() as u64),
            Stmt::Port { port: rt::ports::EEPROM_READ | rt::ports::EEPROM_WRITE, .. } => eeprom_ops,
            _ => 1,
        }))
    }
    let ops = profile.eeprom_access_ops();
    let waits = ops > 1 && lowered.entries.iter().any(|e| writes(&e.program.body) || e.program.funcs.iter().any(|f| writes(&f.body)));
    if waits {
        for entry in &mut lowered.entries { entry.cost = cost(&entry.program.body, &entry.program.funcs, ops); }
    }
    waits
}

/// Compiles with bench overrides of the user's top-level numeric literals.
/// All overrides are validated together before lowering or module compilation.
pub fn compile_with(source: &str, profile: &Profile, pins: &[(&str, u32)], consts: &[(&str, f64)], assets: &[(&str, &[u8])]) -> Result<Compiled, CompileError> {
    let source_error = |e| CompileError::Source(vec![e]);
    let toks = parse::lex(source).map_err(source_error)?;
    let mut items = parse::Parser::new(&toks).items().map_err(source_error)?;
    override_consts(&mut items, consts)?;
    let mut others = Vec::new();
    let mut images = assets::decode(assets, &mut others).map_err(source_error)?;
    glyphs::declare(&mut items, &mut images).map_err(source_error)?;
    let user_len = source.len();
    let mut prelude = makepad_script_compute::parse_prelude(PRELUDE, user_len);
    // The chip's pin names are constants the program can name: items, never text. They sit after
    // the prelude, outside the program's source.
    let at = Span { start: user_len + 1 + PRELUDE.len(), end: user_len + 1 + PRELUDE.len() };
    prelude.extend(pins.iter().map(|&(name, pin)| Item::Let {
        name: name.to_owned(), ann: None, value: Expr { kind: ExprKind::Num(f64::from(pin), true), span: at }, span: at,
    }));
    if let Some(display) = profile.display {
        prelude.extend([("display_w", display.width), ("display_h", display.height)].map(|(name, size)| Item::Let {
            name: name.to_owned(), ann: None, value: Expr { kind: ExprKind::Num(f64::from(size), true), span: at }, span: at,
        }));
    }
    if let Some(motion) = profile.motion {
        prelude.extend([("ACCEL_LSB_PER_G", motion.accel_lsb_per_g), ("GYRO_LSB_PER_DPS", motion.gyro_lsb_per_dps)].map(|(name, size)| Item::Let {
            name: name.to_owned(), ann: None, value: Expr { kind: ExprKind::Num(f64::from(size), true), span: at }, span: at,
        }));
    }
    prelude.extend(assets::declarations(&images, at));
    let own: std::collections::HashSet<String> = items.iter().map(|i| i.name().to_string()).collect();
    let all = makepad_script_compute::with_prelude(&items, prelude, &own);
    let spec = EventSpec { ports: PORTS, forms: FORMS, entry_fns: &[SETUP], floats: true, max_entry_cost: profile.max_entry_ops, cap_port: Some(rt::ports::LOOP_CAP), marks: true, call_ops: profile.call_ops };
    let mut lowered = lower_events(&all, user_len + 1, &spec)
        .map_err(|e| source_error(kernel::foreign_form_error(e, &all, KERNEL_FORMS)))?;
    check_copies(&lowered).map_err(source_error)?;
    let eeprom_waits = eeprom_costs(&mut lowered, profile);
    // v1 carries every asset, even when the program never uses its declaration.
    let (module, marks) = encode::encode_marked(&lowered, profile.id, &images)
        .map_err(|m| source_error(ShaderError::new(0, 1, format!("internal compiler error: {}", m))))?;
    let (pwm, serial) = match rt::module::load(&module, profile) {
        Ok(loaded) => (loaded.pwm().to_vec(), loaded.serial().to_vec()),
        Err(e) => return Err(source_error(load_error(&e, &lowered, profile, pins))),
    };
    let mut notes: Vec<String> = pwm.iter().map(|p| {
        let (output, timer) = pwm_pin(profile, p.pin).expect("verified PWM setting");
        let mode = match p.setting.mode { rt::pwm::PwmMode::Fast => "fast", rt::pwm::PwmMode::PhaseCorrect => "phase-correct" };
        let free = if p.setting.top != timer.top { format!(" ({})", timer.free_top_reg) } else { String::new() };
        format!("pin {} ({}, {}): {} Hz (asked {} Hz), {}, clk/{}, TOP {}{}", pin_name(p.pin, pins), output.output, timer.name, hz_text(p.real_mhz), hz_text(p.asked_mhz as u64), mode, p.setting.prescale, p.setting.top, free)
    }).collect();
    let wcet_ns = lowered.entries.iter().map(|e| e.cost).max().map_or(0, |cost| cost as u128 * profile.op_ns as u128);
    notes.extend(serial.iter().map(|s| {
        format!("serial link {}: {} baud (asked {}), UBRR {}, U2X {}; the watch stream runs at this rate too; FIFO {} bytes; longest handler {} ns against a byte time of {} ns", s.link, hz_text(s.setting.real_milli_baud(profile.clock_hz)), s.setting.asked, s.setting.ubrr, u8::from(s.setting.u2x), s.fifo, wcet_ns, s.setting.byte_ns(profile.clock_hz, profile.serial.frame_bits))
    }));
    if eeprom_waits {
        notes.push(format!("EEPROM: each access is counted at {} ms ({} ops, rounded up) in the handler cost; DS40002061B section 8.4's EEPROM mode-bit table gives the erase-and-write programming time as a single nominal figure", f64::from(profile.eeprom_write_ns) / 1_000_000.0, profile.eeprom_access_ops()));
    }
    // Shared helpers (`call_ops`): the calls that kept a copy for a literal argument, so a build
    // shows what the exact-literal rule costs.
    if !lowered.copies.is_empty() {
        let list: Vec<String> = lowered.copies.iter().map(|(name, n)| format!("{name} x{n}")).collect();
        notes.push(format!("{} helper calls kept a copy for a literal argument: {}", lowered.copies.iter().map(|(_, n)| n).sum::<u32>(), list.join(", ")));
    }
    if !others.is_empty() {
        notes.push(format!("not shipped (pictures are the only files a firmware carries so far): {}", others.join(", ")));
    }
    let images = images.into_iter().map(|image| image.name).collect();
    Ok(finish_compiled(source, module, images, lowered, marks, pwm, serial, notes))
}

fn finish_compiled(source: &str, module: Vec<u32>, images: Vec<String>, lowered: EventLowered, marks: Vec<(u32, u32)>, pwm: Vec<rt::pwm::PwmOut>, serial: Vec<rt::serial::SerialOut>, notes: Vec<String>) -> Compiled {
    let sites = lowered.sites.iter().map(|s| site(source, s.start, s.end)).collect();
    // The lowering numbers statements and conditions in one series (its marks); each gets its
    // own table here.
    let mut ids = vec![None; lowered.marks.len()];
    let (mut stmts, mut conds) = (Vec::new(), Vec::new());
    for (entry, e) in lowered.entries.iter().enumerate() {
        for id in e.marks.clone() {
            let span = lowered.marks[id as usize];
            let site = site(source, span.start, span.end);
            let owner = lowered.funcs.iter().enumerate().filter(|(_, f)| f.marks.contains(&id))
                .min_by_key(|(_, f)| f.marks.end - f.marks.start)
                .map_or(Owner::Handler(entry as u32), |(k, _)| Owner::Func(k as u32));
            ids[id as usize] = Some(match lowered.conds.binary_search_by_key(&id, |c| c.0) {
                Ok(k) => {
                    conds.push(CondInfo { site, owner, tests: lowered.conds[k].1 });
                    (true, conds.len() as u32 - 1)
                }
                Err(_) => {
                    stmts.push(StmtInfo { site, owner });
                    (false, stmts.len() as u32 - 1)
                }
            });
        }
    }
    let (mut stmt_marks, mut cond_marks) = (Vec::new(), Vec::new());
    for (at, id) in marks {
        match ids.get(id as usize).copied().flatten() {
            Some((true, cond)) => cond_marks.push((at, cond)),
            Some((false, stmt)) => stmt_marks.push((at, stmt)),
            None => {}
        }
    }
    Compiled { module, images, lowered, sites, stmts: stmts.into(), marks: stmt_marks, conds: conds.into(), cond_marks, pwm, serial, notes }
}

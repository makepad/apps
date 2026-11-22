//! A firmware document to an ATmega328P flash image in one call: the bench's compile-time
//! constants go into the compiler as typed values (never as text), the chip's pin table names the
//! pins, and every refusal is a diagnostic with a byte span into the document where it has one.

use crate::chip;
use crate::emit::{build_module, Options};
use fab_elec_mcu::{compile_with, debug_table, ChipMap};
pub use fab_elec_mcu::Diagnostic;
use fab_elec_mcu_emu::FwDebug;
use fab_elec_mcu_rt::{module, ports, watch::Level};

/// A built image and what stepping, the observations and the code view need of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Firmware {
    /// The flash image in words from address 0, as `AvrMachine::new` takes it.
    pub words: Vec<u16>,
    /// The CPU clock it is built for, Hz.
    pub f_clk: u32,
    /// The id its Hello frame carries: a hash of the source, constants, clock and watch level.
    pub image_id: u32,
    /// The image's table for a co-simulated device: its step map (None, named in `notes`, when
    /// the emitter's map does not hold together), statements, handlers, variables and runtime
    /// routines, in word and data addresses. A handler is its module entry number (module order,
    /// setup included). Its `source_rev` is the host's to set.
    pub debug: FwDebug,
    /// The compiler's notes and the real PWM, serial and ADC settings.
    pub notes: Vec<String>,
    /// The pins that are ever analog, by the chip's pin names: the inputs the source declares
    /// `analog(..)`, then the chip's analog-only pins. The firmware uses every other pin digitally.
    pub analog: Vec<String>,
    /// Each pin the source declares, by the chip's pin name, and its role.
    pub pins: Vec<(String, ports::PinRole)>,
}

/// Builds `source` for an ATmega328P whose CPU clock is `f_clk` Hz (the crystal's value), with the
/// bench's `consts` replacing the values of the document's top-level `let`s of those names. A
/// constant the document does not declare, or a value its `let` cannot hold, is a diagnostic.
pub fn firmware(source: &str, f_clk: f64, consts: &[(&str, f64)]) -> Result<Firmware, Vec<Diagnostic>> {
    firmware_with_watch(source, f_clk, consts, Level::Faults)
}

/// Builds the same board/simulator image with the requested observation level.
pub fn firmware_with_watch(source: &str, f_clk: f64, consts: &[(&str, f64)], watch: Level) -> Result<Firmware, Vec<Diagnostic>> {
    if !(f_clk.is_finite() && f_clk >= 1.0 && f_clk.fract() == 0.0 && f_clk <= u32::MAX as f64) {
        return Err(vec![Diagnostic::new(None, format!("the MCU clock must be a whole number of Hz; it is {f_clk} Hz"))]);
    }
    let f = f_clk as u32;
    let profile = chip::profile(f);
    let c = compile_with(source, &profile, &chip::compiler_pins(), consts, &[])
        .map_err(|e| e.diagnostics(source.len()))?;
    let m = module::load(&c.module, &profile)
        .map_err(|e| vec![Diagnostic::new(None, format!("internal compiler error: the verified module does not load: {e}"))])?;
    let image_id = image_id(source, consts, f, watch);
    let options = Options {
        f_clk: f,
        image_id,
        watch,
        sites: c.sites.iter().map(|s| (s.line, s.col)).collect(),
        marks: c.marks.clone(),
        conds: c.cond_marks.clone(),
        ..Options::default()
    };
    let built = build_module(&m, &options).map_err(|r| vec![Diagnostic::new(c.code_span(r.entry, r.code, source.len()), r.message)])?;
    let mut entries: Vec<_> = built.map.entries.iter().enumerate().map(|(k, &(start, _))| (start, k as u32)).collect();
    entries.sort_unstable();
    let runtime_end = entries.first().map_or(0, |e| e.0);
    let mut returns: Vec<_> = built.map.returns.iter().map(|r| r.0).collect();
    returns.sort_unstable();
    returns.dedup();
    let (mut debug, note) = debug_table(&c, source.len(), ChipMap {
        funcs: Vec::new(), calls: Vec::new(), owners: Vec::new(),
        image_id, stmt_starts: &built.map.stmt_starts, conds: built.map.conds.iter().map(|&(pc, cond)| (pc, cond, true)).collect(), entries, returns,
        state: u32::from(built.image.layout.state), runtime: built.map.runtime.clone(), runtime_end,
    });
    debug.fault_cells = built.image.layout.fault_cells.clone();
    let mut notes = c.notes.clone();
    if built.image.bounds.eeprom_write != 0 {
        // Replace the frontend's generic nominal-time note for this chip.
        if let Some(note) = notes.iter_mut().find(|note| note.starts_with("EEPROM:")) {
            *note = format!("EEPROM: Assumed full-range write bound {} ns ({} ops per access); DS40002061B Table 8-2 gives a typical 26,368 calibrated RC cycles, and Table 29-9 gives factory 8 MHz ±10% only at 3 V, 25 °C. Charged at 7.2 MHz, rounded up; the emulator's nominal 3.4 ms is below this bound. The datasheet does not guarantee this time over the full operating range", profile.eeprom_write_ns, profile.eeprom_access_ops());
        }
    }
    if built.image.bounds.adc_convert != 0 {
        let adc = crate::runtime::Adc::new(f).expect("image checked the ADC clock");
        let us = |cycles: u32| f64::from(cycles) * 1_000_000.0 / f64::from(f);
        let stub = if built.image.bounds.adc == 0 { String::new() } else {
            format!("; computed-pin stub takes {:.3} us", us(built.image.bounds.adc))
        };
        notes.push(format!("analog inputs: ADC clock {} kHz ({} MHz / {}); analog_read conversion takes {:.3} us and samples {:.3} us after conversion entry{stub} (excluding caller CALL and interrupt service)",
            f64::from(f) / f64::from(adc.div) / 1000.0, f64::from(f) / 1_000_000.0, adc.div,
            us(built.image.bounds.adc_convert), us(built.image.bounds.adc_sample)));
    }
    if let Some(note) = note { notes.push(note); }
    let pins: Vec<(String, ports::PinRole)> = m.decls().iter().filter_map(|d| {
        let (pin, role) = ports::pin_role(d.form, &d.args)?;
        let n = u32::try_from(pin).ok()?;
        let name = fab_elec_mcu_avr::chip::by_number(n).map(|p| p.name)
            .or_else(|| fab_elec_mcu_avr::chip::ANALOG_ONLY.iter().find(|a| a.number == n).map(|a| a.name))?;
        Some((name.to_owned(), role))
    }).collect();
    let mut analog: Vec<String> = pins.iter().filter(|(_, r)| *r == ports::PinRole::Analog).map(|(n, _)| n.clone()).collect();
    for pin in &fab_elec_mcu_avr::chip::ANALOG_ONLY {
        if !analog.iter().any(|a| a == pin.name) {
            analog.push(pin.name.to_owned());
        }
    }
    Ok(Firmware { words: built.image.words, f_clk: f, image_id, debug, notes, analog, pins })
}

/// FNV-1a over source, constants (name, then value bits), clock and watch level.
fn image_id(source: &str, consts: &[(&str, f64)], f_clk: u32, watch: Level) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
    };
    eat(source.as_bytes());
    for (name, value) in consts {
        eat(&[0xff]);
        eat(name.as_bytes());
        eat(&[0]);
        eat(&value.to_bits().to_le_bytes());
    }
    eat(&f_clk.to_le_bytes());
    eat(&[watch as u8]);
    h
}

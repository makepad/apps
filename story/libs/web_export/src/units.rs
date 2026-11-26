//! The module-wise strip for a film (the wasm optimiser's `units`): a
//! coverage build of the linked player runs the film in the safety gate,
//! and the export then cuts every unit of code (a widget module, a
//! decoder, an effect family) the film never used.
//!
//! - [`instrument`]: the linked wasm with the coverage probe (an exported
//!   `__cov` memory), for a coverage export; the gate writes the probe's
//!   memory to `<work>/coverage.bin` after the play.
//! - [`coverage`]: that memory read back as the functions entered.
//! - [`optimise`]: `link::optimise` with the strip: units the coverage
//!   never saw used are stubbed and dropped, with their data.

use crate::{Diagnostic, Phase};
use makepad_wasm_strip::{Coverage, OptimizeOptions};

/// The first entry of a widget's apply ends the registration phase:
/// registering a module at startup is not using it.
pub const PHASE_MARKS: [&str; 1] = ["WidgetRef as makepad_script::traits::ScriptApply>::script_apply"];

/// The linked wasm with the coverage probe.
pub fn instrument(linked: &[u8]) -> Result<Vec<u8>, Diagnostic> {
    let marks: Vec<String> = PHASE_MARKS.iter().map(|m| m.to_string()).collect();
    makepad_wasm_strip::wasm_instrument_coverage(linked, &marks).map_err(|e| Diagnostic::error(Phase::Optimise, format!("coverage build: {e}")))
}

/// The functions a coverage play entered: `instrumented` (what
/// [`instrument`] made) and the probe memory the gate wrote.
pub fn coverage(instrumented: &[u8], probe: &[u8]) -> Result<Coverage, Diagnostic> {
    makepad_wasm_strip::wasm_coverage(instrumented, probe).map_err(|e| Diagnostic::error(Phase::Optimise, format!("coverage: {e}")))
}

/// [`crate::link::optimise`] with the module-wise strip: the units report
/// leads the text. `keep`: units kept whatever the coverage says.
pub fn optimise(wasm: &[u8], coverage: &Coverage, keep: &[String]) -> Result<(Vec<u8>, String), Diagnostic> {
    let profile = makepad_wasm_strip::wasm_size_profile(wasm, &makepad_wasm_strip::ProfileOptions::default()).map(|p| p.to_text(25)).unwrap_or_default();
    let opts = OptimizeOptions { coverage: Some(coverage.clone()), keep_units: keep.to_vec(), ..OptimizeOptions::default() };
    let (out, report) = makepad_wasm_strip::wasm_optimize_checked(wasm, &opts).map_err(|e| Diagnostic::error(Phase::Optimise, format!("the linked wasm: {e}")))?;
    Ok((out, format!("{}{}\n{profile}", report.units.clone().unwrap_or_default(), report.to_text())))
}

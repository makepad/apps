//! Test-only ratchet on the blend sites that build a *phrase cause*: a
//! [`BlendFailure`](super::blend::BlendFailure) that reads as neither a defect nor a
//! refusal ([`BlendFailure::is_phrase`](super::blend::BlendFailure::is_phrase)). The
//! count may only fall: each site moves to a finer cause, a refusal, or a
//! [`BlendFailure::Defect`](super::blend::BlendFailure::Defect) with its reason, and the PR
//! that converts it lowers its file's pin (see [`PINS`]). The other areas' phrase causes
//! are counted the same way, each area's list in its own error file; how to convert a site
//! is the conversion guide (`docs/failure-codes-and-reproduction.md` §6).
//!
//! **What counts as one site**, in the library code of `solidcore` and `solidcore-ops`
//! (comments, string contents and every `#[cfg(test)]` item blanked, test files skipped:
//! [`super::bare_scan::crate_library_files`]), as [`super::site_scan`] counts it for
//! [`BLEND`]:
//! - a construction of a phrase variant, `BlendFailure::Failed`, `Missing`,
//!   `InvalidValue`, `Inconsistent`, `BadIntersections`, `NotConverged`, `OutsideDomain`,
//!   `NotWithinTolerance` or `PassedOn`, by any path ending in `BlendFailure::<variant>`;
//! - a call of a helper that builds one: `blend_failed(`, `blend_missing(`,
//!   `blend_invalid_value(` (their own definitions are not sites);
//! - a call of a local *wrapper*: a `let` closure, a private `fn` or a `macro_rules!` that
//!   passes one of its own parameters on as the phrase of one of the above (a variant's
//!   field or `PassedOn`'s `what`, a helper's second argument, the same parameter of a
//!   wrapper, any argument of a macro). Passing on only the operation does not make a
//!   wrapper: that construction is one site. The wrapper's body is not a site; each call of it in its scope is (the rest of
//!   the enclosing block for a closure, the file for a `fn` or a macro). Wrappers of
//!   wrappers resolve the same way.
//!
//! Not counted: the variant definitions and their `Self::` matches in `error/blend.rs`,
//! patterns (a variant followed by `=>`, `|`, `if` or `=`, binding `_` or `..`, or in the
//! pattern of a `matches!`), and comments, strings and tests. A closure that builds a
//! fixed phrase (`let nul = || SolidError::blend_missing(OP, "..")`) is one site however
//! often it runs. A wrapper `fn` visible outside its file fails the scan, since its calls
//! elsewhere would not be seen.












#[cfg(test)]
mod tests {
    

    

    

    

    
}
// Modified for Makepad by fab/brep/sync/tool (fab/brep/SYNC.md): S1, O1.

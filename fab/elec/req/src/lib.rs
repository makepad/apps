//! Requirements that check themselves: the person's targets for the whole project, written once
//! in their words in a requirements document (`*.req.splash`), each judged pass, fail, unknown or
//! waived against what Fab's own producers measured.
//!
//! ```splash
//! require("fits a 60 x 40 case", board("ledctl").size, {max: [60mm 40mm]})
//! require("ERC clean", checks(@erc).errors, {max: 0})
//! require("LEDs light", bench("bringup"))
//! require("prints in 3 hours", print.time, {max: 3h})
//! waive("prints in 3 hours", "first prototype: time is no concern")
//! ```
//!
//! The document is literal statements read from the Splash syntax tree ([`read`]), never run.
//! [`judge`] holds each requirement against [`Facts`]: plain data the host gathers from the design,
//! the findings list, the case checks, the bench's counted check and the slice estimate, each with
//! whether it is current. A requirement that cannot be measured now is unknown with its why, and
//! unknown never counts as met. The words and the bound shape follow `m.expect` in object scripts.
//!
//! Cost: reading is O(document text); judging is O(requirements x findings of the families they
//! count + facts). Neither allocates per frame; both run on the host's pool.

mod judge;
mod read;
pub mod write;
#[cfg(test)]
mod tests;

pub use judge::{judge, BenchFacts, BenchRun, CaseFacts, CheckFacts, Facts, FindingFact, Focus, Outcome, PrintFacts, Producers, Report, Row, Status, UnitFacts, BoardMeasures, Verdict};
pub use read::{read, units};

use fab_elec_core::DocId;
use std::ops::Range;

/// One requirements document as read: its requirements and waivers in document order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReqDoc {
    pub doc: DocId,
    pub requirements: Vec<Requirement>,
    pub waivers: Vec<Waiver>,
}

/// `require(name, measure, bound?)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Requirement {
    /// The person's words, unique among the project's requirements.
    pub name: String,
    pub measure: Measure,
    pub bound: Bound,
    /// The whole statement's bytes in its document.
    pub span: Range<u32>,
    /// The measure and the bound as written (the bound empty when there is none).
    pub measure_text: String,
    pub bound_text: String,
}

/// `waive(name, reason)`: the person accepts a requirement that is not met, and says why.
/// `propose_waiver(name, reason)`: the AI proposes one; it waives nothing until the person
/// accepts it, which writes their own `waive`.
#[derive(Clone, Debug, PartialEq)]
pub struct Waiver {
    pub name: String,
    pub reason: String,
    pub span: Range<u32>,
    /// A proposal, waiting for the person.
    pub proposed: bool,
}

/// What a requirement measures. Units are named by the unit's (circuit's) name.
#[derive(Clone, Debug, PartialEq)]
pub enum Measure {
    /// `board(u).size`: the outline's bounding box, `[w h]`.
    BoardSize(String),
    /// `board(u).layers`: copper layers.
    BoardLayers(String),
    /// `board(u).parts`: parts placed on the board.
    BoardParts(String),
    /// `board(u).unplaced`: parts of the circuit the board does not place.
    BoardUnplaced(String),
    /// `board(u).unrouted`: the board's `Unconnected` findings.
    BoardUnrouted(String),
    /// `checks(@family, {unit}?).errors` or `.warnings`.
    Checks { family: Family, unit: Option<String>, count: CountOf },
    /// `case(u).fits`: no error among the board's case findings.
    CaseFits(String),
    /// `bench(b)`: every expectation of the bench passes in its last counted check.
    Bench(String),
    /// `bench(b).expect(name)`: that expectation passes.
    BenchExpect { bench: String, expect: String },
    /// `print.time`: the slice estimate's print time.
    PrintTime,
    /// `print.filament`: the slice estimate's filament mass.
    PrintFilament,
}

/// The families of the findings list a `checks` measure counts.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Erc,
    Drc,
    System,
    /// The sharp corners a drag kept, as `elec.check.list` names them.
    Route,
    Case,
    Custom,
    All,
}

impl Family {
    pub const SPELLINGS: [(&'static str, Family); 7] = [
        ("erc", Family::Erc), ("drc", Family::Drc), ("system", Family::System), ("route", Family::Route),
        ("case", Family::Case), ("custom", Family::Custom), ("all", Family::All),
    ];

    pub fn spelling(self) -> &'static str {
        Self::SPELLINGS.iter().find(|(_, f)| *f == self).map(|(s, _)| *s).unwrap_or("all")
    }

    /// Whether a finding of family `of` counts for this family.
    pub fn counts(self, of: Family) -> bool {
        self == Family::All || self == of
    }
}

/// What a `checks` measure counts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CountOf {
    Errors,
    Warnings,
}

/// The kind of value a measure gives, which fixes the units its bound may use.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dim {
    /// `[w h]` in lengths.
    Size,
    /// A whole number, written without a unit.
    Count,
    /// Seconds: `s`, `ms`, `min`, `h`.
    Time,
    /// Grams: `g`, `kg`.
    Mass,
    /// Holds or does not; takes no bound.
    Pass,
}

impl Measure {
    pub fn dim(&self) -> Dim {
        match self {
            Measure::BoardSize(_) => Dim::Size,
            Measure::BoardLayers(_) | Measure::BoardParts(_) | Measure::BoardUnplaced(_) | Measure::BoardUnrouted(_) | Measure::Checks { .. } => Dim::Count,
            Measure::CaseFits(_) | Measure::Bench(_) | Measure::BenchExpect { .. } => Dim::Pass,
            Measure::PrintTime => Dim::Time,
            Measure::PrintFilament => Dim::Mass,
        }
    }
}

/// A value in the measure's base unit: metres for lengths, seconds, grams, counts.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Value {
    Scalar(f64),
    Size([f64; 2]),
}

/// What the person wants of a measure.
#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    /// A pass measure: it must hold.
    Holds,
    /// `{min max}`, either or both.
    Range { min: Option<Value>, max: Option<Value> },
    /// `{is: v tol?: t}`: within `tol` of `v` (default: 0.05 mm for lengths, exact for counts,
    /// 1% for times and masses).
    Is { value: Value, tol: Option<f64> },
}

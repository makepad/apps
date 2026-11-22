//! Circuit edits (C3, E2.6 gestures).
//!
//! The circuit is code, so a `CircuitOp` carries statement text by nature and is applied by the
//! language as a span edit on the circuit document (`fab_elec_lang::splice`), followed by
//! re-evaluation; its inverse is the inverse span edit, recorded in the op log
//! (`fab_elec_design::OpLog`). Nobody builds a model-level inverse for it (E4 item 7).

use crate::{NetStyle, SheetHint, Value};
use fab_elec_core::{CompId, NetId, PinIx, Sel};

/// What fills one `_nk` slot of an `AddPart` template.
#[derive(Clone, PartialEq, Debug)]
pub enum End {
    /// A new unnamed net: the end stays open (`e.net()`).
    Open,
    /// A fresh net shared by the slots with this key in one commit's AddParts.
    Shared(u32),
    /// This net.
    Net(NetId),
    /// This pin: its net when it is on one, else the pin itself (the new part's end and the pin
    /// share a new net).
    Pin(CompId, PinIx),
}

#[derive(Clone, PartialEq, Debug)]
pub enum CircuitOp {
    /// Insert one Splash statement (`stmt`), normally a catalog template, after the statement
    /// of `after` or at the end. `_n0.._nk` stand for `ends[k]`;
    /// a slot past the end of `ends` is open. The template's `let` name is a stem the splice
    /// renames, and `hint` is written as the new part's `sheet({..})` entry.
    AddPart {
        /// One Splash statement, normally a catalog template.
        stmt: String,
        /// The component whose statement precedes the insertion, or the end when absent.
        after: Option<CompId>,
        /// Ends substituted for the template's numbered slots.
        ends: Vec<End>,
        /// The new part's sheet entry, when given.
        hint: Option<SheetHint>,
    },
    Remove(CompId),
    Connect { pin: (CompId, PinIx), net: NetId },
    /// Connect to the net with this exact name, creating a signal net when the name is new.
    ConnectNamed { pin: (CompId, PinIx), name: String },
    /// Wire two pins where neither is on a net; the language makes and names the net.
    ConnectPins {
        /// The first pin to wire.
        a: (CompId, PinIx),
        /// The second pin to wire.
        b: (CompId, PinIx),
    },
    Disconnect { pin: (CompId, PinIx) },
    /// Add the pin to, or remove it from, its part's no-connect list.
    NoConnect {
        /// The pin whose no-connect status changes.
        pin: (CompId, PinIx),
        /// Whether the pin belongs in the no-connect list.
        nc: bool,
    },
    /// Insert a two-terminal part in series on `net`: the net splits, the pins in `moved` (each on
    /// `net`, sorted by component then pin) go to the new side, and `stmt` (Splash, its ends written
    /// `_a` for `net` and `_b` for the new side) joins the two. The new net's name is derived from
    /// `net`'s name (`<NAME>_S`, then `_S2`.. while taken), so a replay names it the same. A moved
    /// pin not on `net` refuses the op and nothing changes.
    InsertSeries { net: NetId, stmt: String, moved: Vec<(CompId, PinIx)> },
    SetValue { comp: CompId, value: Value },
    Rename { what: Sel, to: String },
    /// Write or remove (`None`) a `sheet({...})` entry.
    Hint { path: String, hint: Option<SheetHint> },
    /// Set or remove the named net's drawing style in the sheet's nets table.
    NetStyle { name: String, style: Option<NetStyle> },
    /// Write (`Some`) or remove (`None`) one of a component's own text fields, the
    /// `fields: {key: "text"}` of the call that makes it (`Component::fields`).
    SetField { comp: CompId, key: String, text: Option<String> },
    /// Store the component's reference designator on its call, or remove it to resume numbering.
    SetRef { comp: CompId, to: Option<String> },
    /// Write (`Some`: a Splash object such as `{mesh: "parts/lcsc/C1525.glb"}`) or remove (`None`)
    /// the `e.body` override of the part named `part`, which every component of that part draws:
    /// in its statement where one is written, else at the end of the unit's models document
    /// (`<unit>.models.part.splash`, which the commit adds first when the project has none).
    SetBody { part: String, body: Option<String> },
}

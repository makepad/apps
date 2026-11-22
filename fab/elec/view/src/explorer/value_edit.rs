//! The Circuit sheet's part-value presets and inline edits for values, references and net names.

use fab_elec_circuit::{Circuit, CircuitOp, QtyUnit, RefKind, Value};
use fab_elec_core::{BoardId, CompId, CompRef, NetId};
use fab_elec_explore::{edit::Target, plan::label_net};
use fab_elec_parts::PartKind;
use makepad_widgets_core::{Event, Rect};
use std::sync::Arc;

/// The card's preset labels and their SI values, in menu order. The lookup borrows a static slice.
pub(super) fn value_presets(unit: Option<QtyUnit>) -> &'static [(&'static str, f64)] {
    match unit {
        Some(QtyUnit::Ohm) => &[
            ("10", 10.0), ("22", 22.0), ("47", 47.0), ("100", 100.0), ("220", 220.0), ("470", 470.0),
            ("1k", 1e3), ("2.2k", 2.2e3), ("4.7k", 4.7e3), ("10k", 10e3), ("22k", 22e3),
            ("47k", 47e3), ("100k", 100e3), ("220k", 220e3), ("470k", 470e3), ("1M", 1e6),
        ],
        Some(QtyUnit::Farad) => &[
            ("10pF", 10e-12), ("22pF", 22e-12), ("100pF", 100e-12), ("1nF", 1e-9),
            ("10nF", 10e-9), ("100nF", 100e-9), ("220nF", 220e-9), ("1uF", 1e-6),
            ("4.7uF", 4.7 / 1e6), ("10uF", 10e-6), ("22uF", 22e-6), ("47uF", 47e-6), ("100uF", 100e-6),
        ],
        Some(QtyUnit::Henry) => &[
            ("1uH", 1e-6), ("2.2uH", 2.2 / 1e6), ("4.7uH", 4.7 / 1e6), ("10uH", 10e-6),
            ("22uH", 22e-6), ("47uH", 47e-6), ("100uH", 100e-6), ("1mH", 1e-3),
        ],
        Some(QtyUnit::Hertz) => &[
            ("32.768kHz", 32768.0), ("8MHz", 8e6), ("12MHz", 12e6), ("16MHz", 16e6),
            ("20MHz", 20e6), ("25MHz", 25e6),
        ],
        _ => &[],
    }
}

/// Text to edit on opening. A preset keeps all its digits even when the sheet rounds its label.
pub(super) fn edit_text(current: &Value) -> &str {
    let Some(q) = current.qty else { return &current.text };
    value_presets(Some(q.unit)).iter().find(|&&(_, si)| si == q.si)
        .map_or(current.text.as_str(), |&(label, _)| label)
}

pub(super) struct ValueEdit {
    pub comp: CompId,
    pub unit: BoardId,
    pub refdes: String,
    pub current: Value,
    pub site: Option<String>,
    pub kind: PartKind,
    /// Keystrokes received by the sheet until the input takes focus, then None.
    pub pending_keys: Option<Vec<Event>>,
}

/// Retain a key for the input's first focused frame, preserving native selection and editing.
pub(super) fn queue_field_key(keys: &mut Vec<Event>, event: &Event) -> bool {
    keys.push(match event {
        Event::TextInput(text) => Event::TextInput(text.clone()),
        Event::KeyDown(key) => Event::KeyDown(*key),
        Event::KeyUp(key) => Event::KeyUp(*key),
        _ => return false,
    });
    true
}

/// The single text input on the sheet names either a component on the card or a net at a click.
pub(super) enum NameEdit {
    Reference(ReferenceEdit),
    Net(NetNameEdit),
}

/// The source part and the reference shown when editing began.
pub(super) struct ReferenceEdit {
    pub part: CompRef,
    pub path: String,
    pub reference: String,
    pub kind: RefKind,
}

impl ReferenceEdit {
    /// Enter writes a changed reference or clears a stored one. O(length of the text).
    pub fn submission(&self, text: &str) -> Option<(String, CircuitOp)> {
        if text == self.reference || self.kind == RefKind::Shared { return None; }
        let (label, to) = if text.is_empty() {
            if self.kind != RefKind::Stored { return None; }
            (format!("Number {} automatically", self.reference), None)
        } else {
            (format!("Set reference of {} to {text}", self.reference), Some(text.into()))
        };
        Some((label, CircuitOp::SetRef { comp: self.part.comp, to }))
    }
}

impl NameEdit {
    /// Recheck the field's original target after a source change. A part uses one indexed lookup.
    pub fn matches(&self, circuit: &Circuit) -> bool {
        match self {
            Self::Net(edit) => edit.matches(circuit),
            Self::Reference(edit) => circuit.unit == edit.part.board && circuit.comps.get(edit.part.comp.ix())
                .is_some_and(|c| c.id == edit.part.comp && c.path == edit.path && c.refdes == edit.reference && c.ref_kind == edit.kind),
        }
    }
}

/// What the field names when it opened, and the click where its answer belongs.
pub(super) struct NetNameEdit {
    pub target: Target,
    pub board: BoardId,
    /// The original net and name for a Net label or wire end. A Global label ties by name.
    pub net: Option<(NetId, String)>,
    /// Present when the input finishes a wire on empty sheet.
    pub wire_end: Option<WireName>,
    pub at: [f64; 2],
    pub screen: Rect,
}

/// Wire naming uses the gesture's committed circuit even while its last hover preview clears.
pub(super) struct WireName {
    pub circuit: Arc<Circuit>,
    pub other: Option<NetId>,
}

impl NetNameEdit {
    /// Check a replacement circuit before using positional IDs. Only a picked pin's connections
    /// and the net's name are inspected.
    pub fn matches(&self, circuit: &Circuit) -> bool {
        if let Some(end) = &self.wire_end { return std::ptr::eq(&*end.circuit, circuit); }
        if circuit.unit != self.board { return false; }
        match (&self.net, self.target) {
            (Some((id, name)), target) => label_net(circuit, target).is_ok_and(|net| net.id == *id && net.name == *name),
            (None, Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. }) => {
                circuit.comps.get(comp.ix()).is_some_and(|c| c.part.pins.get(pin.ix()).is_some())
            }
            (None, _) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_circuit::{QtyUnit, Quantity, Tol};
    use fab_elec_core::DocId;
    use fab_elec_lang::{map::site_text, units::{parse_value, value_text}, SourceRange, ValueSite};

    #[test]
    fn reference_submission_sets_clears_or_keeps_the_displayed_reference() {
        let part = CompRef { board: BoardId(0), comp: CompId(3) };
        let mut edit = ReferenceEdit { part, path: "r".into(), reference: "R116".into(), kind: RefKind::Auto };
        assert_eq!(edit.submission("R116"), None, "accepting the displayed number must keep the part automatic");
        assert_eq!(edit.submission(""), None, "clearing an automatic number must leave the source alone");
        assert_eq!(edit.submission("R7"), Some(("Set reference of R116 to R7".into(),
            CircuitOp::SetRef { comp: part.comp, to: Some("R7".into()) })), "a changed reference must be written to this part");
        edit.kind = RefKind::Stored;
        assert_eq!(edit.submission("R116"), None, "accepting an unchanged stored reference must leave the source alone");
        assert_eq!(edit.submission(""), Some(("Number R116 automatically".into(),
            CircuitOp::SetRef { comp: part.comp, to: None })), "clearing a stored reference must remove it from the call");
        assert!(matches!(edit.submission(" "), Some((_, CircuitOp::SetRef { to: Some(text), .. })) if text == " "),
            "whitespace must reach the host for validation and must never clear a stored reference");
    }

    #[test]
    fn card_presets_round_trip() {
        for (unit, kind, count) in [(QtyUnit::Ohm, PartKind::Resistor, 16), (QtyUnit::Farad, PartKind::Capacitor, 13),
            (QtyUnit::Henry, PartKind::Inductor, 8), (QtyUnit::Hertz, PartKind::Crystal, 6)] {
            let current = Value { text: "0".into(), qty: Some(Quantity { si: 0.0, unit }), tol: Some(Tol::pct(1.0)) };
            let presets = value_presets(Some(unit));
            assert_eq!(presets.len(), count, "the card offers every {unit:?} preset");
            for &(label, si) in presets {
                let value = parse_value(label, &current, &kind).unwrap_or_else(|why| panic!("preset {label} must parse: {why}"));
                let expected = Quantity { si, unit };
                assert_eq!(value.qty, Some(expected), "preset {label} must keep its exact quantity");
                assert_eq!(value.tol, current.tol, "preset {label} must preserve tolerance");
                assert_eq!(value.text, value_text(&expected), "preset {label} must use the sheet's display text");
                assert_eq!(edit_text(&value), label, "reopening preset {label} must preserve every digit");
            }
        }
        assert!(value_presets(None).is_empty(), "a text label has no presets");
        assert!(value_presets(Some(QtyUnit::Volt)).is_empty(), "a voltage takes a typed value");
        let current = Value { text: "0F".into(), qty: Some(Quantity { si: 0.0, unit: QtyUnit::Farad }), tol: None };
        for text in ["1µ", "1µF", "1u"] {
            assert_eq!(parse_value(text, &current, &PartKind::Capacitor).map(|v| v.text), Ok("1uF".into()), "{text} must mean one microfarad");
        }
    }

    #[test]
    fn quantities_keep_units_tolerance_and_canonical_text() {
        let resistance = Value {
            text: "10k".into(), qty: Some(Quantity { si: 10_000.0, unit: QtyUnit::Ohm }),
            tol: Some(Tol::pct(5.0)),
        };
        let edited = parse_value("4.7k", &resistance, &PartKind::Resistor).expect("value edit: 4.7k parses on a resistance");
        let expected = Quantity { si: 4700.0, unit: QtyUnit::Ohm };
        assert_eq!(edited.qty, Some(expected), "value edit: 4.7k on a resistance is 4.7 kohm");
        assert_eq!(edited.text, value_text(&expected), "value edit: resistance uses the scrub's canonical text");
        assert_eq!(edited.tol, resistance.tol, "value edit: declared tolerance is kept");
        let capacitance = Value {
            text: "1uF".into(), qty: Some(Quantity { si: 1e-6, unit: QtyUnit::Farad }), tol: None,
        };
        let edited = parse_value("100n", &capacitance, &PartKind::Capacitor).expect("value edit: bare nano prefix parses for capacitance");
        assert_eq!(edited.qty, Some(Quantity { si: 100e-9, unit: QtyUnit::Farad }),
            "value edit: 100n on a capacitance is 100 nF");
        assert_eq!(edited.text, "100nF", "value edit: capacitance has canonical unit text");
        let edited = parse_value(" .5 ", &capacitance, &PartKind::Capacitor).expect("value edit: a trimmed decimal parses");
        assert_eq!(edited.qty, Some(Quantity { si: 0.5, unit: QtyUnit::Farad }),
            "value edit: a bare number keeps the current unit");
        for text in ["4.7kohm", "4.7e3ohm", "4700", " 4.7 k "] {
            assert_eq!(parse_value(text, &resistance, &PartKind::Resistor).map(|v| v.qty), Ok(Some(expected)),
                "value edit: registered suffixes, exponents and whitespace parse: {text}");
        }
        assert_eq!(parse_value("1M", &resistance, &PartKind::Resistor).map(|v| v.qty),
            Ok(Some(Quantity { si: 1e6, unit: QtyUnit::Ohm })), "value edit: uppercase M means mega");
        assert_eq!(parse_value("1m", &resistance, &PartKind::Resistor).map(|v| v.qty),
            Ok(Some(Quantity { si: 1e-3, unit: QtyUnit::Ohm })), "value edit: lowercase m means milli");
    }

    #[test]
    fn invalid_values_are_refused() {
        let resistance = Value {
            text: "10k".into(), qty: Some(Quantity { si: 10_000.0, unit: QtyUnit::Ohm }), tol: None,
        };
        assert_eq!(parse_value("4.7uF", &resistance, &PartKind::Resistor), Err("a resistance needs ohms: 4.7uF".into()),
            "value edit: a suffix of another unit is refused");
        assert_eq!(parse_value("4.7q", &resistance, &PartKind::Resistor), Err("not a value: 4.7q".into()),
            "value edit: an unknown suffix explains the refusal");
        for text in ["", ".", "1e", "1e+", "NaN", "inf", "1e999", "4.7k7"] {
            assert!(parse_value(text, &resistance, &PartKind::Resistor).is_err(), "value edit: malformed or nonfinite numbers are refused: {text}");
        }
        let colour = Value { text: "green".into(), qty: None, tol: None };
        assert_eq!(parse_value("10", &colour, &PartKind::Led), Err("unknown LED colour: 10".into()),
            "value edit: a colour cannot be changed into a number");
        assert_eq!(parse_value(" blue ", &colour, &PartKind::Led), Ok(Value { text: "blue".into(), ..colour.clone() }),
            "value edit: a typed LED colour stays a label");
    }

    #[test]
    fn sites_describe_the_origin() {
        let at = SourceRange { doc: DocId(0), span: 0..1 };
        assert_eq!(site_text(Some(&ValueSite::Library { lib: "passives".into() }), "r"),
            Err("written in library passives: not edited here".into()), "value edit: site text refuses a library write");
        assert_eq!(site_text(None, "r"), Ok(None), "value edit: site text needs no chip without a map");
        assert_eq!(site_text(Some(&ValueSite::Literal { at: at.clone(), count: 1 }), "r"), Ok(None),
            "value edit: site text needs no chip for one literal");
        assert_eq!(site_text(Some(&ValueSite::Literal { at: at.clone(), count: 8 }), "lamps[3].r"),
            Ok(Some("r, 8 parts".into())), "value edit: site text names the definition and shared parts");
        for (count, expected) in [(1, "parameter c_in"), (8, "parameter c_in, 8 instances")] {
            let site = ValueSite::Param { name: "c_in".into(), item: None, call: at.clone(), default: None, count };
            assert_eq!(site_text(Some(&site), "c"), Ok(Some(expected.into())),
                "value edit: site text names the parameter and its instance count");
        }
        let derived = ValueSite::Derived { at, leaf: None, inputs: Vec::new(), siblings: 1 };
        assert_eq!(site_text(Some(&derived), "r"), Ok(Some("computed: edits its inputs".into())),
            "value edit: site text explains a computed value");
    }
}

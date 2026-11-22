//! The schematic's inspector (user, 11-18: "clicking something and changing the value of a
//! resistor"): a small card in a corner of the pane for the part a click selected. It names the
//! part, holds its value and lists its key facts. A click on the value types a new one in the
//! value field; a drag along it scrubs the value through the preferred numbers. Both are
//! `CircuitOp::SetValue`, written back to the circuit's source and heard at once by a playing run.
//! Selecting never moves the sheet or the camera; the projected view around a part is a
//! double-click.
//!
//! Its 3D model's rows (EKMODELS) say where the model came from (the model board's note: KiCad's
//! library on this computer or its download with tag, host and date, the EasyEDA part service, or
//! why the plain body stands in), switch the part's body between KiCad's model, EasyEDA's and its
//! plain body (the design's choice, [`super::body_source`]; one not offered says why), switch
//! where this computer gets a KiCad model, and open the External Resources and Licences page.
//!
//! Cost: per selection, design or model-board change one pass over the part's connections and
//! fields for the text and one model-board lookup; per draw up to eight card quads and a handful
//! of text runs. Nothing while no part is selected.

use fab_elec_circuit::{Circuit, Component};
use crate::explorer::body_source::{body_choices, BodyChoices};
use fab_elec_lang::map::BodySite;
use fab_elec_parts::model_board::{self, Choice, Family};
use fab_elec_parts::{Body3d, Tol};
use makepad_widgets_core::*;

/// The card's width, its inner margin, its distance from the pane's edges, and a text row's height
/// (logical points).
pub(super) const CARD_WIDTH: f64 = 232.0;
pub(super) const CARD_PAD: f64 = 10.0;
pub(super) const CARD_MARGIN: f64 = 10.0;
pub(super) const ROW: f64 = 17.0;
/// A reference or value box's height.
pub(super) const VALUE_ROW: f64 = 24.0;
/// A row of switches' height.
pub(super) const CHIP_ROW: f64 = 22.0;
/// At most this many facts are listed.
const FACTS: usize = 5;

/// What the card says about one part.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CardText {
    /// The displayed reference and the part name beside it.
    pub reference: String,
    pub part_name: String,
    /// The value as written, if the part has one.
    pub value: Option<String>,
    /// Whether the value is a quantity the field and the scrub can change.
    pub editable: bool,
    /// Its tolerance, pins, connections, fields; a part not fitted says so first.
    pub facts: Vec<String>,
    /// Its 3D model's rows, when it has a body.
    pub model: Option<CardModel>,
}

/// What the card says about a part's 3D model.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CardModel {
    /// "3D: KiCad library 9.0.0", then "from GitLab on 2026-11-19".
    pub line: String,
    pub detail: String,
    /// The model's project path, which this computer's choice is kept by.
    pub path: Option<String>,
    /// The part's body source switch: KiCad, EasyEDA or plain, and why one is not offered.
    pub body: BodyChoices,
    /// For a KiCad model: this computer's choice, and whether each of [`Choice::ALL`] has its
    /// source on.
    pub get_from: Option<(Choice, [bool; 3])>,
}

impl CardModel {
    /// Its rows' height: line, detail, the body switch, the switch for a KiCad model, the
    /// licences link.
    pub fn height(&self) -> f64 {
        ROW * 3.0 + CHIP_ROW + if self.get_from.is_some() { CHIP_ROW } else { 0.0 }
    }
}

/// The model rows for `comp`'s `body`; `site` is where its part's `e.body` is written, if it is.
/// A few model-board lookups.
pub(super) fn card_model(comp: &Component, body: &Body3d, site: Option<&BodySite>) -> CardModel {
    let model = model_board::of_body(body);
    let get_from = model.path.as_deref().filter(|_| model.family == Some(Family::Kicad))
        .map(|path| (model_board::choice(path), Choice::ALL.map(|c| model_board::available(Family::Kicad, c))));
    CardModel { line: format!("3D: {}", model.line), detail: model.detail, path: model.path, body: body_choices(comp, site), get_from }
}

/// A number without trailing zeros (`1`, `0.5`, `0.002`).
fn number(v: f64) -> String {
    let text = format!("{v:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" { "0".into() } else { text.into() }
}

/// A tolerance as a person reads it: `±1 %`, `-10 / +20 %`.
fn tolerance_text(tol: Tol) -> String {
    let (minus, plus, unit) = match tol {
        Tol::Pct { minus, plus } => (minus, plus, " %"),
        Tol::Abs { minus, plus } => (minus, plus, ""),
    };
    if minus == plus { format!("±{}{unit}", number(plus)) } else { format!("-{} / +{}{unit}", number(minus), number(plus)) }
}

/// The card's text for `comp`, a part of `circuit`; `site` is where its part's `e.body` is written,
/// if it is. O(connections + fields).
pub(super) fn card_text(circuit: &Circuit, comp: &Component, site: Option<&BodySite>) -> CardText {
    let mut facts = Vec::new();
    if comp.dnp {
        facts.push("not fitted".to_owned());
    }
    if let Some(tol) = comp.tolerance() {
        facts.push(format!("tolerance {}", tolerance_text(tol)));
    }
    let mut nets: Vec<&str> = Vec::new();
    for (_, net) in &comp.conns {
        if let Some(net) = circuit.nets.get(net.ix()) {
            if !nets.contains(&net.name.as_str()) { nets.push(&net.name); }
        }
    }
    let pins = comp.part.pins.len();
    facts.push(match nets.len() {
        0 => format!("{pins} pins, none connected"),
        1..=3 => format!("{pins} pins: {}", nets.join(", ")),
        n => format!("{pins} pins on {n} nets"),
    });
    for (key, value) in comp.fields.0.iter().filter(|(_, value)| !value.is_empty()) {
        if facts.len() >= FACTS { break; }
        facts.push(format!("{key}: {value}"));
    }
    CardText {
        reference: comp.refdes.clone(),
        part_name: if comp.part.name.is_empty() { String::new() } else { format!("· {}", comp.part.name) },
        value: comp.value.as_ref().map(|value| value.text.clone()),
        editable: comp.value.as_ref().is_some_and(|value| value.qty.is_some()),
        facts,
        model: comp.part.body.as_ref().map(|body| card_model(comp, body, site)),
    }
}

/// The card's height for `text`, with `extra` more rows (the value site, an error).
pub(super) fn card_height(text: &CardText, extra: usize) -> f64 {
    let value = if text.value.is_some() { VALUE_ROW + 6.0 } else { 0.0 };
    let model = text.model.as_ref().map_or(0.0, CardModel::height);
    CARD_PAD * 2.0 + VALUE_ROW + 4.0 + value + ROW * (text.facts.len() + extra) as f64 + model
}

fn meets(a: Rect, b: Rect) -> bool {
    a.pos.x < b.pos.x + b.size.x && b.pos.x < a.pos.x + a.size.x && a.pos.y < b.pos.y + b.size.y && b.pos.y < a.pos.y + a.size.y
}

/// Where the card sits in `pane`: its top right corner, else its bottom right, else its bottom
/// left, the first that does not cover `avoid` (the selected part on screen); the top right when
/// every corner would. The top left is the tool strip's.
pub(super) fn place_card(pane: Rect, size: DVec2, avoid: Option<Rect>) -> Rect {
    let right = pane.pos.x + pane.size.x - CARD_MARGIN - size.x;
    let bottom = pane.pos.y + pane.size.y - CARD_MARGIN - size.y;
    let left = pane.pos.x + CARD_MARGIN;
    let top = pane.pos.y + CARD_MARGIN;
    let corners = [dvec2(right, top), dvec2(right, bottom), dvec2(left, bottom)];
    let rect = |pos: DVec2| Rect { pos, size };
    corners.iter().map(|&pos| rect(pos)).find(|card| !avoid.is_some_and(|avoid| meets(*card, avoid)))
        .unwrap_or(rect(corners[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_names_the_part_its_value_and_its_connections() {
        let circuit = fab_elec_fixtures::ledctl();
        let circuit = &circuit.units[0].circuit;
        let resistor = circuit.comps.iter()
            .find(|comp| comp.value.as_ref().is_some_and(|value| value.qty.is_some()) && comp.part.pins.len() == 2)
            .expect("inspector: the fixture has a valued two-pin part");
        let text = card_text(circuit, resistor, None);
        assert_eq!(text.reference, resistor.refdes, "inspector: the first field shows the reference");
        assert_eq!(text.part_name, format!("· {}", resistor.part.name), "inspector: the part name follows the reference");
        assert_eq!(text.value.as_deref(), resistor.value.as_ref().map(|value| value.text.as_str()),
            "inspector: the value is shown as written");
        assert!(text.editable, "inspector: a quantity is editable");
        assert!(text.facts.iter().any(|fact| fact.starts_with("2 pins")), "inspector: the pins and their nets are listed: {:?}", text.facts);
        assert!(text.facts.len() <= FACTS, "inspector: the facts stay a short list");
        let unvalued = circuit.comps.iter().find(|comp| comp.value.as_ref().is_none_or(|value| value.qty.is_none()));
        if let Some(comp) = unvalued {
            assert!(!card_text(circuit, comp, None).editable, "inspector: a part without a quantity is not editable");
        }
        assert_eq!(tolerance_text(Tol::pct(1.0)), "±1 %", "inspector: a symmetric tolerance");
        assert_eq!(tolerance_text(Tol::Pct { minus: 10.0, plus: 20.0 }), "-10 / +20 %", "inspector: an asymmetric one");
        assert_eq!(tolerance_text(Tol::pct(0.002)), "±0.002 %", "inspector: a crystal's small one keeps its digits");
    }

    #[test]
    fn the_card_takes_a_corner_that_leaves_the_part_visible() {
        let pane = Rect { pos: dvec2(100.0, 50.0), size: dvec2(600.0, 400.0) };
        let size = dvec2(CARD_WIDTH, 120.0);
        let top_right = place_card(pane, size, None);
        assert_eq!(top_right.pos, dvec2(700.0 - CARD_MARGIN - CARD_WIDTH, 50.0 + CARD_MARGIN), "inspector: top right by default");
        let part = Rect { pos: top_right.pos + dvec2(20.0, 20.0), size: dvec2(10.0, 10.0) };
        let moved = place_card(pane, size, Some(part));
        assert!(!meets(moved, part), "inspector: the card never covers the selected part");
        assert_eq!(moved.pos.y, 450.0 - CARD_MARGIN - 120.0, "inspector: then the bottom right");
        let everywhere = Rect { pos: pane.pos, size: pane.size };
        assert_eq!(place_card(pane, size, Some(everywhere)).pos, top_right.pos,
            "inspector: a part under every corner leaves the card top right");
    }
}

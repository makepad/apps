//! Warning marks for circuit checks. At the outer end of each pin a finding names, and of the one
//! pin of a net that has only one (an open pin, as the status bar counts it): a dashed box and a
//! `!` triangle. At the top right of each part a finding names, and over the first wire piece of
//! each other net one names: the triangle alone, the badge beside the outline the canvas shader
//! draws round the item (its violation state).

use super::scene::PinHit;
use crate::canvas::{draw::{Common, LineInst}, Tokens};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{BoardId, Sel};
use std::collections::HashSet;

/// One dashed box and warning triangle per affected pin on the sheet, one triangle per affected
/// part (`parts`: its node and body `[x0, y0, x1, y1]`) and per other affected net (`wires`: each
/// piece's net and ends). O(findings + items + pins + parts + wires).
pub(super) fn erc_marks(
    pins: &[PinHit],
    parts: impl Iterator<Item = (u32, [f64; 4])>,
    frames: impl Iterator<Item = (u32, [f64; 4])>,
    wires: impl Iterator<Item = (Option<u32>, [f64; 2], [f64; 2])>,
    violations: &[Violation],
    unit: BoardId,
    tokens: &Tokens,
) -> Vec<LineInst> {
    let mut named = HashSet::new();
    let mut lone_nets = HashSet::new();
    let mut blocks = HashSet::new();
    let mut comps = HashSet::new();
    let mut nets = HashSet::new();
    for finding in violations.iter().filter(|v| v.unit == Some(unit) && v.at.is_none()) {
        for item in &finding.items {
            match item {
                Sel::Pin(pin) if pin.board == unit => { named.insert((pin.comp.0, u32::from(pin.pin.0))); }
                Sel::Net(net) if net.board == unit && finding.kind == CheckKind::SinglePinNet => { lone_nets.insert(net.net.0); }
                Sel::Net(net) if net.board == unit => { nets.insert(net.net.0); }
                Sel::Block(board, id) if *board == unit => { blocks.insert(id.0); }
                Sel::Comp(comp) if comp.board == unit => { comps.insert(comp.comp.0); }
                _ => {}
            }
        }
    }
    for pin in pins.iter().filter(|pin| pin.net.is_some_and(|net| lone_nets.contains(&net))) {
        named.insert((pin.node, pin.pin));
    }
    let mut marks = Vec::new();
    let c = Common::at([0.0; 3], tokens.warning);
    let mut line = |a: [f32; 2], b: [f32; 2], dash| marks.push(LineInst {
        c, seg: [a[0], a[1], b[0], b[1]], width: [1.0, 0.0], dash, ..LineInst::default()
    });
    for pin in pins {
        if named.is_empty() { break; }
        if !named.remove(&(pin.node, pin.pin)) { continue; }
        let [x, y] = pin.outer.map(|v| v as f32);
        let (left, right) = (x - 0.6, x + 0.6);
        let (top, bottom) = (y + 0.6, y - 0.6);
        let dashed = [3.0, 2.0];
        line([left, top], [right, top], dashed);
        line([right, top], [right, bottom], dashed);
        line([right, bottom], [left, bottom], dashed);
        line([left, bottom], [left, top], dashed);
        // The triangle's lower right vertex touches the box.
        triangle(&mut line, left, top);
    }
    for (node, body) in parts {
        if comps.is_empty() { break; }
        if !comps.remove(&node) { continue; }
        // its lower left vertex on the body's top right corner, as a pin's touches its box
        triangle(&mut line, body[2] as f32 + 1.0, body[3] as f32);
    }
    for (id, frame) in frames {
        if blocks.is_empty() { break; }
        if blocks.remove(&id) { triangle(&mut line, frame[2] as f32, frame[3] as f32); }
    }
    for (net, a, b) in wires {
        if nets.is_empty() { break; }
        if !net.is_some_and(|net| nets.remove(&net)) { continue; }
        // above and right of the piece's middle, clear of a level or an upright piece
        let mid = [(a[0] + b[0]) as f32 * 0.5, (a[1] + b[1]) as f32 * 0.5];
        triangle(&mut line, mid[0] + 1.3, mid[1] + 0.3);
    }
    marks
}

/// A `!` warning triangle one unit wide standing on `base`, its right corner at `right` (sheet y
/// increases upwards): its three sides, then the stroke and the dot of the mark.
fn triangle(line: &mut impl FnMut([f32; 2], [f32; 2], [f32; 2]), right: f32, base: f32) {
    let (left, mid) = (right - 1.0, right - 0.5);
    let tip = [mid, base + 0.8660254];
    let solid = [0.0; 2];
    line([left, base], tip, solid);
    line(tip, [right, base], solid);
    line([right, base], [left, base], solid);
    line([mid, base + 0.52], [mid, base + 0.29], solid);
    line([mid, base + 0.13], [mid, base + 0.11], solid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_circuit::CheckKind;
    use fab_elec_core::{CompId, CompRef, NetId, NetRef, PinIx, PinRef, Severity, P};

    #[test]
    fn circuit_findings_mark_each_placed_pin_part_and_net_once() {
        let unit = BoardId(1);
        let pins = [PinHit { node: 2, pin: 3, outer: [6.0, -4.0], inner: [7.0, -4.0], net: None, nc: false, open_end: false }];
        let pin = |board, index| Sel::Pin(PinRef { board, comp: CompId(2), pin: PinIx(index) });
        let finding = |kind, board, items| Violation::new(kind, Severity::Error, Some(board), items, "erc", String::new());
        let placed = finding(CheckKind::UnconnectedPin, unit, vec![pin(unit, 3)]);
        let tokens = Tokens::for_theme(false);
        let marks = |findings: &[Violation]| erc_marks(&pins, std::iter::empty(), std::iter::empty(), std::iter::empty(), findings, unit, tokens);
        let one = marks(std::slice::from_ref(&placed));
        assert_eq!(one.len(), 9, "a placed pin gets a box, a triangle and an exclamation mark");
        assert!(one.iter().all(|line| line.c.ink == tokens.warning), "every stroke uses warning ink");
        let square: Vec<_> = one.iter().filter(|line| line.dash[0] > 0.0 && line.dash[1] > 0.0).collect();
        assert_eq!(square.len(), 4, "the four sides of the box are dashed");
        for corner in [[5.4, -3.4], [6.6, -3.4], [6.6, -4.6], [5.4, -4.6]] {
            assert!(square.iter().any(|line| line.seg[..2] == corner || line.seg[2..] == corner),
                "the 1.2-unit box centred on the outer end must include {corner:?}");
        }
        let conflict = finding(CheckKind::PinConflict, unit, vec![pin(unit, 1), pin(unit, 3)]);
        assert_eq!(marks(std::slice::from_ref(&conflict)), one, "a finding marks a placed pin after an unplaced one");
        assert_eq!(marks(&[placed.clone(), conflict]), one, "two findings naming the same pin get one marker");
        let other = BoardId(2);
        assert!(marks(&[finding(CheckKind::PinConflict, other, vec![pin(unit, 3)])]).is_empty(),
            "another unit's finding leaves the sheet clear");
        assert!(marks(&[finding(CheckKind::PinConflict, unit, vec![pin(other, 3)])]).is_empty(),
            "a foreign pin with the same component and pin indices leaves the sheet clear");
        assert!(marks(&[finding(CheckKind::UnconnectedPin, unit, vec![pin(unit, 4)])]).is_empty(),
            "an unplaced pin has no marker");
        assert!(marks(&[placed.at(P::ZERO)]).is_empty(), "a board finding leaves the sheet clear");
        let lone = |net| finding(CheckKind::SinglePinNet, unit, vec![Sel::Net(NetRef { board: unit, net: NetId(net) })]);
        let on_net = [PinHit { net: Some(5), ..pins[0].clone() }];
        let on_net_marks = |findings: &[Violation]| erc_marks(&on_net, std::iter::empty(), std::iter::empty(), std::iter::empty(), findings, unit, tokens);
        assert_eq!(on_net_marks(&[lone(5)]), one, "a net with one pin marks that pin");
        assert!(on_net_marks(&[lone(6)]).is_empty(), "another net's single pin leaves this pin clear");

        // A small sheet: part 2's body, net 7 drawn as two pieces (a level, then an upright), net 8 as one.
        let parts = [(2, [0.0, 0.0, 3.0, 2.0])];
        let wires = [(Some(7), [0.0, 4.0], [4.0, 4.0]), (Some(7), [4.0, 4.0], [4.0, 7.0]), (Some(8), [10.0, 0.0], [12.0, 0.0])];
        let sheet = |findings: &[Violation]| erc_marks(&[], parts.iter().copied(), std::iter::once((2, [0., 0., 4., 6.])), wires.iter().copied(), findings, unit, tokens);
        let near = |a: &[f32], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5;
        let badge = |marks: &[LineInst], right: f32, base: f32| marks.len() == 5
            && marks.iter().all(|line| line.dash == [0.0; 2] && line.c.ink == tokens.warning)
            && marks.iter().any(|line| near(&line.seg[..2], [right, base]) && near(&line.seg[2..], [right - 1.0, base]));
        let net = |kind, net| finding(kind, unit, vec![Sel::Net(NetRef { board: unit, net: NetId(net) })]);
        assert_eq!(sheet(&[finding(CheckKind::OpenPort, unit, vec![Sel::Block(unit, fab_elec_core::BlockId(2))])]).len(), 5,
            "a finding on a placed block must draw its frame badge");
        let undriven = sheet(&[net(CheckKind::PowerNotDriven, 7)]);
        assert!(badge(&undriven, 2.0 + 1.3, 4.0 + 0.3),
            "a net finding badges the net's first wire piece once, above and right of its middle: {undriven:?}");
        assert!(sheet(&[net(CheckKind::SinglePinNet, 7)]).is_empty(), "a net with one pin badges its pin, never its wires");
        assert!(sheet(&[net(CheckKind::PowerNotDriven, 9)]).is_empty(), "a net with no piece on the sheet has no badge");
        let comp = |board| Sel::Comp(CompRef { board, comp: CompId(2) });
        let twice = sheet(&[finding(CheckKind::DuplicateRefdes, unit, vec![comp(unit)])]);
        assert!(badge(&twice, 4.0, 2.0), "a part finding badges the body's top right corner once: {twice:?}");
        assert!(sheet(&[finding(CheckKind::DuplicateRefdes, unit, vec![comp(BoardId(2))])]).is_empty(),
            "another unit's part leaves this sheet's part clear");
    }
}

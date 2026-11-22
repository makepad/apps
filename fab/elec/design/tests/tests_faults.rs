//! Failure paths on the controller fixtures: findings identify their subjects, rejected edits
//! preserve the board, and board checks agree with the router's clearance check.

use fab_elec_fixtures::{ledctl, ledctl_pair};
use fab_elec_design::{system_erc, PinMap};
use fab_elec_board::ops::{BoardOp, Field, Item, OpError};
use fab_elec_board::zone::fill_all;
use fab_elec_board::{check_pair, connect, drc, resolve_rules, Anchor, Board, ConnItem, Origin, RuleMatrix, Track, Via};
use fab_elec_circuit::{erc, CheckKind, Circuit, Measure, Violation};
use fab_elec_core::{clearance, mm, CompRef, FeatureRef, LayerId, NetRef, PadIx, Path, PinIx, PinRef, Sel, Severity, P};
use fab_elec_parts::{PartDef, VRange};
use std::sync::Arc;

fn pin(c: &Circuit, path: &str, name: &str) -> PinRef {
    let comp = c.comp(path).unwrap_or_else(|| panic!("fixture has component {path}"));
    let pin = comp.part.pin(name).unwrap_or_else(|| panic!("{path} has pin {name}"));
    PinRef { board: c.unit, comp: comp.id, pin }
}

fn one_kind(violations: &[Violation], kind: CheckKind) -> &Violation {
    let found: Vec<_> = violations.iter().filter(|v| v.kind == kind).collect();
    assert_eq!(found.len(), 1, "{kind:?} is reported exactly once: {violations:#?}");
    found[0]
}

fn names(v: &Violation, mut items: Vec<Sel>) {
    items.sort();
    assert_eq!(v.items, items, "{:?} names exactly the affected items", v.kind);
}

fn stable_erc(c: &Circuit, fault: &str) -> Vec<Violation> {
    let violations = erc(c);
    assert_eq!(violations, erc(c), "ERC ids and findings are stable for {fault}");
    violations
}

fn full_drc(b: &Board) -> (RuleMatrix, Vec<Violation>) {
    let rules = resolve_rules(b, None);
    let fills = fill_all(b, &rules);
    let conn = connect(b, &fills);
    let violations = drc(b, &conn, &rules, &fills, None).violations;
    (rules, violations)
}

#[test]
fn erc_reports_each_fault_once() {
    let d = ledctl();
    let original = (*d.units[0].circuit).clone();
    let pa3 = pin(&original, "mcu.u", "PA3");
    let r0 = original.comp("lamps[0].r").expect("fixture has resistor 0").id;
    let r1 = original.comp("lamps[1].r").expect("fixture has resistor 1").id;
    let vout = pin(&original, "reg.u", "VOUT");
    let usb_vbus = pin(&original, "usb.j", "VBUS");
    let vbus = original.net("VBUS").expect("fixture has VBUS").id;
    let v33 = original.net("3V3").expect("fixture has 3V3").id;

    let mut c = original.clone();
    let conns = &mut c.comps[pa3.comp.ix()].conns;
    let at = conns.iter().position(|(p, _)| *p == pa3.pin).expect("PA3 starts connected");
    conns.remove(at);
    let violations = stable_erc(&c, "unconnected PA3");
    names(one_kind(&violations, CheckKind::UnconnectedPin), vec![Sel::Pin(pa3)]);

    let mut c = original.clone();
    c.comps[r1.ix()].refdes = c.comps[r0.ix()].refdes.clone();
    let violations = stable_erc(&c, "duplicate resistor reference");
    names(
        one_kind(&violations, CheckKind::DuplicateRefdes),
        vec![Sel::Comp(CompRef { board: c.unit, comp: r0 }), Sel::Comp(CompRef { board: c.unit, comp: r1 })],
    );

    let mut c = original.clone();
    c.comps[vout.comp.ix()].conns.iter_mut().find(|(p, _)| *p == vout.pin).expect("VOUT starts connected").1 = vbus;
    let violations = stable_erc(&c, "VOUT moved from 3V3 to VBUS");
    // PinConflict selects the competing drivers, while PowerNotDriven selects the rail.
    names(one_kind(&violations, CheckKind::PinConflict), vec![Sel::Pin(usb_vbus), Sel::Pin(vout)]);
    names(one_kind(&violations, CheckKind::PowerNotDriven), vec![Sel::Net(NetRef { board: c.unit, net: v33 })]);

    let mut c = original.clone();
    let part = &mut c.comps[pa3.comp.ix()].part;
    *part = Arc::new(PartDef { footprint: None, ..(**part).clone() });
    let violations = stable_erc(&c, "MCU without a footprint");
    names(one_kind(&violations, CheckKind::MissingFootprint), vec![Sel::Comp(pa3.comp_ref())]);
}

#[test]
fn erc_follows_usb_power_through_series_parts() {
    use fab_elec_circuit::{Block, BlockSource, Component, Net, NetKind};
    use fab_elec_core::{BlockId, CompId, NetId};
    use fab_elec_design::fixture::parts;
    use fab_elec_parts::PinKind;

    let mut c = Circuit::default();
    c.tree.push(Block {
        id: BlockId(0), path: String::new(), def: None, parent: None, ports: vec![], params: vec![], source: BlockSource::Code,
    });
    let mut rail = |name: &str| {
        let id = NetId(c.nets.len() as u32);
        c.nets.push(Net { id, name: name.into(), class: Default::default(), kind: NetKind::Power, volts: None, scope: None });
        id
    };
    let vbus = rail("VBUS");
    let mid = rail("MID");
    let va = rail("VA");
    let mut add = |refdes: &str, part: Arc<PartDef>, connections: &[(&str, NetId)]| {
        let id = CompId(c.comps.len() as u32);
        let conns = connections.iter().map(|&(name, net)| (part.pin(name).expect("the part has this pin"), net)).collect();
        c.comps.push(Component {
            id, path: refdes.into(), refdes: refdes.into(), ref_kind: Default::default(), part, conns, nc: vec![], block: BlockId(0), dnp: false, value: None, fields: Default::default(),
        });
        id
    };

    let mut jack = parts::header(1, 2.54);
    let def = Arc::make_mut(&mut jack);
    def.pins[0].name = "vBus".into();
    def.fields.0.push(("sim".into(), "usb".into()));
    let jack = add("J1", jack, &[("vBus", vbus)]);
    let mut r = parts::resistor("0402");
    // An unspecified terminal supplies no signal drive of its own under the input rule.
    Arc::make_mut(&mut r).pins[1].kind = PinKind::Unspecified;
    // Visit the downstream resistor first so reaching VA needs a second pass.
    add("R2", r.clone(), &[("1", va), ("2", mid)]);
    add("R1", r, &[("1", vbus), ("2", mid)]);
    add("U1", parts::ldo_3v3(), &[("VIN", va), ("EN", mid)]);

    c.comps[jack.ix()].fields.0.push(("sim".into(), "passive".into()));
    let violations = erc(&c);
    let missing = one_kind(&violations, CheckKind::PowerNotDriven);
    let rail_item = Sel::Net(NetRef { board: c.unit, net: va });
    names(missing, vec![rail_item.clone()]);
    assert_eq!(missing.message, "VA: no driver", "an unsupplied rail names its missing driver");
    assert_eq!(missing.fix_hint, "connect a supply jack or a power output, or add a power flag", "the rail finding explains how to supply it");

    c.comps[jack.ix()].fields.0.clear();
    let violations = erc(&c);
    assert!(!violations.iter().any(|v| v.kind == CheckKind::PowerNotDriven), "the jack's VBUS supplies VA through both resistors");

    c.nets[mid.ix()].kind = NetKind::Signal;
    let violations = erc(&c);
    names(one_kind(&violations, CheckKind::PowerNotDriven), vec![rail_item]);
    names(one_kind(&violations, CheckKind::UndrivenInput), vec![Sel::Net(NetRef { board: c.unit, net: mid })]);
}

#[test]
fn drc_reports_each_board_fault() {
    let d = ledctl();
    let original = (**d.units[0].board.as_ref().expect("controller has a board")).clone();
    let r0 = original.placed_of(d.units[0].circuit.comp("lamps[0].r").expect("fixture has resistor 0").id)
        .expect("resistor 0 is placed");
    let r1 = original.placed_of(d.units[0].circuit.comp("lamps[1].r").expect("fixture has resistor 1").id)
        .expect("resistor 1 is placed");
    let track = original.tracks.iter().find(|t| t.ends[0].item() == Some(r0.id)).expect("LED track starts on resistor 0");

    let mut b = original.clone();
    b.apply(&BoardOp::SetField { item: track.id, field: Field::Width(mm(0.1)) }).expect("track width can be edited");
    let (rules, violations) = full_drc(&b);
    let width = one_kind(&violations, CheckKind::TrackWidth);
    names(width, vec![Sel::Item(b.unit, track.id)]);
    assert_eq!(width.actual, Some(Measure::Len(mm(0.1))), "TrackWidth reports the edited width");
    assert_eq!(width.required, Some(Measure::Len(rules.track_min(b.nets[track.net.ix()].class))), "TrackWidth reports the net class's minimum");

    // Guards: a track on a layer the stackup does not have is a finding of its own; the router's
    // collision checks skip such layers, so nothing else would show it.
    let mut b = original.clone();
    let gone = LayerId::inner(5);
    assert!(!b.stackup.copper_set().has(gone), "the fixture's stackup has no In5.Cu");
    b.apply(&BoardOp::SetField { item: track.id, field: Field::Layer(gone) }).expect("a track can be put on any copper layer");
    let (_, violations) = full_drc(&b);
    let missing = one_kind(&violations, CheckKind::MissingLayer);
    names(missing, vec![Sel::Item(b.unit, track.id)]);
    assert_eq!(missing.message, "track on In5.Cu, a copper layer the stackup does not have", "MissingLayer names the layer");

    let mut b = original.clone();
    // This via's 0.075 mm ring meets the default minimum; require 0.1 mm to make it a fault.
    let mut via_rules = b.rules.clone();
    via_rules.base.annular = Some(mm(0.1));
    b.apply(&BoardOp::SetRules(via_rules)).expect("the annular minimum can be raised");
    let via = Via {
        id: b.alloc_id(),
        net: b.net("GND").expect("fixture has GND"),
        at: P::mm(40.0, 16.0),
        drill: mm(0.15),
        pad: mm(0.3),
        ..original.vias[0].clone()
    };
    b.apply(&BoardOp::Add(Item::Via(via.clone()))).expect("the undersized via can be added for DRC");
    let (rules, violations) = full_drc(&b);
    let drill = one_kind(&violations, CheckKind::Drill);
    names(drill, vec![Sel::Item(b.unit, via.id)]);
    assert_eq!(drill.actual, Some(Measure::Len(mm(0.15))), "Drill reports the via's drill diameter");
    assert_eq!(drill.required, Some(Measure::Len(rules.drill_min())), "Drill reports the minimum drill diameter");
    let ring = one_kind(&violations, CheckKind::AnnularRing);
    names(ring, vec![Sel::Item(b.unit, via.id)]);
    assert_eq!(ring.actual, Some(Measure::Len(mm(0.075))), "AnnularRing reports the radial copper width");
    assert_eq!(ring.required, Some(Measure::Len(rules.annular_min())), "AnnularRing reports the raised minimum");

    let mut b = original.clone();
    b.apply(&BoardOp::Move { item: r0.id, by: P::mm(1.9, 0.0) }).expect("resistor 0 can move towards resistor 1");
    let (rules, violations) = full_drc(&b);
    let copper = b.all_copper();
    // The 3V3 pads may touch legally; choose the closest pair on different nets and one layer.
    let (a, other, gap) = copper.iter()
        .filter(|a| matches!(a.item, ConnItem::Pad { placed, .. } if placed == r0.id))
        .flat_map(|a| {
            copper.iter()
                .filter(move |other| matches!(other.item, ConnItem::Pad { placed, .. } if placed == r1.id)
                    && a.layer == other.layer && a.net != other.net)
                .map(move |other| (a, other, clearance(&a.shape, &other.shape)))
        })
        .min_by_key(|(_, _, gap)| *gap)
        .expect("the two resistors have pad copper on different nets on a common layer");
    let mut items: Vec<_> = [a, other].iter().map(|c| match c.item {
        ConnItem::Pad { placed, pad } => {
            let comp = b.placed_item(placed).expect("pad copper belongs to a placement").comp;
            Sel::Feature(FeatureRef::Pad(CompRef { board: b.unit, comp }, pad))
        }
        _ => panic!("the clearance candidates are pads"),
    }).collect();
    items.sort();
    let findings: Vec<_> = violations.iter().filter(|v| v.kind == CheckKind::Clearance && v.items == items).collect();
    assert_eq!(findings.len(), 1, "Clearance names the closest pads of both resistors exactly once: {violations:#?}");
    let v = findings[0];
    let (Some(Measure::Len(actual)), Some(Measure::Len(required))) = (v.actual, v.required) else {
        panic!("Clearance reports actual and required lengths: {v:?}");
    };
    assert!(actual < required, "the resistor pad gap is below the required clearance");
    assert_eq!(actual, gap, "DRC measures the closest resistor pad copper gap");
    let pair = check_pair(&rules, a, other, a.layer).expect("the router rejects the same resistor pad clearance");
    assert_eq!(pair.gap, actual, "the router and DRC measure the same gap");
    assert_eq!(pair.need.gap, required, "the router and DRC require the same clearance");

    let mut b = original.clone();
    let edge_track = Track {
        id: b.alloc_id(),
        net: b.net("BTN").expect("fixture has BTN"),
        layer: LayerId::F_CU,
        w: mm(0.2),
        widths: None,
        path: Path::new(P::mm(49.9, 2.0)).line(P::mm(49.9, 6.0)),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    b.apply(&BoardOp::Add(Item::Track(edge_track.clone()))).expect("the edge track can be added for DRC");
    let (rules, violations) = full_drc(&b);
    let edge = one_kind(&violations, CheckKind::EdgeClearance);
    names(edge, vec![Sel::Item(b.unit, edge_track.id)]);
    assert_eq!(edge.actual, Some(Measure::Len(0)), "the track copper touches the board edge");
    assert_eq!(edge.required, Some(Measure::Len(rules.edge())), "EdgeClearance reports the edge rule's minimum");
    let dangling: Vec<_> = violations.iter().filter(|v| v.kind == CheckKind::Dangling).collect();
    assert_eq!(dangling.len(), 2, "both free track ends have a dangling finding");
    for v in &dangling {
        names(v, vec![Sel::Item(b.unit, edge_track.id)]);
        assert_eq!(v.severity, Severity::Warning, "a free track end is a warning");
    }
    let mut ends: Vec<_> = dangling.iter().map(|v| v.at).collect();
    ends.sort();
    let mut expected = vec![Some(edge_track.path.start), Some(edge_track.path.end())];
    expected.sort();
    assert_eq!(ends, expected, "Dangling reports one finding at each end, not two at one end");
    assert_ne!(dangling[0].id, dangling[1].id, "the two dangling ends have distinct violation ids");
}

#[test]
fn op_edit_rules() {
    let d = ledctl();
    let original = (**d.units[0].board.as_ref().expect("controller has a board")).clone();
    let comp = d.units[0].circuit.comp("lamps[0].r").expect("fixture has resistor 0");
    let placed = original.placed_of(comp.id).expect("resistor 0 is placed");
    let track = original.tracks.iter().find(|t| t.ends[0].item() == Some(placed.id)).expect("LED track starts on resistor 0");
    let mut b = original.clone();

    let error = b.apply(&BoardOp::Add(Item::Track(track.clone()))).expect_err("adding an existing track id fails");
    assert_eq!(error, OpError::DuplicateId(track.id), "Add reports the duplicate item id");
    assert_eq!(b, original, "a duplicate Add leaves the entire board unchanged");

    let wrong_kind = Via { id: track.id, ..original.vias[0].clone() };
    let error = b.apply(&BoardOp::Replace(Item::Via(wrong_kind))).expect_err("a via cannot replace a track");
    assert_eq!(error, OpError::KindMismatch(track.id), "Replace rejects a change of item kind");
    assert_eq!(b, original, "a kind mismatch leaves the entire board unchanged");

    let bad_field = BoardOp::SetField { item: track.id, field: Field::Drill(mm(0.3)) };
    let error = b.apply(&bad_field).expect_err("a track has no drill field");
    assert_eq!(error, OpError::BadField { item: track.id, field: "drill" }, "SetField identifies the unsupported track field");
    assert_eq!(b, original, "a bad field leaves the entire board unchanged");

    let mut bad_anchor = track.clone();
    bad_anchor.id = b.alloc_id();
    bad_anchor.ends[0] = Anchor::Pad { placed: placed.id, pad: PadIx(placed.footprint.pads.len() as u16) };
    let before = b.clone();
    let error = b.apply(&BoardOp::Add(Item::Track(bad_anchor.clone()))).expect_err("a track cannot anchor to a nonexistent pad");
    assert_eq!(error, OpError::BadAnchor(bad_anchor.id), "Add rejects a missing pad index on an existing placement");
    assert_eq!(b, before, "a bad anchor leaves the entire board unchanged");

    let block_via = Via {
        id: b.alloc_id(),
        origin: Origin::Block { block: comp.block, detached: false },
        ..original.vias[0].clone()
    };
    b.apply_host(&BoardOp::Add(Item::Via(block_via.clone()))).expect("the host can instantiate a block item");
    let before = b.clone();
    let error = b.apply(&BoardOp::Move { item: block_via.id, by: P::mm(1.0, 0.0) }).expect_err("an undetached block item cannot move alone");
    assert_eq!(error, OpError::BlockItem { item: block_via.id, block: comp.block }, "Move identifies the item and block that must be detached");
    assert_eq!(b, before, "a rejected block item move leaves the entire board unchanged");

    let anchored: Vec<_> = b.tracks.iter().flat_map(|t| {
        t.ends.iter().enumerate().filter_map(move |(end, anchor)| (anchor.item() == Some(placed.id)).then_some((t.id, end)))
    }).collect();
    assert!(!anchored.is_empty(), "the removed part has anchored track ends to exercise");
    let before_remove = b.clone();
    let removed = b.apply(&BoardOp::Remove(placed.id)).expect("an authored placement can be removed");
    assert!(b.placed_item(placed.id).is_none(), "Remove removes the placement");
    for (id, end) in anchored {
        let t = b.tracks.iter().find(|t| t.id == id).expect("removing a placement preserves its tracks");
        assert_eq!(t.ends[end], Anchor::Free, "Remove frees track {id:?} end {end} formerly anchored to the part");
    }
    b.undo(&removed.inverse).expect("the placement removal can be undone");
    assert_eq!(b.placed, before_remove.placed, "undo restores the placement in its original order");
    assert_eq!(b.tracks, before_remove.tracks, "undo restores every track anchor and its geometry");

    // Keep the complete equality guard: a failed batch must also roll back generation.
    let mut b = original.clone();
    let error = b.apply_all(&[
        BoardOp::Move { item: placed.id, by: P::mm(0.3, -0.2) },
        bad_field,
    ]).expect_err("the second op rejects the batch after a valid move");
    assert_eq!(error, OpError::BadField { item: track.id, field: "drill" }, "apply_all returns the failing op's error");
    assert_eq!(b, original, "a failed batch restores the entire board, including moved anchors and generation");
}

#[test]
fn system_erc_reports_link_faults() {
    let original = ledctl_pair();
    let probe = original.units[1].circuit.comp("j").expect("probe has a header");
    let main_pin = original.units[0].circuit.comp("mcu.dbg").expect("controller has a debug header")
        .part.pin("1").expect("debug header has pin 1");
    let mut bad_map = original.clone();
    bad_map.links[0].pinmap = PinMap::Given(vec![(main_pin, PinIx(probe.part.pins.len() as u16))]);
    let map_violations = system_erc(&bad_map);

    let mut bad_volts = original.clone();
    let main_rail = NetRef {
        board: original.units[0].id,
        net: original.units[0].circuit.net("3V3").expect("controller has 3V3").id,
    };
    let probe_rail = NetRef {
        board: original.units[1].id,
        net: original.units[1].circuit.net("3V3").expect("probe has 3V3").id,
    };
    Arc::make_mut(&mut bad_volts.units[1].circuit).nets[probe_rail.net.ix()].volts = Some(VRange { min: 5.0, max: 5.0 });
    let violations = system_erc(&bad_volts);
    names(one_kind(&violations, CheckKind::SystemVolts), vec![Sel::Net(main_rail), Sel::Net(probe_rail)]);

    // Explicit maps must reject missing pins too; current system ERC only checks derived maps.
    names(one_kind(&map_violations, CheckKind::LinkPinMap), vec![Sel::Link(bad_map.links[0].id)]);
}

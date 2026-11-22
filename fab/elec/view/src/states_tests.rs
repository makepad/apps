use crate::{board_texels, sheet_texels, ItemStates, ViewState};
use fab_elec_board::Anchor;
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{BoardId, CompId, CompRef, FeatureRef, NetRef, PadIx, PinIx, PinRef, Sel, Severity, P};
use fab_elec_fixtures::ledctl;
use fab_elec_explore::rows::SheetRows;
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn board_item_states_follow_selection_findings_and_emphasis() {
    let mut design = ledctl();
    let unit = &mut design.units[0];
    let id = unit.id;
    let gnd = unit.circuit.net("GND").expect("fixture has GND").id;
    let board = Arc::make_mut(unit.board.as_mut().expect("fixture unit 0 has a board"));
    assert_eq!(board.nets[gnd.ix()].name, "GND", "net identity: the circuit's GND id names board GND");
    // The fixture's existing tracks are LED nets; include a GND track so its coverage is real.
    let mut ground_track = board.tracks[0].clone();
    ground_track.id = board.alloc_id();
    ground_track.net = gnd;
    ground_track.ends = [Anchor::Free; 2];
    board.tracks.push(ground_track);
    board.reindex();
    let via = board.vias[0].id;
    let track = board.tracks[0].id;
    let hole = board.holes[0].id;
    let part = &board.placed[0];
    let comp = CompRef { board: id, comp: part.comp };
    let mut view = ViewState { selected: vec![Sel::Item(id, via)], emphasis: vec![] };

    assert_eq!(board_texels(id, board, &[], &view), vec![(via.0, [1.0, 0.0, 0.0, 0.0])], "selection: only the selected via lights x at its item row");
    for sel in [
        Sel::Comp(comp),
        Sel::Pin(PinRef { board: id, comp: comp.comp, pin: PinIx(0) }),
        Sel::Feature(FeatureRef::Part(comp)),
        Sel::Feature(FeatureRef::Pad(comp, PadIx(0))),
        Sel::Feature(FeatureRef::Body(comp)),
        Sel::Feature(FeatureRef::Port(comp, 0)),
    ] {
        let selected = ViewState { selected: vec![sel.clone()], emphasis: vec![] };
        assert_eq!(board_texels(id, board, &[], &selected), vec![(part.id.0, [1.0, 0.0, 0.0, 0.0])], "placement mapping: {sel:?} lights only its placement's row");
    }

    let violations = vec![
        Violation::new(CheckKind::TrackWidth, Severity::Warning, Some(id), vec![Sel::Item(id, track)], "width", String::new()),
        Violation::new(CheckKind::Clearance, Severity::Error, Some(id), vec![Sel::Item(id, track)], "clearance", String::new()),
        Violation::new(CheckKind::Drill, Severity::Info, Some(id), vec![Sel::Feature(FeatureRef::Hole(id, hole))], "drill", String::new()),
        Violation::new(CheckKind::UnconnectedPin, Severity::Info, None, vec![Sel::Comp(comp)], "circuit", String::new()),
        Violation::new(CheckKind::Mech, Severity::Warning, Some(BoardId(1)), vec![Sel::Feature(FeatureRef::Port(comp, 0))], "case", String::new()),
    ];
    let findings: HashMap<_, _> = board_texels(id, board, &violations, &view).into_iter().collect();
    assert_eq!(findings[&track.0][2], 3.0, "findings: Error wins over Warning on the same track");
    assert_eq!(findings[&hole.0][2], 1.0, "findings: an Info on a hole gives severity 1");
    assert_eq!(findings[&part.id.0][2], 2.0, "findings: a case finding from another unit still lights the named placement");
    let circuit = board_texels(id, board, &violations[3..4], &ViewState::default());
    assert_eq!(circuit, vec![(part.id.0, [0.0, 0.0, 1.0, 0.0])], "findings: a circuit-only component finding lights its placement");

    let net = Sel::Net(NetRef { board: id, net: gnd });
    view.emphasis = vec![
        (net.clone(), 0.4), (net.clone(), 2.0), (net, 0.2),
        (Sel::Item(id, track), -1.0), (Sel::Item(id, board.holes[1].id), -0.5),
    ];
    let texels = board_texels(id, board, &violations, &view);
    let values: HashMap<_, _> = texels.iter().copied().collect();
    let emphasis = |row: u32| values.get(&row).map_or(0.0, |texel| texel[3]);
    for item in &board.tracks {
        assert_eq!(emphasis(item.id.0), if item.net == gnd { 1.0 } else { 0.0 }, "net emphasis: every GND track is lit and other tracks are off");
    }
    for item in &board.vias {
        assert_eq!(emphasis(item.id.0), if item.net == gnd { 1.0 } else { 0.0 }, "net emphasis: every GND via is lit");
    }
    for item in &board.zones {
        assert_eq!(emphasis(item.id.0), if item.net == Some(gnd) { 1.0 } else { 0.0 }, "net emphasis: every GND zone is lit");
    }
    for item in &board.placed {
        assert_eq!(emphasis(item.id.0), if item.pad_nets.contains(&Some(gnd)) { 1.0 } else { 0.0 }, "net emphasis: every placement with a GND pad is lit");
    }
    assert_ne!(board.tracks[0].net, gnd, "net emphasis: the fixture supplies an item of another net");
    assert_eq!(emphasis(track.0), 0.0, "net emphasis: an item of another net stays off and negative emphasis clamps to zero");
    assert_eq!(values[&via.0], [1.0, 0.0, 0.0, 1.0], "channels: selection and clamped maximum net emphasis share the via texel");
    assert!(!values.contains_key(&board.holes[1].id.0), "sparse rows: a clamped zero emphasis leaves no texel");
    assert!(texels.windows(2).all(|pair| pair[0].0 < pair[1].0), "sparse rows: output is sorted and unique");
    assert!(texels.iter().all(|(_, texel)| texel[1] == 0.0 && *texel != [0.0; 4]), "sparse rows: ghosts are off and all-zero texels are omitted");

    let other = BoardId(1);
    let other_comp = CompRef { board: other, comp: comp.comp };
    for sel in [
        Sel::Item(other, via), Sel::Comp(other_comp),
        Sel::Pin(PinRef { board: other, comp: comp.comp, pin: PinIx(0) }),
        Sel::Net(NetRef { board: other, net: gnd }),
        Sel::Feature(FeatureRef::Hole(other, hole)),
        Sel::Feature(FeatureRef::Pad(other_comp, PadIx(0))),
        Sel::Feature(FeatureRef::Port(other_comp, 0)),
    ] {
        let foreign = ViewState { selected: vec![sel.clone()], emphasis: vec![(sel.clone(), 1.0)] };
        assert!(board_texels(id, board, &[], &foreign).is_empty(), "unit isolation: {sel:?} from another unit lights nothing");
    }

    let mut states = ItemStates::new();
    let count = board.next_item as usize;
    states.write(count, texels.clone());
    assert!(states.take_dirty(), "dirty state: the initial write changes texels");
    let kept = (via.0, values[&via.0]);
    states.write(count, vec![kept]);
    assert!(states.take_dirty(), "dirty state: removing lit rows marks a change");
    for &(row, texel) in &texels {
        assert_eq!(states.row(row as usize), if row == via.0 { texel } else { [0.0; 4] }, "replacement: dropped rows clear and the kept row retains its texel");
    }
    states.write(count, vec![kept, (count as u32, [1.0; 4]), (count as u32 + 5, [1.0; 4])]);
    assert_eq!(states.row(count), [0.0; 4], "bounds: a row equal to count is ignored");
    assert_eq!(states.row(count + 5), [0.0; 4], "bounds: a row larger than count is ignored");
    assert!(!states.take_dirty(), "dirty state: ignored rows do not change the kept set");
    states.write(count + crate::live::LIVE_ROW_W, vec![kept]);
    assert_eq!(states.row(via.ix()), kept.1, "growth: a larger count keeps existing values");
    assert_eq!(states.row(count), [0.0; 4], "growth: ignored rows do not reappear when the count grows");
    assert!(states.take_dirty(), "dirty state: growth schedules the larger texture");
    states.write(count + crate::live::LIVE_ROW_W, vec![kept]);
    assert!(!states.take_dirty(), "dirty state: rewriting the same set at the same count stays clean");
    states.write(0, vec![kept]);
    assert!(states.take_dirty(), "dirty state: shrinking away a lit row marks a change");
    assert_eq!(states.row(via.ix()), [0.0; 4], "shrink: a dropped row reads zero");
    states.write(count, vec![]);
    assert_eq!(states.row(via.ix()), [0.0; 4], "regrowth: a dropped row does not resurrect");

    // An outside edit's flash: w below zero at its strength, the strongest of a row kept, under
    // a selection; an emphasised row keeps its emphasis.
    let lit = vec![(via.0, [1.0, 0.0, 0.0, 0.0]), (track.0, [0.0, 0.0, 0.0, 0.7])];
    let flashed = crate::with_flash(lit.clone(), &[(via.0, 0.5), (via.0, 0.8), (track.0, 1.0), (hole.0, 0.25), (hole.0 + 1000, 0.0)]);
    let mut want = vec![(via.0, [1.0, 0.0, 0.0, -0.8]), (track.0, [0.0, 0.0, 0.0, 0.7]), (hole.0, [0.0, 0.0, 0.0, -0.25])];
    want.sort_unstable_by_key(|(row, _)| *row);
    assert_eq!(flashed, want, "flash: below-zero w at the strongest flash of a row, emphasis kept, a faded flash lights nothing");
    assert_eq!(crate::with_flash(lit.clone(), &[]), lit, "flash: none leaves the texels as they were");
}

#[test]
fn sheet_item_states_follow_rows_across_edits() {
    let design = ledctl();
    let unit = &design.units[0];
    let id = unit.id;
    let circuit = &unit.circuit;
    let board = unit.board.as_deref().expect("fixture unit 0 has a board");
    let mut rows = SheetRows::new();
    let index = rows.sync(circuit);
    let count = rows.count();
    let comp = circuit.comp("mcu.boot_r").expect("fixture has a boot resistor");
    let part = board.placed_of(comp.id).expect("fixture places the boot resistor");
    let selected = CompRef { board: id, comp: comp.id };
    let row = index.comp(comp.id.0).expect("the component has a sheet row");
    for sel in [
        Sel::Comp(selected),
        Sel::Pin(PinRef { board: id, comp: comp.id, pin: comp.conns[0].0 }),
        Sel::Feature(FeatureRef::Part(selected)),
        Sel::Feature(FeatureRef::Pad(selected, PadIx(0))),
        Sel::Feature(FeatureRef::Body(selected)),
        Sel::Feature(FeatureRef::Port(selected, 0)),
    ] {
        let view = ViewState { selected: vec![sel.clone()], emphasis: vec![] };
        assert_eq!(sheet_texels(id, circuit, Some(board), &index, &[], &view), vec![(row, [1.0, 0.0, 0.0, 0.0])], "component mapping: {sel:?} selects the component's sheet row");
    }
    let block = ViewState { selected: vec![Sel::Block(id, comp.block)], emphasis: vec![] };
    assert_eq!(sheet_texels(id, circuit, Some(board), &index, &[], &block), vec![(index.block(comp.block.0).unwrap(), [1.0, 0.0, 0.0, 0.0])], "block mapping: only the named block's row lights");
    let foreign = ViewState { selected: vec![Sel::Comp(CompRef { board: BoardId(id.0 + 1), comp: comp.id })], emphasis: vec![] };
    assert!(sheet_texels(id, circuit, Some(board), &index, &[], &foreign).is_empty(), "unit isolation: a component of another unit lights nothing");

    let net = circuit.net("GND").expect("fixture has GND").id;
    let view = ViewState { selected: vec![], emphasis: vec![(Sel::Net(NetRef { board: id, net }), 0.7)] };
    let texels = sheet_texels(id, circuit, Some(board), &index, &[], &view);
    let lit: HashMap<_, _> = texels.iter().copied().collect();
    let emphasis = |row| lit.get(&row).copied().unwrap_or([0.0; 4]);
    assert_eq!(emphasis(index.net(net.0).unwrap()), [0.0, 0.0, 0.0, 0.7], "net emphasis: the net's own row lights");
    assert!(circuit.comps.iter().any(|c| c.conns.iter().all(|(_, n)| *n != net)), "net emphasis coverage: the fixture includes a component off this net");
    for c in &circuit.comps {
        let connected = c.conns.iter().any(|(_, n)| *n == net);
        assert_eq!(emphasis(index.comp(c.id.0).unwrap()), if connected { [0.0, 0.0, 0.0, 0.7] } else { [0.0; 4] }, "net emphasis: {} lights exactly when it connects to the net", c.path);
    }
    assert!(texels.windows(2).all(|pair| pair[0].0 < pair[1].0), "sparse sheet rows: output is sorted and unique");

    let placement = Violation::new(CheckKind::Mech, Severity::Warning, Some(id), vec![Sel::Item(id, part.id)], "placement", String::new());
    let track = Violation::new(CheckKind::TrackWidth, Severity::Error, Some(id), vec![Sel::Item(id, board.tracks[0].id)], "track", String::new());
    assert_eq!(sheet_texels(id, circuit, Some(board), &index, &[placement.clone()], &ViewState::default()), vec![(row, [0.0, 0.0, 2.0, 0.0])], "placement finding: its component's row receives the finding's severity");
    assert!(sheet_texels(id, circuit, None, &index, &[placement], &ViewState::default()).is_empty(), "placement finding: a sheet without a board has no placement mapping");
    assert!(sheet_texels(id, circuit, Some(board), &index, &[track], &ViewState::default()).is_empty(), "track finding: no component or net row lights");

    let all = |c: &fab_elec_circuit::Circuit| ViewState {
        selected: c.comps.iter().map(|c| Sel::Comp(CompRef { board: id, comp: c.id })).collect(),
        emphasis: vec![],
    };
    let mut states = ItemStates::new();
    states.write(count as usize, sheet_texels(id, circuit, None, &index, &[], &all(circuit)));
    assert_eq!(states.row(row as usize), [1.0, 0.0, 0.0, 0.0], "removal setup: the soon-deleted component starts lit");
    let before: HashMap<_, _> = circuit.comps.iter().map(|c| (c.path.clone(), index.comp(c.id.0).unwrap())).collect();
    let mut edited = (**circuit).clone();
    edited.comps.remove(comp.id.ix());
    for (i, c) in edited.comps.iter_mut().enumerate() {
        c.id = CompId(i as u32);
    }
    // Layout intents are irrelevant to this row-only edit and carry old component indices.
    edited.intents.clear();
    let after = rows.sync(&edited);
    states.write(rows.count() as usize, sheet_texels(id, &edited, None, &after, &[], &all(&edited)));
    assert_eq!(states.row(row as usize), [0.0; 4], "removed row: rewriting clears the deleted component's texel");
    for c in &edited.comps {
        let old_row = before[&c.path];
        assert_eq!(after.comp(c.id.0), Some(old_row), "stable rows: {} retains its row after component indices shift", c.path);
        assert_eq!(states.row(old_row as usize), [1.0, 0.0, 0.0, 0.0], "stable texels: {} stays selected on its original row", c.path);
    }
    assert_eq!(rows.count(), count, "count retention: deleting a component does not shrink the sheet texture");
}

#[test]
fn the_sheet_shows_circuit_findings_and_board_findings_stay_on_the_board() {
    let design = ledctl();
    let unit = &design.units[0];
    let id = unit.id;
    let circuit = &unit.circuit;
    let board = unit.board.as_deref().expect("fixture unit 0 has a board");
    let index = SheetRows::new().sync(circuit);
    let comp = circuit.comp("mcu.boot_r").expect("fixture has a boot resistor");
    let part = board.placed_of(comp.id).expect("fixture places the boot resistor");
    let selected = CompRef { board: id, comp: comp.id };
    let row = index.comp(comp.id.0).expect("the component has a sheet row");
    let texel = |texels: Vec<(u32, [f32; 4])>, at: u32| texels.into_iter().find(|(r, _)| *r == at).map_or([0.0; 4], |(_, t)| t);
    // An unrouted connection: a board check's finding, with its place on the board.
    let pads = vec![Sel::Feature(FeatureRef::Pad(selected, PadIx(0))), Sel::Comp(selected)];
    let unrouted = Violation::new(CheckKind::Unconnected, Severity::Error, Some(id), pads, "unconnected", String::new()).at(P::new(0, 0));
    let quiet = ViewState::default();
    assert!(texel(board_texels(id, board, &[unrouted.clone()], &quiet), part.id.0)[2] == 3.0 && texel(sheet_texels(id, circuit, Some(board), &index, &[unrouted.clone()], &quiet), row)[2] == 0.0,
        "board finding: an unrouted connection lights its part on the board and leaves its sheet row clear");
    let erc = Violation::new(CheckKind::UnconnectedPin, Severity::Warning, Some(id), vec![Sel::Comp(selected)], "circuit", String::new());
    assert_eq!(texel(sheet_texels(id, circuit, Some(board), &index, &[erc], &quiet), row), [0.0, 0.0, 2.0, 0.0],
        "circuit finding: a finding without a board place lights the component's sheet row");
    let focused = ViewState { selected: vec![], emphasis: unrouted.items.iter().map(|sel| (sel.clone(), 1.0)).collect() };
    assert_eq!(texel(sheet_texels(id, circuit, Some(board), &index, &[unrouted], &focused), row), [0.0, 0.0, 0.0, 1.0],
        "focused board finding: its emphasis still reaches the sheet row (cross-probe), without the finding itself");
}

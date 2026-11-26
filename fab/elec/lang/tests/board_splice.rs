use fab_elec_board::{Anchor, BoardOp, CustomCheck, CustomRule, Field, Item, LabelPose, Origin, RuleCode, RuleSet, Text, TuneSide, TuneStyle, TuneTarget, Tuning, Waiver};
use fab_elec_core::{Angle, ContentHash, Contour, DocId, FeatureRef, ItemId, LayerId, Sel, Severity, Side, P};
use fab_elec_design::{DesignOp, TextEdit};
use fab_elec_parts::FpTextKind;
use fab_elec_lang::{EvalError, NO_CANCEL, board_doc::{self, Ledger}, board_splice::{from_spans, splice_board}, evaluate, evaluate_from, source::{Sources, LIB_DOC}, splice::SpanEdit, splice_verified, std_libs, write};

#[test]
fn junction_remap_preserves_the_attached_statement_verbatim() {
    use fab_elec_core::Path;
    let fixture = fab_elec_fixtures::ledctl();
    let circuit = &fixture.units[0].circuit;
    let host = "track(\"GND\", \"F.Cu\", 0.2mm, [[1mm 1mm] [5mm 1mm]])";
    let branch = "track( \"GND\", // this junction stays here\n  \"F.Cu\",  0.20mm, [[5mm 1mm] [6mm 1mm]] )";
    let text = format!("board(\"ledctl\")\n{host}\n{branch}\n");
    let mut sources = Sources::new();
    let doc = sources.add("ledctl.pcb.splash", text).unwrap();
    let lowered = board_doc::lower(doc, &sources.doc(doc).unwrap().text, circuit, None).unwrap();
    let map = from_spans(doc, &lowered.spans);
    let board = lowered.board;
    let mut changed = board.tracks[0].clone();
    let mut attached = board.tracks[1].clone();
    let end = changed.path.end();
    changed.path = Path::new(changed.path.start).line(P::mm(3.0, 3.0)).line(end);
    attached.ends[0] = Anchor::Track { id: changed.id, at: fab_elec_board::ops::on_track(end, &changed.path).unwrap() };
    assert_ne!(attached.ends, board.tracks[1].ends);
    let ops = [changed, attached].map(|t| BoardOp::Replace(Item::Track(t)));
    let splice = splice_board(&sources, &map, &board, circuit, &ops, None).unwrap();
    assert_eq!(splice.edits.len(), 1);
    sources.apply(&splice.edits).unwrap();
    let written = &sources.doc(doc).unwrap().text;
    assert!(written.contains(branch));
    assert!(!written.contains(host));
    let reloaded = board_doc::lower(doc, written, circuit, None).unwrap().board;
    assert_eq!(reloaded.tracks[0].path.segs.len(), 2);
    assert_eq!(reloaded.anchor_pos(reloaded.tracks[1].ends[0]), Some(end));
}

#[test]
fn board_commits_preserve_text_and_follow_item_spans() {
    let fixture = fab_elec_fixtures::ledctl();
    let circuit = &fixture.units[0].circuit;
    let fixture_board = fixture.units[0].board.as_ref().expect("fixture has a board");
    let canonical = write(fixture_board, circuit);
    let mut placed = fixture_board.placed.last().expect("fixture has placements").clone();
    let omitted = format!("place(\"{}\",", placed.path);
    let original_hole = canonical.lines().filter(|s| s.starts_with("hole(")).last().expect("fixture has holes");
    let untouched = original_hole.replacen(", ", ",   ", 1);
    let mut document = String::new();
    let mut track_index = 0;
    for line in canonical.lines() {
        // Leave one circuit component unplaced for Add and Insert.
        if line.starts_with(&omitted) { continue; }
        if line.starts_with("track(") {
            if track_index == 2 { document.push_str("let t0 = "); }
            if track_index == 3 { document.push_str("let branch = "); }
            track_index += 1;
        }
        if line == original_hole {
            document.push_str(&untouched);
        } else {
            document.push_str(line);
        }
        document.push('\n');
    }
    document.push_str("text(\"label\", \"F.SilkS\", [2mm, 8mm], {size: 1mm thick: 0.15mm})\n");
    let mut sources = Sources::new();
    let doc = sources.add("ledctl.pcb.splash", document).expect("board document is added");
    let lowered = board_doc::lower(doc, &sources.doc(doc).expect("document exists").text, circuit, None).expect("edited fixture lowers");
    let map = from_spans(doc, &lowered.spans);
    let before = lowered.board;
    let id = ItemId(before.next_item);
    placed.id = id;
    let mut track = before.tracks[0].clone();
    track.id = id;
    track.path = fab_elec_core::Path::new(P::mm(2.0, 2.0)).line(P::mm(3.0, 2.0));
    track.ends = [Anchor::Free; 2];
    let mut via = before.vias[0].clone();
    via.id = id;
    via.at = P::mm(2.0, 4.0);
    let mut zone = before.zones[0].clone();
    zone.id = id;
    zone.name = "extra area".into();
    zone.priority = 1;
    let mut hole = before.holes[0].clone();
    hole.id = id;
    hole.name = "extra mount".into();
    hole.at = P::mm(2.0, 6.0);
    let text = Text {
        id, layer: LayerId::F_SILK, text: "added µ".into(), at: P::mm(2.0, 10.0), rot: Angle::ZERO,
        size: 1_000_000, thick: 150_000, mirrored: false, locked: false, origin: Origin::Authored,
    };
    let tuning = Tuning {
        id, net: before.tracks[0].net, pair: None, host: vec![before.tracks[0].id],
        target: TuneTarget::Len(10_000_000), side: TuneSide::Both, amp: (200_000, 800_000),
        pitch: 500_000, style: TuneStyle::Organic, copper: vec![before.tracks[1].id], origin: Origin::Authored,
    };
    let mut bound_tuning = tuning.clone();
    bound_tuning.host = vec![before.tracks[3].id];
    bound_tuning.copper = vec![before.tracks[2].id];
    let mut replacement = before.vias[0].clone();
    replacement.pad += 100_000;
    let mut stackup = before.stackup.clone();
    stackup.thickness += 200_000;
    let mut rules = before.rules.clone();
    rules.base.clearance = Some(200_000);
    rules.waivers.push(Waiver { violation: 0x1234, reason: "reviewed spacing".into() });
    let mut cases = vec![
        ("Add placed", vec![BoardOp::Add(Item::Placed(placed.clone()))]),
        ("Add track", vec![BoardOp::Add(Item::Track(track))]),
        ("Add via", vec![BoardOp::Add(Item::Via(via))]),
        ("Add zone", vec![BoardOp::Add(Item::Zone(zone))]),
        ("Add hole", vec![BoardOp::Add(Item::Hole(hole.clone()))]),
        ("Add text", vec![BoardOp::Add(Item::Text(text))]),
        ("Add tuning", vec![BoardOp::Add(Item::Tuning(tuning))]),
        ("Add bound tuning", vec![BoardOp::Add(Item::Tuning(bound_tuning))]),
        ("Insert", vec![BoardOp::Insert { item: Item::Placed(placed), pos: 1 }]),
        ("Remove", vec![BoardOp::Remove(before.vias[0].id)]),
        ("Remove then Insert", vec![BoardOp::Remove(before.holes[0].id), BoardOp::Insert { item: Item::Hole(before.holes[0].clone()), pos: 1 }]),
        ("Replace", vec![BoardOp::Replace(Item::Via(replacement))]),
        ("Move", vec![BoardOp::Move { item: before.placed[0].id, by: P::mm(0.1, 0.0) }]),
        ("SetField At", vec![BoardOp::SetField { item: before.placed[0].id, field: Field::At(P::mm(5.0, 14.0)) }]),
        ("SetField Rot", vec![BoardOp::SetField { item: before.placed[0].id, field: Field::Rot(Angle::deg(45.0)) }]),
        ("SetField Side", vec![BoardOp::SetField { item: before.placed[0].id, field: Field::Side(Side::Bottom) }]),
        ("SetField Locked", vec![BoardOp::SetField { item: before.placed[0].id, field: Field::Locked(true) }]),
        ("SetField Label", vec![BoardOp::SetField { item: before.placed[0].id, field: Field::Label(FpTextKind::Reference, Some(LabelPose { at: P::mm(0.0, -1.5), rot: Angle::deg(90.0), pinned: false })) }]),
        ("SetField Width", vec![BoardOp::SetField { item: before.tracks[0].id, field: Field::Width(300_000) }]),
        ("SetField Net", vec![BoardOp::SetField { item: before.tracks[0].id, field: Field::Net(before.net("GND")) }]),
        ("SetField Layer", vec![BoardOp::SetField { item: before.tracks[0].id, field: Field::Layer(LayerId::B_CU) }]),
        ("SetField Text", vec![BoardOp::SetField { item: before.texts[0].id, field: Field::Text("µ note".into()) }]),
        ("SetStackup", vec![BoardOp::SetStackup(stackup)]),
        ("SetOutline", vec![BoardOp::SetOutline(vec![Contour::rect(P::ZERO, P::mm(52.0, 31.0), 0)])]),
        ("SetOutline empty", vec![BoardOp::SetOutline(Vec::new())]),
        ("SetRules", vec![BoardOp::SetRules(rules)]),
        ("SetNets", vec![BoardOp::SetNets { nets: before.nets.clone(), classes: before.classes.clone(), pairs: before.pairs.clone() }]),
        ("Add then Remove", vec![BoardOp::Add(Item::Hole(hole.clone())), BoardOp::Remove(id)]),
    ];
    let mut batch: Vec<_> = before.placed.iter().cycle().take(38)
        .map(|p| BoardOp::Move { item: p.id, by: P::mm(0.1, 0.0) }).collect();
    batch.push(BoardOp::Add(Item::Hole(hole)));
    batch.push(BoardOp::Remove(before.vias[0].id));
    assert_eq!(batch.len(), 40, "40 ops: the commit has forty operations");
    cases.push(("40 ops", batch));
    for (class, ops) in cases {
        let mut after = before.clone();
        for op in &ops { after.apply_host(op).unwrap_or_else(|e| panic!("{class}: model op failed: {e}")); }
        let splice = splice_board(&sources, &map, &before, circuit, &ops, None).unwrap_or_else(|e| panic!("{class}: splice failed: {e:?}"));
        assert!(!splice.reevaluate, "{class}: board ops do not reevaluate");
        assert!(splice.edits.windows(2).all(|pair| pair[0].span.end <= pair[1].span.start), "{class}: edits are disjoint in the original text");
        if class == "Move" {
            assert_eq!(splice.edits.len(), 1, "{class}: one statement changes");
            let edit = &splice.edits[0];
            assert_eq!(edit.span.end - edit.span.start, 3, "{class}: only the characters the carry changes are replaced");
            assert_eq!(edit.text, "5.0", "{class}: 4.925 mm becomes 5.025 mm");
        }
        if matches!(class, "SetNets" | "Add then Remove") {
            assert!(splice.edits.is_empty(), "{class}: no surviving statement changes");
        }
        let mut edited = sources.clone();
        edited.apply(&splice.edits).unwrap_or_else(|e| panic!("{class}: edits failed: {e:?}"));
        let document = &edited.doc(doc).expect("edited document exists").text;
        let mut followed = map.clone();
        followed.follow(&splice);
        let inverse = splice.inverse(&sources, &map);
        let mut restored_sources = edited.clone();
        restored_sources.apply(&inverse.edits).unwrap_or_else(|e| panic!("{class}: inverse edits failed: {e:?}"));
        let restored_document = &restored_sources.doc(doc).expect("restored document exists").text;
        assert_eq!(restored_document, &sources.doc(doc).expect("original document exists").text, "{class}: undo restores every document byte");
        let mut restored = followed.clone();
        restored.follow(&inverse);
        let entries = |map: &fab_elec_lang::SourceMap| {
            let mut entries = map.of_doc(doc);
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            entries
        };
        assert_eq!(entries(&restored), entries(&map), "{class}: undo restores every selection and its range with no extra selections");
        let restored_ledger = Ledger::of_map(&restored, before.unit, doc, after.next_item);
        let restored_board = board_doc::lower(doc, restored_document, circuit, Some(&restored_ledger))
            .unwrap_or_else(|e| panic!("{class}: restored document failed to lower: {e:?}")).board;
        let mut redone = restored_sources.clone();
        redone.apply(&splice.edits).unwrap_or_else(|e| panic!("{class}: redo edits failed: {e:?}"));
        restored.follow(&splice);
        assert_eq!(&redone.doc(doc).expect("redone document exists").text, document, "{class}: redo restores every forward document byte");
        assert_eq!(entries(&restored), entries(&followed), "{class}: redo restores every forward selection and range, including added item ids");
        let ledger = Ledger::of_map(&followed, after.unit, doc, after.next_item);
        let lowered = board_doc::lower(doc, document, circuit, Some(&ledger)).unwrap_or_else(|e| panic!("{class}: edited document failed to lower: {e:?}"));
        assert_eq!(write(&lowered.board, circuit), write(&after, circuit), "{class}: text and model have the same content with ledger ids");
        assert_eq!(lowered.board.next_item, after.next_item, "{class}: lowering preserves the model's next item id");
        assert!(document.lines().any(|line| line == untouched), "{class}: the untouched non-canonical statement is byte-identical");
        if class == "Add tuning" {
            assert!(document.contains("let t1 = track(") && document.contains("let t2 = track("), "{class}: new bindings use the smallest unbound indexes");
            assert!(document.contains("host: [t1] copper: [t2]"), "{class}: both unbound tracks are named");
        }
        if class == "Add bound tuning" {
            assert!(document.contains("host: [branch] copper: [t0]"), "{class}: existing binding names are reused");
        }
        if class == "SetField Label" {
            assert!(document.contains("ref_label: {at: [0mm, -1.5mm] rot: 90deg}"), "{class}: an arranged reference label is written into its place statement");
        }
        let mut statements = Vec::new();
        macro_rules! list {
            ($field:ident, $prefix:literal) => {
                assert_eq!(lowered.board.$field.iter().map(|i| i.id).collect::<Vec<_>>(), after.$field.iter().map(|i| i.id).collect::<Vec<_>>(),
                    "{class}: {} ids keep their model order", stringify!($field));
                assert_eq!(restored_board.$field.iter().map(|i| i.id).collect::<Vec<_>>(), before.$field.iter().map(|i| i.id).collect::<Vec<_>>(),
                    "{class}: undo restores {} ids in their original order", stringify!($field));
                statements.extend(after.$field.iter().filter(|i| i.origin.has_statement()).map(|i| (i.id, $prefix)));
            };
        }
        list!(placed, "place(");
        list!(tracks, "track(");
        list!(vias, "via(");
        list!(zones, "zone(");
        list!(keepouts, "keepout(");
        list!(holes, "hole(");
        list!(cutouts, "cutout(");
        list!(graphics, "graphic(");
        list!(texts, "text(");
        list!(tunings, "tuning(");
        for (id, prefix) in statements {
            let range = followed.locate(&Sel::Item(after.unit, id)).unwrap_or_else(|| panic!("{class}: item {} has no followed range", id.0));
            assert_eq!(range.doc, doc, "{class}: item {} stays in its document", id.0);
            let statement = &document[range.span.start as usize..range.span.end as usize];
            assert!(statement.starts_with(prefix) || (prefix == "track(" && statement.starts_with("let ")), "{class}: item {} maps to its statement start: {statement}", id.0);
            assert!(lowered.spans.iter().any(|(_, span)| *span == range.span), "{class}: item {} maps to a complete statement", id.0);
        }
        for sel in &splice.removes { assert!(followed.locate(sel).is_none(), "{class}: a removed statement leaves the map"); }
    }
    let kept_waiver = "waive(\"0000000000000011\",   \"keep\")";
    let kept_rule = "rule(\"keep\", {when: fn(a, b) { true } set: {clearance: 0.2mm}})";
    let kept_check = "check(\"keep\",  fn(b) { true })";
    let mut with_headers = before.clone();
    with_headers.stackup.thickness += 200_000;
    with_headers.rules.base.clearance = Some(200_000);
    let extra_headers = write(&with_headers, circuit).lines()
        .filter(|line| line.starts_with("stackup(") || line.starts_with("rules("))
        .map(|line| format!("{line}\n")).collect::<String>();
    let document = format!("{}{extra_headers}{kept_waiver}\nwaive(\"0000000000000022\", \"drop\")\n{kept_rule}\nrule(\"drop\", {{when: nil set: {{}}}})\n{kept_check}\ncheck(\"drop\", nil)\n", sources.doc(doc).expect("document exists").text);
    let mut header_sources = Sources::new();
    let header_doc = header_sources.add("ledctl.pcb.splash", document).expect("SetRules code: document is added");
    let header_before = board_doc::lower(header_doc, &header_sources.doc(header_doc).expect("document exists").text, circuit, None).expect("SetRules code: headers lower");
    let mut header_map = from_spans(header_doc, &header_before.spans);
    let code = RuleCode { id: 0, hash: ContentHash([0; 16]), at: (header_doc.0, 0) };
    let mut rules = before.rules.clone();
    rules.waivers = vec![Waiver { violation: 0x33, reason: "new".into() }, Waiver { violation: 0x11, reason: "keep".into() }];
    for name in ["new", "keep"] {
        rules.custom.push(CustomRule { name: name.into(), when: code.clone(), layers: None, set: RuleSet::default(), severity: Severity::default() });
        rules.checks.push(CustomCheck { name: name.into(), code: code.clone(), severity: Severity::default() });
    }
    let ops = [BoardOp::SetStackup(before.stackup.clone()), BoardOp::SetOutline(Vec::new()), BoardOp::SetRules(rules.clone())];
    let splice = splice_board(&header_sources, &header_map, &header_before.board, circuit, &ops, None).expect("SetRules code: headers splice");
    header_sources.apply(&splice.edits).expect("SetRules code: edits apply");
    header_map.follow(&splice);
    let document = &header_sources.doc(header_doc).expect("document exists").text;
    assert!(document.contains(kept_waiver) && document.contains(kept_rule) && document.contains(kept_check), "SetRules code: retained waivers and functions keep their exact source");
    assert!(!document.contains("\"drop\"") && !document.contains("stackup(") && !document.contains("outline(") && !document.contains("rules("), "SetRules code: removed headers and omitted defaults disappear");
    assert!(document.contains("rule(\"new\", {when: nil set: {}})") && document.contains("check(\"new\", nil)"), "SetRules code: new functions use nil code");
    let header_after = board_doc::lower(header_doc, document, circuit, None).expect("SetRules code: edited headers lower");
    assert_eq!(header_after.board.rules.waivers, rules.waivers, "SetRules code: new waivers keep the requested order");
    assert_eq!(header_after.code.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["new", "keep", "new", "keep"], "SetRules code: rule and check order survives");
    assert!(header_map.locate(&Sel::Feature(FeatureRef::Outline(before.unit))).is_none(), "SetOutline empty: the outline leaves the map");
    let mut generated = before.clone();
    let mut template = before.vias[0].clone();
    template.id = generated.alloc_id();
    template.at = P::mm(3.0, 4.0);
    template.origin = Origin::Block { block: circuit.tree[1].id, detached: false };
    let detached_id = template.id;
    generated.vias.push(template);
    let ops = [BoardOp::Detach(detached_id)];
    let mut detached = generated.clone();
    detached.apply_host(&ops[0]).expect("Detach: the model op applies");
    let splice = splice_board(&sources, &map, &generated, circuit, &ops, None).expect("Detach: a statement is added");
    let mut detached_sources = sources.clone();
    detached_sources.apply(&splice.edits).expect("Detach: the text edit applies");
    let document = &detached_sources.doc(doc).expect("document exists").text;
    let lowered = board_doc::lower(doc, document, circuit, None).expect("Detach: the new statement lowers");
    assert_eq!(write(&lowered.board, circuit), write(&detached, circuit), "Detach: text and model agree");
    let mut detached_map = map.clone();
    detached_map.follow(&splice);
    let range = detached_map.locate(&Sel::Item(before.unit, detached_id)).expect("Detach: the new statement is mapped");
    assert!(document[range.span.start as usize..range.span.end as usize].starts_with("via("), "Detach: the range starts at the new via");
    assert!(document.lines().any(|line| line == untouched), "Detach: the untouched statement keeps its formatting");
    let library_doc = DocId(LIB_DOC | doc.0);
    let library_map = from_spans(library_doc, &[(Sel::Unit(before.unit), 0..1)]);
    let error = splice_board(&sources, &library_map, &before, circuit, &[], None).expect_err("library board: writes are refused");
    assert!(error.message.contains("read-only") && matches!(error.sel.as_slice(), [Sel::Source { doc, .. }] if *doc == library_doc), "library board: refusal is located in the library");
    let error = splice_board(&sources, &map, &before, circuit, &[BoardOp::Remove(id)], None).expect_err("invalid Remove: the op fails");
    assert!(error.message.contains("Remove") && matches!(error.sel.as_slice(), [Sel::Source { doc: found, .. }] if *found == doc), "invalid Remove: the diagnostic names the op and its document");
}

#[test]
fn typed_deletion_and_reinsertion_preserve_surviving_item_ids() {
    let fixture = fab_elec_fixtures::ledctl();
    let circuit = &fixture.units[0].circuit;
    let board = fixture.units[0].board.as_ref().expect("fixture has a board");
    let document = write(board, circuit).lines().filter(|line| line.starts_with("place(")).take(3)
        .map(|line| format!("{line}\n")).collect::<String>();
    let mut sources = Sources::new();
    let doc = sources.add("ledctl.pcb.splash", document).expect("board document is added");
    let lowered = board_doc::lower(doc, &sources.doc(doc).expect("document exists").text, circuit, None).expect("placements lower");
    let mut map = from_spans(doc, &lowered.spans);
    let before = lowered.board;
    assert_eq!(before.placed.len(), 3, "the deletion has a placement before and after it");
    let deleted = &before.placed[1];
    let sel = Sel::Item(before.unit, deleted.id);
    let range = map.locate(&sel).expect("the deleted placement has a statement");
    let document = &sources.doc(doc).expect("document exists").text;
    assert_eq!(document.as_bytes()[range.span.end as usize], b'\n', "the deleted line ends immediately after its statement");
    let line = document[range.span.start as usize..range.span.end as usize + 1].to_string();
    let edit = SpanEdit {
        edits: vec![TextEdit { doc, span: range.span.start..range.span.end + 1, text: String::new() }],
        ..Default::default()
    };
    sources.apply(&edit.edits).expect("typed line deletion applies");
    map.follow(&edit);
    assert!(map.locate(&sel).is_none(), "typed deletion removes the item from the forward map");
    let ledger = Ledger::of_map(&map, before.unit, doc, before.next_item);
    assert!(ledger.items.iter().all(|(_, id)| *id != deleted.id), "typed deletion removes the item from the reverse map's ledger");
    let lowered = board_doc::lower(doc, &sources.doc(doc).expect("document exists").text, circuit, Some(&ledger)).expect("remaining placements lower");
    let survivors: Vec<_> = before.placed.iter().filter(|p| p.id != deleted.id).map(|p| p.id).collect();
    assert_eq!(lowered.board.placed.iter().map(|p| p.id).collect::<Vec<_>>(), survivors, "every surviving placement keeps its id and order");
    assert!(!lowered.board.placed.iter().any(|p| p.id == deleted.id), "the deleted id is absent from the lowered board");
    assert!(lowered.board.next_item >= before.next_item, "typed deletion never lowers the model's next item id");

    let edit = SpanEdit {
        edits: vec![TextEdit { doc, span: range.span.start..range.span.start, text: line }],
        ..Default::default()
    };
    sources.apply(&edit.edits).expect("typed line reinsertion applies");
    map.follow(&edit);
    let ledger = Ledger::of_map(&map, before.unit, doc, lowered.board.next_item);
    let document = &sources.doc(doc).expect("document exists").text;
    let reinserted = board_doc::lower(doc, document, circuit, Some(&ledger)).expect("reinserted placement lowers");
    assert_eq!(reinserted.board.placed.len(), before.placed.len(), "reinsertion restores the placement count");
    let fresh = reinserted.board.placed[1].id;
    assert_eq!(reinserted.board.placed[1].path, deleted.path, "the copied statement restores the deleted component");
    assert!(fresh >= ItemId(before.next_item), "a typed statement takes an id at or above the model's counter");
    assert_ne!(fresh, deleted.id, "reinsertion never reuses the deleted id");
    assert_eq!(reinserted.board.placed.iter().filter(|p| p.id != fresh).map(|p| p.id).collect::<Vec<_>>(), survivors, "reinsertion preserves every other placement id and its order");
    assert_eq!(reinserted.board.next_item, fresh.0 + 1, "the next item id advances past the new statement");
    let loaded = board_doc::lower(doc, document, circuit, None).expect("a fresh load lowers without a ledger");
    assert_eq!(loaded.board.placed.iter().map(|p| p.id).collect::<Vec<_>>(), vec![ItemId(0), ItemId(1), ItemId(2)], "a fresh load numbers items in statement order");
    assert_eq!(loaded.board.next_item, 3, "a fresh load's counter follows its statement count");
    assert_eq!(write(&loaded.board, circuit), write(&reinserted.board, circuit), "fresh and ledger loads agree on content despite different ids");
}

#[test]
fn verified_board_commits_keep_item_ids() {
    let mut sources = Sources::new();
    sources.add("ledctl.ckt.splash", include_str!("../../fixtures/ledctl.ckt.splash")).expect("the circuit document is added");
    let doc = sources.add("ledctl.pcb.splash", include_str!("../../fixtures/ledctl.pcb.splash")).expect("the board document is added");
    let libs = std_libs();
    let ev = evaluate(&sources, &libs).expect("ledctl evaluates with its board");
    let unit = &ev.design.units[0];
    let before = unit.board.as_ref().expect("the unit has a board");
    let ids = |b: &fab_elec_board::Board| -> Vec<ItemId> {
        b.placed.iter().map(|i| i.id).chain(b.tracks.iter().map(|i| i.id)).chain(b.vias.iter().map(|i| i.id))
            .chain(b.zones.iter().map(|i| i.id)).chain(b.holes.iter().map(|i| i.id)).collect()
    };
    assert!(before.placed.len() > 1 && !before.tracks.is_empty(), "the fixture has statements after the first placement");

    // Removing the first placement moves every later statement's start; the verified evaluation
    // reads ids through the map followed by the edit, so no statement inherits the removed id.
    let removed = before.placed[0].id;
    let op = DesignOp::Board { unit: unit.id, op: BoardOp::Remove(removed) };
    let verified = splice_verified(&sources, &libs, &ev, &[op], &NO_CANCEL).expect("removing a placement verifies");
    let after = verified.evaluated.design.units[0].board.as_ref().expect("the board stays");
    let expected: Vec<_> = ids(before).into_iter().filter(|id| *id != removed).collect();
    assert_eq!(ids(after), expected, "every other item keeps its id after a verified Remove");
    assert!(after.next_item >= before.next_item, "the verified board's counter never moves back");
    for id in &expected {
        let at = verified.evaluated.map.locate(&Sel::Item(unit.id, *id)).unwrap_or_else(|| panic!("item {} keeps its statement", id.0));
        assert_eq!(at.doc, doc, "item {} stays in the board document", id.0);
    }

    // A new statement takes the model's next id, as the model would issue it.
    let mut via = before.vias[0].clone();
    via.id = ItemId(after.next_item);
    via.at = P::mm(2.0, 4.0);
    let op = DesignOp::Board { unit: unit.id, op: BoardOp::Add(Item::Via(via.clone())) };
    let added = splice_verified(&verified.sources, &libs, &verified.evaluated, &[op], &NO_CANCEL).expect("adding a via verifies");
    let board = added.evaluated.design.units[0].board.as_ref().expect("the board stays");
    assert_eq!(board.vias.last().map(|v| (v.id, v.at)), Some((via.id, via.at)), "the added via has the id the model issued");
    assert_eq!(ids(board).into_iter().filter(|id| *id != via.id).collect::<Vec<_>>(), expected, "adding keeps every earlier id");

    // A set cancel flag stops both before their first unit: no edit, no design, only Cancelled.
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let op = DesignOp::Board { unit: unit.id, op: BoardOp::Remove(removed) };
    assert_eq!(splice_verified(&sources, &libs, &ev, &[op], &cancel).map(|_| ()), Err(EvalError::Cancelled), "a cancelled verification gives no edit");
    assert_eq!(evaluate_from(Some(&ev), &sources, &libs, &cancel).map(|_| ()), Err(EvalError::Cancelled), "a cancelled evaluation gives no design");
}

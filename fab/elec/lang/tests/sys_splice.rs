use fab_elec_core::{CompRef, Diag, DocId, LinkId, PinIx, PinRef, Sel, Severity, Xform3};
use fab_elec_design::{Design, DesignOp, Frame, Link, LinkEnd, LinkKind, Mount, PinMap, SysOp, TextEdit};
use fab_elec_lang::{NO_CANCEL, check_system, evaluate, evaluate_from, load_dir, relower_system, splice, std_libs, sys_doc::{self, SysLedger}, DocKind, SourceMap, SourceRange, Sources, SpanEdit, UnitState, SYSTEM_DOC};
use makepad_script::syntax::write::quote;
use std::ops::Range;

#[derive(Clone)]
struct State {
    sources: Sources,
    map: SourceMap,
    design: Design,
    doc: DocId,
}

impl State {
    fn fixture(name: &str) -> State {
        let text = (name == "pair").then_some(concat!(
            "let dbg = mate(ledctl.mcu.dbg, probe.j)\n",
            "wire(ledctl.mcu.dbg.pin(\"1\"), probe.j.pin(\"1\"))\n",
            "mount(probe, {on: dbg at: [10mm, 0mm, -8mm]})\n",
        ));
        State::with_system(name, text)
    }

    fn with_system(name: &str, text: Option<&str>) -> State {
        let mut sources = fab_elec_fixtures::docs(name).expect("the stock project has documents");
        let doc = sources.docs().iter().find(|d| d.kind == DocKind::System).expect("the project has a system document").id;
        if let Some(text) = text {
            sources.apply(&[TextEdit { doc, span: 0..sources.doc(doc).expect("the system exists").text.len() as u32, text: text.into() }])
                .expect("the supplied system replaces the stock text");
        }
        let ev = evaluate(&sources, &std_libs()).expect("the fixture evaluates");
        assert!(ev.units.iter().all(|u| u.state == UnitState::Built), "all fixture units build");
        assert!(ev.system.diags.is_empty(), "the fixture system resolves: {:?}", ev.system.diags);
        assert_eq!(ev.system.doc, Some(doc), "evaluation keeps even an empty system document");
        let state = State { sources, map: ev.map, design: ev.design, doc };
        assert_eq!(state.text(), sys_doc::write(&state.design), "the starting system is canonical");
        state
    }

    fn text(&self) -> &str {
        &self.sources.doc(self.doc).expect("the system document exists").text
    }

    fn next_id(&self) -> LinkId {
        sys_doc::next_link_id(self.design.links.iter().map(|l| l.id))
    }

    fn mount(&self, name: &str) -> Mount {
        let unit = self.design.units.iter().find(|u| u.name == name).expect("the mounted unit exists");
        self.design.assembly.mounts.iter().find(|m| m.unit == unit.id).expect("every unit has a mount").clone()
    }

    fn assert_model(&self) {
        assert_eq!(check_system(&self.sources, &self.design, &self.map), Ok(()), "the system check agrees after every commit and undo");
        let ledger = SysLedger::of_map(&self.map, self.doc);
        let lowered = sys_doc::lower(self.doc, self.text(), &self.design.units, Some(&ledger));
        assert!(lowered.diags.is_empty(), "the spliced text lowers without diagnostics: {:?}", lowered.diags);
        assert_eq!(lowered.links, self.design.links, "lowering preserves model links, ids and order");
        assert_eq!(lowered.mounts, self.design.assembly.mounts, "lowering preserves every model mount");
        let spans: Vec<_> = lowered.spans.into_iter().map(|(sel, span)| (sel, SourceRange { doc: self.doc, span })).collect();
        assert_eq!(self.map.of_doc(self.doc), spans, "the followed map selects every whole link statement, including bindings");
    }

    fn apply(&self, op: SysOp) -> (State, SpanEdit, Vec<DesignOp>) {
        self.commit(vec![op])
    }

    /// One commit of `ops`: the model and the splice, checked against each other and undone.
    fn commit(&self, ops: Vec<SysOp>) -> (State, SpanEdit, Vec<DesignOp>) {
        let ops: Vec<_> = ops.into_iter().map(DesignOp::Sys).collect();
        let mut after = self.clone();
        let mut inverse = Vec::new();
        for op in &ops {
            let mut undo = after.design.apply(op, false).expect("the model accepts the op").inverse();
            undo.extend(inverse);
            inverse = undo;
        }
        let edit = splice(&self.sources, &self.map, &self.design, &ops).expect("the system splice succeeds");
        assert!(!edit.reevaluate, "system splices never request reevaluation");
        assert!(edit.edits.windows(2).all(|p| p[0].span.end <= p[1].span.start), "system token edits are ordered and disjoint");
        after.sources.apply(&edit.edits).expect("the system edits apply");
        after.map.follow(&edit);
        after.assert_model();
        assert_eq!(after.text(), sys_doc::write(&after.design), "splicing canonical text agrees with the shared system writer");

        let undo = edit.inverse(&self.sources, &self.map);
        let mut restored = after.clone();
        restored.sources.apply(&undo.edits).expect("the text inverse applies");
        restored.map.follow(&undo);
        for op in &inverse { restored.design.apply(op, true).expect("the model inverse applies"); }
        assert_eq!(restored.sources, self.sources, "the text inverse restores every source byte");
        for doc in self.sources.docs() {
            assert_eq!(restored.map.of_doc(doc.id), self.map.of_doc(doc.id), "the text inverse restores every selection and range in {}", doc.name);
        }
        assert_eq!(restored.design.links, self.design.links, "the model inverse restores link ids and order");
        assert_eq!(restored.design.assembly.mounts, self.design.assembly.mounts, "the model inverse restores mounts");
        restored.assert_model();
        (after, edit, inverse)
    }
}

fn edit_text(state: &State, edit: &SpanEdit) -> Vec<String> {
    edit.edits.iter().map(|e| state.text()[e.span.start as usize..e.span.end as usize].to_string()).collect()
}

fn located(diag: &Diag, doc: DocId) -> Range<u32> {
    assert_eq!(diag.sel.len(), 1, "a refusal has exactly one source location");
    let Sel::Source { doc: got, start, end } = diag.sel[0] else { panic!("a refusal must select source text"); };
    assert_eq!(got, doc, "the refusal selects the system document");
    start..end
}

#[test]
fn system_check_locates_model_drift_and_recovers_from_text() {
    let sources = fab_elec_fixtures::docs("pair").expect("the pair has documents");
    let ev = evaluate(&sources, &std_libs()).expect("the pair evaluates");
    assert!(ev.units.iter().all(|u| u.state == UnitState::Built), "both pair units build before checking");
    assert!(ev.system.diags.is_empty(), "the pair starts without system diagnostics");
    let doc = ev.system.doc.expect("the pair has a system document");
    let state = State { sources, map: ev.map, design: ev.design, doc };
    assert_eq!(check_system(&state.sources, &state.design, &state.map), Ok(()), "the evaluated system agrees with its text");

    let mut added = state.design.links[0].clone();
    added.id = state.next_id();
    added.kind = LinkKind::Wire;
    let (state, _, _) = state.apply(SysOp::AddLink(added.clone()));
    assert_eq!(check_system(&state.sources, &state.design, &state.map), Ok(()), "AddLink through apply and splice keeps the system consistent");
    let mut mount = state.mount("probe");
    mount.xform.t[0] = 12_500_000;
    let (state, _, _) = state.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(check_system(&state.sources, &state.design, &state.map), Ok(()), "SetMount through apply and splice keeps the system consistent");
    let saved_map = state.map.clone();
    let source_range = |diag: &Diag| {
        let ranges: Vec<_> = diag.sel.iter().filter_map(|sel| match sel {
            Sel::Source { doc, start, end } => Some(SourceRange { doc: *doc, span: *start..*end }),
            _ => None,
        }).collect();
        assert_eq!(ranges.len(), 1, "a system mismatch has exactly one source range");
        assert_eq!(ranges[0].doc, doc, "a system mismatch selects the system document");
        ranges[0].span.clone()
    };

    let mut changed = state.clone();
    let mut replacement = added.clone();
    replacement.kind = LinkKind::Mate;
    changed.design.apply(&DesignOp::Sys(SysOp::ReplaceLink(replacement)), false).expect("the model-only kind change applies");
    let diag = check_system(&changed.sources, &changed.design, &changed.map).expect_err("a model-only link kind change must fail the check");
    assert_eq!(diag.severity, Severity::Error, "a link mismatch is an error");
    assert_eq!(diag.message, format!("link {}: the text has a wire, the model a mate", added.id.0), "the mismatch names the link and both kinds");
    assert!(diag.sel.contains(&Sel::Link(added.id)), "a link mismatch carries the link selection");
    let statement = state.map.locate(&Sel::Link(added.id)).expect("the changed link has a statement");
    assert_eq!(source_range(&diag), statement.span, "a kind mismatch selects the differing link's statement");

    let mut extra = state.clone();
    let mut link = added.clone();
    link.id = extra.next_id();
    extra.design.apply(&DesignOp::Sys(SysOp::AddLink(link.clone())), false).expect("the model-only extra link applies");
    let diag = check_system(&extra.sources, &extra.design, &extra.map).expect_err("a link absent from the text must fail the check");
    assert_eq!(diag.message, format!("link {} is in the model but not in the text", link.id.0), "the mismatch names the extra model link");
    assert!(diag.sel.contains(&Sel::Link(link.id)), "an extra model link carries its link selection");
    assert_eq!(source_range(&diag), 0..extra.text().len() as u32, "a link absent from text selects the whole system document");

    let mut moved = state.clone();
    mount.xform.t[0] += 1_000_000;
    moved.design.apply(&DesignOp::Sys(SysOp::SetMount(mount.clone())), false).expect("the model-only mount change applies");
    let diag = check_system(&moved.sources, &moved.design, &moved.map).expect_err("a model-only mount change must fail the check");
    assert!(diag.message.starts_with("probe's mount differs:"), "the mount mismatch names the unit: {}", diag.message);
    assert!(diag.sel.contains(&Sel::Unit(mount.unit)), "a mount mismatch carries the unit selection");
    assert_eq!(source_range(&diag), 0..moved.text().len() as u32, "a mount mismatch selects the whole system document");

    for mut mismatched in [changed, extra, moved] {
        assert_eq!(mismatched.map, saved_map, "checking a mismatch leaves the source map unchanged");
        let lowered = relower_system(&mismatched.sources, &mismatched.design.units, &mut mismatched.map);
        assert!(lowered.system.diags.is_empty(), "recovery lowers the unchanged text without diagnostics");
        mismatched.design.links = lowered.links;
        mismatched.design.assembly.mounts = lowered.mounts;
        assert_eq!(check_system(&mismatched.sources, &mismatched.design, &mismatched.map), Ok(()), "rebuilding the model from text recovers every mismatch");
    }
}

#[test]
fn relowering_system_follows_typed_insertions_and_deletions() {
    let sources = fab_elec_fixtures::docs("pair").expect("the pair has documents");
    let libs = std_libs();
    let ev = evaluate(&sources, &libs).expect("the pair evaluates");
    assert!(ev.units.iter().all(|u| u.state == UnitState::Built), "both pair units build before re-lowering");
    assert!(ev.system.diags.is_empty(), "the pair starts without system diagnostics");
    let doc = ev.system.doc.expect("the pair has a system document");
    let mut state = State { sources, map: ev.map.clone(), design: ev.design.clone(), doc };
    let link_entries = |map: &SourceMap| map.of_doc(doc).into_iter().filter(|(sel, _)| matches!(sel, Sel::Link(_))).collect::<Vec<_>>();
    let lowered = relower_system(&state.sources, &state.design.units, &mut state.map);
    assert_eq!(lowered.links, ev.design.links, "fresh re-lowering preserves every link and its id");
    assert_eq!(lowered.mounts, ev.design.assembly.mounts, "fresh re-lowering preserves every mount");
    assert_eq!(lowered.system.diags, ev.system.diags, "fresh re-lowering preserves system diagnostics");
    assert_eq!(lowered.system.doc, Some(doc), "re-lowering records the system document");
    assert_eq!(link_entries(&state.map), link_entries(&ev.map), "fresh re-lowering preserves every link location");
    assert_eq!(state.map, ev.map, "fresh re-lowering leaves every other map entry and site alone");

    let next = state.next_id();
    let line = "wire(ledctl.mcu.dbg.pin(\"1\"), probe.j.pin(\"1\"))\n";
    let insertion = SpanEdit { edits: vec![TextEdit { doc, span: 0..0, text: line.into() }], ..SpanEdit::default() };
    state.sources.apply(&insertion.edits).expect("the typed wire line inserts at the document start");
    state.map.follow(&insertion);
    let mut prev = ev.clone();
    prev.map = state.map.clone();
    let lowered = relower_system(&state.sources, &state.design.units, &mut state.map);
    assert_eq!(lowered.links.len(), ev.design.links.len() + 1, "typing a wire adds exactly one link");
    assert_eq!(lowered.links[0].id, next, "the typed wire takes the id after the previous highest id");
    assert_eq!(lowered.links[0].kind, LinkKind::Wire, "the inserted statement lowers as a wire");
    assert_eq!(&lowered.links[1..], ev.design.links.as_slice(), "every old link keeps its id and ends after insertion");
    assert_eq!(state.map.locate(&Sel::Link(next)), Some(SourceRange { doc, span: 0..line.len() as u32 - 1 }), "the new link selects its entire statement without the newline");
    for (sel, range) in link_entries(&ev.map) {
        let shifted = SourceRange { doc, span: range.span.start + line.len() as u32..range.span.end + line.len() as u32 };
        assert_eq!(state.map.locate(&sel), Some(shifted), "the old link {sel:?} keeps its id at the shifted statement");
    }
    let fresh = evaluate_from(Some(&prev), &state.sources, &libs, &NO_CANCEL).expect("the inserted system evaluates with the followed ledger");
    assert_eq!(lowered.links, fresh.design.links, "in-place insertion matches evaluation's links and ids");
    assert_eq!(lowered.mounts, fresh.design.assembly.mounts, "in-place insertion matches evaluation's mounts");
    assert_eq!(lowered.system.diags, fresh.system.diags, "in-place insertion matches evaluation's diagnostics");
    assert_eq!(link_entries(&state.map), link_entries(&fresh.map), "in-place insertion matches every evaluated link location");
    for other in state.sources.docs().iter().filter(|d| d.id != doc) {
        assert_eq!(state.map.of_doc(other.id), prev.map.of_doc(other.id), "re-lowering the insertion leaves {} selections alone", other.name);
    }
    state.design.links = lowered.links;
    state.design.assembly.mounts = lowered.mounts;
    state.assert_model();

    let removed = ev.design.links.iter().find(|l| !state.design.assembly.mounts.iter().any(|m| m.parent == Frame::Mate(l.id)))
        .expect("the pair has an old link that no mount names").id;
    let range = state.map.locate(&Sel::Link(removed)).expect("the removed link has a statement");
    let deletion = SpanEdit { edits: vec![TextEdit { doc, span: range.span.start..range.span.end + 1, text: String::new() }], ..SpanEdit::default() };
    assert!(edit_text(&state, &deletion)[0].ends_with('\n'), "the typed deletion consumes the whole link line and newline");
    state.sources.apply(&deletion.edits).expect("the unmounted link line deletes by text");
    state.map.follow(&deletion);
    let mut prev = fresh;
    prev.map = state.map.clone();
    let lowered = relower_system(&state.sources, &state.design.units, &mut state.map);
    assert!(!lowered.links.iter().any(|l| l.id == removed), "the deleted link is absent from the lowered model");
    assert!(state.map.locate(&Sel::Link(removed)).is_none(), "the deleted link has no stale map entry");
    let remaining: Vec<_> = state.design.links.iter().filter(|l| l.id != removed).cloned().collect();
    assert_eq!(lowered.links, remaining, "deletion preserves every surviving link and its id");
    let fresh = evaluate_from(Some(&prev), &state.sources, &libs, &NO_CANCEL).expect("the deleted system evaluates with the followed ledger");
    assert_eq!(lowered.links, fresh.design.links, "in-place deletion matches evaluation's links and ids");
    assert_eq!(lowered.mounts, fresh.design.assembly.mounts, "in-place deletion matches evaluation's mounts");
    assert_eq!(lowered.system.diags, fresh.system.diags, "in-place deletion matches evaluation's diagnostics");
    assert_eq!(link_entries(&state.map), link_entries(&fresh.map), "in-place deletion matches every evaluated link location");
    for other in state.sources.docs().iter().filter(|d| d.id != doc) {
        assert_eq!(state.map.of_doc(other.id), prev.map.of_doc(other.id), "re-lowering the deletion leaves {} selections alone", other.name);
    }
    state.design.links = lowered.links;
    state.design.assembly.mounts = lowered.mounts;
    state.assert_model();
}

#[test]
fn add_links_to_an_empty_system_and_undo() {
    let before = State::fixture("ledctl");
    assert_eq!(before.sources.doc(before.doc).expect("system document exists").name, SYSTEM_DOC, "the stock empty system document uses the fixed name");
    assert_eq!(before.text(), "", "a single-board project starts with empty system text");
    let unit = &before.design.units[0];
    let end = |i: usize| LinkEnd::Comp(CompRef { board: unit.id, comp: unit.circuit.comps[i].id });
    let link = Link { id: before.next_id(), kind: LinkKind::Wire, a: end(0), b: end(1), pinmap: PinMap::Derived };
    let (first, edit, _) = before.apply(SysOp::AddLink(link.clone()));
    assert_eq!(edit.edits.len(), 1, "the first link needs one insertion");
    assert_eq!(edit.edits[0].span, 0..0, "the first link inserts at the empty document start");
    assert_eq!(edit.edits[0].text, first.text(), "the insertion is the complete first line");
    assert_eq!(edit.adds, vec![(Sel::Link(link.id), SourceRange { doc: before.doc, span: 0..first.text().len() as u32 - 1 })], "the added range covers the new statement without its newline");
    assert!(edit.removes.is_empty(), "adding the first link removes no selections");
    let undo = edit.inverse(&before.sources, &before.map);
    assert_eq!(undo.edits, vec![TextEdit { doc: before.doc, span: 0..first.text().len() as u32, text: String::new() }], "undo deletes the whole first line back to empty text");
    let mut second_link = link;
    second_link.id = first.next_id();
    let (second, edit, _) = first.apply(SysOp::AddLink(second_link.clone()));
    assert_eq!(edit.edits[0].span, first.text().len() as u32..first.text().len() as u32, "the second link inserts after the first line");
    assert_eq!(second.design.links[1].id, second_link.id, "the second link keeps its allocated id and model position");
    assert!(second.text().starts_with(first.text()), "adding a second link preserves the first line byte for byte");
}

#[test]
fn insertion_order_and_removal_inverses() {
    let before = State::fixture("pair");
    let mut link = before.design.links[1].clone();
    link.id = before.next_id();
    let (front, edit, _) = before.apply(SysOp::InsertLink { link: link.clone(), pos: 0 });
    assert_eq!(edit.edits[0].span, 0..0, "InsertLink at zero precedes the bound first statement");
    assert_eq!(front.design.links[0], link, "the inserted statement is first in model order");
    link.id = front.next_id();
    let at = front.text().find("mount(").expect("the pair has a mount") as u32;
    let (end, edit, _) = front.apply(SysOp::InsertLink { link: link.clone(), pos: front.design.links.len() as u32 });
    assert_eq!(edit.edits[0].span, at..at, "InsertLink at the end follows the last link and precedes mounts");
    assert_eq!(end.design.links.last(), Some(&link), "the inserted statement is last in model link order");
    let removed = before.design.links[1].clone();
    let range = end.map.locate(&Sel::Link(removed.id)).expect("the unbound link is mapped");
    let (after, deletion, inverse) = end.apply(SysOp::RemoveLink(removed.id));
    assert_eq!(deletion.edits, vec![TextEdit { doc: end.doc, span: range.span.start..range.span.end + 1, text: String::new() }], "RemoveLink deletes exactly the statement and newline");
    assert_eq!(deletion.removes, vec![Sel::Link(removed.id)], "RemoveLink drops the removed link selection");
    assert!(after.map.locate(&Sel::Link(removed.id)).is_none(), "the removed id has no stale statement");
    assert_eq!(inverse, vec![DesignOp::Sys(SysOp::InsertLink { link: removed.clone(), pos: 2 })], "the model inverse remembers the removed link's id and exact position");
    let fresh = splice(&after.sources, &after.map, &after.design, &inverse).expect("the model inverse also has a splice");
    let saved = deletion.inverse(&end.sources, &end.map);
    assert_eq!(fresh.edits, saved.edits, "fresh InsertLink and the saved inverse restore identical bytes at the same offset");
    let mut restored = after.clone();
    restored.sources.apply(&fresh.edits).expect("fresh InsertLink edits apply");
    restored.map.follow(&fresh);
    for op in &inverse { restored.design.apply(op, true).expect("InsertLink restores the model"); }
    assert_eq!(restored.sources, end.sources, "fresh InsertLink restores the same statement bytes at the same offset");
    assert_eq!(restored.map.locate(&Sel::Link(removed.id)), Some(range), "fresh InsertLink restores the removed id and its range");
    assert_eq!(restored.map.of_doc(end.doc), end.map.of_doc(end.doc), "fresh InsertLink restores all link ranges like the saved inverse");
    restored.assert_model();

    // An unused authored binding has no representation in Link; only the text inverse keeps it.
    let mut bound = before.clone();
    let range = bound.map.locate(&Sel::Link(removed.id)).expect("the unbound link is mapped");
    let prefix = "let spare = ";
    let binding = SpanEdit {
        edits: vec![TextEdit { doc: bound.doc, span: range.span.start..range.span.start, text: prefix.into() }],
        adds: vec![(Sel::Link(removed.id), SourceRange { doc: bound.doc, span: range.span.start..range.span.end + prefix.len() as u32 })],
        ..SpanEdit::default()
    };
    bound.sources.apply(&binding.edits).expect("an unused binding can be authored");
    bound.map.follow(&binding);
    bound.assert_model();
    let (after, deletion, inverse) = bound.apply(SysOp::RemoveLink(removed.id));
    let saved = deletion.inverse(&bound.sources, &bound.map);
    let fresh = splice(&after.sources, &after.map, &after.design, &inverse).expect("reinserting a formerly bound link has a splice");
    let mut restored = after.sources.clone();
    restored.apply(&saved.edits).expect("the saved inverse restores the binding");
    assert_eq!(restored, bound.sources, "the text inverse restores even an unused binding byte for byte");
    let mut rewritten = after.sources.clone();
    rewritten.apply(&fresh.edits).expect("fresh InsertLink can rewrite the model link");
    assert_eq!(rewritten, before.sources, "fresh InsertLink loses an unused binding because Link stores no name");
}

#[test]
fn a_commit_of_several_link_ops_is_one_edit_set() {
    let before = State::fixture("pair");
    let unbound = before.design.links[1].clone();
    let fresh = |state: &State, n: u32| Link { id: LinkId(state.next_id().0 + n), ..unbound.clone() };
    let (two, edit, _) = before.commit(vec![SysOp::AddLink(fresh(&before, 0)), SysOp::AddLink(fresh(&before, 1))]);
    assert_eq!(edit.edits.len(), 1, "two links added at one place are one insertion");
    assert_eq!(edit.adds.iter().map(|(sel, _)| sel.clone()).collect::<Vec<_>>(), vec![Sel::Link(fresh(&before, 0).id), Sel::Link(fresh(&before, 1).id)], "each added link gets its own range, in model order");
    assert_eq!(two.design.links.len(), 4, "both links are in the model");

    let (_, edit, _) = before.commit(vec![SysOp::RemoveLink(unbound.id), SysOp::AddLink(fresh(&before, 0))]);
    assert_eq!(edit.edits.len(), 2, "a removal and an addition are one deletion and one insertion");
    assert_eq!(edit.removes, vec![Sel::Link(unbound.id)], "the removed link's selection is dropped");

    let mut wire = before.design.links[0].clone();
    wire.kind = LinkKind::Wire;
    let (_, edit, _) = before.commit(vec![SysOp::InsertLink { link: fresh(&before, 0), pos: 0 }, SysOp::ReplaceLink(wire)]);
    assert_eq!(edit_text(&before, &edit), vec!["", "mate"], "an insertion before a link and that link's changed callee are both kept");

    let (_, edit, _) = before.commit(vec![SysOp::AddLink(fresh(&before, 0)), SysOp::RemoveLink(fresh(&before, 0).id)]);
    assert_eq!(edit, SpanEdit::default(), "a link added and removed in one commit leaves the text alone");

    let error = splice(&before.sources, &before.map, &before.design, &[
        DesignOp::Sys(SysOp::RemoveLink(unbound.id)),
        DesignOp::Sys(SysOp::InsertLink { link: unbound.clone(), pos: 0 }),
    ]).expect_err("moving a kept link within one commit is refused");
    assert!(error.message.contains("moves a link"), "the refusal names the move: {}", error.message);
    assert_eq!(located(&error, before.doc), 0..before.text().len() as u32, "the move refusal selects the system document");
}

#[test]
fn replacements_touch_only_changed_tokens_and_keep_bindings() {
    let before = State::fixture("pair");
    let mut link = before.design.links[0].clone();
    let repeated = before.design.units.iter().find_map(|u| u.circuit.comps.iter().find_map(|c| {
        c.part.pins.iter().enumerate().find(|(i, p)| c.part.pins[..*i].iter().any(|q| q.name == p.name))
            .map(|(i, _)| PinRef { board: u.id, comp: c.id, pin: PinIx(i as u16) })
    }));
    let LinkEnd::Comp(b) = link.b else { panic!("the fixture mate ends at a connector"); };
    let pin = repeated.unwrap_or(PinRef { board: b.board, comp: b.comp, pin: PinIx(1) });
    let unit = before.design.unit(pin.board).expect("the chosen pin's unit exists");
    let comp = &unit.circuit.comps[pin.comp.ix()];
    let name = &comp.part.pin_def(pin.pin).expect("the chosen pin exists").name;
    let occurrence = comp.part.pins[..pin.pin.ix()].iter().filter(|p| p.name == *name).count();
    let pin_arg = if repeated.is_some() { format!("{}, {occurrence}", quote(name)) } else { quote(name) };
    let mapped_pin = if repeated.is_some() { format!("[{}, {occurrence}]", quote(name)) } else { quote(name) };
    let end = format!("{}.{}.pin({pin_arg})", unit.name, comp.path);
    link.b = LinkEnd::Pin(pin);
    let (changed_end, edit, _) = before.apply(SysOp::ReplaceLink(link.clone()));
    assert_eq!(edit_text(&before, &edit), vec!["probe.j"], "changing end b replaces only its original expression");
    assert_eq!(edit.edits[0].text, end, "end b writes a pin by name and occurrence when repeated");
    assert!(changed_end.text().starts_with("let dbg = mate(ledctl.mcu.dbg, "), "the binding, callee and end a stay byte-identical");
    assert!(edit.adds.is_empty() && edit.removes.is_empty(), "replacing an end only shifts existing statement spans");

    link.pinmap = PinMap::Given(vec![(PinIx(0), pin.pin)]);
    let (with_pins, edit, _) = changed_end.apply(SysOp::ReplaceLink(link.clone()));
    assert_eq!(edit.edits.len(), 1, "adding pins is one token insertion");
    assert!(edit.edits[0].span.is_empty(), "adding pins replaces no existing bytes");
    assert_eq!(edit.edits[0].text, format!(", {{pins: [[\"1\", {mapped_pin}]]}}"), "pins use names and retain a repeated name's occurrence");
    let pins_at = edit.edits[0].span.start;
    link.pinmap = PinMap::Given(Vec::new());
    let (empty_pins, edit, _) = with_pins.apply(SysOp::ReplaceLink(link.clone()));
    assert_eq!(edit_text(&with_pins, &edit), vec![format!("{{pins: [[\"1\", {mapped_pin}]]}}")], "replacing pins selects only the third argument");
    assert_eq!(edit.edits[0].text, "{pins: []}", "an explicit empty pin map stays explicit");
    link.pinmap = PinMap::Derived;
    let (without_pins, edit, _) = empty_pins.apply(SysOp::ReplaceLink(link.clone()));
    assert_eq!(edit_text(&empty_pins, &edit), vec![", {pins: []}"], "removing pins deletes the third argument and its separator");
    assert_eq!(edit.edits[0].span.start, pins_at, "removing pins begins immediately after end b");
    assert_eq!(edit.edits[0].text, "", "removing pins inserts no replacement bytes");
    assert_eq!(without_pins.text(), changed_end.text(), "adding and removing pins preserves both ends and the binding");
    link.kind = LinkKind::Wire;
    let (wire, edit, _) = without_pins.apply(SysOp::ReplaceLink(link.clone()));
    assert_eq!(edit_text(&without_pins, &edit), vec!["mate"], "changing kind replaces only the callee token");
    assert_eq!(edit.edits[0].text, "wire", "the new callee spells the model kind");
    assert!(wire.text().starts_with("let dbg = wire("), "changing kind preserves the bound link's let");
    let (_, same, _) = wire.apply(SysOp::ReplaceLink(link));
    assert_eq!(same, SpanEdit::default(), "replacing a link with itself touches no text or map entries");
}

#[test]
fn mount_pose_edits_touch_only_changed_tokens_and_fields() {
    let before = State::with_system("pair", None);
    let mut mount = before.mount("probe");
    mount.xform.t[0] = 12_500_000;
    let (x, edit, _) = before.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&before, &edit), vec!["10mm"], "an X drag replaces exactly one length token");
    assert_eq!(edit.edits[0].text, "12.5mm", "the dragged length uses the writer's decimal spelling");

    mount.xform.t = [15_000_000, 0, -6_500_000];
    let (plane, edit, _) = x.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&x, &edit), vec!["12.5mm", "-8mm"], "an XZ drag replaces just the two changed components");
    assert_eq!(edit.edits.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), vec!["15mm", "-6.5mm"], "both plane coordinates use canonical lengths");

    mount.xform = Xform3::from_euler(mount.xform.t, [0, 0, 90_000_000]);
    let (z, edit, _) = plane.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit.edits.len(), 1, "the first rotation adds one field");
    let close = plane.text().rfind('}').expect("mount options have a closing brace") as u32;
    assert_eq!(edit.edits[0].span, close..close, "a Z rotation inserts immediately before the closing brace");
    assert_eq!(edit.edits[0].text, " rot: 90deg", "a Z-only rotation is one angle and its separating space");

    mount.xform = Xform3::from_euler(mount.xform.t, [0, 0, 45_000_000]);
    let (z_changed, edit, _) = z.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&z, &edit), vec!["90deg"], "a scalar rotation change replaces only its angle");
    assert_eq!(edit.edits[0].text, "45deg", "the replacement scalar uses the angle writer");

    mount.xform = Xform3::from_euler(mount.xform.t, [90_000_000, 0, 45_000_000]);
    let (list, edit, _) = z_changed.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&z_changed, &edit), vec!["45deg"], "changing rotation shape replaces only the field value");
    assert_eq!(edit.edits[0].text, "[90deg, 0deg, 45deg]", "an X rotation writes the three-angle shape");

    mount.xform = Xform3::from_euler(mount.xform.t, [45_000_000, 0, 90_000_000]);
    let (list_changed, edit, _) = list.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&list, &edit), vec!["90deg", "45deg"], "a list keeps its shape and edits only changed angle tokens");
    assert_eq!(edit.edits.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), vec!["45deg", "90deg"], "the changed X and Z angles retain list order");

    mount.xform = Xform3::from_euler(mount.xform.t, [0, 0, 90_000_000]);
    let (scalar, edit, _) = list_changed.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&list_changed, &edit), vec!["[45deg, 0deg, 90deg]"], "returning to a scalar replaces the whole list value once");
    assert_eq!(edit.edits[0].text, "90deg", "a Z-only pose returns to the scalar spelling");

    mount.xform = Xform3::from_euler(mount.xform.t, [0; 3]);
    let (no_rot, edit, _) = scalar.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&scalar, &edit), vec![" rot: 90deg"], "zero rotation removes the field and its separating space");
    assert_eq!(edit.edits[0].text, "", "zero rotation leaves no replacement field");

    mount.xform.t = [0; 3];
    let (no_at, edit, _) = no_rot.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&no_rot, &edit), vec![" at: [15mm, 0mm, -6.5mm]"], "zero translation removes at and its separating space");
    assert_eq!(edit.edits[0].text, "", "zero translation leaves only the parent field");

    mount.xform = Xform3::from_euler([0; 3], [0, 0, 90_000_000]);
    let (rot_only, _, _) = no_at.apply(SysOp::SetMount(mount.clone()));
    mount.xform.t = [1_000_000, 0, 0];
    let (with_at, edit, _) = rot_only.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit.edits.len(), 1, "adding at beside rot needs one insertion");
    assert!(edit.edits[0].span.is_empty(), "adding at touches no existing field tokens");
    assert_eq!(edit.edits[0].text, " at: [1mm, 0mm, 0mm]", "at is inserted before the existing rot separator");

    let parent = mount.parent.clone();
    mount.parent = Frame::World;
    let (world, edit, _) = with_at.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit_text(&with_at, &edit), vec!["on: ledctl "], "moving to the world removes on and its following separator");
    assert_eq!(edit.edits[0].text, "", "a world parent has no on field");
    mount.parent = parent;
    let (parented, edit, _) = world.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit.edits.len(), 1, "adding on needs one insertion");
    assert_eq!(edit.edits[0].text, "on: ledctl ", "an added parent is the first field");
    assert!(edit.edits[0].span.is_empty(), "adding the parent preserves existing at and rot tokens");
    assert_eq!(parented.text(), with_at.text(), "removing and restoring the parent restores field order");

    mount.xform = Xform3::IDENTITY;
    let (reset, edit, _) = parented.apply(SysOp::SetMount(mount));
    assert_eq!(edit_text(&parented, &edit), vec![" at: [1mm, 0mm, 0mm] rot: 90deg"], "adjacent removed fields share one disjoint deletion");
    assert_eq!(reset.text(), no_at.text(), "removing both pose fields leaves the original parent-only statement");
    let (_, edit, _) = reset.apply(SysOp::SetMount(reset.mount("probe")));
    assert_eq!(edit, SpanEdit::default(), "setting the stored mount again is a text and map no-op");
}

#[test]
fn mount_lines_insert_in_unit_order_and_defaults_delete_them() {
    let before = State::with_system("pair", None);
    let probe = before.mount("probe");
    let mut root = before.mount("ledctl");
    root.xform.t[0] = 1_000_000;
    let at = before.text().find("mount(").expect("the pair has a mount") as u32;
    let (two, edit, _) = before.apply(SysOp::SetMount(root.clone()));
    assert_eq!(edit.edits, vec![TextEdit { doc: before.doc, span: at..at, text: "mount(ledctl, {at: [1mm, 0mm, 0mm]})\n".into() }], "a first mount inserts one whole line before the next unit's mount");
    let (one, edit, _) = two.apply(SysOp::ClearMount(probe.unit));
    assert_eq!(edit_text(&two, &edit), vec!["mount(probe, {on: ledctl at: [10mm, 0mm, -8mm]})\n"], "ClearMount deletes the whole statement and newline");
    assert_eq!(edit.edits[0].text, "", "ClearMount inserts no text");
    let (restored, edit, _) = one.apply(SysOp::SetMount(probe.clone()));
    let end = one.text().len() as u32;
    assert_eq!(edit.edits[0].span, end..end, "a later unit's first mount follows the last mount line");
    assert_eq!(restored.text(), two.text(), "restoring the later mount recovers the same unit order");

    let default = Mount { unit: probe.unit, parent: Frame::World, xform: Xform3::IDENTITY };
    let (one_again, edit, _) = restored.apply(SysOp::SetMount(default));
    assert_eq!(edit_text(&restored, &edit), vec!["mount(probe, {on: ledctl at: [10mm, 0mm, -8mm]})\n"], "SetMount to the default deletes the same whole line");
    assert_eq!(one_again.text(), one.text(), "SetMount(default) and ClearMount produce identical text");
    let (no_mounts, _, _) = one_again.apply(SysOp::ClearMount(root.unit));
    let end = no_mounts.text().len() as u32;
    let (only_probe, edit, _) = no_mounts.apply(SysOp::SetMount(probe.clone()));
    assert_eq!(edit.edits.len(), 1, "the first mount after links is one inserted line");
    assert_eq!(edit.edits[0].span, end..end, "without mounts the first mount follows the final link's line");
    assert_eq!(only_probe.text(), before.text(), "the stock mount restores the canonical pair");

    let (both, edit, _) = no_mounts.commit(vec![SysOp::SetMount(probe), SysOp::SetMount(root)]);
    assert_eq!(edit.edits.len(), 1, "mounts added at one offset are one insertion");
    assert_eq!(both.text(), two.text(), "mount insertion order follows unit ids even when op order is reversed");
    let (_, edit, _) = no_mounts.apply(SysOp::ClearMount(no_mounts.mount("probe").unit));
    assert_eq!(edit, SpanEdit::default(), "clearing an already default mount changes no text");

    let empty = State::fixture("ledctl");
    let mut mount = empty.mount("ledctl");
    mount.xform.t[0] = 1_000_000;
    let (first, edit, _) = empty.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit.edits, vec![TextEdit { doc: empty.doc, span: 0..0, text: first.text().into() }], "a mount can be the first line of an empty system document");
    let (cleared, edit, _) = first.apply(SysOp::ClearMount(mount.unit));
    assert_eq!(edit_text(&first, &edit), vec![first.text()], "clearing the only mount consumes its full line");
    assert_eq!(cleared.text(), "", "clearing the only system statement restores empty text");
}

#[test]
fn mount_link_bindings_replace_whole_statements_and_keep_map_starts() {
    let fixture = State::with_system("pair", None);
    let mut mount = fixture.mount("probe");
    mount.xform = Xform3::IDENTITY;
    let (before, _, _) = fixture.apply(SysOp::SetMount(mount.clone()));
    let link = before.design.links[0].clone();
    let range = before.map.locate(&Sel::Link(link.id)).expect("the unbound link is mapped");
    mount.parent = Frame::Mate(link.id);
    let (bound, edit, _) = before.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit.edits.len(), 2, "mounting on a link changes its whole statement and the on token");
    assert_eq!(edit.edits[0].span, range.span, "adding let replaces the entire statement instead of inserting at its start");
    assert_eq!(edit.edits[0].text, "let dbg = mate(ledctl.mcu.dbg, probe.j)", "the new binding uses the writer's component-segment name");
    assert_eq!(edit_text(&before, &edit)[1], "ledctl", "moving onto a link replaces only the parent value");
    assert_eq!(edit.edits[1].text, "dbg", "on names the new binding");
    assert_eq!(bound.design.links[0].id, link.id, "adding a binding retains the LinkId");
    assert_eq!(bound.map.locate(&Sel::Link(link.id)).expect("the bound link remains mapped").span.start, range.span.start, "map.follow keeps the statement start at let");
    assert!(edit.adds.is_empty() && edit.removes.is_empty(), "a binding change follows the existing selection without replacing it");

    mount.parent = before.mount("probe").parent;
    let bound_range = bound.map.locate(&Sel::Link(link.id)).expect("the bound statement is mapped");
    let (unbound, edit, _) = bound.apply(SysOp::SetMount(mount.clone()));
    assert_eq!(edit.edits[0].span, bound_range.span, "the last mount leaving removes let by whole-statement replacement");
    assert_eq!(edit.edits[0].text, "mate(ledctl.mcu.dbg, probe.j)", "leaving the link writes the bare call");
    assert_eq!(unbound.text(), before.text(), "moving back to the unit restores the original canonical text");
    assert_eq!(unbound.map.locate(&Sel::Link(link.id)), Some(range.clone()), "removing let preserves the id and restores its exact range");

    let added = Link { id: before.next_id(), kind: LinkKind::Wire, ..link.clone() };
    mount.parent = Frame::Mate(added.id);
    let (added_state, edit, _) = before.commit(vec![SysOp::AddLink(added.clone()), SysOp::SetMount(mount.clone())]);
    assert_eq!(edit.edits[0].text, "let dbg = wire(ledctl.mcu.dbg, probe.j)\n", "a link added and mounted in one commit is inserted with its binding");
    assert!(edit.edits[0].span.is_empty(), "a new bound link is one line insertion");
    let added_range = added_state.map.locate(&Sel::Link(added.id)).expect("the newly bound link is mapped");
    assert_eq!(edit.adds, vec![(Sel::Link(added.id), added_range.clone())], "the bound insertion records its new LinkId exactly once");
    assert_eq!(&added_state.text()[added_range.span.start as usize..added_range.span.end as usize], "let dbg = wire(ledctl.mcu.dbg, probe.j)", "the added range includes let but excludes the newline");

    let (no_mount, _, _) = before.apply(SysOp::ClearMount(mount.unit));
    let (_, edit, _) = no_mount.commit(vec![SysOp::AddLink(added), SysOp::SetMount(mount.clone())]);
    assert_eq!(edit.edits.len(), 1, "a new link and the first mount share one insertion when their anchors coincide");
    assert_eq!(edit.edits[0].text, "let dbg = wire(ledctl.mcu.dbg, probe.j)\nmount(probe, {on: dbg})\n", "a shared insertion puts the bound link before its mount");

    let changed = Link { kind: LinkKind::Wire, ..link.clone() };
    mount.parent = Frame::Mate(link.id);
    let (replaced, edit, _) = before.commit(vec![SysOp::ReplaceLink(changed), SysOp::SetMount(mount.clone())]);
    assert_eq!(edit.edits.len(), 2, "a kind change and new binding share one statement replacement plus the mount's on edit");
    assert_eq!(edit.edits[0].span, range.span, "the combined change covers the old whole statement once");
    assert_eq!(edit.edits[0].text, "let dbg = wire(ledctl.mcu.dbg, probe.j)", "the whole replacement contains both the new binding and kind");
    assert_eq!(replaced.map.locate(&Sel::Link(link.id)).expect("the replaced link remains mapped").span.start, range.span.start, "the combined replacement preserves the mapped start");
    mount.xform.t[0] = 1_000_000;
    let (_, edit, _) = replaced.apply(SysOp::SetMount(mount));
    assert_eq!(edit.edits.len(), 1, "a pose change on an already bound link leaves its name and statement alone");
    assert_eq!(edit.edits[0].text, " at: [1mm, 0mm, 0mm]", "the existing binding is reused when the mount's pose changes");
}

#[test]
fn removing_a_mounted_link_moves_its_mount_in_the_same_commit() {
    let before = State::fixture("pair");
    let link = before.design.links[0].clone();
    let mut mount = before.mount("probe");
    let LinkEnd::Comp(end) = link.a else { panic!("the fixture mate has a component end"); };
    mount.parent = Frame::Unit(end.board);
    let (after, edit, _) = before.commit(vec![SysOp::SetMount(mount.clone()), SysOp::RemoveLink(link.id)]);
    assert_eq!(edit_text(&before, &edit), vec!["let dbg = mate(ledctl.mcu.dbg, probe.j)\n", "dbg"], "removal deletes the bound line and changes only the mount's on token");
    assert_eq!(edit.edits.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), vec!["", "ledctl"], "the removed binding leaves with its link and the mount names its unit");
    assert_eq!(edit.removes, vec![Sel::Link(link.id)], "the removed link's map entry is dropped once");
    assert!(edit.adds.is_empty(), "moving a mount off a removed link creates no new link selection");
    assert_eq!(after.mount("probe"), mount, "removing the link keeps the stored pose on the replacement parent");
    assert!(after.map.locate(&Sel::Link(link.id)).is_none(), "the deleted bound statement has no stale map entry");
}

#[test]
fn refusals_preserve_the_model_and_locate_unavailable_splices() {
    let before = State::fixture("pair");
    let link = &before.design.links[0];
    assert!(before.design.assembly.mounts.iter().any(|m| m.parent == Frame::Mate(link.id)), "the refusal case has a mount on the bound link");
    let mut model = before.design.clone();
    let error = model.apply(&DesignOp::Sys(SysOp::RemoveLink(link.id)), false).expect_err("the model refuses removal while a mount uses the link");
    assert!(matches!(error, fab_elec_design::ops::DesignOpError::LinkInUse { link: id, .. } if id == link.id), "removing a mounted link reports LinkInUse");
    assert_eq!(model, before.design, "a refused removal changes nothing, including generation, and needs no splice");
    let missing = before.design.links[1].id;
    let mut map = before.map.clone();
    map.follow(&SpanEdit { removes: vec![Sel::Link(missing)], ..SpanEdit::default() });
    // A replacement that changes nothing edits no text, so the refused replacement changes the kind.
    let mut changed = before.design.links[1].clone();
    changed.kind = LinkKind::Mate;
    for op in [SysOp::RemoveLink(missing), SysOp::ReplaceLink(changed)] {
        let error = splice(&before.sources, &map, &before.design, &[DesignOp::Sys(op)]).expect_err("a link without a statement is refused");
        assert!(error.message.contains("has no statement range"), "the refusal explains the missing link statement");
        assert_eq!(located(&error, before.doc), 0..before.text().len() as u32, "a missing statement still has a source-located refusal");
    }
    let mut sources = before.sources.clone();
    let start = before.text().find("mount(").expect("the pair has a mount") as u32;
    sources.apply(&[TextEdit { doc: before.doc, span: start..before.text().len() as u32, text: String::new() }]).expect("only the mount statement is removed");
    let mut mount = before.mount("probe");
    mount.xform.t[0] += 1_000_000;
    for op in [SysOp::ClearMount(mount.unit), SysOp::SetMount(mount)] {
        let error = splice(&sources, &before.map, &before.design, &[DesignOp::Sys(op)]).expect_err("a non-default mount without a statement is refused");
        assert!(error.message.contains("mount for unit") && error.message.contains("has no statement"), "the refusal identifies the missing mount statement");
        assert_eq!(located(&error, before.doc), 0..start, "the missing mount refusal selects the current system document");
    }

    // Only the edited unit's own mount statement must be editable.
    let mut broken = before.sources.clone();
    let end = before.text().len() as u32;
    let ghost = "mount(ghost, {at: [1mm, 0mm, 0mm]})\n";
    broken.apply(&[TextEdit { doc: before.doc, span: end..end, text: ghost.into() }]).expect("a dangling mount is appended");
    let mut link = before.design.links[1].clone();
    link.id = before.next_id();
    let edit = splice(&broken, &before.map, &before.design, &[DesignOp::Sys(SysOp::AddLink(link))])
        .expect("a mount whose unit does not resolve does not refuse a link commit");
    assert_eq!(edit.edits.len(), 1, "the link commit is its one insertion; the dangling mount stays as written");
    let second = before.text().find("mount(").expect("the pair has a mount");
    let probe_mount = &before.text()[second..];
    let mut twice = before.sources.clone();
    twice.apply(&[TextEdit { doc: before.doc, span: end..end, text: probe_mount.into() }]).expect("a second probe mount is appended");
    let mut mount = before.mount("probe");
    mount.xform.t[0] += 1_000_000;
    let error = splice(&twice, &before.map, &before.design, &[DesignOp::Sys(SysOp::SetMount(mount))])
        .expect_err("editing a unit with two mount statements is refused");
    assert!(error.message.contains("second mount statement"), "the refusal names the second statement: {}", error.message);
    assert_eq!(located(&error, before.doc), end..end + probe_mount.trim_end().len() as u32, "the refusal selects the second statement");
}

#[test]
fn loading_a_folder_adds_one_empty_system_document() {
    struct Folder(std::path::PathBuf);
    impl Drop for Folder {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("the clock follows the epoch").as_nanos();
    let name = format!("system-splice-{}-{stamp}", std::process::id());
    let path = std::env::temp_dir().join(&name);
    std::fs::create_dir(&path).expect("the test owns a unique temporary project folder");
    let folder = Folder(path.clone());
    let fixture = fab_elec_fixtures::docs("ledctl").expect("the controller has stock documents");
    let mut without_system = Sources::new();
    for doc in fixture.docs().iter().filter(|d| d.kind != DocKind::System) {
        std::fs::write(path.join(&doc.name), doc.text.as_bytes()).expect("a stock document is written to the temporary project");
        without_system.add(doc.name.clone(), doc.text.clone()).expect("stock names are distinct");
    }
    let loaded = load_dir(&path).expect("the project folder loads");
    let systems: Vec<_> = loaded.docs().iter().copied().filter(|d| d.kind == DocKind::System).collect();
    assert_eq!(systems.len(), 1, "a folder without a system file gets exactly one system document");
    assert_eq!(systems[0].name, SYSTEM_DOC, "the implicit system document uses the fixed name independent of the folder");
    assert_eq!(systems[0].text.as_ref(), "", "the implicit system document is empty");
    assert!(!systems[0].is_file(), "the implicit empty system document is not a file");
    assert!(!path.join(&systems[0].name).exists(), "loading an empty system document writes no file");
    assert_eq!(loaded.docs().len(), without_system.docs().len() + 1, "only the implicit system document is added");
    for doc in without_system.docs() {
        let id = loaded.find(&doc.name).expect("every original document is loaded");
        assert_eq!(loaded.doc(id).expect("the loaded document exists").text, doc.text, "loading preserves every byte of {}", doc.name);
    }
    let libs = std_libs();
    let baseline = evaluate(&without_system, &libs).expect("the project without system text evaluates");
    let ev = evaluate(&loaded, &libs).expect("the loaded project evaluates");
    assert_eq!(ev.system.doc, Some(systems[0].id), "evaluation records the empty system document");
    assert!(ev.system.diags.is_empty(), "the empty system document has no diagnostics");
    assert_eq!(ev.design.links, baseline.design.links, "the empty document preserves the default links");
    assert_eq!(ev.design.assembly.mounts, baseline.design.assembly.mounts, "the empty document preserves all default mounts");
    assert_eq!(ev.design.units, baseline.design.units, "the empty document preserves all evaluated unit content");
    assert!(ev.units.iter().all(|u| u.state == UnitState::Built), "the controller unit still builds");
    assert!(ev.benches.iter().all(|b| b.state == UnitState::Built), "the controller benches still build");
    assert_eq!(sys_doc::write(&ev.design), "", "the default system still writes empty text");
    let doc = systems[0].id;
    let before = State { sources: loaded, map: ev.map, design: ev.design, doc };
    let unit = &before.design.units[0];
    let end = |i: usize| LinkEnd::Comp(CompRef { board: unit.id, comp: unit.circuit.comps[i].id });
    let link = Link { id: before.next_id(), kind: LinkKind::Wire, a: end(0), b: end(1), pinmap: PinMap::Derived };
    let (linked, _, _) = before.apply(SysOp::AddLink(link));
    assert!(linked.sources.doc(doc).expect("the system document remains").is_file(), "the system document is a file after a link splice is applied");
    std::fs::write(path.join("existing.sys.splash"), "").expect("an explicit empty system file can be loaded");
    let existing_id = DocId(without_system.docs().iter().filter(|d| d.name.as_str() < "existing.sys.splash").count() as u32);
    let explicit = load_dir(&path).expect("the project with a system file loads");
    let systems: Vec<_> = explicit.docs().iter().copied().filter(|d| d.kind == DocKind::System).collect();
    assert_eq!(systems.len(), 1, "an existing system file prevents another implicit document");
    assert_eq!(systems[0].name, SYSTEM_DOC, "an existing empty system document takes the fixed name");
    assert_eq!(systems[0].id, existing_id, "renaming keeps the id assigned to the file in sorted load order");
    assert_eq!(explicit.find(SYSTEM_DOC), Some(existing_id), "the fixed name finds the same document");
    assert_eq!(explicit.find("existing.sys.splash"), None, "the old name no longer finds a document");
    assert!(!systems[0].is_file(), "an existing empty system document is not a file, whatever its name");
    let text = "// Keep this system document's name.\n";
    std::fs::write(path.join("existing.sys.splash"), text).expect("an explicit system file with text can be loaded");
    let explicit = load_dir(&path).expect("the project with non-empty system text loads");
    let systems: Vec<_> = explicit.docs().iter().copied().filter(|d| d.kind == DocKind::System).collect();
    assert_eq!(systems.len(), 1, "a non-empty system file prevents an implicit document");
    assert_eq!(systems[0].name, "existing.sys.splash", "a non-empty system document keeps its name");
    assert_eq!(systems[0].id, existing_id, "the non-empty system file retains its loaded id");
    assert_eq!(systems[0].text.as_ref(), text, "loading preserves the system comment");
    assert!(systems[0].is_file(), "a comment is non-empty text, so the system document is a file");
    assert!(sys_doc::lower(systems[0].id, text, &baseline.design.units, None).diags.is_empty(), "the comment is valid system text");
    drop(folder);
    assert!(!path.exists(), "the temporary project folder is removed at the end");
}

/// The project's run settings are one `sim` statement in the system document (`sys_sim`): read
/// with frequencies in their units, left alone by the lowering and the splices (a mount edit keeps
/// it), written by the pickers (added first, replaced, removed with its line), and every mistake
/// in it is one located diag that leaves the settings not chosen.
#[test]
fn the_sim_statement_holds_the_run_settings_beside_the_system() {
    use fab_elec_lang::sys_sim::{self, AudioLoad, SimMode, SimSettings};
    let mut sources = fab_elec_fixtures::docs("pair").expect("the pair has documents");
    let doc = sources.docs().iter().find(|d| d.kind == DocKind::System).expect("the pair has a system document").id;
    let pair = sources.doc(doc).expect("the system exists").text.to_string();
    let picked = SimSettings { mode: Some(SimMode::Exact), sample_rate: Some(44_100.0), ..SimSettings::default() };
    let edits = sys_sim::edit(doc, &pair, &picked).expect("a parsing system takes the settings");
    assert_eq!(edits.len(), 1, "one edit adds the statement");
    sources.apply(&edits).expect("the settings edit applies");
    let text = sources.doc(doc).expect("the system exists").text.to_string();
    assert_eq!(text, format!("sim({{mode: exact sample_rate: 44.1kHz}})\n{pair}"), "the statement is added as the first line, its rate in kHz");
    let read = sys_sim::read(doc, &text);
    assert_eq!((&read.settings, read.diags.len()), (&picked, 0), "the statement reads back exactly what was picked");
    assert_eq!(read.span, Some(0..text.find('\n').unwrap() as u32), "the statement's span is its call");

    // the lowering skips it and the splices keep it
    let ev = evaluate(&sources, &std_libs()).expect("the pair with run settings evaluates");
    assert!(ev.system.diags.is_empty(), "the lowering takes the sim statement as no declaration: {:?}", ev.system.diags);
    assert_eq!(check_system(&sources, &ev.design, &ev.map), Ok(()), "the system check reads past it");
    let probe = ev.design.units.iter().find(|u| u.name == "probe").expect("the pair has the probe").id;
    let mut mount = ev.design.assembly.mounts.iter().find(|m| m.unit == probe).expect("the probe is mounted").clone();
    mount.xform.t[0] = 12_500_000;
    let op = DesignOp::Sys(SysOp::SetMount(mount));
    let edit = splice(&sources, &ev.map, &ev.design, std::slice::from_ref(&op)).expect("a mount edit splices beside the settings");
    let mut design = ev.design.clone();
    design.apply(&op, false).expect("the model takes the mount");
    sources.apply(&edit.edits).expect("the mount edit applies");
    let mut map = ev.map.clone();
    map.follow(&edit);
    let text = sources.doc(doc).expect("the system exists").text.to_string();
    assert!(text.starts_with("sim({mode: exact sample_rate: 44.1kHz})\n") && text.contains("12.5mm"), "the statement stays as the mount moves:\n{text}");
    assert_eq!(check_system(&sources, &design, &map), Ok(()), "the spliced system agrees with the model");

    // replaced in place, then removed with its line: the rest is the canonical system
    let fast = SimSettings { mode: Some(SimMode::Fast), ..SimSettings::default() };
    let edits = sys_sim::edit(doc, &text, &fast).expect("the settings change");
    sources.apply(&edits).expect("the change applies");
    let text = sources.doc(doc).expect("the system exists").text.to_string();
    assert!(text.starts_with("sim({mode: fast})\n"), "a rate left out is not written:\n{text}");
    assert_eq!(sys_sim::read(doc, &text).settings, fast, "the replaced statement reads back");
    let edits = sys_sim::edit(doc, &text, &SimSettings::default()).expect("nothing chosen");
    sources.apply(&edits).expect("the removal applies");
    assert_eq!(&*sources.doc(doc).expect("the system exists").text, sys_doc::write(&design), "with nothing chosen the statement and its line go");
    assert_eq!(sys_sim::edit(doc, "", &SimSettings::default()), Ok(vec![]), "nothing chosen in a system without the statement is no edit");

    // rates read back exactly; a rate between the picker's is written in Hz
    for (literal, hz) in [("44.1kHz", 44_100.0), ("88.2kHz", 88_200.0), ("192kHz", 192_000.0), ("22050Hz", 22_050.0)] {
        assert_eq!(sys_sim::rate_literal(hz), literal, "{hz} Hz is written {literal}");
        let read = sys_sim::read(doc, &format!("sim({{sample_rate: {literal}}})\n"));
        assert_eq!((read.settings.sample_rate, read.diags.len()), (Some(hz), 0), "{literal} reads back as {hz} Hz");
    }

    // each mode is written by its word and reads back
    for (mode, word) in [(SimMode::Exact, "exact"), (SimMode::Fast, "fast"), (SimMode::Logic, "logic")] {
        let line = sys_sim::statement(&SimSettings { mode: Some(mode), ..SimSettings::default() }).expect("a mode is a choice");
        assert_eq!(line, format!("sim({{mode: {word}}})"), "{word} is written by its word");
        assert_eq!(sys_sim::read(doc, &format!("{line}\n")).settings.mode, Some(mode), "{word} reads back");
    }

    // the recording rate is written after the sample rate, and reads back apart from it
    let rates = SimSettings { sample_rate: Some(48_000.0), recording_rate: Some(44_100.0), ..SimSettings::default() };
    let line = sys_sim::statement(&rates).expect("rates are choices");
    assert_eq!(line, "sim({sample_rate: 48kHz recording_rate: 44.1kHz})", "the recording rate follows the sample rate, in kHz");
    assert_eq!(sys_sim::read(doc, &format!("{line}\n")).settings, rates, "{line} reads back");

    // the default load and the loads of individual jacks read back independently
    for load in [AudioLoad::Open, AudioLoad::Line, AudioLoad::Phones, AudioLoad::Ohms(2.2)] {
        let settings = SimSettings { audio_load: Some(load), ..SimSettings::default() };
        let line = sys_sim::statement(&settings).expect("a load is a choice");
        assert_eq!(line, format!("sim({{audio_load: {}}})", load.literal()), "the load is written by its literal");
        assert_eq!(sys_sim::read(doc, &format!("{line}\n")).settings, settings, "{line} reads back");
    }
    let chosen = "sim({jack_loads: {J5: 0.6kohm J3: phones J4: open J2: line} audio_load: phones})";
    let loads = sys_sim::read(doc, chosen);
    assert!(loads.diags.is_empty(), "the jack loads parse: {:?}", loads.diags);
    assert_eq!(loads.settings.jack_loads, [
        ("J2".into(), AudioLoad::Line), ("J3".into(), AudioLoad::Phones),
        ("J4".into(), AudioLoad::Open), ("J5".into(), AudioLoad::Ohms(600.0)),
    ], "each jack keeps its load in reference order");
    let line = sys_sim::statement(&loads.settings).expect("jack loads are choices");
    assert_eq!(line, "sim({audio_load: phones jack_loads: {J2: line J3: phones J4: open J5: 600ohm}})");
    assert_eq!(sys_sim::read(doc, &line).settings, loads.settings, "the default and every jack's load round trip");

    // the person's recording length reads back in seconds or milliseconds
    for (seconds, literal) in [(4.0, "4s"), (2.5, "2.5s"), (0.5, "500ms")] {
        let settings = SimSettings { recording_length: Some(seconds), ..SimSettings::default() };
        let line = sys_sim::statement(&settings).expect("a length is a choice");
        assert_eq!(line, format!("sim({{recording_length: {literal}}})"), "{seconds} s is written {literal}");
        assert_eq!(sys_sim::read(doc, &format!("{line}\n")).settings, settings, "{line} reads back");
    }

    // each mistake is one located diag, and its statement chooses nothing
    for (bad, says, at) in [
        ("sim({audio_load: speaker})\n", "audio_load is open, line, phones or a resistance", "speaker"),
        ("sim({mode: slow})\n", "mode is exact, fast or logic", "slow"),
        ("sim({rate: 96kHz})\n", "sim has no field rate: its fields are mode, sample_rate, recording_rate, audio_load, jack_loads, recording_length", "rate: 96kHz"),
        ("sim({recording_length: 4kHz})\n", "recording_length is a positive time", "4kHz"),
        ("sim({sample_rate: 96})\n", "sample_rate is a positive frequency", "96"),
        ("sim({recording_rate: fast})\n", "recording_rate is a positive frequency", "fast"),
        ("sim({mode: exact mode: fast})\n", "sim has mode twice", "mode: fast"),
        ("sim(fast)\n", "sim takes one options object", "fast"),
    ] {
        let read = sys_sim::read(doc, bad);
        assert_eq!((read.settings, read.diags.len()), (SimSettings::default(), 1), "{bad:?} chooses nothing, with one diag: {:?}", read.diags);
        let diag = &read.diags[0];
        assert!(diag.message.contains(says) && diag.message.contains("not chosen"), "{bad:?} says {says:?}: {}", diag.message);
        let span = located(diag, doc);
        assert_eq!(&bad[span.start as usize..span.end as usize], at, "{bad:?}'s diag sits on {at:?}");
    }
    let two = "sim({sample_rate: 96kHz})\nsim({mode: exact})\n";
    let read = sys_sim::read(doc, two);
    assert_eq!((read.settings.sample_rate, read.settings.mode, read.diags.len()), (Some(96_000.0), None, 1), "a second statement is not read, and says so");
    assert!(sys_sim::edit(doc, two, &picked).is_err(), "the pickers do not choose between two statements");
    let broken = format!("sim({{mode: exact\n{pair}");
    let read = sys_sim::read(doc, &broken);
    assert!(read.diags.len() == 1 && read.diags[0].message.contains("syntax"), "a sim line that does not parse is the settings' diag: {:?}", read.diags);
    let lowered = sys_doc::lower(doc, &broken, &ev.design.units, None);
    assert!(lowered.diags.is_empty(), "and not the lowering's too (one mistake, one diag): {:?}", lowered.diags);
    assert!(sys_sim::edit(doc, &broken, &picked).is_err(), "the pickers wait for a system that parses");
}

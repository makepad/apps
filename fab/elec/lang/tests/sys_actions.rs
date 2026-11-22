use fab_elec_core::{Diag, DocId, LinkId, PinIx, Sel, Severity, Xform3};
use fab_elec_design::action::{catalog, check_returns, FieldKind, Risk};
use fab_elec_design::ops::DesignOpError;
use fab_elec_design::{Design, DesignOp, Frame, LinkKind, Mount, PinMap, SysOp};
use fab_elec_lang::{evaluate, splice, std_libs, sys_actions::{register_actions, sys_ops, ACTIONS}, sys_doc::{self, SysLedger}, SourceMap, Sources, SpanEdit, UnitState};
use std::collections::HashSet;

// These catalogs depend only on design. Include their definitions to check names without
// adding the simulation or manufacturing crates to the language's test dependencies.
#[path = "../../cam/src/actions.rs"]
mod cam_actions;
#[path = "../../simhost/src/actions.rs"]
mod sim_actions;

fn commit(sources: &Sources, map: &SourceMap, design: &Design, doc: DocId, tool: &str, args: &[(&str, &str)]) -> (Design, Sources, SourceMap, SpanEdit) {
    let ops = sys_ops(design, tool, args).unwrap_or_else(|e| panic!("{tool} builds: {}", e.message));
    assert_eq!(ops.len(), 1, "{tool} builds exactly one op");
    assert!(matches!(ops[0], DesignOp::Sys(_)), "{tool} builds a system op");
    let mut after = design.clone();
    for op in &ops { after.apply(op, false).unwrap_or_else(|e| panic!("{tool} applies to the model: {e}")); }
    let edit = splice(sources, map, design, &ops).unwrap_or_else(|e| panic!("{tool} splices: {}", e.message));
    assert!(!edit.reevaluate, "{tool} needs no document reevaluation");
    let mut sources = sources.clone();
    sources.apply(&edit.edits).expect("the action's source edits apply");
    let mut map = map.clone();
    map.follow(&edit);
    let ledger = SysLedger::of_map(&map, doc);
    let lowered = sys_doc::lower(doc, &sources.doc(doc).expect("system document exists").text, &design.units, Some(&ledger));
    assert!(lowered.diags.is_empty(), "{tool} produces valid system text: {:?}", lowered.diags);
    assert_eq!(lowered.links, after.links, "{tool} splices the model's links and ids");
    assert_eq!(lowered.mounts, after.assembly.mounts, "{tool} splices the model's mounts");
    (after, sources, map, edit)
}

fn refused(design: &Design, tool: &str, args: &[(&str, &str)], field: &str, sel: &[Sel]) -> Diag {
    let diag = sys_ops(design, tool, args).expect_err("invalid action arguments must be refused");
    assert_eq!(diag.severity, Severity::Error, "{tool} {field} is an error");
    assert!(diag.message.starts_with(&format!("{field}: ")), "{tool} names the offending field first: {}", diag.message);
    assert_eq!(diag.sel, sel, "{tool} {field} selects the resolved model item, without a fabricated source range");
    diag
}

#[test]
fn system_actions_build_model_ops_and_matching_source_edits() {
    let names: HashSet<_> = ACTIONS.iter().map(|a| a.name).collect();
    assert_eq!(ACTIONS.len(), 5, "the system catalog has exactly five actions");
    assert_eq!(names, HashSet::from(["elec.link.add", "elec.link.remove", "elec.link.replace", "elec.unit.mount", "elec.unit.unmount"]), "the five system action names are unique and complete");
    // Registration replaces repeated names, so compare the definitions before registering.
    let all: Vec<_> = ACTIONS.iter().chain(cam_actions::ACTIONS).chain(sim_actions::ACTIONS).collect();
    assert_eq!(all.iter().map(|a| a.name).collect::<HashSet<_>>().len(), all.len(), "system, manufacturing and simulation catalogs have no overlapping names");
    for action in ACTIONS {
        assert!(check_returns(action.returns).is_ok(), "{} has a valid reply grammar", action.name);
        assert_eq!(action.group, "System", "{} belongs to the System group", action.name);
        assert_eq!(action.risk, Risk::Edit, "{} declares a design edit", action.name);
        assert!(action.fields.iter().all(|f| !matches!(f.kind, FieldKind::Sel(_) | FieldKind::Sels(_))), "{} uses names and numeric ids that tools can spell", action.name);
    }
    cam_actions::register_actions();
    sim_actions::register_actions();
    register_actions();
    let cat = catalog();
    assert_eq!(cat.iter().map(|a| a.name).collect::<HashSet<_>>().len(), cat.len(), "the registered catalog has no duplicate names");
    for action in all { assert!(cat.contains(action), "registration preserves {}", action.name); }

    let sources = fab_elec_fixtures::docs("pair").expect("the pair has documents");
    let ev = evaluate(&sources, &std_libs()).expect("the pair evaluates");
    assert!(ev.units.iter().all(|u| u.state == UnitState::Built), "both fixture units build");
    assert!(ev.system.diags.is_empty(), "the fixture system resolves: {:?}", ev.system.diags);
    let design = &ev.design;
    let before = design.clone();
    let doc = ev.system.doc.expect("the pair has a system document");
    let original = &sources.doc(doc).expect("the system document exists").text;
    let probe = design.unit_by_name("probe").expect("probe exists").id;
    let controller = design.unit_by_name("ledctl").expect("controller exists").id;
    let id = sys_doc::next_link_id(design.links.iter().map(|l| l.id));
    assert_eq!(id, LinkId(1), "the stock link with id zero is followed by id one");
    assert_eq!(sys_doc::next_link_id([]), LinkId(0), "an empty set starts at zero");
    assert_eq!(sys_doc::next_link_id([LinkId(7), LinkId(2)]), LinkId(8), "allocation uses the greatest identity, not the count or final position");
    let (added, added_sources, added_map, _) = commit(&sources, &ev.map, design, doc, "elec.link.add", &[
        ("kind", "@wire"), ("a", "ledctl.mcu.dbg.pin(\"1\")"), ("b", "probe.j.pin(\"1\")"),
    ]);
    let statement = "wire(ledctl.mcu.dbg.pin(\"1\"), probe.j.pin(\"1\"))";
    let expected = sys_doc::lower(doc, statement, &design.units, Some(&SysLedger { links: vec![(0, id)] }));
    assert!(expected.diags.is_empty(), "the composed link statement resolves");
    assert_eq!(added.link(id), expected.links.first(), "add uses the document's ends and the next model id");
    assert_eq!(added_sources.doc(doc).expect("added system exists").text.lines().count(), original.lines().count() + 1, "add inserts exactly one system line");
    let (removed, removed_sources, _, _) = commit(&added_sources, &added_map, &added, doc, "elec.link.remove", &[("link", &id.0.to_string())]);
    assert!(removed.link(id).is_none(), "remove deletes the selected link");
    assert_eq!(removed_sources, sources, "removing the added link restores all original source bytes");

    let old = &design.links[0];
    let old_id = old.id.0.to_string();
    let (replaced, replaced_sources, _, edit) = commit(&sources, &ev.map, design, doc, "elec.link.replace", &[("link", &old_id), ("kind", "wire")]);
    let mut expected_link = old.clone();
    expected_link.kind = LinkKind::Wire;
    assert_eq!(replaced.link(old.id), Some(&expected_link), "kind-only replace preserves the id, both ends and pin map");
    assert_eq!(edit.edits.len(), 1, "kind-only replace splices exactly one token");
    assert_eq!(&original[edit.edits[0].span.start as usize..edit.edits[0].span.end as usize], "mate", "replace edits only the old kind token");
    assert_eq!(edit.edits[0].text, "wire", "replace writes only the new kind token");
    assert_eq!(replaced_sources.doc(doc).expect("replaced system exists").text.as_ref(), original.replacen("mate", "wire", 1), "every other byte of the link line and system document stays unchanged");

    let mount_args = [("unit", "\"probe\""), ("on", "ledctl"), ("at", "[20mm, 0mm, -8mm]"), ("rot", "[0deg, 0deg, 90deg]")];
    let (mounted, mounted_sources, mounted_map, _) = commit(&sources, &ev.map, design, doc, "elec.unit.mount", &mount_args);
    let expected = sys_doc::lower(doc, "mount(probe, {on: ledctl at: [20mm, 0mm, -8mm] rot: [0deg, 0deg, 90deg]})", &design.units, None);
    assert!(expected.diags.is_empty(), "the composed mount statement resolves");
    assert_eq!(mounted.mount(probe), expected.mounts.iter().find(|m| m.unit == probe), "mount uses the document's parent, translation and rotation");
    let unmount_ops = sys_ops(&mounted, "elec.unit.unmount", &[("unit", "probe")]).expect("unmount builds");
    assert_eq!(unmount_ops, vec![DesignOp::Sys(SysOp::ClearMount(probe))], "unmount builds ClearMount");
    let (unmounted, _, _, _) = commit(&mounted_sources, &mounted_map, &mounted, doc, "elec.unit.unmount", &[("unit", "probe")]);
    assert_eq!(unmounted.mount(probe), Some(&Mount { unit: probe, parent: Frame::World, xform: Xform3::IDENTITY }), "unmount restores world identity");
    let (world, _, _, _) = commit(&sources, &ev.map, design, doc, "elec.unit.mount", &[("unit", "probe")]);
    assert_eq!(world.mount(probe), unmounted.mount(probe), "omitted parent and pose mount at world identity");
    let (on_mate, _, _, _) = commit(&sources, &ev.map, design, doc, "elec.unit.mount", &[("unit", "probe"), ("link", &old_id)]);
    assert_eq!(on_mate.mount(probe), Some(&Mount { unit: probe, parent: Frame::Mate(old.id), xform: Xform3::IDENTITY }), "mount by numeric link id stores no connector pose");
    let removal = sys_ops(&on_mate, "elec.link.remove", &[("link", &old_id)]).expect("the builder leaves link-in-use preconditions to the model");
    let mut applied = on_mate.clone();
    assert_eq!(applied.apply(&removal[0], false), Err(DesignOpError::LinkInUse { link: old.id, unit: probe }), "the model refuses to remove a mounted link");

    let mut explicit = design.clone();
    explicit.links[0].pinmap = PinMap::Given(vec![(PinIx(0), PinIx(0))]);
    let ops = sys_ops(&explicit, "elec.link.replace", &[("link", &old_id), ("a", "ledctl.mcu.dbg.pin(\"1\")")]).expect("a single end can be replaced");
    let DesignOp::Sys(SysOp::ReplaceLink(link)) = &ops[0] else { panic!("end replacement builds ReplaceLink"); };
    assert_eq!(link.kind, old.kind, "end-only replace keeps the kind");
    assert_eq!(link.b, old.b, "end-only replace keeps the other end");
    assert_ne!(link.a, old.a, "end-only replace resolves the requested pin");
    assert_eq!(link.pinmap, explicit.links[0].pinmap, "replacement keeps an explicit pin map");
    assert_eq!(sys_ops(design, "elec.link.replace", &[("link", &old_id)]).expect("all optional fields may be absent"), vec![DesignOp::Sys(SysOp::ReplaceLink(old.clone()))], "omitting all replacement fields preserves the whole link");

    let diag = refused(design, "elec.link.add", &[("kind", "mate"), ("a", "missing.j"), ("b", "probe.j")], "a", &[]);
    let lowered = sys_doc::lower(doc, "mate(missing.j, probe.j)", &design.units, None);
    assert_eq!(diag.message, format!("a: {}", lowered.diags[0].message), "unresolvable ends carry the document reader's message");
    refused(design, "elec.link.add", &[("kind", "mate"), ("a", "ledctl.missing"), ("b", "probe.j")], "a", &[Sel::Unit(controller)]);
    refused(design, "elec.link.add", &[("kind", "wire"), ("a", "ledctl.mcu.dbg"), ("b", "probe.j.pin(\"missing\")")], "b", &[Sel::Unit(probe)]);
    refused(design, "elec.link.remove", &[("link", "999")], "link", &[Sel::Link(LinkId(999))]);
    refused(design, "elec.unit.mount", &[("unit", "probe"), ("on", "ledctl"), ("link", &old_id)], "on", &[Sel::Unit(controller)]);
    for field in ["at", "rot"] {
        refused(design, "elec.unit.mount", &[("unit", "probe"), ("link", &old_id), (field, "[0, 0, 0]")], field, &[Sel::Unit(probe)]);
    }
    refused(design, "elec.unit.mount", &[("unit", "probe"), ("on", "probe")], "on", &[Sel::Unit(probe)]);
    refused(design, "elec.link.add", &[("kind", "wire"), ("a", "ledctl.mcu.dbg")], "b", &[]);
    refused(design, "elec.unknown", &[], "tool", &[]);
    refused(design, "elec.unit.unmount", &[("unit", "missing")], "unit", &[]);
    refused(design, "elec.unit.mount", &[("unit", "probe"), ("on", "missing")], "on", &[]);
    refused(design, "elec.unit.mount", &[("unit", "probe"), ("at", "[1mm, 2mm]")], "at", &[Sel::Unit(probe)]);
    refused(design, "elec.unit.mount", &[("unit", "probe"), ("rot", "[0deg, 1mm, 0deg]")], "rot", &[Sel::Unit(probe)]);
    refused(design, "elec.unit.unmount", &[("unit", "probe"), ("extra", "0")], "extra", &[]);
    refused(design, "elec.unit.unmount", &[("unit", "probe"), ("unit", "ledctl")], "unit", &[]);
    refused(design, "elec.link.remove", &[("link", "-1")], "link", &[]);
    refused(design, "elec.link.replace", &[("link", &old_id), ("kind", "other")], "kind", &[Sel::Link(old.id)]);
    refused(&added, "elec.unit.mount", &[("unit", "probe"), ("link", &id.0.to_string())], "link", &[Sel::Link(id)]);
    refused(design, "elec.link.add", &[("kind", "mate"), ("a", "ledctl.mcu.dbg"), ("b", "probe.j, probe.j")], "b", &[]);
    refused(design, "elec.unit.mount", &[("unit", "probe"), ("at", "[0mm, 0mm, 0mm] rot: 90deg")], "at", &[Sel::Unit(probe)]);
    assert_eq!(design, &before, "building and applying to clones leaves the evaluated design unchanged");
}

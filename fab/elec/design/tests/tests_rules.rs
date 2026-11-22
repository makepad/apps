//! System ops preserve model invariants; factory minima never change the authored board rules.

use fab_elec_design::engines;
use fab_elec_fixtures::{ledctl, ledctl_pair};
use fab_elec_design::ops::{DesignApplied, DesignOpError};
use fab_elec_design::{Design, DesignOp, Frame, Mount, SysOp};
use fab_elec_board::{resolve_rules, resolve_rules_with, Capabilities, Factories};
use fab_elec_circuit::{CheckKind, Measure};
use fab_elec_core::{mm, BoardId, LinkId, Sel, Xform3};
use std::sync::Arc;

#[test]
fn system_ops_preserve_consistency_and_undo_exactly() {
    let mut d = ledctl_pair();
    let original = d.clone();
    let probe = BoardId(1);
    let link = LinkId(0);
    let unknown = BoardId(u16::MAX);
    let missing_link = LinkId(u32::MAX);
    let old_mount = d.mount(probe).expect("the probe has a mount").clone();
    let default = Mount { unit: probe, parent: Frame::World, xform: Xform3::IDENTITY };
    assert_eq!(old_mount, Mount { unit: probe, parent: Frame::Unit(BoardId(0)), xform: Xform3::translation([mm(10.0), 0, mm(-8.0)]) }, "the pair starts with the probe on the controller");
    assert_eq!(d.check_consistent(), Vec::new(), "the system-op fixture starts consistent");
    let apply = |d: &mut Design, op: &SysOp| {
        let applied = d.apply(&DesignOp::Sys(op.clone()), false).unwrap_or_else(|e| panic!("{op:?} applies: {e}"));
        assert_eq!(d.check_consistent(), Vec::new(), "every applied system op leaves a consistent design: {op:?}");
        let DesignApplied::Sys { inverse } = applied else { panic!("{op:?} has a system inverse") };
        inverse
    };
    let reject = |d: &mut Design, op: SysOp, error: DesignOpError| {
        let before = d.clone();
        assert_eq!(d.apply(&DesignOp::Sys(op.clone()), false), Err(error), "the precondition refuses {op:?}");
        assert_eq!(*d, before, "rejected {op:?} changes nothing, including generation");
        assert_eq!(d.check_consistent(), Vec::new(), "rejected {op:?} keeps the design consistent");
    };
    let mut added = d.links[0].clone();
    added.id = LinkId(1);
    let mut replaced = d.links[0].clone();
    std::mem::swap(&mut replaced.a, &mut replaced.b);
    let off_grid = Xform3 { t: [mm(2.0), 3, mm(-4.0)], q: [2.0, 0.123456789, -0.234567891, 0.345678912] };
    assert_ne!(off_grid, off_grid.snapped(), "the SetMount input exercises quaternion normalisation and grid snapping");
    let changed = Mount { unit: probe, parent: Frame::Unit(BoardId(0)), xform: off_grid };
    let body = Mount { parent: Frame::Body { mechanism: "fixture".into(), body: "carrier".into() }, ..changed.clone() };
    for op in [
        SysOp::AddLink(added.clone()),
        SysOp::InsertLink { link: added, pos: 0 },
        SysOp::RemoveLink(link),
        SysOp::ReplaceLink(replaced),
        SysOp::SetMount(changed),
        SysOp::ClearMount(probe),
        SysOp::SetFactory { unit: probe, profile: Some("fab-a".into()) },
        SysOp::SetMount(body),
        SysOp::SetMount(default.clone()),
    ] {
        let inverse = apply(&mut d, &op);
        match &op {
            SysOp::SetMount(m) => {
                assert_eq!(d.mount(probe), Some(&Mount { xform: m.xform.snapped(), ..m.clone() }), "SetMount accepts its parent and stores the snapped pose");
                assert_eq!(inverse, SysOp::SetMount(old_mount.clone()), "SetMount returns the previous mount exactly as stored");
            }
            SysOp::ClearMount(_) => {
                assert_eq!(d.mount(probe), Some(&default), "ClearMount resets to World and identity");
                assert_eq!(inverse, SysOp::SetMount(old_mount.clone()), "ClearMount returns the previous mount");
            }
            SysOp::InsertLink { .. } => assert_eq!(d.links.iter().map(|l| l.id).collect::<Vec<_>>(), vec![LinkId(1), link], "InsertLink respects its requested position"),
            SysOp::SetFactory { profile, .. } => assert_eq!(&d.unit(probe).unwrap().factory, profile, "SetFactory stores the unit profile"),
            _ => {}
        }
        apply(&mut d, &inverse);
        assert_eq!(d.links, original.links, "{op:?} then its inverse restores links exactly and in order");
        assert_eq!(d.assembly.mounts, original.assembly.mounts, "{op:?} then its inverse restores mounts exactly");
        assert_eq!(d.units, original.units, "{op:?} then its inverse restores every unit, including its factory");
    }

    for parent in [Frame::Unit(probe), Frame::Unit(unknown), Frame::Mate(missing_link)] {
        reject(&mut d, SysOp::SetMount(Mount { parent, ..old_mount.clone() }), DesignOpError::BadMountParent { unit: probe });
    }
    reject(&mut d, SysOp::SetMount(Mount { unit: unknown, ..default.clone() }), DesignOpError::NoUnit(unknown));
    reject(&mut d, SysOp::ClearMount(unknown), DesignOpError::NoUnit(unknown));
    reject(&mut d, SysOp::RemoveLink(missing_link), DesignOpError::NoLink(missing_link));

    let mate = Mount { parent: Frame::Mate(link), ..old_mount.clone() };
    let restore_mount = apply(&mut d, &SysOp::SetMount(mate));
    reject(&mut d, SysOp::RemoveLink(link), DesignOpError::LinkInUse { link, unit: probe });
    let controller = d.mount(BoardId(0)).unwrap().clone();
    let restore_controller = apply(&mut d, &SysOp::SetMount(Mount { parent: Frame::Mate(link), ..controller }));
    reject(&mut d, SysOp::RemoveLink(link), DesignOpError::LinkInUse { link, unit: BoardId(0) });
    apply(&mut d, &restore_controller);
    apply(&mut d, &restore_mount);
    let restore_link = apply(&mut d, &SysOp::RemoveLink(link));
    assert!(d.link(link).is_none(), "RemoveLink is allowed once all mounts have moved back");
    apply(&mut d, &restore_link);
    assert_eq!(d.links, original.links, "the detached link's inverse restores the original links");
    assert_eq!(d.assembly.mounts, original.assembly.mounts, "the temporary mate mounts undo exactly");

    // Repair an incomplete input at either end of the mount list. ClearMount never
    // recreates the missing-entry state, which cannot occur in a consistent design.
    for unit in [BoardId(0), probe] {
        let i = d.assembly.mounts.iter().position(|m| m.unit == unit).unwrap();
        let old = d.assembly.mounts.remove(i);
        let inverse = apply(&mut d, &SysOp::ClearMount(unit));
        let reset = Mount { unit, parent: Frame::World, xform: Xform3::IDENTITY };
        assert_eq!(d.mount(unit), Some(&reset), "ClearMount inserts a default when unit {unit:?} has no mount");
        assert_eq!(d.assembly.mounts.iter().map(|m| m.unit).collect::<Vec<_>>(), vec![BoardId(0), probe], "ClearMount inserts unit {unit:?} at its sorted position");
        assert_eq!(inverse, SysOp::SetMount(reset), "a missing mount's ClearMount inverse keeps its repaired default");
        apply(&mut d, &inverse);
        apply(&mut d, &SysOp::SetMount(old.clone()));
        d.assembly.mounts.remove(i);
        let inverse = apply(&mut d, &SysOp::SetMount(old));
        assert_eq!(inverse, SysOp::ClearMount(unit), "SetMount inserting unit {unit:?} retains its ClearMount inverse");
        assert_eq!(d.assembly.mounts, original.assembly.mounts, "SetMount inserts unit {unit:?} in sorted order");
        apply(&mut d, &inverse);
        assert_eq!(d.mount(unit), Some(&Mount { unit, parent: Frame::World, xform: Xform3::IDENTITY }), "undoing insertion resets unit {unit:?} instead of removing its mount");
        apply(&mut d, &SysOp::SetMount(original.mount(unit).unwrap().clone()));
    }
    assert_eq!(d.links, original.links, "all system-op checks restore the original links");
    assert_eq!(d.assembly.mounts, original.assembly.mounts, "all system-op checks restore the original mounts");
    assert_eq!(d.units, original.units, "all system-op checks restore the original units and factories");
}

#[test]
fn factory_raises_effective_rules_never_stored() {
    let mut d = ledctl();
    let board = d.units[0].board.as_ref().expect("controller has a board").clone();
    let stored = board.rules.clone();
    let width = |t: &fab_elec_board::Track| t.widths.as_ref().and_then(|w| w.iter().min().copied()).unwrap_or(t.w).min(t.w);
    let track = board.tracks.iter().min_by_key(|t| width(t)).expect("fixture has a narrowest track");
    let have = width(track);
    let class = board.nets[track.net.ix()].class;
    let min_track = have.checked_add(1).expect("fixture width leaves room for a raised minimum");
    let caps = Capabilities { min_track, min_clearance: 0, min_drill: 0, min_annular: 0, via_in_pad: false, layers: vec![2], board_min: (0, 0), board_max: (0, 0), thickness: Vec::new(), thickness_tol_ppm: 0 };
    let stage: Factories = [("fab-a".to_string(), Arc::new(caps))].into_iter().collect();
    let prev = engines::full_with(&d, &stage);

    let before = d.clone();
    let unknown = BoardId(u16::MAX);
    let error = d.apply(&DesignOp::Sys(SysOp::SetFactory { unit: unknown, profile: Some("fab-a".into()) }), false)
        .expect_err("a factory change on an unknown unit fails");
    assert_eq!(error, DesignOpError::NoUnit(unknown), "an unknown factory unit reports NoUnit");
    assert_eq!(d, before, "a rejected factory change preserves the entire design");

    let applied = d.apply(&DesignOp::Sys(SysOp::SetFactory { unit: BoardId(0), profile: Some("fab-a".into()) }), false)
        .expect("the controller's factory can be set");
    assert_eq!(d.units[0].factory.as_deref(), Some("fab-a"), "SetFactory stores the profile on the unit");
    let deltas = engines::deltas_of(&d, &applied);
    assert_eq!(deltas.len(), 1, "a factory change invalidates exactly one board");
    assert_eq!(deltas[0].0, BoardId(0), "a factory delta identifies the changed unit");
    assert!(deltas[0].1.global, "a factory change requires a full rules pass");
    assert_eq!(deltas[0].1.bbox, board.outline_bbox(), "a factory delta covers the board outline");
    let result = engines::update_with(&prev, &d, &deltas, &stage);
    assert_eq!(result, engines::full_with(&d, &stage), "a factory update equals a full engine run");
    let raised = result.boards.iter().find(|b| b.unit == BoardId(0)).expect("factory results contain the controller");
    let violation = raised.drc.iter().find(|v| v.kind == CheckKind::TrackWidth && v.items.contains(&Sel::Item(BoardId(0), track.id)))
        .expect("the factory minimum reports the fixture's narrowest track");
    assert!(violation.rule.contains("fab-a"), "the raised track rule names its factory");
    assert!(violation.fix_hint.contains("below fab-a's minimum"), "the raised track hint names its factory");
    assert_eq!(violation.actual, Some(Measure::Len(have)), "the factory finding measures the unchanged track width");
    assert_eq!(violation.required, Some(Measure::Len(min_track)), "the factory finding requires the raised width");
    assert_eq!(raised.rules.origin(raised.rules.track_ref(class)), Some("fab-a"), "the track matrix records the factory origin");
    assert_eq!(d.units[0].board.as_ref().expect("controller keeps its board").rules, stored, "effective rules are never stored on the board");

    let mut via_board = board.as_ref().clone();
    via_board.classes[class.ix()].rules.via_in_pad = Some(true);
    let mut factories = stage.clone();
    for allowed in [false, true] {
        Arc::make_mut(factories.get_mut("fab-a").expect("the bound factory is present")).via_in_pad = allowed;
        let effective = engines::effective(&d.units[0], &via_board, &factories).expect("the bound factory resolves");
        assert_eq!(effective.via_in_pad, allowed, "effective rules carry the factory's via-in-pad capability");
        assert_eq!(
            resolve_rules_with(&via_board, Some(&effective), None).via_in_pad(class), allowed,
            "a class allowing vias in pads follows the bound factory's capability"
        );
    }

    let inverse = applied.inverse();
    assert_eq!(inverse.len(), 1, "a factory change has one undo step");
    let undone = d.apply(&inverse[0], false).expect("the factory change can be undone");
    assert_eq!(d.units[0].factory, None, "the factory inverse restores the previous profile");
    let restored = engines::update_with(&result, &d, &engines::deltas_of(&d, &undone), &stage);
    let own = restored.boards.iter().find(|b| b.unit == BoardId(0)).expect("undo results contain the controller");
    assert!(own.drc.iter().all(|v| v.kind != CheckKind::TrackWidth), "without a factory the fixture's track widths satisfy the design rules");
    assert_eq!(own.rules.origin(own.rules.track_ref(class)), None, "without a factory the track rule has no factory origin");
    assert_eq!(restored.boards, prev.boards, "undo restores the board results before the factory change");
    let board = d.units[0].board.as_ref().expect("undo keeps the controller board");
    assert_eq!(board.rules, stored, "applying and undoing a factory leaves the stored rules unchanged");
    assert_eq!(resolve_rules_with(board, None, None), resolve_rules(board, None), "absent effective rules preserve the original resolver");
}

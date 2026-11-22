use super::*;
use crate::canvas::{Look, Tokens};
use crate::explorer::{liveix::SceneLive, scene::{PinHit, Scene, SceneState}, targets::{HOLD, PICK}};
use crate::snapshot::{Committed, Refusal, StageFlags, StateOf, Tagged};
use fab_elec_circuit::{CircuitOp, Net, NetKind, SheetHints};
use fab_elec_core::{ClassId, CompId, NetId, PinIx};
use fab_elec_design::{By, DesignOp};
use fab_elec_explore::{edit::Toward, layout, rows::SheetRows, Opts};

/// Snapshot has no constructor or Default here; mirror the host's explicit empty-result setup.
pub(in crate::explorer) fn snapshot(design: Arc<Design>) -> DesignSnapshot {
    DesignSnapshot {
        design, preview: None, preview_tag: None, preview_by: None, created: vec![], last_commit: None,
        sheets: vec![], proposal: None, job: None, conn: vec![], rules: vec![], fills: vec![], sysnets: None,
        export: vec![], violations: Tagged { unit: None, of: StateOf::Committed, generation: 0, value: Arc::new(vec![]) },
        awaiting: Arc::new(vec![]), last_refusal: None, stale: StageFlags::NONE, head: String::new(),
        benches: Arc::default(), source_map: None, docs: None, catalog: None,
    }
}

/// A semantic pin aim independent of the approach direction.
fn pin(hit: &PinHit) -> Target {
    Target::Pin { comp: CompId(hit.node), pin: PinIx(hit.pin as u16), toward: Toward::Across }
}

/// Return the tag only when the step contains exactly the expected action.
fn preview(step: Step) -> u64 {
    assert_eq!(step.phase, Phase::Dragging, "a candidate preview is sent while Dragging");
    match step.action {
        Some(ViewAction::Preview { tag, .. }) => tag,
        action => panic!("a new edit candidate sends one Preview, got {action:?}"),
    }
}

#[test]
fn moved_parts_keep_their_frozen_bodies_pins_and_connection_strokes() {
    let design = fab_elec_fixtures::ledctl();
    let circuit = &design.units[0].circuit;
    let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
    let rows = SheetRows::new().sync(circuit);
    let live = SceneLive {
        pins: circuit.comps.iter().map(|part| vec![0.0; part.part.pins.len()]).collect(),
        ..SceneLive::default()
    };
    let state = SceneState { assemble_at: Some(0.0), ..SceneState::default() };
    let scene = Scene::build(&design, 0, &sheet, &rows, Tokens::for_theme(true), &state, &live);
    let node = scene.hits.pins.iter().find(|pin| pin.net.is_some()
        && scene.boxes.iter().any(|row| scene.part_node(row.c.item as u32) == Some(pin.node))
        && scene.pins.iter().any(|row| scene.part_node(row.c.item as u32) == Some(pin.node)))
        .expect("ghost setup: a boxed part has a connected pin and a pin mark").node;
    let mut display = sheet.clone();
    let part = &mut display.parts[node as usize];
    part.at[0] += 4;
    part.body[0] += 4;
    part.body[2] += 4;
    let moved = moved_parts(circuit, &sheet, circuit, &display);
    assert_eq!(moved.len(), circuit.comps.len(), "ghost membership is indexed by frozen node");
    assert_eq!(moved.iter().enumerate().filter_map(|(node, &moved)| moved.then_some(node)).collect::<Vec<_>>(),
        vec![node as usize], "only the part displaced by four grid steps needs a ghost");
    let ghosts = scene.ghost_rows(&moved);
    assert!(scene.boxes.iter().any(|row| scene.part_node(row.c.item as u32) == Some(node) && row.c.span[1] > 0.0),
        "ghost setup: the frozen source carries an animation that copies must discard");
    assert!(!ghosts.boxes.is_empty(), "ghosts retain the moved part's body");
    assert!(!ghosts.lines.is_empty(), "ghosts retain the moved part's strokes");
    assert!(!ghosts.pins.is_empty(), "ghosts retain the moved part's pin marks");
    let belongs = |item: f32| item >= 0.0 && scene.part_node(item as u32) == Some(node);
    assert!(ghosts.boxes.iter().map(|row| row.c).chain(ghosts.arcs.iter().map(|row| row.c))
        .chain(ghosts.pins.iter().map(|row| row.c)).all(|c| belongs(c.item)),
        "every ghost body, arc and pin mark belongs to the moved part");
    let outer: Vec<_> = scene.hits.pins.iter().filter(|pin| pin.node == node).map(|pin| pin.outer).collect();
    assert!(ghosts.lines.iter().all(|row| belongs(row.c.item)
        || [[row.seg[0], row.seg[1]], [row.seg[2], row.seg[3]]].iter()
            .any(|at| outer.contains(&at.map(f64::from)))),
        "every ghost stroke belongs to the part or ends at one of its frozen pins");
    assert!(ghosts.lines.iter().any(|row| !belongs(row.c.item)),
        "ghosts include a connection stroke beyond the part's own glyph");
    assert!(ghosts.lines.iter().all(|row| row.follow == [0.0; 2]),
        "ghost connections never follow the moving display");
    for c in ghosts.boxes.iter().map(|row| row.c).chain(ghosts.lines.iter().map(|row| row.c))
        .chain(ghosts.arcs.iter().map(|row| row.c)).chain(ghosts.pins.iter().map(|row| row.c))
    {
        assert_eq!(c.look, Look::Ghost.code(), "every copied row has the ghost look");
        assert_eq!(c.pose_a, [0.0, 0.0, 0.0, 1.0], "ghost pose starts at its frozen geometry");
        assert_eq!(c.pose_b, c.pose_a, "ghost pose ends at the same frozen geometry");
        assert_eq!(c.span, [0.0; 2], "ghost pose has no animation clock");
    }
}

#[test]
fn wire_gesture_holds_frozen_hits_and_sends_one_action_per_candidate() {
    let design = Arc::new(fab_elec_fixtures::ledctl());
    let circuit = &design.units[0].circuit;
    let sheet = Arc::new(layout(&design, 0, &SheetHints::default(), &Opts::default()));
    let mut sheet_rows = SheetRows::new();
    let rows = Arc::new(sheet_rows.sync(circuit));
    let tokens = Tokens::for_theme(true);
    let scene = Scene::build(&design, 0, &sheet, &rows, tokens, &SceneState::default(), &SceneLive::default());
    let frozen = || Frozen {
        design: design.clone(), unit: 0, circuit: circuit.clone(), sheet: sheet.clone(),
        hits: scene.hits.clone(),
    };
    let scale = 16.0;
    let free: Vec<_> = scene.hits.pins.iter().filter(|hit| hit.net.is_none()
        && same_aim(resolve(&scene.hits, &sheet, hit.outer, scale, &Target::None), pin(hit))).collect();

    // The first two free pins on different parts: connecting them plans ConnectPins. The preview
    // design models that edit; its display is the target part reflowed a few grid steps away, which
    // a connectivity layout may do. The display is built, never searched for, so the trap is certain.
    let a = *free.first().expect("no-flap setup: the fixture has a free pin");
    let b = *free.iter().find(|hit| hit.node != a.node).expect("no-flap setup: a free pin on another part");
    let mut preview_design = (*design).clone();
    let preview_circuit = Arc::make_mut(&mut preview_design.units[0].circuit);
    let net = NetId(preview_circuit.nets.len() as u32);
    preview_circuit.nets.push(Net {
        id: net, name: "gesture_connection".into(), class: ClassId(0), volts: None,
        kind: NetKind::Signal, scope: Some(preview_circuit.comps[a.node as usize].block),
    });
    for hit in [a, b] {
        let part = &mut preview_circuit.comps[hit.node as usize];
        let pin = PinIx(hit.pin as u16);
        part.nc.retain(|&marked| marked != pin);
        part.conns.push((pin, net));
    }
    let changed = Arc::new(preview_design);
    let mut changed_sheet = (*sheet).clone();
    let place = &mut changed_sheet.parts[b.node as usize];
    place.at[0] += 4;
    place.body[0] += 4;
    place.body[2] += 4;
    let moved = 4.0;
    let changed_rows = sheet_rows.clone().sync(&changed.units[0].circuit);
    let changed_scene = Scene::build(&changed, 0, &changed_sheet, &changed_rows,
        tokens, &SceneState::default(), &SceneLive::default());
    assert!(moved > PICK + HOLD, "no-flap precondition: ConnectPins moves the target part beyond PICK + HOLD");
    assert_ne!(a.node, b.node, "no-flap precondition: source and target belong to different parts");
    assert!(a.net.is_none() && b.net.is_none(), "no-flap precondition: both pins are unconnected");

    let begin = || EditGesture::begin(frozen(), Source::Wire(pin(a)), a.outer)
        .expect("a source with a frozen pin hit arms the gesture");
    let mut gesture = begin();
    assert_eq!(gesture.phase(), Phase::Armed, "press arms without previewing");
    assert_eq!(gesture.from_point(), a.outer, "rubber origin is the source's frozen outer end");
    assert!(Arc::ptr_eq(&gesture.frozen().hits, &scene.hits), "the press shares its hit table without copying it");
    let first = gesture.sample(b.outer, scale);
    assert!(matches!(&first.action, Some(ViewAction::Preview { ops, .. }) if matches!(ops.as_slice(),
        [DesignOp::Circuit { op: CircuitOp::ConnectPins { a: source, b: target }, .. }]
            if *source == (CompId(a.node), PinIx(a.pin as u16)) && *target == (CompId(b.node), PinIx(b.pin as u16)))),
        "the preview really plans ConnectPins for the two free pins");
    let first_tag = preview(first);
    assert!(!same_aim(resolve(&changed_scene.hits, &changed_sheet, b.outer, scale, &pin(b)), pin(b)),
        "the trap: resolving the stationary pointer against preview hits loses the target");
    let mut snap = snapshot(design.clone());
    snap.preview = Some(changed.clone());
    snap.preview_tag = Some(first_tag);
    let (step, answer) = gesture.snapshot(&snap);
    assert!(step.action.is_none() && answer == Some(Answer::Shown(first_tag)),
        "a preview acknowledges its tag without sending another action");
    assert!(gesture.shown(), "shown reports the last snapshot's matching live tag");
    assert!(gesture.sample(b.outer, scale).action.is_none(), "stationary pointer on frozen hits sends no replacement preview");
    assert!(same_aim(gesture.target(), pin(b)), "preview reflow cannot change the frozen target");

    let mut sweep = begin();
    let below = [a.outer[0] + DRAG_THRESHOLD / scale * 0.5, a.outer[1]];
    let step = sweep.sample(below, scale);
    assert!(step.action.is_none() && step.phase == Phase::Armed, "nothing is sent below the screen-space drag threshold");
    let exact = [a.outer[0] + DRAG_THRESHOLD / scale, a.outer[1]];
    let step = sweep.sample(exact, scale);
    assert!(step.action.is_none() && step.phase == Phase::Armed, "the threshold must be exceeded, not merely reached");
    // Three distinct free pins make three distinct plans from the same source.
    let aims: Vec<_> = free.iter().copied().filter(|hit| hit.node != a.node || hit.pin != a.pin).take(3).collect();
    assert_eq!(aims.len(), 3, "sweep precondition: three other free pins are pickable");
    let mut tags = Vec::new();
    for hit in &aims {
        tags.push(preview(sweep.sample(hit.outer, scale)));
        let dir = [hit.outer[0] - hit.inner[0], hit.outer[1] - hit.inner[1]];
        let mid = [(hit.outer[0] + hit.inner[0]) * 0.5, (hit.outer[1] + hit.inner[1]) * 0.5];
        let jitter = [mid[0] - dir[1] * (PICK + HOLD * 0.5), mid[1] + dir[0] * (PICK + HOLD * 0.5)];
        assert!(sweep.sample(jitter, scale).action.is_none(), "jitter inside HOLD sends no action, even when toward changes");
        assert!(same_aim(sweep.target(), pin(hit)), "hysteresis retains the current candidate inside HOLD");
        assert!(sweep.tab(false).action.is_none() && sweep.tab(true).action.is_none(),
            "Tab and Shift-Tab preserve a wire's single candidate");
    }
    assert!(tags.windows(2).all(|pair| pair[0] < pair[1]), "each Preview in the three-pin sweep has a rising tag");
    let outside = [sheet.bounds[2] as f64 + 20.0, sheet.bounds[3] as f64 + 20.0];
    assert_eq!(sweep.sample(outside, scale).action, Some(ViewAction::CancelPreview),
        "the sweep is exactly Preview, Preview, Preview, CancelPreview on leaving the parts");
    let empty = [outside[0] + 1.25, outside[1] + 0.4];
    assert!(sweep.sample(empty, scale).action.is_none(),
        "empty canvas cells are one aim and cannot repeat CancelPreview");
    assert_eq!(sweep.tag(), None, "leaving the candidate clears its live tag");
    let step = sweep.release(scale);
    assert!(step.action.is_none() && step.phase == Phase::Idle, "a drag to empty sheet ends without a commit or hold");
    assert_eq!(step.name_end, Some((pin(a), empty)), "the field receives the dragged pin and the unsnapped release point");
    let body = sheet.parts.iter().filter(|part| part.placed).map(|part| part.at.map(f64::from))
        .find(|&at| resolve(&scene.hits, &sheet, at, scale, &Target::None) == Target::None)
        .expect("wire name setup: a component body away from its pins");
    let mut missed = begin();
    missed.sample(empty, scale);
    missed.sample(body, scale);
    let step = missed.release(scale);
    assert!(step.phase == Phase::FlyBack && step.action.is_none() && step.name_end.is_none() && missed.note().is_some(),
        "a body is not empty sheet: an unusable drop retains its reason and never opens the field");

    let connected = scene.hits.pins.iter().find(|p| p.net.is_some() && !p.nc && !p.open_end)
        .expect("wire name setup: a connected pin has a wire end");
    let source = Target::WireEnd { comp: CompId(connected.node), pin: PinIx(connected.pin as u16), net: NetId(connected.net.unwrap()) };
    let mut wire = EditGesture::begin(frozen(), Source::Wire(source), connected.outer).expect("a wire end arms");
    assert!(wire.sample(empty, scale).action.is_none(), "empty sheet never previews a wire-end disconnect");
    let step = wire.release(scale);
    assert!(step.action.is_none() && step.phase == Phase::Idle, "a wire end stops without disconnecting or landing");
    assert_eq!(step.name_end, Some((source, empty)), "a dragged wire end keeps its connection for the naming plan");
    let mut clicked_end = EditGesture::begin(frozen(), Source::Wire(source), connected.outer).expect("a wire end arms");
    let step = clicked_end.release(scale);
    assert!(step.phase == Phase::Idle && step.name_end.is_none(), "a click on a wire end starts no click wire (it selects)");

    let mut landing = begin();
    let drag_preview = landing.sample(b.outer, scale);
    let Some(ViewAction::Preview { ops: drag_ops, .. }) = drag_preview.action.clone() else {
        panic!("releasing over the second pin needs a preview to commit");
    };
    let tag = preview(drag_preview);
    let step = landing.release(scale);
    assert!(step.action == Some(ViewAction::CommitPreview) && step.phase == Phase::Landing,
        "release commits the live tag and holds Landing before its preview is shown");
    assert_eq!(landing.end_point(), b.outer, "Landing attaches the rubber end to the frozen target");
    assert!(landing.sample(outside, scale).action.is_none() && landing.end_point() == b.outer,
        "Landing ignores pointer samples and holds its display");
    let step = landing.escape();
    assert!(step.action.is_none() && step.phase == Phase::Landing, "Escape cannot cancel an already sent commit");
    landing.defer(a.outer);
    landing.defer(b.outer);
    assert_eq!(landing.take_deferred(), None, "a press during Landing is retained until the host answers");
    let mut snap = snapshot(design.clone());
    let (step, answer) = landing.snapshot(&snap);
    assert!(step.action.is_none() && step.phase == Phase::Landing && answer.is_none(),
        "a snapshot with no preview and no commit keeps the Landing gate closed");
    let created = vec![Sel::Net(fab_elec_core::NetRef { board: circuit.unit,
        net: NetId(circuit.nets.len() as u32) })];
    snap.last_commit = Some(Arc::new(Committed { seq: 1, tag: Some(first_tag), by: By::Person, created: vec![] }));
    snap.last_refusal = Some(Arc::new(Refusal { seq: 1, label: "older candidate".into(), op: "circuit",
        text: "old refusal".into(), reason: "The earlier edit was refused.".into(), site: None, sample: Some(first_tag) }));
    let (step, answer) = landing.snapshot(&snap);
    assert!(step.phase == Phase::Landing && step.action.is_none() && answer.is_none(),
        "commit and refusal records from older tags cannot end Landing");
    snap.design = changed.clone();
    snap.last_commit = Some(Arc::new(Committed { seq: 2, tag: Some(tag), by: By::Person, created: created.clone() }));
    let (step, answer) = landing.snapshot(&snap);
    assert!(step.action.is_none() && step.phase == Phase::Idle && answer == Some(Answer::Committed { created }),
        "the matching commit ends Landing with its created selections before the base-change check");
    assert_eq!(landing.take_deferred(), Some(b.outer), "only the latest deferred press is returned after Landing");
    assert_eq!(landing.take_deferred(), None, "a deferred press can be consumed only once");

    let mut refused = begin();
    let refused_tag = preview(refused.sample(b.outer, scale));
    assert_eq!(refused.release(scale).action, Some(ViewAction::CommitPreview), "the second gesture sends its commit once");
    let refusal = Arc::new(Refusal { seq: 3, label: "wire".into(), op: "circuit", text: "host refused this wire".into(),
        reason: "The pin cannot be wired to itself.".into(), site: None, sample: Some(refused_tag) });
    let mut snap = snapshot(changed.clone());
    snap.last_refusal = Some(refusal.clone());
    let (step, answer) = refused.snapshot(&snap);
    assert!(step.action.is_none() && step.phase == Phase::FlyBack && answer == Some(Answer::Refused),
        "a matching refusal ends Landing in FlyBack before the base-change check");
    assert_eq!(refused.chip(), Some((refusal.reason.as_str(), ChipLook::Refused)), "the drop chip shows the host's plain reason");
    assert_eq!(refused.refusal(), Some(refusal.reason.as_str()), "the input exposes the tagged reason to the display");

    let mut rejected_drag = begin();
    let tag = preview(rejected_drag.sample(b.outer, scale));
    let mut snap = snapshot(design.clone());
    snap.last_refusal = Some(Arc::new(Refusal { sample: Some(tag), ..(*refusal).clone() }));
    let (step, answer) = rejected_drag.snapshot(&snap);
    assert!(step.action.is_none() && step.phase == Phase::Dragging && answer == Some(Answer::Refused),
        "a tagged refusal during the drag keeps its chip until release");
    let step = rejected_drag.release(scale);
    assert!(step.action == Some(ViewAction::CancelPreview) && step.phase == Phase::FlyBack,
        "release of a refused live sample cancels instead of committing");

    let mut moved = begin();
    preview(moved.sample(b.outer, scale));
    // Even equal circuit contents in a different Arc invalidate positional ids.
    let mut other = (*design).clone();
    other.units[0].circuit = Arc::new((**circuit).clone());
    let snap = snapshot(Arc::new(other));
    let (step, answer) = moved.snapshot(&snap);
    assert!(step.action == Some(ViewAction::CancelPreview) && step.phase == Phase::Idle && answer == Some(Answer::BaseMoved),
        "a different committed circuit Arc mid-drag sends exactly one CancelPreview and ends Idle");
    let (step, answer) = moved.snapshot(&snap);
    assert!(step.action.is_none() && answer.is_none(), "the same base change cannot send another cancellation");
    assert!(moved.sample(b.outer, scale).action.is_none(), "an ended gesture cannot issue new previews");

    let mut clicked = begin();
    clicked.sample(below, scale);
    let step = clicked.release(scale);
    assert!(step.action.is_none() && step.phase == Phase::ClickWire, "a pin click below the threshold arms a wire without writing");
    let step = clicked.sample(b.outer, scale);
    assert!(matches!(step.action, Some(ViewAction::Preview { ops, .. }) if ops == drag_ops),
        "moving to the second pin previews the same operations as the committed drag");
    assert_eq!(clicked.end_point(), b.outer, "the armed rubber wire follows the pointer with the button up");
    assert_eq!(clicked.sample(outside, scale).action, Some(ViewAction::CancelPreview), "leaving the pin clears its candidate");
    let step = clicked.release(scale);
    assert!(step.action.is_none() && step.phase == Phase::Idle, "an empty click ends the wire without committing");
    assert_eq!(step.name_end, Some((pin(a), outside)), "the click-started wire reports its start and the empty click for the field");
    assert!(clicked.release(scale).name_end.is_none(), "a repeated release cannot open the field twice");
    let mut clicked = begin();
    clicked.release(scale);
    clicked.sample(b.outer, scale);
    let mut snap = snapshot(design.clone());
    snap.last_refusal = Some(Arc::new(Refusal { sample: clicked.tag(), ..(*refusal).clone() }));
    assert_eq!(clicked.snapshot(&snap).1, Some(Answer::Refused), "the armed wire sees its tagged refusal");
    let step = clicked.release(scale);
    assert!(step.action.is_none() && step.phase == Phase::ClickWire && clicked.refusal() == Some(refusal.reason.as_str()),
        "a refused click keeps the wire and its reason");
    clicked.sample(outside, scale);
    clicked.sample(b.outer, scale);
    let step = clicked.release(scale);
    assert!(step.action == Some(ViewAction::CommitPreview) && step.phase == Phase::ClickLanding,
        "clicking the valid pin commits its preview in one edit");
    assert!(clicked.release(scale).action.is_none(), "a landing click-wire cannot send a second commit");
    snap.last_refusal = Some(Arc::new(Refusal { sample: clicked.tag(), ..(*refusal).clone() }));
    let (step, answer) = clicked.snapshot(&snap);
    assert!(answer == Some(Answer::Refused) && step.phase == Phase::ClickWire && step.action.is_none(),
        "a refusal that arrives after the click leaves the wire armed on its committed base");

    let mut clicked = begin();
    clicked.release(scale);
    clicked.sample(b.outer, scale);
    let step = clicked.escape();
    assert!(step.action == Some(ViewAction::CancelPreview) && step.phase == Phase::Idle, "Escape discards the click-wire preview");
    let mut clicked = begin();
    clicked.release(scale);
    clicked.sample(b.outer, scale);
    assert_eq!(clicked.sample(a.outer, scale).action, Some(ViewAction::CancelPreview), "returning to the source clears the preview");
    let step = clicked.release(scale);
    assert!(step.action.is_none() && step.phase == Phase::Idle, "clicking the starting pin ends the wire without writing");
    let mut armed = begin();
    let step = armed.escape();
    assert!(step.action.is_none() && step.phase == Phase::Idle, "Escape before dragging needs no host cancellation");
    let mut escaped = begin();
    preview(escaped.sample(b.outer, scale));
    let step = escaped.escape();
    assert!(step.action == Some(ViewAction::CancelPreview) && step.phase == Phase::FlyBack,
        "Escape during a live drag cancels once and flies back");
    assert!(escaped.escape().action.is_none(), "Escape cannot repeat the cancellation");
}

/// A Library entry carried over the sheet previews the part at each grid cell under the pointer,
/// R turns it, X/Y mirror it, and a click places it with one commit. A click where nothing would
/// be placed keeps it carrying, and Escape puts it down without a note. Its keep result either
/// retags the landing or flies back with the no-room note.
#[test]
fn a_library_carry_previews_each_cell_turns_and_places_with_one_commit() {
    use fab_elec_explore::plan_drop::{Template, TemplateKind};
    let design = Arc::new(fab_elec_fixtures::ledctl());
    let circuit = &design.units[0].circuit;
    let sheet = Arc::new(layout(&design, 0, &SheetHints::default(), &Opts::default()));
    let rows = Arc::new(SheetRows::new().sync(circuit));
    let scene = Scene::build(&design, 0, &sheet, &rows, Tokens::for_theme(true), &SceneState::default(), &SceneLive::default());
    let frozen = || Frozen {
        design: design.clone(), unit: 0, circuit: circuit.clone(), sheet: sheet.clone(), hits: scene.hits.clone(),
    };
    let template = Arc::new(Template {
        stmt: "let r = e.r(10kohm, _n0, _n1)".into(), stem: "r".into(), kind: TemplateKind::Passive,
        pins: vec![("1".into(), fab_elec_parts::PinKind::Passive), ("2".into(), fab_elec_parts::PinKind::Passive)],
    });
    let scale = 16.0;
    // Beyond the sheet's right edge: empty canvas of the root block, one grid cell per unit.
    let b = sheet.bounds;
    let empty = [b[2] as f64 + 10.0, (b[1] as f64 + b[3] as f64) * 0.5];
    assert!(matches!(resolve(&scene.hits, &sheet, empty, scale, &Target::None), Target::Canvas { .. }),
        "precondition: right of the sheet is empty canvas");
    let added = |step: &Step| match &step.action {
        Some(ViewAction::Preview { ops, .. }) => match ops.as_slice() {
            [DesignOp::Circuit { op: CircuitOp::AddPart { stmt, hint: Some(hint), .. }, .. }] if *stmt == template.stmt =>
                Some((hint.at.expect("a canvas drop has a place"), hint.turn, hint.mirror)),
            _ => None,
        },
        _ => None,
    };

    let mut carry = EditGesture::carry(frozen(), template.clone(), "Resistor", empty);
    assert!(carry.carries() && carry.phase() == Phase::Dragging, "a carry has no press to arm: it drags at once");
    let first = carry.sample(empty, scale);
    let (at, turn, mirror) = added(&first).expect("empty canvas plans the AddPart with its sheet place");
    assert_eq!((turn, mirror), (0, false), "a new carry starts unturned and unmirrored");
    assert_eq!(carry.chip().map(|(chip, _)| chip), Some("Place Resistor · R turns · X mirrors · Esc"),
        "over the canvas the chip names the part and its placement keys");
    preview(first);
    assert!(carry.sample([empty[0] + 0.2, empty[1]], scale).action.is_none(), "inside the same grid cell nothing is sent again");
    let next = carry.sample([empty[0] + 1.0, empty[1]], scale);
    let (next_at, _, _) = added(&next).expect("the next cell plans again");
    assert_ne!(next_at, at, "each grid cell is its own place");
    let turned = carry.turn(false);
    assert_eq!(added(&turned), Some((next_at, 3, false)), "R turns the carried part a quarter clockwise in place, as at rest");
    assert_eq!(added(&carry.mirror(false)), Some((next_at, 3, true)), "X toggles the drop hint's mirror and keeps its turn");
    assert_eq!(added(&carry.turn(true)), Some((next_at, 0, true)), "Shift-R turns it back while preserving the mirror");
    assert_eq!(added(&carry.mirror(false)), Some((next_at, 0, false)), "a second X restores the original mirror");
    assert_eq!(added(&carry.mirror(true)), Some((next_at, 2, true)), "Y toggles the mirror and adds a half turn in place");
    let final_sample = carry.sample(empty, scale);
    assert_eq!(added(&final_sample), Some((at, 2, true)), "the drop at the final cell keeps the flipped orientation");
    let tag = preview(final_sample);
    let mut snap = snapshot(design.clone());
    snap.preview = Some(design.clone());
    snap.preview_tag = Some(tag);
    assert_eq!(carry.snapshot(&snap).1, Some(Answer::Shown(tag)), "the host has shown the final carry sample");
    let placed = carry.release(scale);
    assert!(placed.action == Some(ViewAction::CommitPreview) && placed.phase == Phase::Landing,
        "a click places the previewed part with one commit");
    assert!(carry.sample(empty, scale).action.is_none(), "a landing carry sends nothing more");
    let kept = next_tag();
    carry.retag(tag, kept);
    assert!(!carry.shown(), "the replacement preview has not been acknowledged yet");
    snap.preview = None;
    snap.preview_tag = None;
    snap.last_commit = Some(Arc::new(Committed { seq: 1, tag: Some(tag), by: By::Person, created: vec![] }));
    let (step, answer) = carry.snapshot(&snap);
    assert!(step.phase == Phase::Landing && answer.is_none(), "the original tag cannot finish the kept drop");
    let created = vec![Sel::Comp(CompRef { board: circuit.unit, comp: CompId(circuit.comps.len() as u32) })];
    snap.last_commit = Some(Arc::new(Committed { seq: 2, tag: Some(kept), by: By::Person, created: created.clone() }));
    let (step, answer) = carry.snapshot(&snap);
    assert!(step.phase == Phase::Idle && step.action.is_none() && answer == Some(Answer::Committed { created }),
        "the kept commit finishes the carry and reports the newly placed part");

    let mut refused = EditGesture::carry(frozen(), template.clone(), "Resistor", empty);
    let tag = preview(refused.sample(empty, scale));
    refused.release(scale);
    let note = "No room in the block for this part";
    let step = refused.refuse_keep(tag, note.into());
    assert!(step.phase == Phase::FlyBack && step.action == Some(ViewAction::CancelPreview),
        "a no-room keep cancels the released preview and flies back");
    assert_eq!(refused.note(), Some((note, ChipLook::Refused)), "the refusal retains the worker's note");
    assert_eq!(refused.end_point(), empty, "the no-room note stays where the part was dropped");

    // On a part's body nothing would be placed: the click leaves it carrying.
    let body = sheet.parts.iter().find(|part| part.placed).map(|part| part.body).expect("the fixture has a placed part");
    let inside = [(body[0] + body[2]) as f64 * 0.5, (body[1] + body[3]) as f64 * 0.5];
    let mut blocked = EditGesture::carry(frozen(), template.clone(), "Resistor", inside);
    assert!(blocked.sample(inside, scale).action.is_none(), "a part's body is no place for a new part");
    let step = blocked.release(scale);
    assert!(step.action.is_none() && step.phase == Phase::Dragging, "a click where nothing is placed keeps carrying");
    preview(blocked.sample(empty, scale));
    let step = blocked.escape();
    assert!(step.action == Some(ViewAction::CancelPreview) && step.phase == Phase::Idle,
        "Escape puts the carry down: one cancellation, no fly-back note");
}

/// A live press on a button takes the click that ends it (no selection, no reveal; the user's
/// "pressing the button opens a popover" and the mouse-up flicker), while a click with no live press,
/// a click after the press was dragged away, and a click on another part still pick.
#[test]
fn a_live_press_takes_its_click_and_other_clicks_still_pick() {
    let sw = CompRef { board: BoardId(1), comp: CompId(4) };
    let led = CompRef { board: BoardId(1), comp: CompId(9) };
    let mut press = LivePress::default();

    assert_eq!(press.down(sw, true), Some(SimCmd::press(sw, true)), "the press goes down live");
    assert_eq!(press.held(), Some(sw), "the held button draws closed");
    assert_eq!(press.up(), Some(SimCmd::press(sw, false)), "the release goes up with the pointer");
    assert_eq!(press.held(), None, "a released button draws open");
    assert!(press.takes_click(Some(&Sel::Comp(sw))), "the click that ends the press is the press's");
    assert!(!press.takes_click(Some(&Sel::Comp(sw))), "a press takes one click, not the next");

    assert_eq!(press.down(sw, false), None, "a stopped run sends no press");
    assert_eq!(press.held(), None, "nor draws the button closed");
    assert_eq!(press.up(), None, "nor a release");
    assert!(!press.takes_click(Some(&Sel::Comp(sw))), "a click on a button of a stopped run picks it");

    press.down(sw, true);
    press.up();
    assert!(!press.takes_click(Some(&Sel::Comp(led))), "a click elsewhere picks");
    press.down(sw, true);
    press.up();
    press.other();
    assert!(!press.takes_click(Some(&Sel::Comp(sw))), "a press dragged away no longer takes a click");
    press.down(sw, true);
    press.up();
    press.down(led, false);
    assert!(!press.takes_click(Some(&Sel::Comp(led))), "a new press clears the old one's claim");
}

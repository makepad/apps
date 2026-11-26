#[test]
fn journal_links_a_ledctl_session_and_rebuilds_each_revision() {
    use crate::journal::{self, Plate};
    use fab_elec_host::doc_actions::TextChange;
    use makepad_ai_services::wire::CallOrigin;
    use makepad_app_shell::chat::ChatCaller;
    use makepad_strict_json::Value;
    use std::fs;

    let _cx = crate::cx_shared();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    crate::chat::register_elec_actions();
    let root = journal::test_dir();
    let dir = root.join("journal");
    let mut docs = (*ledctl_plate()).clone();
    docs.remove("ledctl.req.splash");
    let start = Arc::new(docs);
    let plate = Plate::testing(&dir, "ledctl.fab.splash", &start);
    let mut host = ElecHost::open(&mut cx, Some(plate.clone()), 1, None, start, Arc::default(), Arc::default(), Arc::default());
    idle(&mut host, &mut cx, "load");
    assert!(!dir.exists(), "just opening a plate writes nothing");
    host.queue_action(&mut cx, "elec.stackup.set", vec![("thickness".into(),"1.2".into())], By::Person, Some(EngineCaller::BoardEdit(7)));
    idle(&mut host, &mut cx, "board form");
    assert_eq!(host.design().unwrap().units[0].board.as_ref().unwrap().stackup.thickness, 1_200_000);
    assert!(host.take_answers().is_empty());

    let ai = crate::electronics_actor(ChatCaller::from_origin(CallOrigin::Conversation, 9), 12);
    let mut calls = crate::chat::Calls::default();
    let args = makepad_strict_json::parse(br#"{"action":"elec.rules.set","args":{"clearance":"0.31"}}"#).unwrap();
    calls.begin(Some(&plate), ai, 12, "fab.elec_edit", &args);
    calls.pending.insert(12, "elec_edit".into());
    host.tool_action(&mut cx, "elec.rules.set", vec![("clearance".into(),"0.31".into())], 12, calls.work_for(12, "fab.elec_edit").unwrap());
    idle(&mut host, &mut cx, "AI rules");
    let answers = host.take_answers();
    assert!(matches!(answers.as_slice(), [(12, true, _)]));
    let (_, ok, text) = answers.into_iter().next().unwrap();
    calls.finish(12, crate::chat::Answer { ok, text, images: vec![] });
    let result = crate::chat::tool_result("chat-rule", calls.answers.remove(&12).unwrap());
    calls.complete(12, "ok", &result.text);
    assert!(matches!(host.take_turn_steps().as_slice(), [(9, _)]), "the conversation edit belongs to turn 9");

    let agent = crate::electronics_actor(ChatCaller::from_origin(CallOrigin::Remote, 9), 13);
    let sent = cx.capture_actions(|cx| {
        host.run_action(cx, "elec.part.place", vec![("part".into(), "lamps[0].r".into()), ("at".into(), "[31mm 22mm]".into())], agent, Some(13));
        idle(&mut host, cx, "remote move");
    });
    let answers = host.take_answers();
    assert!(matches!(answers.as_slice(), [(13, true, text)] if text.contains("checks:") && text.contains("new: [") && text.contains("gone: ")
        && (text.contains("check: \"Dangling\"") || text.contains("check: \"Unconnected\""))),
        "the agent's move must answer with the before/after checks for the track it disconnected");
    assert!(host.take_turn_steps().is_empty(), "a remote edit during turn 9 must add no conversation undo step");
    let cues: Vec<_> = sent.iter().filter_map(|a| a.downcast_ref::<ElecOutsideEdit>()).collect();
    assert!(matches!(cues.as_slice(), [cue] if cue.origin == EditOrigin::Agent && cue.origin.cue(&cue.label) == format!("Agent: {}", cue.label)),
        "the remote edit must emit one pane cue labelled with the agent");

    host.run_action(&mut cx, "elec.req.add", vec![("name".into(),"two layers".into()), ("measure".into(),"board(\"ledctl\").layers".into()),
        ("bound".into(),"{max: 2}".into())], By::Person, None);
    idle(&mut host, &mut cx, "create document");
    let sources = |host: &ElecHost| -> fab_model::Docs {
        host.driver.snap().unwrap().docs.as_ref().unwrap().texts.iter()
            .filter(|doc| !(fab_elec_lang::DocKind::of_name(&doc.name) == Some(fab_elec_lang::DocKind::System) && doc.text.is_empty()))
            .map(|doc| (doc.name.clone(), doc.text.clone())).collect()
    };
    let before_type = sources(&host);
    assert!(before_type.contains_key("ledctl.req.splash"));
    let name = "ledctl.ckt.splash";
    let rev = host.driver.snap().unwrap().docs.as_ref().unwrap().texts.iter().find(|d| d.name == name).unwrap().rev;
    let typed = format!("{}\n// typed change\n", before_type[name]);
    host.run_text(&mut cx, TextAsk { doc: name.into(), rev, change: TextChange::Whole(typed.clone()) }, By::Person, None);
    idle(&mut host, &mut cx, "typed change");
    let before_undo = sources(&host);
    assert!(&*before_undo[name] == typed);
    host.undo(&mut cx);
    idle(&mut host, &mut cx, "undo");
    assert!(sources(&host) == before_type);

    host.run_action(&mut cx, "elec.stackup.set", vec![("finish".into(), "unknown finish".into())], By::Person, None);
    idle(&mut host, &mut cx, "refusal");
    let mut external = before_type;
    external.insert(name.into(), Arc::from(format!("{}\n// outside\n", external[name])));
    host.reload(&mut cx, 1, None, Arc::new(external.clone()), Arc::default());
    idle(&mut host, &mut cx, "outside reload");
    assert!(sources(&host) == external);

    let snap = host.driver.snap().unwrap();
    let board = snap.design.units[0].board.as_ref().unwrap();
    let unit = board.unit;
    let conn = snap.conn.iter().find(|conn| conn.unit == Some(unit) && conn.of == fab_elec_view::StateOf::Committed).unwrap();
    let net = *conn.value.unrouted.first().expect("ledctl has a net for the Route button");
    let net_name = board.nets[net.ix()].name.clone();
    let unit_number = unit.0.to_string();
    let led_path = "lamps[3].d";
    let led = snap.design.units[0].circuit.comp(led_path).unwrap().id;
    let place = |free_panel| ViewAction::Place { unit, parts: vec![led], all: false, free_panel };
    let mut presses = Vec::new();
    for (press, action, args, commits) in [
        (ViewAction::Route { unit, nets: vec![net], method: fab_elec_route::Method::Interactive, reroute: false },
            "elec.route", vec![("unit", unit_number.as_str()), ("nets", net_name.as_str())], true),
        (place(true), "elec.place", vec![("unit", "ledctl"), ("mode", "@place"), ("parts", led_path), ("panel", "@free")], true),
        (place(false), "elec.place", vec![("unit", "ledctl"), ("mode", "@place"), ("parts", led_path)], false),
    ] {
        let before = host.undo_step();
        host.act(&mut cx, press);
        idle(&mut host, &mut cx, action);
        let after = host.undo_step();
        assert_eq!(after != before, commits, "Route and free placement commit; the standing panel LED is held");
        assert!(host.take_answers().is_empty(), "a board button has no chat recipient");
        let args = args.into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect::<Vec<_>>();
        presses.push((action, journal::arguments(&args), after.filter(|_| commits)));
    }

    let raw = "{\"clearance\":";
    let reason = "the arguments do not parse: unexpected end of input";
    journal::Call::refused(&plate, agent, "fab.apply", "remote-malformed", raw, reason);
    let deadline = Instant::now() + HANG_GUARD;
    while plate.pending() != 0 { assert!(Instant::now() < deadline); std::thread::yield_now(); }
    let rows = journal::read_rows(&dir).unwrap();
    let kind = |r: &Value, k: &str| r.get("kind").and_then(Value::as_str) == Some(k);
    let num = |r: &Value, k: &str| r.get(k).and_then(Value::as_u64).unwrap();
    let calls: Vec<_> = rows.iter().filter(|r| kind(r,"call")).collect();
    assert_eq!(calls.len(), 7 + presses.len());
    for (action, args, seq) in presses {
        let matching: Vec<_> = calls.iter().filter(|call| call.get("action").and_then(Value::as_str) == Some(action)
            && call.get("args") == Some(&args)).collect();
        let [call] = matching.as_slice() else { panic!("each board press must record its catalog action and arguments exactly once") };
        assert_eq!(call.get("actor").and_then(|actor| actor.get("kind")).and_then(Value::as_str), Some("person"));
        let id = num(call, "id");
        let done = rows.iter().find(|row| kind(row, "done") && num(row, "id") == id).expect("every board press must finish");
        assert_eq!(done.get("outcome").and_then(Value::as_str), Some(if seq.is_some() { "ok" } else { "refused" }));
        let seqs: Vec<_> = done.get("seqs").unwrap().as_arr().unwrap().iter().map(|s| s.as_u64().unwrap()).collect();
        assert_eq!(seqs, seq.into_iter().collect::<Vec<_>>(), "a press records the undo step its job actually committed");
    }
    let outer = calls.iter().filter(|r| r.get("action").and_then(Value::as_str) == Some("fab.elec_edit")).count();
    assert_eq!(outer, 1, "the electronics tool must have one outer call");
    assert!(calls.iter().all(|r| r.get("action").and_then(Value::as_str) != Some("elec.rules.set")),
        "the electronics host must only bind the outer tool's call");
    let remote = calls.iter().find(|r| r.get("action").and_then(Value::as_str) == Some("elec.part.place")).expect("journal must contain the remote move");
    let actor = remote.get("actor").expect("the journal must record the remote caller");
    assert_eq!(actor.get("kind").and_then(Value::as_str), Some("agent"), "the journal must name the outside agent as actor");
    assert_eq!(actor.get("call").and_then(Value::as_u64), Some(13), "the actor must carry the remote call id");
    assert!(actor.get("turn").is_none(), "the journal must give the outside agent no conversation turn");
    assert_eq!(rows.iter().filter(|r| kind(r,"done")).count(), calls.len());
    for call in calls {
        let id = num(call,"id");
        let done = rows.iter().find(|r| kind(r,"done") && num(r,"id") == id).unwrap();
        if call.get("action").and_then(Value::as_str) == Some("fab.apply") {
            assert_eq!(call.get("args").and_then(Value::as_str), Some(raw), "malformed arguments must be kept verbatim");
            assert_eq!(done.get("outcome").and_then(Value::as_str), Some("refused"));
            assert_eq!(done.get("reply").and_then(Value::as_str), Some(reason), "the refusal must contain only the parser's reason");
        }
        let mut effects: Vec<_> = rows.iter().filter(|r| kind(r,"effect") && num(r,"call") == id).map(|r| num(r,"seq")).collect();
        effects.sort_unstable();
        let seqs: Vec<_> = done.get("seqs").unwrap().as_arr().unwrap().iter().map(|s| s.as_u64().unwrap()).collect();
        assert_eq!(seqs, effects);
        assert_eq!(effects.len(), usize::from(done.get("outcome").and_then(Value::as_str) == Some("ok")));
        if call.get("action").unwrap().as_str() == Some("fab.elec_edit") {
            assert_eq!(call.get("args"), Some(&args), "the outer call records the tool arguments as received");
            assert_eq!(num(call.get("actor").unwrap(),"turn"), 9);
        }
    }
    assert_eq!(rows.iter().filter(|r| kind(r,"effect")).count(),7);
    assert_eq!(rows.iter().filter(|r| kind(r,"reload")).count(),1);
    let undo = rows.iter().find(|r| kind(r,"history")).unwrap();
    for (bound, folder, expected) in [(Some(num(undo,"order") - 1), "before", before_undo), (None, "after", sources(&host))] {
        let copy = root.join(folder);
        journal::rebuild(&dir, bound, &copy).unwrap();
        assert_eq!(fs::read(copy.join("ledctl.fab.splash")).unwrap(), b"// fixture\nuse mod.fab.*\n");
        let (docs, assets, _) = fab_model::project::load_elec(&copy.join("ledctl.fab.splash")).unwrap();
        assert!(docs == expected, "rebuilt documents at {folder}");
        assert!(assets.is_empty());
    }
    use journal::replay::{Mode, Outcome, Request, StepKind};
    let replayed = root.join("calls");
    let mut steps = Vec::new();
    let report = journal::replay::replay(Request { journal: &dir, upto: None, into: &replayed, mode: Mode::Calls },
        &mut |step| steps.push(step.clone())).unwrap();
    assert_eq!(report.stopped, None, "the ledctl calls, including route and place, must reproduce their effects");
    for action in ["elec.stackup.set", "fab.elec_edit", "elec.route", "elec.place"] {
        assert!(steps.iter().any(|s| s.action.as_deref() == Some(action) && s.outcome == Outcome::Matched), "{action}");
    }
    assert!(steps.iter().any(|s| s.kind == StepKind::History && s.outcome == Outcome::Applied));
    let (docs, _, _) = fab_model::project::load_elec(&replayed.join("ledctl.fab.splash")).unwrap();
    assert_eq!(docs, sources(&host));
    let names: BTreeSet<_> = fs::read_dir(&root).unwrap().map(|f| f.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names, ["after".into(),"before".into(),"calls".into(),"journal".into(),"journal.lock".into(),"presence".into()].into());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn journal_planner_gestures_replay_the_gestures_document_hashes() {
    use crate::journal::{self, reader::{Filter, Key, Position, Reader}, Plate};
    use fab_elec_core::{Angle, PinIx};
    use fab_elec_view::board::{plan::{Arrangement, Axis, Edge}, tools::{BoardTool, PartEdit}, view::{part_gesture, shape_gesture}};
    use fab_elec_view::explorer::view::{self as sheet, Gesture, Planned, Target, Toward};
    use makepad_core_util::sha256::sha256_hex;
    use makepad_strict_json::Value;
    use std::fs;

    let _guard = crate::cx_shared();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    crate::chat::register_elec_actions();
    let root = journal::test_dir();
    let dir = root.join("gesture").join("journal");
    let mut docs = (*ledctl_plate()).clone();
    let text = &docs["ledctl.ckt.splash"];
    docs.insert("ledctl.ckt.splash".into(), format!("{text}\nlet test_net = e.net({{name: \"WITNESS\"}})\nlet witness = e.r(10kohm, test_net, v33, {{package: \"0402\"}})\nlet spare = e.r(1kohm, gnd, v33)\n").into());
    let docs = Arc::new(docs);
    let plate = Plate::testing(&dir, "ledctl.fab.splash", &docs);
    let open = |cx: &mut Cx, plate| ElecHost::open(cx, plate, 1, None, docs.clone(), Arc::default(), Arc::default(), Arc::default());
    let mut person = open(&mut cx, None);
    idle(&mut person, &mut cx, "gesture fixture");
    person.plate_open(&mut cx, Some(plate.clone()), 1, None, docs.clone(), Arc::default());
    idle(&mut person, &mut cx, "attach the demo journal");
    let mut replay = open(&mut cx, None);
    idle(&mut replay, &mut cx, "replay fixture");
    let hashes = |host: &ElecHost| -> BTreeMap<_, _> {
        host.driver.snap().unwrap().docs.as_ref().unwrap().texts.iter()
            .map(|d| (d.name.clone(), sha256_hex(d.text.as_bytes()))).collect()
    };
    let durable = || {
        let until = Instant::now() + HANG_GUARD;
        while plate.pending() != 0 { assert!(Instant::now() < until); std::thread::yield_now(); }
        journal::read_rows(&dir).unwrap()
    };
    let num = |r: &Value, field: &str| r.get(field).and_then(Value::as_u64);
    let kind = |r: &Value, k: &str| r.get("kind").and_then(Value::as_str) == Some(k);
    let mut rotate = None;
    let names = ["elec.part.rotate", "elec.part.flip", "elec.part.align", "elec.part.distribute",
        "elec.board.outline", "elec.keepout.add", "elec.cutout.add", "elec.net.disconnect",
        "elec.pin.nc", "elec.pin.nc", "elec.net.connect", "elec.net.rename", "elec.part.rename",
        "elec.part.add", "elec.part.remove"];
    for (i, name) in names.into_iter().enumerate() {
        let snap = person.driver.snap().unwrap().clone();
        let u = &snap.design.units[0];
        let board = u.board.as_ref().unwrap();
        let id = |path| u.circuit.comp(path).expect("fixture component").id;
        let witness = id("witness");
        let pin = Target::Pin { comp: witness, pin: PinIx(0), toward: Toward::Across };
        let wire = |name| Target::Wire { net: u.circuit.net(name).unwrap().id, run: 0, seg: 0, at: [0,0] };
        let mut preview = None;
        let mut submit = |planned: Planned, call, land: bool| {
            let Planned::Edit(r) = planned else { panic!("{name}: the real gesture must plan an edit") };
            let action = if land {
                let tag = fab_elec_view::snapshot::next_tag();
                preview = Some(ViewAction::Preview { tag, ops: r.ops, place: r.place, label: r.label, notes: vec![] });
                ViewAction::Land(tag)
            } else { ViewAction::Ops { ops: r.ops, place: r.place, label: r.label } };
            fab_elec_view::snapshot::Invocation::attach(call, action)
        };
        let action = match i {
            0 => part_gesture(&snap.design, u.id, &[id("reg.u")], PartEdit::Rotate(Angle::deg(90.0))),
            1 => part_gesture(&snap.design, u.id, &[id("reg.u")], PartEdit::Flip),
            2 => part_gesture(&snap.design, u.id, &[id("reg.c_in"), id("reg.c_out")], PartEdit::Arrange(Arrangement::Align(Edge::Left))),
            3 => part_gesture(&snap.design, u.id, &[id("lamps[0].r"), id("lamps[1].r"), id("lamps[7].r")], PartEdit::Arrange(Arrangement::Distribute(Axis::X))),
            4..=6 => {
                let tool = [BoardTool::Outline, BoardTool::Keepout, BoardTool::Cutout][i-4];
                let points = if i == 4 { [(0,0), (52,0), (52,32), (0,32)] } else { [(2,2), (5,2), (5,5), (2,5)] };
                shape_gesture(tool, &snap.design, board, Some(fab_elec_core::LayerId::B_CU), None,
                    points.map(|(x,y)| P::new(x*1_000_000+1,y*1_000_000)).to_vec())
            }
            7..=11 | 14 => {
                let gesture = match i {
                    7 => Gesture::Delete(pin),
                    8 | 9 => Gesture::NoConnect(pin),
                    10 => Gesture::Wire { from: pin, to: wire("GND") },
                    11 => Gesture::NameNet { target: wire("WITNESS"), to: "RENAMED".into() },
                    _ => Gesture::Delete(Target::None),
                };
                let parts = if i == 14 { vec![witness] } else { vec![] };
                let (p, call) = sheet::key_plan(u, u, gesture, &parts);
                submit(p, call.expect("the sheet can name its target"), i == 10)
            }
            12 => {
                let (p, call) = sheet::reference_gesture(u, witness, "R200").unwrap();
                submit(p, call.unwrap(), false)
            }
            13 => {
                let entry = snap.catalog.as_ref().unwrap().entries.iter().find(|e| e.stem == "r").unwrap();
                let p = sheet::plan_drop(&u.circuit, &sheet::template_of(entry), pin, 0, None).planned;
                let key = fab_elec_view::library::EntryKey { name: entry.name.clone(), lib: entry.lib.clone() };
                let call = sheet::library_invocation(u, &key, pin, &p).expect("a series drop has catalog arguments");
                submit(p, call, true)
            }
            _ => unreachable!(),
        };
        let ViewAction::Planned { call: proposed, .. } = &action else { panic!("{name}: the view must identify this gesture") };
        assert_eq!(proposed.name, name);
        let expected_args = journal::arguments(&proposed.args);
        let before = hashes(&replay);
        assert!(before == hashes(&person), "{name}: both paths start from identical documents");
        if let Some(preview) = preview { person.act(&mut cx, preview); idle(&mut person, &mut cx, "shown preview"); }
        let prior = person.undo_step();
        let sent = cx.capture_actions(|cx| fab_elec_view::snapshot::Invocation::send(cx, makepad_widgets::WidgetUid(1), action));
        let [widget] = sent.as_slice() else { panic!("{name}: one press must emit one widget action") };
        let sent = widget.as_widget_action().and_then(|w| w.action.downcast_ref::<ViewAction>()).unwrap();
        assert!(matches!(sent, ViewAction::Planned { .. }), "{name}: a demo attached after host startup must retain the widget's invocation");
        person.act(&mut cx, sent.clone());
        idle(&mut person, &mut cx, name);
        let seq = person.undo_step().filter(|s| Some(*s) != prior).unwrap_or_else(|| panic!("{name}: the press must commit"));
        let rows = durable();
        let effect = rows.iter().find(|r| kind(r,"effect") && num(r,"seq") == Some(seq)).expect("the actual undo step has an effect");
        let call_id = num(effect, "call").unwrap_or_else(|| panic!("{name}: the effect must link to the press"));
        let calls: Vec<_> = rows.iter().filter(|r| kind(r,"call") && num(r,"id") == Some(call_id)).collect();
        let [call] = calls.as_slice() else { panic!("{name}: expected one call") };
        assert_eq!(call.get("actor").and_then(|v| v.get("kind")).and_then(Value::as_str), Some("person"));
        assert!(call.get("args") == Some(&expected_args), "{name}: the arguments published by the pane must reach the call");
        assert_eq!(call.get("action").and_then(Value::as_str), Some(name));
        let done: Vec<_> = rows.iter().filter(|r| kind(r,"done") && num(r,"id") == Some(call_id)).collect();
        assert!(matches!(done.as_slice(), [r] if r.get("outcome").and_then(Value::as_str) == Some("ok")
            && r.get("seqs").and_then(Value::as_arr).is_some_and(|seqs| seqs.len() == 1 && seqs[0].as_u64() == Some(seq))), "{name}: one done must name this undo step");
        let args = call.get("args").unwrap().as_arr().unwrap().iter().map(|pair| {
            let field = |f| pair.get(f).unwrap().as_str().unwrap().to_owned();
            (field("name"), field("value"))
        }).collect();
        replay.run_action(&mut cx, name, args, By::Person, None);
        idle(&mut replay, &mut cx, "invoke recorded arguments");
        let after = hashes(&replay);
        let changes = effect.get("changes").unwrap().as_arr().unwrap();
        assert!(!changes.is_empty() && before != after, "{name}: replay must change source");
        for change in changes {
            let path = change.get("name").unwrap().as_str().unwrap().trim_start_matches("ledctl.elec/");
            assert!(change.get("before").and_then(Value::as_str) == before.get(path).map(String::as_str), "{name}: before hash differs");
            assert!(change.get("after").and_then(Value::as_str) == after.get(path).map(String::as_str), "{name}: replay differs from the recorded effect");
        }
        assert!(after == hashes(&person), "{name}: every replayed document must match the gesture");
        if i == 0 { rotate = Some((num(call,"order").unwrap(), num(done[0],"order").unwrap())); }
    }

    // Pulling an attached end onto a different named net changes only that pin.
    // The connect action starts a new wire and would join the nets, or refuse.
    let u = &person.driver.snap().unwrap().design.units[0];
    let spare = u.circuit.comp("spare").unwrap().id;
    let from = Target::WireEnd { comp: spare, pin: PinIx(0), net: u.circuit.net_of(spare, PinIx(0)).unwrap() };
    let to = Target::Wire { net: u.circuit.net_of(spare, PinIx(1)).unwrap(), run: 0, seg: 0, at: [0,0] };
    let wire = Gesture::Wire { from, to };
    let planned = sheet::plan(&u.circuit, &wire);
    let candidate = sheet::sheet_invocation(u, &wire, &[], &planned);
    let Planned::Edit(edit) = planned else { panic!("moving the end must plan the rewiring") };
    let tag = fab_elec_view::snapshot::next_tag();
    let calls_before = durable().iter().filter(|r| kind(r, "call")).count();
    let prior = person.undo_step();
    person.act(&mut cx, ViewAction::Preview { tag, ops: edit.ops, place: edit.place, label: edit.label, notes: vec![] });
    let landing = candidate.map_or(ViewAction::CommitPreview, |call| call.attach(ViewAction::Land(tag)));
    person.act(&mut cx, landing);
    idle(&mut person, &mut cx, "relocate the wire end");
    let rows = durable();
    assert_ne!(person.undo_step(), prior, "the original wire-end gesture must still commit");
    assert_eq!(rows.iter().filter(|r| kind(r, "call")).count(), calls_before, "moving an end must not claim a catalog connect");
    assert!(rows.iter().any(|r| kind(r, "effect") && num(r, "seq") == person.undo_step()
        && r.get("call").is_some_and(Value::is_null)), "wire relocation must copy as its own source patch");

    let u = &person.driver.snap().unwrap().design.units[0];
    let board = u.board.as_ref().unwrap();
    let placed = board.placed_of(u.circuit.comp("reg.u").unwrap().id).unwrap();
    person.act(&mut cx, ViewAction::Preview { tag: fab_elec_view::snapshot::next_tag(), ops: vec![DesignOp::Board { unit: u.id,
        op: BoardOp::Move { item: placed.id, by: P::new(100_000,0) } }], label: "Drag".into(), place: vec![], notes: vec![] });
    person.act(&mut cx, ViewAction::CommitPreview);
    idle(&mut person, &mut cx, "plain drag");
    let rows = durable();
    assert!(rows.iter().any(|r| kind(r,"effect") && num(r,"seq") == person.undo_step() && r.get("call").is_some_and(Value::is_null)), "a drag must remain a plain effect");
    let u = &person.driver.snap().unwrap().design.units[0];
    let back = part_gesture(person.design().unwrap(), u.id, &[u.circuit.comp("reg.u").unwrap().id], PartEdit::Rotate(Angle::deg(-90.0)));
    let ViewAction::Planned { call, .. } = back else { panic!("Shift+R must retain its signed quarter turn") };
    assert!(call.args.iter().any(|(k, v)| k == "by" && v == "-90"), "the script must preserve the direction of Shift+R");
    let refused = part_gesture(person.design().unwrap(), u.id, &[], PartEdit::Flip);
    person.act(&mut cx, refused);
    let rows = durable();
    assert!(rows.last().is_some_and(|r| kind(r,"done") && r.get("outcome").and_then(Value::as_str) == Some("refused")
        && r.get("seqs").unwrap().as_arr().unwrap().is_empty()), "an empty selection records its refused press");
    let mut reader = Reader::start(&cx.thread_spawner(), root.clone()).unwrap();
    let watch = reader.select("gesture").unwrap();
    let (a,b) = rotate.unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    reader.query(move |browse| {
        let bview = browse.unwrap();
        let key = |order| Key { folder: watch.folder.clone(), journal: bview.journal().unwrap(), position: Position::Order(order) };
        tx.send(bview.script(&key(a), &key(b), Filter::default(), bview.complete_end()).unwrap()).unwrap();
    }).unwrap();
    let until = Instant::now() + HANG_GUARD;
    let script = loop {
        reader.try_recv().unwrap();
        if let Ok(text) = rx.try_recv() { break text }
        assert!(Instant::now() < until);
        std::thread::yield_now();
    };
    assert!(script.lines().count() == 1 && script.starts_with("elec.part.rotate "), "Copy as script must emit one rotate invocation");
    let args = makepad_strict_json::parse(script.split_once(' ').unwrap().1.as_bytes()).unwrap();
    assert_eq!(args.get("legalise").and_then(Value::as_str), Some("off"));
    assert_eq!(args.get("by").and_then(Value::as_str), Some("90"));
    drop((reader, person, replay, plate));
    fs::remove_dir_all(root).unwrap();
}

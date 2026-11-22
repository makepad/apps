use super::*;
use a11y_tree::{ActionRequest, NodeId, query::Query};
use crate::instruments::{acquire::TriggerState, semantics::Summary};

#[test]
fn lane_height_lookup_and_handle_share_the_resize_contract() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut lane = cx.with_vm(|vm| {
        makepad_platform::script::script_mod(vm);
        makepad_draw::script_mod(vm);
        makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
        crate::live::script_mod(vm);
        crate::trace::script_mod(vm);
        crate::instruments::script_mod(vm);
        let value = vm.eval(script! {
            use mod.prelude.widgets_internal.*
            use mod.widgets.*
            SignalLane {}
        });
        SignalLane::script_from_value(vm, value)
    });
    let query = Query::parse("name=height").unwrap();
    let mut nodes = Vec::new();
    lane.lookup(&cx, &Lookup::new(&SemanticLookup::Terms(&query), None).unwrap(), &mut nodes);
    assert_eq!(nodes.len(), 1, "every lane exposes one height value before drawing");
    let key = nodes[0].0;
    assert_eq!(nodes[0].2.value, "160 pt", "lookup reports the default in points");
    assert!(nodes[0].2.actions.contains(A::SET_VALUE), "height accepts an exact numeric action");
    let request = |arg| ActionRequest { req: 1, node: NodeId::part(7, 1), action: A::SET_VALUE, arg };
    for collapsed in [false, true] {
        lane.set_collapsed(&mut cx, collapsed);
        for arg in [ActionArg::Number(55.99), ActionArg::Number(39.0), ActionArg::Number(-1.0), ActionArg::Number(f64::NAN), ActionArg::Number(f64::INFINITY), ActionArg::Text("55 pt".into()), ActionArg::Text("80 V".into())] {
            let actions = cx.capture_actions(|cx| {
                assert_eq!(lane.lookup_action(cx, key, &request(arg.clone())), ActionResult::Unsupported, "invalid height uses the existing lookup refusal");
            });
            assert_eq!((lane.height, lane.collapsed()), (160.0, collapsed), "refusal preserves both height and meter state");
            assert_eq!(actions.filter_widget_actions_cast::<ScopeAction>(lane.uid).count(), 0, "refusal emits no resize or reconfigure");
        }
    }
    for (arg, height) in [(ActionArg::Number(56.0), 56.0), (ActionArg::Text("245.5 pt".into()), 245.5), (ActionArg::Number(10000.0), 10000.0)] {
        let was_collapsed = lane.collapsed();
        let actions = cx.capture_actions(|cx| {
            assert_eq!(lane.lookup_action(cx, key, &request(arg.clone())), ActionResult::Handled);
        });
        let actions = actions.filter_widget_actions_cast::<ScopeAction>(lane.uid).collect::<Vec<_>>();
        let mut expected = Vec::new();
        if was_collapsed { expected.push(ScopeAction::Reconfigure(lane.settings().clone())); }
        expected.push(ScopeAction::Resize { height, done: true });
        assert_eq!(actions, expected, "lookup publishes the same collapse transition and completed resize as the handle");
        assert_eq!((lane.height, lane.collapsed()), (height, false), "a valid height opens the signal lane at the exact requested size");
        assert_eq!(lane.semantic_items(&cx).node(Item::Control(Part::Resize)).value, format!("{height} pt"), "lookup follows the current session height");
    }
    lane.apply_resize(&mut cx, resize(160.0, 0.0, false), true);
    lane.plot = Rect { pos: dvec2(0.0, 0.0), size: dvec2(200.0, 160.0) };
    lane.parts.push(PartRect { part: Part::Resize, rect: Rect { pos: dvec2(0.0, 156.0), size: dvec2(200.0, 4.0) }, selected: false });
    let point = dvec2(100.0, 158.0);
    assert_eq!(lane.semantic_items(&cx).hit(point), Some(Item::Control(Part::Resize)), "the bottom edge wins over plot gestures");
    assert_eq!(lane.pointer_cursor(point), MouseCursor::RowResize, "the quiet handle advertises row resizing");
    lane.drag = Some(Drag { part: Part::Resize, grab: point.y, start: lane.height, pending: None, extent: 0.0 });
    for (y, height, collapsed, transition) in [(37.0, 56.0, true, true), (53.0, 56.0, true, false), (54.0, 56.0, false, true), (78.0, 80.0, false, false)] {
        let actions = cx.capture_actions(|cx| lane.drag_to(cx, dvec2(point.x, y), false));
        let actions = actions.filter_widget_actions_cast::<ScopeAction>(lane.uid).collect::<Vec<_>>();
        let mut expected = Vec::new();
        if transition { expected.push(ScopeAction::Reconfigure(lane.settings().clone())); }
        expected.push(ScopeAction::Resize { height, done: false });
        assert_eq!(actions, expected, "a snap uses the collapse glyph's action and continues publishing live height");
        assert_eq!((lane.height, lane.collapsed()), (height, collapsed), "the gesture rebases at each snap and continues from 56 pt");
        assert_eq!(lane.pointer_cursor(dvec2(point.x, y)), MouseCursor::RowResize, "a captured resize keeps its cursor beyond the handle");
    }
}

#[test]
fn scope_value_entry_and_lookup_share_exact_values_and_setter_actions() {
    let initial = ScopeSettings {
        channels: vec![Channel::default(), Channel { unit: "A".into(), volts_per_div: 2.0, ..Default::default() }],
        position: 0.37, ..Default::default()
    };
    let cases = [
        (Item::Control(Part::Time), "0.0002", "200u", "200 µs/div", 200e-6, false),
        (Item::Control(Part::Frequency), "0.00002", "20u", "20 us/div", 20e-6, false),
        (Item::Control(Part::SpectrumScale), "2", "2000m", "2000 mdB/div", 2.0, true),
        (Item::Control(Part::SpectrumOffset), "-3", "-3000m", "-3 dBV", -3.0, true),
        (Item::Control(Part::Scale(0)), "0.25", "250m", "250 mV/div", 0.25, false),
        (Item::Control(Part::Scale(1)), "2.5", "2500m", "2500 mA/div", 2.5, false),
        (Item::Control(Part::Offset(0)), "0.402", "402m", "402 mV", 0.402, true),
        (Item::Control(Part::Offset(1)), "0.04", "40m", "40 mA", 0.04, true),
        (Item::Control(Part::LevelMarker), "1.65", "1650m", "1.65 V", 1.65, false),
        (Item::Control(Part::PositionMarker), "25", "25000m", "25 %", 25.0, true),
        (Item::Control(Part::Divider), "25", "25000m", "25 %", 25.0, false),
        (Item::Control(Part::CellDivider), "25", "25000m", "25 %", 25.0, false),
        (Item::Holdoff, "0.00002", "20u", "20 µs", 20e-6, false),
    ];
    for (item, bare, prefix, unit, value, clear) in cases {
        let mut expected = initial.clone();
        match item {
            Item::Control(Part::Time | Part::Frequency) => expected.time_per_div = value,
            Item::Control(Part::SpectrumScale) => expected.fft.db_per_div = value,
            Item::Control(Part::SpectrumOffset) => expected.fft.ref_dbv = value,
            Item::Control(Part::Scale(i)) => expected.channels[i].volts_per_div = value,
            Item::Control(Part::Offset(i)) => expected.channels[i].offset_div = value / expected.channels[i].volts_per_div,
            Item::Control(Part::LevelMarker) => expected.trigger.level = value,
            Item::Control(Part::PositionMarker) => expected.position = value / 100.0,
            Item::Control(Part::Divider) => expected.fft.waterfall_share = value / 100.0,
            Item::Control(Part::CellDivider) => expected.fft.cell_share = value / 100.0,
            Item::Holdoff => expected.trigger.holdoff = value,
            _ => unreachable!(),
        }
        let prefixed_unit = format!("{prefix}{}", value_unit(&initial, item).unwrap());
        for text in [bare, prefix, unit, prefixed_unit.as_str()] {
            let mut settings = initial.clone();
            let actions = set_value_text(&mut settings, item, text).expect("entry accepts the part's implied or explicit unit");
            assert_eq!(settings, expected, "{item:?}: {text} edits exactly the intended value");
            assert_eq!(actions, expected.actions(clear), "{item:?}: {text} emits the setter's clear/reconfigure actions");
            let mut lookup = initial.clone();
            let request = ActionRequest { req: 1, node: NodeId::part(7, 1), action: A::SET_VALUE, arg: ActionArg::Text(text.into()) };
            assert_eq!(set_value(&mut lookup, item, &request), Ok(actions), "{item:?}: lookup text uses the entry path");
            assert_eq!(lookup, expected, "{item:?}: lookup and entry have identical settings");
        }
        let mut settings = initial.clone();
        let request = ActionRequest { req: 1, node: NodeId::part(7, 1), action: A::SET_VALUE, arg: ActionArg::Number(value) };
        assert_eq!(set_value(&mut settings, item, &request), Ok(expected.actions(clear)), "{item:?}: numeric lookup arguments stay in base units");
        assert_eq!(settings, expected);
        let prefill = value_text(&settings, item).unwrap();
        assert_eq!(set_value_text(&mut settings, item, &prefill), Ok(Vec::new()), "{item:?}: accepting the displayed value is a no-op");
        assert_eq!(settings, expected, "{item:?}: prefill retains the value and unit");
    }
}

#[test]
fn scope_value_entry_explains_rejection_without_mutating_settings() {
    let cases = [
        (Item::Control(Part::Time), "not a time per division", "out of range: 1 ps/div to 1 Ts/div", "0.1 ps/div", "2 Ts/div"),
        (Item::Control(Part::Frequency), "not a time per division", "out of range: 1 ps/div to 1 Ts/div", "0", "2 Ts/div"),
        (Item::Control(Part::SpectrumScale), "not a dB per division", "out of range: 0.1 dB/div to 50 dB/div", "0.09 dB/div", "51 dB/div"),
        (Item::Control(Part::SpectrumOffset), "not a dBV value", "out of range: a finite dBV value", "-inf dBV", "inf dBV"),
        (Item::Control(Part::Scale(0)), "not a V per division", "out of range: 1 pV/div to 1 TV/div", "0.1 pV/div", "2 TV/div"),
        (Item::Control(Part::Offset(0)), "not a V value", "out of range: a finite offset in divisions", "-inf V", "inf V"),
        (Item::Control(Part::LevelMarker), "not a V value", "out of range: a finite V value", "-inf V", "inf V"),
        (Item::Control(Part::PositionMarker), "not a percentage", "out of range: 0 % to 100 %", "-1 %", "101 %"),
        (Item::Control(Part::Divider), "not a percentage", "out of range: 15 % to 75 %", "14 %", "76 %"),
        (Item::Control(Part::CellDivider), "not a percentage", "out of range: 20 % to 60 %", "19 %", "61 %"),
        (Item::Holdoff, "not a time", "out of range: 0 s or more, finite", "-1 s", "inf s"),
    ];
    let mut settings = ScopeSettings::default();
    let before = settings.clone();
    for (item, parse_error, range_error, below, above) in cases {
        for (text, reason) in [("1 Hz", parse_error), ("not a number", parse_error), (below, range_error), (above, range_error), ("NaN", range_error), ("1e999", range_error)] {
            assert_eq!(set_value_text(&mut settings, item, text), Err(reason.into()), "{item:?}: {text} explains the failure");
            assert_eq!(settings, before, "{item:?}: {text} changes no setting");
            let request = ActionRequest { req: 1, node: NodeId::part(7, 1), action: A::SET_VALUE, arg: ActionArg::Text(text.into()) };
            assert_eq!(set_value(&mut settings, item, &request), Err(ActionResult::Unsupported), "{item:?}: lookup reports its existing action error");
            assert_eq!(settings, before, "{item:?}: rejected lookup text is atomic too");
        }
        for value in [f64::NAN, f64::INFINITY] {
            let request = ActionRequest { req: 1, node: NodeId::part(7, 1), action: A::SET_VALUE, arg: ActionArg::Number(value) };
            assert_eq!(set_value(&mut settings, item, &request), Err(ActionResult::Unsupported), "{item:?}: numeric lookup arguments must also be finite");
            assert_eq!(settings, before, "{item:?}: rejected numeric lookup is atomic");
        }
    }
    settings.channels[0].volts_per_div = DIV_MIN;
    let before = settings.clone();
    assert_eq!(set_value_text(&mut settings, Item::Control(Part::Offset(0)), "1e308 V"), Err("out of range: a finite offset in divisions".into()), "a finite voltage may overflow in divisions");
    assert_eq!(settings, before);
    settings.channels[0].offset_div = 1e308;
    let before = settings.clone();
    assert_eq!(set_value_text(&mut settings, Item::Control(Part::Scale(0)), "2 V/div"), Err("out of range: scale must give a finite, nonzero value span".into()), "the continuous setter's span check also has a reason");
    assert_eq!(settings, before, "a rejected span cannot partly change the scale");
    assert_eq!(set_value_text(&mut settings, Item::Control(Part::Scale(9)), "2"), Err("not a settable value".into()), "a removed channel is no longer editable");
    assert_eq!(settings, before);
    assert_eq!(semantics::number(&ActionArg::Text("inf V".into()), "V"), None, "other lookup callers still reject nonfinite text");
    assert_eq!(semantics::pair(&ActionArg::Text("0 s .. inf s".into()), "s"), None, "brackets still reject nonfinite endpoints");
}

#[test]
fn scope_value_entry_accepts_the_actual_setter_boundaries() {
    let mut settings = ScopeSettings::default();
    for (item, low, high) in [
        (Item::Control(Part::Time), "1 ps/div", "1 Ts/div"),
        (Item::Control(Part::Frequency), "1 ps/div", "1 Ts/div"),
        (Item::Control(Part::Scale(0)), "1 pV/div", "1 TV/div"),
        (Item::Control(Part::SpectrumScale), "0.1 dB/div", "50 dB/div"),
        (Item::Control(Part::PositionMarker), "0 %", "100 %"),
        (Item::Control(Part::Divider), "15 %", "75 %"),
        (Item::Control(Part::CellDivider), "20 %", "60 %"),
        (Item::Holdoff, "0 s", "1e308 s"),
        (Item::Control(Part::LevelMarker), "-1e308 V", "1e308 V"),
        (Item::Control(Part::Offset(0)), "-1e308 V", "1e308 V"),
        (Item::Control(Part::SpectrumOffset), "-1e308 dBV", "1e308 dBV"),
    ] {
        for text in [low, high] { assert!(set_value_text(&mut settings, item, text).is_ok(), "{item:?}: {text} is inside the setter's limits"); }
    }
}

#[test]
fn runt_level_lookup_sets_volts_and_refuses_invalid_pairs() {
    use crate::instruments::acquire::TriggerSlot;
    let mut settings = ScopeSettings { trigger: Trigger { level: 0.9, ..Default::default() }, ..Default::default() };
    assert!(!settings.set_runt(Some(2.4)).is_empty(), "a valid second level publishes a runt qualifier");
    assert_eq!(settings.trigger_qualifier_label(), "runt < 2.4 V", "rising tag names the upper bound");
    let parts = [PartRect { part: Part::RuntLevel, rect: Rect { pos: dvec2(120.0, 20.0), size: dvec2(22.0, 20.0) }, selected: false }];
    let answers = Answers::default();
    let items = Items {
        net: "signal", settings: &settings, answers: &answers, names: &[], parts: &parts,
        widths: &[], indices: &[], rows: &[], rows_origin: dvec2(0.0, 0.0), sources: &[], primary: 0, more: 0,
        trigger: true, shared: true, collapsed: false, closable: false,
        bracket_visible: false, bracket: None, values: None, delays: &[],
        plot: Rect { pos: dvec2(0.0, 0.0), size: dvec2(160.0, 100.0) }, shift: dvec2(0.0, 0.0),
        clip: Some(Rect { pos: dvec2(0.0, 0.0), size: dvec2(180.0, 120.0) }), time: None, holdoff: None, focused: None, lane: None,
    };
    let query = Query::parse("name=\"runt level\"").unwrap();
    let lookup = SemanticLookup::Terms(&query);
    let mut nodes = Vec::new();
    items.lookup(&Lookup::new(&lookup, None).unwrap(), &mut nodes);
    assert_eq!(nodes.len(), 1, "a configured runt exposes one stable level node");
    assert_eq!(nodes[0].2.value, "2.4 V", "lookup reports the second level in volts");
    assert!(nodes[0].2.actions.contains(A::SET_VALUE) && !nodes[0].2.bounds.is_empty(), "the drawn runt marker advertises a typed value action");
    assert_eq!(items.find(nodes[0].0), Some(Item::Control(Part::RuntLevel)), "lookup resolves the new marker's stable key");
    let request = |arg| ActionRequest { req: 1, node: NodeId::part(7, 1), action: A::SET_VALUE, arg };
    let item = Item::Control(Part::RuntLevel);
    let actions = set_value(&mut settings, item, &request(ActionArg::Text("2800 mV".into()))).unwrap();
    assert_eq!(settings.trigger.qualify, Qualifier::Runt { other: 2.8 }, "SET_VALUE converts voltage units into the qualifier");
    assert_eq!(actions, settings.actions(false), "runt edits reconfigure without clearing the picture");
    assert_eq!(set_value(&mut settings, item, &request(ActionArg::Number(2.8))), Ok(Vec::new()), "an unchanged numeric value is accepted without actions");
    let before = settings.clone();
    for arg in [ActionArg::Number(0.9 + settings.trigger().hyst), ActionArg::Number(-2.0), ActionArg::Number(f64::NAN), ActionArg::Text("1 Hz".into())] {
        assert_eq!(set_value(&mut settings, item, &request(arg)), Err(ActionResult::Unsupported), "invalid runt values return the existing refused action without reason text");
        assert_eq!(settings, before, "refused runt actions change no settings");
    }
    let clamped = settings.clamp_runt_level(-2.0);
    assert!(validate_runt(&settings.trigger(), clamped), "dragging across the trigger stops outside the band");
    assert_eq!(clamped, (settings.trigger.level + settings.trigger().hyst).next_up(), "drag clamping uses the first valid value beyond the band");
    let mut moved = settings.clone();
    assert!(!moved.set_trigger_level(3.0).is_empty() && moved.trigger.level < 2.8 - moved.trigger().hyst, "a level edit stops a band short of the runt level, keeping its side");
    let mut sloped = settings.clone();
    assert!(!sloped.set_trigger_slope(Slope::Falling).is_empty() && sloped.trigger.qualify == settings.trigger.qualify, "a slope edit keeps the runt; its side sets the direction");
    let slot = TriggerSlot::default();
    slot.set(settings.trigger(), settings.trigger.qualify, None, None);
    let (_, trigger, qualifier, _, _) = slot.read(0).unwrap();
    assert_eq!((trigger, qualifier), (settings.trigger(), Qualifier::Runt { other: 2.8 }), "runt kind and second level cross the atomic slot together");
    settings.set_trigger_slope(Slope::Either);
    assert_eq!(settings.trigger_qualifier_label(), "runt < 2.8 V", "Either names the bound on the second level's side");
    settings.set_runt(Some(-0.9));
    assert_eq!(settings.trigger_qualifier_label(), "runt > \u{2212}900 mV", "Either also takes a negative excursion");
    settings.set_trigger_slope(Slope::Falling);
    assert_eq!(settings.trigger_qualifier_label(), "runt > \u{2212}900 mV", "falling tag names the lower bound");
    assert!(!settings.set_runt(None).is_empty(), "clearing removes the qualifier");
    assert_eq!(settings.trigger.qualify, Qualifier::None, "Delete and qualifier press can clear the runt");
    assert_eq!(value_unit(&settings, item), None, "a removed runt has no settable level");
}

#[test]
fn answer_lookup_dismiss_removes_plain_answers_and_refuses_expected_ones() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut lane = cx.with_vm(|vm| {
        makepad_platform::script::script_mod(vm);
        makepad_draw::script_mod(vm);
        makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
        crate::live::script_mod(vm);
        crate::trace::script_mod(vm);
        crate::instruments::script_mod(vm);
        let value = vm.eval(script! {
            use mod.prelude.widgets_internal.*
            use mod.widgets.*
            SignalLane {}
        });
        SignalLane::script_from_value(vm, value)
    });
    let rows: Vec<_> = [("freq", 10_000.0, "Hz"), ("duty", 50.0, "%"), ("Vpp", 3.3, "V"), ("rise", 2e-9, "s")]
        .into_iter().map(|(name, value, unit)| Measurement::exact(0, name, value, unit)).collect();
    lane.set_lane_measurements(&mut cx, &rows, 0);
    lane.set_expects(&mut cx, &[LaneExpect { name: "edge".into(), net: "CLK".into(), answer: "rise".into(), min: None, max: Some(3e-9) }]);
    lane.refresh_answers(&mut cx);
    let lookup = |lane: &SignalLane, cx: &Cx, name: &str| {
        let query = Query::parse(&format!("name={name}")).unwrap();
        let mut nodes = Vec::new();
        lane.lookup(cx, &Lookup::new(&SemanticLookup::Terms(&query), None).unwrap(), &mut nodes);
        assert_eq!(nodes.len(), 1, "{name} resolves to exactly one answer node");
        nodes.remove(0)
    };
    let plain = lookup(&lane, &cx, "duty");
    let expected = lookup(&lane, &cx, "rise");
    assert!(plain.2.actions.contains(A::DISMISS), "a plain answer advertises DISMISS");
    assert!(!expected.2.actions.contains(A::DISMISS), "an expected answer does not advertise DISMISS");
    let request = ActionRequest { req: 1, node: NodeId::part(7, 1), action: A::DISMISS, arg: ActionArg::None };
    let before = (lane.settings().clone(), lane.source_answers(), lane.expects.clone());
    let actions = cx.capture_actions(|cx| {
        assert_eq!(lane.lookup_action(cx, expected.0, &request), ActionResult::Unsupported, "DISMISS sent to an expected answer uses the existing refusal");
    });
    assert_eq!((lane.settings().clone(), lane.source_answers(), lane.expects.clone()), before, "a refused DISMISS changes no settings, answers or expectations");
    assert_eq!(actions.filter_widget_actions_cast::<ScopeAction>(lane.uid).count(), 0, "a refused DISMISS emits no lane actions");
    let actions = cx.capture_actions(|cx| {
        assert_eq!(lane.lookup_action(cx, plain.0, &request), ActionResult::Handled, "a plain answer handles DISMISS");
    });
    assert_eq!(lane.settings().answers, Some(vec!["freq".into(), "Vpp".into(), "rise".into()]), "DISMISS removes duty using the automatic order");
    assert_eq!(lane.source_answers().items, vec![before.1.items[0].clone(), before.1.items[2].clone(), before.1.items[3].clone()], "the dismissed answer leaves the lane while the other values and expectation marks stay intact");
    assert_eq!(actions.filter_widget_actions_cast::<ScopeAction>(lane.uid).collect::<Vec<_>>(), vec![ScopeAction::Reconfigure(lane.settings().clone())], "DISMISS publishes the shared removal's settings action");
    assert!(!lookup(&lane, &cx, "rise").2.actions.contains(A::DISMISS), "the expected node keeps its refusal after the preceding answer is removed");
}

#[test]
fn scope_lookup_has_stable_pressable_parts_and_exact_typed_values() {
    let mut settings = ScopeSettings {
        channels: vec![Channel { name: "PA0".into(), ..Default::default() }],
        trigger: Trigger { level: 1.65, qualify: Qualifier::Width { min: 0.0, max: 12e-6 }, ..Default::default() }, ..Default::default()
    };
    let answers = Answers {
        text: "10.00 kHz 51.3 %".into(),
        items: vec![Answer { name: String::new(), value: "10.00".into(), unit: "kHz".into(), dim: Vec::new(), marks: vec![(true, "ok (9 kHz to 11 kHz)".into())] }],
    };
    let parts = [
        PartRect { part: Part::Tag, rect: Rect { pos: dvec2(0.0, 0.0), size: dvec2(60.0, 22.0) }, selected: false },
        PartRect { part: Part::Answer(0), rect: Rect { pos: dvec2(60.0, 0.0), size: dvec2(100.0, 22.0) }, selected: false },
        PartRect { part: Part::LevelMarker, rect: Rect { pos: dvec2(220.0, 60.0), size: dvec2(20.0, 20.0) }, selected: false },
        PartRect { part: Part::Scale(0), rect: Rect { pos: dvec2(0.0, 40.0), size: dvec2(40.0, 100.0) }, selected: false },
        PartRect { part: Part::Time, rect: Rect { pos: dvec2(40.0, 160.0), size: dvec2(180.0, 24.0) }, selected: false },
        PartRect { part: Part::Qualifier, rect: Rect { pos: dvec2(150.0, 22.0), size: dvec2(90.0, 18.0) }, selected: true },
        PartRect { part: Part::AddAnswer, rect: Rect { pos: dvec2(160.0, 0.0), size: dvec2(22.0, 22.0) }, selected: false },
    ];
    let items = Items {
        net: "pa0-net", settings: &settings, answers: &answers, names: &[Some("freq")], parts: &parts,
        widths: &[], indices: &[], rows: &[], rows_origin: dvec2(0.0, 0.0),
        sources: &[], primary: 0, more: 0, trigger: true, shared: true, collapsed: false, closable: true,
        bracket_visible: true, bracket: Some((-10e-6, 10e-6)), values: Some((-1.0, 1.0)), delays: &[],
        plot: Rect { pos: dvec2(40.0, 40.0), size: dvec2(180.0, 100.0) }, shift: dvec2(0.0, 0.0),
        clip: Some(Rect { pos: dvec2(0.0, 0.0), size: dvec2(250.0, 150.0) }),
        time: Some(parts[4].rect), holdoff: Some(Rect { pos: dvec2(126.0, 160.0), size: dvec2(11.0, 24.0) }), focused: None, lane: None,
    };
    let lookup = |lookup: &SemanticLookup, id| {
        let mut out = Vec::new();
        if let Some(lookup) = Lookup::new(lookup, id) { items.lookup(&lookup, &mut out); }
        out
    };
    let query = |text| Query::parse(text).expect("fixture query parses");
    let add = lookup(&SemanticLookup::Terms(&query("name=\"add measurement\"")), None);
    assert_eq!(add.len(), 1, "the measure palette has one stable Scope lookup node");
    assert!(add[0].2.actions.contains(A::PRESS) && !add[0].2.bounds.is_empty(), "add measurement is a drawn, pressable control");
    let trigger = lookup(&SemanticLookup::Terms(&query("name=trigger")), None);
    assert_eq!(trigger.len(), 1, "the shared trigger is one lookup answer");
    assert_eq!(trigger[0].2.value, "PA0 rising 1.65 V", "trigger lookup includes channel, slope and level");
    assert!(trigger[0].2.actions.contains(A::SET_VALUE), "the trigger offers typed values");
    let qualifier = lookup(&SemanticLookup::Terms(&query("name=\"trigger qualifier\"")), None);
    assert_eq!(qualifier.len(), 1);
    assert_eq!(qualifier[0].2.value, "w < 12 µs", "qualifier lookup uses the tag's answer precision");
    assert!(qualifier[0].2.actions.contains(A::PRESS));
    assert!(!qualifier[0].2.actions.contains(A::SET_VALUE), "the qualifier clears on press");
    let freq = lookup(&SemanticLookup::Terms(&query("text~=freq")), None);
    assert_eq!(freq.len(), 1, "measurement identity finds a frequency with no printed prefix");
    assert_eq!(freq[0].2.value, "10.00 kHz ok (9 kHz to 11 kHz)", "lookup keeps the displayed value and expectation mark");
    assert_eq!(lookup(&SemanticLookup::Terms(&query("name=PA0")), None).len(), 1, "the lane is named by its tag");
    let all = lookup(&SemanticLookup::Terms(&query("name~=\"\"")), None);
    assert_eq!(all, lookup(&SemanticLookup::Terms(&query("name~=\"\"")), None), "repeated queries preserve order, keys and geometry");
    for (key, _, node) in &all {
        assert_eq!(lookup(&SemanticLookup::Key(*key), None), vec![(*key, None, node.clone())], "a key resolves exactly its own part");
        assert!(node.actions.contains(A::SCROLL_INTO_VIEW), "every lookup answer can be revealed");
        if node.bounds.is_empty() {
            assert!(node.states.contains(States::OFFSCREEN), "missing press bounds explicitly mean offscreen");
        } else {
            let item = items.find(*key).expect("lookup key belongs to fixture");
            let (x, y) = node.bounds.center();
            let hit = items.hit(dvec2(f64::from(x), f64::from(y)));
            assert!(hit == Some(item) || matches!((item, hit), (Item::TimeBracket, Some(Item::TimeEnd(_))) | (Item::ValueBracket, Some(Item::ValueEnd(_)))), "{}: the serialized centre presses its own handle", node.name);
        }
    }
    let wire = NodeId::part(7, 1);
    let by_id_text = format!("id={} name=trigger", wire.short());
    let by_id = query(&by_id_text);
    assert_eq!(lookup(&SemanticLookup::Terms(&by_id), Some(trigger[0].0)), trigger, "wire ids resolve through the owner-checked stable key");
    assert!(lookup(&SemanticLookup::Terms(&by_id), None).is_empty(), "a foreign or stale id cannot scan lanes");
    assert!(lookup(&SemanticLookup::Terms(&query("role=button")), None).is_empty(), "structural queries do not materialise parts");
    assert_eq!(wrapped_rect(&[30.0, 40.0, 50.0], &[0, 1, 2], &[1..3], dvec2(8.0, 160.0), 2), Some(Rect { pos: dvec2(64.0, 160.0), size: dvec2(50.0, 22.0) }), "wrapped hit geometry uses the retained widths and row gaps");
    assert_eq!(wrapped_rect(&[30.0, 50.0], &[0, 2], &[0..2], dvec2(8.0, 160.0), 2), Some(Rect { pos: dvec2(54.0, 160.0), size: dvec2(50.0, 22.0) }), "a removal preview closes the gap while retaining the following answer's identity");
    assert_eq!(wrapped_rect(&[30.0, 50.0], &[0, 2], &[0..2], dvec2(8.0, 160.0), 1), None, "the omitted answer has no wrapped hit target during preview");
    let clipped = Items { clip: None, time: None, holdoff: None, ..items };
    let mut offscreen = Vec::new();
    clipped.lookup(&Lookup::new(&SemanticLookup::Key(trigger[0].0), None).unwrap(), &mut offscreen);
    assert_eq!(offscreen.len(), 1, "an offscreen trigger keeps its identity");
    assert!(offscreen[0].2.bounds.is_empty() && offscreen[0].2.states.contains(States::OFFSCREEN), "offscreen items never advertise stale bounds");

    let request = |text: &str| ActionRequest { req: 1, node: wire, action: A::SET_VALUE, arg: ActionArg::Text(text.into()) };
    settings.trigger.level = 0.3;
    assert!(set_value(&mut settings, Item::Control(Part::LevelMarker), &request("1.65 V")).is_ok(), "a unit-bearing trigger request is handled");
    assert_eq!(settings.trigger.level, 1.65, "typed trigger level is exact");
    let before = settings.clone();
    assert_eq!(set_value(&mut settings, Item::Control(Part::LevelMarker), &request("not volts")), Err(ActionResult::Unsupported), "an unparsable value returns an action error");
    assert_eq!(settings, before, "a rejected request changes no settings");
    assert!(set_value(&mut settings, Item::Control(Part::Time), &request("50 us/div")).is_ok(), "ASCII microseconds are accepted");
    assert_eq!(settings.time_per_div, 50e-6, "typed time divisions are exact");
    assert!(set_value(&mut settings, Item::Control(Part::Scale(0)), &request("250 mV/div")).is_ok(), "a continuous lane scale is accepted");
    assert_eq!(settings.channels[0].volts_per_div, 0.25, "typed scales are not snapped to the keyboard menu");
    assert_eq!(semantics::pair(&ActionArg::Text("−20 µs .. 50 us".into()), "s"), Some((-20e-6, 50e-6)), "bracket endpoints use the displayed SI prefixes and exact decimals");
    assert_eq!(bracket_value(Item::TimeBracket, (-1.0, 1.0), &request("−20 µs .. 50 us").arg, "s"), Some((-20e-6, 50e-6)), "a typed time bracket sets both endpoints exactly");
    assert_eq!(bracket_value(Item::ValueEnd(End::Start), (0.0, 3.3), &request("402 mV").arg, "V"), Some((0.402, 3.3)), "a typed value endpoint preserves the other end");
    assert_eq!(bracket_value(Item::ValueBracket, (0.0, 3.3), &request("402 mV .. bad").arg, "V"), None, "both bracket ends validate before either changes");
    assert_eq!(bracket_value(Item::TimeEnd(End::Start), (-20e-6, 50e-6), &request("60 us").arg, "s"), None, "reversed time endpoints return an error before mutation");
    for text in ["NaN V", "inf V", "1 Hz", "1e999 V"] {
        let before = settings.clone();
        assert_eq!(set_value(&mut settings, Item::Control(Part::LevelMarker), &request(text)), Err(ActionResult::Unsupported), "{text}: nonfinite or wrong units are rejected");
        assert_eq!(settings, before, "{text}: rejection is atomic");
    }

    settings.domain = Domain::Fft;
    let time_channel = settings.channels[0].clone();
    assert!(set_value(&mut settings, Item::Control(Part::SpectrumScale), &request("2 dB/div")).is_ok(), "the cell owns its dB scale");
    assert!(set_value(&mut settings, Item::Control(Part::SpectrumOffset), &request("-3 dBV")).is_ok(), "the cell owns its reference");
    assert_eq!((settings.fft.db_per_div, settings.fft.ref_dbv), (2.0, -3.0), "typed spectrum axes retain exact values");
    assert_eq!(settings.channels[0], time_channel, "spectrum gestures cannot edit the time value axis");
    assert!(set_value(&mut settings, Item::Control(Part::CellDivider), &request("46 %")).is_ok(), "the cell divider is available on demand");
    assert_eq!(settings.fft.cell_share, 0.46, "typed cell width uses its own share");
    assert!(set_value(&mut settings, Item::Control(Part::Divider), &request("60 %")).is_ok(), "the waterfall divider stays separate");
    assert_eq!((settings.fft.cell_share, settings.fft.waterfall_share), (0.46, 0.6), "the two dividers retain independent values");

    let mut summary = Summary::default();
    summary.lanes([("pa0-net", "PA0"), ("d1-net", "D1.2"), ("power", "3V3")].into_iter());
    summary.answer("pa0-net", &answers.text);
    summary.answer("d1-net", "402 mV pp");
    summary.answer("power", "3.299 V");
    summary.trigger(Some("pa0-net"), settings.trigger, "V");
    assert_eq!(summary.state(TriggerState::Fired), Some("firing"), "a changed trigger state produces an announcement");
    assert_eq!(summary.state(TriggerState::Fired), None, "an unchanged trigger state produces no announcement");
    assert_eq!(summary.text, "3 lanes; trigger PA0 rising 1.65 V, firing; PA0 10.00 kHz 51.3 %; D1.2 402 mV pp; 3V3 3.299 V", "the three-lane summary uses the cached displayed strings");
    summary.lanes([("power", "3V3"), ("pa0-net", "PA0"), ("d1-net", "D1.2")].into_iter());
    assert!(summary.text.ends_with("3V3 3.299 V; PA0 10.00 kHz 51.3 %; D1.2 402 mV pp"), "reordering retains answers by lane identity");
}

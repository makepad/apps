//! Source ordering and comparisons are independent of acquisition and the GPU.
use fab_elec_bench::source::Side;
use fab_elec_view::instruments::scope::{lane_answers, twin_answers, Domain, LaneSource, Measurement, ScopeAction, ScopeSettings, SignalLane};
use makepad_widgets_core::*;

fn rows(frequency: f64, duty: f64, vpp: f64, rise: f64) -> Vec<Measurement> {
    [("freq", frequency, "Hz"), ("duty", duty, "%"), ("Vpp", vpp, "V"), ("rise", rise, "s")]
        .into_iter().map(|(name, value, unit)| Measurement::exact(0, name, value, unit)).collect()
}

#[test]
fn twin_differences_order_and_single_source_answers() {
    let real = rows(10_020.0, 51.0, 3.12, 32e-9);
    let sim = rows(10_000.0, 50.0, 3.0, 20e-9);
    let answers = twin_answers(&real, &sim, Side::Real, Side::Sim);
    assert_eq!(answers.items.len(), 4, "a twin selects the four common PWM measurements");
    assert_eq!(answers.items[0].value, "10.02 kHz real   10.00 sim   +0.2 %", "frequency compares against the secondary with shared units and signed percent");
    assert!(answers.items[1].value.ends_with("+2 %"), "duty difference is relative percent");
    assert!(answers.items[2].value.ends_with("+120 mV"), "level difference keeps voltage units and a positive sign");
    assert!(answers.items[3].value.ends_with("+12 ns"), "rise difference keeps time units and a positive sign");
    let reverse = twin_answers(&sim, &real, Side::Sim, Side::Real);
    assert!(reverse.items[0].value.starts_with("10.00 kHz sim   10.02 real"), "reversing the primary reverses the side and value order");
    assert!(reverse.items[0].value.ends_with("−0.2 %"), "reversed ratio uses the new secondary as reference, no finer than the values shown");
    assert!(reverse.items[2].value.ends_with("−120 mV"), "reversed level difference is signed in volts");
    assert!(reverse.items[3].value.ends_with("−12 ns"), "reversed time difference is signed in seconds");
    let near = twin_answers(&rows(10_000.0, 30.00003, 3.3001, 20e-9), &rows(10_000.0, 30.0, 3.3, 20e-9), Side::Real, Side::Sim);
    assert!(near.items[0].value.ends_with("±0 %"), "an equal frequency reads as no difference");
    assert!(near.items[1].value.ends_with("±0 %"), "a duty difference below the shown digits reads as none, not 0.0001 %");
    assert!(near.items[2].value.ends_with("±0 V"), "a level difference below the shown millivolt reads as none");
    let partial = twin_answers(&real, &sim[..1], Side::Real, Side::Replay);
    assert_eq!(partial.items.len(), 1, "only measurements present on both sides are compared");
    assert!(partial.text.contains("replay"), "replay is named explicitly in the answer");
    let zero = rows(0.0, 0.0, 3.0, 0.0);
    assert!(twin_answers(&real, &zero, Side::Real, Side::Sim).items[0].value.ends_with('—'), "zero ratio reference has no fabricated percent difference");

    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut lane = cx.with_vm(|vm| {
        vm.bx.captured_errors = Some(Vec::new());
        makepad_platform::script::script_mod(vm);
        makepad_draw::script_mod(vm);
        makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::trace::script_mod(vm);
        fab_elec_view::instruments::script_mod(vm);
        let value = vm.eval(script! {
            use mod.prelude.widgets_internal.*
            use mod.widgets.*
            SignalLane {}
        });
        let lane = SignalLane::script_from_value(vm, value);
        let errors = vm.take_errors();
        vm.bx.captured_errors = None;
        assert!(errors.is_empty(), "registering the modules and building a lane logs no script error: {errors:?}");
        lane
    });
    lane.set_sources(&[LaneSource { channel: None, side: Side::Real, name: "CH1".into() }, LaneSource { channel: None, side: Side::Sim, name: "sim".into() }]);
    lane.set_source_measurements(&mut cx, 0, &real);
    lane.set_source_measurements(&mut cx, 1, &sim);
    assert_eq!(lane.source_answers(), answers, "installed primary supplies the first answers");
    assert_eq!(lane.swap_sources(&mut cx), ScopeAction::SwapSources { primary: 1 }, "swap reports the stable source index for host persistence");
    assert_eq!(lane.sources()[lane.primary_source()].side, Side::Sim, "swap makes the secondary side primary");
    assert_eq!(lane.source_answers(), reverse, "swap reverses the retained measurements without reacquisition");
    lane.set_source_measurements(&mut cx, 0, &rows(10_030.0, 51.0, 3.12, 32e-9));
    assert!(lane.source_answers().items[0].value.contains("10.03 real"), "an update follows its source identity after swapping");
    let replacement = rows(10_030.0, 51.0, 3.12, 32e-9);
    lane.set_sources(&[LaneSource { channel: None, side: Side::Real, name: "CH1".into() }]);
    assert_eq!(lane.source_answers(), lane_answers(&replacement, &[], 0, Domain::Time, false, false, false), "removing the primary promotes the surviving source's retained measurements");
    lane.set_sources(&[LaneSource { channel: None, side: Side::Sim, name: "sim".into() }]);
    assert!(lane.source_answers().items.is_empty(), "an unrelated replacement cannot inherit measurements from the old side");
    lane.set_source_measurements(&mut cx, 0, &sim);
    assert_eq!(lane.source_answers(), lane_answers(&sim, &[], 0, Domain::Time, false, false, false), "a single source retains the original answers unchanged");
    assert_eq!(lane.swap_sources(&mut cx), ScopeAction::None, "one source cannot swap with an absent twin");
    lane.freeze_ghost(&mut cx);
    lane.set_source_measurements(&mut cx, 0, &real);
    let frozen = lane_answers(&real, &sim, 0, Domain::Time, false, false, false);
    assert_eq!(lane.source_answers(), frozen, "freezing retains measurement rows independently of later publications");
    lane.set_ghost_on_top(&mut cx, true);
    assert_eq!(lane.source_answers(), frozen, "ghost draw order leaves the answer comparison unchanged");
    lane.clear_ghost(&mut cx);
    assert_eq!(lane.source_answers(), lane_answers(&real, &[], 0, Domain::Time, false, false, false), "clearing the picture also clears its answer comparison");
    lane.freeze_ghost(&mut cx);
    let time = lane.settings().clone();
    lane.set_settings(&mut cx, ScopeSettings { domain: Domain::Fft, ..time.clone() });
    lane.set_settings(&mut cx, time);
    lane.set_source_measurements(&mut cx, 0, &sim);
    assert_eq!(lane.source_answers(), lane_answers(&sim, &[], 0, Domain::Time, false, false, false), "a domain change drops ghost rows before returning to time");

    lane.set_sources(&[LaneSource { side: Side::Sim, name: "host".into(), channel: Some(0) }, LaneSource { side: Side::Sim, name: "guest".into(), channel: Some(1) }]);
    lane.set_source_measurements(&mut cx, 0, &sim);
    lane.set_source_measurements(&mut cx, 1, &real);
    lane.set_overlay_preview(&mut cx, true);
    assert_eq!(lane.sources().iter().map(|source| source.channel).collect::<Vec<_>>(), [Some(0), Some(1)], "overlays retain both channel hues in installation order");
    assert_eq!(lane.source_answers(), lane_answers(&sim, &[], 0, Domain::Time, false, false, false), "overlay previews show the host's own answers without deltas");
    lane.set_overlay_preview(&mut cx, false);
    lane.swap_sources(&mut cx);
    assert_eq!(lane.sources()[lane.primary_source()].name, "guest", "swapping makes the guest the primary chip");
    assert_eq!(lane.source_answers(), lane_answers(&real, &[], 0, Domain::Time, false, false, false), "an overlaid primary shows its own answers without side comparisons");
    lane.set_collapsed(&mut cx, true);
    assert_eq!(lane.source_answers(), lane_answers(&real, &[], 0, Domain::Time, true, false, false), "a collapsed pair meters the primary");
    lane.set_collapsed(&mut cx, false);
    lane.set_sources(&[LaneSource { side: Side::Sim, name: "sim".into(), channel: None }]);
    assert_eq!(lane.sources().len(), 1, "separation reinstalls the plain sim source");
    assert_eq!(lane.source_answers(), lane_answers(&sim, &[], 0, Domain::Time, false, false, false), "separating after a swap restores the host's retained answers");
}

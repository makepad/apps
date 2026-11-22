use fab_elec_sim::cosim::CoState;
use fab_elec_simhost::rows::{CoPinReader, Emitter, LiveTextures, RowPlan, RowValues};
use fab_elec_view::live::{
    current_level, emission_attr, glow_attr, net_row, pin_row, LiveLook, PinLiveMode, LIVE_REBASE_S, NET_OFF, PIN_OFF,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn rows_follow_the_texel_contract() {
    let look = LiveLook::dark();
    let epoch = 10.0;
    let mut textures = LiveTextures::new(epoch, look);
    assert_eq!(textures.look(), &look, "the host retains its initial look");
    assert_eq!(textures.live_time(epoch), 0.0, "the clock starts at the supplied epoch");
    assert_eq!(
        (textures.nets().nets(), textures.segs().nets(), textures.parts().nets()),
        (0, 0, 0),
        "new textures have no allocated indices",
    );
    textures.resize(3, 3);
    for index in 0..3 {
        assert_eq!(textures.nets().row(index), NET_OFF, "new net {index} starts off");
        assert_eq!(textures.parts().row(index), [0.0; 4], "new part {index} starts at zero");
    }

    let plan = RowPlan {
        p_rated: vec![0.063, 0.1],
        pin_net: vec![Some(0), Some(0), Some(1), Some(2), None],
        emitters: vec![Emitter { part: 1, pin: 2, i_rated: 0.02 }],
        co_pins: HashMap::new(),
    };
    let net_v = [1.0, 4.0, f32::NAN];
    let pin_i = [0.012, -0.008, 0.01, -0.006, 4.0];
    let part_p = [0.005, 0.03, -0.02];
    let values = RowValues { net_v: &net_v, pin_i: &pin_i, led_i: &pin_i, part_p: &part_p };
    // Half of the absolute pin currents: pins 0 + 1, pin 2, pin 3; pin 4 has no net.
    let through = [0.5 * (0.012_f32 + 0.008), 0.005, 0.003];
    let mut lights = textures.lights();
    textures.write_values(&plan, &values);
    assert_ne!(textures.lights(), lights, "lights: an LED lighting moves the counter");
    lights = textures.lights();
    textures.write_values(&plan, &RowValues { net_v: &[2.0, 3.0, 4.0], part_p: &[0.1, 0.2, 0.3], ..values });
    textures.write_values(&plan, &values);
    assert_eq!(textures.lights(), lights, "lights: new values with every LED's light the same hold the counter");
    for net in 0..3 {
        let voltage = if net == 2 { None } else { Some(net_v[net]) };
        assert_eq!(
            textures.nets().row(net), net_row(voltage, through[net], 0.0, &look),
            "net {net} packs voltage and half the sum of its absolute pin currents",
        );
    }
    assert_eq!(textures.nets().row(2)[0], -1.0, "a NaN net voltage is marked absent");
    for (part, &power) in part_p.iter().enumerate() {
        let row = textures.parts().row(part);
        assert_eq!(row[0], glow_attr(power, [0.063, 0.1, 0.5][part]), "part {part} uses the shared glow mapping");
        assert_eq!(row[2], power, "part {part} retains signed raw dissipation");
        let emission = if part == 1 { emission_attr(pin_i[2], 0.02) } else { 0.0 };
        assert_eq!(row[1], emission, "only the LED part emits from its anode's forward current");
    }
    // at rest the host hands the sample at t as the LEDs' current: the light reads it, the nets
    // keep the means
    let at_rest = [0.0, 0.0, 0.004, 0.0, 0.0];
    textures.write_values(&plan, &RowValues { led_i: &at_rest, ..values });
    assert_ne!(textures.lights(), lights, "lights: a changed LED level moves the counter");
    assert_eq!(textures.parts().row(1)[1], emission_attr(0.004, 0.02), "an LED's light reads its own current slice");
    assert_eq!(textures.emission(), emission_attr(0.004, 0.02), "the brightest light follows the latest write, down as well as up");
    assert_eq!(textures.nets().row(1), net_row(Some(net_v[1]), through[1], 0.0, &look), "the nets still read the pin currents");
    textures.write_values(&plan, &values);

    // An LED that is off leaks a solver's noise, new in each step (1e-14 to 1e-12 of its rating):
    // it reads dark and holds the counter, so nothing draws it again. A change too small to show
    // holds the counter too; changes that add up to one that shows move it.
    let led = |i: f32| [0.0, 0.0, i, 0.0, 0.0];
    textures.write_values(&plan, &RowValues { led_i: &led(0.0), ..values });
    lights = textures.lights();
    for leak in [5.6e-15, 2.1e-14, 0.0, 6e-16] {
        textures.write_values(&plan, &RowValues { led_i: &led(leak), ..values });
        assert_eq!(textures.parts().row(1)[1], 0.0, "an LED leaking {leak} A reads dark");
    }
    assert_eq!((textures.lights(), textures.emission()), (lights, 0.0), "lights: a dark LED's leakage holds the counter and lights nothing");
    textures.write_values(&plan, &values);
    lights = textures.lights();
    for step in 1..=4 {
        textures.write_values(&plan, &RowValues { led_i: &led(0.01 + step as f32 * 1e-6), ..values });
    }
    assert_eq!(textures.lights(), lights, "lights: changes of a light no display shows hold the counter");
    textures.write_values(&plan, &RowValues { led_i: &led(0.01 + 1.2e-5), ..values });
    assert_ne!(textures.lights(), lights, "lights: small changes that add up to one that shows move the counter");
    textures.write_values(&plan, &values);

    lights = textures.lights();
    textures.emphasis(&[0, 2], &[0, 1]);
    textures.emphasis(&[1], &[2]);
    assert_eq!(textures.lights(), lights, "lights: emphasis changes no light");
    for index in 0..3 {
        assert_eq!(textures.nets().row(index)[3], if index == 1 { 1.0 } else { 0.0 }, "net emphasis replaces the previous selection immediately");
        assert_eq!(textures.parts().row(index)[3], if index == 2 { 1.0 } else { 0.0 }, "part emphasis replaces the previous selection immediately");
    }
    textures.write_values(&plan, &values);
    for index in 0..3 {
        assert_eq!(textures.nets().row(index)[3], if index == 1 { 1.0 } else { 0.0 }, "values writes preserve net emphasis");
        assert_eq!(textures.parts().row(index)[3], if index == 2 { 1.0 } else { 0.0 }, "values writes preserve part emphasis");
    }
    let retained_net = textures.nets().row(1);
    let retained_part = textures.parts().row(1);
    let sparse_plan = RowPlan {
        p_rated: Vec::new(),
        pin_net: vec![Some(u32::MAX), Some(0), Some(1), Some(2), None, Some(0)],
        emitters: vec![
            Emitter { part: u32::MAX, pin: 0, i_rated: 0.02 },
            Emitter { part: 0, pin: u32::MAX, i_rated: 0.02 },
            Emitter { part: 1, pin: 0, i_rated: 0.02 },
        ],
        co_pins: HashMap::new(),
    };
    textures.write_values(&sparse_plan, &RowValues { net_v: &[2.0], pin_i: &pin_i, led_i: &pin_i, part_p: &[0.1] });
    assert_eq!(textures.nets().row(0), net_row(Some(2.0), 0.004, 0.0, &look), "invalid pin nets and missing pin currents are skipped");
    assert_eq!(textures.parts().row(0)[0], glow_attr(0.1, 0.5), "a missing rating uses the half-watt default");
    assert_eq!(textures.parts().row(0)[1], 0.0, "an emitter with a missing pin current stays dark");
    assert_eq!(textures.nets().row(1), retained_net, "a short voltage slice leaves later net rows unchanged");
    assert_eq!(textures.parts().row(1), retained_part, "a short power slice leaves later parts, including emitters, unchanged");

    textures.resize_segs(4);
    for row in 0..4 {
        assert_eq!(textures.segs().row(row), [0.0; 4], "new segment {row} starts at zero");
    }
    textures.write_segs(10.0, &[1, u32::MAX, 2], &[0.01, 0.03]);
    assert_eq!(textures.segs().row(2), [0.0; 4], "a segment without a matching current is untouched");
    let time = f64::from(textures.live_time(10.5));
    let before = textures.segs().row(1);
    let u_before = f64::from(before[0]) + f64::from(before[1]) * time;
    textures.write_segs(10.5, &[1], &[0.02, 0.04]);
    let after = textures.segs().row(1);
    let u_after = f64::from(after[0]) + f64::from(after[1]) * time;
    let jump = u_after - u_before;
    assert!((jump - jump.round()).abs() < 1e-4, "changing current preserves pattern position modulo one cycle: {u_before} -> {u_after}");
    assert!(after[1] > before[1], "20 mA flows faster than 10 mA");
    assert_eq!(before[2], current_level(0.01, &look), "the first segment level represents 10 mA");
    assert_eq!(after[2], current_level(0.02, &look), "the changed segment level represents 20 mA");
    assert_eq!(after[3], 0.0, "the segment's reserved channel stays zero");

    let next_look = LiveLook { i_ref: 0.1, ..look };
    textures.set_look(next_look);
    assert_eq!(textures.look(), &next_look, "a new look is retained for later writes");
    assert_eq!(textures.segs().row(1), after, "changing the look waits for a values write");
    let now = epoch + LIVE_REBASE_S + 1.0;
    let old_u = f64::from(after[0]) + f64::from(after[1]) * (now - epoch);
    let rebased_time = textures.live_time(now);
    let rebased = textures.segs().row(1);
    let new_u = f64::from(rebased[0]) + f64::from(rebased[1]) * f64::from(rebased_time);
    let jump = new_u - old_u;
    assert!(rebased_time.abs() < 1e-6, "rebasing resets shader time to approximately zero");
    assert!((jump - jump.round()).abs() < 1e-3, "rebasing preserves pattern position modulo one cycle: {old_u} -> {new_u}");
    assert_eq!([rebased[1], rebased[2]], [after[1], after[2]], "rebasing preserves speed and level even with a pending look change");

    textures.clear_segs(&[1, u32::MAX]);
    assert_eq!(textures.segs().row(1), [0.0; 4], "freeing a segment zeros every channel");
    let later = now + LIVE_REBASE_S + 1.0;
    textures.live_time(later);
    assert_eq!(textures.segs().row(1), [0.0; 4], "a freed segment stays zero across later rebases");
    textures.resize_segs(2);
    assert_eq!(textures.segs().nets(), 4, "segment storage never shrinks or relocates rows");
    textures.write_segs(later, &[1], &[0.02]);
    let reused = textures.segs().row(1);
    assert_eq!(reused[0], 0.0, "a reused segment starts with a reset flow phase at the epoch");
    assert_eq!(reused[2], current_level(0.02, &next_look), "the next segment write uses the new look");

    textures.emphasis(&[1, u32::MAX], &[2, u32::MAX]);
    lights = textures.lights();
    textures.clear();
    assert_ne!(textures.lights(), lights, "lights: a clear that darkens a lit LED moves the counter");
    lights = textures.lights();
    textures.clear();
    assert_eq!(textures.lights(), lights, "lights: clearing dark rows holds the counter");
    for index in 0..3 {
        let mut off = NET_OFF;
        off[3] = if index == 1 { 1.0 } else { 0.0 };
        assert_eq!(textures.nets().row(index), off, "clear keeps net {index}'s emphasis in NET_OFF");
        assert_eq!(textures.parts().row(index), [0.0, 0.0, 0.0, if index == 2 { 1.0 } else { 0.0 }], "clear keeps only part {index}'s emphasis");
    }
    assert_eq!(textures.emission(), 0.0, "cleared rows hold no light");
    textures.live_time(later + LIVE_REBASE_S + 1.0);
    for row in 0..4 {
        assert_eq!(textures.segs().row(row), [0.0; 4], "clear resets segment {row} and its flow across rebases");
    }
    textures.resize(1, 1);
    assert_ne!(textures.lights(), lights, "lights: renumbered rows move the counter");
    textures.resize(3, 3);
    assert_eq!(textures.nets().row(1), NET_OFF, "a regrown net does not inherit old emphasis");
    assert_eq!(textures.parts().row(2), [0.0; 4], "a regrown part starts at zero without old emphasis");
}

/// A toy device state: its pins as a reader reports them.
struct Fw {
    pins: Vec<(&'static str, PinLiveMode, Option<bool>, f32)>,
}

fn fw(pins: Vec<(&'static str, PinLiveMode, Option<bool>, f32)>) -> CoState {
    CoState(Arc::new(Fw { pins }))
}

/// Pin rows show a device's published state: each pin the state names that the plan joins gets
/// its mode, level and duty; the same state `Arc` again reads and writes nothing; a pin that had
/// state and has none goes off with its emphasis kept; no state, or a new lowering, turns every
/// row off, and after a new lowering the same state is written again.
#[test]
fn pin_rows_show_a_device_state_once_per_arc() {
    let mut textures = LiveTextures::new(0.0, LiveLook::dark());
    textures.resize_pins(4);
    assert!((0..4).all(|i| textures.pins().row(i) == PIN_OFF), "new pin rows are off");
    let reads = Arc::new(AtomicUsize::new(0));
    let reader: CoPinReader = {
        let reads = reads.clone();
        Arc::new(move |co: &CoState, pin: &mut dyn FnMut(&str, PinLiveMode, Option<bool>, f32)| {
            reads.fetch_add(1, Ordering::Relaxed);
            for &(name, mode, level, duty) in &co.get::<Fw>().expect("the toy state").pins {
                pin(name, mode, level, duty);
            }
        })
    };
    let plan = RowPlan { co_pins: HashMap::from([("PB1".to_owned(), 1), ("PB2".to_owned(), 3)]), ..RowPlan::default() };

    let a = fw(vec![
        ("PB1", PinLiveMode::Output, Some(true), 0.0),
        ("PB2", PinLiveMode::Pwm, Some(false), 0.5),
        ("PC9", PinLiveMode::Output, Some(true), 0.0),
    ]);
    assert!(textures.write_pins(&plan, Some(&a), Some(&reader)), "a new state writes");
    assert_eq!(textures.pins().row(1), pin_row(Some(PinLiveMode::Output), Some(true), 0.0, 0.0), "PB1: an output, high");
    assert_eq!(textures.pins().row(3), [3.0, 0.0, 0.5, 0.0], "PB2: a PWM output at 50 %, low now");
    assert!([0, 2].iter().all(|&i| textures.pins().row(i) == PIN_OFF), "pins the state does not name stay off; PC9 has no row");

    assert!(!textures.write_pins(&plan, Some(&a.clone()), Some(&reader)), "the same Arc writes nothing");
    assert_eq!(reads.load(Ordering::Relaxed), 1, "and is not read again");

    textures.emphasis_pins(&[1]);
    assert_eq!(textures.pins().row(1)[3], 1.0, "emphasis brightens PB1 at once");
    let b = fw(vec![("PB2", PinLiveMode::Input, None, 0.7)]);
    assert!(textures.write_pins(&plan, Some(&b), Some(&reader)), "a new state writes");
    assert_eq!(textures.pins().row(1), pin_row(None, None, 0.0, 1.0), "PB1 had state and has none: off, its emphasis kept");
    assert_eq!(textures.pins().row(3), [0.0, -1.0, 0.0, 0.0], "PB2: an input of unknown level, no duty");

    assert!(textures.write_pins(&plan, None, Some(&reader)), "no state after one writes");
    assert_eq!(textures.pins().row(3), PIN_OFF, "no state: every row off");

    assert!(textures.write_pins(&plan, Some(&b), Some(&reader)));
    textures.resize_pins(5);
    assert!((0..5).all(|i| textures.pins().row(i) == PIN_OFF), "a new lowering turns every row off");
    assert!(textures.write_pins(&plan, Some(&b), Some(&reader)), "and the same state is written again on it");
    assert_eq!(textures.pins().row(3), [0.0, -1.0, 0.0, 0.0], "PB2 again");
}

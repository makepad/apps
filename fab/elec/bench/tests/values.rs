//! The seam: design ids onto live values and kernel commands.

use fab_elec_bench::input::Taper;
use fab_elec_bench::values::{SimCmd, Tune, ValueIndex};
use fab_elec_core::ids::{BoardId, CompId, CompRef, NetId, NetRef, PinIx, PinRef};
use fab_elec_sim::live::{LiveCmd, LiveValues};

/// Two boards numbered sparsely: lookups by NetRef / PinRef / CompRef land on the lowering's
/// indices, values of another generation read as none, and a SetParam reaches the kernel device
/// the component tunes (none for a component with no tunable value).
#[test]
fn value_index_reads_and_commands() {
    let (b0, b1) = (BoardId(0), BoardId(1));
    let mut ix = ValueIndex::new(7);
    let gnd = NetRef { board: b0, net: NetId(0) };
    let rail = NetRef { board: b1, net: NetId(5) };
    assert_eq!((ix.add_net(gnd), ix.add_net(rail), ix.add_net(gnd)), (0, 1, 0), "nets numbered once, in order");
    let r1 = CompRef { board: b1, comp: CompId(3) };
    let u1 = CompRef { board: b0, comp: CompId(9) };
    assert_eq!(ix.add_comp(r1, 2, Some(Tune::device("r1", "value"))), (0, 0), "first component and its pins");
    assert_eq!(ix.add_comp(u1, 5, None), (1, 2), "second component's pins follow");
    assert_eq!(ix.counts(), (2, 7, 2), "counts");
    let pin = |c: CompRef, p: u16| PinRef { board: c.board, comp: c.comp, pin: PinIx(p) };
    assert_eq!((ix.pin(pin(r1, 1)), ix.pin(pin(u1, 4)), ix.pin(pin(u1, 5))), (Some(1), Some(6), None), "pin lookups and range");
    assert_eq!(ix.net(NetRef { board: b1, net: NetId(4) }), None, "an unnumbered net");
    let lv = LiveValues { t: 1e-3, generation: 7, ratio: 1.0, steady: false, net_v: vec![0.0, 3.3], branch_i: (0..7).map(|i| i as f32).collect(), part_p: vec![0.5, 0.0], ..Default::default() };
    assert_eq!((ix.v(&lv, rail), ix.i(&lv, pin(u1, 2)), ix.p(&lv, r1)), (Some(3.3), Some(4.0), Some(0.5)), "reads by design id");
    let stale = LiveValues { generation: 6, steady: false, ..lv.clone() };
    assert_eq!(ix.v(&stale, rail), None, "another generation's values read as none");
    assert_eq!(ix.command(&SimCmd::SetParam { comp: r1, value: 2.2e3 }), [LiveCmd::SetParam { generation: 7, device: "r1".into(), key: "value".into(), value: 2.2e3 }], "a tunable component carries its generation");
    assert_eq!(ix.command(&SimCmd::SetParam { comp: u1, value: 1.0 }), [], "no tunable value");
    assert_eq!(ix.command(&SimCmd::Step(1e-6)), [LiveCmd::Step(1e-6)], "plain commands pass through");
    // A pot's turn sets both halves, which always add to its track: a linear 10 kohm pot a
    // quarter turned, with 0.5 % left at each end.
    let rv = CompRef { board: b0, comp: CompId(12) };
    ix.add_comp(rv, 3, Some(Tune::Pot { a: "rv#a".into(), b: "rv#b".into(), ohms: 10e3, taper: Taper::Linear, end: 0.005 }));
    ix.add_knob(rv, 0.5);
    assert_eq!(ix.knobs(), [(rv, 0.5)], "the pot is a knob, starting where the design puts it");
    let halves = ix.command(&SimCmd::SetParam { comp: rv, value: 0.25 });
    let ohms: Vec<(String, f64)> = halves.iter().map(|c| match c {
        LiveCmd::SetParam { device, key, value, .. } if key == "value" => (device.clone(), *value),
        other => panic!("a pot's turn sets resistances, got {other:?}"),
    }).collect();
    assert_eq!(ohms.iter().map(|(d, _)| d.as_str()).collect::<Vec<_>>(), ["rv#a", "rv#b"], "both halves, A-W first");
    assert!((ohms[0].1 - 10e3 * (0.005 + 0.99 * 0.25)).abs() < 1e-9 && (ohms[0].1 + ohms[1].1 - 10e3).abs() < 1e-9,
        "A-W follows the turn and the halves add to the track: {ohms:?}");
    assert!((Taper::Log.track(0.5) - 0.1).abs() < 1e-12 && Taper::Log.track(0.0) == 0.0 && Taper::Log.track(1.0) == 1.0,
        "the audio taper is a tenth of the track at half the turn, and spans it");
    let foreign = CompRef { board: BoardId(4), comp: CompId(0) };
    assert_eq!(
        ix.subscribe(&[pin(u1, 2), pin(r1, 0), pin(u1, 9)], &[u1, foreign]),
        LiveCmd::Subscribe { generation: 7, pins: vec![4, 0], parts: vec![1] },
        "a subscription by design id, ids outside the lowering skipped"
    );
}

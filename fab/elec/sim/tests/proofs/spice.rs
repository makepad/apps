//! Text decks reach the same closed-form results as circuits built through the API.

use super::linear::run_ok;
use fab_elec_sim::spice::{number, parse, Level};
use fab_elec_sim::{AcSweep, Analysis, DcSweep, TranSpec, Wave};
use std::f64::consts::PI;

#[test]
fn spice_numbers() {
    for (text, want) in [
        ("1k", 1e3), ("1K", 1e3), ("1kohm", 1e3), ("10uF", 10e-6),
        ("1e3", 1e3), ("2.2E+3", 2.2e3), (".5", 0.5), ("1meg", 1e6),
        ("1MEG", 1e6), ("1m", 1e-3), ("1mil", 25.4e-6), ("3.3v", 3.3),
        ("4.7n", 4.7e-9), ("100p", 100e-12), ("1a", 1e-18),
    ] {
        let actual = number(text).expect("numeric table entry is accepted");
        assert!((actual - want).abs() <= 1e-14 * want, "scale suffix for {text}: {actual} vs {want}");
    }
    for text in ["k", "meg", "x1", "", "''", "1e+", "NaN", "1e999"] {
        assert_eq!(number(text), None, "non-number {text:?} is rejected");
    }
}

#[test]
fn spice_deck_analyses_and_substrates() {
    let text = "  A mixed-case SPICE deck\x20\x20
* A full-line comment
V1 supply 0 DC 9
Rtop supply divider {base}
Rbottom divider 0 {twice} ; inline comment
V2 auxiliary 0 0 $ another inline comment
Va ac_in 0 AC 1
Xfirst ac_in out1 LowPass
Xsecond ac_in out2 LowPass r={twice}
VP pulse_in 0 PuLsE(0 1
+ 10u)
Rp pulse_in pulse_out 1k
Cp pulse_out 0 1u
.param base=1k twice=pow(2,1)*base corner={1/(2*3.141592653589793*base*1u)}
.MODEL diode D (IS=1e-14, N=1)
.subckt LowPass input output params: r=1k c=1u
R1 input output {r}
C1 output 0 {c}
.ends LowPass
.control
this 'unclosed text must be skipped
.endc
Abad divider 0 model
.options reltol=1e-6 unknown=7
.op
.ac dec 10 1 1meg
.ac lin 2 {corner/2} {corner}
.tran 1u 5m
.dc v1 0 5 1 v2 0 1 0.5
.end
garbage after the end
";
    let deck = parse(text);
    assert_eq!(deck.title, "A mixed-case SPICE deck", "title preserves case and trims whitespace");
    let line = |needle: &str| text.lines().position(|line| line == needle).map(|i| i + 1).expect("diagnostic fixture line exists");
    let diags: Vec<_> = deck.diags.iter().map(|d| (d.line, d.level)).collect();
    assert_eq!(diags, vec![
        (line(".control"), Level::Warning),
        (line("Abad divider 0 model"), Level::Error),
        (line(".options reltol=1e-6 unknown=7"), Level::Warning),
    ], "diagnostics have exactly the original physical lines and levels; trailing text is ignored");
    let corner = 1.0 / (2.0 * PI * 1e3 * 1e-6);
    assert_eq!(deck.analyses, vec![
        Analysis::Op,
        Analysis::Ac(AcSweep::Dec { n: 10, fstart: 1.0, fstop: 1e6 }),
        Analysis::Ac(AcSweep::Lin { n: 2, fstart: corner / 2.0, fstop: corner }),
        Analysis::Tran(TranSpec { tstop: 5e-3, max_step: None, uic: false }),
        Analysis::Dc(vec![DcSweep::lin("v2", "dc", 0.0, 1.0, 0.5), DcSweep::lin("v1", "dc", 0.0, 5.0, 1.0)]),
    ], "analysis arguments and second-source outer DC sweep");
    let pulse = &deck.circuit.devices.v.iter().find(|v| v.name == "vp").expect("continued source exists").src;
    assert_eq!(pulse.dc, None, "a waveform without explicit DC uses its initial value");
    assert!(matches!(pulse.wave, Wave::Pulse { tr, tf, pw, per, .. } if tr == 1e-6 && tf == 1e-6 && pw == 5e-3 && per == 5e-3), "first transient supplies omitted pulse arguments");
    assert!(deck.circuit.devices.r.iter().any(|r| r.name == "xfirst.r1" && r.r == 1e3), "first subcircuit has its default resistance and prefixed device name");
    assert!(deck.circuit.devices.r.iter().any(|r| r.name == "xsecond.r1" && r.r == 2e3), "second subcircuit override is isolated");
    assert_eq!(deck.circuit.models[0].params, vec![("is".into(), 1e-14), ("n".into(), 1.0)], "parenthesized model parameters and commas");
    let records: Vec<_> = deck.analyses.iter().map(|a| run_ok(&deck.circuit, a).0).collect();
    assert!((records[0].last("divider") - 6.0).abs() < 1e-9, "parsed operating-point divider voltage");
    assert!((records[0].last("v1#branch") + 3e-3).abs() < 1e-12, "parsed voltage-source branch current");
    for (node, point) in [("out1", 1), ("out2", 0)] {
        let mag = records[2].ac_v(node)[point].norm();
        assert!((mag - 1.0 / 2.0f64.sqrt()).abs() < 1e-6, "{node} low-pass magnitude at its exact corner: {mag}");
    }
    let step = records[3].at_time("pulse_out", 10e-6 + 1e-6 + 1e-3);
    assert!((step - (1.0 - (-1.0f64).exp())).abs() < 2e-3, "RC response one time constant after the pulse edge: {step}");
    assert_eq!(records[4].at.len(), 18, "both nested DC sweeps execute");
    assert_eq!(records[4].outer[6], Some(0.5), "second source advances only after the first source's sweep");

    let bjts = parse("BJT substrate forms\nQplain c b e Part 1\nQsub c b e substrate Part\n.model Part NPN (BF=100)\n.end\n");
    assert!(bjts.diags.is_empty(), "both BJT forms accept a model declared later: {:?}", bjts.diags);
    assert_eq!(bjts.circuit.devices.q.len(), 2, "both BJT forms are retained");
    assert_eq!(bjts.circuit.devices.q[0].s, 0, "model in fourth position implies grounded substrate");
    assert_eq!(Some(bjts.circuit.devices.q[1].s), bjts.circuit.find_node("substrate"), "explicit substrate is preserved");
    assert!(bjts.circuit.devices.q.iter().all(|q| q.model == "part"), "BJT model names are case insensitive");
}

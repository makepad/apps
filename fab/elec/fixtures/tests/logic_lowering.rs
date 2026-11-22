//! Logic mode's lowering rule on the stock boards (A-L1's L1): what it would take digital, and that
//! it never takes a net a firmware pin reads as analog.

use fab_elec_bench::{lower, Lowered, ModelLibs, PinFacts};
use fab_elec_fw::{compile, Built, Chip, FwInput, PinRole};
use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_mcu_cosim::{LogicMachine, NetDriver};
use fab_elec_lang::{evaluate, std_libs};
use fab_elec_mcu_emit_s3::wiring;
use fab_elec_sim::analysis::logic::{lower as logic, with_loads, LogicReport, Pads, Verdict};

fn fw_text(doc: &str) -> &'static str {
    match doc {
        "drum16.fw.splash" => include_str!("../drum16.fw.splash"),
        "ledavr.fw.splash" => include_str!("../ledavr.fw.splash"),
        "handheld.fw.splash" => include_str!("../handheld.fw.splash"),
        _ => panic!("no stock firmware {doc}"),
    }
}

/// The bench lowered, each firmware part's analog pins and pin roles from its build (and an AVR's
/// image), and the rule's report.
type Facts = Vec<(String, Vec<String>, Vec<(String, PinRole)>, Option<Vec<u16>>)>;

fn report(doc: &str, bench: &str) -> (Lowered, Facts, Pads, LogicReport) {
    let sources = fab_elec_fixtures::docs(doc).unwrap_or_else(|| panic!("{doc} is a stock project"));
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("{doc} evaluates: {d:?}"));
    let b = ev.benches.iter().find(|b| b.name == bench).unwrap_or_else(|| panic!("{doc} has bench {bench}"));
    let low = lower(&ev.design, b.scope_id(), &b.spec, &ModelLibs::generic());
    let facts: Facts = low.firmware.iter().map(|fw| {
        let chip = fw.chip.as_deref().and_then(Chip::from_model).unwrap_or_else(|| panic!("{doc}: {} is a simulated chip", fw.part));
        let wiring = (chip == Chip::S3).then(|| wiring(&fw.bus.board(0)).unwrap_or_else(|e| panic!("{doc}: the S3's wiring: {e}")));
        let input = FwInput {
            part: fw.part.clone(), doc: fw.doc.clone(), rev: 1, text: fw_text(&fw.doc).into(),
            f_clk: fw.f_clk.unwrap_or_else(|| panic!("{doc}: a clock")), params: fw.params.clone(), chip, wiring, island: None, files: Vec::new(), net_files: Vec::new(),
        };
        let built = compile(&input).unwrap_or_else(|d| panic!("{doc}: the firmware builds: {d:?}"));
        let words = match &built { Built::Avr(fw) => Some(fw.words.clone()), Built::S3(_) => None };
        (fw.part.clone(), built.analog().to_vec(), built.pins().to_vec(), words)
    }).collect();
    let pads = low.logic_pads(&pin_facts(&facts));
    let r = logic(&low.circuit, &pads, None);
    println!("{doc}/{bench}: {r}");
    (low, facts, pads, r)
}

fn pin_facts(facts: &Facts) -> Vec<PinFacts<'_>> {
    facts.iter().map(|(part, analog, pins, _)| PinFacts { part, analog, pins }).collect()
}

#[test]
fn the_logic_rule_takes_the_digital_nets_and_never_an_analog_read() {
    // per board: its matrices as (nets, keys, keys with a diode, keys to a rail), its indicator
    // branches, those of them on a write-only pad that become loads (ledctl's LEDs sink into a
    // stub's pads, which may be read), and the keys Logic mode takes to the logic side (a key to a
    // rail stays analog)
    let boards: [(&str, &str, &[(usize, usize, usize, usize)], usize, usize, usize); 5] = [
        ("drum16", "board", &[(9, 20, 20, 0)], 20, 20, 20),
        ("ledavr", "chase", &[(1, 1, 0, 1)], 8, 8, 0),
        ("handheld", "sand", &[(1, 1, 0, 1), (1, 1, 0, 1)], 0, 0, 0),
        ("ledctl", "bringup", &[(1, 1, 0, 1)], 8, 0, 0),
        ("tonebox", "box", &[], 0, 0, 0),
    ];
    for (doc, bench, matrices, indicators, loads, keys) in boards {
        let (low, analog, pads, r) = report(doc, bench);
        let got: Vec<(usize, usize, usize, usize)> = r.matrices.iter()
            .map(|m| (m.nets.len(), m.keys.len(), m.keys.iter().filter(|k| k.1.is_some()).count(), m.to_rail)).collect();
        assert_eq!(got, matrices, "{doc}: the key matrices the rule takes\n{r}");
        assert_eq!(r.indicators.len(), indicators, "{doc}: the indicator branches the rule takes\n{r}");
        // L1's falsifier: no net a firmware's analog pin sits on is digital
        for (part, pins, _, _) in &analog {
            for pin in pins {
                let Some(g) = low.circuit.devices.gpio.iter().find(|g| g.name == format!("{part}#{pin}.io")) else { continue };
                let net = &low.circuit.nodes[g.pad as usize].name;
                assert!(!r.nets.iter().any(|(n, v)| n == net && matches!(v, Verdict::Digital(_))),
                    "{doc}: {net}, which {part}'s analog pin {pin} reads, stays analog");
            }
        }
        assert!(r.unknowns.1 <= r.unknowns.0, "{doc}: the lowering keeps no more unknowns than it found");
        // the indicator loads: each lights in one state, and the circuit with them solves
        let (circuit, taken) = with_loads(&low.circuit, &pads, &r).unwrap_or_else(|e| panic!("{doc}: the loads: {e}"));
        for l in &taken {
            println!("  load {}: {} high {} i {:.3e} A g {:.3e} S v_off {:.4} V", l.pad, l.devices.join(" "), l.high, l.i, l.g, l.v_off);
        }
        assert_eq!(taken.len(), loads, "{doc}: the indicator branches taken as loads\n{r}");
        assert!(taken.iter().all(|l| l.i > 0.0 && l.g > 0.0 && l.v_off.is_finite()), "{doc}: each load draws when lit");
        let mut s = fab_elec_sim::newton::Solver::new(&circuit).unwrap_or_else(|e| panic!("{doc}: the circuit with loads: {e:?}"));
        fab_elec_sim::analysis::op::operating_point(&mut s, true).unwrap_or_else(|e| panic!("{doc}: its operating point: {e:?}"));
        println!("  with loads: {} nodes, {} unknowns (was {} nodes)", circuit.nodes.len(), s.unknowns.len(), low.circuit.nodes.len());
        // Logic mode: the matrices' keys go to the logic plan, their pins leave the co-simulation,
        // a row read on a pull rises after its release ticks, the rest solves and the plan joins
        // the chip
        let facts = pin_facts(&analog);
        let mut taken_keys = 0;
        for (fw, (part, _, _, words)) in low.firmware.iter().zip(&analog) {
            let f_clk = fw.f_clk.expect("a clock");
            let mode = low.logic_mode(part, &facts, f_clk).unwrap_or_else(|e| panic!("{doc}: Logic mode: {e}"));
            taken_keys += mode.plan.keys.len();
            assert_eq!(mode.keys.len(), mode.plan.keys.len(), "{doc}: each key is named by its press");
            assert!(mode.pins.iter().all(|p| mode.circuit.devices.gpio.iter().any(|g| g.name == p.gpio)), "{doc}: each row's pad is in the circuit");
            for key in &mode.plan.keys {
                for net in [key.a, key.b] {
                    let NetDriver::Machine(pin) = &mode.plan.nets[net as usize].driver else { continue };
                    assert!(!mode.pins.iter().any(|p| p.name == *pin), "{doc}: matrix pin {pin} leaves the co-simulation");
                    assert!(mode.plan.release.iter().any(|&(n, t)| n == net && t > 0), "{doc}: matrix row {pin} rises on its pull after a release");
                }
            }
            // each LED a load stands for reads that load's pad
            let rebound = low.leds.iter().filter(|led| mode.values.pins[led.anode as usize].iter().any(|t| mode.loads.iter().any(|l| l.pad == t.device))).count();
            assert_eq!(rebound, mode.loads.len(), "{doc}: each load's LED reads the load");
            let mut s = fab_elec_sim::newton::Solver::new(&mode.circuit).unwrap_or_else(|e| panic!("{doc}: the Logic circuit: {e:?}"));
            fab_elec_sim::analysis::op::operating_point(&mut s, true).unwrap_or_else(|e| panic!("{doc}: its operating point: {e:?}"));
            println!("  Logic mode for {part}: {} unknowns, {} keys, releases {:?}, {} cells (tau, period alone, delay a flip: {:?}), analog cells {:?}", s.unknowns.len(), mode.plan.keys.len(), mode.plan.release,
                mode.cells.len(), mode.cells.iter().map(|c| (c.tau, c.period, c.delay)).collect::<Vec<_>>(), mode.analog_cells);
            if let Some(words) = words {
                LogicMachine::new(AvrMachine::new(words, f_clk as u64), mode.plan.clone()).unwrap_or_else(|e| panic!("{doc}: the plan joins {part}: {e}"));
            }
        }
        assert_eq!(taken_keys, keys, "{doc}: the keys Logic mode takes");
    }
}

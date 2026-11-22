//! Load by block (`blocks`): a circuit cut at its rails and its sum loads as a whole load does.
//! Entries one block writes are bit-equal; the border's shared entries differ only by the order
//! the blocks' parts are summed in. An operating point through the blocks agrees with one
//! without them within Newton's own tolerance.

use fab_elec_sim::analysis::op::operating_point;
use fab_elec_sim::{ModelKind, SimCircuit, Solver, Source};

/// Two transistor voices on a rail, mixed into a sum; a small RC tail; a resistor between the
/// rail and the sum, which touches only the border.
fn board(border: &[&str]) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("npn", ModelKind::Npn, &[("is", 1e-15), ("cje", 2e-12), ("cjc", 1e-12), ("tf", 3e-10)]);
    c.model("dm", ModelKind::Diode, &[("cjo", 1e-12)]);
    c.v("vrail", "VA", "0", Source::dc(9.0));
    for v in ["k", "s"] {
        let n = |what: &str| format!("{v}.{what}");
        c.v(&format!("vin.{v}"), &n("in"), "0", Source::dc(0.65));
        c.r(&format!("rb.{v}"), &n("in"), &n("b"), 10e3);
        c.q4(&format!("q.{v}"), &n("c"), &n("b"), &n("e"), "0", "npn");
        c.r(&format!("rc.{v}"), "VA", &n("c"), 4.7e3);
        c.r(&format!("re.{v}"), &n("e"), "0", 470.0);
        c.c(&format!("cc.{v}"), &n("c"), &n("out"), 1e-6);
        c.d(&format!("d.{v}"), &n("out"), &n("clip"), "dm");
        c.r(&format!("rclip.{v}"), &n("clip"), "0", 1e3);
        c.r(&format!("rmix.{v}"), &n("out"), "SUM", 10e3);
    }
    c.r("rsum", "SUM", "0", 10e3);
    c.r("rtail", "VA", "tail", 1e3);
    c.c("ctail", "tail", "0", 1e-9);
    c.r("rbridge", "VA", "SUM", 1e6);
    c.opts.border = border.iter().map(|s| s.to_string()).collect();
    c
}

#[test]
fn block_load_matches_whole_load() {
    let mut whole = Solver::new(&board(&[])).expect("the board without a border builds");
    let mut cut = Solver::new(&board(&["VA", "SUM", "no such net"])).expect("the board cut at its rail and sum builds");
    assert!(whole.blocks.is_none(), "no border: a whole load");
    let plan = cut.blocks.as_ref().expect("the rail and the sum cut the board");
    // the two voices, the small group (the rail source's branch and the tail), the border group
    assert_eq!(plan.names.len(), 4, "blocks: {}", plan.report());
    assert_eq!(plan.border.len(), 2, "the missing name is skipped");
    assert_eq!(plan.names.last().map(String::as_str), Some("border"));
    assert!(plan.instances.iter().all(|&k| k > 0), "every block holds instances: {}", plan.report());
    assert_eq!(plan.instances.iter().sum::<u32>() as usize, cut.banks.iter().map(|b| b.len()).sum::<usize>(), "every instance is in one block");

    let border: Vec<usize> = ["VA", "SUM"].iter().map(|n| cut.unknown(n).expect("border net") as usize).collect();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    for round in 0..3 {
        for u in 1..whole.x.len() {
            let v = if whole.unknowns[u].kind == fab_elec_sim::stamp::UnkKind::Voltage { 9.0 * next() - 1.0 } else { 1e-3 * (next() - 0.5) };
            whole.x[u] = v;
            cut.x[u] = v;
        }
        let mut cx = whole.base_cx();
        cx.tran = true;
        cx.limit = round > 0;
        let (a, b) = (whole.load(&cx), cut.load(&cx));
        assert_eq!(a.0, b.0, "round {round}: the same instances flag non-convergence");
        let shared_slot = |s: usize| border.contains(&(whole.slot_row[s] as usize)) && border.contains(&(whole.slot_col[s] as usize));
        for s in 1..whole.g.len() {
            for (what, x, y) in [("g", whole.g[s], cut.g[s]), ("c", whole.c[s], cut.c[s])] {
                let (r, c) = (&whole.unknowns[whole.slot_row[s] as usize].name, &whole.unknowns[whole.slot_col[s] as usize].name);
                if shared_slot(s) {
                    assert!((x - y).abs() <= 1e-12 * x.abs().max(y.abs()), "round {round}: shared {what}[{r}, {c}] {x:e} against {y:e}");
                } else {
                    assert_eq!(x.to_bits(), y.to_bits(), "round {round}: owned {what}[{r}, {c}] {x:e} against {y:e}");
                }
            }
        }
        for u in 1..whole.f.len() {
            let name = &whole.unknowns[u].name;
            for (what, x, y) in [("f", whole.f[u], cut.f[u]), ("q", whole.q[u], cut.q[u])] {
                if border.contains(&u) {
                    assert!((x - y).abs() <= 1e-12 * x.abs().max(y.abs()).max(1e-30), "round {round}: shared {what}[{name}] {x:e} against {y:e}");
                } else {
                    assert_eq!(x.to_bits(), y.to_bits(), "round {round}: owned {what}[{name}] {x:e} against {y:e}");
                }
            }
        }
    }

    let mut whole = Solver::new(&board(&[])).unwrap();
    let mut cut = Solver::new(&board(&["VA", "SUM"])).unwrap();
    operating_point(&mut whole, false).expect("the whole board's operating point");
    operating_point(&mut cut, false).expect("the cut board's operating point");
    for u in 1..whole.x.len() {
        let (x, y) = (whole.x[u], cut.x[u]);
        let abs = if whole.unknowns[u].kind == fab_elec_sim::stamp::UnkKind::Voltage { whole.opts.vntol } else { whole.opts.abstol };
        assert!((x - y).abs() <= whole.opts.reltol * x.abs().max(y.abs()) + abs, "operating point at {}: {x:e} against {y:e}", whole.unknowns[u].name);
    }
}

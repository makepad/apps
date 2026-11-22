//! A stand-in for drum16's whole board, until the board itself exists: the analog side built from
//! drum16's design table, with the parts the rig's ideal board leaves out. It answers what the
//! real board's rails and op-amps do to a partition and to the step cost.
//!
//! - Power: USB 5 V (an ideal source) into the analog rail VA through 10 ohm and 22 uF. VREF is
//!   half of VA: 2 x 10 kohm, 10 uF, buffered by one op-amp section.
//! - Op-amps: each section is the bench's macro-model of `OpAmpCard`, as the lowering builds it
//!   (`a0` 1e5, `gbw` 1 MHz, `rout` 50 ohm, `ilim` 20 mA, `iq` 100 uA), on VA and ground. There
//!   are eleven sections: VREF, four ringers, three noise stages, the hat filter, the mixer and
//!   the output buffer.
//! - Triggers: the sequencer's pins are ideal pulse sources (in the board, drive events from the
//!   MCU). Each voice gets the diode AND: pin - 10k - T, T - 22k - ACCENT, a diode from T back to
//!   the pin.
//! - Ringers (multiple-feedback band-pass, + input at VREF): the kick (C 100n, R2 1.2M, R3 680,
//!   R1 22k), the low tom (47n, 1.5M, 680, 47k), the high tom (33n, 1.5M, 560, 68k) and the snare
//!   body (22n, 1.5M, 1k, 100k).
//! - Snare noise:
//!   - a 1 Mohm resistor's thermal noise at the first stage's + input, as a noise current;
//!   - three non-inverting stages of x31 (30k / 1k, the 1k through 10 uF to VREF);
//!   - the gate cell: an NPN follower from the snare's T charging C_env 470n with 150k across it,
//!     an NPN chopper on C_env through 100k, its base from the noise through 10n with 100k to
//!     ground;
//!   - a 10n / 10k high-pass, and SNAP at half (25k) plus 33k into the mixer.
//! - Closed hat: the rig's metal bank (six Schmitt oscillators on their own 5 V, as the 74HC14
//!   sits on the digital supply) chopping a gate cell (C_env 220n, 470k), then the shared
//!   Sallen-Key high-pass (1n, 1n, 12k, 47k, unity gain) into the mixer.
//! - Mixer and out: an inverting summer, Rf 22k, inputs 33k (kick) and 47k (the rest), + input at
//!   VREF; VOLUME (10k, wiper at 15 %) into a unity buffer, then 100 ohm and 10 uF into a 10 kohm
//!   load.
//!
//! Left out against the design: the open hat, the cymbal and the MCU with its keys, LEDs and shift
//! registers.

use fab_elec_sim::expr::Expr;
use fab_elec_sim::{BKind, CtlKind, ModelKind, SimCircuit, Source, Wave};

const A0: f64 = 1e5;
const GBW: f64 = 1e6;
const ROUT: f64 = 50.0;
const ILIM: f64 = 0.02;
const IQ: f64 = 1e-4;
const R0: f64 = 1e6;
const CLAMP: f64 = 10.0;

/// One op-amp section `name` as the bench lowers it: a transconductance into the internal node
/// `{name}#x`, R0 and C0 there making the dominant pole, a clamp holding that node between the
/// rails, and the output as a current of `ilim tanh((v(x) - v(out)) / (rout ilim))`.
fn opamp(c: &mut SimCircuit, name: &str, inp: &str, inn: &str, out: &str) {
    let x = format!("{name}#x");
    c.vctl(&format!("{name}.gm"), CtlKind::Vccs, "0", &x, inp, inn, A0 / R0);
    c.r(&format!("{name}.r0"), &x, "0", R0);
    c.c(&format!("{name}.c0"), &x, "0", A0 / (2.0 * std::f64::consts::PI * GBW * R0));
    let mut e = Expr::parse(&format!("{CLAMP}*(uramp(v(x)-v(hi))-uramp(v(lo)-v(x)))")).expect("the clamp parses");
    e.rename(&|n| match n { "x" => x.clone(), "hi" => "va".to_owned(), _ => "0".to_owned() }, &|d| d.to_owned());
    c.b(&format!("{name}.clamp"), BKind::I, &x, "0", e);
    let mut e = Expr::parse(&format!("{ILIM}*tanh((v(x)-v(o))/{})", ROUT * ILIM)).expect("the output parses");
    e.rename(&|n| if n == "x" { x.clone() } else { out.to_owned() }, &|d| d.to_owned());
    c.b(&format!("{name}.out"), BKind::I, "0", out, e);
}

/// A trigger pin struck every `per` seconds from `td`, 1 ms wide, through the diode AND to `t`.
fn trigger(c: &mut SimCircuit, v: &str, t: &str, td: f64, per: f64) {
    let pin = format!("{v}.pin");
    c.v(&format!("{v}.src"), &pin, "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td, tr: 10e-6, tf: 10e-6, pw: 1e-3, per }));
    c.r(&format!("{v}.rt"), &pin, t, 10e3);
    c.r(&format!("{v}.ra"), t, "accent", 22e3);
    c.d(&format!("{v}.dt"), t, &pin, "dsig");
}

/// A multiple-feedback ringing filter `v` struck at `t`, its output `{v}.out`.
fn ringer(c: &mut SimCircuit, v: &str, t: &str, cap: f64, r1: f64, r2: f64, r3: f64) {
    let (a, inn, out) = (format!("{v}.a"), format!("{v}.inn"), format!("{v}.out"));
    c.r(&format!("{v}.r1"), t, &a, r1);
    c.r(&format!("{v}.r3"), &a, "ref", r3);
    c.c(&format!("{v}.ca"), &a, &inn, cap);
    c.c(&format!("{v}.cb"), &a, &out, cap);
    c.r(&format!("{v}.r2"), &out, &inn, r2);
    opamp(c, &format!("{v}.u"), "ref", &inn, &out);
}

/// A gate cell `g`: the follower from `t` charges `{g}.env`, the chopper's base is driven from
/// `drive` through `base_r` (and `couple` farads when AC-coupled), its collector `{g}.out` is the
/// output.
fn gate(c: &mut SimCircuit, g: &str, t: &str, drive: &str, env_c: f64, dis_r: f64, couple: Option<f64>, base_r: f64) {
    let (env, out, b) = (format!("{g}.env"), format!("{g}.out"), format!("{g}.b"));
    c.q(&format!("{g}.qf"), "va", t, &env, "qn");
    c.c(&format!("{g}.cenv"), &env, "0", env_c);
    c.r(&format!("{g}.rdis"), &env, "0", dis_r);
    c.r(&format!("{g}.rc"), &env, &out, 100e3);
    c.q(&format!("{g}.qc"), &out, &b, "0", "qn");
    match couple {
        Some(cc) => {
            let k = format!("{g}.k");
            c.c(&format!("{g}.cc"), drive, &k, cc);
            c.r(&format!("{g}.rb"), &k, &b, base_r);
            c.r(&format!("{g}.rg"), &b, "0", 100e3);
        }
        None => c.r(&format!("{g}.rb"), drive, &b, base_r),
    }
}

pub fn board() -> SimCircuit {
    board_with(false)
}

/// With `ideal_rail` VA is an ideal 5 V source (what a cut at the rail would give the partition).
pub fn board_with(ideal_rail: bool) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("qn", ModelKind::Npn, &[("is", 1e-14), ("bf", 200.0), ("vaf", 100.0), ("cje", 8e-12), ("cjc", 4e-12), ("tf", 4e-10), ("tr", 5e-8)]);
    c.model("dsig", ModelKind::Diode, &[("is", 2.5e-9), ("n", 1.75), ("rs", 0.6), ("cjo", 4e-12), ("tt", 6e-9)]);

    // power: USB 5 V, VA through 10 ohm and 22 uF, the op-amps' quiescent current, VREF
    if ideal_rail {
        c.v("vusb", "va", "0", Source::dc(5.0));
    } else {
        c.v("vusb", "usb", "0", Source::dc(5.0));
        c.r("rva", "usb", "va", 10.0);
        c.c("cva", "va", "0", 22e-6);
    }
    c.i("iq", "va", "0", Source::dc(IQ * 11.0));
    c.r("rref_hi", "va", "vmid", 10e3);
    c.r("rref_lo", "vmid", "0", 10e3);
    c.c("cref", "vmid", "0", 10e-6);
    opamp(&mut c, "uref", "vmid", "ref", "ref");
    // the supplies, as drum16 declares them (VBUS and VA power nets, VREF a power output)
    for node in if ideal_rail { &["va", "ref"][..] } else { &["usb", "va", "ref"] } {
        c.declare_supply(node);
    }

    // the sequencer's pins: accent on the first beat of every bar, the voices on a two-beat pattern
    c.v("vaccent", "accent", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 10e-3, tr: 10e-6, tf: 10e-6, pw: 1e-3, per: 1.0 }));
    trigger(&mut c, "bd", "bd.t", 10e-3, 0.5);
    trigger(&mut c, "lt", "lt.t", 135e-3, 0.5);
    trigger(&mut c, "ht", "ht.t", 385e-3, 0.5);
    trigger(&mut c, "sd", "sd.t", 260e-3, 0.5);
    trigger(&mut c, "ch", "ch.t", 72e-3, 0.125);

    ringer(&mut c, "bd", "bd.t", 100e-9, 22e3, 1.2e6, 680.0);
    ringer(&mut c, "lt", "lt.t", 47e-9, 47e3, 1.5e6, 680.0);
    ringer(&mut c, "ht", "ht.t", 33e-9, 68e3, 1.5e6, 560.0);
    ringer(&mut c, "sd", "sd.t", 22e-9, 100e3, 1.5e6, 1e3);

    // snare noise: 1 Mohm's thermal noise (129 nV/sqrt(Hz) over 48 kHz, as a current), three x31 stages
    c.r("rnz", "nz.p", "ref", 1e6);
    c.i("inz", "ref", "nz.p", Source::wave(Wave::Noise { seed: 11, sigma: 2.8e-11, dt: 1.0 / 96_000.0, base: 1.0 / 96_000.0 }));
    let mut prev = "nz.p".to_owned();
    for k in 1..=3 {
        let (fb, g, out) = (format!("nz{k}.fb"), format!("nz{k}.g"), format!("nz{k}.out"));
        c.r(&format!("nz{k}.rf"), &out, &fb, 30e3);
        c.r(&format!("nz{k}.rg"), &fb, &g, 1e3);
        c.c(&format!("nz{k}.cg"), &g, "ref", 10e-6);
        opamp(&mut c, &format!("nz{k}.u"), &prev, &fb, &out);
        prev = out;
    }
    gate(&mut c, "sn", "sd.t", &prev, 470e-9, 150e3, Some(10e-9), 10e3);
    c.c("sn.chp", "sn.out", "sn.hp", 10e-9);
    c.r("sn.rhp", "sn.hp", "ref", 10e3);

    // closed hat: the metal bank chopping its gate, then the Sallen-Key high-pass
    super::add_metal(&mut c);
    gate(&mut c, "hh", "ch.t", "metal", 220e-9, 470e3, None, 10e3);
    c.c("hpf.c1", "hh.out", "hpf.n1", 1e-9);
    c.c("hpf.c2", "hpf.n1", "hpf.n2", 1e-9);
    c.r("hpf.r1", "hpf.n1", "hpf.out", 12e3);
    c.r("hpf.r2", "hpf.n2", "ref", 47e3);
    opamp(&mut c, "hpf.u", "hpf.n2", "hpf.out", "hpf.out");

    // the mixer, VOLUME and the output
    c.r("mx.rf", "mx.out", "mx.inn", 22e3);
    c.r("mx.bd", "bd.out", "mx.inn", 33e3);
    c.r("mx.lt", "lt.out", "mx.inn", 47e3);
    c.r("mx.ht", "ht.out", "mx.inn", 47e3);
    c.r("mx.sd", "sd.out", "mx.inn", 47e3);
    c.r("mx.sn", "sn.hp", "mx.inn", 25e3 + 33e3);
    c.r("mx.hh", "hpf.out", "mx.inn", 47e3);
    opamp(&mut c, "mx.u", "ref", "mx.inn", "mx.out");
    c.r("vol.top", "mx.out", "vol.w", 8.5e3);
    c.r("vol.bot", "vol.w", "ref", 1.5e3);
    opamp(&mut c, "outbuf", "vol.w", "line", "line");
    c.r("rjack", "line", "jk", 100.0);
    c.c("cjack", "jk", "jack", 10e-6);
    c.r("rload", "jack", "0", 10e3);
    c
}

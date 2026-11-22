use fab_elec_core::{mm, Angle, Canon, LayerSet, Sel, Writer, P};
use fab_elec_lang::{evaluate, source::line_col, std_libs, Sources};
use fab_elec_parts::{BodyPortKind, Emitter, Light, PadKind, PadShape};

#[test]
fn every_std_generator_evaluates_with_reported_lands() {
    let text = r#"
use mod.elec.*
use mod.std.*
let e = mod.elec
let f = mod.elec.f
let fp_part = fn(name, fp) {
    return e.part(e.define(name, {
        kind: @ic symbol: f.ic_symbol(name, {}) footprint: fp pads: {}
    }), {})
}

let chip_part = e.part(chip("R", "0805"), {})
let led = e.part(led_part("0805", "green"), {})
let lamp = e.part(e.define("Lamp", {
    kind: @led
    symbol: f.two_terminal([["A", @passive], ["K", @passive]])
    body: {height: 1mm ports: [top_port(@light, 1mm, 0.4mm, 0.1mm, 5mm)]}
    emitter: {angle: 30deg light: {peak: 450nm}}
}), {})
let small5 = fp_part("Small5", sot23({pins: 5}))
let thin20 = fp_part("Thin20", tssop({
    pins: 20 pitch: 0.65mm body: [6.5mm, 4.4mm] span: 6.4mm lead: [0.3mm, 0.6mm]
}))
let hdr = e.part(header({pins: 4 pitch: 2.54mm}), {})
let receptacle = e.part(usb_c_receptacle(), {})
let jack = e.part(jack_3_5mm({}), {})
let mono_jack = e.part(jack_3_5mm({stereo: false}), {})
let switch_part = e.part(tact_switch(), {})
let symbol_part = e.part(e.define("Symbol", {
    kind: @ic
    symbol: ic_symbol("Symbol", {left: [["1", @passive]] right: [["2", @passive]]})
    footprint: f.footprint({
        pads: [f.smd("1", [-1mm, 0mm], [0.5mm, 0.5mm]),
               f.smd("2", [1mm, 0mm], [0.5mm, 0.5mm])]
        body: [[-0.5mm, -0.5mm], [0.5mm, 0.5mm]]
        mount: @smd text: 0.6mm graphics: []
    })
    pads: [["1", "1"], ["2", "2"]]
}), {})

let soic8 = fp_part("Soic8", soic({
    pins: 8 body: [4.9mm, 3.9mm] span: 6mm lead: [0.42mm, 0.835mm]
}))
let narrow8 = fp_part("Narrow8", soic_narrow(8))
let wide16 = fp_part("Wide16", soic_wide(16))
let qfp48 = fp_part("Qfp48", qfp({
    pins: 48 pitch: 0.5mm body: [7mm, 7mm] span: [9mm, 9mm] lead: [0.22mm, 0.6mm]
}))
let lqfp48 = fp_part("Lqfp48", lqfp(48, 7mm))
let qfn16 = fp_part("Qfn16", qfn({
    pins: 16 pitch: 0.5mm body: [3mm, 3mm] lead: [0.25mm, 0.4mm]
}))
let dfn8 = fp_part("Dfn8", dfn({
    pins: 8 pitch: 0.5mm body: [3mm, 3mm] lead: [0.25mm, 0.4mm]
}))
let balls = fp_part("Balls", bga({rows: 4 cols: 4 pitch: 0.8mm ball: 0.4mm body: [4mm, 4mm]}))
let ball_grid = fp_part("BallGrid", bga_grid(4, 0.8mm, 0.4mm, [4mm, 4mm]))

let flat = fp_part("Flat", flat2({body: [2.7mm, 1.6mm] span: 3.7mm lead: [0.55mm, 0.5mm]}))
let flat323 = fp_part("Flat323", sod323())
let flat123 = fp_part("Flat123", sod123())
let flat_a = fp_part("FlatA", sma())
let flat_b = fp_part("FlatB", smb())
let flat_c = fp_part("FlatC", smc())
let diode = e.part(diode_part("sod123"), {})

let small = fp_part("Small", small_outline({
    bottom: 3 top: 3 pitch: 0.65mm body: [2mm, 1.25mm] span: 2.1mm lead: [0.3mm, 0.35mm]
}))
let small323 = fp_part("Small323", sot323())
let small363 = fp_part("Small363", sot363())
let small353 = fp_part("Small353", sot353())
let small89 = fp_part("Small89", sot89({}))
let tab = fp_part("Tab", tab_package({
    leads: 3 pitch: 2.3mm body: [6.5mm, 3.5mm] span: 7mm
    lead: [0.7mm, 0.9mm] tab: [3mm, 0.9mm]
}))
let tab223 = fp_part("Tab223", sot223())
let tab252 = fp_part("Tab252", to252())

let dip8 = fp_part("Dip8", dip({pins: 8}))
let axial_part = fp_part("Axial", axial({span: 7.62mm body: [6.3mm, 2.5mm]}))
let radial_part = fp_part("Radial", radial({pitch: 2.5mm d: 5mm}))
let round3 = fp_part("Round3", to92({}))
let upright220 = fp_part("Upright220", to220({}))
let upright247 = fp_part("Upright247", to247({}))
let hdr2 = e.part(header2({pins: 10 pitch: 1.27mm lead: 0.30mm}), {})
let boxed = e.part(box_header({pins: 10}), {})
let wire = e.part(wire_smd({
    pins: 4 pitch: 1.25mm lead: [0.6mm, 1.6mm] tab: [1.2mm, 1.8mm]
    tab_gap: 1mm row_gap: 2.5mm
}), {})

let can = fp_part("Can", elcap({d: 6.3mm base: 6.6mm lead: [0.65mm, 2.2mm] span: 7.4mm}))
let can_size = fp_part("CanSize", elcap_size(6.3mm))
let can_part = e.part(elcap_part(6.3mm), {})
let coil = fp_part("Coil", power_inductor({body: 4mm term: [1.2mm, 3.4mm] gap: 2mm}))
let coil_size = fp_part("CoilSize", inductor_size(4mm))
let coil_part = e.part(inductor_part(4mm), {})

let ceramic = fp_part("Ceramic", ceramic4({
    body: [3.2mm, 2.5mm] land: [1mm, 0.8mm] gap: [1.2mm, 0.9mm]
}))
let ceramic3225 = fp_part("Ceramic3225", xtal3225())
let ceramic2520 = fp_part("Ceramic2520", xtal2520())
let ceramic2016 = fp_part("Ceramic2016", xtal2016())
let crystal = e.part(crystal_part("3225"), {})

let module = fp_part("Module", castellated({
    body: [18mm, 20mm] pitch: 1.27mm pad: [1.5mm, 0.9mm]
    left: 12 bottom: 10 right: 12 top: 0
}))
let module_grid = fp_part("ModuleGrid", castellated_grid(4, 1.27mm, [18mm, 20mm]))
let tp = e.part(testpoint({}), {})
let tp_thru = e.part(testpoint_thru({}), {})
let fid = e.part(fiducial({}), {})
let hole = e.part(mounting_hole({drill: 3.2mm}), {})
let hole2 = e.part(m2_hole(), {})
let hole25 = e.part(m25_hole(), {})
let hole3 = e.part(m3_hole(), {})
"#;
    let libs = std_libs();
    let mut sources = Sources::new();
    sources.add("std_parts.ckt.splash", text).expect("std_parts: circuit document");
    let evaluated = evaluate(&sources, &libs)
        .unwrap_or_else(|errors| panic!("std generators: document evaluates: {errors:#?}"));
    assert_eq!(evaluated.design.units.len(), 1, "std generators: one circuit");
    let circuit = &evaluated.design.units[0].circuit;
    let component = |generator: &str, path: &str| {
        circuit.comps.iter().find(|c| c.path == path)
            .unwrap_or_else(|| panic!("{generator}: component {path} exists"))
    };

    // Reported pad number, x, y, width, height, all lengths in millimetres.
    // A one-pad part repeats its sole pad; a two-pad part repeats its second as last.
    type Sample = (&'static str, f64, f64, f64, f64);
    let chip_lands = [
        ("1", -0.98, 0.0, 0.75, 1.25),
        ("2", 0.98, 0.0, 0.75, 1.25),
        ("2", 0.98, 0.0, 0.75, 1.25),
    ];
    let soic_lands = [
        ("1", -2.58, 1.905, 1.54, 0.48),
        ("2", -2.58, 0.635, 1.54, 0.48),
        ("8", 2.58, 1.905, 1.54, 0.48),
    ];
    let qfp_lands = [
        ("1", -4.20, 2.75, 1.30, 0.28),
        ("2", -4.20, 2.25, 1.30, 0.28),
        ("48", -2.75, 4.20, 0.28, 1.30),
    ];
    let ball_lands = [
        ("A1", -1.2, 1.2, 0.32, 0.32),
        ("A2", -0.4, 1.2, 0.32, 0.32),
        ("D4", 1.2, -1.2, 0.32, 0.32),
    ];
    let flat_lands = [
        ("1", -1.70, 0.0, 0.80, 0.55),
        ("2", 1.70, 0.0, 0.80, 0.55),
        ("2", 1.70, 0.0, 0.80, 0.55),
    ];
    let small_lands = [
        ("1", -0.65, -0.88, 0.36, 1.05),
        ("2", 0.0, -0.88, 0.36, 1.05),
        ("6", -0.65, 0.88, 0.36, 1.05),
    ];
    let tab_lands = [
        ("1", -2.30, -3.05, 0.76, 1.60),
        ("2", 0.0, -3.05, 0.76, 1.60),
        ("4", 0.0, 3.23, 3.00, 1.25),
    ];
    let can_lands = [
        ("1", -2.70, 0.0, 2.50, 0.65),
        ("2", 2.70, 0.0, 2.50, 0.65),
        ("2", 2.70, 0.0, 2.50, 0.65),
    ];
    let coil_lands = [
        ("1", -1.75, 0.0, 1.50, 3.40),
        ("2", 1.75, 0.0, 1.50, 3.40),
        ("2", 1.75, 0.0, 1.50, 3.40),
    ];
    let ceramic_lands = [
        ("1", -1.25, -1.0, 1.30, 1.10),
        ("2", 1.25, -1.0, 1.30, 1.10),
        ("4", -1.25, 1.0, 1.30, 1.10),
    ];
    let cases: &[(&str, &str, usize, [Sample; 3])] = &[
        ("chip", "chip_part", 2, chip_lands),
        ("led_part", "led", 2, chip_lands),
        ("sot23", "small5", 5, [
            ("1", -0.95, -1.1, 0.6, 1.1),
            ("2", 0.0, -1.1, 0.6, 1.1),
            ("5", -0.95, 1.1, 0.6, 1.1),
        ]),
        ("tssop", "thin20", 20, [
            ("1", -2.9, 2.925, 1.5, 0.4),
            ("2", -2.9, 2.275, 1.5, 0.4),
            ("20", 2.9, 2.925, 1.5, 0.4),
        ]),
        ("header", "hdr", 4, [
            ("1", 0.0, 0.0, 2.03, 2.03),
            ("2", 0.0, -2.54, 2.03, 2.03),
            ("4", 0.0, -7.62, 2.03, 2.03),
        ]),
        ("usb_c_receptacle", "receptacle", 10, [
            ("B12", -2.7, 2.025, 0.8, 1.2),
            ("B9", -1.5, 2.025, 0.7, 1.2),
            ("S1", 4.32, -2.025, 1.1, 1.9),
        ]),
        ("jack_3_5mm", "jack", 5, [
            ("S", 2.25, -3.425, 1.2, 2.0),
            ("T", 2.05, 3.575, 1.2, 2.0),
            ("", 0.0, 2.525, 1.2, 1.2),
        ]),
        ("jack_3_5mm", "mono_jack", 5, [
            ("S", 2.25, -3.425, 1.2, 2.0),
            ("T", 2.05, 3.575, 1.2, 2.0),
            ("", 0.0, 2.525, 1.2, 1.2),
        ]),
        ("tact_switch", "switch_part", 4, [
            ("1", -3.0, 1.85, 1.0, 0.75),
            ("2", 3.0, 1.85, 1.0, 0.75),
            ("4", 3.0, -1.85, 1.0, 0.75),
        ]),
        ("ic_symbol", "symbol_part", 2, [
            ("1", -1.0, 0.0, 0.5, 0.5),
            ("2", 1.0, 0.0, 0.5, 0.5),
            ("2", 1.0, 0.0, 0.5, 0.5),
        ]),
        ("soic", "soic8", 8, soic_lands),
        ("soic_narrow", "narrow8", 8, soic_lands),
        ("soic_wide", "wide16", 16, [
            ("1", -4.73, 4.445, 1.54, 0.48),
            ("2", -4.73, 3.175, 1.54, 0.48),
            ("16", 4.73, 4.445, 1.54, 0.48),
        ]),
        ("qfp", "qfp48", 48, qfp_lands),
        ("lqfp", "lqfp48", 48, qfp_lands),
        ("qfn", "qfn16", 16, [
            ("1", -1.45, 0.75, 0.70, 0.25),
            ("2", -1.45, 0.25, 0.70, 0.25),
            ("16", -0.75, 1.45, 0.25, 0.70),
        ]),
        ("dfn", "dfn8", 8, [
            ("1", -1.45, 0.75, 0.70, 0.25),
            ("2", -1.45, 0.25, 0.70, 0.25),
            ("8", 1.45, 0.75, 0.70, 0.25),
        ]),
        ("bga", "balls", 16, ball_lands),
        ("bga_grid", "ball_grid", 16, ball_lands),
        ("flat2", "flat", 2, flat_lands),
        ("sod323", "flat323", 2, [
            ("1", -1.20, 0.0, 0.60, 0.30),
            ("2", 1.20, 0.0, 0.60, 0.30),
            ("2", 1.20, 0.0, 0.60, 0.30),
        ]),
        ("sod123", "flat123", 2, flat_lands),
        ("sma", "flat_a", 2, [
            ("1", -2.10, 0.0, 1.30, 1.40),
            ("2", 2.10, 0.0, 1.30, 1.40),
            ("2", 2.10, 0.0, 1.30, 1.40),
        ]),
        ("smb", "flat_b", 2, [
            ("1", -2.30, 0.0, 1.30, 2.00),
            ("2", 2.30, 0.0, 1.30, 2.00),
            ("2", 2.30, 0.0, 1.30, 2.00),
        ]),
        ("smc", "flat_c", 2, [
            ("1", -3.55, 0.0, 1.30, 3.00),
            ("2", 3.55, 0.0, 1.30, 3.00),
            ("2", 3.55, 0.0, 1.30, 3.00),
        ]),
        ("diode_part", "diode", 2, flat_lands),
        ("small_outline", "small", 6, small_lands),
        ("sot323", "small323", 3, [
            ("1", -0.325, -0.88, 0.36, 1.05),
            ("2", 0.325, -0.88, 0.36, 1.05),
            ("3", 0.0, 0.88, 0.36, 1.05),
        ]),
        ("sot363", "small363", 6, small_lands),
        ("sot353", "small353", 5, [
            ("1", -0.65, -0.88, 0.36, 1.05),
            ("2", 0.0, -0.88, 0.36, 1.05),
            ("5", -0.65, 0.88, 0.36, 1.05),
        ]),
        ("sot89", "small89", 4, [
            ("1", -1.50, -1.65, 0.50, 1.50),
            ("2", 0.0, -1.65, 0.50, 1.50),
            ("4", 0.0, 1.65, 1.76, 1.50),
        ]),
        ("tab_package", "tab", 4, tab_lands),
        ("sot223", "tab223", 4, tab_lands),
        ("to252", "tab252", 3, [
            ("1", -2.28, -4.45, 0.82, 1.70),
            ("3", 2.28, -4.45, 0.82, 1.70),
            ("2", 0.0, 2.38, 5.30, 5.85),
        ]),
        ("dip", "dip8", 8, [
            ("1", -3.81, 3.81, 1.30, 1.30),
            ("2", -3.81, 1.27, 1.30, 1.30),
            ("8", 3.81, 3.81, 1.30, 1.30),
        ]),
        ("axial", "axial_part", 2, [
            ("1", -3.81, 0.0, 1.40, 1.40),
            ("2", 3.81, 0.0, 1.40, 1.40),
            ("2", 3.81, 0.0, 1.40, 1.40),
        ]),
        ("radial", "radial_part", 2, [
            ("1", -1.25, 0.0, 1.40, 1.40),
            ("2", 1.25, 0.0, 1.40, 1.40),
            ("2", 1.25, 0.0, 1.40, 1.40),
        ]),
        ("to92", "round3", 3, [
            ("1", -1.27, 0.0, 1.25, 1.25),
            ("2", 0.0, 0.0, 1.25, 1.25),
            ("3", 1.27, 0.0, 1.25, 1.25),
        ]),
        ("to220", "upright220", 3, [
            ("1", -2.54, 0.0, 1.75, 1.75),
            ("2", 0.0, 0.0, 1.75, 1.75),
            ("3", 2.54, 0.0, 1.75, 1.75),
        ]),
        ("to247", "upright247", 3, [
            ("1", -5.45, 0.0, 2.15, 2.15),
            ("2", 0.0, 0.0, 2.15, 2.15),
            ("3", 5.45, 0.0, 2.15, 2.15),
        ]),
        ("header2", "hdr2", 10, [
            ("1", 0.0, 0.0, 1.25, 1.25),
            ("2", 1.27, 0.0, 1.25, 1.25),
            ("10", 1.27, -5.08, 1.25, 1.25),
        ]),
        ("box_header", "boxed", 10, [
            ("1", 0.0, 0.0, 1.75, 1.75),
            ("2", 2.54, 0.0, 1.75, 1.75),
            ("10", 2.54, -10.16, 1.75, 1.75),
        ]),
        ("wire_smd", "wire", 6, [
            ("1", -1.875, 0.0, 0.66, 2.30),
            ("2", -0.625, 0.0, 0.66, 2.30),
            ("MP", 3.475, 2.5, 1.20, 1.80),
        ]),
        ("elcap", "can", 2, can_lands),
        ("elcap_size", "can_size", 2, can_lands),
        ("elcap_part", "can_part", 2, can_lands),
        ("power_inductor", "coil", 2, coil_lands),
        ("inductor_size", "coil_size", 2, coil_lands),
        ("inductor_part", "coil_part", 2, coil_lands),
        ("ceramic4", "ceramic", 4, ceramic_lands),
        ("xtal3225", "ceramic3225", 4, ceramic_lands),
        ("xtal2520", "ceramic2520", 4, [
            ("1", -1.0, -0.8, 1.10, 1.0),
            ("2", 1.0, -0.8, 1.10, 1.0),
            ("4", -1.0, 0.8, 1.10, 1.0),
        ]),
        ("xtal2016", "ceramic2016", 4, [
            ("1", -0.85, -0.7, 0.90, 0.80),
            ("2", 0.85, -0.7, 0.90, 0.80),
            ("4", -0.85, 0.7, 0.90, 0.80),
        ]),
        ("crystal_part", "crystal", 4, ceramic_lands),
        ("castellated", "module", 34, [
            ("1", -8.30, 6.985, 1.60, 0.90),
            ("2", -8.30, 5.715, 1.60, 0.90),
            ("34", 8.30, 6.985, 1.60, 0.90),
        ]),
        ("castellated_grid", "module_grid", 16, [
            ("1", -8.30, 1.905, 1.60, 0.90),
            ("2", -8.30, 0.635, 1.60, 0.90),
            ("16", -1.905, 9.30, 0.90, 1.60),
        ]),
        ("testpoint", "tp", 1, [("1", 0.0, 0.0, 1.0, 1.0); 3]),
        ("testpoint_thru", "tp_thru", 1, [("1", 0.0, 0.0, 1.6, 1.6); 3]),
        ("fiducial", "fid", 1, [("", 0.0, 0.0, 1.0, 1.0); 3]),
        ("mounting_hole", "hole", 1, [("", 0.0, 0.0, 3.2, 3.2); 3]),
        ("m2_hole", "hole2", 1, [("", 0.0, 0.0, 2.2, 2.2); 3]),
        ("m25_hole", "hole25", 1, [("", 0.0, 0.0, 2.7, 2.7); 3]),
        ("m3_hole", "hole3", 1, [("", 0.0, 0.0, 3.2, 3.2); 3]),
    ];
    assert_eq!(circuit.comps.len(), cases.len() + 1, "std generators: component count (the cases and the footprintless lamp)");
    for &(generator, path, count, samples) in cases {
        let footprint = component(generator, path).part.footprint.as_ref()
            .unwrap_or_else(|| panic!("{generator}: footprint exists"));
        assert_eq!(footprint.pads.len(), count, "{generator}: pad count");
        for (index, (no, x, y, w, h)) in [0, 1.min(count - 1), count - 1].into_iter().zip(samples) {
            let pad = &footprint.pads[index];
            assert_eq!(pad.no, no, "{generator}: pad {index} number");
            assert_eq!(pad.at, P::mm(x, y), "{generator}: pad {no:?} at");
            let size = match &pad.stack.front {
                PadShape::Circle { d } => P::new(*d, *d),
                PadShape::Rect { size } | PadShape::RoundRect { size, .. } | PadShape::Oval { size } => *size,
                shape => panic!("{generator}: pad {no:?} has an unexpected land shape: {shape:?}"),
            };
            assert_eq!(size, P::mm(w, h), "{generator}: pad {no:?} land size");
        }
        if matches!(generator, "bga" | "bga_grid" | "testpoint" | "fiducial") {
            for pad in &footprint.pads {
                assert!(matches!(&pad.stack.front, PadShape::Circle { .. }),
                    "{generator}: pad {:?} circular land", pad.no);
            }
        }
        if matches!(generator, "mounting_hole" | "m2_hole" | "m25_hole" | "m3_hole") {
            let pad = &footprint.pads[0];
            assert_eq!(pad.kind, PadKind::Npth, "{generator}: unplated hole");
            assert_eq!(pad.stack.copper, LayerSet::EMPTY, "{generator}: no copper");
            let drill = pad.drill.as_ref().unwrap_or_else(|| panic!("{generator}: drill exists"));
            assert_eq!(drill.size, P::mm(samples[0].3, samples[0].4), "{generator}: drill size");
        }
    }
    let pins = &component("ic_symbol", "symbol_part").part.pins;
    assert_eq!(pins.len(), 2, "ic_symbol: pin count");
    assert_eq!((&pins[0].name[..], &pins[1].name[..]), ("1", "2"), "ic_symbol: pin order");
    let fid = component("fiducial", "fid").part.footprint.as_ref().expect("fiducial: footprint exists");
    assert_eq!(fid.pads[0].mask_margin, Some(P::mm(1.0, 0.0).x), "fiducial: mask clearance");

    let bad = r#"
use mod.elec.*
use mod.std.*
let bad_soic = soic({pins: 7 body: [4.9mm, 3.9mm] span: 6mm lead: [0.42mm, 0.835mm]})
let bad_lqfp = lqfp(48, 8mm)
"#;
    // Faces that take more than the whole flux are refused.
    let over = r#"
use mod.elec.*
use mod.std.*
let e = mod.elec
let f = mod.elec.f
let over = e.part(e.define("Over", {
    kind: @led
    symbol: f.two_terminal([["A", @passive], ["K", @passive]])
    body: {height: 1mm ports: [top_port(@light, 1mm, 0.4mm, 0.1mm, 5mm)]}
    emitter: {angle: 120deg light: @red faces: [
        {at: [0.5mm, 0, 0.5mm] dir: [1, 0, 0] size: [1mm, 1mm] share: 0.7}
        {at: [-0.5mm, 0, 0.5mm] dir: [-1, 0, 0] size: [1mm, 1mm] share: 0.6}
    ]}
}), {})
"#;
    let mut over_sources = Sources::new();
    over_sources.add("std_parts_over.ckt.splash", over).expect("over: circuit document");
    let errors = evaluate(&over_sources, &libs).expect_err("over: faces past the whole flux fail");
    assert!(errors.iter().any(|error| error.message.contains("their shares add up to 1.300")), "over: the shares' total is named: {errors:#?}");

    let mut bad_sources = Sources::new();
    let bad_doc = bad_sources.add("std_parts_bad.ckt.splash", bad).expect("std errors: circuit document");
    let errors = evaluate(&bad_sources, &libs).expect_err("soic and lqfp: invalid calls fail");
    assert_eq!(errors.len(), 2, "soic and lqfp: one error per bad call: {errors:#?}");
    for (generator, binding) in [("soic", "let bad_soic"), ("lqfp", "let bad_lqfp")] {
        let error = errors.iter().find(|error| error.message.contains(&format!("{generator}:")))
            .unwrap_or_else(|| panic!("{generator}: error names the generator: {errors:#?}"));
        let line = bad.lines().position(|line| line.starts_with(binding))
            .unwrap_or_else(|| panic!("{generator}: bad call exists")) as u32 + 1;
        match error.sel.first() {
            Some(Sel::Source { doc, start, end }) => {
                assert_eq!(*doc, bad_doc, "{generator}: error in the circuit document");
                assert!(*start < *end && (*end as usize) <= bad.len(), "{generator}: located error span");
                assert_eq!(line_col(bad, *start).map(|at| at.0), Some(line), "{generator}: error at its bad call");
            }
            _ => panic!("{generator}: error has a source location: {error:?}"),
        }
    }

    // Std part bodies and case ports, using the components evaluated above.
    let body = component("chip", "chip_part").part.body.as_ref()
        .expect("chip_part: body exists");
    assert_eq!(body.height, mm(0.5), "chip_part: body height");
    assert_eq!(body.ports.len(), 0, "chip_part: body port count");

    let body = component("led_part", "led").part.body.as_ref()
        .expect("led: body exists");
    assert_eq!(body.height, mm(0.6), "led: body height");
    assert_eq!(body.ports.len(), 1, "led: body port count");
    assert_eq!(body.ports[0].kind, BodyPortKind::Light, "led: port 0 kind");
    let led = &component("led_part", "led").part;
    let e = led.emitter.as_ref().expect("led: a std LED has an emitter");
    let port = &body.ports[0];
    assert_eq!((e.at, e.dir), (port.at, port.dir), "led: the light leaves by the body's light port");
    let open = port.profile.bbox();
    assert_eq!(e.size, P::new(open.width() as i32, open.height() as i32), "led: the emitting face is the port's opening");
    assert_eq!((e.angle, &e.light, e.flux_per_a), (Angle::deg(120.0), &Light::Peak(525.0), Some(15.0)), "led: a true green die in a wide beam");
    assert_eq!(led.name, "LED_0805_green", "led: each colour is its own part");
    // The dome's walls light sideways beside its top: a 150-degree beam, the data sheet's stated
    // angle, with its polar plot's points (read by eye) within 0.08, averaged round the axis.
    assert_eq!(e.faces.len(), 4, "led: the dome's four walls emit beside its top");
    assert!(e.faces.iter().all(|face| face.angle == Angle::deg(120.0)), "led: each wall is a Lambertian face");
    assert!((e.port_share() - 0.354).abs() < 1e-9 && (e.lobes().map(|face| face.share).sum::<f64>() - 1.0).abs() < 1e-9,
        "led: the top keeps 0.354 of the flux and the walls the rest");
    let lobes: Vec<_> = e.lobes().collect();
    let intensity = |theta: f64| -> f64 {
        (0..720).map(|k| {
            let phi = k as f64 * std::f64::consts::TAU / 720.0;
            let d = [theta.sin() * phi.cos(), theta.sin() * phi.sin(), theta.cos()];
            lobes.iter().map(|face| face.share * (face.dir[0] * d[0] + face.dir[1] * d[1] + face.dir[2] * d[2]).max(0.0)).sum::<f64>()
        }).sum::<f64>() / 720.0
    };
    let axis = intensity(0.0);
    for (degrees, read) in [(30.0, 0.93), (45.0, 0.83), (60.0, 0.70), (75.0, 0.56), (85.0, 0.45), (90.0, 0.38)] {
        let relative = intensity(f64::to_radians(degrees)) / axis;
        assert!((relative - read).abs() <= 0.08, "led: {relative:.3} at {degrees} degrees, the plot reads {read}");
    }
    let (mut lo, mut hi) = (0.0, std::f64::consts::FRAC_PI_2);
    for _ in 0..50 {
        let mid = (lo + hi) / 2.0;
        if intensity(mid) / axis > 0.5 { lo = mid } else { hi = mid }
    }
    let beam = 2.0 * lo.to_degrees();
    assert!((beam - 150.0).abs() <= 3.0, "led: a {beam:.1} degree beam at half intensity, the data sheet states 150");
    let lamp = &component("e.define", "lamp").part;
    let e = lamp.emitter.as_ref().expect("lamp: an explicit emitter is kept");
    assert_eq!((e.angle, &e.light, e.flux_per_a), (Angle::deg(30.0), &Light::Peak(450.0), None), "lamp: an explicit peak wavelength in nm");
    let shares: Vec<f64> = e.lobes().map(|face| face.share).collect();
    assert!(e.faces.is_empty() && shares == [1.0], "lamp: one face keeps all the light, as before: {shares:?}");
    // A one-face emitter writes no faces, so it keeps the hash it had before faces; faces add.
    let bytes = |c: &dyn Canon| { let mut w = Writer::bytes(); c.canon(&mut w); w.into_bytes() };
    let fields: [&dyn Canon; 6] = [&e.at, &e.dir, &e.size, &e.angle, &e.light, &e.flux_per_a];
    let mut before = Writer::bytes();
    for field in fields {
        field.canon(&mut before);
    }
    assert_eq!(bytes(e), before.into_bytes(), "lamp: a one-face emitter keeps its canonical form");
    let walled = Emitter { faces: led.emitter.as_ref().expect("led: emitter").faces.clone(), ..e.clone() };
    assert_ne!(bytes(&walled), bytes(e), "lamp: further faces are part of the emitter's canonical form");

    let body = component("header", "hdr").part.body.as_ref()
        .expect("hdr: body exists");
    assert_eq!(body.height, mm(2.5), "hdr: body height");
    assert_eq!(body.ports.len(), 0, "hdr: body port count");

    let legs = &component("usb_c_receptacle", "receptacle").part.footprint.as_ref().expect("receptacle: footprint exists").pads[6..];
    assert!(legs.iter().all(|leg| leg.no == "S1" && leg.drill.as_ref().map(|d| d.size) == Some(P::mm(0.6, 1.3))),
        "receptacle: each shell leg is drilled as a 0.6 x 1.3 mm slot along the plug's axis");
    let body = component("usb_c_receptacle", "receptacle").part.body.as_ref()
        .expect("receptacle: body exists");
    assert_eq!(body.height, mm(3.16), "receptacle: body height");
    assert_eq!(body.ports.len(), 1, "receptacle: body port count");
    assert_eq!(body.ports[0].kind, BodyPortKind::Plug, "receptacle: port 0 kind");
    assert_eq!(body.ports[0].at, [mm(0.0), mm(-4.625), mm(1.58)], "receptacle: port 0 at");
    assert_eq!(body.ports[0].dir, [0.0, -1.0, 0.0], "receptacle: port 0 dir");

    // A jack's mouth is a plug port, so a case opens round it, and its role says which pins a host
    // hears: tip (left) and ring (right) against the sleeve; a mono jack has no right channel.
    let jack = &component("jack_3_5mm", "jack").part;
    let body = jack.body.as_ref().expect("jack: body exists");
    assert_eq!(body.height, mm(5.0), "jack: body height");
    assert_eq!(body.ports.len(), 1, "jack: body port count");
    assert_eq!(body.ports[0].kind, BodyPortKind::Plug, "jack: port 0 kind");
    assert_eq!(body.ports[0].at, [mm(0.0), mm(-8.575), mm(2.5)], "jack: port 0 at, the mouth at the nose's tip toward -Y");
    assert_eq!(body.ports[0].dir, [0.0, -1.0, 0.0], "jack: port 0 dir");
    let role = |part: &fab_elec_parts::PartDef| ["audio", "audio_left", "audio_right", "audio_ref"].map(|k| part.fields.get(k).map(str::to_string));
    let some = |s: &str| Some(s.to_string());
    assert_eq!(role(jack), [some("out"), some("T"), some("R"), some("S")], "jack: an audio output, tip left, ring right, sleeve the reference");
    let mono = &component("jack_3_5mm", "mono_jack").part;
    assert_eq!(role(mono), [some("out"), some("T"), None, some("S")], "mono_jack: an audio output with no right channel");
    assert_eq!(mono.pins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["T", "S"], "mono_jack: tip and sleeve pins");

    let body = component("tact_switch", "switch_part").part.body.as_ref()
        .expect("switch_part: body exists");
    assert_eq!(body.height, mm(1.5), "switch_part: body height");
    assert_eq!(body.ports.len(), 1, "switch_part: body port count");
    assert_eq!(body.ports[0].kind, BodyPortKind::Press, "switch_part: port 0 kind");
    assert_eq!(body.ports[0].at, [mm(0.0), mm(0.0), mm(1.5)], "switch_part: port 0 at, the plunger's top");
    assert_eq!(body.ports[0].travel, mm(0.25), "switch_part: port 0 states the plunger's travel");
    assert_eq!(body.ports[0].stand, mm(0.3), "switch_part: port 0 states the plunger's stand above its housing");

    let body = component("diode_part", "diode").part.body.as_ref()
        .expect("diode: body exists");
    assert_eq!(body.height, mm(1.1), "diode: body height");
    assert_eq!(body.ports.len(), 0, "diode: body port count");

    let body = component("header2", "hdr2").part.body.as_ref()
        .expect("hdr2: body exists");
    assert_eq!(body.height, mm(2.5), "hdr2: body height");
    assert_eq!(body.ports.len(), 1, "hdr2: body port count");
    assert_eq!(body.ports[0].kind, BodyPortKind::Plug, "hdr2: port 0 kind");

    let body = component("box_header", "boxed").part.body.as_ref()
        .expect("boxed: body exists");
    assert_eq!(body.height, mm(8.8), "boxed: body height");
    assert_eq!(body.ports.len(), 1, "boxed: body port count");
    assert_eq!(body.ports[0].kind, BodyPortKind::Plug, "boxed: port 0 kind");

    let body = component("wire_smd", "wire").part.body.as_ref()
        .expect("wire: body exists");
    assert_eq!(body.height, mm(4.0), "wire: body height");
    assert_eq!(body.ports.len(), 1, "wire: body port count");
    assert_eq!(body.ports[0].kind, BodyPortKind::Plug, "wire: port 0 kind");

    let body = component("elcap_part", "can_part").part.body.as_ref()
        .expect("can_part: body exists");
    assert_eq!(body.height, mm(5.4), "can_part: body height");
    assert_eq!(body.ports.len(), 0, "can_part: body port count");

    let body = component("inductor_part", "coil_part").part.body.as_ref()
        .expect("coil_part: body exists");
    assert_eq!(body.height, mm(2.0), "coil_part: body height");
    assert_eq!(body.ports.len(), 0, "coil_part: body port count");

    let body = component("crystal_part", "crystal").part.body.as_ref()
        .expect("crystal: body exists");
    assert_eq!(body.height, mm(0.8), "crystal: body height");
    assert_eq!(body.ports.len(), 0, "crystal: body port count");

    assert!(component("testpoint", "tp").part.body.is_none(), "tp: no body");
    assert!(component("testpoint_thru", "tp_thru").part.body.is_none(), "tp_thru: no body");
    assert!(component("fiducial", "fid").part.body.is_none(), "fid: no body");
    assert!(component("mounting_hole", "hole").part.body.is_none(), "hole: no body");
    assert!(component("m2_hole", "hole2").part.body.is_none(), "hole2: no body");
    assert!(component("m25_hole", "hole25").part.body.is_none(), "hole25: no body");
    assert!(component("m3_hole", "hole3").part.body.is_none(), "hole3: no body");
}

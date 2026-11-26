//! Hand-made bad files must all be refused; a well-formed one must pass.
//! The profile and templates here are fictional, written for these tests.

use super::*;

const START: &str = "; fictional start\nM104 S[nozzle_temperature_initial_layer]\nG28\nT[initial_extruder]\n{if bed_temperature > 0}\nM190 S[bed_temperature]\n{endif}\nG29 A1 X{first_layer_print_min[0]} Y{first_layer_print_min[1]} I{first_layer_print_size[0]} J{first_layer_print_size[1]}\nG90\nG1 Z5 F1200\nM83\n";
const END: &str = "; fictional end\nM400\nG91\nG1 Z2 F600\nG90\nM104 S0\n";
const CHANGE: &str = "M620 S[next_filament_id]A\nG1 Z{max_layer_z + 3.0} F1200\nT[next_filament_id]\nM621 S[next_filament_id]A\n";
const LAYER: &str = "M73 L{layer_num}\n";

pub(crate) fn profile() -> MachineProfile {
    MachineProfile {
        flavor: Flavor::Mask,
        printer_model: "Bambu Lab H2D".into(),
        printable_area: parse_points("0x0,350x0,350x320,0x320"),
        printable_height: 325.0,
        extruder_areas: vec![parse_points("0x0,325x0,325x320,0x320"), parse_points("25x0,350x0,350x320,25x320")],
        extruder_heights: vec![320.0, 325.0],
        extruder_offsets: vec![[0.0, 0.0], [0.0, 0.0]],
        nozzle_diameters: vec![0.4, 0.4],
        templates: vec![
            ("machine_start_gcode".into(), START.into()), ("machine_end_gcode".into(), END.into()),
            ("change_filament_gcode".into(), CHANGE.into()), ("layer_change_gcode".into(), LAYER.into()),
        ],
        head: None,
        max_speed_z: Some(30.0),
        filament_diameters: vec![1.75, 1.75],
        max_volumetric: vec![25.0, 25.0],
        filament_types: vec!["PLA".into(), "PLA".into()],
        nozzle_temp_ranges: vec![[190.0, 240.0]; 2],
        print_temps: vec![[220.0, 220.0]; 2],
        required_hrc: vec![3.0, 3.0],
        chamber_temps: vec![0.0, 0.0],
        physical_extruder_map: vec![0, 1],
        limits: MotionLimits { acceleration: [Some(20000.0); 4], speed: [Some(1000.0); 4], jerk: [Some(100.0); 4], move_acceleration: Some(20000.0) },
        prime_allowance: 5.8,
    }
}

/// A config-block value: backslashes doubled, newlines as `\n`, as the gate reads them.
fn esc(t: &str) -> String { t.replace('\\', "\\\\").replace('\n', "\\n") }

/// A whole file: settings (overridable), the start, `body`, the end.
fn file(settings: &[(&str, &str)], start: &str, body: &str) -> String {
    let mut keys: Vec<(String, String)> = vec![
        ("printer_model".into(), "Bambu Lab H2D".into()), ("curr_bed_type".into(), "Textured PEI Plate".into()),
        ("nozzle_diameter".into(), "0.4,0.4".into()), ("extruder_offset".into(), "0x0,0x0".into()),
        ("printable_area".into(), "0x0,350x0,350x320,0x320".into()),
        ("extruder_printable_area".into(), "0x0,325x0,325x320,0x320#25x0,350x0,350x320,25x320".into()),
        ("extruder_printable_height".into(), "320,325".into()), ("filament_map".into(), "2,1".into()),
        ("machine_start_gcode".into(), esc(START)), ("machine_end_gcode".into(), esc(END)),
        ("change_filament_gcode".into(), esc(CHANGE)), ("layer_change_gcode".into(), esc(LAYER)),
    ];
    for (k, v) in settings {
        match keys.iter_mut().find(|(key, _)| key == k) { Some(e) => e.1 = v.to_string(), None => keys.push((k.to_string(), v.to_string())) }
    }
    let mut s = String::from("; HEADER_BLOCK_START\n; HEADER_BLOCK_END\n\n; CONFIG_BLOCK_START\n");
    for (k, v) in keys { s.push_str(&format!("; {k} = {v}\n")); }
    s.push_str("; CONFIG_BLOCK_END\n\n; EXECUTABLE_BLOCK_START\nM73 P0 R5\nM201 X20000 Y20000\n");
    s.push_str(start);
    s.push_str("; MACHINE_START_GCODE_END\nG90\nG21\nM83\n");
    s.push_str(body);
    s.push_str(&END.replace("; fictional end\n", ""));
    s.push_str("; EXECUTABLE_BLOCK_END\n");
    s
}

const GOOD_START: &str = "M104 S220\nG28\nT0\nM190 S55\nG29 A1 X165 Y150 I20 J20\nG90\nG1 Z5 F1200\nM83\n";

/// A 20 mm square on the right nozzle (filament 0 -> nozzle 2), two layers.
fn square(z1: f64, z2: f64) -> String {
    format!("M73 L1\n; FEATURE: Outer wall\n; LINE_WIDTH: 0.42\n; LAYER_HEIGHT: 0.2\nG1 X165 Y150 F30000\nG1 Z{z1}\nG1 X185 Y150 E1 F3000\nG1 X185 Y170 E1\nG1 X165 Y170 E1\nG1 X165 Y150 E1\nG1 Z{} F1200\nM73 L2\nG1 Z{z2}\nG1 X185 Y150 E1\nG1 X185 Y170 E1\nG3 X175 Y160 I-5 J-5 E0.5\n", z1 + 0.4)
}

fn check(text: &str) -> Result<JobFacts, Refusal> { check_gcode(text, &profile()) }

fn refused(text: &str, rule: &str) {
    match check(text) {
        Ok(_) => panic!("accepted a file that breaks {rule}"),
        Err(r) => assert!(r.has(rule), "expected {rule}, got {:?}", r.0),
    }
}

#[test]
fn a_well_formed_file_passes() {
    let facts = check(&file(&[], GOOD_START, &square(0.2, 0.4))).unwrap();
    assert_eq!(facts.first_layer_z, 0.2);
    assert_eq!(facts.extruders_used, vec![1]);
    assert_eq!(facts.filaments, vec![FilamentFacts { kind: "PLA".into(), extruder: 1, print_temps: [220.0, 220.0], needs_hardened: false }, FilamentFacts { kind: "PLA".into(), extruder: 0, print_temps: [220.0, 220.0], needs_hardened: false }]);
    // A tool change through the installed template, then the left nozzle's area.
    let body = format!("{}M620 S1A\nG1 Z3.4 F1200\nT1\nM621 S1A\nG1 Z1.4\nG1 X20 Y150\nG1 Z0.4\nG1 X40 Y150 E1\n", square(0.2, 0.4));
    let facts = check(&file(&[], GOOD_START, &body)).unwrap();
    assert_eq!(facts.extruders_used, vec![0, 1]);
}

#[test]
fn pressure_advance_is_bounded_and_has_only_k() {
    for command in ["M900 K0.020", "M900 K0", "M900 K0.5"] {
        check(&file(&[], GOOD_START, &format!("{command}\n{}", square(0.2, 0.4)))).unwrap();
    }
    for command in [
        "M900 K0.9", "M900 K0.02 L1000", "M900 K-0.01", "M900", "M900 L0.02", "M900 K",
        "M900 KNaN", "M900 Kinf", "M900 K0.02 K0.03", "M900 K0.02L1000", "M900 K2e-2",
    ] {
        refused(&file(&[], GOOD_START, &format!("{command}\n{}", square(0.2, 0.4))), "pressure advance");
    }
}

#[test]
fn moves_below_the_plate_are_refused_in_every_frame() {
    refused(&file(&[], GOOD_START, &square(0.2, 0.4).replace("G1 Z0.2\n", "G1 Z-0.1\n")), "below the plate");
    // Relative mode: G91 Z-1 from 0.2 ends below the plate.
    refused(&file(&[], GOOD_START, &format!("{}G91\nG1 Z-1\nG90\n", square(0.2, 0.4))), "below the plate");
    // G92 Z5 at 0.4 makes Z0 mean 4.6 mm below the plate.
    let reset = format!("{}G92 Z5\nG1 Z0\n", square(0.2, 0.4));
    refused(&file(&[], GOOD_START, &reset), "position reset");
    refused(&file(&[], GOOD_START, &reset), "below the plate");
    refused(&file(&[], GOOD_START, &square(0.05, 0.25)), "below the plate");
    // A placeholder value of the installed change sequence is checked too.
    refused(&file(&[], GOOD_START, &format!("{}M620 S1A\nG1 Z-2 F1200\nT1\nM621 S1A\n", square(0.2, 0.4))), "below the plate");
    // G92 E is fine.
    check(&file(&[], GOOD_START, &format!("G92 E0\n{}", square(0.2, 0.4)))).unwrap();
}

#[test]
fn the_first_layer_band_and_lifts() {
    // These first layers reach outside the 20 mm square: a probe over all of them.
    let wide = GOOD_START.replace("X165 Y150 I20 J20", "X150 Y130 I50 J50");
    let start = wide.as_str();
    refused(&file(&[], start, &square(0.6, 0.8)), "first layer");
    refused(&file(&[], start, &format!("{}G1 Z12\nG1 X170 Y150\n", square(0.2, 0.4))), "travel lift");
    // Travelling across the printed first layer at first-layer height.
    let across = "M73 L1\nG1 X165 Y150 F30000\nG1 Z0.2\nG1 X185 Y150 E1 F3000\nG1 X185 Y152 E0.1\nG1 X165 Y152 E1\nG1 X165 Y154\nG1 X175 Y140\nG1 X176 Y141 E0.1\n";
    refused(&file(&[], start, across), "first-layer travel");
    // A short dry hop along a line (a surface pattern's connector) is not a travel across the part.
    check(&file(&[], start, "M73 L1\nG1 X165 Y150 F30000\nG1 Z0.2\nG1 X185 Y150 E1 F3000\nG1 X185 Y150.4\nG1 X170 Y150.4 E1\nG1 X170 Y149.8\nG1 X185 Y149.8 E1\n")).unwrap();
    // The same travel lifted is fine.
    let lifted = across.replace("G1 X175 Y140\n", "G1 Z0.6\nG1 X175 Y140\nG1 Z0.2\n");
    check(&file(&[], start, &lifted)).unwrap();
    // Across support lines (sacrificial) it may.
    check(&file(&[], start, &format!("; FEATURE: Support\n{across}"))).unwrap();
}

#[test]
fn the_bed_probe_covers_the_first_layer() {
    // Support beside the part, outside the probed 20 x 20 mm: refused; probed: fine.
    let support = format!("; FEATURE: Support\nM73 L1\nG1 X140 Y150 F30000\nG1 Z0.2\nG1 X160 Y150 E1\n; FEATURE: Outer wall\n{}", square(0.2, 0.4).replace("M73 L1\n", "").replace("G1 Z0.2\n", ""));
    refused(&file(&[], GOOD_START, &support), "bed probe");
    check(&file(&[], &GOOD_START.replace("X165 Y150 I20 J20", "X139.5 Y150 I45.5 J20"), &support)).unwrap();
    // A probe smaller than the part itself.
    refused(&file(&[], &GOOD_START.replace("I20 J20", "I10 J10"), &square(0.2, 0.4)), "bed probe");
}

#[test]
fn homing_start_and_foreign_sequences() {
    // No homing in the file's start: it is not the installed start, and nothing is homed.
    let unhomed = GOOD_START.replace("G28\n", "");
    let r = check(&file(&[], &unhomed, &square(0.2, 0.4))).unwrap_err();
    assert!(r.has("not homed") && r.has("foreign sequence"), "{:?}", r.0);
    // A changed literal line of the start sequence.
    refused(&file(&[], &GOOD_START.replace("G1 Z5 F1200", "G1 Z0.5 F1200"), &square(0.2, 0.4)), "foreign sequence");
    // The file's settings carry another start sequence.
    refused(&file(&[("machine_start_gcode", "G28\\nG1 Z0\\n")], GOOD_START, &square(0.2, 0.4)), "foreign sequence");
    // An installed profile without homing: Fab's moves are still refused.
    let mut p = profile();
    p.templates[0].1 = START.replace("G28\n", "");
    let text = file(&[("machine_start_gcode", &esc(&p.templates[0].1))], &unhomed, &square(0.2, 0.4));
    assert!(check_gcode(&text, &p).unwrap_err().has("not homed"));
    // Homing or probing in Fab's own toolpath.
    refused(&file(&[], GOOD_START, &format!("G28\n{}", square(0.2, 0.4))), "command");
    refused(&file(&[], GOOD_START, &format!("{}G29.1 Z-1\n", square(0.2, 0.4))), "command");
    refused(&file(&[], GOOD_START, &format!("{}G20\n", square(0.2, 0.4))), "command");
}

#[test]
fn machine_nozzles_and_volume() {
    refused(&file(&[("extruder_offset", "0x0,2x0")], GOOD_START, &square(0.2, 0.4)), "nozzle offset");
    refused(&file(&[("nozzle_diameter", "0.4,0.6")], GOOD_START, &square(0.2, 0.4)), "nozzle diameter");
    refused(&file(&[("printer_model", "Bambu Lab A1")], GOOD_START, &square(0.2, 0.4)), "machine");
    refused(&file(&[("extruder_printable_area", "0x0,350x0,350x320,0x320#0x0,350x0,350x320,0x320")], GOOD_START, &square(0.2, 0.4)), "build volume");
    // The right nozzle cannot print at X 10 (its area starts at 25); nothing goes past X 350.
    refused(&file(&[], GOOD_START, &square(0.2, 0.4).replace("X165 Y150 F30000", "X10 Y150 F30000")), "outside the build volume");
    refused(&file(&[], GOOD_START, &square(0.2, 0.4).replace("X185 Y170 E1\nG3", "X400 Y170 E1\nG3")), "outside the build volume");
    // An arc whose extremes leave the plate though both ends are on it.
    refused(&file(&[], GOOD_START, &format!("{}G2 X175 Y161 I0 J-165 E1\n", square(0.2, 0.4))), "outside the build volume");
    refused(&file(&[], GOOD_START, &format!("{}M104 S400\n", square(0.2, 0.4))), "temperature");
    refused(&file(&[], GOOD_START, &format!("{}G1 X500Y10\n", square(0.2, 0.4))), "outside the build volume");
    refused(&file(&[], GOOD_START, &format!("{}G1 x150 y150\n", square(0.2, 0.4))), "words");
    refused(&file(&[], GOOD_START, &format!("{}G1 ZNaN\n", square(0.2, 0.4))), "words");
    refused(&file(&[], GOOD_START, &format!("{}M104 SNaN\n", square(0.2, 0.4))), "words");
    refused(&file(&[], GOOD_START, &format!("{}G1 X170 Y160 Z0.4 Z-5\n", square(0.2, 0.4))), "words");
    refused(&file(&[], GOOD_START, &format!("{}G2 I200 J0\n", square(0.2, 0.4))), "outside the build volume");
    refused(&file(&[], GOOD_START, &format!("{}M221 S999\n", square(0.2, 0.4))), "command");
    refused(&file(&[], GOOD_START, &format!("{}M104 S260\n", square(0.2, 0.4))), "temperature");
    refused(&file(&[], GOOD_START, &format!("{}G4 S100000\n", square(0.2, 0.4))), "limit");
    // The end sequence is required and nothing moves after it.
    let text = file(&[], GOOD_START, &square(0.2, 0.4));
    refused(&text.replace("; EXECUTABLE_BLOCK_END", "M104 S250\n; EXECUTABLE_BLOCK_END"), "foreign sequence");
    refused(&text.replace("M400\nG91\nG1 Z2 F600\nG90\nM104 S0\n", "M104 S0\n"), "foreign sequence");
}

/// A first layer (an outer wall square 160..200 x 140..180 at Z 0.2), then at Z 0.4 the
/// square's three walls (0.4 mm apart), the head at `at`, then `body`: for the arc and
/// crossing rules.
fn layer_then(at: &str, body: &str) -> String {
    let e = 40.0 * paths::bead_e_per_mm(0.42, 0.2, 1.75);
    let lp = |o: f64| format!("G1 X{} Y{} F30000\nG1 X{} Y{} E{e:.5}\nG1 X{} Y{} E{e:.5}\nG1 X{} Y{} E{e:.5}\nG1 X{} Y{} E{e:.5}\n", 160.0 + o, 140.0 + o, 200.0 - o, 140.0 + o, 200.0 - o, 180.0 - o, 160.0 + o, 180.0 - o, 160.0 + o, 140.0 + o);
    format!("M73 L1\n; FEATURE: Outer wall\n; LINE_WIDTH: 0.42\n; LAYER_HEIGHT: 0.2\nG1 X160 Y140 F30000\nG1 Z0.2\n{}G1 Z0.4\nM73 L2\n{}; FEATURE: Inner wall\n{}{}; FEATURE: Outer wall\nG1 {at}\n{body}", lp(0.0), lp(0.0), lp(0.4), lp(0.8))
}

fn wide_probe() -> String { GOOD_START.replace("X165 Y150 I20 J20", "X150 Y130 I60 J60") }

/// The exact arc from FAB5's tree-support slice (layer 59): a 4° clockwise run written
/// counter-clockwise, which sweeps 356° round a 12.7 mm circle with a 4° run's filament.
#[test]
fn the_wrong_way_arc_is_refused() {
    let start = wide_probe();
    let arc = "; FEATURE: Support\n; LINE_WIDTH: 0.42\n; LAYER_HEIGHT: 0.2\nG1 X186.617 Y148.845 F30000\nG1 X186.971 Y149.677 E.02776\nG3 X186.755 Y150.554 I12.247 J3.482 E.02806\n";
    let text = file(&[], &start, &layer_then("X186.617 Y148.845", arc));
    let r = check(&text).unwrap_err();
    assert!(r.has("arc flow"), "{:?}", r.0);
    // Through the square's walls at their height: the crossing rule finds it too.
    assert!(r.has("crossing"), "{:?}", r.0);
    // The same 4° written the right way round (G2, centre on the other side) passes.
    let right = arc.replace("G3 X186.755", "G2 X186.755");
    check(&file(&[], &start, &layer_then("X186.617 Y148.845", &right))).unwrap();
}

#[test]
fn arcs_are_simulated_exactly() {
    let start = wide_probe();
    let tags = "; FEATURE: Sparse infill\n; LINE_WIDTH: 0.45\n; LAYER_HEIGHT: 0.2\n";
    // A 90° arc of r 5 inside the square with its bead's filament: fine.
    let ok = format!("{tags}G3 X175 Y165 I-5 J0 E{:.5}\n", 0.25 * std::f64::consts::TAU * 5.0 * paths::bead_e_per_mm(0.45, 0.2, 1.75));
    check(&file(&[], &start, &layer_then("X180 Y160", &ok))).unwrap();
    // 300° of infill: no infill pattern turns that far in one arc.
    let e = 300f64.to_radians() * 5.0 * paths::bead_e_per_mm(0.45, 0.2, 1.75);
    let far = format!("{tags}G3 X177.5 Y155.67 I-5 J0 E{e:.5}\n");
    refused(&file(&[], &start, &layer_then("X180 Y160", &far)), "arc sweep");
    // The same sweep as a wall loop is a hole's perimeter: allowed.
    check(&file(&[], &start, &layer_then("X180 Y160", &far.replace("Sparse infill", "Inner wall")))).unwrap();
    // A sweep with a fraction of its filament (travel-like while extruding), or far too much.
    refused(&file(&[], &start, &layer_then("X180 Y160", &ok.replace("E0.", "E0.0"))), "arc flow");
    refused(&file(&[], &start, &layer_then("X180 Y160", &format!("{tags}G3 X175 Y165 I-5 J0 E0.9\n"))), "arc flow");
    // 240° with the 120° short way's filament (half the bead over the sweep: inside the flow band's
    // edge, so only the direction rule tells): the wrong letter.
    let e200 = 120f64.to_radians() * 5.0 * paths::bead_e_per_mm(0.45, 0.2, 1.75) * 1.05;
    let wrong = format!("{tags}G3 X172.5 Y155.67 I-5 J0 E{e200:.5}\n");
    refused(&file(&[], &start, &layer_then("X180 Y160", &wrong)), "arc direction");
    // The same arc with the long way's filament is a real 240° arc of a loop.
    let e200l = 240f64.to_radians() * 5.0 * paths::bead_e_per_mm(0.45, 0.2, 1.75);
    check(&file(&[], &start, &layer_then("X180 Y160", &wrong.replace(&format!("E{e200:.5}"), &format!("E{e200l:.5}")).replace("Sparse infill", "Inner wall")))).unwrap();
    // An extruding arc with no width or height tags is not trusted.
    refused(&file(&[], &start, "M73 L1\nG1 X180 Y160 F30000\nG1 Z0.2\nG3 X175 Y165 I-5 J0 E0.26\n"), "arc flow");
    // The end off the start's radius; a radius-form arc; no centre.
    refused(&file(&[], &start, &layer_then("X180 Y160", &format!("{tags}G3 X175 Y165.5 I-5 J0 E0.1\n"))), "arc geometry");
    refused(&file(&[], &start, &layer_then("X180 Y160", &format!("{tags}G3 X175 Y165 R5 E0.1\n"))), "arc geometry");
    refused(&file(&[], &start, &layer_then("X180 Y160", &format!("{tags}G3 X175 Y165 E0.1\n"))), "arc geometry");
    // The swept bounds, not the ends: a full-circle travel lift whose circle leaves the plate.
    check(&file(&[], &start, &layer_then("X180 Y160", "G3 Z0.6 I1.2 J0 P1 F60000\n"))).unwrap();
    refused(&file(&[], &start, &layer_then("X180 Y160", "G1 X349 Y160\nG3 Z0.6 I1.2 J0 P1 F60000\n")), "outside the build volume");
    // Absolute extrusion (M82): the flow is the E difference.
    let abs = format!("M82\nG92 E10\n{tags}G3 X175 Y165 I-5 J0 E{:.5}\nM83\n", 10.0 + 0.25 * std::f64::consts::TAU * 5.0 * paths::bead_e_per_mm(0.45, 0.2, 1.75));
    check(&file(&[], &start, &layer_then("X180 Y160", &abs))).unwrap();
    refused(&file(&[], &start, &layer_then("X180 Y160", &abs.replace("G92 E10", "G92 E5"))), "arc flow");
}

#[test]
fn extrusions_may_not_plough_through_the_layer() {
    let start = wide_probe();
    let e = |l: f64| format!("{:.5}", l * paths::bead_e_per_mm(0.42, 0.2, 1.75));
    // Solid infill straight across the square's walls and out the other side.
    let across = format!("; FEATURE: Internal solid infill\nG1 X150 Y160 E{}\n", e(30.0));
    refused(&file(&[], &start, &layer_then("X210 Y160", &across)), "crossing");
    // Infill inside the square, its ends into the walls: fine.
    let mut inside = String::from("; FEATURE: Sparse infill\nG1 X198.9 Y160 F30000\n");
    for k in 0..4 {
        let (y, x) = (160 + k, if k % 2 == 0 { 161.1 } else { 198.9 });
        inside.push_str(&format!("G1 X{x} Y{y} E{}\nG1 X{x} Y{} E{}\n", e(37.8), y + 1, e(1.0)));
    }
    check(&file(&[], &start, &layer_then("X180 Y160", &inside))).unwrap();
    // A grid pattern crosses itself: fine.
    let grid = format!("{inside}G1 X180 Y178.9 F30000\nG1 X180 Y141.1 E{}\n", e(37.8));
    check(&file(&[], &start, &layer_then("X180 Y160", &grid))).unwrap();
    // A wall loop across infill of the same layer is not.
    let wall = format!("{inside}; FEATURE: Inner wall\nG1 X170 Y150 F30000\nG1 X170 Y170 E{}\n", e(20.0));
    refused(&file(&[], &start, &layer_then("X180 Y160", &wall)), "crossing");
    // One layer up it is a new layer.
    let up = format!("G1 Z0.6\n; FEATURE: Internal solid infill\nG1 X150 Y160 E{}\n", e(30.0));
    check(&file(&[], &start, &layer_then("X210 Y160", &up))).unwrap();
    // A path printed a second time on top of itself (FAB-01's doubled floating-shell loop).
    let twice = format!("; FEATURE: Floating vertical shell\nG1 X170 Y150 F30000\nG1 X190 Y150 E{0}\nG1 X190 Y170 E{0}\nG1 X170 Y170 F30000\n; FEATURE: Internal solid infill\nG1 X190.01 Y170 F30000\nG1 X190.01 Y150 E{0}\n", e(20.0));
    refused(&file(&[], &start, &layer_then("X180 Y160", &twice)), "crossing");
    // Side by side, half a width apart (a thin wall's overlap): fine.
    check(&file(&[], &start, &layer_then("X180 Y160", &twice.replace("X190.01", "X189.8")))).unwrap();
    // Ironing goes over the surface it irons.
    let iron = format!("; FEATURE: Ironing\nG1 X150 Y160 E{}\n", e(3.0));
    check(&file(&[], &start, &layer_then("X210 Y160", &iron))).unwrap();

    // A thin wall turns gently alongside two steps in an earlier wall.
    let bends = format!(
        "G1 X170 Y156 E{four} F1200\nG1 X170 Y158 E{two}\nG1 X174 Y158 E{four}\nG1 X174 Y156 E{two}\n\
         G1 X178 Y156 E{four}\nG1 X178 Y158 E{two}\nG1 X182 Y158 E{four}\nG1 X182 Y156 E{two}\n\
         G1 X186 Y156 E{four}\nG1 X166 Y158.1 F30000\n; FEATURE: Inner wall\n\
         G1 X178 Y158.15 E{twelve} F1200\nG1 X190 Y158.1 E{twelve}\n",
        four = e(4.0), two = e(2.0), twelve = e(12.0_f64.hypot(0.05)),
    );
    check(&file(&[], &start, &layer_then("X166 Y156", &bends))).unwrap();
    // Solid infill crosses three interior wall joints on the diagonal y = x - 20.
    let diagonal = format!("; FEATURE: Internal solid infill\nG1 X194 Y174 E{} F1200\n", e(10.0_f64.hypot(10.0)));
    refused(&file(&[], &start, &layer_then("X204 Y184", &diagonal)), "crossing");
}

fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = makepad_zip_file::ZipWriter::new();
    for (n, d) in members { z.add(n, d, makepad_zip_file::ZipMethod::Deflate).unwrap(); }
    z.finish().unwrap()
}

/// A job through the check itself (tests of upload and start use it; nothing bypasses `validate`).
pub(crate) fn checked_square(name: &str) -> ValidatedJob {
    let g = file(&[], GOOD_START, &square(0.2, 0.4));
    let md5: String = makepad_network::digest::md5_hash(g.as_bytes()).iter().map(|b| format!("{b:02X}")).collect();
    validate(zip(&[("Metadata/plate_1.gcode", g.as_bytes()), ("Metadata/plate_1.gcode.md5", md5.as_bytes())]), &profile(), name).expect("the square passes")
}

#[test]
fn a_validated_job_is_the_exact_checked_package() {
    let g = file(&[], GOOD_START, &square(0.2, 0.4));
    let md5: String = makepad_network::digest::md5_hash(g.as_bytes()).iter().map(|b| format!("{b:02X}")).collect();
    let package = zip(&[("Metadata/plate_1.gcode", g.as_bytes()), ("Metadata/plate_1.gcode.md5", md5.as_bytes())]);
    let job = validate(package.clone(), &profile(), "square").unwrap();
    assert_eq!(job.bytes(), &package[..]);
    assert_eq!(job.md5(), makepad_network::digest::md5_hash(&package).iter().map(|b| format!("{b:02X}")).collect::<String>());
    assert!(job.intact());
    assert!(validate(zip(&[("Metadata/plate_1.gcode", g.as_bytes()), ("Metadata/plate_1.gcode.md5", b"00")]), &profile(), "x").unwrap_err().has("package"));
    assert!(validate(zip(&[("Metadata/plate_1.gcode", g.as_bytes()), ("Metadata/plate_2.gcode", g.as_bytes())]), &profile(), "x").is_err());
    assert!(validate(b"not a zip".to_vec(), &profile(), "x").is_err());
}

#[test]
fn non_planar_extrusion_fails_closed() {
    let wide = GOOD_START.replace("X165 Y150 I20 J20", "X150 Y130 I50 J50");
    let start = wide.as_str();
    let base = square(0.2, 0.4);
    // A sloped extrusion steeper than a ramp (1.2 mm over 10 mm, 6.8 deg).
    refused(&file(&[], start, &format!("{base}G1 X185 Y160 Z1.6 E1\n")), "non-planar extrusion");
    // Falling Z while extruding, in relative positioning.
    refused(&file(&[], start, &format!("{base}G91\nG1 X2 Z-0.1 E0.1\nG90\n")), "non-planar extrusion");
    // A helical extruding arc, and an extruding arc with a Z word even at the same height.
    refused(&file(&[], start, &format!("{base}G2 X185 Y160 Z0.6 I5 J5 E0.5\n")), "non-planar extrusion");
    refused(&file(&[], start, &format!("{base}G2 X185 Y160 Z0.4 I5 J5 E0.5\n")), "non-planar extrusion");
    // Pulling filament while Z moves is refused too (fail closed on any E).
    refused(&file(&[], start, &format!("{base}G1 Z0.8 E-0.8\n")), "non-planar extrusion");
    // Planar extrusion with a repeated Z word, dry lifts and a dry helical lift stay fine.
    check(&file(&[], start, &format!("{base}G1 X185 Y160 Z0.4 E1\nG1 Z0.8\nG1 X170 Y160 Z0.4\nG17\nG3 Z0.8 I1 J0 P1 F1200\n"))).unwrap();
}

fn head_profile(head: Option<super::nonplanar::HeadEnvelope>) -> MachineProfile { MachineProfile { head, ..profile() } }

/// The placeholder head, marked measured (for the rules only; it is not a real measurement).
fn test_head() -> super::nonplanar::HeadEnvelope {
    let mut h = super::nonplanar::HeadEnvelope::placeholder_two_nozzle();
    h.measured = true;
    h.version = "test-head".into();
    h.z_speed = 30.0;
    h.z_jerk = 3.0;
    h.max_rise = 3.0;
    h
}

fn refused_with(p: &MachineProfile, text: &str, rule: &str) {
    match check_gcode(text, p) {
        Ok(_) => panic!("accepted a file that breaks {rule}"),
        Err(r) => assert!(r.has(rule), "expected {rule}, got {:?}", r.0),
    }
}

#[test]
fn the_head_envelope_schema() {
    use super::nonplanar::HeadEnvelope;
    let p = HeadEnvelope::placeholder_two_nozzle();
    assert!(p.usable().is_err(), "a placeholder is never usable");
    assert!(test_head().usable().is_ok());
    let mut steep = test_head();
    steep.max_slope_deg = 25.0;
    assert!(steep.usable().is_err(), "the v1 slope cap is at most 20 deg");
    let mut low = test_head();
    low.parts[0][0].e = 0.1;
    assert!(low.usable().is_err(), "a part below its margin is not a head");
    let text = "version = fixture-2026-11\nmeasured = true\nmax_slope_deg = 15\nmax_rise = 2\nrise_half = 15\nz_speed = 30\nz_jerk = 3\npart = 0 cone 0 0 2 2 1.2 0.3\npart = 1 cone 0 0 2 2 1.2 0.3\n";
    let h = HeadEnvelope::parse(text).unwrap();
    assert_eq!((h.parts.len(), h.parts[1][0].e, h.max_slope_deg), (2, 1.2, 15.0));
    assert!(h.usable().is_ok());
    assert!(HeadEnvelope::parse("version = x\nwhat = 1\n").is_err());
    // A ring part with its hole (the 9th field), and the generated cone rings around a soft tip zone.
    let ring = HeadEnvelope::parse("version = r\npart = 0 cone1 0 0 1.4 1.4 0.69 0.2 1.1\n").unwrap();
    assert_eq!(ring.parts[0][0].hole, 1.1);
    let cone = super::nonplanar::cone_rings(0.5, 40.0, 4.0, 0.0, 0.0);
    assert_eq!(cone.len(), 7);
    assert!((cone[0].hole - 1.1).abs() < 1e-9 && cone.windows(2).all(|w| w[1].hole == w[0].hx && w[1].e > w[0].e));
}

#[test]
fn non_planar_moves_with_a_measured_head() {
    let wide = GOOD_START.replace("X165 Y150 I20 J20", "X150 Y130 I50 J50");
    let start = wide.as_str();
    let base = square(0.2, 0.4);
    let measured = head_profile(Some(test_head()));
    // A 6.8 deg slope (steeper than a monotone ramp) passes with a measured head, and the facts say so.
    let ramp = format!("{base}G1 X165 Y150 F1200\nG1 X170 Y150 Z1.0 E0.25\nG1 X185 Y170 E1.2\n");
    let facts = check_gcode(&file(&[], start, &ramp), &measured).unwrap();
    assert_eq!((facts.nonplanar_moves, facts.ramp_moves, facts.head_version.as_deref()), (1, 0, Some("test-head")));
    assert!(facts.max_pitch_deg > 6.7 && facts.max_pitch_deg < 6.9);
    assert!(facts.min_head_clearance.is_some_and(|c| c >= 0.5));
    // Down again right after the climb: the tip runs over its own fresh trail, which is not a collision.
    let down = format!("{base}G1 X165 Y150 F600\nG1 X170 Y150 Z1.0 E0.25\nG1 X175 Y150 Z0.4 E0.25\n");
    let facts = check_gcode(&file(&[], start, &down), &measured).unwrap();
    assert_eq!(facts.nonplanar_moves, 2);
    // The same ramp: no head, or an unmeasured one, fails closed.
    refused_with(&head_profile(None), &file(&[], start, &ramp), "non-planar extrusion");
    refused_with(&head_profile(Some(super::nonplanar::HeadEnvelope::placeholder_two_nozzle())), &file(&[], start, &ramp), "non-planar extrusion");
    // A planar file with a measured head has no non-planar facts.
    let planar = check_gcode(&file(&[], start, &base), &measured).unwrap();
    assert_eq!((planar.nonplanar_moves, planar.head_version), (0, None));
    // Slope cap: 0.4 mm over 1 mm is 21.8 deg.
    refused_with(&measured, &file(&[], start, &format!("{base}G1 X165 Y150 F1200\nG1 X166 Y150 Z0.8 E0.1\n")), "non-planar slope");
    // Z speed: 11.3 deg at 500 mm/s moves Z at 98 mm/s.
    refused_with(&measured, &file(&[], start, &format!("{base}G1 X165 Y150 F30000\nG1 X166 Y150 Z0.6 E0.1\n")), "Z speed");
    // Z jerk: up then down at 100 mm/s.
    refused_with(&measured, &file(&[], start, &format!("{base}G1 X165 Y150 F6000\nG1 X170 Y150 Z0.6 E1\nG1 X175 Y150 Z0.4 E1\n")), "Z jerk");
    // Straight moves only: an extruding arc with Z stays refused.
    refused_with(&measured, &file(&[], start, &format!("{base}G2 X185 Y160 Z0.6 I5 J5 E0.5\n")), "non-planar extrusion");
    // Head clearance: a 6 mm wall 10 mm from a low ramp (the heater block reaches it).
    let wall = format!("{base}G1 Z6 F1200\nG1 X190 Y150\nG1 X190 Y170 E1\nG1 X175 Y155\nG1 Z0.6\nG1 X180 Y155 Z0.7 E0.2\n");
    refused_with(&measured, &file(&[], start, &wall), "head clearance");
    // Rise cap: a 2.6 mm wall 14-15 mm away clears every part but rises 2 mm over the tip.
    let mut tight = test_head();
    tight.max_rise = 1.0;
    let rise = format!("{base}G1 Z2.6 F1200\nG1 X190 Y150\nG1 X190 Y170 E1\nG1 X175 Y160\nG1 Z0.6\nG1 X176 Y160 Z0.62 E0.1\n");
    refused_with(&head_profile(Some(tight)), &file(&[], start, &rise), "non-planar rise");
    match check_gcode(&file(&[], start, &rise), &measured) {
        Ok(_) => {}
        Err(r) => panic!("the 3 mm rise cap should pass this: {:?}", r.0),
    }
}

#[test]
fn monotone_ramps_without_head_geometry() {
    let wide = GOOD_START.replace("X165 Y150 I20 J20", "X150 Y130 I50 J50");
    let start = wide.as_str();
    let base = square(0.2, 0.4);
    // A scarf seam: starts 0.2 mm below the layer printed so far, climbs 0.2 over 4 mm (2.9 deg).
    let scarf = format!("{base}G1 X165 Y176 F1200\nG1 Z0.2\nG1 X169 Y176 Z0.4 E0.1\nG1 X175 Y176 E0.2\n");
    let facts = check(&file(&[], start, &scarf)).unwrap();
    assert_eq!((facts.ramp_moves, facts.nonplanar_moves), (1, 0));
    // A vase revolution rising from the top of the print.
    check(&file(&[], start, &format!("{base}G1 X165 Y150 F3000\nG1 X185 Y150 Z0.45 E0.6\nG1 X185 Y170 Z0.5 E0.6\n"))).unwrap();
    // Too steep (0.2 over 1 mm), Z drops, starts too far below, arcs, pulls: all refused without a head.
    refused(&file(&[], start, &format!("{base}G1 X165 Y150 F1200\nG1 X166 Y150 Z0.6 E0.03\n")), "non-planar extrusion");
    refused(&file(&[], start, &format!("{base}G1 X165 Y150 F1200\nG1 X175 Y150 Z0.3 E0.3\n")), "non-planar extrusion");
    refused(&file(&[], start, &format!("{base}G1 X165 Y150 F1200\nG1 Z1\nG1 X166 Y150 E0.05\nG1 Z0.6\nG1 X176 Y150 Z0.65 E0.3\n")), "non-planar extrusion");
    refused(&file(&[], start, &format!("{base}G1 X165 Y150 F1200\nG1 X175 Y150 Z0.6 E-0.3\n")), "non-planar extrusion");
    // Z speed: 2.9 deg at 1200 mm/s moves Z at 60 mm/s.
    refused(&file(&[], start, &format!("{base}G1 X165 Y150 F72000\nG1 X169 Y150 Z0.6 E0.1\n")), "non-planar extrusion");
    // No machine Z speed in the profile: ramps are refused.
    let p = MachineProfile { max_speed_z: None, ..profile() };
    refused_with(&p, &file(&[], start, &scarf), "non-planar extrusion");
}

#[test]
fn flow_sanity() {
    let wide = GOOD_START.replace("X165 Y150 I20 J20", "X150 Y130 I50 J50");
    let start = wide.as_str();
    let base = square(0.2, 0.4);
    // The square's lines: 1 mm of filament over 20 mm is 0.12 mm2, fine; ironing-thin flow is fine too.
    check(&file(&[], start, &format!("{base}G1 X165 Y176 F1200\nG1 X185 Y176 E0.01\n"))).unwrap();
    // 4 mm of filament over 10 mm is 0.96 mm2 > 3 d^2 = 0.48.
    refused(&file(&[], start, &format!("{base}G1 X165 Y150 F1200\nG1 X175 Y150 E4\n")), "flow");
    // Volumetric: 0.24 mm2 at 500 mm/s is 120 mm3/s > 2.5 x 25.
    refused(&file(&[], start, &format!("{base}G1 X165 Y150 F30000\nG1 X185 Y150 E2\n")), "flow");
    // In place: a 0.8 mm unretract is fine, 20 mm is not.
    check(&file(&[], start, &format!("{base}G1 E-0.8 F1800\nG1 E0.8\n"))).unwrap();
    refused(&file(&[], start, &format!("{base}G1 E20 F1800\n")), "flow");
    // No filament diameter in the profile: extrusion is refused.
    let p = MachineProfile { filament_diameters: vec![], ..profile() };
    refused_with(&p, &file(&[], start, &base), "flow");
}

/// Nothing outside the executable block is checked, so nothing there may run; and a firmware may
/// take a lone CR or another control character for a line end, which the check would not see.
/// The print's pause for items placed into their parts, as Fab writes it: parked over the print,
/// the printer's own pause, the heaters back, then down onto the next layer. Only that pause.
#[test]
fn the_pause_for_placed_items_is_the_printers_own() {
    let resume = "M190 S55\nM109 S220\nG1 Z2.4 F1200\nG1 X165 Y150 F30000\nM73 L3\nG1 Z0.6\nG1 X185 Y150 E1 F3000\n";
    let body = |pause: &str| format!("{}G1 Z2.4 F1200\nG1 X200 Y200 F30000\n; FAB_PAUSE: layer 3 z 0.6: place M3 nut\n{pause}\n{resume}", square(0.2, 0.4));
    check(&file(&[], GOOD_START, &body("M400 U1"))).unwrap_or_else(|r| panic!("{}", r.message()));
    refused(&file(&[], GOOD_START, &body("M400 U2")), "words");
    // The second family: its PAUSE macro, no words.
    let at = "EXCLUDE_OBJECT_END NAME=part_0\n";
    let named = |pause: &str| named_file().replacen(at, &format!("{at}G1 Z2.2 F1200\nG1 X200 Y200 F30000\n; FAB_PAUSE: layer 2 z 0.4: place M3 nut\n{pause}\nM190 S55\nM109 S220 T0\n"), 1);
    validate_gcode(named("PAUSE").into_bytes(), &named_profile(), "fixture.gcode").unwrap_or_else(|r| panic!("{}", r.message()));
    named_refused(named("PAUSE X=1"), "words", "PAUSE");
    named_refused(named("M117 Place M3 nut"), "command", "M117");
}

#[test]
fn commands_outside_the_checked_block_are_refused() {
    let good = file(&[], GOOD_START, &square(0.2, 0.4));
    assert!(check_gcode(&good, &profile()).is_ok());
    assert!(check_gcode(&good.replace('\n', "\r\n"), &profile()).is_ok(), "CRLF ends lines like LF");
    let stray = "M104 S400";
    let cases = [
        ("in the header", good.replacen("; HEADER_BLOCK_END", &format!("; filament_density: 1.24\n{stray}\n; HEADER_BLOCK_END"), 1), "outside the block"),
        ("in the settings", file(&[("filament_density", "1.24\nM104 S400")], GOOD_START, &square(0.2, 0.4)), "outside the block"),
        ("after the end", format!("{good}{stray}\n"), "outside the block"),
        ("after a smuggled end", good.replacen("M73 L1\n", &format!("; EXECUTABLE_BLOCK_END\n{stray}\n; EXECUTABLE_BLOCK_START\nM73 L1\n"), 1), "outside the block"),
        ("CR in a header comment", good.replacen("; HEADER_BLOCK_END", &format!("; note\r{stray}\n; HEADER_BLOCK_END"), 1), "control character"),
        ("CR in a block comment", good.replacen("M73 L1\n", &format!("; note\r{stray}\nM73 L1\n"), 1), "control character"),
        ("form feed", good.replacen("M73 L1\n", &format!("; note\x0c{stray}\nM73 L1\n"), 1), "control character"),
    ];
    for (name, text, rule) in cases {
        let refusal = check_gcode(&text, &profile()).expect_err(name);
        assert!(refusal.has(rule), "{name}: {}", refusal.message());
    }
}


const NAMED_START: &str = "START_JOB BED={bed_temperature}\n{if heat}M104 S[temp] T0\n{endif}G28\nG90\nM83\n";
const NAMED_END: &str = "END_JOB\n";
const NAMED_CHANGE: &str = "TOOL_CHANGE SLOT={next_filament}\nT[next_filament]\n";
const NAMED_DEFINE: &str = "EXCLUDE_OBJECT_DEFINE NAME=part_0 CENTER=175,160 POLYGON=[[165,150],[185,150],[185,170],[165,170]]";
const NAMED_ACCEL: &str = "SET_VELOCITY_LIMIT ACCEL=1000 ACCEL_TO_DECEL=500";

fn named_profile() -> MachineProfile {
    let mut p = profile();
    p.flavor = Flavor::Named;
    p.printer_model = makepad_fab_settings::family::FAMILIES[1].models[0].into();
    p.templates = vec![
        ("machine_start_gcode".into(), NAMED_START.into()),
        ("machine_end_gcode".into(), NAMED_END.into()),
        ("change_filament_gcode".into(), NAMED_CHANGE.into()),
    ];
    p.filament_diameters = vec![1.75; 4];
    p.max_volumetric = vec![25.0; 4];
    p.filament_types = vec!["PLA".into(); 4];
    p.nozzle_temp_ranges = vec![[190.0, 240.0], [190.0, 250.0], [190.0, 260.0], [190.0, 270.0]];
    p.print_temps = vec![[220.0, 220.0]; 4];
    p.required_hrc = vec![3.0; 4];
    p
}

/// Small fixture files live here so each mutation carries the same complete trailing config.
fn named_file() -> String {
    let config = file(&[
        ("printer_model", makepad_fab_settings::family::FAMILIES[1].models[0]), ("printable_height", "325"),
        ("filament_map", "2,1,2,1"), ("filament_diameter", "1.75,1.75,1.75,1.75"),
        ("machine_start_gcode", &esc(NAMED_START)), ("machine_end_gcode", &esc(NAMED_END)),
        ("change_filament_gcode", &esc(NAMED_CHANGE)), ("layer_change_gcode", ""),
    ], "", "");
    let config = config.split_once("; CONFIG_BLOCK_START\n").unwrap().1.split_once("; CONFIG_BLOCK_END").unwrap().0;
    let mut text = format!("; HEADER_BLOCK_START\n; generated by fixture\n; HEADER_BLOCK_END\n; thumbnail begin 1x1 4\n; AAAA\n; thumbnail end\n; EXECUTABLE_BLOCK_START\n{NAMED_DEFINE}\n{}\nSTART_JOB BED=55\nM104 S220 T0\nG28\nG90\nM83\n{NAMED_ACCEL}\n", NAMED_DEFINE.replace("part_0", "part_1"));
    for f in 0..4 {
        let y = 150 + f * 5;
        if f > 0 { text.push_str(&format!("TOOL_CHANGE SLOT={f}\n")); }
        text.push_str(&format!("T{f}\nM104 S220 T{f}\nG1 Z0.6 F1200\nG1 X165 Y{y} F3000\nG1 Z0.2\nEXCLUDE_OBJECT_START NAME=part_0\nG1 X185 Y{y} E1\nEXCLUDE_OBJECT_END NAME=part_0\n"));
    }
    text.push_str(&format!("END_JOB\n; EXECUTABLE_BLOCK_END\n; filament used [mm] = 12, 13, 14, 15\n; filament used [g] = 0.1, 0.2, 0.3, 0.4\n; CONFIG_BLOCK_START\n{config}; CONFIG_BLOCK_END\n"));
    text
}

fn named_refused(text: String, rule: &str, command: &str) {
    let refusal = validate_gcode(text.into_bytes(), &named_profile(), "fixture.gcode").expect_err(command);
    assert!(refusal.0.iter().any(|v| v.rule == rule && v.detail.contains(command)), "expected {rule}: {command}, got {}", refusal.message());
}

#[test]
fn plain_job_preserves_bytes_and_maps_four_filaments() {
    let p = named_profile();
    let text = named_file();
    let job = validate_gcode(text.as_bytes().to_vec(), &p, "fixture.gcode").unwrap();
    assert_eq!(job.bytes(), text.as_bytes());
    assert_eq!(job.name(), "fixture.gcode");
    assert!(job.intact());
    assert_eq!(job.sha256, makepad_network::digest::sha256_hash(text.as_bytes()));
    assert_eq!(job.md5(), makepad_network::digest::md5_hash(text.as_bytes()).iter().map(|b| format!("{b:02X}")).collect::<String>());
    assert_eq!(job.facts().filaments.iter().map(|f| f.extruder).collect::<Vec<_>>(), vec![1, 0, 1, 0]);
    assert_eq!(job.facts().extruders_used, vec![0, 1]);
    assert_eq!(job.facts().first_layer_z, 0.2);
    // No identity mapping: T0 has the second head's area, T1 the first head's area.
    named_refused(text.replacen("G1 X165 Y150", "G1 X20 Y150", 1), "outside the build volume", "X 20.00");
    named_refused(text.replacen("G1 X185 Y155", "G1 X330 Y155", 1), "outside the build volume", "X 330.00");
}

/// A job is checked as its family sends it, and keeps Fab's name: the second family's under
/// the file name it is uploaded as.
#[test]
fn a_job_is_checked_as_its_family_sends_it() {
    let plain = named_file().into_bytes();
    let job = validate_job(plain.clone(), &named_profile(), "My part (2)").unwrap();
    assert_eq!((job.name(), job.bytes()), ("My part (2)", &plain[..]));
    assert_eq!(crate::job::plain_file_name(job.name()), "My-part-2.gcode");
    assert!(validate_job(plain.clone(), &profile(), "fixture").unwrap_err().has("package"));
    assert!(validate_job(zip(&[("Metadata/plate_1.gcode", &plain)]), &named_profile(), "fixture").is_err());
}

#[test]
fn families_require_their_own_file_form() {
    let plain = named_file().into_bytes();
    let package = zip(&[("Metadata/plate_1.gcode", &plain)]);
    let p = named_profile();
    for bytes in [plain.clone(), package.clone()] {
        assert!(validate(bytes, &p, "fixture.gcode.3mf").unwrap_err().has("family format"));
    }
    assert!(validate(plain.clone(), &profile(), "fixture.gcode").unwrap_err().has("package"));
    assert!(validate_gcode(plain.clone(), &profile(), "fixture.gcode").unwrap_err().has("family format"));
    assert!(validate_gcode(package, &p, "fixture.gcode").is_err());
    assert!(validate_gcode(plain, &p, "fixture.gcode.3mf").unwrap_err().has("file name"));
    assert!(validate_gcode(vec![0xff], &p, "fixture.gcode").unwrap_err().has("G-code text"));
    assert_eq!(MachineProfile::default().flavor, Flavor::Mask);
    // The check's profile is an installed printer's, which names its model.
    assert_eq!(MachineProfile::from_settings(&|_| vec![], vec![]).expect_err("no model").0[0].detail, family::NO_MODEL);
    let model = family::FAMILIES.iter().find(|f| f.flavor == Flavor::Named).unwrap().models[0];
    assert_eq!(MachineProfile::from_settings(&|k| if k == "printer_model" { vec![model.into()] } else { vec![] }, vec![]).expect("claimed fixture").flavor, Flavor::Named);
}

#[test]
fn the_second_familys_speed_factor_guard_has_three_forms() {
    let p = named_profile();
    let good = named_file();
    let at = "SET_VELOCITY_LIMIT ACCEL=1000 ACCEL_TO_DECEL=500\n";
    let with = |lines: &str| good.replacen(at, &format!("{at}{lines}"), 1);
    assert!(good.contains(at));
    validate_gcode(with("M220 B\nM220 S100\nM220 R\n").into_bytes(), &p, "fixture.gcode").unwrap_or_else(|e| panic!("{:?}", e.0));
    for bad in ["M220 S50", "M220 S100 B", "M220", "M220 R1", "M220 B R"] {
        let refusal = validate_gcode(with(&format!("{bad}\n")).into_bytes(), &p, "fixture.gcode").expect_err(bad);
        assert!(refusal.has("words"), "{bad}: {:?}", refusal.0);
    }
    assert!(check(&file(&[], GOOD_START, &format!("M220 S100\n{}", square(0.2, 0.4)))).unwrap_err().has("command"), "not for the default family");
}

/// A template line that is only a placeholder authorizes no line in the default family either:
/// what it renders is checked as Fab's own, so a forbidden command there is refused.
#[test]
fn a_placeholder_alone_on_a_line_authorizes_nothing() {
    let mut p = profile();
    p.templates[0].1 = START.replace("G90\nG1 Z5 F1200\n", "[machine_extra]\nG90\nG1 Z5 F1200\n");
    let start = GOOD_START.replace("G90\nG1 Z5", "M500\nG90\nG1 Z5");
    let text = file(&[("machine_start_gcode", &esc(&p.templates[0].1))], &start, &square(0.2, 0.4));
    match check_gcode(&text, &p) {
        Ok(_) => panic!("a placeholder line passed a forbidden command"),
        Err(r) => assert!(r.0.iter().any(|v| v.detail.contains("M500")), "{:?}", r.0),
    }
}

#[test]
fn heads_without_their_own_areas_reach_the_whole_printable_volume() {
    let named = family::FAMILIES.iter().find(|f| f.flavor == Flavor::Named).unwrap().models[0];
    let settings = |model: &'static str, heights: &'static str| move |k: &str| -> Vec<String> {
        match k {
            "printer_model" => vec![model.into()],
            "printable_area" => vec!["0x0,40x0,40x40,0x40".into()],
            "printable_height" => vec!["40".into()],
            "nozzle_diameter" => vec!["0.4".into(); 3],
            "extruder_printable_height" if !heights.is_empty() => heights.split(',').map(str::to_string).collect(),
            _ => vec![],
        }
    };
    let area = parse_points("0x0,40x0,40x40,0x40");
    for heights in ["", "0"] {
        let p = MachineProfile::from_settings(&settings(named, heights), vec![]).expect("claimed fixture");
        assert_eq!((p.extruder_areas, p.extruder_heights), (vec![area.clone(); 3], vec![40.0; 3]), "heights {heights:?}");
    }
    // A stated height, or the default family, keeps the lists as they are.
    let p = MachineProfile::from_settings(&settings(named, "30,30,30"), vec![]).expect("claimed fixture");
    assert_eq!((p.extruder_areas.len(), p.extruder_heights), (0, vec![30.0; 3]));
    let first = family::FAMILIES.iter().find(|f| f.flavor == Flavor::Mask).unwrap().models[0];
    let p = MachineProfile::from_settings(&settings(first, "0"), vec![]).expect("claimed fixture");
    assert_eq!((p.extruder_areas.len(), p.extruder_heights), (0, vec![0.0]));
    // A single-nozzle printer of the default family (a P1S, X1C, A1 or H2S: its installed profile
    // leaves the per-nozzle keys at their defaults, no areas and height 0) reaches the whole volume.
    let one = |k: &str| if k == "nozzle_diameter" { vec!["0.4".to_string()] } else { settings(first, "0")(k) };
    let p = MachineProfile::from_settings(&one, vec![]).expect("claimed fixture");
    assert_eq!((p.extruder_areas, p.extruder_heights), (vec![area.clone()], vec![40.0]), "a single nozzle of the default family reaches the whole bed");
}

#[test]
fn named_acceleration_has_strict_words_and_bounds() {
    let text = named_file();
    for (args, rule) in [
        ("ACCEL=20001", "acceleration"), ("ACCEL=1000 ACCEL_TO_DECEL=1001", "acceleration to deceleration"),
        ("ACCEL=1000 VELOCITY=1", "words"), ("ACCEL=1000 SQUARE_CORNER_VELOCITY=1", "words"),
        ("ACCEL=1000 UNKNOWN=1", "words"), ("ACCEL=1000 ACCEL=999", "words"),
        ("ACCEL=1000 ACCEL_TO_DECEL=500 ACCEL_TO_DECEL=500", "words"),
        ("ACCEL=bad", "acceleration"), ("ACCEL=nan", "acceleration"), ("ACCEL=inf", "acceleration"),
        ("ACCEL=1e3", "acceleration"), ("ACCEL=1000X1", "acceleration"), ("ACCEL=0", "acceleration"),
        ("ACCEL=-1", "acceleration"), ("ACCEL_TO_DECEL=500", "acceleration"),
        ("ACCEL=1000 ACCEL_TO_DECEL=bad", "acceleration to deceleration"),
        ("ACCEL=1000 ACCEL_TO_DECEL=0", "acceleration to deceleration"),
        ("ACCEL=1000 ACCEL_TO_DECEL=-1", "acceleration to deceleration"),
        ("ACCEL=", "words"), ("ACCEL", "words"), ("", "acceleration"),
    ] {
        named_refused(text.replace(NAMED_ACCEL, &format!("SET_VELOCITY_LIMIT {args}")), rule, "SET_VELOCITY_LIMIT");
    }
    for args in ["ACCEL=20000", "ACCEL=1000 ACCEL_TO_DECEL=1000", "ACCEL_TO_DECEL=500 ACCEL=1000"] {
        validate_gcode(text.replace(NAMED_ACCEL, &format!("SET_VELOCITY_LIMIT {args}")).into_bytes(), &named_profile(), "fixture.gcode").unwrap();
    }
    let mut p = named_profile();
    p.limits.move_acceleration = None;
    p.limits.acceleration = [Some(2000.0), Some(1000.0), None, None];
    validate_gcode(text.as_bytes().to_vec(), &p, "fixture.gcode").unwrap();
    assert!(validate_gcode(text.replace(NAMED_ACCEL, "SET_VELOCITY_LIMIT ACCEL=1001").into_bytes(), &p, "fixture.gcode").unwrap_err().has("acceleration"));
    p.limits.acceleration[1] = None;
    assert!(validate_gcode(text.into_bytes(), &p, "fixture.gcode").unwrap_err().has("acceleration"));
}

#[test]
fn named_objects_require_definitions_pairing_and_bed_geometry() {
    let text = named_file();
    let start = "EXCLUDE_OBJECT_START NAME=part_0";
    let end = "EXCLUDE_OBJECT_END NAME=part_0";
    for (bad, rule, command) in [
        (text.replacen(start, "EXCLUDE_OBJECT_START NAME=unknown", 1), "object defined", "EXCLUDE_OBJECT_START"),
        (text.replacen(start, &format!("{start}\nEXCLUDE_OBJECT_START NAME=part_1"), 1), "object nesting", "EXCLUDE_OBJECT_START"),
        (text.replacen(end, "EXCLUDE_OBJECT_END NAME=part_1", 1), "object pairing", "EXCLUDE_OBJECT_END"),
        (text.replace("END_JOB\n; EXECUTABLE", &format!("{start}\nEND_JOB\n; EXECUTABLE")), "object pairing", "EXCLUDE_OBJECT_START"),
        (text.replacen("G1 Z0.6 F1200", &format!("G1 Z0.6 F1200\n{}", NAMED_DEFINE.replace("part_0", "late")), 1), "object definition order", "EXCLUDE_OBJECT_DEFINE"),
        (text.replacen("[185,170]", "[351,170]", 1), "object area", "EXCLUDE_OBJECT_DEFINE"),
        (text.replacen("CENTER=175,160", "CENTER=-1,160", 1), "object area", "EXCLUDE_OBJECT_DEFINE"),
        (text.replacen(NAMED_DEFINE, &format!("{NAMED_DEFINE}\n{NAMED_DEFINE}"), 1), "object name", "EXCLUDE_OBJECT_DEFINE"),
        (text.replacen("NAME=part_0", "NAME=bad/name", 1), "object name", "EXCLUDE_OBJECT_DEFINE"),
        (text.replacen("NAME=part_0", "NAME=part_0 NAME=part_0", 1), "words", "EXCLUDE_OBJECT_DEFINE"),
        (text.replacen("CENTER=175,160", "CENTER=NaN,160", 1), "object geometry", "EXCLUDE_OBJECT_DEFINE"),
        (text.replacen("POLYGON=[[165,150],[185,150],[185,170],[165,170]]", "POLYGON=[[165,150],[185,150]]", 1), "object geometry", "EXCLUDE_OBJECT_DEFINE"),
        (text.replacen(start, &format!("{start} EXTRA=1"), 1), "words", "EXCLUDE_OBJECT_START"),
        (text.replacen(end, &format!("{end} NAME=part_0"), 1), "words", "EXCLUDE_OBJECT_END"),
    ] { named_refused(bad, rule, command); }
    // Transparent feed moves inside a matched template are still toolpath commands.
    let late = text.replacen("G28\n", "G1 F600\nG28\n", 1)
        .replacen(NAMED_ACCEL, &format!("{}\n{NAMED_ACCEL}", NAMED_DEFINE.replace("part_0", "late")), 1);
    named_refused(late, "object definition order", "EXCLUDE_OBJECT_DEFINE");
    validate_gcode(text.replace("part_0", "Ab_09-.").into_bytes(), &named_profile(), "fixture.gcode").unwrap();
}

#[test]
fn named_heaters_and_tools_use_job_filaments() {
    let text = named_file();
    named_refused(text.replace("M104 S220 T3", "M104 S220 T9"), "job filament", "M104");
    named_refused(text.replace("M104 S220 T3", "M104 T9"), "job filament", "M104");
    named_refused(text.replace("M104 S220 T3", "M104 S220 T1.5"), "job filament", "M104");
    named_refused(text.replace("M104 S220 T3", "M104 S220 T-1"), "job filament", "M104");
    named_refused(text.replace("M104 S220 T3", "M104 S220 T3 N0"), "words", "M104");
    named_refused(text.replace("M104 S220 T3", "M104 S220 T3 T3"), "words", "M104");
    named_refused(text.replace("M104 S220 T0", "M104 S250 T0"), "temperature", "M104");
    named_refused(text.replace("T3\n", "T9\n"), "job filament", "T9");
    // T0 and T2 share a head, but T0 may not borrow T2's higher temperature range.
    validate_gcode(text.replace("M104 S220 T2", "M104 S260 T2").into_bytes(), &named_profile(), "fixture.gcode").unwrap();
    named_refused(text.replace("M104 S220 T2", "M104 S261 T2"), "temperature", "M104");
}

#[test]
fn unknown_commands_never_make_a_plain_validated_job() {
    let text = named_file();
    for command in ["FOO_BAR X=1", "M486 S0", "M620 S0A", "G29", "M112", "SAVE_CONFIG", "FIRMWARE_RESTART", "SET_PRESSURE_ADVANCE ADVANCE=0.02 SMOOTH_TIME=1"] {
        let bad = text.replace(NAMED_ACCEL, &format!("{NAMED_ACCEL}\n{command}"));
        let refusal = validate_gcode(bad.into_bytes(), &named_profile(), "fixture.gcode").expect_err(command);
        assert!(refusal.0.iter().any(|v| v.detail.contains(command.split_whitespace().next().unwrap())), "{command}: {}", refusal.message());
    }
    for command in [NAMED_ACCEL, NAMED_DEFINE, "EXCLUDE_OBJECT_START NAME=part_0", "EXCLUDE_OBJECT_END NAME=part_0"] {
        let text = file(&[], GOOD_START, &format!("{command}\n{}", square(0.2, 0.4)));
        let refusal = validate(zip(&[("Metadata/plate_1.gcode", text.as_bytes())]), &profile(), "fixture.gcode.3mf").expect_err(command);
        assert!(refusal.0.iter().any(|v| v.detail.contains(command.split_whitespace().next().unwrap())), "{}", refusal.message());
    }
}

#[test]
fn trailing_settings_and_literal_template_macros_are_checked() {
    let text = named_file();
    for (old, new, rule) in [
        (format!("; printer_model = {}", named_profile().printer_model).as_str(), format!("; printer_model = {}", profile().printer_model).as_str(), "machine"),
        ("; printable_area = 0x0,350x0,350x320,0x320", "; printable_area = 0x0,351x0,350x320,0x320", "build volume"),
        ("; printable_height = 325", "; printable_height = 326", "build volume"),
        ("; extruder_printable_height = 320,325", "; extruder_printable_height = 321,325", "build volume"),
        ("; extruder_printable_area = 0x0,325x0", "; extruder_printable_area = 0x0,326x0", "build volume"),
        ("; extruder_offset = 0x0,0x0", "; extruder_offset = 1x0,0x0", "nozzle offset"),
        ("; nozzle_diameter = 0.4,0.4", "; nozzle_diameter = 0.6,0.4", "nozzle diameter"),
        ("; machine_start_gcode = START_JOB", "; machine_start_gcode = OTHER_START", "foreign sequence"),
        ("; machine_end_gcode = END_JOB", "; machine_end_gcode = OTHER_END", "foreign sequence"),
        ("; change_filament_gcode = TOOL_CHANGE", "; change_filament_gcode = OTHER_CHANGE", "foreign sequence"),
        ("START_JOB BED=55", "OTHER_START BED=55", "foreign sequence"),
        ("END_JOB\n; EXECUTABLE", "OTHER_END\n; EXECUTABLE", "foreign sequence"),
    ] {
        assert!(text.contains(old), "missing mutation {old}");
        let refusal = validate_gcode(text.replace(old, new).into_bytes(), &named_profile(), "fixture.gcode").expect_err(old);
        assert!(refusal.has(rule), "{old}: {}", refusal.message());
    }
    named_refused(text.replace("TOOL_CHANGE SLOT=1", "OTHER_CHANGE SLOT=1"), "words", "OTHER_CHANGE");
    named_refused(text.replace(NAMED_ACCEL, &format!("{NAMED_ACCEL}\nEND_JOB_EXTRA")), "words", "END_JOB_EXTRA");
    // A macro has no known homing semantics: never infer homing from its name.
    let mut p = named_profile();
    p.templates[0].1 = NAMED_START.replace("G28\n", "");
    let no_home = text.replace("G28\n", "").replace("G28\\n", "");
    assert!(validate_gcode(no_home.into_bytes(), &p, "fixture.gcode").unwrap_err().has("not homed"));
}


fn named_template(text: &str, rendered: &str) -> (MachineProfile, String) {
    let mut p = named_profile();
    p.templates.push(("before_layer_change_gcode".into(), text.into()));
    let file = named_file().replace("; CONFIG_BLOCK_END", &format!("; before_layer_change_gcode = {}\n; CONFIG_BLOCK_END", esc(text)))
        .replacen(NAMED_ACCEL, &format!("{NAMED_ACCEL}\n{rendered}"), 1);
    (p, file)
}

fn named_rule(p: &MachineProfile, text: &str, rule: &str) {
    let r = validate_gcode(text.as_bytes().to_vec(), p, "fixture.gcode").expect_err(rule);
    assert!(r.has(rule), "expected {rule}, got {:?}", r.0);
}

/// Shapes the second family's installed templates write: a sum is one value, a head switched
/// off by its physical index, and Z homed at a named point.
#[test]
fn installed_template_shapes_pass_and_stay_bounded() {
    let pass = |template: &str, rendered: &str| {
        let (p, text) = named_template(template, rendered);
        validate_gcode(text.into_bytes(), &p, "fixture.gcode").unwrap_or_else(|e| panic!("{template:?} as {rendered:?}: {:?}", e.0));
    };
    pass("LAYER_REPORT TOTAL={total_layer_count} NOW={layer_num+1}\n", "LAYER_REPORT TOTAL=50 NOW=3");
    pass("{\"LAYER_REPORT NOW=\" + (layer_num + 1) + \"\\n\"}\n", "LAYER_REPORT NOW=31");
    pass("M104 S0 T1 A0\n", "M104 S0 T1 A0");
    pass("G28 Z I140 J140\nG90\n", "G28 Z I140 J140\nG90");
    for (template, rendered, rule) in [
        ("M104 S200 T1 A0\n", "M104 S200 T1 A0", "temperature"),
        ("M104 S0 T2 A0\n", "M104 S0 T2 A0", "words"),
        ("M104 S0 T1 A1\n", "M104 S0 T1 A1", "words"),
        ("G28 Z I400 J140\n", "G28 Z I400 J140", "outside the build volume"),
        ("G28 Z I140\n", "G28 Z I140", "outside the build volume"),
    ] {
        let (p, text) = named_template(template, rendered);
        named_rule(&p, &text, rule);
    }
}

/// The writer's pressure-advance line (a calibration band) is a known command with a bound,
/// in the file's own lines and inside a template alike.
#[test]
fn pressure_advance_is_known_and_bounded() {
    let body = |line: &str| named_file().replacen(NAMED_ACCEL, &format!("{NAMED_ACCEL}\n{line}"), 1);
    validate_gcode(body("SET_PRESSURE_ADVANCE ADVANCE=0.02").into_bytes(), &named_profile(), "fixture.gcode").unwrap_or_else(|e| panic!("{:?}", e.0));
    for (line, rule) in [
        ("SET_PRESSURE_ADVANCE ADVANCE=0.9", "pressure advance"), ("SET_PRESSURE_ADVANCE ADVANCE=abc", "pressure advance"),
        ("SET_PRESSURE_ADVANCE", "pressure advance"), ("SET_PRESSURE_ADVANCE ADVANCE=0.02 EXTRUDER=x", "words"),
    ] {
        named_rule(&named_profile(), &body(line), rule);
    }
    let (p, text) = named_template("SET_PRESSURE_ADVANCE ADVANCE={pa}\n", "SET_PRESSURE_ADVANCE ADVANCE=0.7");
    named_rule(&p, &text, "pressure advance");
}

/// A word capture only from a placeholder with fixed choices, and only one of its words.
#[test]
fn fixed_choice_placeholders_take_only_their_words() {
    let mesh = "BED_MESH_CALIBRATE mesh_min={min[0]},{min[1]} ALGORITHM=[bed_mesh_algo] PROBE_COUNT={count[0]},{count[1]}\n";
    for algo in ["bicubic", "lagrange"] {
        let (p, text) = named_template(mesh, &format!("BED_MESH_CALIBRATE mesh_min=125,125 ALGORITHM={algo} PROBE_COUNT=4,4"));
        validate_gcode(text.into_bytes(), &p, "fixture.gcode").unwrap_or_else(|e| panic!("{algo}: {:?}", e.0));
    }
    let code = "{\"BED_MESH_CALIBRATE ALGORITHM=\" + bed_mesh_algo + \" PROBE_COUNT=\" + n + \"\\n\"}\n";
    let (p, text) = named_template(code, "BED_MESH_CALIBRATE ALGORITHM=lagrange PROBE_COUNT=5");
    validate_gcode(text.into_bytes(), &p, "fixture.gcode").unwrap_or_else(|e| panic!("code block: {:?}", e.0));
    for (template, rendered) in [
        // An unknown word in the choice capture.
        (mesh, "BED_MESH_CALIBRATE mesh_min=125,125 ALGORITHM=spline PROBE_COUNT=4,4"),
        (mesh, "BED_MESH_CALIBRATE mesh_min=125,125 ALGORITHM=Bicubic PROBE_COUNT=4,4"),
        // A word, even a choice's, in any other capture.
        (mesh, "BED_MESH_CALIBRATE mesh_min=bicubic,125 ALGORITHM=bicubic PROBE_COUNT=4,4"),
        ("BED_MESH_CALIBRATE ALGORITHM=[algo]\n", "BED_MESH_CALIBRATE ALGORITHM=bicubic"),
        (code, "BED_MESH_CALIBRATE ALGORITHM=bicubic PROBE_COUNT=lagrange"),
    ] {
        let (p, text) = named_template(template, rendered);
        named_rule(&p, &text, "template capture");
    }
    // A template holding the choice stand-in itself is unusable: the line is Fab's own and refused.
    let marked = "BED_MESH_CALIBRATE ALGORITHM=\u{E000}\n";
    assert!(Template::for_family("fixture", marked, Flavor::Named).is_empty());
    let (p, text) = named_template(marked, "BED_MESH_CALIBRATE ALGORITHM=bicubic");
    assert!(validate_gcode(text.into_bytes(), &p, "fixture.gcode").is_err(), "a literal stand-in authorizes nothing");
}

#[test]
fn template_commands_captures_and_word_counts_are_strict() {
    let text = named_file();
    named_rule(&named_profile(), &text.replace("TOOL_CHANGE SLOT=1\nT1\n", "TOOL_CHANGE SLOT=1\nTEST_UNSAFE\n"), "template command");
    for (value, rule) in [("55 EXTRA=1", "template words"), ("5 5", "template words"), ("55a", "template capture"), ("1e2", "template capture"), ("[55]", "template capture"), ("55=1", "template capture")] {
        named_rule(&named_profile(), &text.replace("START_JOB BED=55", &format!("START_JOB BED={value}")), rule);
    }
    // Rule B: neither bare expression nor bracket placeholder can authorize a command.
    for template in ["{x}\n", "[x]\n"] {
        let (p, text) = named_template(template, "TEST_UNSAFE");
        named_rule(&p, &text, "words");
        let t = Template::for_family("fixture", template, Flavor::Named);
        assert!(t.match_at(&["TEST_UNSAFE"]).is_none());
    }
    let (p, text) = named_template("{x}_UNSAFE\n", "TEST_UNSAFE");
    named_rule(&p, &text, "template command");
    let (p, text) = named_template("{x}T1\n", "TESTT1");
    named_rule(&p, &text, "template command");
    // Adjacent placeholders are one value: `T` and one capture is the tool command, and its
    // handler refuses a head the machine lacks.
    let (p, text) = named_template("T[x][y]\n", "T12");
    named_rule(&p, &text, "job filament");
}

#[test]
fn known_template_commands_keep_all_handlers() {
    for (template, rendered, rule) in [
        ("SET_VELOCITY_LIMIT ACCEL={a}\n", "SET_VELOCITY_LIMIT ACCEL=20001", "acceleration"),
        ("EXCLUDE_OBJECT_START NAME=unknown\n", "EXCLUDE_OBJECT_START NAME=unknown", "object defined"),
        ("EXCLUDE_OBJECT_END NAME=unknown\n", "EXCLUDE_OBJECT_END NAME=unknown", "object defined"),
        ("M104 S400 T1\n", "M104 S400 T1", "temperature"),
        ("M106 S999\n", "M106 S999", "limit"),
        ("G4 S1000\n", "G4 S1000", "limit"),
        ("G92 Z0\n", "G92 Z0", "position reset"),
        ("T9\n", "T9", "job filament"),
    ] {
        let (p, text) = named_template(template, rendered);
        named_rule(&p, &text, rule);
    }
    // Literal lowercase modal state must apply to all subsequent moves.
    let (p, text) = named_template("G1 Z300\ng91\nG1 Z20\nG1 Z20\nG90\n", "G1 Z300\ng91\nG1 Z20\nG1 Z20\nG90");
    named_rule(&p, &text, "above the build volume");
    let text = named_file().replace("SET_VELOCITY_LIMIT ACCEL=1000 ACCEL_TO_DECEL=500", "set_velocity_limit accel=1000 accel_to_decel=500")
        .replace("EXCLUDE_OBJECT_START NAME=part_0", "exclude_object_start name=PART_0")
        .replace("EXCLUDE_OBJECT_END NAME=part_0", "exclude_object_end name=Part_0");
    validate_gcode(text.into_bytes(), &named_profile(), "fixture.gcode").unwrap();
}

#[test]
fn denied_commands_are_refused_in_every_case_and_location() {
    for command in NAMED_DENIED {
        for command in [command.to_string(), command.to_ascii_lowercase()] {
            let (p, text) = named_template(&format!("{command}\n"), &command);
            named_rule(&p, &text, "denied command");
            let text = named_file().replacen(NAMED_ACCEL, &format!("{NAMED_ACCEL}\n{command}"), 1);
            named_rule(&named_profile(), &text, "denied command");
        }
    }
}

#[test]
fn object_definitions_precede_every_other_command_and_fold_names() {
    let text = named_file();
    for command in ["G28", "M106 S0"] {
        named_rule(&named_profile(), &text.replacen(NAMED_DEFINE, &format!("{command}\n{NAMED_DEFINE}"), 1), "object definition order");
    }
    named_rule(&named_profile(), &text.replacen(NAMED_ACCEL, &format!("{}\n{NAMED_ACCEL}", NAMED_DEFINE.replace("part_0", "late")), 1), "object definition order");
    named_rule(&named_profile(), &text.replacen("NAME=part_1", "NAME=PART_0", 1), "object name");
}

#[test]
fn config_and_header_blocks_are_unique_closed_and_unambiguous() {
    let text = named_file();
    for bad in [
        format!("{text}; CONFIG_BLOCK_START\n; printer_model = {}\n; printable_height = 999\n; CONFIG_BLOCK_END\n", profile().printer_model),
        text.replace("; CONFIG_BLOCK_END", &format!("; printer_model = {}\n; CONFIG_BLOCK_END", profile().printer_model)),
        text.replace("; CONFIG_BLOCK_END", "; printable_height = 325\n; CONFIG_BLOCK_END"),
        text.replace("; CONFIG_BLOCK_END", ""),
        text.replace("; CONFIG_BLOCK_START", ""),
    ] { named_rule(&named_profile(), &bad, "config block"); }
    let (body, config) = text.split_once("; CONFIG_BLOCK_START").unwrap();
    named_rule(&named_profile(), &format!("; CONFIG_BLOCK_START{config}{body}"), "config block");
    for bad in [
        format!("{text}; HEADER_BLOCK_START\n; HEADER_BLOCK_END\n"),
        text.replace("; HEADER_BLOCK_END", "; a: 1\n; a: 2\n; HEADER_BLOCK_END"),
        text.replace("; HEADER_BLOCK_END", ""),
    ] { named_rule(&named_profile(), &bad, "header block"); }
}

#[test]
fn plain_names_are_safe_basenames() {
    let p = named_profile();
    let text = named_file();
    for name in ["../a.gcode", "a\\b.gcode", "a\0b.gcode", ".gcode", "\"a\".gcode", "'a'.gcode", "", "a..gcode", "a\ng.gcode"] {
        assert!(validate_gcode(text.as_bytes().to_vec(), &p, name).unwrap_err().has("file name"), "{name:?}");
    }
    let long = format!("{}.gcode", "a".repeat(195));
    assert!(validate_gcode(text.as_bytes().to_vec(), &p, &long).unwrap_err().has("file name"));
    let longest = format!("{}.gcode", "a".repeat(194));
    validate_gcode(text.into_bytes(), &p, &longest).unwrap();
}

#[test]
fn checked_config_and_statistics_are_retained() {
    let text = named_file();
    let p = named_profile();
    let job = validate_gcode(text.as_bytes().to_vec(), &p, "fixture.gcode").unwrap();
    assert_eq!(job.config_value("printer_model"), Some(p.printer_model.as_str()));
    assert_eq!(job.config_value("machine_start_gcode"), Some(esc(NAMED_START).as_str()));
    assert_eq!(job.config_value("missing"), None);
    assert_eq!(job.facts().used_mm, [12.0, 13.0, 14.0, 15.0]);
    assert_eq!(job.facts().used_g, [0.1, 0.2, 0.3, 0.4]);
    for unit in ["mm", "g"] {
        let line = text.lines().find(|l| l.starts_with(&format!("; filament used [{unit}]"))).unwrap();
        named_rule(&p, &text.replace(line, ""), "filament statistics");
        named_rule(&p, &text.replace(line, &format!("{line}\n{line}")), "filament statistics");
        for bad in ["1,2,3", "1,2,3,4,5", "1,-1,0,0", "NaN,0,0,0", "inf,0,0,0", "0,0,0,x"] {
            named_rule(&p, &text.replace(line, &format!("; filament used [{unit}] = {bad}")), "filament statistics");
        }
    }
    let default = checked_square("fixture");
    assert!(default.facts().used_mm.is_empty() && default.facts().used_g.is_empty());
}

#[test]
fn hostile_coordinates_are_refusals_for_both_families() {
    for (axis, command, rule) in [
        ("X", "G1 X100000000000000000000 Y150 E1", "outside the build volume"),
        ("Y", "G1 X185 Y100000000000000000000 E1", "outside the build volume"),
        ("Z", "G1 X185 Y150 Z100000000000000000000 E1", "above the build volume"),
    ] {
        let text = named_file().replacen("G1 X185 Y150 E1", command, 1);
        named_rule(&named_profile(), &text, rule);
        let text = file(&[], GOOD_START, &square(0.2, 0.4).replacen("G1 X185 Y150 E1 F3000", command, 1));
        let r = validate(zip(&[("Metadata/plate_1.gcode", text.as_bytes())]), &profile(), "fixture.gcode.3mf").expect_err(axis);
        assert!(r.has(rule), "{axis}: {:?}", r.0);
    }
    for command in ["G2 I100000000000000000000 J0", "G3 I0 J100000000000000000000"] {
        let text = named_file().replacen("G1 X185 Y150 E1", command, 1);
        named_rule(&named_profile(), &text, "outside the build volume");
        refused(&file(&[], GOOD_START, &square(0.2, 0.4).replacen("G1 X185 Y150 E1 F3000", command, 1)), "outside the build volume");
    }
}

const CODE_CHANGE: &str = r#"; change [previous_extruder] -> [next_extruder]
{
local lift = 1.5;
local fast = 300.0;
if travel_speed < fast then fast = travel_speed; endif
if mode == "a" then
"G90\nG1 Z" + lift + " F600\n";
else
"G91\nG1 Z" + lift + " F600\nG90\n";
endif
"G1 F" + (fast * 60) + "\n";
"M104 S" + temperature[next_extruder] + " T" + next_extruder + "\n";
"M400\n";
"T" + next_extruder + "\n";
"G90\n";
}
"#;

fn code_change_file(relative: bool) -> (MachineProfile, String, String) {
    let mut p = named_profile();
    p.templates[2].1 = CODE_CHANGE.into();
    // A separate four-head fixture, preserving the original four-filament fixture's assertions.
    p.nozzle_diameters = vec![0.4; 4];
    p.extruder_offsets = vec![[0.0, 0.0]; 4];
    p.extruder_areas = vec![p.printable_area.clone(); 4];
    p.extruder_heights = vec![325.0; 4];
    p.physical_extruder_map = vec![0, 1, 2, 3];
    let mut text = named_file()
        .replace(&format!("; change_filament_gcode = {}", esc(NAMED_CHANGE)), &format!("; change_filament_gcode = {}", esc(CODE_CHANGE)))
        .replace("; nozzle_diameter = 0.4,0.4\n", "; nozzle_diameter = 0.4,0.4,0.4,0.4\n")
        .replace("; extruder_offset = 0x0,0x0\n", "; extruder_offset = 0x0,0x0,0x0,0x0\n")
        .replace("; filament_map = 2,1,2,1\n", "; filament_map = 1,2,3,4\n")
        .replace("; extruder_printable_height = 320,325\n", "; extruder_printable_height = 325,325,325,325\n");
    let old = header_config(&text).into_iter().find(|(k, _)| k == "extruder_printable_area").unwrap().1;
    text = text.replace(&format!("; extruder_printable_area = {old}"), &format!("; extruder_printable_area = {}", ["0x0,350x0,350x320,0x320"; 4].join("#")));
    let mut first = String::new();
    for f in 1..4 {
        let branch = if relative { "G91\nG1 Z1.5 F600\nG90\n" } else { "G90\nG1 Z1.5 F600\n" };
        let rendered = format!("{branch}G1 F18000\nM104 S220 T{f}\nM400\nT{f}\nG90\n");
        text = text.replace(&format!("TOOL_CHANGE SLOT={f}\nT{f}\n"), &rendered);
        if f == 1 { first = rendered; }
    }
    (p, text, first)
}

#[test]
fn code_blocks_write_checked_lines_for_both_branches() {
    for relative in [false, true] {
        let (p, text, rendered) = code_change_file(relative);
        validate_gcode(text.as_bytes().to_vec(), &p, "fixture.gcode").unwrap();
        for (old, new, rule) in [
            ("T1\n", "T9\n", "job filament"),
            ("G1 F18000", "G1 Ffast", "template capture"),
            ("M104 S220 T1", "M104 S400 T1", "temperature"),
            ("M400\n", "PURGE_ALL\nM400\n", "words"),
        ] {
            named_rule(&p, &text.replacen(&rendered, &rendered.replace(old, new), 1), rule);
        }
        // A body line cannot disappear into the code block's output.
        let bad = text.replacen(&rendered, "G1 X100000000000000000000 Y150 E1\n", 1);
        named_rule(&p, &bad, "outside the build volume");
        let body = "G1 X185 Y151 E0.1\n";
        let facts = validate_gcode(text.replacen(&rendered, body, 1).into_bytes(), &p, "fixture.gcode").unwrap();
        assert!(facts.facts().moves > 0, "the replacement extrusion is simulated as a body line");
    }
}

#[test]
fn code_block_lexing_output_joining_and_fail_closed_rules() {
    let parse = |s| Template::for_family("fixture", s, Flavor::Named);
    assert!(parse(r#"{"a\\b"}"#).match_at(&[r"a\b"]).is_some());
    assert!(parse(r#"{"x\"y"}"#).match_at(&["x\"y"]).is_some());
    for text in [r#"{"a\qb"}"#, r#"{"unclosed}"#, "{a[1}", "{(a]}", "{é}", "{if x then a;}"] {
        let t = parse(text);
        assert!(t.is_empty() && t.match_at(&["SAVE_CONFIG"]).is_none(), "unusable: {text}");
    }
    let t = parse(r#"G1 X{"1"; "2"} Y2"#);
    assert!(t.match_at(&["G1 X12 Y2"]).is_some() && t.match_at(&["G1 X2 Y2"]).is_none(), "two expressions write two pieces");
    let t = parse("G1 X{a; b} Y2");
    assert_eq!(t.captures(0, "G1 X12 Y2"), Some(vec!["12".into()]), "adjacent values are one value");
    let t = parse("G1 X{x}{if c}0{else}5{endif} Y2");
    assert_eq!(t.captures(0, "G1 X120 Y2").map(|c| c.len()), Some(2), "a branch's part of a line is its own capture");
    let t = parse(r#"{"G1 X" + x} Y2"#);
    assert_eq!(t.captures(0, "G1 X12 Y2"), Some(vec!["12".into()]));
    assert!(parse(r#"{"[name]{expr}"}"#).match_at(&["[name]{expr}"]).is_some(), "strings contain literal braces and brackets");
    let t = parse(r#"{local x = 2; global y = 3; x = 4;; "M400\n";}"#);
    assert_eq!(t.match_at(&["M400"]).map(|m| m.0), Some(1));
    let t = parse(r#"G1 X{if c then "12"; else "34"; endif} Y2"#);
    assert_eq!(t.captures(0, "G1 X12 Y2"), Some(vec!["12".into()]), "partial branches become one value");
    let t = parse(r#"{"G1 X" + (c ? 1 : 2) + " Y2\n";}"#);
    assert_eq!(t.captures(0, "G1 X12 Y2"), Some(vec!["12".into()]));
}

#[test]
fn unclaimed_models_fail_before_checking_commands() {
    let mut p = profile();
    p.printer_model = "Unclaimed".into();
    let refusal = check_gcode("", &p).expect_err("unclaimed model");
    assert_eq!(refusal.0[0].detail, family::UNSUPPORTED);
    let refusal = MachineProfile::from_settings(&|key| if key == "printer_model" { vec![p.printer_model.clone()] } else { vec![] }, vec![]).expect_err("unclaimed settings");
    assert_eq!(refusal.0[0].detail, family::UNSUPPORTED);
}

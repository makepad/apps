use crate::{eval::Evaluator, FabModel};
use fab_plate::machine::Machine;

#[test]
fn flat_carved_bodies_rest_on_the_bed_with_and_without_a_hinge() {
    let common = r#"
use mod.model.*
use mod.fab.*
use mod.math.*
let m = mod.model
let ground = m.box({size: [400mm, 20mm, 400mm] at: [0, -10mm, 0]})
let a = m.carve(m.blend([m.cylinder({radius: 3.9mm height: 7.9mm at: [0, 3.95mm, 0]}) m.box({size: [8mm, 7.9mm, 9mm] at: [0, 3.95mm, -4.5mm]})], 0), ground, 0)
let hub = m.cylinder({radius: 3.9mm height: 7.9mm at: [0, 3.95mm, 0]})
let cap = m.ellipsoid({radii: [14.5mm, 10mm, 8mm] at: [0, 10mm, 25mm]})
let skull = m.ellipsoid({radii: [14mm, 13mm, 14mm] at: [0, 12mm, 14mm]})
let bells = for s in [-1, 1] { m.sphere({radius: 4mm at: [s * 23mm, 3.5mm, 33mm]}) }
let horns = for s in [-1, 1] { m.ellipsoid({radii: [4mm, 6mm, 13mm] at: [s * 13mm, 6mm, 28mm] rotate: [0, s * 55°, 0]}) }
"#;
    let mut failures = Vec::new();
    for (name, holes) in [
        ("upright", "let holes = for s in [-1, 1] { m.cylinder({radius: 1.3mm height: 60mm at: [s * 23mm, 0, 33mm]}) }"),
        ("along_z", "let holes = for s in [-1, 1] { m.cylinder({radius: 1.3mm height: 12mm at: [s * 23mm, 3.5mm, 33mm] rotate: [90°, 0, 0]}) }"),
        ("none", "let holes = [ground ground]"),
    ] {
        for hinged in [true, false] {
            let joint = if hinged { "m.hinge(a, b, {at: [0, 3.95mm, 0] pin: 1.6mm lower: -25° upper: 25°})" } else { "object(b, name: \"b\")" };
            let script = format!("{common}{holes}\nlet b = m.carve(m.blend([skull hub cap horns[0] horns[1] bells[0] bells[1]], 2.5mm), [ground holes[0] holes[1]], 0)\n{joint}\nobject(a, name: \"a\")\n");
            let model = FabModel::with_script("assembly", "assembly.cad.splash", &script);
            let mut evaluator = Evaluator::new(Machine::dual_nozzle_350());
            let (_, evaluated) = model.evaluate(&mut evaluator, None).unwrap_or_else(|e| panic!("{name} hinged={hinged}: {e}"));
            assert_eq!(evaluated.plate.parts.len(), 2);
            for ((part, field), frame) in evaluated.plate.parts.iter().zip(&evaluated.fields).zip(&evaluated.model_to_plate) {
                println!("{name} hinged={hinged} {} floor={:.9} box={:.9}..{:.9} field_floor={:?} field_gap={:?} drop_z={:.9}", part.name, part.floor(), part.bounds().min[2], part.bounds().max[2], part.field_floor, field.as_ref().map(|f| f.bottom_gap), frame[2][3]);
                if part.floor().abs() > 1e-6 || frame[2][3].abs() > 1e-6 {
                    failures.push(format!("{name} hinged={hinged} {} floor={} drop_z={}", part.name, part.floor(), frame[2][3]));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

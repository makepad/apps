//! A floor printed in the air, through the real slice: a blend's lifted round holds a whole floor
//! off the bed. A cone on its tip and a sphere, which also grow fast off the bed, do not, nor does
//! a low bracket like the one Fab opens with.

use fab_model::eval::Evaluator;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use makepad_fab_slice::plate::slice_plate;

fn setup() -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.2");
    JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

/// The part's `floor_in_air` findings, from its field where it has one (a field part has no mesh).
fn floors(source: &str) -> Vec<String> {
    let m = FabModel::with_script("part", "part.cad.splash", source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    let sliced = match ev.fields[0].as_ref() {
        Some(pf) => {
            let fr = &ev.frames[0];
            let mut to = pf.to_printer;
            for (r, o) in fr.offset.iter().enumerate() {
                to[r][3] -= o;
            }
            let frames = vec![Frame { local: fr.local.clone(), offset: fr.offset, field: Some(PartField::new(pf.shape.clone(), to)) }];
            slice_frames(&setup(), &ev.plate, &frames, &[], None).expect("slices from the field")
        }
        None => slice_plate(&setup(), &ev.plate).expect("slices from the mesh"),
    };
    let mut report = fab_printability::Report::default();
    fab_printability::analyse_slice(&ev.machine, &ev.plate, &fab_printability::Materials::default(), sliced.layers(), &sliced.timeline, &mut report);
    report.issues.iter().filter(|i| i.kind == "floor_in_air").map(|i| i.message.clone()).collect()
}

#[test]
fn only_a_lifted_floor_prints_in_the_air() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let head = "use mod.fab.*\nlet m = mod.model\n";
    // A 30 x 30 x 1.5 floor with a round flush on it: the blend lifts the round by a quarter of its radius.
    let joined = "let floor = m.box({size: [30mm, 1.5mm, 30mm] at: [0, 0.75mm, 0]})\nlet round = m.cylinder({radius: 5mm height: 1.5mm at: [0, 0.75mm, 0]})\nlet joined = m.blend([floor round], 1.5mm)\n";
    let lifted = floors(&format!("{head}{joined}object(joined)\n"));
    assert_eq!(lifted.len(), 1, "the floor over the lifted round: {lifted:?}");
    let under = "let under = m.box({size: [40mm, 4mm, 40mm] at: [0, -2mm, 0]})\nlet part = m.carve(joined, under, 0)\n";
    assert_eq!(floors(&format!("{head}{joined}{under}object(part)\n")), Vec::<String>::new(), "the round cut flush");
    // Only the first layers matter: a sphere's and a cone's lowest 2 mm.
    assert_eq!(floors(&format!("{head}let c = m.cylinder({{radius: 0.1mm top_radius: 2mm height: 2mm at: [0, 1mm, 0]}})\nobject(c)\n")), Vec::<String>::new(), "a cone on its tip");
    let cap = format!("{head}let s = m.sphere({{radius: 30mm at: [0, 30mm, 0]}})\nlet over = m.box({{size: [80mm, 80mm, 80mm] at: [0, 42mm, 0]}})\nlet cap = m.carve(s, over, 0)\nobject(cap)\n");
    assert_eq!(floors(&cap), Vec::<String>::new(), "a sphere");
    let low = include_str!("../../app/resources/bracket.cad.splash").replace("param(52mm,", "param(26mm,");
    assert_eq!(floors(&low), Vec::<String>::new(), "the bracket");
}

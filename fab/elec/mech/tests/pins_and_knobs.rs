//! The enclosure prelude's printed parts on a part's moving or turning member: the press pin on a
//! keyswitch's stem (`stand`) and the knob on a shaft (`@shaft`), on the `ledctl` fixture with its
//! button's body swapped for such a part. Points are in the case's frame (model mm, assembled):
//! board x is model x + the slab's min x, board y is the slab's max z - model z.

use fab_elec_core::{mm, Contour, PolySet, P};
use fab_elec_fixtures::ledctl;
use fab_elec_mech::bridge::{board_inputs, CaseInputs};
use fab_elec_mech::{board_mech, case_checks, case_solids, CheckOpts, Level};
use fab_elec_parts::{BodyPort, BodyPortKind, BodySource};
use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use fab_plate::PartMesh;
use std::sync::Arc;

/// ledctl with its button's body replaced: `height` tall, with `port` as its one port.
fn with_button(height: f64, port: BodyPort) -> fab_elec_design::Design {
    let mut design = ledctl();
    let circuit = Arc::make_mut(&mut design.units[0].circuit);
    let sw = circuit.comp("btn.sw").expect("the fixture has its button").id;
    let body = Arc::make_mut(&mut circuit.comps[sw.ix()].part).body.as_mut().expect("the button declares a body");
    body.height = mm(height);
    body.ports = vec![port];
    design
}

fn build(design: &fab_elec_design::Design, script: &str) -> Evaluated {
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let mut model = FabModel::with_script("case", "case.cad.splash", script);
    model.set_env(inputs.env());
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    model.evaluate(&mut ev, None).unwrap_or_else(|e| panic!("the case script builds: {e}")).1
}

/// A shafted part's model as a maker draws it, in its own frame: a 9 x 9 mm housing 6.5 mm tall
/// and a square 4.2 mm shaft (inside a 6 mm one's section) standing from it to 16.5 mm.
fn shafted_model() -> Vec<u8> {
    use fab_elec_import::{glb::write_glb, Material, MeshBuilder, Normals};
    let mut b = MeshBuilder::new();
    let plastic = b.add_material(Material::named("body"));
    let flat = Normals::Crease { domain: 0, angle: 0.5 };
    let mut cuboid = |lo: [f32; 3], hi: [f32; 3]| {
        let c = |x: usize, y: usize, z: usize| [[lo[0], hi[0]][x], [lo[1], hi[1]][y], [lo[2], hi[2]][z]];
        for face in [
            [c(0, 0, 0), c(0, 1, 0), c(1, 1, 0), c(1, 0, 0)], [c(0, 0, 1), c(1, 0, 1), c(1, 1, 1), c(0, 1, 1)],
            [c(0, 0, 0), c(1, 0, 0), c(1, 0, 1), c(0, 0, 1)], [c(0, 1, 0), c(0, 1, 1), c(1, 1, 1), c(1, 1, 0)],
            [c(0, 0, 0), c(0, 0, 1), c(0, 1, 1), c(0, 1, 0)], [c(1, 0, 0), c(1, 1, 0), c(1, 1, 1), c(1, 0, 1)],
        ] {
            assert!(b.polygon(plastic, &face, flat));
        }
    };
    cuboid([-4.5, -4.5, 0.0], [4.5, 4.5, 6.5]);
    cuboid([-2.1, -2.1, 6.5], [2.1, 2.1, 16.5]);
    write_glb(&b.finish())
}

/// Is `p` inside the closed mesh? Parity of crossings along a ray in a direction no face of a
/// case lines up with.
fn inside(mesh: &PartMesh, p: [f64; 3]) -> bool {
    let d = [0.5773, 0.5774, 0.5775];
    let mut n = 0;
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|i| mesh.positions[i as usize]);
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let h = [d[1] * e2[2] - d[2] * e2[1], d[2] * e2[0] - d[0] * e2[2], d[0] * e2[1] - d[1] * e2[0]];
        let det = e1[0] * h[0] + e1[1] * h[1] + e1[2] * h[2];
        if det.abs() < 1e-12 {
            continue;
        }
        let s = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
        let u = (s[0] * h[0] + s[1] * h[1] + s[2] * h[2]) / det;
        let q = [s[1] * e1[2] - s[2] * e1[1], s[2] * e1[0] - s[0] * e1[2], s[0] * e1[1] - s[1] * e1[0]];
        let v = (d[0] * q[0] + d[1] * q[1] + d[2] * q[2]) / det;
        let t = (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) / det;
        if u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t > 0.0 {
            n += 1;
        }
    }
    n % 2 == 1
}

/// The plate's objects by name, printed or shown only (the board's slab), read in the model frame.
struct Plate<'a>(&'a Evaluated);

impl Plate<'_> {
    fn ix(&self, name: &str) -> usize {
        let names: Vec<&str> = self.0.plate.placed_parts().map(|p| p.name.as_str()).collect();
        names.iter().position(|n| *n == name).unwrap_or_else(|| panic!("{name} on the plate: {names:?}"))
    }
    fn mesh(&self, name: &str) -> &PartMesh {
        &self.0.placed_part(self.ix(name)).mesh
    }
    fn frame(&self, name: &str) -> [[f64; 4]; 3] {
        *self.0.placed_frame(self.ix(name)).expect("every placed part has its frame")
    }
    fn solid(&self, name: &str, p: [f64; 3]) -> bool {
        let m = self.frame(name);
        inside(self.mesh(name), [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]))
    }
    /// The object's bounds in the model frame (the placement's rotation part is orthonormal).
    fn bounds(&self, name: &str) -> ([f64; 3], [f64; 3]) {
        let m = self.frame(name);
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for q in &self.mesh(name).positions {
            let d = [0, 1, 2].map(|r| q[r] - m[r][3]);
            let p: [f64; 3] = std::array::from_fn(|c| (0..3).map(|r| m[r][c] * d[r]).sum());
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        (lo, hi)
    }
}

/// Why: a keyswitch's stem stands as far out of its housing as it travels (3 mm), and its section
/// is a slot, not a circle. The press pin used to take the opening's nearest corner for its shaft
/// (3.3 mm in radius through a 5 mm slot) and a foot as long as the travel (3.3 mm, the roof raised
/// to match), as if the housing were right under the stem's top. Now the pin rests on the stem's
/// top, its foot is the travel the stem's stand does not cover plus 0.3 mm, so pressed its whole
/// travel its flange stays 0.3 mm over the housing, and its shaft passes the slot's narrow width.
#[test]
fn a_press_pin_rests_on_a_keyswitch_stem_and_passes_its_slot() {
    // The stem: 5 x 3 mm, its top 8 mm over the board, 3 mm out of the housing, 3 mm of travel;
    // the opening is the stem grown by 1 mm, a 7 x 5 mm slot.
    let stem = Contour::rect(P::mm(-2.5, -1.5), P::mm(2.5, 1.5), 0);
    let design = with_button(8.0, BodyPort {
        kind: BodyPortKind::Press,
        profile: PolySet::from_ring_nonzero(stem.to_ring(mm(0.01))),
        at: [0, 0, mm(8.0)],
        dir: [0.0, 0.0, 1.0],
        reach: mm(12.0),
        margin: mm(1.0),
        travel: mm(3.0),
        stand: mm(3.0),
    });
    let unit = &design.units[0];
    let mech = board_mech(unit.board.as_deref().expect("the fixture has a board"), &unit.circuit);
    assert!(mech.splash().contains("travel: 3mm stand: 3mm"), "board() gives the case the stem's travel and stand");

    let e = build(&design, "use mod.model.*\nuse mod.fab.*\nlet pcb = board(\"ledctl\")\nlet case = enclosure(pcb, {})\nobject(case.lid_on, name: \"Lid\")\nobject(board_solid(pcb, [], case.frame), name: \"Slab\")\nfor pin in case.pins {\n    object(pin.pin_on, name: pin.name)\n}\n");
    let plate = Plate(&e);
    let (slab_lo, slab_hi) = plate.bounds("Slab");
    // The button at board (38, 9); the stem's top 8 mm over the board's top face.
    let (kx, kz) = (38.0 + slab_lo[0], slab_hi[2] - 9.0);
    let stem_top = slab_hi[1] + 8.0;
    let (pin_lo, pin_hi) = plate.bounds("Pin btn.sw");
    assert!((pin_lo[1] - stem_top).abs() < 1e-3, "the pin rests on the stem's top at {stem_top}, not on the housing: its foot at {}", pin_lo[1]);
    let flange_r = (pin_hi[0] - pin_lo[0]) / 2.0;
    assert!(plate.solid("Pin btn.sw", [kx + 1.2, stem_top + 0.15, kz]) && !plate.solid("Pin btn.sw", [kx + 2.0, stem_top + 0.15, kz]),
        "the foot is the stem's own width (r 1.5)");
    assert!(plate.solid("Pin btn.sw", [kx + 2.0, stem_top + 0.4, kz]),
        "the flange starts 0.3 mm over the stem's top: pressed 3 mm, it stays 0.3 mm over the housing 3 mm under that top");

    let (_, lid_hi) = plate.bounds("Lid");
    let (top, roof_in) = (lid_hi[1], lid_hi[1] - 1.6);
    for (dx, dz) in [(2.2, 0.0), (0.0, 2.2), (-2.2, 0.0), (0.0, -2.2)] {
        assert!(plate.solid("Pin btn.sw", [kx + dx, top + 0.5, kz + dz]), "the pin's shaft is 2.3 mm in radius over the roof ({dx}, {dz})");
    }
    assert!(!plate.solid("Pin btn.sw", [kx + 2.4, top + 0.5, kz]), "the shaft passes the slot's 5 mm width with 0.2 mm to spare, not its corners' 7.8 mm");
    assert!(!plate.solid("Lid", [kx + 3.3, top - 0.5, kz]) && plate.solid("Lid", [kx + 3.8, top - 0.5, kz]), "the slot is 7 mm long across the roof");
    assert!(!plate.solid("Lid", [kx, top - 0.5, kz + 2.3]) && plate.solid("Lid", [kx, top - 0.5, kz + 2.8]), "and 5 mm wide");
    // foot 0.3, flange 0.8, then the 45-degree cone in from the flange's rim to the shaft
    let cone_top = stem_top + 0.3 + 0.8 + flange_r - 2.3;
    assert!(roof_in >= cone_top + 0.3 - 1e-3, "the roof rises over the pin with 0.3 mm to lift: roof {roof_in}, cone {cone_top}");
}

/// Why: a potentiometer's shaft goes up through the lid and takes a knob, and the case knew no
/// shaft: a pot was a box under the roof, its shaft inside the lid. A @shaft port now opens the
/// lid round its section grown by its margin, whatever the roof's height, and the case makes a
/// printed knob for it: on the shaft, its bore the shaft's own section, riding 0.5 mm over the
/// roof, with a pointer groove toward the board's back, printed on its top as a closed solid.
#[test]
fn a_shaft_goes_up_through_the_lid_into_its_printed_knob() {
    // A 6 mm shaft standing 10 mm out of a 6.5 mm housing: its tip 16.5 mm over the board.
    let design = with_button(6.5, BodyPort {
        kind: BodyPortKind::Shaft,
        profile: PolySet::from_ring_nonzero(Contour::circle(P::ZERO, mm(3.0)).to_ring(mm(0.01))),
        at: [0, 0, mm(16.5)],
        dir: [0.0, 0.0, 1.0],
        reach: mm(10.0),
        margin: mm(0.5),
        travel: 0,
        stand: mm(10.0),
    });
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mech = board_mech(pcb, &unit.circuit);
    let source = mech.splash();
    assert!(source.contains("kind: @shaft") && source.contains("stand: 10mm") && source.contains("profile: [["), "board() gives the case the shaft, its stand and its section");

    let e = build(&design, "use mod.model.*\nuse mod.fab.*\nlet pcb = board(\"ledctl\")\nlet case = enclosure(pcb, {})\nobject(case.lid_on, name: \"Lid\")\nobject(board_solid(pcb, [], case.frame), name: \"Slab\")\nfor k in case.knobs {\n    object(k.knob_on, name: \"On\")\n    object(k.knob, name: k.name)\n}\n");
    let plate = Plate(&e);
    let (slab_lo, slab_hi) = plate.bounds("Slab");
    let (kx, kz) = (38.0 + slab_lo[0], slab_hi[2] - 9.0);
    let tip = slab_hi[1] + 16.5;
    let (_, lid_hi) = plate.bounds("Lid");
    let top = lid_hi[1];
    assert!(top < tip - 3.0, "the roof ({top}) stays under the shaft's tip ({tip}): the housing, not the shaft, sets its height");
    assert!(!plate.solid("Lid", [kx + 3.3, top - 0.5, kz]) && plate.solid("Lid", [kx + 3.8, top - 0.5, kz]), "the lid opens round the shaft, 3.5 mm in radius (its 0.5 mm margin)");

    let (on_lo, on_hi) = plate.bounds("On");
    assert!((on_lo[1] - (top + 0.5)).abs() < 1e-3 && (on_hi[1] - on_lo[1] - 10.0).abs() < 1e-3, "the knob rides 0.5 mm over the roof, 10 mm tall: y {}..{}", on_lo[1], on_hi[1]);
    assert!((on_hi[0] - on_lo[0] - 12.0).abs() < 0.05, "the knob is 12 mm across: {on_lo:?}..{on_hi:?}");
    let bottom = on_lo[1];
    assert!(!plate.solid("On", [kx + 2.8, tip - 0.5, kz]) && plate.solid("On", [kx + 3.2, tip - 0.5, kz]), "its bore is the shaft's own 6 mm, so it grips the tip");
    assert!(!plate.solid("On", [kx, bottom + 0.5, kz]), "the bore opens at the knob's underside: the shaft goes in from below");
    assert!(plate.solid("On", [kx + 2.0, bottom + 9.5, kz]), "a 1.2 mm top closes the bore");
    assert!(!plate.solid("On", [kx, bottom + 9.8, kz - 3.0]) && plate.solid("On", [kx, bottom + 9.8, kz + 3.0]), "the pointer groove runs from the middle toward the board's back only");

    let printed = plate.mesh("Knob btn.sw");
    assert_eq!(printed.open_edges(), 0, "the printed knob is a closed solid");
    let pb = printed.bounds();
    assert!(pb.min[2].abs() < 1e-6 && (pb.max[2] - 10.0).abs() < 1e-3, "the knob prints standing on its top, 10 mm tall: {pb:?}");

    let case = case_solids(&e, "ledctl");
    let findings: Vec<_> = case_checks(&mech, &case, &CheckOpts::default()).into_iter()
        .filter(|f| f.level != Level::Info && f.message.contains("btn.sw")).collect();
    assert!(findings.is_empty(), "the housing fits under the roof and nothing blocks the shaft: {findings:#?}");

    // With its model on, the part is fitted by its housing: the shaft standing in its opening is
    // the port's, not a cone of hull from the housing to the tip that meets the roof round the hole.
    let dir = std::env::temp_dir().join(format!("fab-elec-mech-shafted-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let glb = dir.join("shafted.glb");
    std::fs::write(&glb, shafted_model()).expect("write the model");
    let mut modelled = mech.clone();
    let sw = unit.circuit.comp("btn.sw").expect("the fixture has its button").id;
    modelled.bodies.iter_mut().find(|b| b.comp == sw).expect("the button has a body").source = Some(BodySource::Mesh(glb.display().to_string()));
    let findings: Vec<_> = case_checks(&modelled, &case, &CheckOpts::default()).into_iter()
        .filter(|f| f.level != Level::Info && f.message.contains("btn.sw")).collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(findings.is_empty(), "the modelled housing fits and its shaft goes through its hole: {findings:#?}");
}

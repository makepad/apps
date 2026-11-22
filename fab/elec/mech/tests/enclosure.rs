//! The enclosure prelude on the `ledctl` fixture, through the host's own path (`bridge`): the
//! board's inputs from its committed snapshot, the prelude a library of the language. The case
//! builds, slices with no printability issue, leaves its ports open, and holds the board in place
//! off the print.

use fab_elec_fixtures::ledctl;
use fab_elec_mech::bridge::{board_inputs, CaseInputs};
use fab_elec_mech::scene::scene_of;
use fab_elec_mech::Pose;
use fab_model::eval::{Engine, Evaluator};
use fab_model::{Change, FabModel, PlaceArg};
use fab_plate::machine::Machine;
use fab_plate::PartMesh;
use makepad_fab_settings::profiles::{JobSetup, Preset};

/// The ledctl case document: the case reads `board(..)` alone; the board's volume (what the case
/// holds, off the print) reads its bodies too.
const CASE: &str = include_str!("../../fixtures/ledctl.cad.splash");

#[test]
fn board_read_states_the_size_and_the_button_where_the_case_reads_them() {
    use fab_elec_core::BBox;
    use fab_elec_design::action::{find, Risk};
    use fab_elec_mech::board_mech_with;
    use fab_elec_mech::read::{board_read, register_actions};
    use fab_elec_parts::PartKind;
    use makepad_model_script::lang::{evaluate, Arg, Options, RootFn, V, NO_INPUTS};

    fn field<'a>(value: &'a V, name: &str) -> &'a V {
        value.obj().and_then(|object| object.get(name)).unwrap_or_else(|| panic!("the reply has field {name}"))
    }
    fn list(value: &V) -> &[V] {
        let V::Arr(items) = value else { panic!("the reply field is a list") };
        items
    }
    fn text(value: &V) -> &str {
        match value {
            V::Str(text) | V::Id(text) => text,
            _ => panic!("the reply field is text or a symbol"),
        }
    }
    let lengths = |value: &V| list(value).iter().map(|value| {
        (value.num().expect("a length evaluates as an SI quantity") * 1_000_000_000.0).round() as i64
    }).collect::<Vec<_>>();

    let design = ledctl();
    let unit = design.units.iter().find(|unit| unit.board.is_some()).expect("ledctl has a board unit");
    let board = unit.board.as_deref().expect("the selected unit has its board");
    let sw = unit.circuit.comps.iter().find(|comp| comp.refdes == "SW1").expect("ledctl has SW1");
    let placed = board.placed.iter().find(|placed| placed.comp == sw.id).expect("the board places SW1");
    let mech = board_mech_with(board, &unit.circuit, &board.drills());
    let bounds = mech.outline.iter().fold(BBox::EMPTY, |bounds, contour| bounds.union(contour.bbox()));
    let reply = board_read(&design, None, None).expect("the design's only board can be read without a name");
    let button_read = board_read(&design, Some(&unit.name), Some("SW1")).expect("SW1 can be read by refdes");
    let pair = fab_elec_fixtures::ledctl_pair();
    let summary = board_read(&pair, None, None).expect("omitting board lists both boards");
    let probe = board_read(&pair, Some("probe"), None).expect("the second board can be read by name");
    let source = format!("mod.fab.read({reply})\nmod.fab.read({button_read})\nmod.fab.read({summary})\nmod.fab.read({probe})");
    let program = evaluate(&source, &Options {
        generated: &Default::default(),
        assets: &Default::default(),
        images: &Default::default(),
        extension: "fab",
        libraries: &[],
        inputs: &NO_INPUTS,
        unit: 1000.0,
        roots: &[RootFn { name: "read", args: &["value"], model: false }],
        overrides: Default::default(),
        weave: None,
        solved: Default::default(),
    }).expect("the replies are Splash objects that the case language can read");
    let replies: Vec<_> = program.roots.iter().map(|root| {
        let Some(Arg::Value(value)) = root.get("value") else { panic!("each read returns its Splash value") };
        value
    }).collect();
    assert_eq!(replies.len(), 4, "every board reply reaches the reader");
    let (reply, filtered, summary, probe) = (replies[0], replies[1], replies[2], replies[3]);
    assert_eq!(text(field(summary, "pick")), "pass board: one of these names to read it");
    let boards = list(field(summary, "boards"));
    assert_eq!(boards.len(), 2, "both boards appear in the summary");
    for (brief, full) in boards.iter().zip([reply, probe]) {
        assert_eq!(text(field(brief, "board")), text(field(full, "board")), "the summary names each board in design order");
        assert_eq!(lengths(field(brief, "size")), lengths(field(full, "size")), "the summary uses the full read's size");
        for name in ["parts", "ports"] {
            assert_eq!(field(brief, name).num(), Some(list(field(full, name)).len() as f64), "the summary counts the board's {name}");
        }
    }
    assert_eq!(text(field(reply, "board")), unit.name, "the reply names the design's board unit");
    let parts = list(field(reply, "parts"));
    let button = parts.iter().find(|part| text(field(part, "ref")) == "SW1").expect("the reply names SW1");
    assert_eq!(lengths(field(button, "at")), [placed.at.x as i64, placed.at.y as i64], "SW1 uses its board placement, not its body's box centre");
    assert_eq!(lengths(field(reply, "size")), [bounds.width(), bounds.height()], "the size bounds the same BoardMech outline the case reads");
    let selected_parts = list(field(filtered, "parts"));
    let selected_ports = list(field(filtered, "ports"));
    assert_eq!(selected_parts.iter().map(|part| text(field(part, "ref"))).collect::<Vec<_>>(), ["SW1"], "only SW1 keeps its part");
    assert_eq!(selected_ports.iter().map(|port| (text(field(port, "part")), text(field(port, "kind")))).collect::<Vec<_>>(), [("SW1", "press")], "only SW1 keeps its press port");
    assert!(list(field(filtered, "holes")).is_empty() && list(field(filtered, "keepouts")).is_empty(), "a part read drops holes and keepouts");
    for only in ["SW", "sw", "press"] {
        let selected = board_read(&design, Some(&unit.name), Some(only)).expect("the prefix or press port matches");
        assert_eq!(selected, button_read, "only {only} selects SW1 and its press port");
    }
    let leds: Vec<_> = unit.circuit.comps.iter().filter(|comp| comp.part.kind == PartKind::Led).map(|comp| comp.refdes.as_str()).collect();
    assert!(!leds.is_empty() && parts.iter().any(|part| leds.contains(&text(field(part, "ref")))), "the unfiltered reply includes the fixture's LEDs");
    assert!(selected_parts.iter().all(|part| !leds.contains(&text(field(part, "ref")))) && selected_ports.iter().all(|port| !leds.contains(&text(field(port, "part")))), "only SW1 drops the LEDs and their ports");
    assert_eq!(lengths(field(filtered, "size")), [bounds.width(), bounds.height()], "filtering entries preserves the board's size");

    let refusal = board_read(&design, Some("missing-board"), None).expect_err("an unknown board is refused");
    assert!(design.units.iter().filter(|unit| unit.board.is_some()).all(|unit| refusal.contains(&unit.name)), "an unknown board refusal lists the design's board names");
    assert_eq!(refusal.lines().count(), 1, "the board refusal is a single line");
    let refusal = board_read(&design, None, Some("ZZ9")).expect_err("an unknown reference is refused");
    assert!(refusal.contains("SW1") && refusal.contains("D1–D8") && refusal.lines().count() == 1 && refusal.chars().count() <= 200,
        "an unknown filter names the board's parts in one short line: {refusal}");
    register_actions();
    let action = find("elec.board.read").expect("the board reader is registered in the shared action catalog");
    assert_eq!(action.risk, Risk::Read, "reading a board has read-only risk");
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

/// Where a mesh is open: both ends of every edge not used exactly once each way, in the model
/// frame (`to_plate` undone: a rigid map), so a failing check says which feature broke.
fn open_at(mesh: &PartMesh, to_plate: &[[f64; 4]; 3]) -> Vec<[[f64; 3]; 2]> {
    let mut edges: std::collections::HashMap<(u32, u32), i32> = std::collections::HashMap::new();
    for t in &mesh.triangles {
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            if a != b {
                *edges.entry((a.min(b), a.max(b))).or_insert(0) += if a < b { 1 } else { -1 };
            }
        }
    }
    let model = |v: u32| {
        let q = mesh.positions[v as usize];
        let d: [f64; 3] = std::array::from_fn(|r| q[r] - to_plate[r][3]);
        std::array::from_fn(|c| (0..3).map(|r| to_plate[r][c] * d[r]).sum::<f64>())
    };
    edges.iter().filter(|(_, n)| **n != 0).map(|((a, b), _)| [model(*a), model(*b)]).collect()
}

#[test]
fn the_ledctl_case_builds_closed_prints_clean_and_leaves_its_ports_open() {
    let design = ledctl();
    let unit = &design.units[0];
    let pcb = unit.board.as_deref().expect("the fixture has a board");
    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, pcb, &unit.circuit, &pcb.drills()));
    let env = inputs.env();
    let mut model = FabModel::with_script("case", "case.cad.splash", CASE);
    model.set_env(env);
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    // The board is an insert: it rides in the base, on its frame, and never prints. The printed
    // parts stand as the script lays them out, each object's origin at (150, 150): the lid 64.8 mm
    // and the button's press pin 102.2 mm right of the base. (Placing the base alone leaves the
    // others to the plate's shelf pack, which does not avoid a placed object.)
    let place = |object: &str| Change::Place { instance: "case".into(), object: object.into(), arg: PlaceArg::At(Some([150.0, 150.0])) };
    let changes = [place("Base"), place("Lid"), place("Pin btn.sw")];
    let head = model.head();
    let (_, evaluated) = model.apply(&mut ev, "case", head, &changes, None).unwrap_or_else(|e| panic!("the case script builds: {e}"));
    let (_, e) = evaluated.expect("a fresh apply evaluates");
    let names: Vec<&str> = e.plate.parts.iter().map(|p| p.name.as_str()).collect();
    let part = |n: &str| e.plate.parts.iter().find(|p| p.name == n).unwrap_or_else(|| panic!("{n} on the plate: {names:?}"));
    let (base, lid) = (part("Base"), part("Lid"));
    let ix = |n: &str| e.plate.parts.iter().position(|p| p.name == n).unwrap();
    let open = |n: &str| open_at(&e.plate.parts[ix(n)].mesh, &e.model_to_plate[ix(n)]);
    assert_eq!((base.mesh.open_edges(), lid.mesh.open_edges()), (0, 0), "base and lid are closed solids: open edges (model mm) {:?} and {:?}", open("Base"), open("Lid"));
    assert!(base.mesh.bounds().min[2].abs() < 1e-6 && lid.mesh.bounds().min[2].abs() < 1e-6, "both stand on the bed (the lid on its top face)");
    assert_eq!((e.engine(ix("Base")), e.engine(ix("Lid"))), (Engine::Mesh, Engine::Mesh), "the case is mesh parts (the views draw it as one)");
    let errors: Vec<_> = e.printability.issues.iter().filter(|i| i.severity != fab_printability::Severity::Info).collect();
    assert!(errors.is_empty(), "the case prints with no printability errors or warnings: {errors:?}");
    // The slice: the printed parts only (the board never reaches the slicer), its report over the
    // whole plate.
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.2");
    let setup = JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    let sliced = makepad_fab_slice::plate::slice_plate(&setup, &e.plate.printable()).unwrap_or_else(|err| panic!("the case slices: {err}"));
    assert_eq!(sliced.objects.len(), 3, "three objects print: base, lid and the button's press pin");
    let mut report = fab_printability::Report::default();
    fab_printability::analyse_slice(&e.machine, &e.plate, &fab_printability::Materials::default(), sliced.layers(), &sliced.timeline, &mut report);
    let sliced_errors: Vec<_> = report.issues.iter().filter(|i| i.severity != fab_printability::Severity::Info).map(|i| (i.kind, i.object.clone(), i.layer, i.message.clone())).collect();
    assert!(sliced_errors.is_empty(), "the sliced case has no printability errors or warnings: {sliced_errors:?}");

    // The board is on the plate, off the print, inside the base: its slab and bodies from the
    // header under it (2.5 mm down from its underside at 6.2: the 1.6 mm floor and the 4.6 mm
    // standoffs that hold the M3 inserts) to the receptacle on top (7.8 + 3.16).
    let board = e.plate.excluded.iter().find(|x| x.part.name == "Board").expect("the board is a placed part that does not print");
    assert_eq!(board.insert.as_ref().map(|i| i.into.as_str()), Some(base.object.as_str()), "the board is an insert into the base");
    let (bb, outer) = (board.part.mesh.bounds(), base.mesh.bounds());
    assert!((0..2).all(|a| bb.min[a] > outer.min[a] + 1.9 && bb.max[a] < outer.max[a] - 1.9), "the board lies within the base's walls: {bb:?} in {outer:?}");
    assert!((bb.min[2] - 3.7).abs() < 1e-3 && (bb.max[2] - 10.96).abs() < 1e-3, "the board sits on its standoffs, parts under and over it: z {}..{}", bb.min[2], bb.max[2]);
    // Its plan is the scene board's outline (the volume follows the true outline; no part's box
    // pokes past the ledctl edge): board x, y to the plate at x - 25 + 150, y - 15 + 150.
    let scene = scene_of(&design, &Pose::default());
    let edge = scene.boards[0].outline[0].bbox();
    let plan = [edge.min.x as f64 * 1e-6 + 125.0, edge.min.y as f64 * 1e-6 + 135.0, edge.max.x as f64 * 1e-6 + 125.0, edge.max.y as f64 * 1e-6 + 135.0];
    assert!((bb.min[0] - plan[0]).abs() < 1e-3 && (bb.min[1] - plan[1]).abs() < 1e-3 && (bb.max[0] - plan[2]).abs() < 1e-3 && (bb.max[1] - plan[3]).abs() < 1e-3, "the board's volume has the scene board's plan: {bb:?} against {plan:?}");
    // The real board's place, for the host that draws it: the part is linked `board:ledctl` and
    // carries the board's frame of that name, which takes board (x, y, z) mm to the plate at
    // (x + 125, y + 135, 6.2 + z), the same map as the volume's.
    assert_eq!(board.link.as_deref(), Some("board:ledctl"), "the board's volume is linked to its board");
    let (name, f) = board.frame.as_ref().expect("the board's volume carries the board's frame");
    assert_eq!(name, "board:ledctl", "the frame is named for its board");
    let m = board.model_to_plate;
    let apply = |m: &[[f64; 4]; 3], p: [f64; 3]| [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
    for (p, want) in [([0.0, 0.0, 0.0], [125.0, 135.0, 6.2]), ([10.0, 0.0, 0.0], [135.0, 135.0, 6.2]), ([0.0, 10.0, 0.0], [125.0, 145.0, 6.2]), ([0.0, 0.0, 1.0], [125.0, 135.0, 7.2])] {
        let got = apply(&m, apply(f, p));
        assert!((0..3).all(|a| (got[a] - want[a]).abs() < 1e-6), "board {p:?} lands on the plate at {want:?}, not {got:?}");
    }

    // Points in the case's frame (model mm, assembled): the board's middle over the origin, its
    // bottom face at 1.6 floor + 4.6 standoff (the M3 insert's 5 mm hole and the 1.2 mm floor
    // under it need it), the roof 11.96..13.56 (the 3.16 mm receptacle, 1 mm over it, 1.6 roof).
    // The lid prints turned over beside the base: x + 64.8 (its width and 10 mm), y to 13.56 - y,
    // z to -z.
    let solid_at = |n: &str, p: [f64; 3]| {
        let p = if n == "Lid" { [p[0] + 64.8, 13.56 - p[1], -p[2]] } else { p };
        let m = e.model_to_plate[ix(n)];
        let q = [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        inside(&e.plate.parts[ix(n)].mesh, q)
    };
    // The USB-C receptacle's mouth is at board (0.3, 15, 3.18): model (-24.7, 9.38, 0). The
    // left wall spans x -27.4..-25.4; the opening is its 8.94 x 3.16 shell grown by 0.3 mm round
    // the mouth (y 7.5..11.26).
    assert!(!solid_at("Lid", [-26.4, 9.38, 0.0]) && !solid_at("Base", [-26.4, 7.7, 0.0]), "the USB-C opening goes through the lid's and the base's wall");
    assert!(solid_at("Lid", [-26.4, 9.38, 8.0]) && solid_at("Base", [-26.4, 7.1, 0.0]), "the wall stands beside and under the opening");
    // The first LED at board (28, 26.5) is model (3, -, -11.5): a hole through the roof (11.96..13.56).
    assert!(!solid_at("Lid", [3.0, 12.76, -11.5]) && solid_at("Lid", [3.0, 12.76, -9.0]), "a light hole over each LED, roof round it");
    // The button at board (38, 9) is model (13, -, 6): a 4 mm hole over its plunger, whose top
    // is at 7.8 + 1.5 = 9.3, 2.66 under the roof: out of a finger's reach, so a press pin carries
    // the press. The hole lets its 3.6 mm shaft through and stops its 5.6 mm flange.
    assert!(!solid_at("Lid", [13.0, 12.76, 6.0]) && solid_at("Lid", [13.0, 12.76, 8.6]), "a press hole over the button, roof round it");
    assert!(!solid_at("Lid", [14.9, 12.76, 6.0]) && solid_at("Lid", [15.2, 12.76, 6.0]), "the press hole is 2 mm in radius: wider than the pin's shaft, narrower than its flange");
    // The pin prints beside the lid (x 64.8 + 54.8 / 2 + 10 = 102.2), standing on its shaft's
    // top: 5.26 tall, from 1 mm over the roof (13.56 + 1, more than the switch's 0.25 mm travel)
    // down to the plunger at 9.3. From the bed: the shaft (r 1.8) to 3.16, the 45-degree cone to
    // 4.16, the flange (r 2.8) to 4.96, and the foot (the plunger's r 1.25) 0.3 long: the
    // plunger stands 0.3 out of the housing, more than its 0.25 travel. In place the cone's top
    // is at 9.3 + 2.1 = 11.4: the pin rises 0.56 off the plunger before the cone meets the hole's
    // rim, and pressed 0.25 its flange stays 0.35 over the housing and its shaft 0.75 over the
    // roof.
    let pin = part("Pin btn.sw");
    assert_eq!(pin.mesh.open_edges(), 0, "the press pin is a closed solid");
    let pb = pin.mesh.bounds();
    assert!(pb.min[2].abs() < 1e-6 && (pb.max[2] - 5.26).abs() < 1e-3, "the pin stands on the bed, 5.26 mm tall: {pb:?}");
    assert!((pb.max[0] - pb.min[0] - 5.6).abs() < 0.05, "the pin's flange is 5.6 mm across: {pb:?}");
    let pin_at = |p: [f64; 3]| {
        let m = e.model_to_plate[ix("Pin btn.sw")];
        let q = [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        inside(&pin.mesh, q)
    };
    assert!(pin_at([102.2 + 1.7, 1.0, 0.0]) && !pin_at([102.2 + 1.9, 1.0, 0.0]), "the pin's shaft is 1.8 mm in radius: it passes the hole with 0.2 mm to spare");
    assert!(pin_at([102.2 + 2.7, 4.3, 0.0]), "the pin's flange reaches past the hole's rim: the pin cannot leave the closed case");
    assert!(pin_at([102.2 + 1.1, 5.0, 0.0]) && !pin_at([102.2 + 1.4, 5.0, 0.0]), "the pin's foot is the plunger's width, so the flange stays off the switch's housing");
    // The standoff under mount1 (46, 4) is model (21, 1.6..6.2, 11), 8 across with a 4 mm insert hole.
    assert!(solid_at("Base", [23.5, 3.0, 11.0]) && !solid_at("Base", [21.0, 3.0, 11.0]), "a standoff under each hole, open for its insert");
    assert!(solid_at("Base", [0.0, 0.8, 0.0]) && !solid_at("Base", [0.0, 3.0, 0.0]), "a floor under an open cavity");
}

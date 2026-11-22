use fab_elec_board::{Hole, Item, Origin};
use fab_elec_core::{mm, to_mm, BBox, CompRef, FeatureRef, ItemId, P};
use fab_elec_fixtures::ledctl;
use fab_elec_mech::{board_mech, case_solids, PlacedPort};
use fab_elec_mech::bridge::{board_inputs, elec_generator, BoardItemSink, CaseInputs};
use fab_elec_mech::csg::{Csg, Prim};
use fab_elec_mech::generate::{ElecGenInput, GenBody, GenOutput, PanelAt, ParamValue};
use fab_elec_parts::BodyPortKind;
use fab_materials::{Colour, FinishId, MaterialId};
use fab_model::eval::Evaluator;
use fab_model::program::GenInput;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_csg_graph::{eval::transform_matrix, Graph, Op, TriMesh};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const CASE: &str = r#"use mod.model.*
use mod.fab.*
let m = mod.model
let pcb = board("ledctl")
let case = enclosure(pcb, {})
let lid = m.panel(case.lid_shell, {pose: case.frame cuts: case.lid_ports})
object(lid, name: "Lid")
"#;

#[derive(Debug, PartialEq)]
enum SeenValue {
    Num(f64),
    Bool(bool),
    Text(String),
    Colour(u32),
}

struct Seen {
    part: String,
    params: Vec<(String, SeenValue)>,
    panel: Option<PanelAt>,
}

static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
const BORE: f64 = 0.3;

fn stub(input: &ElecGenInput<'_>) -> Result<GenOutput, String> {
    stub_output(input, true)
}

fn narrow_stub(input: &ElecGenInput<'_>) -> Result<GenOutput, String> {
    stub_output(input, false)
}

fn stub_output(input: &ElecGenInput<'_>, stepped: bool) -> Result<GenOutput, String> {
    let seen = Seen {
        part: input.part.to_string(),
        params: input.params.iter().map(|(key, value)| {
            (key.to_string(), match value {
                ParamValue::Num(n) => SeenValue::Num(*n),
                ParamValue::Bool(b) => SeenValue::Bool(*b),
                ParamValue::Text(s) => SeenValue::Text(s.to_string()),
                ParamValue::Colour(c) => SeenValue::Colour(*c),
            })
        }).collect(),
        panel: input.panel,
    };
    *SEEN.lock().map_err(|_| "input recorder is poisoned")? = Some(seen);
    let mut body = Csg::default();
    body.add(Prim::cuboid("body", [0.0; 3], [2.0; 3]));
    let (back, thickness) = input.panel.map_or((10.0, 1.6), |panel| (panel.back, panel.thickness));
    let mut cut = Csg::default();
    cut.add(Prim::cylinder(
        "bore",
        [0.0, 0.0, back - 1.0],
        BORE,
        thickness + 2.0,
        [0.0, 0.0, 1.0],
    ));
    if stepped {
        if let Some(radius) = input.params.iter().find_map(|(key, value)| match value {
            ParamValue::Num(n) if *key == "radius" => Some(n * 1000.0),
            _ => None,
        }) {
            let step = back + thickness / 2.0 - 0.2;
            cut.add(Prim::cylinder("upper opening", [0.0, 0.0, step], radius,
                back + thickness + 1.0 - step, [0.0, 0.0, 1.0]));
        }
    }
    Ok(GenOutput {
        bodies: vec![GenBody {
            name: "body".into(),
            csg: body,
            material: MaterialId(0),
            finish: FinishId(0),
            colour: Colour::hex(0xabcdef),
        }],
        panel_cuts: vec![cut],
        panel_adds: Vec::new(),
        board_items: vec![Item::Hole(Hole {
            id: ItemId(0),
            name: "mount".into(),
            at: P::ZERO,
            drill: mm(1.0),
            pad: None,
            net: None,
            plated: false,
            origin: Origin::Derived { from: FeatureRef::Generated(input.part.to_string()) },
        })],
        warnings: Vec::new(),
        notes: Vec::new(),
    })
}

fn opening_radii(port: &PlacedPort) -> (f64, f64) {
    let opening = port.profile.offset(port.margin, mm(0.02));
    let vertices = &opening.polygons().first().expect("the light port has an opening polygon").outer;
    let centre = vertices.iter().fold([0.0; 2], |sum, p| [sum[0] + to_mm(p.x), sum[1] + to_mm(p.y)])
        .map(|sum| sum / vertices.len() as f64);
    assert!(centre[0].hypot(centre[1]) < 1e-6, "the fixture's light opening is centred on its mouth");
    vertices.iter().map(|p| (to_mm(p.x) - centre[0]).hypot(to_mm(p.y) - centre[1]))
        .fold((f64::INFINITY, 0.0_f64), |(inner, outer), radius| (inner.min(radius), outer.max(radius)))
}

/// Cross a closed final mesh with a ray tilted away from the roof's axes.
fn inside(mesh: &TriMesh, p: [f64; 3]) -> bool {
    let d = [0.5773, 0.5774, 0.5775];
    let mut n = 0;
    for triangle in &mesh.triangles {
        let [a, b, c] = triangle.map(|i| { let v = mesh.vertices[i as usize]; [v.x, v.y, v.z] });
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let h = [d[1] * e2[2] - d[2] * e2[1], d[2] * e2[0] - d[0] * e2[2], d[0] * e2[1] - d[1] * e2[0]];
        let det = e1[0] * h[0] + e1[1] * h[1] + e1[2] * h[2];
        if det.abs() < 1e-12 { continue; }
        let s = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
        let u = (s[0] * h[0] + s[1] * h[1] + s[2] * h[2]) / det;
        let q = [s[1] * e1[2] - s[2] * e1[1], s[2] * e1[0] - s[0] * e1[2], s[0] * e1[1] - s[1] * e1[0]];
        let v = (d[0] * q[0] + d[1] * q[1] + d[2] * q[2]) / det;
        let t = (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) / det;
        if u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t > 0.0 { n += 1; }
    }
    n % 2 == 1
}

#[test]
fn the_generator_maps_parameters_panel_geometry_and_board_items() {
    let design = Arc::new(ledctl());
    let unit = &design.units[0];
    let board = unit.board.as_deref().expect("the fixture has a board");
    let led = board.placed.iter().find(|placed| placed.refdes == "D1")
        .expect("the fixture places its first LED");
    let other_led = board.placed.iter().find(|placed| placed.refdes == "D2")
        .expect("the fixture places its second LED without a generated part");
    let mech = board_mech(board, &unit.circuit);
    let light = |comp| mech.ports.iter().find(|port| port.comp == comp && port.kind == BodyPortKind::Light)
        .expect("the fixture LED has a light port");
    let port = light(led.comp);
    let other_port = light(other_led.comp);
    let (r_in, r_out) = opening_radii(port);
    let (_, other_r_out) = opening_radii(other_port);
    assert!(BORE < r_in, "the 0.3 mm bore is smaller than the actual light opening");
    // Half a millimetre past the opening, in whole tenths so the script literal is the value it reads.
    let radius = ((r_out + 0.5) * 10.0).ceil() / 10.0;
    let probe = (BORE + r_in) / 2.0;
    let key = fab_elec_mech::keys::feature_key(&FeatureRef::Port(CompRef { board: mech.unit, comp: port.comp }, port.index))
        .expect("the light port has a feature key");
    let mut inputs = CaseInputs::default();
    inputs.update(board_inputs(&unit.name, board, &unit.circuit, &board.drills()));
    let sink: BoardItemSink = Arc::new(Mutex::new(BTreeMap::new()));
    let generator = elec_generator("stub", stub, design.clone(), sink.clone());
    let env = inputs.env().with_generator("stub", generator.clone());
    let mut evaluator = Evaluator::new(Machine::dual_nozzle_350());

    let source = format!(
        "{CASE}\nlet part = m.generated(\"stub\", {{panel: lid on: {:?} height: 5mm radius: {radius}mm mode: @polished colour: #12345678 enabled: true omitted: nil}})\nobject(part, name: \"Part\")\n",
        led.refdes,
    );
    let mut model = FabModel::with_script("case", "case.cad.splash", &source);
    model.set_env(env.clone());
    let (compiled, evaluated) = model.evaluate(&mut evaluator, None)
        .expect("the enclosure with a generated request evaluates");
    let seen = SEEN.lock().expect("the recorder is available").take()
        .expect("evaluation invoked the generator");
    assert_eq!(seen.part, "part", "the script part name reaches the generator");
    assert_eq!(seen.params, vec![
        ("colour".into(), SeenValue::Colour(0x12345678)),
        ("enabled".into(), SeenValue::Bool(true)),
        ("height".into(), SeenValue::Num(0.005)),
        ("mode".into(), SeenValue::Text("polished".into())),
        ("radius".into(), SeenValue::Num(radius * 0.001)),
    ], "parameters retain VM units and scalar types, without on, panel or nil keys");

    // The standoff holds an M3 heat-set insert: its 5 mm hole and the 1.2 mm floor under it, less
    // the case's 1.6 mm floor.
    let standoff = 3.0_f64.max(to_mm(mech.envelope[1]) + 1.0).max(5.0 + 1.2 - 1.6);
    let y0 = 1.6 + standoff;
    let board_top = y0 + to_mm(board.stackup.thickness);
    let roof_in = board_top + to_mm(mech.envelope[0]) + 1.0;
    let panel = seen.panel.expect("the LED's +z ray meets the intact lid roof");
    assert!((panel.back - (roof_in - board_top)).abs() < 1e-6,
        "panel back is measured from the footprint's top-face origin: {panel:?}");
    assert!((panel.thickness - 1.6).abs() < 1e-6,
        "panel thickness is the enclosure's 1.6 mm roof: {panel:?}");
    assert!(panel.normal.iter().zip([0.0, 0.0, -1.0]).all(|(a, b)| (a - b).abs() < 1e-6),
        "the roof entry normal points down in the part frame: {panel:?}");

    let outline = mech.outline.iter().fold(BBox::EMPTY, |bounds, contour| bounds.union(contour.bbox()));
    let cx = (to_mm(outline.min.x) + to_mm(outline.max.x)) / 2.0;
    let cy = (to_mm(outline.min.y) + to_mm(outline.max.y)) / 2.0;
    let expected = [to_mm(led.at.x) - cx, board_top, cy - to_mm(led.at.y)];
    let part = evaluated.objects.iter().find(|object| object.name == "case/part")
        .expect("the generated part is an evaluated object");
    let (min, max) = part.bounds.expect("the generated cube has model bounds");
    for axis in 0..3 {
        assert!(((min[axis] + max[axis]) / 2.0 - expected[axis]).abs() < 1e-6,
            "the generated cube is centred over the LED on model axis {axis}: {min:?}..{max:?}");
    }
    let lid = evaluator.object_mesh("case/lid").expect("the final Lid has a mesh in model coordinates");
    let roof_point = |port: &PlacedPort, radius, height| [to_mm(port.at[0]) - cx + radius, roof_in + height, cy - to_mm(port.at[1])];
    assert!(inside(&lid, roof_point(port, probe, 0.2)), "D1 retains roof material outside the bore below the claimed opening's step");
    assert!(!inside(&lid, roof_point(port, 0.0, 0.2)), "D1's generated bore opens the roof on the LED axis");
    assert!(!inside(&lid, roof_point(port, (r_out + radius) / 2.0, 1.2)), "D1's wide generated opening cuts the upper roof beyond the original port");
    assert!(!inside(&lid, roof_point(other_port, 0.0, 0.2)), "D2 keeps its automatic case opening without a generated part");
    assert!(inside(&lid, roof_point(other_port, other_r_out + 0.3, 0.2)), "D2 keeps roof material outside its automatic opening");
    let case = case_solids(&evaluated, "ledctl");
    assert_eq!(case.iter().map(|solid| solid.name.as_str()).collect::<Vec<_>>(), ["Lid"],
        "case_solids finds the final Lid through the panel's inherited base frame");

    // The fixture's -90 degree LED placement is a gimbal pole after mapping
    // the board into the case. A symmetric cube alone cannot prove orientation.
    let generated = compiled.instances[0].script.generated.get("part")
        .expect("pass two retains the generated solid");
    assert_eq!(generated.claims, vec![key.clone()], "the stepped cut claims exactly D1's light port key");
    let mut graph = Graph::new();
    let root = (generated.solid)(&mut graph).expect("the generated solid lowers into a graph");
    let Op::Transform { at, rotate, scale, .. } = &graph.node(root).op else {
        panic!("the generated solid ends in its part-to-model transform");
    };
    let matrix = transform_matrix(*at, *rotate, *scale);
    let pose = fab_elec_mech::scene::placement_pose(led, board.stackup.thickness);
    for axis in 0..3 {
        let mut direction = [0.0; 3];
        direction[axis] = 1.0;
        let board_direction = pose.rotate(direction);
        let model_direction = [board_direction[0], board_direction[2], -board_direction[1]];
        for row in 0..3 {
            assert!((matrix[row][axis] - model_direction[row]).abs() < 1e-12,
                "Euler decomposition preserves the gimbal-pole orientation at ({row}, {axis})");
        }
    }
    {
        let items = sink.lock().expect("the item sink is available");
        let (board_id, items) = items.get("part").expect("the sink contains the generated part");
        assert_eq!(*board_id, unit.id, "generated board items identify the anchor's unit");
        assert_eq!(items.len(), 1, "the generator publishes exactly one board item");
        let Item::Hole(hole) = &items[0] else { panic!("the generated board item is a hole") };
        assert_eq!(hole.at, led.at, "the footprint-origin hole lands at the LED's board position");
    }

    let narrow = elec_generator("stub", narrow_stub, design.clone(), sink.clone());
    let mut narrow_model = FabModel::with_script("case", "case.cad.splash", &source);
    narrow_model.set_env(env.clone().with_generator("stub", narrow));
    let (narrow_compiled, _) = narrow_model.evaluate(&mut evaluator, None)
        .expect("the enclosure with only the narrow bore evaluates");
    let narrow_part = narrow_compiled.instances[0].script.generated.get("part")
        .expect("the narrow generated part is retained");
    assert!(narrow_part.claims.is_empty(), "a bore smaller than the port opening claims no keys");
    let narrow_lid = evaluator.object_mesh("case/lid").expect("the narrow case has a final Lid mesh");
    assert!(!inside(&narrow_lid, roof_point(port, probe, 0.2)),
        "an unclaimed case opening remains open outside the narrow bore in the lower roof");
    let _ = SEEN.lock().expect("the recorder is available").take();

    // The light pipe itself through the same adapter: its 3 mm panel hole covers D1's opening, so
    // it takes the port over; D2 keeps its own; derived::place puts its keepout round the LED.
    let pipe_entry = fab_elec_mech::generate::generator("light_pipe").expect("the light pipe has an entry");
    let pipe_sink: BoardItemSink = Arc::new(Mutex::new(BTreeMap::new()));
    let pipe = elec_generator("light_pipe", pipe_entry, design.clone(), pipe_sink.clone());
    let pipe_source = format!("{CASE}\nlet pipe = m.generated(\"light_pipe\", {{panel: lid on: {:?}}})\nobject(pipe, name: \"Pipe\")\n", led.refdes);
    let mut pipe_model = FabModel::with_script("case", "case.cad.splash", &pipe_source);
    pipe_model.set_env(env.clone().with_generator("light_pipe", pipe));
    let (pipe_compiled, _) = pipe_model.evaluate(&mut evaluator, None)
        .expect("the enclosure with a light pipe over D1 evaluates");
    let pipe_part = pipe_compiled.instances[0].script.generated.get("pipe")
        .expect("the light pipe is a generated part");
    assert_eq!(pipe_part.claims, vec![key.clone()], "the pipe's hole covers D1's opening and takes it over");
    let pipe_lid = evaluator.object_mesh("case/lid").expect("the pipe case has a final Lid mesh");
    assert!(!inside(&pipe_lid, roof_point(port, 0.0, 0.8)), "the pipe's hole opens the roof over D1");
    assert!(!inside(&pipe_lid, roof_point(other_port, 0.0, 0.8)), "D2 keeps its own opening beside the pipe");
    {
        let items = pipe_sink.lock().expect("the pipe's item sink is available");
        let (_, items) = items.get("pipe").expect("the sink holds the pipe's board items");
        let keepout = items.iter().find_map(|item| match item { Item::Keepout(keepout) => Some(keepout), _ => None })
            .expect("the pipe keeps footprints out under its body");
        let centre = keepout.area.bbox().center();
        assert!((centre.x - led.at.x).abs() <= mm(0.01) && (centre.y - led.at.y).abs() <= mm(0.01),
            "derived::place puts the pipe's keepout round the LED: {centre:?} vs {:?}", led.at);
    }

    // A complete evaluation above keeps subsequent generator failures from
    // falling back to the evaluator's initial, unresolved preview.
    for (params, cause) in [
        ("{height: 5mm}", "on: names the part it sits on"),
        ("{on: \"missing-reference\"}", "refdes 'missing-reference' was not found"),
        ("{on: \"D1\" height: [5mm]}", "'height': a list or an object is not a generator parameter"),
    ] {
        let source = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet part = m.generated(\"stub\", {params})\nobject(part)\n");
        let mut invalid = FabModel::with_script("case", "case.cad.splash", &source);
        invalid.set_env(env.clone());
        let failure = invalid.evaluate(&mut evaluator, None).expect_err("an invalid request fails evaluation");
        assert!(failure.message.contains(cause), "the generator error names its cause '{cause}': {failure}");
    }

    let text = "quote: \" slash: \\ newline: \n tab: \t null: \0 scalar: \u{7f} λ";
    let params = format!("{{on: {:?}, \"text key\": {text:?}, small: 1e-7, enabled: false, omitted: nil}}", format!("{}/{}", unit.name, led.refdes));
    generator(&GenInput { kind: "stub", part: "part", params: &params, panel: None, inputs: &env.inputs })
        .expect("qualified anchors and canonical escaped strings are accepted");
    let seen = SEEN.lock().expect("the recorder is available").take()
        .expect("the direct request invoked the generator");
    assert!(seen.panel.is_none(), "without a panel the generator keeps its own distances");
    assert_eq!(seen.params, vec![
        ("text key".into(), SeenValue::Text(text.into())),
        ("small".into(), SeenValue::Num(1e-7)),
        ("enabled".into(), SeenValue::Bool(false)),
    ], "Rust debug escapes, quoted keys, exponents, false and nil retain their meanings");
    {
        let items = sink.lock().expect("the item sink is available");
        assert_eq!(items.len(), 1, "rerunning a part replaces its sink entry");
        assert_eq!(items["part"].1.len(), 1, "rerunning a part does not append duplicate board items");
    }
    for (params, cause) in [
        ("{on: \"D1\", height: {x: 1}}", "'height': a list or an object"),
        ("{on: \"D1\", height: Proto{x: 1}}", "'height': a list or an object"),
        ("{on: \"D1\", height: 1e-}", "byte"),
        ("{on: \"D1\", colour: #1234567}", "byte"),
        (r#"{on: "D1", text: "\u{d800}"}"#, "byte"),
        (r#"{on: "D1", text: "\q"}"#, "byte"),
        ("{on: \"D1\"} trailing", "byte"),
    ] {
        let failure = generator(&GenInput { kind: "stub", part: "part", params, panel: None, inputs: &env.inputs })
            .expect_err("invalid canonical parameters are rejected");
        assert!(failure.contains(cause), "parser error names '{cause}' for {params:?}: {failure}");
    }
}

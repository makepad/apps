use super::*;
use eval::Evaluator;
use fab_plate::machine::Machine;

/// The test document's one script and its instance.
const F: &str = "t.cad.splash";

#[test]
fn a_script_rebuilds_only_for_the_inputs_it_reads() {
    let m = FabModel::new("test");
    let solved = program::Solved::new();
    let inputs = |b1: u8, b2: Option<u8>| lang::Inputs {
        kinds: vec!["board".into()],
        values: [(("board".to_string(), "b1".to_string()), lang::Input { source: "{width: 30mm}".into(), hash: [b1; 16] })]
            .into_iter()
            .chain(b2.map(|h| (("board".to_string(), "b2".to_string()), lang::Input { source: format!("{{width: {}mm}}", 40 + h as u32).into(), hash: [h; 16] })))
            .collect(),
        lazy: None,
    };
    let env = ScriptEnv::new(Vec::new(), inputs(1, Some(2)));
    let key = |source: &str, env: &ScriptEnv| script_key(F, source, &m.assets, &m.facets, &m.pictures, &m.context, env, &[], m.state(), &solved, &[]);
    let literal = src("let b = board(\"b1\")");
    assert_eq!(key(&literal, &env), key(&literal, &env.clone()), "unchanged inputs keep the cached compile");
    assert_eq!(key(&literal, &env), key(&literal, &ScriptEnv::new(Vec::new(), inputs(1, Some(3)))), "an unread input's hash does not invalidate the compile");
    assert_ne!(key(&literal, &env), key(&literal, &ScriptEnv::new(Vec::new(), inputs(4, Some(2)))), "a read input's hash invalidates the compile");
    let dynamic = src("let name = \"b1\"\nlet b = board(name)");
    assert_ne!(key(&dynamic, &env), key(&dynamic, &ScriptEnv::new(Vec::new(), inputs(1, Some(3)))), "a computed name depends on every input of its kind");
    let qualified = src("let b = mod.fab.board(\"b2\")");
    assert_ne!(key(&qualified, &env), key(&qualified, &ScriptEnv::new(Vec::new(), inputs(1, Some(3)))), "a call through the module path is a read");
    let only_b2 = ScriptEnv::new(Vec::new(), lang::Inputs { values: inputs(9, Some(2)).values.into_iter().filter(|((_, n), _)| n == "b2").collect(), ..inputs(1, None) });
    assert_ne!(key(&literal, &env), key(&literal, &only_b2), "a missing named input has its own cache tag");
    let shelf = |source: &str| lang::Library { file: "shelf.splash".into(), source: source.into() };
    let library_key = key(&literal, &ScriptEnv::new(vec![shelf("{width: 30mm}")], inputs(1, Some(2))));
    assert_ne!(library_key, key(&literal, &env), "adding a library invalidates the compile");
    assert_ne!(library_key, key(&literal, &ScriptEnv::new(vec![shelf("{width: 40mm}")], inputs(1, Some(2)))), "library source changes invalidate the compile");
    assert_ne!(library_key, key(&literal, &ScriptEnv::new(vec![lang::Library { file: "rack.splash".into(), source: "{width: 30mm}".into() }], inputs(1, Some(2)))), "library file changes invalidate the compile");

    // What an evaluation reads is within what the key holds: the document's calls and the
    // libraries' own.
    let wide = lang::Library { file: "wide.splash".into(), source: "let fab = mod.fab\nfn wide() { return fab.board(\"b2\").width }\nlet lib = {wide: wide}\nlib".into() };
    let reads_both = src("let b = board(\"b1\")\nobject(m.box({size: [wide() b.width 2mm]}), name: \"a\")");
    let with_lib = |b2: u8| ScriptEnv::new(vec![wide.clone()], inputs(1, Some(b2)));
    let mut model = FabModel::with_script("test", F, &reads_both);
    model.set_env(with_lib(2));
    let first = model.compile_script(F).expect("a library fn reads an input");
    let syntax = input_reads(&reads_both, &model.env);
    assert_eq!(first.program.inputs_read, vec![("board".to_string(), "b1".to_string()), ("board".to_string(), "b2".to_string())], "the evaluation reads the document's input and the library's");
    assert!(first.program.inputs_read.iter().all(|r| syntax.contains(r)), "every read is in the key's set: {:?} in {syntax:?}", first.program.inputs_read);
    assert_ne!(key(&reads_both, &with_lib(2)), key(&reads_both, &with_lib(3)), "a library's read input is in the key");

    // A read no syntax shows (an aliased call) is still never served stale.
    let aliased = src("let f = board\nlet w = f(\"b2\").width\nobject(m.box({size: [w 2mm 2mm]}), name: \"a\")");
    let mut model = FabModel::with_script("test", F, &aliased);
    model.set_env(ScriptEnv::new(Vec::new(), inputs(1, Some(2))));
    let a = model.compile_script(F).expect("an aliased input call evaluates");
    assert!(Arc::ptr_eq(&a, &model.compile_script(F).unwrap()), "nothing moved: the cached compile serves");
    model.set_env(ScriptEnv::new(Vec::new(), inputs(1, Some(3))));
    let b = model.compile_script(F).unwrap();
    assert!(!Arc::ptr_eq(&a, &b), "the aliased input moved: the script compiles again");

    // The plate's content follows what the script read, so the slice and the views take a
    // rebuilt part as a new plate; an input it does not read changes nothing downstream.
    let mut ev = Evaluator::new(Machine::dual_nozzle_350());
    let mut content_with = |b1: u8, b2: u8| {
        model.set_env(ScriptEnv::new(Vec::new(), inputs(b1, Some(b2))));
        model.evaluate(&mut ev, None).expect("the aliased script builds").1.content
    };
    let (read, moved, unread) = (content_with(1, 2), content_with(1, 3), content_with(4, 3));
    assert_ne!(read, moved, "a read input that moved is a new plate");
    assert_eq!(moved, unread, "an input the script does not read leaves the plate as it was");

    // A kind only a lazy provider serves keys on the provider: making the env asks it nothing,
    // the compile's first read asks it once, the same provider keeps the cached compile and
    // another compiles again.
    struct Widths(std::sync::atomic::AtomicUsize);
    impl lang::Provide for Widths {
        fn provide(&self, _kind: &str, name: &str) -> Option<lang::Input> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (name == "lazy").then(|| lang::Input { source: "{width: 7mm}".into(), hash: [7; 16] })
        }
        fn names(&self, _kind: &str) -> Vec<String> {
            vec!["lazy".into()]
        }
        fn hash(&self, _kind: &str, _name: &str) -> Option<[u8; 16]> {
            None
        }
    }
    let widths = Arc::new(Widths(Default::default()));
    let asked = || widths.0.load(std::sync::atomic::Ordering::Relaxed);
    let lazy_env = || ScriptEnv::new(Vec::new(), lang::Inputs { kinds: vec!["board".into()], values: Default::default(), lazy: Some(lang::Lazy::new(widths.clone())) });
    let mut model = FabModel::with_script("test", F, &src("let w = board(\"lazy\").width\nobject(m.box({size: [w 2mm 2mm]}), name: \"a\")"));
    model.set_env(lazy_env());
    assert_eq!(asked(), 0, "making the env asks the provider nothing");
    let a = model.compile_script(F).expect("a lazy input builds at the compile's first read");
    assert_eq!(asked(), 1, "the compile asked the provider once");
    assert!(Arc::ptr_eq(&a, &model.compile_script(F).unwrap()), "the same provider serves the cached compile");
    model.set_env(lazy_env());
    assert!(!Arc::ptr_eq(&a, &model.compile_script(F).unwrap()), "another provider compiles again");
}

fn setup() -> (FabModel, Evaluator) {
    (FabModel::with_script("test", F, &src("")), Evaluator::new(Machine::dual_nozzle_350()))
}

/// A script of `body` (after the `use` lines).
fn src(body: &str) -> String {
    format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\n{body}")
}

/// A script edit of the test script.
fn sc(edit: Edit) -> Change {
    Change::Script { file: F.into(), edit }
}

/// A placement change of object `object` of the test instance.
fn pl(object: &str, arg: PlaceArg) -> Change {
    Change::Place { instance: "t".into(), object: object.into(), arg }
}

fn apply(m: &mut FabModel, e: &mut Evaluator, id: &str, changes: Vec<Change>) -> (Applied, Compiled, Evaluated) {
    let head = m.head();
    let (applied, evaluated) = m.apply(e, id, head, &changes, None).unwrap_or_else(|err| panic!("{id}: {err}"));
    let (c, ev) = evaluated.expect("a fresh apply evaluates");
    (applied, c, ev)
}

fn program(m: &mut FabModel, e: &mut Evaluator, id: &str, body: &str) -> (Applied, Compiled, Evaluated) {
    apply(m, e, id, vec![sc(Edit::Program { source: src(body) })])
}

/// The test script's compiled program.
fn prog(c: &Compiled) -> &makepad_model_script::lang::Program {
    &c.instances[0].script.program
}

const BRACKET: &str = "let plate = m.box({size: [60mm 6mm 40mm]})\nlet hole = m.cylinder({radius: 4mm height: 20mm segments: 48})\nlet bracket = m.subtract(plate, hole)\nobject(bracket)\n";

#[test]
fn part_steps_follow_the_written_build_and_model_edits() {
    let source = src(r#"let edge = param(5mm, min: 1mm, max: 6mm)
let profile = Rect{size: [12mm 12mm]}
let l = m.brep.extrude({sketch: profile height: 8mm})
let eased = m.brep.fillet(
    l, 0.1cm, {edges: m.edge(l.bottom, l.side(0))}
)
let rounded = m.brep.fillet(eased, edge, {edges: m.edge(l.top, l.side(2))})
let rib = param(2mm)
let width = rib
let gussets = for x in [-1, 1] { m.brep.box({size: [width 2mm 2mm] at: [x * 6mm, 0, 0]}) }
let braced = m.brep.union(rounded, gussets)
let holes = for x in [-1, 1] {
    [m.brep.cylinder({radius: 1mm height: 8mm at: [x * 3mm, 0, 0]}),
     m.brep.cylinder({radius: 2mm height: 2mm at: [x * 3mm, 3mm, 0]})]
}
let drilled = m.brep.subtract(braced, holes)
let bracket = m.brep.chamfer(drilled, 0.6mm, {edges: m.edge(l.top, l.side(0))})
object(bracket)
let twin = m.brep.mirror(l, {plane: @yz})
fn make_pin(edge) { return m.brep.cylinder({radius: edge height: 2mm}) }
let pin = make_pin(1mm)
object([twin, pin], name: "Pair")
let inset_size = [4mm 4mm 4mm]
let inset_radius = 0.2mm
let inset = m.brep.fillet(m.brep.box({size: inset_size}), inset_radius)
object(inset)
let block = m.brep.box({size: [8mm 8mm 8mm]})
object(block)
fn by_parameter(block) { return block.top }
fn by_local(other) {
    if other {
        let block = [block.front, other]
        return block.len()
    }
    return block.back
}
"#);
    let mut model = FabModel::with_script("test", F, &source);
    let script = model.compile_script(F).expect("the step examples compile without meshing");
    let lists = steps::object_steps(&script, "bracket");
    let [(body, spine)] = lists.as_slice() else { panic!("the bracket has one history") };
    assert_eq!(spine, &steps::steps(&script, *body), "body and object queries give the same history");
    let ids: Vec<_> = spine.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["l", "eased", "rounded", "braced", "drilled", "bracket"], "only spine solids are steps");
    assert_eq!(spine.iter().map(|s| s.verb.as_ref()).collect::<Vec<_>>(), ["Extrude", "Round", "Round", "Join", "Cut", "Chamfer"], "the history uses the tools' verbs");
    assert_eq!(spine.iter().map(|s| s.value.as_deref()).collect::<Vec<_>>(), [Some("8 mm"), Some("1 mm"), Some("5 mm"), None, None, Some("0.6 mm")], "literals and declared parameters use display units");
    assert_eq!(spine[3].uses.iter().map(ToString::to_string).collect::<Vec<_>>(), ["+gussets ×2"], "the join groups the two loop boxes");
    assert_eq!(spine[4].uses.iter().map(ToString::to_string).collect::<Vec<_>>(), ["−holes ×4"], "the cut counts both cylinders from each pass");
    for (i, step) in spine.iter().enumerate() {
        assert_eq!(step.n, i + 1, "step numbers start at one in build order");
        let line = source.lines().nth(step.line as usize - 1).expect("the step has a source line");
        assert!(line.starts_with(&format!("let {} =", step.id)), "step {} points to its own builder line", step.n);
    }
    assert!(spine.windows(2).all(|w| w[0].line < w[1].line), "the bracket's lines follow its construction");
    assert_eq!(spine[0].end_line, spine[0].line, "a single-line builder ends on its own line");
    let round = &spine[1];
    assert_eq!(round.end_line, round.line + 2);
    for line in round.line..=round.end_line {
        assert_eq!(steps::step_of_line(spine, line), Some(1), "line {line} belongs to the multiline round");
    }
    assert_eq!(steps::step_of_line(spine, 1), Some(0), "before the first builder");
    assert_eq!(steps::step_of_line(spine, spine[2].end_line + 1), Some(3), "an operand between steps points to its join");
    assert_eq!(steps::step_of_line(spine, spine.last().unwrap().end_line + 1), None, "after the last step");
    assert_eq!(spine[0].shared_with, ["Pair"], "the base names the other object built on it");
    let pair = steps::object_steps(&script, "Pair");
    assert_eq!(pair.len(), 2, "a multi-body object has a history for each body");
    assert_eq!(pair[0].1[0].shared_with, ["bracket"], "sharing is visible from either object's history");
    let pin = pair[1].1.last().expect("the pin has a step");
    assert_eq!(pin.made_in.as_deref(), Some("make_pin"), "the function-made solid names its maker");
    assert_eq!(pin.value.as_deref(), Some("edge"), "a function argument that shadows a declared param shows its written expression");
    let inline = steps::object_steps(&script, "inset");
    assert_eq!(inline[0].1.iter().map(|s| (s.id.as_str(), s.verb.as_ref())).collect::<Vec<_>>(), [("inset.box", "Box"), ("inset", "Round")], "the inline box keeps its own id and step");

    let after = |solid: &str, code: &str| sc(Edit::After { solid: solid.into(), code: code.into() });
    let (_, compiled) = model.edited(&[
        after("block", "m.brep.fillet(block, 1mm, {edges: m.edge(block.top, block.front)})"),
        after("block", "m.brep.chamfer(block, 1mm, {edges: m.edge(block_box.bottom, block_box.back)})"),
        after("block_fillet", "m.brep.fillet(block_fillet, 1mm, {edges: m.edge(block_box.top, block_box.back)})"),
        after("block_fillet2", "m.brep.fillet(block_fillet2, 1mm, {edges: m.edge(block_box.bottom, block_box.front)})"),
        after("block", "m.brep.shell(block, 0.5mm)"),
        Change::Param { instance: "t".into(), name: "edge".into(), value: Some(0.003) },
    ]).expect("the tool edits and parameter override compile through the model");
    let script = &compiled.instances[0].script;
    let edited = steps::object_steps(script, "block");
    assert_eq!(edited[0].1.iter().map(|s| (s.id.as_str(), s.verb.as_ref())).collect::<Vec<_>>(), [
        ("block_box", "Box"), ("block_fillet3", "Round"), ("block_fillet2", "Round"),
        ("block_fillet", "Round"), ("block_chamfer", "Chamfer"), ("block", "Shell"),
    ], "After numbers repeated fillets, names a different kind for its own builder, and keeps the steps in build order");
    let text = script.source();
    assert!(text.contains("fn by_parameter(block) { return block.top }"), "a parameter keeps its own dotted reads: {text}");
    assert!(text.contains("let block = [block_box.front, other]"), "a local binding's initializer can still read the original box: {text}");
    assert!(text.contains("return block.len()"), "a dotted read after the local binding belongs to the local: {text}");
    assert!(text.contains("return block_box.back"), "a read after the nested block belongs to the original box again: {text}");
    assert_eq!(steps::object_steps(script, "bracket")[0].1[2].value.as_deref(), Some("3 mm"), "the headline follows this instance's current param value");

    let rollback = |solid: &str, to: &str| sc(Edit::Truncate { solid: solid.into(), to: to.into() });
    let mut eval = Evaluator::new(Machine::dual_nozzle_350());
    let (_, rolled, ev) = apply(&mut model, &mut eval, "shorten", vec![rollback("bracket", "rounded")]);
    assert!(ev.plate.parts.iter().any(|p| p.object == "t/bracket"));
    let short = steps::part_steps(&rolled, "t/bracket");
    assert_eq!(short.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["l", "eased", "bracket"]);
    assert_eq!((&short[2].verb, &short[2].value), (&spine[2].verb, &spine[2].value));
    let text = model.source(F).unwrap();
    assert!(text.contains("let bracket = m.brep.fillet(eased, edge, {edges: m.edge(l.top, l.side(2))})"));
    assert!(!text.contains("gussets") && !text.contains("let holes") && !text.contains("let width"));
    assert!(text.contains("let edge = param(") && text.contains("let rib = param("));
    assert_eq!(model.undo_labels().next(), Some("t: roll back bracket to rounded"));
    model.history(&mut eval, model.head(), HistoryAction::Undo, None).expect("one undo restores the whole script");
    assert_eq!(model.source(F), Some(source.as_str()));

    let line = source.lines().position(|line| line.starts_with("let twin")).unwrap() + 1;
    let shared = model.apply(&mut eval, "shared", model.head(), &[rollback("bracket", "l")], None).unwrap_err();
    assert!(shared.to_string().contains(&format!("l is also read on line {line}")), "{shared}");
    let off = model.apply(&mut eval, "off spine", model.head(), &[rollback("bracket", "pin")], None).unwrap_err();
    assert!(off.to_string().contains("l, eased, rounded, braced, drilled, bracket"), "{off}");
    assert_eq!(model.source(F), Some(source.as_str()), "both refusals leave the script intact");

    let (_, rolled, _) = apply(&mut model, &mut eval, "inline", vec![rollback("inset", "inset.box")]);
    assert_eq!(steps::part_steps(&rolled, "t/inset").iter().map(|s| (s.id.as_str(), s.verb.as_ref())).collect::<Vec<_>>(), [("inset", "Box")]);
    let text = model.source(F).unwrap();
    assert!(text.contains("let inset = m.brep.box({size: inset_size})") && text.contains("let inset_size") && !text.contains("inset_radius"));

    let shared = format!("{source}object(\n    braced, name: \"Brace\"\n)\n");
    let line = shared.lines().count() - 1;
    let mut other = FabModel::with_script("test", F, &shared);
    let err = other.apply(&mut eval, "shared removed step", other.head(), &[rollback("bracket", "rounded")], None).unwrap_err();
    assert!(err.to_string().contains(&format!("line {line} reads braced: roll back would leave it unbound")), "{err}");
}

#[test]
fn a_refused_insert_names_the_committed_step_that_lost_its_edge() {
    let (mut model, mut eval) = setup();
    let (_, compiled, _) = program(&mut model, &mut eval, "build", r#"let hole = m.brep.cylinder({radius: 2mm height: 40mm})
let a = m.brep.box({size: [40mm 20mm 30mm]})
let b = m.brep.fillet(a, 1mm, {edges: m.edge(a.top, a.front)})
let c = m.brep.subtract(b, hole)
object(c)
"#);
    let steps = steps::part_steps(&compiled, "t/c");
    assert_eq!(steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"], "the committed spine has three adjacent builders");
    let head = model.head();
    let cut_top = sc(Edit::After { solid: "a".into(), code: "m.brep.subtract(a, m.brep.box({size: [60mm 4mm 60mm] at: [0 10mm 0]}))".into() });
    let err = model.apply(&mut eval, "cut top", head, &[cut_top], None).expect_err("the insert takes away the edge b rounds");
    let ApplyError::Build(f) = err else { panic!("the downstream fillet should refuse the build: {err}") };
    assert_eq!(model.head(), head, "a refused insert leaves the committed history intact");
    assert!(f.id == "b" && f.lost.is_some(), "the evaluator names the fillet whose edge was lost: {f}");
    assert_eq!(steps::step_of_line(&steps, f.line), Some(2), "the candidate's fillet line points at c in the committed source");
    assert_eq!(steps::step_of_failure(&steps, f.line, Some(&f.id)), Some(1), "the solid id points back to b despite the inserted line");
    assert_eq!(steps::step_of_failure(&steps, steps[0].line, Some("hole")), Some(2), "the hole's badge attributes its failure to the cut");
    assert_eq!(steps::step_of_failure(&steps, f.line, None), Some(2), "a program error uses its line");
    assert_eq!(steps::step_of_failure(&steps, f.line, Some("new_solid")), Some(2), "an id absent from the committed steps also uses its line");
}

#[test]
fn a_step_shows_as_it_was_built() {
    let source = src("let base = m.brep.box({size: [20mm 10mm 20mm]})\nlet eased = m.brep.fillet(base, 2mm)\nlet hole = m.brep.cylinder({radius: 3mm height: 40mm})\nlet bracket = m.brep.subtract(eased, hole)\nobject(bracket)\nlet pin = m.brep.cylinder({radius: 2mm height: 8mm})\nobject(pin)\n");
    let model = FabModel::with_script("test", F, &source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let (compiled, ev) = model.evaluate(&mut e, None).expect("the bracket builds");
    let script = compiled.instances[0].script.clone();
    let spine = steps::part_steps(&compiled, "t/bracket");
    assert_eq!(spine, steps::object_steps(&script, "bracket")[0].1, "part_steps is the part's object history");
    assert_eq!(spine.len(), 3, "the bracket's spine is the box, its round and the cut");
    assert!(steps::part_steps(&compiled, "t/nothing").is_empty() && steps::part_steps(&compiled, "nothing").is_empty(), "an unknown part has no steps");
    let at = ev.plate.parts.iter().position(|p| p.object == "t/bracket").expect("the bracket is on the plate");
    let frame = *ev.placed_frame(at).expect("the part has a frame");
    let first = e.step_render(&script, spine[0].solid, &frame, None).expect("step 1 meshes");
    let last = e.step_render(&script, spine.last().unwrap().solid, &frame, None).expect("the last step meshes");
    assert!(!first.indices.is_empty() && !last.indices.is_empty(), "both steps give triangles");
    let bounds = |rm: &fab_plate::RenderMesh| rm.positions.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), p| (std::array::from_fn(|k| lo[k].min(p[k])), std::array::from_fn(|k| hi[k].max(p[k]))));
    let (lo1, hi1) = bounds(&first);
    let (lo2, hi2) = bounds(&last);
    assert!((0..3).all(|k| lo1[k] <= lo2[k] + 1e-3 && hi1[k] >= hi2[k] - 1e-3), "step 1 holds the last step's bounds: {lo1:?} {hi1:?} vs {lo2:?} {hi2:?}");
    assert!(first.indices.len() != last.indices.len(), "the cut changed the mesh");
    assert!(e.step_render(&script, 9999, &frame, None).is_err(), "a solid that does not exist is an error");

    let shown = ev.with_step(at, first.clone());
    assert_eq!(shown.render[at].positions, first.positions, "the part shows the step's mesh");
    assert!(shown.fields[at].is_none() && shown.paint[at].is_none(), "the part draws as a mesh, not from its field");
    assert_eq!(shown.handles[at], ev.handles[at], "the part keeps its place call: the panels read its turn and scale from it");
    let other = 1 - at;
    assert_eq!(shown.render[other].positions, ev.render[other].positions, "the other part keeps its mesh");
    assert_eq!(shown.plate.parts.len(), ev.plate.parts.len(), "the plate keeps its parts");
}

#[test]
fn a_panel_applies_only_unclaimed_base_cuts_and_always_applies_generated_cuts() {
    use crate::program::{GenPart, GenSolid};
    use makepad_csg_graph::Op;
    use makepad_csg_sdf::mesh_field::{MeshData, MeshField};

    let body = r#"let shell = m.box({size: [20mm 2mm 20mm] at: [0 10mm 0] frame: Frame{name: "datum"}})
let opening = m.link(m.cylinder({radius: 2mm height: 4mm at: [0 10mm 0]}), "port")
let unkeyed = m.cylinder({radius: 1mm height: 4mm at: [6mm 10mm 0]})
let lid = m.panel(shell, {cuts: [opening unkeyed]})
object(lid, name: "Lid")
"#;
    for claim in [None, Some(false), Some(true)] {
        let env = ScriptEnv::default().with_generator("post", Arc::new(move |input| {
            let hit = input.panel.ok_or("expected a panel")?.ray([0.0; 3], [0.0, 1.0, 0.0])
                .ok_or("the panel base must still include its opening")?;
            assert!((hit.back - 9.0).abs() < 1e-6 && (hit.thickness - 2.0).abs() < 1e-6,
                "the generator queries the shell before its claimable cuts");
            let cut: GenSolid = Arc::new(|graph| {
                let source = graph.push(Op::Cylinder { radius: 0.5, top_radius: 0.5, height: 4.0, segments: 24 })?;
                graph.push(Op::Transform { source, at: [0.0, 10.0, 0.0], rotate: [0.0; 3], scale: [1.0; 3] })
            });
            Ok(GenPart {
                solid: cut.clone(), adds: Vec::new(), cuts: vec![cut],
                claims: if claim == Some(true) { vec!["port".into(), "".into()] } else { Vec::new() },
            })
        }));
        let source = if claim.is_some() { format!("{body}\nlet p = m.generated(\"post\", {{panel: lid}})\n") } else { body.into() };
        let (mut model, mut evaluator) = setup();
        model.set_env(env);
        let (_, compiled, _) = program(&mut model, &mut evaluator, "claims", &source);
        let mesh = evaluator.object_mesh("t/lid").expect("the composed panel has a mesh");
        let field = MeshField::new(MeshData {
            positions: mesh.vertices.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(),
            triangles: mesh.triangles,
        });
        assert_eq!(field.signed([1.5, 10.0, 0.0]) < 0.0, claim == Some(true),
            "the base opening applies unless a generated part claims its key: {claim:?}");
        assert!(field.signed([0.0, 10.0, 0.0]) > 0.0, "the generated cut still opens the claimed panel: {claim:?}");
        assert!(field.signed([6.0, 10.0, 0.0]) > 0.0, "a cut without a key always applies: {claim:?}");
        let panel = compiled.instances[0].script.program.solid("lid").expect("the panel is retained");
        assert_eq!(panel.frame.as_ref().map(|frame| frame.0.as_str()), Some("datum"), "the panel inherits its base's frame");
    }
}

#[test]
fn generated_parts_keep_their_ids_and_overrides_through_an_inserted_line_and_other_repeats_changing() {
    use crate::program::{GenInput, GenPart};
    use makepad_csg_graph::Op;
    use std::sync::atomic::{AtomicUsize, Ordering};
    // A box 2 mm wide at x = 10 mm times the request's `x`.
    let runs = Arc::new(AtomicUsize::new(0));
    let count = runs.clone();
    let env = ScriptEnv::default().with_generator("post", Arc::new(move |input: &GenInput<'_>| {
        count.fetch_add(1, Ordering::Relaxed);
        let x: f64 = input.params.trim_matches(|c| c == '{' || c == '}').split(':').nth(1)
            .and_then(|v| v.trim().parse().ok()).ok_or("expected {x: n}")?;
        Ok(GenPart {
            solid: Arc::new(move |graph| {
                let source = graph.push(Op::Box { size: [2.0; 3], radius: 0.0 })?;
                graph.push(Op::Transform { source, at: [10.0 * x, 0.0, 0.0], rotate: [0.0; 3], scale: [1.0; 3] })
            }),
            adds: Vec::new(), cuts: Vec::new(), claims: Vec::new(),
        })
    }));
    // One generated call, repeated by a fn called twice and by its loop, each part its own
    // object, and one generated object outside any fn: no `let` names them.
    let script = |gap: &str, first: u32, second: u32| format!("fn posts(n, x0) {{\n    for i in 0..n {{ object(m.generated(\"post\", {{x: x0 + i}})) }}\n}}\n{gap}posts({first}, 1)\nposts({second}, 11)\nobject(m.generated(\"post\", {{x: 50}}))\n");
    let (mut model, mut evaluator) = setup();
    model.set_env(env);
    let ids = |compiled: &Compiled| -> std::collections::BTreeMap<String, String> {
        compiled.instances[0].script.program.gen_requests.iter().map(|r| (r.params.clone(), r.part.clone())).collect()
    };
    let (_, compiled, _) = program(&mut model, &mut evaluator, "3+2", &script("", 3, 2));
    let before = ids(&compiled);
    assert_eq!(before.len(), 6, "six requests with six ids: {before:?}");
    assert_eq!(before.values().collect::<std::collections::HashSet<_>>().len(), 6, "no two parts share an id: {before:?}");
    let pinned = |params: &str, id: &str| if params == "{x: 50}" { id.contains('@') && !id.contains('#') } else { id.contains('#') && !id.contains('@') };
    assert!(before.iter().all(|(params, id)| pinned(params, id)), "a line name outside the fn, ordinals and no line inside it: {before:?}");
    // Stored overrides on part 1 (the first call's first post, x = 1), on the second call's
    // first post (x = 11) and on the object outside the fn: each object is named by its id.
    let colours = [("{x: 1}", "Blue"), ("{x: 11}", "Orange"), ("{x: 50}", "Blue")];
    let set = colours.iter().map(|(params, colour)| pl(&before[*params], PlaceArg::Color(Some(colour.to_string())))).collect();
    apply(&mut model, &mut evaluator, "colours", set);
    let ran = runs.load(Ordering::Relaxed);
    // A line inserted above the calls, then the first loop shrinking from 3 to 2 and to none
    // while the second runs once: every remaining part keeps its id (no line number and no
    // repeat count is in it), so each override stays on its part and no generator reruns.
    let gap = "let gap = 1\n";
    for (step, (first, second), count) in [("line above", (3, 2), 6), ("2+2", (2, 2), 5), ("0+1", (0, 1), 2)] {
        let (_, compiled, _) = program(&mut model, &mut evaluator, step, &script(gap, first, second));
        let after = ids(&compiled);
        assert_eq!(after.len(), count, "{step}: {count} requests: {after:?}");
        let placed = &compiled.instance("t").expect("the test instance").placed;
        for (params, id) in &after {
            assert_eq!(before.get(params), Some(id), "{step}: the part made for {params} keeps its id");
            let colour = placed.iter().find(|p| &p.object == id).unwrap_or_else(|| panic!("{step}: {id} is placed: {placed:?}")).color.clone();
            let want = colours.iter().find(|(p, _)| p == params).map(|(_, c)| *c);
            assert_eq!(colour.as_deref(), want, "{step}: the part made for {params} keeps its own override");
        }
        assert_eq!(runs.load(Ordering::Relaxed), ran, "{step}: no remaining request changed, so no generator reruns");
    }
}

#[test]
fn a_generated_part_cuts_its_panel_and_reruns_only_when_its_request_changes() {
    use crate::program::{GenInput, GenPart, GenSolid, Generator};
    use makepad_csg_graph::{Graph, Op};
    use makepad_csg_sdf::mesh_field::{MeshData, MeshField};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runs = Arc::new(AtomicUsize::new(0));
    let count = runs.clone();
    let generator: Generator = Arc::new(move |input: &GenInput<'_>| {
        // Read the canonical parameter value with the same language that wrote it.
        let params = lang::evaluate(&format!("use mod.fab.*\nread({})", input.params), &lang::Options {
            assets: &Default::default(), images: &Default::default(), extension: "fab",
            libraries: &[], inputs: &lang::NO_INPUTS, unit: program::UNIT,
            roots: &[lang::RootFn { name: "read", args: &["value"], model: false }],
            overrides: Default::default(), weave: None, solved: Default::default(), generated: &Default::default(),
        }).map_err(|e| format!("{e:?}"))?;
        let lang::Arg::Value(lang::V::Obj(value)) = &params.roots[0].args[0].1 else { return Err("expected generator parameters".into()) };
        let Some(lang::V::Arr(at)) = value.get("at") else { return Err("expected at: [x z]".into()) };
        let (x, z) = (at[0].num().unwrap() * program::UNIT, at[1].num().unwrap() * program::UNIT);
        let panel = input.panel.ok_or("expected a panel")?;
        let hit = panel.ray([x, -50.0, z], [0.0, 1.0, 0.0]).ok_or("ray missed the panel")?;
        assert!((hit.back - 59.0).abs() < 1e-6, "ray reports distance to the underside");
        assert!((hit.thickness - 2.0).abs() < 1e-6, "ray reports the panel's thickness");
        assert!(hit.normal[1] < -0.99, "ray reports the entry face's outward normal");
        let y = -50.0 + hit.back;
        let cylinder = move |radius, height, y| -> GenSolid {
            Arc::new(move |graph: &mut Graph| {
                let source = graph.push(Op::Cylinder { radius, top_radius: radius, height, segments: 32 })?;
                graph.push(Op::Transform { source, at: [x, y, z], rotate: [0.0; 3], scale: [1.0; 3] })
            })
        };
        count.fetch_add(1, Ordering::SeqCst);
        Ok(GenPart { solid: cylinder(2.0, 5.0, y - 2.5), adds: Vec::new(), cuts: vec![cylinder(1.0, hit.thickness + 2.0, y + hit.thickness * 0.5)], claims: Vec::new() })
    });
    let env = ScriptEnv::default().with_generator("post", generator);
    let body = "let lid = m.panel(m.box({size: [40mm 2mm 40mm] at: [0 10mm 0]}), {})\nlet p = m.generated(\"post\", {panel: lid at: [5mm, 5mm]})\nobject(lid, name: \"Lid\")\nobject(p, name: \"Post\")\n";
    let (mut m, mut e) = setup();
    m.set_env(env.clone());
    let (_, compiled, _) = program(&mut m, &mut e, "generated", body);
    assert_eq!(compiled.instances[0].script.generated.len(), 1, "published scripts carry pass two's generated parts");
    let lid = e.object_mesh("t/lid").expect("the resolved panel has a mesh");
    let field = MeshField::new(MeshData { positions: lid.vertices.iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect(), triangles: lid.triangles.clone() });
    assert!(field.signed([5.0, 10.0, 5.0]) > 0.0, "the generated cut leaves a hole at the requested position");
    assert!(field.signed([15.0, 10.0, 15.0]) < 0.0, "the panel remains solid away from the cut");
    let post = e.object_mesh("t/p").expect("the generated post has a mesh");
    let min_y = post.vertices.iter().map(|v| v.y).fold(f64::INFINITY, f64::min);
    let max_y = post.vertices.iter().map(|v| v.y).fold(f64::NEG_INFINITY, f64::max);
    assert!((min_y - 4.0).abs() < 1e-6 && (max_y - 9.0).abs() < 1e-6, "the post spans y=4..9 beneath the panel: {min_y}..{max_y}");
    let bounds = post.bounding_box();
    assert!((bounds.min.x - 3.0).abs() < 1e-6 && (bounds.max.x - 7.0).abs() < 1e-6 && (bounds.min.z - 3.0).abs() < 1e-6 && (bounds.max.z - 7.0).abs() < 1e-6, "the post is radius two at x=5, z=5");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the first evaluation runs the generator once");
    program(&mut m, &mut e, "comment", &format!("// unrelated edit\n{body}"));
    assert_eq!(runs.load(Ordering::SeqCst), 1, "an unrelated source comment reuses the generated result");
    program(&mut m, &mut e, "moved", &body.replace("[5mm, 5mm]", "[6mm, 5mm]"));
    assert_eq!(runs.load(Ordering::SeqCst), 2, "changing the request reruns the generator exactly once");
    let mut unknown = FabModel::with_script("test", F, &src("let p = m.generated(\"nope\", {})\nobject(p)"));
    unknown.set_env(env.clone());
    let failure = unknown.evaluate(&mut e, None).expect_err("an unknown generator is an error");
    assert!(failure.message.contains("no generator for 'nope'"), "unknown kinds name the missing generator: {failure}");
    assert_eq!(failure.line, 5, "the missing generator error locates its call");
    assert_eq!(e.object_mesh("t/p").unwrap().triangles.len(), post.triangles.len(), "a failed request preserves the last complete inspection mesh");

    for (bad, message) in [
        ("let p = m.generated()", "'kind' is missing"),
        ("let p = m.generated(\"post\", {panel: m.box({size: [1mm 1mm 1mm]})})", "must be a panel part"),
        ("let lid = m.panel(m.box({size: [40mm 2mm 40mm]}), {})\nlet p = m.generated(\"post\", {panel: lid})\nlet cycle = m.panel(p, {name: \"lid\"})", "a generated part cannot be the base or a cut of the panel it cuts"),
        ("let shell = m.box({size: [40mm 2mm 40mm]})\nlet lid = m.panel(shell, {})\nlet p = m.generated(\"post\", {panel: lid})\nlet cycle = m.panel(shell, {name: \"lid\" cuts: [p]})", "a generated part cannot be the base or a cut of the panel it cuts"),
    ] {
        let failure = program::compile(F, &src(bad), &m.assets, &m.facets, &m.pictures, &m.context, &|_| None).expect_err("invalid generated requests fail during pass one");
        assert!(failure.iter().any(|d| d.line >= 5 && d.message.contains(message)), "invalid request has its located diagnostic '{message}': {failure:?}");
    }

    let complete = e.object_mesh("t/p").unwrap();
    let broken = ScriptEnv::default().with_generator("post", Arc::new(|_| Ok(GenPart {
        solid: Arc::new(|_| Err("generated builder failed".into())), adds: Vec::new(), cuts: Vec::new(), claims: Vec::new(),
    })));
    m.set_env(broken.clone());
    let failure = m.evaluate(&mut e, None).expect_err("pass two reports a failed builder");
    assert!(failure.message.contains("generated builder failed"), "pass two preserves the builder diagnostic: {failure}");
    assert_eq!(e.object_mesh("t/p").unwrap().vertices, complete.vertices, "a failed second pass keeps the last complete mesh");

    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = cancelled.clone();
    let good = env.generators["post"].clone();
    m.set_env(env.with_generator("post", Arc::new(move |input| {
        let part = good(input)?;
        stop.store(true, Ordering::SeqCst);
        Ok(part)
    })));
    let failure = m.evaluate(&mut e, Some(&|| cancelled.load(Ordering::SeqCst))).expect_err("a cancelled generation does not publish");
    assert!(failure.message.contains("cancelled"), "generation honours cancellation before pass two");
    assert_eq!(e.object_mesh("t/p").unwrap().vertices, complete.vertices, "cancellation keeps the last complete mesh");

    let mut first = FabModel::with_script("test", F, &src(body));
    first.set_env(broken);
    let mut initial = Evaluator::new(Machine::dual_nozzle_350());
    let (partial, shown) = first.evaluate(&mut initial, None).expect("without a complete result the first pass can show");
    assert!(partial.instances[0].script.generated.is_empty(), "the initial fallback carries only pass one's program");
    assert!(initial.object_mesh("t/p").unwrap().triangles.is_empty(), "the initial fallback's generated placeholder is empty");
    assert!(shown.warnings.iter().any(|w| w.contains("generated builder failed")), "the initial fallback explains why generated parts are pending");
}

#[test]
fn bracket_prints_one_closed_part_with_a_hole() {
    let (mut m, mut e) = setup();
    let (applied, _, ev) = program(&mut m, &mut e, "r1", BRACKET);
    assert_eq!(applied.added, vec!["t/plate", "t/hole", "t/bracket"]);
    assert_eq!(ev.plate.parts.len(), 1, "only placed solids print: {:?}", ev.objects);
    let part = &ev.plate.parts[0];
    assert_eq!(part.object, "t/bracket");
    assert_eq!(part.mesh.open_edges(), 0);
    let hole = std::f64::consts::PI * 16.0 * 6.0;
    let v = part.mesh.volume();
    assert!((v - (60.0 * 6.0 * 40.0 - hole)).abs() < 60.0 * 6.0 * 40.0 * 0.01, "volume {v}");
    // Model Y (6 mm thick) is the printer's Z; model Z (40) its Y.
    let size = part.mesh.bounds().size();
    assert!((size[0] - 60.0).abs() < 1e-6 && (size[1] - 40.0).abs() < 1e-6 && (size[2] - 6.0).abs() < 1e-6, "{size:?}");
    // Centred on the bed, on the plate.
    let b = part.mesh.bounds();
    assert!(b.min[2].abs() < 1e-9);
    assert!((b.center()[0] - 175.0).abs() < 1e-6 && (b.center()[1] - 160.0).abs() < 1e-6);
    assert!(ev.reports[0].issues.is_empty(), "{:?}", ev.reports);
    // One render mesh per part, with a normal per corner.
    assert_eq!(ev.render.len(), 1);
    assert_eq!(ev.render[0].normals.len(), ev.render[0].positions.len());
}

#[test]
fn rounded_bottom_box_has_an_overhang_note_without_a_warning() {
    let (mut m, mut e) = setup();
    // Model Y is printer Z: a 24 x 24 footprint, 30 mm high, R2 edges.
    let (_, _, ev) = program(&mut m, &mut e, "rounded-bottom", "let rounded = m.box({size: [24mm 30mm 24mm] radius: 2mm})\nobject(rounded)\n");
    assert_eq!(ev.plate.parts.len(), 1);
    let patches: Vec<_> = ev.printability.issues.iter().filter(|i| i.kind == "overhang").collect();
    assert!(!patches.is_empty(), "the rounded bottom exercises the bed-edge classification");
    assert!(patches.iter().all(|i| i.severity == fab_printability::Severity::Info), "{patches:?}");
    assert!(patches.iter().all(|i| i.message.contains("bottom edge rounding/chamfer")), "{patches:?}");
    // Moving the same downward surface off the bed must remove the exemption.
    let mut floating = ev.plate.clone();
    floating.parts[0].mesh.translate([0.0, 0.0, 5.0]);
    let report = fab_printability::analyse_plate(&ev.machine, &floating, &e.materials);
    assert!(report.issues.iter().any(|i| i.kind == "overhang" && i.severity == fab_printability::Severity::Warning));
}

#[test]
fn boolean_union_mushroom_keeps_its_elevated_overhang_warning() {
    let (mut m, mut e) = setup();
    // Primitives are centred. The exposed stem is 20 mm high; its last 2 mm overlap the
    // cap so this is a single connected boolean solid.
    let (_, _, ev) = program(
        &mut m,
        &mut e,
        "mushroom",
        "let stem = m.cylinder({radius: 4mm height: 22mm segments: 48 at: [0 11mm 0]})\nlet cap = m.cylinder({radius: 18mm height: 4mm segments: 64 at: [0 22mm 0]})\nlet mushroom = m.union(stem, cap)\nobject(mushroom)\n",
    );
    assert_eq!(ev.plate.parts.len(), 1, "the boolean's operands do not print");
    let part = &ev.plate.parts[0];
    assert_eq!(part.object, "t/mushroom");
    assert_eq!(part.mesh.open_edges(), 0);
    assert!(part.mesh.bounds().min[2].abs() < 1e-6);
    assert!((part.mesh.bounds().max[2] - 24.0).abs() < 1e-6);
    let patches: Vec<_> = ev.printability.issues.iter().filter(|i| i.kind == "overhang").collect();
    assert!(patches.iter().any(|i| i.severity == fab_printability::Severity::Warning && (i.lowest_z - 20.0).abs() < 1e-6), "{patches:?}");
    assert!(!patches.iter().any(|i| i.message.contains("bottom edge rounding/chamfer")));
}

#[test]
fn a_value_edit_changes_the_hole_and_rebuilds_only_what_it_touches() {
    let (mut m, mut e) = setup();
    let (_, _, before) = program(&mut m, &mut e, "r1", BRACKET);
    let (applied, _, after) = apply(&mut m, &mut e, "r2", vec![sc(Edit::Set { site: "hole.radius".into(), value: "8mm".into() })]);
    assert_eq!(applied.changed, vec!["t/hole"]);
    assert_eq!(m.source(F).unwrap(), src(BRACKET).replace("radius: 4mm", "radius: 8mm"));
    let removed = before.plate.parts[0].mesh.volume() - after.plate.parts[0].mesh.volume();
    let expected = std::f64::consts::PI * (64.0 - 16.0) * 6.0;
    assert!((removed - expected).abs() < expected * 0.03, "removed {removed}, expected {expected}");
    // The plate was built before: the hole and the cut are new.
    assert_eq!(after.built, 2);
}

#[test]
fn undo_redo_and_heads() {
    let (mut m, mut e) = setup();
    let h0 = m.head();
    program(&mut m, &mut e, "r1", BRACKET);
    let h1 = m.head();
    assert_eq!(h1.generation, 1);
    let (h2, _, ev) = m.history(&mut e, h1, HistoryAction::Undo, None).unwrap();
    assert_eq!(h2.generation, 2);
    assert_eq!(h2.content, h0.content);
    assert!(ev.plate.parts.is_empty());
    let (h3, _, ev) = m.history(&mut e, h2, HistoryAction::Redo, None).unwrap();
    assert_eq!(h3.content, h1.content);
    assert_eq!(ev.plate.parts.len(), 1);
    assert_eq!(m.history(&mut e, h3, HistoryAction::Redo, None).unwrap_err(), ApplyError::NoHistory);
    assert!(h3.matches(&h3.text()) && !h2.matches(&h3.text()));
}

/// Why: one Ctrl+Z order across the plate and the electronics, 11-15.
#[test]
fn steps_share_one_counter_and_a_commit_anywhere_kills_redo() {
    let (mut m, mut e) = setup();
    let steps = Arc::new(AtomicU64::new(40));
    let logged = Arc::new(AtomicU64::new(40));
    m.set_steps(steps.clone(), logged.clone());
    assert!(Arc::ptr_eq(m.steps(), &steps), "the model retains the window's shared counter");
    assert!(Arc::ptr_eq(m.logged(), &logged), "the model retains the shared logged-entry counter");
    let initial = m.state().clone();
    program(&mut m, &mut e, "r1", "let n = 1");
    let first = m.state().clone();
    assert_eq!(m.undo_top(), Some(41), "the first commit takes the shared counter's next number");
    program(&mut m, &mut e, "r2", "let n = 2");
    assert_eq!(m.undo_top(), Some(42), "the second commit takes the next shared number");
    assert_eq!(steps.load(SeqCst), 42, "both commits advance the supplied counter");

    m.history(&mut e, m.head(), HistoryAction::Undo, None).expect("the second commit undoes");
    assert_eq!(m.state(), &first, "undo restores the first committed state");
    assert_eq!((m.undo_top(), m.redo_top()), (Some(41), Some(42)), "undo preserves both steps' numbers");
    assert!(m.can_redo(), "redo is available before another history commits");
    let seq = steps.fetch_add(1, SeqCst) + 1;
    logged.fetch_max(seq, SeqCst);
    assert_eq!(m.redo_top(), None, "a commit in the other history immediately hides redo");
    assert!(!m.can_redo(), "can_redo agrees with the invalidated redo top");
    assert_eq!(m.history(&mut e, m.head(), HistoryAction::Redo, None).unwrap_err(), ApplyError::NoHistory, "a commit in the other history refuses redo");
    assert!(!m.can_redo(), "refused redo leaves no redo available");
    assert_eq!(m.state(), &first, "refused redo changes no model state");

    m.history(&mut e, m.head(), HistoryAction::Undo, None).expect("the first commit still undoes");
    assert_eq!(m.state(), &initial, "the remaining undo restores the initial state");
    assert_eq!((m.undo_top(), m.redo_top()), (None, Some(41)), "a fresh undo keeps the first step's number");
    m.history(&mut e, m.head(), HistoryAction::Redo, None).expect("redo succeeds without a counter move");
    assert_eq!(m.state(), &first, "redo restores the first committed state");
    assert_eq!((m.undo_top(), m.redo_top()), (Some(41), None), "redo restores its original number without reviving discarded steps");
    assert_eq!(steps.load(SeqCst), 43, "undo, redo and refusal allocate no step numbers");

    program(&mut m, &mut e, "r3", "let n = 3");
    m.history(&mut e, m.head(), HistoryAction::Undo, None).expect("the new commit undoes");
    assert_eq!(m.redo_top(), Some(44), "the new commit has its own shared step number");
    let seq = steps.fetch_add(1, SeqCst) + 1;
    logged.fetch_max(seq, SeqCst);
    m.history(&mut e, m.head(), HistoryAction::Undo, None).expect("undo works after an external commit");
    m.history(&mut e, m.head(), HistoryAction::Redo, None).expect("the fresh undo can redo");
    assert_eq!(m.state(), &first, "the fresh redo restores only the remaining first commit");
    assert_eq!((m.undo_top(), m.redo_top()), (Some(41), None), "another undo cannot revive redo killed by an external commit");
    assert_eq!(steps.load(SeqCst), 45, "the fresh undo and redo also retain the shared counter");
}

/// A reservation can leave a gap without invalidating redo in the other history.
#[test]
fn a_reservation_elsewhere_preserves_model_redo_until_an_entry_is_logged() {
    let (mut m, mut e) = setup();
    let steps = m.steps().clone();
    let logged = m.logged().clone();
    program(&mut m, &mut e, "reserved", "let n = 1");
    let committed = m.state().clone();
    let seq = m.undo_top().expect("the model commit has an undo number");
    m.history(&mut e, m.head(), HistoryAction::Undo, None).expect("the model commit undoes");
    let mark = logged.load(SeqCst);
    let reserved = steps.fetch_add(1, SeqCst) + 1;
    assert!(reserved > seq, "the other history reserves a newer number");
    assert_eq!(logged.load(SeqCst), mark, "a reservation logs no entry");
    assert_eq!(m.redo_top(), Some(seq), "a reservation elsewhere leaves model redo available");
    m.history(&mut e, m.head(), HistoryAction::Redo, None).expect("redo applies after a reservation elsewhere");
    assert_eq!(m.state(), &committed, "redo after a reservation restores the committed model state");
    assert_eq!(steps.load(SeqCst), reserved, "redo allocates no number after the reservation");

    m.history(&mut e, m.head(), HistoryAction::Undo, None).expect("the restored model commit undoes again");
    let seq = steps.fetch_add(1, SeqCst) + 1;
    logged.fetch_max(seq, SeqCst);
    assert_eq!(m.redo_top(), None, "a logged entry elsewhere invalidates model redo");
    assert_eq!(m.history(&mut e, m.head(), HistoryAction::Redo, None).unwrap_err(), ApplyError::NoHistory,
        "redo refuses after the other history logs an entry");
}

/// A compiled script goes once neither the state nor the one before asks it: an edit, its undo
/// and its redo take the scripts they left as they were, however often; edits in a row let the
/// old ones go; an undo further back compiles its script again from the text; and a script no
/// edit touches is never compiled again.
#[test]
fn a_script_goes_once_neither_the_state_nor_the_one_before_asks_it() {
    let (mut m, mut e) = setup();
    let body = |k: usize| format!("let mug = m.box({{size: [{}mm 20mm 20mm]}})\nobject(mug)\n", 20 + k);
    let _ = program(&mut m, &mut e, "r0", &body(0));
    let first = m.compile_script(F).unwrap();
    let _ = program(&mut m, &mut e, "r1", &body(1));
    let second = m.compile_script(F).unwrap();
    assert!(!Arc::ptr_eq(&first, &second), "an edit is a script of its own");
    for round in 0..3 {
        m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
        assert!(Arc::ptr_eq(&m.compile_script(F).unwrap(), &first), "undo {round} takes the script it left");
        m.history(&mut e, m.head(), HistoryAction::Redo, None).unwrap();
        assert!(Arc::ptr_eq(&m.compile_script(F).unwrap(), &second), "redo {round} takes the script it left");
    }
    let gone = Arc::downgrade(&first);
    drop((first, second));
    for k in 2..5 {
        let _ = program(&mut m, &mut e, &format!("r{k}"), &body(k));
    }
    assert!(gone.upgrade().is_none(), "edits in a row let the old scripts go");
    for _ in 0..4 {
        m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    }
    assert!(m.compile_script(F).unwrap().file == F && m.source(F) == Some(src(&body(0)).as_str()), "an undo further back compiles its text again");

    let (a, b) = ("a.cad.splash", "b.cad.splash");
    let script = |k: usize| src(&format!("let mug = m.box({{size: [{}mm 20mm 20mm]}})\nobject(mug)\n", 20 + k));
    let mut m = FabModel::load("two", "use mod.fab.*\nscript(\"a.cad.splash\")\nscript(\"b.cad.splash\")\n", [(a.to_string(), script(0)), (b.to_string(), script(1))].into()).unwrap();
    let other = m.compile_script(b).unwrap();
    for k in 2..6 {
        let change = Change::Script { file: a.into(), edit: Edit::Program { source: script(k) } };
        let _ = apply(&mut m, &mut e, &format!("a{k}"), vec![change]);
        assert!(Arc::ptr_eq(&m.compile_script(b).unwrap(), &other), "edit {k} of the other script");
    }
}

#[test]
fn stale_heads_replays_and_reused_ids() {
    let (mut m, mut e) = setup();
    let h0 = m.head();
    let batch = vec![sc(Edit::Program { source: src(BRACKET) })];
    m.apply(&mut e, "r1", h0, &batch, None).unwrap();
    // Same request again (as a retry would send it): replayed, not applied twice.
    let (again, ev) = m.apply(&mut e, "r1", h0, &batch, None).unwrap();
    assert!(again.replayed && ev.is_none());
    assert_eq!(m.head().generation, 1);
    let unplace = vec![pl("bracket", PlaceArg::Show(false))];
    // Same id, different batch.
    assert_eq!(m.apply(&mut e, "r1", m.head(), &unplace, None).unwrap_err(), ApplyError::RequestIdReused);
    // Old head.
    let err = m.apply(&mut e, "r2", h0, &unplace, None).unwrap_err();
    assert_eq!(err, ApplyError::StaleHead { current: m.head() });
}

#[test]
fn a_batch_that_does_not_build_commits_nothing() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "r1", BRACKET);
    let head = m.head();
    let before = m.source(F).unwrap().to_string();
    // A program error names its line and column.
    let err = m.apply(&mut e, "r2", head, &[sc(Edit::Add { code: "let ball = m.union(m.sphere({radius: 5mm}), nothing)".into(), at: Anchor::Auto })], None).unwrap_err();
    match err {
        ApplyError::Build(f) => {
            assert_eq!((f.file.as_str(), f.line), (F, 8), "{f}");
            assert!(f.message.contains("nothing"), "{}", f.message);
        }
        other => panic!("{other:?}"),
    }
    // A solid that cannot be built names the solid.
    let err = m.apply(&mut e, "r3", head, &[sc(Edit::Set { site: "hole.radius".into(), value: "-4mm".into() })], None).unwrap_err();
    match err {
        ApplyError::Build(f) => assert_eq!((f.id.as_str(), f.op.as_str()), ("hole", "Cylinder"), "{f}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(m.head(), head);
    assert_eq!(m.source(F).unwrap(), before);
    // An edit the program refuses names its index.
    assert!(matches!(m.apply(&mut e, "r4", head, &[sc(Edit::Remove { solid: "nothing".into() })], None), Err(ApplyError::Invalid { index: 0, .. })));
}

/// Two horns a fn builds as cages in loops: replacing one edits its call (the fn's code is both
/// calls'), and a batch that leaves the script calling itself without end runs its budget out
/// and commits nothing, so no such script can be saved.
#[test]
fn a_fn_part_is_replaced_at_its_call_and_a_runaway_script_commits_nothing() {
    let horns = "fn horn(x, n) {\n    let points = []\n    let faces = []\n    for i in 0..n {\n        let s = 9mm - i * 1.2mm\n        for c in [[s, 0], [0, s], [-s, 0], [0, -s]] { points.push([x + i * i * 0.5mm + c[0], i * 8mm, c[1]]) }\n    }\n    for i in 0..n - 1 {\n        for k in 0..4 {\n            let j = (k + 1) % 4\n            faces.push([4 * i + k, 4 * i + 4 + k, 4 * i + 4 + j, 4 * i + j])\n        }\n    }\n    faces.push([0, 1, 2, 3])\n    faces.push([4 * n - 1, 4 * n - 2, 4 * n - 3, 4 * n - 4])\n    return m.cage({points: points faces: faces})\n}\nlet hornA = horn(-30mm, 7)\nlet hornB = horn(30mm, 7)\nobject(hornA)\nobject(hornB)\n";
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, "r1", horns);
    assert_eq!(ev.plate.parts.len(), 2);
    let (_, c, ev) = apply(&mut m, &mut e, "r2", vec![sc(Edit::Replace { solid: "hornA".into(), code: "horn(-25mm, 6)".into() })]);
    assert_eq!(m.source(F).unwrap(), src(&horns.replace("horn(-30mm, 7)", "horn(-25mm, 6)")));
    assert_eq!(ev.plate.parts.len(), 2);
    assert!(prog(&c).instructions * 100 < makepad_model_script::lang::EVAL_LIMIT as u64, "{} instructions", prog(&c).instructions);

    // What the replace used to write: the fn's own cage call made a call of the fn.
    let head = m.head();
    let before = m.source(F).unwrap().to_string();
    let runaway = before.replacen("return m.cage({points: points faces: faces})", "return horn(x, n)", 1);
    match m.apply(&mut e, "r3", head, &[sc(Edit::Program { source: runaway })], None).unwrap_err() {
        ApplyError::Build(f) => assert!(f.message.contains("calls itself"), "{f}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(m.head(), head);
    assert_eq!(m.source(F).unwrap(), before);
}

/// `fab.params` lists a fn's parts' values under the call that made each, and a set or a move of
/// one call's part rewrites that call alone: the other call stays byte for byte as it was.
#[test]
fn a_fn_parts_params_are_its_calls_and_the_other_call_stays() {
    use params::PValue;
    let posts = "fn post(r, at) {\n    return m.sphere({radius: r * 2 at: at segments: 24})\n}\nlet a = post(1.5mm, [0mm, 0mm, 0mm])\nlet b = post(1.5mm, [20mm, 0mm, 0mm])\nobject(a)\nobject(b)\n";
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "r1", posts);
    // (the plate lays the parts out again: b's shape, not where it lies)
    let b_mesh = |ev: &Evaluated| ev.plate.parts.iter().find(|p| p.object == "t/b").unwrap().mesh.bounds().size();
    let b_before = b_mesh(&ev);
    let source = m.source(F).unwrap().to_string();
    let a_line = source.find("let a = ").unwrap()..source.find("let b = ").unwrap();
    let parts = params::model(&c, &ev);
    let of_a: Vec<&params::Param> = parts.iter().flat_map(|pp| pp.params.iter()).filter(|q| q.key.starts_with("t/a.")).collect();
    let mut keys: Vec<&str> = of_a.iter().map(|q| q.key.as_str()).collect();
    keys.sort();
    // The call's argument by the fn's parameter, and the placement it passes; never the fn's own
    // `segments` or its computed radius.
    assert_eq!(keys, ["t/a.at.0", "t/a.at.1", "t/a.at.2", "t/a.r"]);
    for q in &of_a {
        assert_eq!(q.group, "a = post(..)", "{}", q.key);
        assert!(a_line.start <= q.site.start && q.site.end <= a_line.end, "{} is written at {:?}", q.key, q.site);
    }
    let r = of_a.iter().find(|q| q.key == "t/a.r").unwrap();
    assert!(matches!(r.value, PValue::Num(v) if (v - 1.5).abs() < 1e-9), "{:?}", r.value);

    let change = params::edit(&c, &parts, "t/a.r", &PValue::Num(2.0)).unwrap();
    let (_, c, ev) = apply(&mut m, &mut e, "r2", vec![change]);
    let set = source.replacen("let a = post(1.5mm,", "let a = post(2mm,", 1);
    assert_eq!(m.source(F).unwrap(), set);
    assert_eq!(b_mesh(&ev), b_before);

    // A move writes the placement the call passes.
    let parts = params::model(&c, &ev);
    let change = params::edit(&c, &parts, "t/a.at", &PValue::Point(vec![5.0, 0.0, 0.0])).unwrap();
    apply(&mut m, &mut e, "r3", vec![change]);
    let moved = m.source(F).unwrap().to_string();
    let changed: Vec<(&str, &str)> = set.lines().zip(moved.lines()).filter(|(x, y)| x != y).collect();
    assert_eq!(changed.len(), 1, "{moved}");
    assert!(changed[0].0.starts_with("let a = post(2mm,") && changed[0].1.starts_with("let a = post(2mm, [5mm"), "{changed:?}");
}

/// A drag of a value the script writes compiles it once with the instance's own param values,
/// wherever the edit moves the param calls; a moved param takes its new value.
#[test]
fn a_dragged_value_keeps_the_instances_params_wherever_their_calls_move() {
    use params::PValue;
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "r1", "let w = param(10mm, min: 2mm, max: 40mm)\nlet b = m.box({size: [w 6mm 8mm]})\nobject(b)\n");
    let (_, c, ev) = apply(&mut m, &mut e, "w", vec![Change::Param { instance: "t".into(), name: "w".into(), value: Some(0.02) }]);
    let s = &c.instances[0].script;
    let (w, at) = s.param("w").unwrap();
    assert!((w - 0.02).abs() < 1e-12, "{w}");
    let parts = params::model(&c, &ev);
    let live = params::with_value(&c, &parts, "t/b.size.1", &PValue::Num(9.0)).unwrap();
    assert!(live.source().contains("[w 9mm 8mm]"), "{}", live.source());
    assert_eq!(live.param("w").map(|p| p.0), Some(w));
    // A line above moves the call: the instance's value still goes to it, and a moved one wins.
    let shifted = s.source().replacen("let w", "let lift = 2mm\n  let w", 1);
    let again = params::compile_pinned(s, &shifted, None).unwrap();
    let (v, moved_at) = again.param("w").unwrap();
    assert_ne!(moved_at, at);
    assert_eq!(v, w);
    assert_eq!(params::compile_pinned(s, &shifted, Some(("w", 0.03))).unwrap().param("w").map(|p| p.0), Some(0.03));
}

/// A drag of a cage point in blends nested in blends (a figure's limbs into its body) compiles
/// the script once a frame: about one compile of it, not two.
#[test]
fn a_drag_in_nested_blends_compiles_once_a_frame() {
    use params::PValue;
    let six = "[0 1 2 3] [4 7 6 5] [0 4 5 1] [1 5 6 2] [2 6 7 3] [3 7 4 0]";
    let cube = "[[-4mm, 0, -4mm] [4mm, 0, -4mm] [4mm, 0, 4mm] [-4mm, 0, 4mm] [-4mm, 8mm, -4mm] [4mm, 8mm, -4mm] [4mm, 8mm, 4mm] [-4mm, 8mm, 4mm]]";
    let mut body = format!("let r = param(1mm, min: 0.5mm, max: 3mm)\nlet b0 = m.cage({{points: {cube} faces: [{six}]}})\n");
    for k in 1..=12 {
        body += &format!("let b{k} = m.blend([b{} m.cage({{points: {cube} faces: [{six}] at: [{}mm, 0, 0]}})], r)\n", k - 1, k * 6);
    }
    body += "object(b12)\n";
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "chain", &body);
    let parts = params::model(&c, &ev);
    let key = parts.iter().flat_map(|pp| pp.params.iter()).map(|q| q.key.as_str()).find(|k| k.ends_with("b0.points.6")).expect("the first cage's points are params").to_string();
    let s = &c.instances[0].script;
    // Work counted on this thread (evaluations, name parses), never timed.
    let count = |run: &dyn Fn()| {
        let before = makepad_model_script::lang::work_done();
        run();
        let after = makepad_model_script::lang::work_done();
        (after.0 - before.0, after.1 - before.1)
    };
    let drag = count(&|| {
        let live = params::with_value(&c, &parts, &key, &PValue::Point(vec![4.0, 9.0, 4.0])).unwrap();
        assert!(live.source().contains("[4mm 9mm 4mm]"));
    });
    let compile = count(&|| {
        program::compile(&s.file, s.source(), &s.assets, &s.facets, &s.pictures, &s.context, &|_| None).unwrap();
    });
    assert_eq!(compile.0, 1, "one compile evaluates the program once: {compile:?}");
    assert_eq!(drag.0, 1, "a drag frame in 12 nested blends evaluates the program once, not once per blend or twice: {drag:?}");
    assert!(drag.1 <= compile.1, "a drag frame parses the source no more often than a compile does: drag {drag:?}, compile {compile:?}");
}

#[test]
fn sdf_blob_joins_a_csg_box() {
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(
        &mut m,
        &mut e,
        "r1",
        "let knob = m.sdf({cell: 0.6mm bounds: [[-9mm, 0, -9mm], [9mm, 21mm, 9mm]] field: fn(p) {\n    return smooth_union(sdf_sphere(p - vec3(0, 12mm, 0), 8mm), sdf_cylinder(p - vec3(0, 6mm, 0), 4mm, 12mm), 3mm)\n}})\nlet base = m.box({size: [30mm 4mm 30mm]})\nlet handle = m.union(base, knob)\nobject(handle)\n",
    );
    assert_eq!(ev.plate.parts.len(), 1);
    let (_, _, ev) = apply(&mut m, &mut e, "r2", vec![
        Change::Color { name: "Orange".into(), set: Some(crate::project::ColorDef { name: "Orange".into(), rgb: Some(0xff8800ff), at: Vec::new(), material: None }) },
        pl("handle", PlaceArg::Color(Some("Orange".into()))),
    ]);
    let part = &ev.plate.parts[0];
    assert_eq!((part.filament, part.nozzle), (1, Some(0)), "its one colour prints with the first tool");
    assert!((part.color[0] - 1.0).abs() < 1e-6 && part.color[2] < 0.01);
    assert_eq!(part.mesh.open_edges(), 0, "the union of the meshed blob and the box is closed");
    assert!(part.mesh.bounds().size()[2] > 19.0, "the knob stands on the base");
}

#[test]
fn place_and_position() {
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(
        &mut m,
        &mut e,
        "r1",
        "let a = m.box({size: [10mm 10mm 10mm]})\nlet b = m.box({size: [10mm 30mm 10mm] at: [0 40mm 0] rotate: [90°, 0, 0]})\nobject(a)\nobject(b)\n",
    );
    let free_b = ev.plate.parts.iter().find(|p| p.object == "t/b").unwrap().mesh.bounds();
    let (_, _, ev) = apply(&mut m, &mut e, "r2", vec![pl("a", PlaceArg::At(Some([12.0, 100.0])))]);
    let a = ev.plate.parts.iter().find(|p| p.object == "t/a").unwrap();
    let b = ev.plate.parts.iter().find(|p| p.object == "t/b").unwrap();
    assert!((a.mesh.bounds().center()[0] - 12.0).abs() < 1e-6);
    // Moving one object never moves another.
    assert_eq!(b.mesh.bounds(), free_b);
    // In the left nozzle's strip, and its colour prints on the left nozzle: no issue.
    let ra = &ev.reports[ev.plate.parts.iter().position(|p| p.object == "t/a").unwrap()];
    assert_eq!(ev.plate.parts.iter().find(|p| p.object == "t/a").unwrap().nozzle, Some(0));
    assert!(ra.issues.is_empty(), "{:?}", ra.issues);
    // b was turned 90 degrees about X: its 30 mm side now lies along the bed, and it dropped to the plate.
    let bb = b.mesh.bounds();
    assert!((bb.size()[2] - 10.0).abs() < 1e-6 && (bb.size()[1] - 30.0).abs() < 1e-6, "{:?}", bb.size());
    assert!(bb.min[2].abs() < 1e-9);
}

#[test]
fn place_print_settings_are_local_to_each_part() {
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(
        &mut m,
        &mut e,
        "settings",
        r#"let mug = m.box({size: [20mm 20mm 20mm]})
object(mug)
let plain = m.box({size: [10mm 10mm 10mm]})
object(plain)
"#,
    );
    assert_eq!(ev.plate.parts.len(), 2);
    let settings = vec![("fuzzy_skin_thickness".to_string(), "0.35".to_string()), ("fab_wall_texture".to_string(), "knurl".to_string()), ("fab_adaptive_layers".to_string(), "1".to_string())];
    let (_, _, ev) = apply(&mut m, &mut e, "settings2", vec![pl("mug", PlaceArg::Settings(settings))]);
    let mug = ev.plate.parts.iter().find(|p| p.object == "t/mug").unwrap();
    assert_eq!(mug.print, vec![
        ("fab_adaptive_layers".to_string(), "1".to_string()),
        ("fab_wall_texture".to_string(), "knurl".to_string()),
        ("fuzzy_skin_thickness".to_string(), "0.35".to_string()),
    ]);
    let plain = ev.plate.parts.iter().find(|p| p.object == "t/plain").unwrap();
    assert!(plain.print.is_empty());
}

/// A deleted tree is a mark on its part's placement: the part plans with its tips (the
/// planner's setting, to 0.1 um, its user settings as they were), no other part does, the project
/// writes and reads it back, and an undo takes it out.
#[test]
fn a_deleted_tree_is_a_mark_its_part_plans_with() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "boxes", "let mug = m.box({size: [20mm 20mm 20mm]})\nobject(mug)\nlet plain = m.box({size: [10mm 10mm 10mm]})\nobject(plain)\n");
    let marks = vec![project::SupportMark::NoTree { tips: vec![[1.234_567, -2.0, 3.2], [4.0, 5.0, 3.4]] }];
    apply(&mut m, &mut e, "walls", vec![pl("mug", PlaceArg::Settings(vec![("wall_loops".into(), "4".into())]))]);
    let (_, _, ev) = apply(&mut m, &mut e, "drop", vec![pl("mug", PlaceArg::Supports(marks))]);
    assert_eq!(fab_plate::TREE_DROPS, makepad_fab_toolpath::DROPPED_TIPS, "the planner reads the key the placement writes");
    let mug = ev.plate.parts.iter().find(|p| p.object == "t/mug").unwrap();
    assert_eq!(mug.print, vec![("wall_loops".to_string(), "4".to_string()), (fab_plate::TREE_DROPS.to_string(), "1.2346 -2 3.2;4 5 3.4".to_string())]);
    assert_eq!(fab_plate::own_settings(&mug.print).collect::<Vec<_>>(), [&("wall_loops".to_string(), "4".to_string())], "the user's settings alone");
    assert!(ev.plate.parts.iter().find(|p| p.object == "t/plain").unwrap().print.is_empty());
    let text = m.project_text();
    assert!(text.contains("supports: [{on: \"tree\" tips: [[1.2346mm, -2mm, 3.2mm], [4mm, 5mm, 3.4mm]]}]"), "{text}");
    let loaded = FabModel::load("marks", &text, m.state().scripts.clone()).unwrap();
    assert_eq!(loaded.project().place("t", "mug").unwrap().supports, vec![project::SupportMark::NoTree { tips: vec![[1.2346, -2.0, 3.2], [4.0, 5.0, 3.4]] }]);
    assert!(m.undo_labels().any(|l| l.contains("supports mug")), "{:?}", m.undo_labels().collect::<Vec<_>>());
    // One more tree deleted: the marks made before stay, and the same tree again changes nothing.
    let (_, _, ev) = apply(&mut m, &mut e, "drop again", vec![pl("mug", PlaceArg::NoTree(vec![[7.0, -8.0, 3.6]]))]);
    let drops = |ev: &Evaluated| ev.plate.parts.iter().find(|p| p.object == "t/mug").unwrap().print.iter().find(|(k, _)| k == fab_plate::TREE_DROPS).map(|(_, v)| v.clone());
    assert_eq!(drops(&ev).as_deref(), Some("1.2346 -2 3.2;4 5 3.4;7 -8 3.6"));
    let (_, _, ev) = apply(&mut m, &mut e, "drop the same", vec![pl("mug", PlaceArg::NoTree(vec![[7.0, -8.0, 3.6]]))]);
    assert_eq!(drops(&ev).as_deref(), Some("1.2346 -2 3.2;4 5 3.4;7 -8 3.6"));
    assert_eq!(m.project().place("t", "mug").unwrap().supports.len(), 2);
    let mut undone = ev;
    while drops(&undone).is_some() {
        undone = m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap().2;
    }
    let mug = undone.plate.parts.iter().find(|p| p.object == "t/mug").unwrap();
    assert_eq!(fab_plate::own_settings(&mug.print).collect::<Vec<_>>(), [&("wall_loops".to_string(), "4".to_string())], "undo takes the marks out, and nothing else");
    assert!(project::parse("use mod.fab.*\nscript(\"t.cad.splash\")\nplace(\"t\", object: \"mug\", supports: [{on: \"tree\" tips: [[1mm, 2mm]]}])\n").is_err(), "a tip is three lengths");
}

#[test]
fn place_rejects_settings_an_object_cannot_have() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "box", "let mug = m.box({size: [20mm 20mm 20mm]})\nobject(mug)\n");
    let head = m.head();
    // The plate's alone, and it says why.
    let err = m.apply(&mut e, "settings", head, &[pl("mug", PlaceArg::Settings(vec![("initial_layer_print_height".into(), "0.3".into())]))], None).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("initial_layer_print_height") && message.contains("first layer"), "{message}");
    assert_eq!(m.head(), head);
    // A value out of range, and a key that is no setting.
    assert!(m.apply(&mut e, "settings-a", head, &[pl("mug", PlaceArg::Settings(vec![("sparse_infill_density".into(), "140%".into())]))], None).unwrap_err().to_string().contains("0 to 100"));
    assert!(m.apply(&mut e, "settings-b", head, &[pl("mug", PlaceArg::Settings(vec![("no_such".into(), "1".into())]))], None).is_err());
    // The project's text is checked the same way.
    let err = project::parse("use mod.fab.*\nscript(\"t.cad.splash\")\nplace(\"t\", object: \"mug\", settings: {no_such: 0.3})\n").unwrap_err();
    assert!(err[0].message.contains("no_such") && err[0].message.contains("fab_wall_texture"), "{err:?}");
    let err = project::parse("use mod.fab.*\nscript(\"t.cad.splash\")\nplace(\"t\", object: \"mug\", settings: {initial_layer_print_height: 0.3})\n").unwrap_err();
    assert!(err[0].message.contains("settings({initial_layer_print_height"), "{err:?}");
}

/// A colour's printer positions name a side or a numbered head, and are written back as read.
#[test]
fn colour_positions_name_sides_and_numbered_heads() {
    let text = "use mod.fab.*\ncolor(\"Body\", at: [[\"Four heads\" @tool_1 \"A1\"] [\"Two heads\" @right \"A2\"]])\n";
    let p = project::parse(text).unwrap();
    assert_eq!(p.colors[0].at.iter().map(|a| a.side.as_str()).collect::<Vec<_>>(), ["tool_1", "right"]);
    let written = project::write(&p, &|_, _| project::Unit::Plain);
    assert_eq!(project::parse(&written).unwrap().colors[0].at, p.colors[0].at);
    for bad in ["@tool_0", "@tool_", "@tool_x", "@middle"] {
        let err = project::parse(&text.replace("@tool_1", bad)).unwrap_err();
        assert!(err[0].message.contains("the side is"), "{bad}: {err:?}");
    }
}

/// What the project writes reads back as it was, every list in it: a placement's at, rotate and
/// paint, a colour's printer positions and a ruler's points, with negatives, zeros and large
/// values. With only a space between a list's values, `[90° -45° 0°]` would read back as
/// two values and `[10mm -20mm]` as one.
#[test]
fn a_written_project_reads_back_its_lists_negatives_and_all() {
    let text = "use mod.fab.*\nscript(\"t.cad.splash\")\ncolor(\"Body\", at: [[\"Four heads\", @tool_1, \"A1\"], [\"Two heads\", @right, \"A2\"]])\ncolor(\"Red\", rgb: #xff0000)\ncolor(\"Green\", rgb: #x00ff00)\nplace(\"t\", object: \"a\", at: [10mm, -20mm], rotate: [90°, -45°, 0°])\nplace(\"t\", object: \"b\", link: 1, at: [-12345.5mm, 0mm], rotate: [-180°, 0°, -0.5°])\nplace(\"t\", object: \"c\", link: 2, at: [0mm, 999999mm], rotate: -30°, paint: [[#xff0000, \"Red\"], [#x00ff00, \"Green\"]])\nmeasure(points: [[-1mm, -2mm, -3mm], [4mm, -5mm, 0mm]], kind: @distance)\n";
    let p = project::parse(text).unwrap();
    let close = |got: &[f64], want: &[f64]| got.len() == want.len() && got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-6);
    let place = |k: usize| [p.places[k].at.unwrap().as_slice(), p.places[k].rotate.as_slice()].concat();
    assert!(close(&place(0), &[10.0, -20.0, 90.0, -45.0, 0.0]) && close(&place(1), &[-12345.5, 0.0, -180.0, 0.0, -0.5]) && close(&place(2), &[0.0, 999999.0, 0.0, 0.0, -30.0]), "the text reads as written: {:?}", p.places);
    assert!(close(&p.measures[0].points.concat(), &[-1.0, -2.0, -3.0, 4.0, -5.0, 0.0]), "{:?}", p.measures);
    let written = project::write(&p, &|_, _| project::Unit::Plain);
    assert_eq!(project::parse(&written).unwrap_or_else(|e| panic!("{e:?}\n{written}")), p, "{written}");
}

/// A placement's scale: one factor scales all three axes and writes back as one (none at 1);
/// three scale by axis (along X, Y and Z) and write back as three. Each is above zero.
#[test]
fn a_scale_is_one_factor_or_three_by_axis_and_writes_back_as_read() {
    let text = "use mod.fab.*\nscript(\"t.cad.splash\")\nplace(\"t\", object: \"a\", scale: 1.5)\nplace(\"t\", object: \"b\", link: 1, scale: [1, 0.5, 2])\nplace(\"t\", object: \"c\", link: 2)\n";
    let p = project::parse(text).unwrap();
    assert_eq!(p.places.iter().map(|p| p.scale).collect::<Vec<_>>(), [[1.5; 3], [1.0, 0.5, 2.0], [1.0; 3]]);
    let written = project::write(&p, &|_, _| project::Unit::Plain);
    assert!(written.contains("scale: 1.5") && written.contains("scale: [1, 0.5, 2]") && written.matches("scale").count() == 2, "{written}");
    assert_eq!(project::parse(&written).unwrap_or_else(|e| panic!("{e:?}\n{written}")), p, "{written}");
    for bad in ["scale: 0", "scale: [1, 0, 2]", "scale: [1, 2]", "scale: [1, -1, 1]"] {
        let err = project::parse(&text.replace("scale: 1.5", bad)).unwrap_err();
        assert!(err[0].message.contains("scale"), "{bad}: {err:?}");
    }
}

#[test]
fn one_override_keeps_the_others_and_a_no_op_is_no_undo_step() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "box", "let mug = m.box({size: [20mm 20mm 20mm]})\nobject(mug)\n");
    let one = |key: &str, value: Option<&str>| pl("mug", PlaceArg::Setting { key: key.into(), value: value.map(str::to_string) });
    apply(&mut m, &mut e, "a", vec![one("wall_loops", Some("4"))]);
    let (_, _, ev) = apply(&mut m, &mut e, "b", vec![one("sparse_infill_density", Some("30"))]);
    let mug = &ev.plate.parts[0];
    assert_eq!(mug.print, vec![("sparse_infill_density".to_string(), "30%".to_string()), ("wall_loops".to_string(), "4".to_string())]);
    let steps = m.undo_labels().count();
    apply(&mut m, &mut e, "c", vec![one("wall_loops", Some("4"))]);
    assert_eq!(m.undo_labels().count(), steps, "nothing changed, nothing to undo");
    let (_, _, ev) = apply(&mut m, &mut e, "d", vec![one("wall_loops", None)]);
    assert_eq!(ev.plate.parts[0].print.len(), 1);
}

/// An object's setting the same as the plate's is no override: set one way or another (one
/// key, a whole set, the plate's own coming to equal it) it is not kept, and one undo step
/// brings each back as it was.
#[test]
fn an_override_equal_to_the_plates_is_never_stored() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "box", "let mug = m.box({size: [20mm 20mm 20mm]})\nobject(mug)\n");
    let plate = |set: Vec<(&str, &str)>| Change::PlateSettings { set: set.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), reset: Vec::new() };
    let own = |ev: &Evaluated| ev.plate.parts[0].print.clone();
    let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
    apply(&mut m, &mut e, "global", vec![plate(vec![("enable_support", "1")])]);
    // one key, the plate's value: nothing stored
    let (_, _, ev) = apply(&mut m, &mut e, "one", vec![pl("mug", PlaceArg::Setting { key: "enable_support".into(), value: Some("1".into()) }), pl("mug", PlaceArg::Setting { key: "wall_loops".into(), value: Some("4".into()) })]);
    assert_eq!(own(&ev), vec![kv("wall_loops", "4")]);
    // a whole set (a copy, the AI's place): the plate's value goes, the rest stays
    let (_, _, ev) = apply(&mut m, &mut e, "set", vec![pl("mug", PlaceArg::Settings(vec![kv("enable_support", "1"), kv("sparse_infill_density", "100%")]))]);
    assert_eq!(own(&ev), vec![kv("sparse_infill_density", "100%")]);
    assert!(!m.project_text().contains("settings: {enable_support"), "{}", m.project_text());
    // the plate's own coming to equal it: the object's stays, never dropped unasked (the
    // panel lists it as no effect, with its delete)
    let (_, _, ev) = apply(&mut m, &mut e, "global2", vec![plate(vec![("sparse_infill_density", "100")])]);
    assert_eq!(own(&ev), vec![kv("sparse_infill_density", "100%")]);
    // nor does an edit of another of its settings drop it
    let (_, _, ev) = apply(&mut m, &mut e, "walls", vec![pl("mug", PlaceArg::Setting { key: "wall_loops".into(), value: Some("5".into()) })]);
    assert_eq!(own(&ev), vec![kv("sparse_infill_density", "100%"), kv("wall_loops", "5")]);
    // set to the plate's value itself: that one goes back to the plate's
    let (_, _, ev) = apply(&mut m, &mut e, "back", vec![pl("mug", PlaceArg::Setting { key: "sparse_infill_density".into(), value: Some("100%".into()) })]);
    assert_eq!(own(&ev), vec![kv("wall_loops", "5")]);
    let head = m.head();
    m.history(&mut e, head, HistoryAction::Undo, None).unwrap();
    assert!(m.project_text().contains("sparse_infill_density: \"100%\" wall_loops"), "{}", m.project_text());
}

#[test]
fn the_plates_global_settings_are_in_the_project() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "box", "let mug = m.box({size: [20mm 20mm 20mm]})\nobject(mug)\n");
    let before = m.project_text();
    let (_, _, ev) = apply(&mut m, &mut e, "global", vec![Change::PlateSettings { set: vec![("wall_loops".into(), "4".into()), ("sparse_infill_density".into(), "35".into())], reset: Vec::new() }]);
    assert_eq!(ev.plate.settings, vec![("sparse_infill_density".to_string(), "35%".to_string()), ("wall_loops".to_string(), "4".to_string())]);
    // The text holds it, reads back the same, and the edit undoes.
    let text = m.project_text();
    assert!(text.contains("settings({sparse_infill_density: \"35%\" wall_loops: \"4\"})"), "{text}");
    assert_eq!(project::parse(&text).unwrap().settings, ev.plate.settings);
    let edits = project::changes("settings({wall_loops: nil layer_height: 0.12})").unwrap();
    let (_, _, ev) = apply(&mut m, &mut e, "global2", edits);
    assert_eq!(ev.plate.settings, vec![("layer_height".to_string(), "0.12".to_string()), ("sparse_infill_density".to_string(), "35%".to_string())]);
    for _ in 0..2 {
        let head = m.head();
        m.history(&mut e, head, HistoryAction::Undo, None).unwrap();
    }
    assert_eq!(m.project_text(), before);
    // A value out of range is refused in the statement, and in a change made any other way.
    assert!(project::changes("settings({layer_height: 9})").unwrap_err()[0].message.contains("layer_height"));
    let head = m.head();
    assert!(m.apply(&mut e, "bad", head, &[Change::PlateSettings { set: vec![("layer_height".into(), "9".into())], reset: Vec::new() }], None).is_err());
}

#[test]
fn place_turns_in_3d_and_scales() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "r1", "let a = m.box({size: [10mm 30mm 10mm]})\nobject(a)\n");
    let (_, _, ev) = apply(&mut m, &mut e, "r2", vec![pl("a", PlaceArg::At(Some([100.0, 100.0]))), pl("a", PlaceArg::Rotate([90.0, 0.0, 0.0])), pl("a", PlaceArg::Scale([2.0; 3]))]);
    let b = ev.plate.parts[0].mesh.bounds();
    // the 30 mm side (model Y: the printer's Z as modelled) lies along the bed's Y after 90
    // degrees about X, everything twice the size, dropped onto the plate where asked
    let s = b.size();
    assert!((s[0] - 20.0).abs() < 1e-6 && (s[1] - 60.0).abs() < 1e-6 && (s[2] - 20.0).abs() < 1e-6, "{s:?}");
    assert!(b.min[2].abs() < 1e-9 && (b.center()[0] - 100.0).abs() < 1e-6 && (b.center()[1] - 100.0).abs() < 1e-6);
    let r = crate::eval::rotation_xyz([30.0, -20.0, 75.0]);
    let d = crate::eval::euler_xyz(&r);
    assert!((d[0] - 30.0).abs() < 1e-9 && (d[1] + 20.0).abs() < 1e-9 && (d[2] - 75.0).abs() < 1e-9, "{d:?}");
}

/// A part's placement is its own frame, whatever its shape: a cutter's eye turned off and on
/// and a param that shrinks its box (on one side) move no point the edit leaves alone, exactly,
/// turned on the plate or not; it still rests on the bed.
#[test]
fn shape_edits_never_move_the_part() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "r1", "let tab = param(30mm, min: 5mm, max: 60mm)\nlet block = m.box({size: [40mm 10mm 30mm]})\nlet arm = m.box({size: [tab 10mm 10mm] at: [20mm + tab / 2, 0, 0]})\nlet cut1 = m.box({size: [12mm 20mm 40mm] at: [-20mm 0 0]})\nlet part = m.subtract(m.union(block, [arm]), [cut1])\nobject(part)\n");
    for (turn, run) in [(0.0, "a"), (30.0, "b")] {
        let (_, _, ev) = apply(&mut m, &mut e, &format!("{run}-place"), vec![pl("part", PlaceArg::At(Some([150.0, 120.0]))), pl("part", PlaceArg::Rotate([0.0, 0.0, turn]))]);
        // the block's top middle, which no edit below touches, on the plate
        let at = |ev: &Evaluated| {
            let f = &ev.model_to_plate[0];
            std::array::from_fn::<f64, 3, _>(|i| f[i][1] * 5.0 + f[i][3])
        };
        let start = at(&ev);
        let (_, _, ev) = apply(&mut m, &mut e, &format!("{run}-off"), vec![sc(Edit::Field { solid: "cut1".into(), field: "off".into(), code: "true".into() })]);
        assert_eq!(at(&ev), start, "the cutter's eye off ({turn} deg)");
        let (_, _, ev) = apply(&mut m, &mut e, &format!("{run}-on"), vec![sc(Edit::Unset { solid: "cut1".into(), field: "off".into() })]);
        assert_eq!(at(&ev), start, "the cutter's eye on ({turn} deg)");
        let (_, _, ev) = apply(&mut m, &mut e, &format!("{run}-shrink"), vec![Change::Param { instance: "t".into(), name: "tab".into(), value: Some(0.01) }]);
        assert_eq!(at(&ev), start, "the arm shrunk ({turn} deg)");
        assert!(ev.plate.parts[0].mesh.bounds().min[2].abs() < 1e-9, "on the bed");
        apply(&mut m, &mut e, &format!("{run}-back"), vec![Change::Param { instance: "t".into(), name: "tab".into(), value: None }]);
    }
}

/// The view's gizmo on a feature, through the script and back. On the cutter (a primitive)
/// and on the union inside the subtract (a nested op), of a part turned on the plate: a move, a
/// turn and a scale each write that one placement field in builder form, one undo step, and the
/// rebuilt feature sits exactly where the gesture put it, the part where it was (only dropped
/// back onto the bed). A position written with a parameter keeps it, offset, through the
/// parameter's edits.
#[test]
fn feature_gestures_round_trip_through_the_script() {
    use params::Gesture;
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "r1", "let tab = param(30mm, min: 5mm, max: 60mm)\nlet block = m.box({size: [40mm 10mm 30mm]})\nlet arm = m.box({size: [tab 10mm 10mm] at: [20mm + tab / 2, 0, 0]})\nlet cut1 = m.box({size: [12mm 20mm 40mm] at: [-20mm 0 0]})\nlet part = m.subtract(m.union(block, [arm]), [cut1])\nobject(part)\n");
    let (_, mut c, mut ev) = apply(&mut m, &mut e, "place", vec![pl("part", PlaceArg::At(Some([150.0, 120.0])))]);
    let script = |m: &FabModel| m.state().scripts[F].clone();
    let gesture = |m: &mut FabModel, e: &mut Evaluator, c: &Compiled, ev: &Evaluated, id: &str, g: Gesture, step: &str| {
        let (key, value) = params::placed(c, ev, 0, &format!("t/{id}"), g).unwrap();
        let change = params::edit(c, &params::model(c, ev), &key, &value).unwrap();
        apply(m, e, step, vec![change])
    };
    // A position written with a parameter: offset, the trailing number taking a second move.
    let (_, c1, ev1) = gesture(&mut m, &mut e, &c, &ev, "arm", Gesture::Move([4.0, 0.0, 0.0]), "arm1");
    assert!(script(&m).contains("let arm = m.box({size: [tab 10mm 10mm] at: [20mm + tab / 2 + 4mm, 0, 0]})"), "{}", script(&m));
    gesture(&mut m, &mut e, &c1, &ev1, "arm", Gesture::Move([1.0, 0.0, 0.0]), "arm2");
    assert!(script(&m).contains("at: [20mm + tab / 2 + 5mm, 0, 0]"), "{}", script(&m));
    let (_, _, ev2) = apply(&mut m, &mut e, "tab", vec![Change::Param { instance: "t".into(), name: "tab".into(), value: Some(0.01) }]);
    let arm = ev2.objects.iter().find(|o| o.name == "t/arm").unwrap().world;
    assert!((arm[0][3] - 30.0).abs() < 1e-9, "the arm's offset survives the parameter: {arm:?}");
    apply(&mut m, &mut e, "tab-back", vec![Change::Param { instance: "t".into(), name: "tab".into(), value: None }]);
    (_, c, ev) = apply(&mut m, &mut e, "turn", vec![pl("part", PlaceArg::Rotate([0.0, 0.0, 30.0]))]);
    // A feature's frame on the plate.
    let frame = |ev: &Evaluated, id: &str| -> [[f64; 4]; 3] {
        let w = ev.objects.iter().find(|o| o.name == format!("t/{id}")).unwrap().world;
        eval::affine_mul(&ev.model_to_plate[0], &[w[0], w[1], w[2]])
    };
    for id in ["cut1", "part.union"] {
        // The union stays on its plane: a move up would only lift the part off the bed.
        let by = if id == "cut1" { [5.0, -3.0, 2.0] } else { [5.0, -3.0, 0.0] };
        for (n, g) in [Gesture::Move(by), Gesture::Turn(30.0), Gesture::Scale(1.5)].into_iter().enumerate() {
            let (before, part) = (frame(&ev, id), ev.model_to_plate[0]);
            let undo_to = script(&m);
            let (_, _, ev2) = gesture(&mut m, &mut e, &c, &ev, id, g, &format!("{id}-{n}"));
            // The part only drops back onto the bed (a scaled union reaches lower).
            let now = ev2.model_to_plate[0];
            let drop = now[2][3] - part[2][3];
            assert!((0..3).all(|r| (0..4).all(|k| (now[r][k] - part[r][k]).abs() < 1e-9 || (r, k) == (2, 3))), "{id} {g:?} moved the part: {part:?} -> {now:?}");
            let (s, co) = 30f64.to_radians().sin_cos();
            let want: [[f64; 4]; 3] = std::array::from_fn(|r| {
                std::array::from_fn(|k| match (g, k) {
                    (Gesture::Move(d), 3) => before[r][3] + d[r] + if r == 2 { drop } else { 0.0 },
                    (Gesture::Turn(_), k) if k < 3 => match r {
                        0 => co * before[0][k] - s * before[1][k],
                        1 => s * before[0][k] + co * before[1][k],
                        _ => before[2][k],
                    },
                    (Gesture::Scale(f), k) if k < 3 => f * before[r][k],
                    (_, 3) => before[r][3] + if r == 2 { drop } else { 0.0 },
                    _ => before[r][k],
                })
            });
            let got = frame(&ev2, id);
            assert!((0..3).all(|r| (0..4).all(|k| (got[r][k] - want[r][k]).abs() < 1e-4)), "{id} {g:?}: {got:?}, wanted {want:?}");
            // One undo step takes it back.
            let head = m.head();
            m.history(&mut e, head, HistoryAction::Undo, None).unwrap();
            assert_eq!(script(&m), undo_to, "{id} {g:?} undoes in one step");
            let head = m.head();
            (_, c, ev) = m.history(&mut e, head, HistoryAction::Redo, None).unwrap();
            assert_eq!(frame(&ev, id), got);
        }
    }
    let s = script(&m);
    assert!(s.contains("rotate: [0 30° 0] scale: 1.5})") && s.contains("let cut1 = m.box({size: [12mm 20mm 40mm] at: [") && s.contains("m.union(block, [arm], {at: ["), "builder form: {s}");
}

/// A gizmo that leaves a feature's placement negative writes lists that read back as written: a
/// move to negative coordinates, a turn the other way and a scale each put the cutter where the
/// gesture did. With only a space between the values, `at: [-5mm -8mm 10mm]` would read back as
/// two values and `rotate: [0 -30° 0]` as two.
#[test]
fn a_gesture_to_negative_values_reads_back_where_it_put_the_feature() {
    use params::Gesture;
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "neg", "let block = m.box({size: [40mm 10mm 30mm]})\nlet cut1 = m.box({size: [12mm 20mm 40mm] at: [5mm 2mm 0]})\nlet part = m.subtract(block, [cut1])\nobject(part)\n");
    let (_, mut c, mut ev) = apply(&mut m, &mut e, "place", vec![pl("part", PlaceArg::At(Some([150.0, 120.0])))]);
    let frame = |ev: &Evaluated| -> [[f64; 4]; 3] {
        let w = ev.objects.iter().find(|o| o.name == "t/cut1").unwrap().world;
        eval::affine_mul(&ev.model_to_plate[0], &[w[0], w[1], w[2]])
    };
    for (n, g) in [Gesture::Move([-10.0, -10.0, -10.0]), Gesture::Turn(-30.0), Gesture::Scale(1.5)].into_iter().enumerate() {
        let (before, part) = (frame(&ev), ev.model_to_plate[0]);
        let (key, value) = params::placed(&c, &ev, 0, "t/cut1", g).unwrap();
        let change = params::edit(&c, &params::model(&c, &ev), &key, &value).unwrap();
        (_, c, ev) = apply(&mut m, &mut e, &format!("neg-{n}"), vec![change]);
        // The part only drops back onto the bed.
        let drop = ev.model_to_plate[0][2][3] - part[2][3];
        let (s, co) = (-30f64).to_radians().sin_cos();
        let want: [[f64; 4]; 3] = std::array::from_fn(|r| {
            std::array::from_fn(|k| match (g, k) {
                (Gesture::Move(d), 3) => before[r][3] + d[r] + if r == 2 { drop } else { 0.0 },
                (Gesture::Turn(_), k) if k < 3 => match r {
                    0 => co * before[0][k] - s * before[1][k],
                    1 => s * before[0][k] + co * before[1][k],
                    _ => before[2][k],
                },
                (Gesture::Scale(f), k) if k < 3 => f * before[r][k],
                (_, 3) => before[r][3] + if r == 2 { drop } else { 0.0 },
                _ => before[r][k],
            })
        });
        let got = frame(&ev);
        assert!((0..3).all(|r| (0..4).all(|k| (got[r][k] - want[r][k]).abs() < 1e-4)), "{g:?}: {got:?}, wanted {want:?}\n{}", m.state().scripts[F]);
    }
    let s = m.state().scripts[F].clone();
    let cut = s.lines().find(|l| l.starts_with("let cut1")).unwrap_or_else(|| panic!("{s}"));
    assert!(cut.contains("at: [-") && cut.contains(", -"), "negative values, kept apart by commas: {cut}");
}

/// A distance-field part's box cutter is lowered into the part's field, not meshed: its box (for
/// the view's highlight and the feature gizmo) is its tagged subtree's there, in the model frame
/// under the carve's own placement, and a move of it is written into its `at`.
#[test]
fn a_field_parts_cutter_has_its_box_and_moves() {
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "r1", "let body = m.ellipsoid({radii: [10mm 10mm 10mm]})\nlet cut1 = m.box({size: [4mm 6mm 8mm] at: [5mm 0 0]})\nlet carved = m.carve(body, cut1, 0, {at: [0, 20mm, 0] cell: 1mm})\nobject(carved)\n");
    let cut = |ev: &Evaluated| ev.objects.iter().find(|o| o.name == "t/cut1").unwrap().clone();
    let (b, w) = (cut(&ev).bounds.expect("the cutter's box"), cut(&ev).world);
    let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-6);
    assert!(near(b.0, [3.0, 17.0, -4.0]) && near(b.1, [7.0, 23.0, 4.0]), "{b:?}");
    assert!(near([w[0][3], w[1][3], w[2][3]], [5.0, 20.0, 0.0]), "{w:?}");
    assert!(params::model(&c, &ev)[0].objects.iter().any(|o| o == "t/cut1"));
    // Up on the plate is up in the model (its Y).
    let (key, value) = params::placed(&c, &ev, 0, "t/cut1", params::Gesture::Move([0.0, 0.0, 2.0])).unwrap();
    let change = params::edit(&c, &params::model(&c, &ev), &key, &value).unwrap();
    let (_, _, ev) = apply(&mut m, &mut e, "move", vec![change]);
    assert!(m.state().scripts[F].contains("let cut1 = m.box({size: [4mm 6mm 8mm] at: [5mm 2mm 0]})"), "{}", m.state().scripts[F]);
    let b = cut(&ev).bounds.unwrap();
    assert!(near(b.0, [3.0, 19.0, -4.0]) && near(b.1, [7.0, 25.0, 4.0]), "{b:?}");
}

/// A project is its text and its scripts: saved, loaded back, the same document; the same
/// script placed twice with other parameter values is two instances.
#[test]
fn a_project_saves_its_scripts_and_loads_back() {
    let text = "// A Fab project: object scripts placed on the plate.\nuse mod.fab.*\nuse mod.model.param\n\nscript(\"t.cad.splash\")\n";
    let body = "let size = param(20mm, min: 5mm, max: 80mm)\nlet cube = m.box({size: [size size size]})\nobject(cube)\n";
    let source = format!("let m = mod.model\n{body}");
    let mut m = FabModel::load("test", text, [(F.to_string(), source.clone())].into()).expect("the project with two imports parses");
    assert_eq!(m.project_text(), text, "the project's ordered imports round-trip verbatim");
    let implicit = project::parse("mod.fab.script(\"t.cad.splash\")\n").expect("a project can use qualified calls");
    assert_eq!(project::write(&implicit, &|_, _| project::Unit::Plain), text.replace("use mod.model.param\n", ""), "a project with no use statements saves the usual Fab import");
    let script = m.compile_script(F).expect("object and param resolve through the project's imports");
    assert!(script.object("cube").is_some(), "the inherited object call emits the named cube");
    assert_eq!(script.source(), source, "the compiled object script retains its own source");

    let mut state = m.state().clone();
    state.scripts.insert(F.to_string(), format!("{source}    m.param(\"invalid\")\n"));
    let err = m.compile_ref(&state, &state.project.scripts[0], &[]).expect_err("the invalid param call must report a script error");
    assert!(err.file == F && err.message.contains("param needs a number"), "the error belongs to the failing object script: {err}");
    assert_eq!((err.line, err.col), (5, 12), "the error points to m.param's opening parenthesis in the script's own text");

    state.scripts.insert(F.to_string(), source);
    state.project = project::parse("use mod.model.param\nmod.fab.script(\"t.cad.splash\")\n").expect("a selective import project parses");
    let err = m.compile_ref(&state, &state.project.scripts[0], &[]).expect_err("removing the Fab import must invalidate the compiled cube");
    assert!(err.file == F && err.message.contains("variable object not found"), "the script reports the missing import: {err}");
    state.scripts.insert(F.to_string(), src("let param = 2mm\nlet cube = m.box({size: [param param param]})\nobject(cube)\n"));
    let explicit = m.compile_ref(&state, &state.project.scripts[0], &[]).expect("a script with its own use lines still builds");
    assert!(explicit.object("cube").is_some(), "the script's own Fab import and shadowing let build the cube");

    state.project = project::parse("let project_math = mod.math\nuse project_math.*\nmod.fab.script(\"t.cad.splash\")\n").expect("the import resolves in its original project");
    let err = m.compile_ref(&state, &state.project.scripts[0], &[]).expect_err("the import alone cannot resolve a project-local let");
    assert!(err.file.is_empty() && err.message.contains("use project_math.*"), "an invalid inherited import identifies the project and statement: {err}");

    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let (_, c, ev) = apply(&mut m, &mut e, "r2", vec![
        Change::AddInstance { file: F.into(), source: None, name: Some("big".into()), at: Some([250.0, 100.0]) },
        Change::Param { instance: "big".into(), name: "size".into(), value: Some(0.04) },
        Change::Color { name: "First".into(), set: Some(crate::project::ColorDef::new("First")) },
        Change::Color { name: "Second".into(), set: Some(crate::project::ColorDef::new("Second")) },
        pl("cube", PlaceArg::Color(Some("Second".into()))),
    ]);
    assert_eq!(c.instances.len(), 2);
    let big = ev.plate.parts.iter().find(|p| p.object == "big/cube").unwrap().mesh.bounds();
    assert!((big.size()[2] - 40.0).abs() < 1e-6 && (big.center()[0] - 250.0).abs() < 1e-6, "{big:?}");
    let small = ev.plate.parts.iter().find(|p| p.object == "t/cube").unwrap();
    assert!((small.mesh.bounds().size()[2] - 20.0).abs() < 1e-6 && small.filament == 2);
    let text = m.project_text();
    assert!(text.contains("script(\"t.cad.splash\")") && text.contains("params: {size: 40mm}") && text.contains("at: [250mm, 100mm]"), "{text}");
    let dir = std::env::temp_dir().join(format!("fab-project-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("cubes.fab.splash");
    project::save(&m, &path).unwrap();
    let loaded = project::load(&path, &ScriptEnv::default()).unwrap();
    assert_eq!(loaded.state(), m.state());
    assert_eq!(loaded.head().content, m.head().content);
    assert_eq!(loaded.head().generation, 0);
    let _ = std::fs::remove_dir_all(&dir);
    // A project holds no geometry.
    assert!(project::parse("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet b = m.box({size: [1mm 1mm 1mm]})\n").is_err());
}

/// A script that emits several objects: each is its own plate object, placed, moved and
/// left off the plate on its own.
#[test]
fn emitted_objects_are_placed_on_their_own() {
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, "r1", "let base = m.box({size: [40mm 6mm 30mm]})\nlet lid = m.box({size: [40mm 3mm 30mm]})\nobject(base)\nobject(lid, name: \"cover\")\n");
    let ids: Vec<&str> = ev.plate.parts.iter().map(|p| p.object.as_str()).collect();
    assert_eq!(ids, vec!["t/base", "t/lid"]);
    assert_eq!(ev.plate.parts[1].name, "cover");
    let lid = ev.plate.parts[1].mesh.bounds();
    let (_, _, ev) = apply(&mut m, &mut e, "r2", vec![pl("base", PlaceArg::At(Some([60.0, 60.0]))), pl("base", PlaceArg::Rotate([0.0, 0.0, 90.0]))]);
    assert_eq!(ev.plate.parts[1].mesh.bounds(), lid, "moving the base leaves the cover where it was");
    let (_, _, ev) = apply(&mut m, &mut e, "r3", vec![pl("cover", PlaceArg::Show(false))]);
    assert_eq!(ev.plate.parts.len(), 1);
    // The project names only objects the script emits.
    let head = m.head();
    assert!(m.apply(&mut e, "r4", head, &[pl("nothing", PlaceArg::Print(false))], None).is_err());
}

/// A let names the part it binds and changes nothing else: `object(m.union(slab, rise))` and
/// `let block = m.union(slab, rise); object(block)` emit the same part (its mesh, its place, its
/// colour, what the views draw) under the ids `union@7` and `block`. Only a placement that names
/// an object tells them apart (it names one or the other).
#[test]
fn a_let_names_the_part_it_binds_and_changes_nothing_else() {
    let parts = "let slab = m.box({size: [70mm 2mm 70mm] at: [0, 1mm, 0]})\nlet rise = m.box({size: [20mm 12mm 20mm] at: [0, 8mm, 0]})\n";
    let emitted = |body: String| {
        let (mut m, mut e) = setup();
        let (_, _, ev) = program(&mut m, &mut e, "r1", &format!("{parts}{body}"));
        assert_eq!(ev.plate.parts.len(), 1, "{body}");
        (ev.plate.parts[0].clone(), ev.render[0].clone())
    };
    let (inline, inline_render) = emitted("object(m.union(slab, rise))\n".into());
    let (bound, bound_render) = emitted("let block = m.union(slab, rise)\nobject(block)\n".into());
    assert_eq!((inline.object.as_str(), bound.object.as_str()), ("t/union@7", "t/block"));
    assert_eq!(fab_plate::Part { object: bound.object.clone(), name: bound.name.clone(), ..inline }, bound);
    assert_eq!(inline_render, bound_render);
}

#[test]
fn ids_come_from_lets_and_paths_and_survive_value_edits() {
    let (mut m, mut e) = setup();
    let body = "let holes = for i in 0..3 {\n    m.cylinder({radius: 2mm height: 20mm at: [i * 15mm - 15mm, 0, 0]})\n}\nlet plate = m.subtract(m.extrude({sketch: Rect{size: [60mm 30mm]} height: 4mm}), holes)\nobject(plate)\n";
    let (_, c, _) = program(&mut m, &mut e, "r1", body);
    let ids: Vec<&str> = prog(&c).solids.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["holes[0]", "holes[1]", "holes[2]", "plate.extrude", "plate"]);
    let (_, c2, _) = apply(&mut m, &mut e, "r2", vec![sc(Edit::Set { site: "plate.extrude.height".into(), value: "5mm".into() })]);
    assert_eq!(prog(&c2).solids.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ids);
    // Inserting before keeps every id too.
    let (_, c3, _) = apply(&mut m, &mut e, "r3", vec![sc(Edit::Add { code: "let spare = m.box({size: [5mm 5mm 5mm]})".into(), at: Anchor::Before("plate".into()) })]);
    for id in &ids {
        assert!(prog(&c3).solid(id).is_some(), "{id}");
    }
}

/// A texture's mapping and its knurl's kind are choice rows and the pattern's own literals are
/// rows of the part; a knurl finer than a line warns (and still slices).
#[test]
fn texture_rows_and_a_fine_knurl_warns() {
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "k", "let knob = m.cylinder({radius: 10mm height: 10mm})\nlet grip = m.texture(knob, {pattern: Knurl{pitch: 0.6mm angle: 30° kind: @diamond} map: @cylindrical depth: 0.3mm})\nobject(grip)\n");
    let model = params::model(&c, &ev);
    let row = |k: &str| model[0].params.iter().find(|p| p.key.ends_with(k)).unwrap_or_else(|| panic!("no {k}: {:?}", model[0].params.iter().map(|p| &p.key).collect::<Vec<_>>()));
    let map = row("grip.map");
    assert_eq!((map.kind, &map.value, map.choices[2].1.as_str()), (params::Kind::Choice, &params::PValue::Choice(1), "@planar"));
    let to_planar = params::edit(&c, &model, &map.key, &params::PValue::Choice(2)).unwrap();
    assert!(matches!(&to_planar, Change::Script { edit: Edit::Set { value, .. }, .. } if value == "@planar"), "{to_planar:?}");
    assert_eq!(row("grip.pattern.pitch").value, params::PValue::Num(0.6));
    assert!(matches!(row("grip.pattern.kind").value, params::PValue::Choice(_)));
    // Grooves 0.26 mm apart round a 10 mm radius, under the 0.4 mm line.
    let fine: Vec<_> = ev.printability.issues.iter().filter(|i| i.kind == "texture_fine").collect();
    assert!(fine.len() == 1 && (fine[0].measure - 0.26).abs() < 0.01, "{fine:?}");
}

#[test]
fn parameters_drive_solids_and_carry_kinds_handles_and_edits() {
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(
        &mut m,
        &mut e,
        "p1",
        "let h = param(30mm, min: 5mm, max: 100mm, label: \"Height\")\nlet wall = param(2mm)\nlet post = m.cylinder({radius: wall * 4 height: h segments: 32})\nlet fins = for x in [-9mm, 9mm] {\n    m.box({size: [2mm 10mm 12mm] at: [x, 0, 0]})\n    m.box({size: [12mm 10mm 2mm] at: [0, 0, x]})\n}\nlet part = m.union(post, fins, {at: [wall + 1, 0, 0]})\nobject(part)\n",
    );
    // A 30 mm post of radius 8 with four fins round it.
    let size = ev.plate.parts[0].mesh.bounds().size();
    assert!((size[2] - 30.0).abs() < 1e-6, "the post is 30 mm tall: {size:?}");
    assert!((size[0] - 20.0).abs() < 0.01, "fins reach 10 mm out: {size:?}");
    let model = params::model(&c, &ev);
    assert_eq!(model.len(), 1, "the post and fins share a part");
    let pp = &model[0];
    let find = |k: &str| pp.params.iter().find(|p| p.key == k).unwrap_or_else(|| panic!("no {k}: {:?}", pp.params.iter().map(|p| &p.key).collect::<Vec<_>>()));
    let h = find("t/h");
    assert!(h.declared && h.kind == params::Kind::Length && h.min == 5.0 && h.max == 100.0 && h.label == "Height", "height carries its declared kind, bounds and label: {h:?}");
    // Declared bounds are limits; a range nothing declared is only a drag's reach, never a limit,
    // and the AI is told the limits alone.
    assert_eq!(h.limits, [Some(5.0), Some(100.0)], "the declared bounds hold a set value");
    let wall = find("t/wall");
    assert!(wall.max > wall.min && wall.limits == [None, None], "an undeclared range is reach only: {wall:?}");
    let text = params::model_text(&model, &ev, true);
    let line = |k: &str| text.lines().find(|l| l.contains(&format!("key: \"{k}\""))).unwrap_or_else(|| panic!("no {k} in {text}")).to_string();
    assert!(line("t/h").contains("min: 5mm max: 100mm"), "the AI sees both height limits: {}", line("t/h"));
    assert!(!line("t/wall").contains("max:"), "the AI sees no limit for wall's soft reach: {}", line("t/wall"));
    assert!(line("t/post.radius").contains("formula: \"wall * 4\""), "the AI sees the radius formula on its row: {}", line("t/post.radius"));
    // The arrow's tip moves half a millimetre per millimetre of height (a centred cylinder).
    match &h.anchor {
        Some(params::Anchor::Linear { to, rate, .. }) => assert!((to[1] - 15.0).abs() < 1e-9 && (rate - 0.5).abs() < 1e-6, "height's arrow follows the post top: {to:?} {rate}"),
        other => panic!("height needs a linear handle: {other:?}"),
    }
    // `wall` sets a radius through `wall * 4`: its ring moves 4 mm per mm.
    match &find("t/wall").anchor {
        Some(params::Anchor::Radius { rate, .. }) => assert!((rate - 4.0).abs() < 1e-6, "wall's ring follows four times its change: {rate}"),
        other => panic!("wall needs the post's radius handle: {other:?}"),
    }
    let radius = find("t/post.radius");
    assert_eq!(radius.value.num(), Some(8.0), "the linked radius is in millimetres");
    assert_eq!(radius.formula.as_deref(), Some("wall * 4"), "the linked row keeps the expression as written");
    assert!(radius.anchor.is_none() && params::can_link(radius), "the linked radius accepts formulas and leaves its handle to wall");
    assert!(!pp.params.iter().any(|p| p.key == "t/post.height"), "a lone name keeps only its declared row");
    assert!(!pp.params.iter().any(|p| p.key == "t/part.at.0"), "a bare addend with a different script value stays code-only");
    assert_eq!(find("t/part.at.1").value.num(), Some(0.0), "the adjacent literal in the same position vector has a row");
    let fin = pp.params.iter().find(|p| p.key.ends_with(".size.1")).expect("a fin's height");
    assert!(fin.kind == params::Kind::Length && (fin.value.num().unwrap() - 10.0).abs() < 1e-9, "the literal fin height is a length in millimetres");
    assert_eq!(find("t/post.segments").kind, params::Kind::Count, "segments are a count");
    let fin_key = &fin.key;
    let is_fin = |p: &&params::Param| &p.key == fin_key;
    let fin_height = |ev: &Evaluated| {
        let (lo, hi) = ev.objects.iter().find(|o| o.name == fin.objects[0]).and_then(|o| o.bounds).expect("the fin has built bounds");
        hi[1] - lo[1]
    };
    let link = params::link(&c, &model, fin_key, "wall * 5").expect("a literal field takes a formula");
    assert_eq!(link, sc(Edit::Splice { range: fin.site.clone(), text: "wall * 5".into() }), "the link replaces exactly the fin height's source span");
    let refused = params::link(&c, &model, "t/h", "wall * 5").expect_err("a declared parameter holds its own number");
    assert!(refused.contains("Height") && refused.contains("holds a number"), "the refusal identifies the declared row: {refused}");
    assert!(params::link(&c, &model, fin_key, "wall * foo(2)").is_err(), "a formula cannot call script code");
    assert!(params::link(&c, &model, fin_key, "2 * 3").is_err(), "constant arithmetic goes through the number edit");
    let ambiguous = params::link(&c, &model, fin_key, "wall + 1").expect_err("a link must mean the same in box and script units");
    assert!(ambiguous.contains("wall + 1mm"), "the unit error gives the spelling for the intended sum: {ambiguous}");
    // A ratio's box shows percent and its script holds a fraction: a plain name doubled is 10% in
    // the box but a factor of 10 in Splash, so it is refused; a named fraction reads the same.
    let (n, ratio) = (
        makepad_script::syntax::quantity::NamedValue { name: "n".into(), label: "N".into(), value: 5.0, measure: makepad_script::syntax::quantity::Measure::Plain },
        makepad_script::syntax::quantity::NamedValue { name: "ratio".into(), label: "Ratio".into(), value: 0.25, measure: makepad_script::syntax::quantity::Measure::Fraction },
    );
    assert!(params::formula_value("n * 2", params::Kind::Ratio, std::slice::from_ref(&n)).is_err(), "a ratio formula whose box and script readings differ is refused");
    assert_eq!(params::formula_value("ratio * 2", params::Kind::Ratio, &[ratio]), Ok(0.5), "a ratio formula reads as the model's fraction");
    // A declared parameter is the instance's value; a part field is its script's literal.
    let edit = params::edit(&c, &model, "t/h", &params::PValue::Num(42.0)).unwrap();
    assert!(matches!(&edit, Change::Param { instance, name, value: Some(v) } if instance == "t" && name == "h" && (v - 0.042).abs() < 1e-12), "declared height is set in metres on its instance: {edit:?}");
    let field = params::edit(&c, &model, "t/post.segments", &params::PValue::Num(40.0)).unwrap();
    assert_eq!(field, sc(Edit::Set { site: "post.segments".into(), value: "40".into() }), "a literal count still uses Set");
    let (_, c, ev2) = apply(&mut m, &mut e, "p2", vec![edit, link]);
    assert!((ev2.plate.parts[0].mesh.bounds().size()[2] - 42.0).abs() < 1e-6, "the built post follows its edited height");
    let model = params::model(&c, &ev2);
    let linked = model[0].params.iter().find(is_fin).expect("the fin row survives linking");
    assert_eq!(linked.formula.as_deref(), Some("wall * 5"), "the rebuilt model keeps the fin formula");
    assert_eq!(linked.value.num(), Some(10.0), "the new link reads 10 mm");
    assert!((fin_height(&ev2) - 10.0).abs() < 1e-6, "the linked fin is 10 mm tall");
    // A preview evaluates without committing.
    let before = m.head();
    let (_, _, pv) = m.preview(&mut e, &[params::edit(&c, &model, "t/h", &params::PValue::Num(60.0)).unwrap()], None).unwrap();
    assert!((pv.plate.parts[0].mesh.bounds().size()[2] - 60.0).abs() < 1e-6, "the height preview rebuilds the post");
    assert_eq!(m.head(), before, "a preview leaves the document head in place");

    let wall_edit = params::edit(&c, &model, "t/wall", &params::PValue::Num(3.0)).unwrap();
    let (_, c, ev) = apply(&mut m, &mut e, "wall", vec![wall_edit]);
    let model = params::model(&c, &ev);
    let linked = model[0].params.iter().find(is_fin).expect("the fin row survives a wall edit");
    assert_eq!(linked.value.num(), Some(15.0), "the linked fin follows the instance's 3 mm wall");
    assert!((fin_height(&ev) - 15.0).abs() < 1e-6, "the built fin follows the wall to 15 mm");
    let zero = params::link(&c, &model, fin_key, "wall * (1mm / (wall - 3mm))").expect_err("the denominator is zero at the instance's wall value");
    assert!(zero.contains("divides by zero"), "link validation uses the edited wall value: {zero}");
    let relink = params::link(&c, &model, fin_key, "wall * 4").expect("an existing formula can be replaced");
    assert_eq!(relink, sc(Edit::Splice { range: linked.site.clone(), text: "wall * 4".into() }), "relink replaces the whole old formula");
    let (_, c, ev) = apply(&mut m, &mut e, "relink", vec![relink]);
    let model = params::model(&c, &ev);
    let linked = model[0].params.iter().find(is_fin).expect("the fin row survives relinking");
    assert_eq!((linked.value.num(), linked.formula.as_deref()), (Some(12.0), Some("wall * 4")), "the rebuilt fin follows the replacement formula");
    let unlink = params::edit(&c, &model, fin_key, &params::PValue::Num(12.0)).expect("a number unlinks the row");
    assert_eq!(unlink, sc(Edit::Splice { range: linked.site.clone(), text: "12mm".into() }), "a number replaces the formula even at the same value");
    let (_, c, ev) = apply(&mut m, &mut e, "unlink", vec![unlink]);
    let model = params::model(&c, &ev);
    let literal = model[0].params.iter().find(is_fin).expect("the fin row survives unlinking");
    assert_eq!((literal.value.num(), literal.formula.as_deref()), (Some(12.0), None), "the rebuilt fin holds a literal again");
}

/// A cage lists each point as a point row with its handle at the built point, each face as a
/// read-out row, and draws its wire; an edit writes the one point, its unmoved coordinates as
/// written, and the wire follows a dragged point before anything is built.
#[test]
fn a_cage_lists_its_points_and_faces_and_draws_its_wire() {
    use params::{Anchor, Kind, PValue};
    let (mut m, mut e) = setup();
    let body = "let box = m.cage({points: [[-8mm, 0, -8mm] [8mm, 0, -8mm] [8mm, 0, 8mm] [-8mm, 0, 8mm] [-8mm, 16mm, -8mm] [8mm, 16mm, -8mm] [8mm, 16mm, 8mm] [-8mm, 16mm, 8mm]] faces: [[0 1 2 3] [4 7 6 5] [0 4 5 1] [1 5 6 2] [2 6 7 3] [3 7 4 0]] levels: 2 at: [0 0 20mm]})\nobject(box)\n";
    let (_, c, ev) = program(&mut m, &mut e, "cage", body);
    let model = params::model(&c, &ev);
    let pp = &model[0];
    let row = |k: &str| pp.params.iter().find(|p| p.key == k).unwrap_or_else(|| panic!("no {k}: {:?}", pp.params.iter().map(|p| &p.key).collect::<Vec<_>>()));
    let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9);
    let p6 = row("t/box.points.6");
    assert_eq!((p6.kind, p6.label.as_str(), &p6.value, p6.derived), (Kind::Point, "Point 6", &PValue::Point(vec![8.0, 16.0, 8.0]), false));
    match &p6.anchor {
        Some(Anchor::Point { at, axes }) => assert!(near(*at, [8.0, 16.0, 28.0]) && axes.len() == 3, "{at:?} {axes:?}"),
        other => panic!("{other:?}"),
    }
    let f1 = row("t/box.faces.1");
    assert_eq!((f1.label.as_str(), &f1.value, f1.derived, f1.anchor.is_none()), ("Face 1", &PValue::Point(vec![4.0, 7.0, 6.0, 5.0]), true, true));
    assert!(params::edit(&c, &model, &f1.key, &PValue::Point(vec![4.0, 5.0, 6.0, 7.0])).is_err(), "faces are a read-out");
    // The wire: the cage's 8 points where they are built, its 12 edges once each, every point a row.
    let w = &pp.wires[0];
    assert_eq!((w.object.as_str(), w.points.len(), w.edges.len()), ("t/box", 8, 12));
    assert!(near(w.points[6], [8.0, 16.0, 28.0]) && w.keys.iter().all(Option::is_some), "{w:?}");
    // An edit writes the point; a plain zero moved takes millimetres, a negative a comma.
    let up = params::edit(&c, &model, &p6.key, &PValue::Point(vec![8.0, 20.0, 8.0])).unwrap();
    assert_eq!(up, sc(Edit::Set { site: "box.points.6".into(), value: "[8mm 20mm 8mm]".into() }));
    let p0 = params::edit(&c, &model, "t/box.points.0", &PValue::Point(vec![-8.0, 2.5, -8.0])).unwrap();
    assert_eq!(p0, sc(Edit::Set { site: "box.points.0".into(), value: "[-8mm, 2.5mm, -8mm]".into() }));
    // While dragged, the wire is the live script's (nothing built).
    let live = std::sync::Arc::new(params::with_value(&c, &model, &p6.key, &PValue::Point(vec![8.0, 20.0, 8.0])).unwrap());
    let during = params::model_with(&c, &ev, &model, "t", &live);
    assert!(near(during[0].wires[0].points[6], [8.0, 20.0, 28.0]), "{:?}", during[0].wires[0].points[6]);
    let (_, c2, ev2) = apply(&mut m, &mut e, "cage-up", vec![up]);
    assert_eq!(params::model(&c2, &ev2)[0].params.iter().find(|p| p.key == "t/box.points.6").map(|p| &p.value), Some(&PValue::Point(vec![8.0, 20.0, 8.0])));
}

/// Two rounded boxes that share coplanar faces (a shelf with a wall standing on its back
/// edge: bottom, back and both side faces coplanar) union into one closed part.
#[test]
fn rounded_boxes_with_coplanar_faces_union_closed() {
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(
        &mut m,
        &mut e,
        "shelf-wall",
        "let shelf = m.box({size: [64mm 6mm 46mm] radius: 1.2mm at: [0 3mm 0]})\nlet wall = m.box({size: [64mm 52mm 6mm] radius: 1.2mm at: [0, 26mm, -20mm]})\nlet unit = m.union(shelf, wall)\nobject(unit)\n",
    );
    assert_eq!(ev.plate.parts.len(), 1, "{:?}", ev.objects);
    let part = &ev.plate.parts[0];
    assert_eq!(part.mesh.open_edges(), 0, "the union of the rounded shelf and wall is closed");
    let v = part.mesh.volume();
    assert!(v > 64.0 * 6.0 * 46.0 && v < 64.0 * 6.0 * 46.0 + 64.0 * 52.0 * 6.0, "volume {v}");
}

/// The demo project (`demo.fab.splash` and its three object scripts).
pub(crate) fn demo() -> FabModel {
    let mut scripts = BTreeMap::new();
    scripts.insert("bracket.cad.splash".to_string(), include_str!("../../app/resources/bracket.cad.splash").to_string());
    scripts.insert("vase.cad.splash".to_string(), include_str!("../../app/resources/vase.cad.splash").to_string());
    scripts.insert("lattice_cube.cad.splash".to_string(), include_str!("../../app/resources/lattice_cube.cad.splash").to_string());
    FabModel::load("demo", include_str!("../../app/resources/demo.fab.splash"), scripts).unwrap_or_else(|f| panic!("{f}"))
}

/// The demo plate overlaps nothing. A part placed on another is a Printability error naming
/// both: the vase (a field) on the bracket (a mesh), the vase on the lattice (two fields).
#[test]
fn a_part_placed_on_another_on_the_demo_plate_is_an_overlap_error() {
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let m = demo();
    let overlaps = |ev: &eval::Evaluated| -> Vec<String> {
        ev.printability.issues.iter().filter(|i| i.kind == "overlap").map(|i| { assert_eq!(i.severity, fab_printability::Severity::Error, "{}", i.message); i.message.clone() }).collect()
    };
    let (_, ev) = m.evaluate(&mut e, None).unwrap();
    assert!(overlaps(&ev).is_empty(), "the demo's parts stand apart: {:?}", overlaps(&ev));
    let vase_at = |at: [f64; 2]| [Change::Place { instance: "vase".into(), object: "vase".into(), arg: PlaceArg::At(Some(at)) }];
    let (_, _, ev) = m.preview(&mut e, &vase_at([150.0, 165.0]), None).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(overlaps(&ev), ["Shelf bracket and Bud vase overlap: their bodies take the same space"], "the vase where the bracket stands");
    let (_, _, ev) = m.preview(&mut e, &vase_at([80.0, 165.0]), None).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(overlaps(&ev), ["Bud vase and Lattice cube overlap: their bodies take the same space"], "the vase over the lattice");
}

/// The demo plate builds: the bracket, the node-built vase and the Splash-kernel lattice,
/// three object scripts in one project. The vase's wall is its true thickness, inside its
/// outer surface: the belly is its radius wide.
#[test]
fn the_demo_plate_builds() {
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let m = demo();
    let (c, ev) = m.evaluate(&mut e, None).unwrap();
    let names: Vec<&str> = ev.plate.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["Shelf bracket", "Bud vase", "Lattice cube"]);
    let islands: Vec<&str> = ev.printability.issues.iter().filter(|i| i.kind == "floating_island").map(|i| i.message.as_str()).collect();
    assert!(islands.is_empty(), "nothing floats: {islands:?}");
    let vase = ev.plate.parts[1].bounds().size();
    assert!((vase[0] - 42.0).abs() < 1.0, "the belly's outside is 2 x 21 mm: {vase:?}");
    for p in &ev.plate.parts {
        assert_eq!(p.mesh.open_edges(), 0, "{}", p.name);
    }
    // The vase and the lattice are distance fields: laid out from their fields, never meshed.
    for (p, f) in ev.plate.parts.iter().zip(&ev.fields).skip(1) {
        assert!(f.is_some() && p.field_bounds.is_some() && p.mesh.triangles.is_empty(), "{} is a field part without a mesh", p.name);
        assert!(p.bounds().min[2].abs() < 0.02, "{} stands on the plate: {:?}", p.name, p.bounds());
    }
    let model = params::model(&c, &ev);
    let keys: Vec<&str> = model[1].params.iter().filter(|p| p.declared).map(|p| p.key.as_str()).collect();
    assert_eq!(keys, vec!["vase/vase_height", "vase/vase_belly", "vase/vase_neck", "vase/vase_wall", "vase/vase_blend"]);
    // The vase's height arrow (its handle call) moves 0.94 mm per mm of height; the bracket's
    // thickness arrow follows the thickness.
    match &model[1].params[0].anchor {
        Some(params::Anchor::Linear { rate, .. }) => assert!((rate - 0.94).abs() < 1e-3, "{rate}"),
        other => panic!("{other:?}"),
    }
    let t = model[0].params.iter().find(|p| p.key == "bracket/thickness").unwrap();
    assert!(matches!(t.anchor, Some(params::Anchor::Linear { rate, .. }) if (rate - 1.0).abs() < 1e-3), "{:?}", t.anchor);
}

/// The demo vase built from engine nodes (ellipsoid, cylinder, torus, blend, carve, shell) is
/// the shape its former Splash kernel described: the same volume and bounds.
#[test]
fn the_node_vase_is_the_kernel_vase() {
    let (mut m, mut e) = setup();
    let kernel = "let vase = m.sdf({\n    bounds: [[-25.25mm, 0, -25.25mm], [25.25mm, 72mm, 25.25mm]]\n    cell: 0.9mm\n    field: fn(p) {\n        let belly = sdf_ellipsoid(p - vec3(0, 23.8mm, 0), vec3(21mm, 25.2mm, 21mm))\n        let neck = sdf_cylinder(p - vec3(0, 51.8mm, 0), 9.5mm, 29.4mm)\n        let lip = sdf_torus(p - vec3(0, 65.8mm, 0), 10.7mm, 1.8mm)\n        let body = smooth_union(smooth_union(belly, neck, 9mm), lip, 9mm)\n        let foot = sdf_box(p - vec3(0, -7mm, 0), vec3(84mm, 20mm, 84mm))\n        let hollow = sdf_shell(sdf_difference(body, foot), 1.6mm)\n        let mouth = sdf_cylinder(p - vec3(0, 74mm, 0), 8.7mm, 20mm)\n        return sdf_difference(hollow, mouth)\n    }\n})\nobject(vase)\n";
    let nodes = "let belly = m.ellipsoid({radii: [21mm, 25.2mm, 21mm] at: [0, 23.8mm, 0]})\nlet neck = m.cylinder({radius: 9.5mm height: 29.4mm at: [0, 51.8mm, 0]})\nlet lip = m.torus({radius: 10.7mm tube: 1.8mm at: [0, 65.8mm, 0]})\nlet body = m.blend([belly neck lip], 9mm)\nlet foot = m.box({size: [84mm, 20mm, 84mm] at: [0, -7mm, 0]})\nlet footed = m.carve(body, foot, 0)\nlet hollow = m.shell(footed, 3.2mm, {open: @closed side: @both})\nlet mouth = m.cylinder({radius: 8.7mm height: 20mm at: [0, 74mm, 0]})\nlet vase = m.carve(hollow, mouth, 0, {cell: 0.9mm})\nobject(vase)\n";
    // Their meshes compared (meshed as for an export).
    e.mesh_fields = true;
    let (_, _, a) = program(&mut m, &mut e, "kernel", kernel);
    let (mut m, mut e) = setup();
    e.mesh_fields = true;
    let (_, _, b) = program(&mut m, &mut e, "nodes", nodes);
    let (a, b) = (&a.plate.parts[0].mesh, &b.plate.parts[0].mesh);
    assert_eq!(b.open_edges(), 0);
    let (va, vb) = (a.volume(), b.volume());
    assert!((va - vb).abs() < va * 0.01, "volume: kernel {va}, nodes {vb}");
    let (ba, bb) = (a.bounds().size(), b.bounds().size());
    for i in 0..3 {
        assert!((ba[i] - bb[i]).abs() < 0.9, "bounds: kernel {ba:?}, nodes {bb:?}");
    }
}

/// An imported mesh is a solid for sharp booleans and a distance field for smooth blends: a
/// keychain loop blended onto it, a hole carved through.
#[test]
fn an_imported_mesh_takes_booleans_and_smooth_blends() {
    use makepad_csg_graph::{Evaluator as Graph, Graph as G, Op};
    use makepad_model_script::lang::csg_sdf::mesh_field::{MeshData, MeshField};
    // The "figure": a 20 mm box (12 triangles: a mesh field is measured per triangle, 2000 triangles
    // made the blend below take 8 s; see the product perf notes), as a host-imported mesh.
    let mut g = G::new();
    let b = g.push(Op::Box { size: [20.0, 20.0, 20.0], radius: 0.0 }).unwrap();
    let m = Graph::new().mesh(&g, b, None).unwrap();
    let mut mm = m.as_ref().clone();
    mm.weld_vertices(1e-9);
    let stl = makepad_fab_slice::bench::stl_bytes(&makepad_fab_slicer::mesh::Mesh {
        verts: mm.vertices.iter().map(|v| [v.x, v.y, v.z]).collect(), tris: mm.triangles.clone(), paint: Vec::new(),
    });
    let imported = makepad_fab_slicer::mesh::load_stl(&stl).unwrap();
    let field = std::sync::Arc::new(MeshField::new(MeshData {
        positions: imported.verts.iter().map(|v| v.map(|x| x as f32)).collect(), triangles: imported.tris,
    }));
    let node = makepad_csg_sdf::SdfNode::Mesh(field.clone());
    let mut slots = Vec::new();
    fab_sdf_engine::kernel::collect(&node, &mut slots).unwrap();
    let lowered = fab_sdf_engine::program::lower_with(&node, &slots, 1.0).unwrap();
    for axis in 0..3 { for side in [-1.0, 1.0] { for offset in [-0.001, 0.0, 0.001] {
        let mut p = [0.37f32, -0.29, 0.63]; p[axis] = side * 10.0 + offset;
        let d = fab_sdf_engine::twin::eval_with(&lowered.texels, p, &slots).0;
        assert_eq!(d.to_bits(), field.signed(p).to_bits(), "imported STL {p:?}");
        let oracle = field.signed64(makepad_csg_math::Vec3d::new(p[0] as f64, p[1] as f64, p[2] as f64));
        assert!((d as f64 - oracle).abs() < 1e-5, "imported STL f64 {p:?}");
    } } }
    let (mut model, mut e) = setup();
    // The booleans' meshes are what is checked (meshed as for an export).
    e.mesh_fields = true;
    let mut assets = program::Assets::new();
    assets.insert("figure".into(), field);
    model.assets = std::sync::Arc::new(assets);
    let (_, c, ev) = program(
        &mut model,
        &mut e,
        "m1",
        "let figure = m.import(\"figure\")\nlet tag = m.blend([figure m.torus({radius: 4mm tube: 1.2mm at: [0, 12mm, 0] rotate: [90°, 0, 0]})], 1.5mm, {cell: 1mm})\nlet plate = m.subtract(figure, m.cylinder({radius: 3mm height: 30mm}))\nobject(tag)\nobject(plate)\n",
    );
    assert!(prog(&c).solid("tag").is_some());
    for p in &ev.plate.parts {
        assert_eq!(p.mesh.open_edges(), 0, "{}", p.object);
    }
    let tag = ev.plate.parts.iter().find(|p| p.object == "t/tag").unwrap();
    // The loop stands above the figure's top (10 mm + its ring).
    assert!(tag.mesh.bounds().size()[2] > 24.0, "{:?}", tag.mesh.bounds().size());
    let plate = ev.plate.parts.iter().find(|p| p.object == "t/plate").unwrap();
    let want = 8000.0 - std::f64::consts::PI * 9.0 * 20.0;
    assert!((plate.mesh.volume() - want).abs() < 0.02 * want, "the bore is cut: volume {} for {want}", plate.mesh.volume());
    // A missing mesh names the meshes there are.
    let head = model.head();
    let err = model.apply(&mut e, "m2", head, &[sc(Edit::Add { code: "let x = m.import(\"dog\")".into(), at: Anchor::Auto }), sc(Edit::Add { code: "object(x)".into(), at: Anchor::Auto })], None).unwrap_err();
    assert!(err.to_string().contains("no mesh 'dog'") && err.to_string().contains("figure"), "{err}");
}

#[test]
fn meshes_save_beside_the_project_and_load_back() {
    use makepad_model_script::lang::csg_sdf::mesh_field::{MeshData, MeshField};
    let dir = std::env::temp_dir().join(format!("fab-meshes-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let project_path = dir.join("cat.fab.splash");
    let data = MeshData { positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], triangles: vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]] };
    let mut assets = program::Assets::new();
    assets.insert("tet".into(), std::sync::Arc::new(MeshField::new(data.clone())));
    project::save_meshes(&project_path, &assets, &Default::default()).unwrap();
    let (back, _) = project::load_meshes(&project_path).unwrap();
    assert_eq!(back.get("tet").map(|m| &m.data), Some(&data));
    assert!(project::mesh_of_bytes(b"FMSH\xff\xff\xff\xff").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// An object with no name of its own is known by the line it is written on (`box@7`): placing it
/// writes that name into its code, so a statement added above it (a support override, a `let`)
/// moves its line, not its name or its placement.
#[test]
fn an_added_statement_keeps_an_unnamed_objects_placement() {
    let (mut m, mut e) = setup();
    let (_, c, _) = program(&mut m, &mut e, "r1", "let lid = m.box({size: [12mm 2mm 12mm]})\nobject(lid)\nobject(m.box({size: [10mm 10mm 10mm]}))\n");
    let name = c.instances[0].script.objects[1].name.clone();
    assert!(name.contains('@'), "{name}");
    let (_, _, ev) = apply(&mut m, &mut e, "r2", vec![pl(&name, PlaceArg::At(Some([50.0, 60.0])))]);
    let centre = |ev: &Evaluated| ev.plate.parts.iter().find(|p| p.object.ends_with(&name)).map(|p| p.mesh.bounds().center());
    let placed = (m.project_text(), centre(&ev).expect("placed"));
    for code in ["m.support_block(m.box({size: [5mm 5mm 5mm] at: [0 5mm 0]}))", "let peg = m.cylinder({radius: 1mm height: 4mm})"] {
        let (_, c, ev) = apply(&mut m, &mut e, code, vec![sc(Edit::Add { code: code.into(), at: Anchor::Auto })]);
        let script = m.source(F).unwrap();
        assert!(script.contains(code), "{script}");
        assert_eq!(c.instances[0].script.objects[1].name, name, "{script}");
        assert_eq!((m.project_text(), centre(&ev).expect("still placed")), placed, "{script}");
    }
    let script = m.source(F).unwrap();
    assert!(script.find("let peg").unwrap() < script.find(&format!("object(m.box({{size: [10mm 10mm 10mm]}}), name: \"{name}\")")).unwrap(), "{script}");
}

/// A project written before objects were named: an object known by its line placed by that
/// name (`place(.., object: "box@7")`), its code without one. It loads where it was placed,
/// and its next write names it, here a line typed above it in the code panel (a whole text).
/// A stale whole text that lacks the name (written from before the naming) keeps it, and an
/// object new on the line a named one left is known by a numbered line name.
#[test]
fn an_old_projects_line_named_object_is_named_on_its_next_write() {
    let body = "let lid = m.box({size: [12mm 2mm 12mm]})\nobject(lid)\nobject(m.box({size: [10mm 10mm 10mm]}))\n";
    let (mut first, mut e) = setup();
    program(&mut first, &mut e, "r1", body);
    apply(&mut first, &mut e, "r2", vec![pl("box@7", PlaceArg::At(Some([50.0, 60.0])))]);
    assert_eq!(first.source(F).unwrap(), src(&body.replace("10mm]}))", "10mm]}), name: \"box@7\")")));
    let project = first.project_text();
    assert!(project.contains("box@7"), "{project}");
    let mut m = FabModel::load("test", &project, BTreeMap::from([(F.to_string(), src(body))])).unwrap();
    let centre = |ev: &Evaluated| ev.plate.parts.iter().find(|p| p.object == "t/box@7").map(|p| p.mesh.bounds().center());
    let (_, ev) = m.evaluate(&mut e, None).unwrap();
    let placed = centre(&ev).expect("loads placed");
    let peg = "let peg = m.cylinder({radius: 1mm height: 4mm})\n";
    let typed = src(&body.replace("object(lid)\n", &format!("object(lid)\n{peg}")));
    let (_, _, ev) = apply(&mut m, &mut e, "r3", vec![sc(Edit::Program { source: typed.clone() })]);
    let named = typed.replace("10mm]}))", "10mm]}), name: \"box@7\")");
    assert_eq!(m.source(F).unwrap(), named);
    assert_eq!((m.project_text(), centre(&ev)), (project.clone(), Some(placed)));
    // Written from before the naming: the name stays.
    let stale = src(&format!("{peg}{body}"));
    let (_, _, ev) = apply(&mut m, &mut e, "r4", vec![sc(Edit::Program { source: stale.clone() })]);
    assert_eq!(m.source(F).unwrap(), stale.replace("10mm]}))", "10mm]}), name: \"box@7\")"));
    assert_eq!((m.project_text(), centre(&ev)), (project.clone(), Some(placed)));
    // A new object on line 7, whose line name `box@7` is written for another: `box@7 2`.
    let crowded = src(&format!("let a = 1\nlet b = 2\nobject(m.box({{size: [4mm 4mm 4mm]}}))\n{}", body.replace("10mm]}))", "10mm]}), name: \"box@7\")")));
    let (_, c, ev) = apply(&mut m, &mut e, "r5", vec![sc(Edit::Program { source: crowded.clone() })]);
    assert_eq!(m.source(F).unwrap(), crowded);
    let names: Vec<&str> = c.instances[0].script.objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["box@7 2", "lid", "box@7"]);
    assert_eq!(centre(&ev), Some(placed));
}

/// A `let` of a loop added on its own (FABIO): before the first `object`, and at the end of
/// a script that emits nothing yet; both evaluate.
#[test]
fn an_added_loop_let_evaluates() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "r1", BRACKET);
    let add = "let pegs = for i in 0..3 { m.cylinder({radius: 1mm height: 5mm at: [i * 4mm, 0, 0]}) }";
    let (_, c, _) = apply(&mut m, &mut e, "r2", vec![sc(Edit::Add { code: add.into(), at: Anchor::Auto })]);
    assert!(prog(&c).solid("pegs[2]").is_some(), "{}", m.source(F).unwrap());
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "r1", "let plate = m.box({size: [10mm 2mm 10mm]})\n");
    let (_, c, _) = apply(&mut m, &mut e, "r2", vec![sc(Edit::Add { code: add.into(), at: Anchor::Auto })]);
    assert!(prog(&c).solid("pegs[2]").is_some(), "{}", m.source(F).unwrap());
}

/// A field part with a painted region: its program, two colours; none without paint.
pub(crate) const PLAIN: &str = "let body = m.blend([m.box({size: [30mm 30mm 20mm]}) m.sphere({radius: 13mm})], 1mm)\nobject(body)\n";
pub(crate) const TWO_COLOUR: &str = "let red = Material{albedo: [0.8 0.05 0.05]}\nlet body = m.blend([m.box({size: [30mm 30mm 20mm]}) m.sphere({radius: 13mm})], 1mm)\nobject(body)\nm.fill(m.sphere({radius: 13mm at: [0 20mm 0]}), red)\n";

#[test]
fn a_painted_part_has_its_program_and_the_field_is_untouched() {
    let (mut m, mut e) = setup();
    let (_, _, plain) = program(&mut m, &mut e, "p0", PLAIN);
    assert!(plain.paint[0].is_none());
    let (_, _, ev) = program(&mut m, &mut e, "p1", TWO_COLOUR);
    assert_eq!(ev.plate.parts.len(), 1);
    let p = ev.paint[0].as_ref().expect("painted");
    assert_eq!(p.colors.len(), 2);
    assert!(p.colors[1].rgb[0] > 0.5 && p.colors[1].filament == Some(2), "an unnamed material is a colour of its own, on its own tool");
    // The model frame's top of the part (y 13 mm) is in the region; the box's corner is not.
    let to = |q: [f64; 3]| -> [f64; 3] { std::array::from_fn(|r| p.to_printer[r][0] * q[0] + p.to_printer[r][1] * q[1] + p.to_printer[r][2] * q[2] + p.to_printer[r][3]) };
    assert_eq!(p.slot_at(to([0.0, 13.0, 0.0])), 1);
    assert_eq!(p.slot_at(to([15.0, -10.0, 15.0])), 0);
    // Paint never enters the field: the same shape key, and nothing was built for it.
    assert_eq!(ev.fields[0].as_ref().unwrap().key, plain.fields[0].as_ref().unwrap().key, "the field is the same painted or not");
    assert_eq!(ev.built, 0, "a paint edit builds no mesh or field");
    assert!(ev.changes[0] != crate::eval::PartChange::Same, "the slicer recuts the painted part");
}

/// A chain at the tightest gaps a weave takes (0.1 mm in a layer and across layers).
const TIGHT_CHAIN: &str = "let chain = m.links([[-60mm 0 0] [60mm 0 0]], {size: 6mm bar: 0.8mm clearance: 0.1mm clearance_z: 0.1mm})\nobject(chain)\n";

/// Every link's slot in a painted weave, after checking the whole of each link has it: points
/// along every segment of its centreline and round it out to the tube's surface.
fn whole_link_slots(ev: &crate::eval::Evaluated) -> Vec<u8> {
    let p = ev.paint[0].as_ref().expect("painted");
    let w = &ev.weaves[0];
    w.links.loops.iter().enumerate().map(|(k, l)| {
        let mut seen = None;
        let n = l.pts.len();
        for i in 0..n {
            let (a, b) = (l.pts[i], l.pts[(i + 1) % n]);
            let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-9);
            let t = [d[0] / len, d[1] / len, d[2] / len];
            let s = if t[1].abs() < 0.9 { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] };
            let c = [t[1] * s[2] - t[2] * s[1], t[2] * s[0] - t[0] * s[2], t[0] * s[1] - t[1] * s[0]];
            let cl = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
            let u = [c[0] / cl, c[1] / cl, c[2] / cl];
            let v = [t[1] * u[2] - t[2] * u[1], t[2] * u[0] - t[0] * u[2], t[0] * u[1] - t[1] * u[0]];
            for f in [0.0, 0.25, 0.5, 0.75] {
                for r in [0.0, 0.5, 0.9, 1.0] {
                    for (cu, cv) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (0.7071, 0.7071), (-0.7071, 0.7071), (0.7071, -0.7071), (-0.7071, -0.7071)] {
                        let q: [f64; 3] = std::array::from_fn(|j| a[j] + f * d[j] + r * w.links.radius * (cu * u[j] + cv * v[j]));
                        let slot = p.slot_at(crate::weave::affine(&w.to_plate, q));
                        assert_eq!(*seen.get_or_insert(slot), slot, "link {k} has two materials (segment {i}, at {q:?})");
                    }
                }
            }
        }
        seen.unwrap_or(0)
    }).collect()
}

fn link_centre(l: &makepad_csg_sdf::LinkLoop) -> [f64; 3] {
    let n = l.pts.len() as f64;
    l.pts.iter().fold([0.0; 3], |a, q| [a[0] + q[0] / n, a[1] + q[1] / n, a[2] + q[2] / n])
}

/// A fill on a chain colours whole links: each link takes the material the fill gives at its
/// centre, so the whole of each link prints in that one material even where its neighbours pass
/// within 0.1 mm, the weave's link slots (the Mechanics view's colours, the weave tool's
/// painted links) are those picks, and a recolour builds nothing and leaves the field as it was.
#[test]
fn a_fill_on_a_chain_paints_whole_links() {
    let gold = |g: f64| format!("{TIGHT_CHAIN}m.fill(m.box({{size: [40mm 30mm 40mm] at: [21.3mm 0 0]}}), Material{{albedo: [0.85 {g} 0.12]}})\n");
    let (mut m, mut e) = setup();
    let (_, _, plain) = program(&mut m, &mut e, "c0", TIGHT_CHAIN);
    assert!(plain.weaves[0].instances.slots.iter().all(|s| *s == 0));
    let (_, _, ev) = program(&mut m, &mut e, "c1", &gold(0.62));
    // the box holds the centres from x = 1.3 to 41.3 mm (the links' frame is the model's here)
    let centres: Vec<[f64; 3]> = ev.weaves[0].links.loops.iter().map(link_centre).collect();
    assert!(centres.iter().all(|c| (c[0] - 1.3).abs() > 0.3 && (c[0] - 41.3).abs() > 0.3), "no centre on the box's faces: {centres:?}");
    let want: Vec<u8> = centres.iter().map(|c| (c[0] > 1.3 && c[0] < 41.3) as u8).collect();
    assert!(want.contains(&1) && want.contains(&0), "some links in the box, some out: {want:?}");
    assert_eq!(whole_link_slots(&ev), want, "each link is the material at its centre, all of it");
    assert_eq!(ev.weaves[0].instances.slots, want, "the weave's link slots are the picks");
    // a recolour: the same field, nothing built, the same picks
    let (_, _, again) = program(&mut m, &mut e, "c2", &gold(0.5));
    assert_eq!(again.fields[0].as_ref().unwrap().key, plain.fields[0].as_ref().unwrap().key, "the links' field is the same painted or not");
    assert_eq!(again.built, 0, "a recolour builds no mesh or field");
    assert_eq!(again.weaves[0].instances.slots, want);
}

/// A blend of two materials over a chain is dithered link by link: each whole link prints in one
/// of the two, about as many in the second as its share.
#[test]
fn a_mixed_fill_on_a_chain_dithers_by_link() {
    let source = format!("{TIGHT_CHAIN}let blue = Material{{albedo: [0.05 0.2 0.85]}}\nlet gold = Material{{albedo: [0.9 0.75 0.05]}}\nm.fill(chain, Mix{{materials: [blue gold] field: fn(p) {{ 0.3 }}}})\n");
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, "x1", &source);
    let slots = whole_link_slots(&ev);
    assert_eq!(ev.weaves[0].instances.slots, slots);
    let p = ev.paint[0].as_ref().expect("painted");
    let find = |want: fn([f32; 3]) -> bool| p.colors.iter().position(|c| want(c.rgb)).expect("the colour is in the palette") as u8;
    let (blue, gold) = (find(|c| c[2] > 0.5 && c[0] < 0.2), find(|c| c[0] > 0.5 && c[2] < 0.2));
    assert!(slots.iter().all(|s| *s == blue || *s == gold), "every link is blue or gold: {slots:?}");
    let share = slots.iter().filter(|s| **s == gold).count() as f64 / slots.len() as f64;
    assert!((share - 0.3).abs() < 0.12, "about three in ten links gold: {share} of {}", slots.len());
}

#[test]
fn material_names_reach_painted_parts() {
    let (mut m, mut e) = setup();
    let body = TWO_COLOUR.replace("albedo: [0.8 0.05 0.05]", "albedo: [0.8 0.05 0.05] name: \"Red\"");
    let (_, _, ev) = program(&mut m, &mut e, "p1", &body);
    let p = ev.paint[0].as_ref().expect("painted");
    assert_eq!(p.colors[0].color, None);
    assert_eq!(p.colors[1].color.as_deref(), Some("Red"));
    assert_eq!(p.colors[1].filament, Some(2));
}

/// A material field can follow a distance: shells round a point (`length(p)`) and layers under a
/// surface (an `sdf_` primitive's distance, here the part's own box), stacked by `noise.bands`
/// four millimetres a pair.
#[test]
fn a_material_field_stacks_shells_by_a_distance() {
    let (mut m, mut e) = setup();
    let slots = |m: &mut FabModel, e: &mut Evaluator, rev: &str, field: &str, at: &[[f64; 3]]| -> Vec<u8> {
        let source = format!("let blue = Material{{albedo: [0.05 0.2 0.85]}}\nlet yellow = Material{{albedo: [0.9 0.75 0.05]}}\nlet body = m.box({{size: [40mm 40mm 40mm]}})\nobject(body)\nm.fill(body, Mix{{materials: [blue yellow] field: fn(p) {{ {field} }} sharp: 1}})\n");
        let (_, _, ev) = program(m, e, rev, &source);
        let p = ev.paint[0].as_ref().expect("painted");
        let find = |want: fn([f32; 3]) -> bool| p.colors.iter().position(|c| want(c.rgb)).expect("the colour is in the palette") as u8;
        let (blue, yellow) = (find(|c| c[2] > 0.5 && c[0] < 0.2), find(|c| c[0] > 0.5 && c[2] < 0.2));
        at.iter().map(|q| match p.program.sample(fab_paint_field::Channel::Color, *q).slot {
            s if s == blue => b'b',
            s if s == yellow => b'y',
            s => panic!("{field}: slot {s} at {q:?}"),
        }).collect()
    };
    // Round the centre: yellow 1..3 mm out, blue 3..5, yellow 5..7, the same on a diagonal.
    let d = std::f64::consts::FRAC_1_SQRT_2;
    let shells = slots(&mut m, &mut e, "s1", "noise.bands(length(p) / 4mm)", &[[2.0, 0.0, 0.0], [4.0, 0.0, 0.0], [6.0, 0.0, 0.0], [0.0, 6.0 * d, 6.0 * d], [0.0, 0.0, 8.0]]);
    assert_eq!(shells, b"ybyyb".to_vec(), "shells round a point");
    // Under the box's faces: yellow 2 mm in, blue 4 mm in, on the +x face and the +y face alike.
    let layers = slots(&mut m, &mut e, "s2", "noise.bands(-sdf_box(p, vec3(40mm, 40mm, 40mm)) / 4mm)", &[[18.0, 0.0, 0.0], [16.0, 0.0, 0.0], [14.0, 0.0, 0.0], [0.0, 18.0, 0.0], [0.0, 16.0, 0.0], [3.0, 14.0, -2.0]]);
    assert_eq!(layers, b"ybyyby".to_vec(), "layers under a surface");
}

/// A Turing fill grows its pattern on the part's skin: a plate built for a test (or a picture)
/// has the state at its steps, both materials show on the surface and the slicer's sampler reads
/// the program the views bake. The script's presets are the simulation's.
#[test]
fn a_turing_fill_grows_on_the_part_at_its_steps() {
    assert_eq!(makepad_model_script::lang::protos::TURING_PRESETS, fab_paint_field::pattern::preset_names_of("Turing").as_slice());
    assert_eq!(makepad_model_script::lang::protos::LICHTENBERG_PRESETS, fab_paint_field::pattern::preset_names_of("Lichtenberg").as_slice());
    assert_eq!(makepad_model_script::lang::protos::SCALES_PRESETS, fab_paint_field::pattern::preset_names_of("Scales").as_slice());
    assert_eq!(makepad_model_script::lang::protos::PHYSARUM_PRESETS, fab_paint_field::pattern::preset_names_of("Physarum").as_slice());
    assert_eq!(makepad_model_script::lang::protos::FLOW_PRESETS, fab_paint_field::pattern::preset_names_of("Flow").as_slice());
    let (mut m, mut e) = setup();
    let source = "let teal = Material{albedo: [0.1 0.6 0.6]}\nlet dark = Material{albedo: [0.1 0.1 0.1]}\nlet body = m.sphere({radius: 12mm})\nobject(body)\nm.fill(body, Mix{materials: [teal dark] field: Turing{preset: @spots scale: 4mm seed: 2 steps: 1200} sharp: 1})\n";
    let (_, _, ev) = program(&mut m, &mut e, "t1", source);
    assert!(!ev.patterns_pending(), "the plate has the state that prints");
    let p = ev.paint[0].as_ref().expect("painted");
    let g = p.program.patterns().next().expect("a pattern");
    let grid = g.grid.as_ref().expect("grown");
    assert!(grid.done && grid.step == 1200, "{grid:?}");
    let sliced = ev.plate.parts[0].paint.as_ref().expect("the slicer's paint");
    let mut counts = [0usize; 8];
    for i in 0..400 {
        // a spiral over the sphere (model frame), into the printer's
        let t = (i as f64 + 0.5) / 400.0;
        let (z, a) = (1.0 - 2.0 * t, i as f64 * 2.399_963);
        let r = (1.0 - z * z).sqrt();
        let q = [12.0 * r * a.cos(), 12.0 * z, 12.0 * r * a.sin()];
        let m2p = &p.to_printer;
        let at: [f64; 3] = std::array::from_fn(|k| m2p[k][0] * q[0] + m2p[k][1] * q[1] + m2p[k][2] * q[2] + m2p[k][3]);
        let shown = p.program.sample(fab_paint_field::Channel::Color, q).slot;
        assert_eq!((sliced.sample_at)(at).slot, shown, "at {q:?}");
        counts[shown as usize] += 1;
    }
    assert!(counts[1] > 40 && counts[2] > 40, "both materials on the skin: {counts:?}");
}

/// A grown pattern's knobs are its part's controls, in a Pattern group: its growth (the live
/// counter), preset, scale, steps, its model's rates and depth, each edit written into the
/// script: a literal rewritten in its unit, a field not written added to the pattern, a preset
/// by its name. A field written as a declared parameter is that parameter's row (a seed's kind
/// gives it its Reseed).
#[test]
fn a_patterns_knobs_are_part_controls_that_write_the_script() {
    use params::Kind;
    let (mut m, mut e) = setup();
    let source = "let teal = Material{albedo: [0.1 0.6 0.6]}\nlet dark = Material{albedo: [0.1 0.1 0.1]}\nlet seed = param(3, min: 1, max: 999, kind: \"seed\")\nlet body = m.sphere({radius: 12mm})\nobject(body)\nm.fill(body, Mix{materials: [teal dark] field: Turing{preset: @spots scale: 4mm seed: seed steps: 300} sharp: 1})\n";
    let (_, c, ev) = program(&mut m, &mut e, "t1", source);
    let model = params::model(&c, &ev);
    let rows: Vec<(&str, Kind)> = model[0].params.iter().filter(|p| p.group == "Pattern").map(|p| (p.label.as_str(), p.kind)).collect();
    assert_eq!(rows, [("Grown", Kind::Growth), ("Preset", Kind::Choice), ("Scale", Kind::Length), ("Steps", Kind::Count), ("Feed", Kind::Number), ("Kill", Kind::Number), ("Depth", Kind::Length)]);
    assert_eq!(model[0].params.iter().find(|p| p.key == "t/seed").map(|p| p.kind), Some(Kind::Seed));
    let key = |f: &str| format!("t/body#turing.{f}");
    assert!(params::commits_only(&key("steps")) && !params::commits_only("t/body.radius"));
    let set = |m: &mut FabModel, e: &mut Evaluator, c: &Compiled, ev: &Evaluated, f: &str, v: params::PValue| {
        let change = params::edit(c, &params::model(c, ev), &key(f), &v).unwrap();
        apply(m, e, f, vec![change])
    };
    let (_, c, ev) = set(&mut m, &mut e, &c, &ev, "scale", params::PValue::Num(6.5));
    let (_, c, ev) = set(&mut m, &mut e, &c, &ev, "kill", params::PValue::Num(0.061));
    let stripes = fab_paint_field::pattern::preset_names().iter().position(|n| *n == "stripes").unwrap();
    let (_, c, ev) = set(&mut m, &mut e, &c, &ev, "preset", params::PValue::Choice(stripes));
    let script = &m.state().scripts[F];
    assert!(script.contains("Turing{preset: @stripes scale: 6.5mm seed: seed steps: 300 kill: 0.061}"), "{script}");
    let model = params::model(&c, &ev);
    let kill = model[0].params.iter().find(|p| p.key == key("kill")).unwrap();
    assert!(kill.value == params::PValue::Num(0.061) && !kill.site.is_empty(), "{kill:?}");
    assert!(!ev.patterns_pending(), "it grew to its steps");
}

#[test]
fn plate_colour_modes_preserve_picture_filaments_and_print_keys() {
    use fab_plate::{ColourMode, UNMAPPED_RGB};
    let (mut model, mut evaluator) = setup();
    let picture = crate::picture::Picture {
        bytes: Arc::new(vec![1]),
        image: Arc::new(fab_colour::Image { width: 2, height: 1, pixels: vec![[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]] }),
    };
    model.pictures = Arc::new([("tile.png".to_string(), Arc::new(picture))].into());
    let ink = [0.04, 0.25, 0.85];
    evaluator.mapped = Arc::new(eval::Mapped {
        inks: [("Red".into(), ink)].into(),
        tools: [("Red".into(), (1, 0)), ("White".into(), (2, 1))].into(),
    });
    let body = r#"let tile = m.picture({image: "tile.png" width: 2mm})
object(tile)
"#;
    let picked_srgb = 0xb01020ff;
    let colour = |name, rgb| Change::Color {
        name: String::from(name),
        set: Some(crate::project::ColorDef { rgb, ..crate::project::ColorDef::new(name) }),
    };
    let (_, _, mut ev) = apply(&mut model, &mut evaluator, "tile", vec![
        colour("Red", Some(picked_srgb)), colour("White", Some(0xffffffff)), sc(Edit::Program { source: src(body) }),
    ]);
    let [r, g, b, _] = crate::program::color_linear(picked_srgb);
    let picked = [r, g, b];
    let paint = ev.paint[0].as_ref().unwrap().clone();
    let slot = |name| paint.colors.iter().position(|c| c.color.as_deref() == Some(name)).unwrap();
    let red = slot("Red");
    let white = slot("White");
    assert_eq!(paint.palette()[red], picked);
    let keys = |ev: &Evaluated| ev.plate.parts.iter().zip(&ev.paint).map(|(part, paint)| {
        (part.filament, part.nozzle, part.paint.as_ref().map(|p| (p.key, p.states(part.filament))), paint.as_ref().map(|p| p.key()))
    }).collect::<Vec<_>>();
    let before = keys(&ev);
    let part = &ev.plate.parts[0];
    let own = ev.part_color(0).unwrap();
    assert_eq!(part.color[..3], eval::design_color(&ev.colors, own));
    let material_own = ev.mapped.inks.get(own).copied().unwrap_or(UNMAPPED_RGB);
    ev.set_colour_mode(ColourMode::Material);
    assert_eq!(ev.plate.parts[0].color[..3], material_own);
    let material = ev.paint[0].as_ref().unwrap();
    assert_eq!(material.palette()[red], ink, "mapped spool");
    assert_eq!(material.palette()[white], UNMAPPED_RGB, "mapped slot without ink");
    assert_eq!(eval::display_color(&ev.colors, &ev.mapped.inks, ev.colour_mode, "Unassigned"), UNMAPPED_RGB);
    assert_eq!(keys(&ev), before);
    assert!(Arc::ptr_eq(&paint.program, &material.program), "same baked filament selections");
    assert!(Arc::ptr_eq(paint.shown(), material.shown()), "same picture face");
    assert_eq!(&material.palette()[paint.colors.len()..], &paint.palette()[paint.colors.len()..], "same simulated stack colours");
    assert_eq!(ev.in_colours(ColourMode::Model).paint[0].as_ref().unwrap().palette(), paint.palette());
    evaluator.colour_mode = ColourMode::Material;
    let (_, built) = model.evaluate(&mut evaluator, None).unwrap();
    assert_eq!(keys(&built), before, "building under either mode picks the same picture filaments");
    assert_eq!(built.paint[0].as_ref().unwrap().palette(), material.palette());
    ev.set_colour_view(ColourMode::Material, false);
    assert_eq!(ev.paint[0].as_ref().unwrap().palette()[red], UNMAPPED_RGB, "a disconnected printer can still have a remembered tray");
    assert_eq!(ev.mapped.inks["Red"], ink, "disconnecting the display leaves the print inputs intact");
}

/// A grown pattern's plate prints each colour on the tool the job puts it on. A part built with
/// the job's tools known while its pattern grows (as the app builds, its views following the
/// simulation), its steps then set a little past where the simulation stands and the part built
/// again: that plate waits on the same simulation, with its paint's colours on their tools, and
/// so has the plate with the pattern grown, which the slice takes. Its colours mapped again
/// before the pattern has grown (the printer's tools changed), it grows on the new tools.
#[test]
fn a_grown_patterns_plate_prints_its_colours_on_their_tools() {
    let (mut m, mut e) = setup();
    let mapped = |teal: (u32, usize), dark: (u32, usize)| std::sync::Arc::new(eval::Mapped { inks: Default::default(), tools: [("Teal".to_string(), teal), ("Dark".to_string(), dark)].into() });
    e.mapped = mapped((1, 0), (2, 1));
    e.patterns_wait = false;
    let body = |steps: u32| format!("let teal = Material{{albedo: [0.1 0.6 0.6] name: \"Teal\"}}\nlet dark = Material{{albedo: [0.1 0.1 0.1] name: \"Dark\"}}\nlet body = m.sphere({{radius: 12mm}})\nobject(body)\nm.fill(body, Mix{{materials: [teal dark] field: Turing{{preset: @spots scale: 4mm seed: 5 steps: {steps}}} sharp: 1}})\n");
    let sim = |ev: &eval::Evaluated| ev.paint[0].as_ref().and_then(|p| p.program.patterns().next()).and_then(|g| g.sim.clone());
    let (_, _, growing) = program(&mut m, &mut e, "t1", &body(fab_paint_field::pattern::MAX_STEPS));
    let at = sim(&growing).expect("the pattern grows").step();
    let (_, _, ev) = program(&mut m, &mut e, "t2", &body(at + 200));
    assert!(ev.patterns_pending(), "the plate built again waits on the simulation, at {at} of {} steps", at + 200);
    let states = |ev: &eval::Evaluated| {
        let part = &ev.plate.parts[0];
        part.paint.as_ref().expect("the slicer's paint").states(part.filament)
    };
    assert!(states(&ev).contains(&2), "the job puts dark on tool 2: {:?}", states(&ev));
    fab_paint_field::pattern::live::work(&|n: usize, f: &(dyn Fn(usize) + Sync)| makepad_fab_slicer::par::fan(n, f), &|| false);
    let grown = ev.with_patterns_grown().expect("the pattern has grown");
    assert!(!grown.patterns_pending(), "the grown plate has the state that prints");
    assert_eq!(states(&grown), states(&ev), "the grown plate prints each colour on its tool");
    let mut remapped = ev.clone();
    remapped.set_mapped(mapped((2, 1), (1, 0)));
    assert_ne!(states(&remapped), states(&ev), "mapped again, the plate's colours change tools");
    let regrown = remapped.with_patterns_grown().expect("the pattern has grown");
    assert_eq!(states(&regrown), states(&remapped), "grown after the colours were mapped again, the plate prints each colour on its new tool");
    drop(growing);
}

/// A region's print settings (`m.settings(box, {..})`) are rows of its part in the box's group:
/// the infill density (a share), the pattern (the patterns a region takes) and the wall loops,
/// each as written or, not written, the table's to stand in for the part's. An edit rewrites a
/// written value, writes an unwritten one into the statement, and commits only (nothing
/// previews it). A second statement on the same box has rows of its own.
#[test]
fn marked_areas_are_counted_apart_from_the_solids_that_print() {
    let (mut m, mut e) = setup();
    // a body of two solids, one settings volume set twice, a no-trees area
    let source = "let body = m.union(m.box({size: [20mm 20mm 20mm]}), m.sphere({radius: 12mm}))\nobject(body)\nlet core = m.box({size: [6mm 6mm 6mm]})\nm.settings(core, {sparse_infill_density: \"100%\"})\nm.settings(core, {wall_loops: 4})\nm.no_trees(m.box({size: [8mm 8mm 8mm] at: [30mm 4mm 0]}))\n";
    let (_, c, _) = program(&mut m, &mut e, "t1", source);
    let p = prog(&c);
    assert_eq!((program::printed_solids(p), program::marked_areas(p, "settings"), program::marked_areas(p, "no_trees")), (3, 1, 1));
    // the printed body itself marked: still printed, and a volume too
    let source = "let body = m.box({size: [20mm 20mm 20mm]})\nobject(body)\nm.settings(body, {wall_loops: 4})\n";
    let (_, c, _) = program(&mut m, &mut e, "t2", source);
    let p = prog(&c);
    assert_eq!((program::printed_solids(p), program::marked_areas(p, "settings")), (1, 1));
}

#[test]
fn a_regions_print_settings_are_rows_that_write_its_statement() {
    use params::{Kind, PValue};
    let (mut m, mut e) = setup();
    let source = "let body = m.box({size: [20mm 20mm 20mm] at: [0 10mm 0]})\nobject(body)\nlet core = m.box({size: [6mm 6mm 6mm] at: [0 10mm 0]})\nm.settings(core, {sparse_infill_pattern: \"grid\" wall_loops: 3})\nm.settings(core, {sparse_infill_density: 40})\n";
    let (_, c, ev) = program(&mut m, &mut e, "t1", source);
    let rows = |c: &Compiled, ev: &Evaluated| params::model(c, ev).into_iter().flat_map(|pp| pp.params).filter(|p| params::region_setting(&p.key).is_some()).collect::<Vec<_>>();
    let all = rows(&c, &ev);
    let shown: Vec<(String, Kind, bool)> = all.iter().map(|p| (p.key.split_once('#').unwrap().1.to_string(), p.kind, p.site.is_empty())).collect();
    assert_eq!(shown, [
        ("settings.sparse_infill_density".to_string(), Kind::Ratio, true),
        ("settings.sparse_infill_pattern".to_string(), Kind::Choice, false),
        ("settings.wall_loops".to_string(), Kind::Count, false),
        ("settings2.sparse_infill_density".to_string(), Kind::Ratio, false),
        ("settings2.sparse_infill_pattern".to_string(), Kind::Choice, true),
        ("settings2.wall_loops".to_string(), Kind::Count, true),
    ]);
    let row = |rows: &[params::Param], tail: &str| rows.iter().find(|p| p.key.ends_with(tail)).cloned().unwrap();
    let pattern = row(&all, "#settings.sparse_infill_pattern");
    assert_eq!(pattern.choices[match pattern.value { PValue::Choice(i) => i, _ => unreachable!() }].1, "\"grid\"");
    assert!(!pattern.choices.iter().any(|(_, w)| w == "\"lightning\""), "a region takes no pattern that reads other layers");
    assert_eq!(row(&all, "#settings2.sparse_infill_density").value, PValue::Num(0.4), "40 is 40%");
    assert_eq!(row(&all, "#settings.wall_loops").group, "Core");
    assert!(params::commits_only(&pattern.key));
    let set = |m: &mut FabModel, e: &mut Evaluator, c: &Compiled, ev: &Evaluated, tail: &str, v: PValue| {
        let key = row(&rows(c, ev), tail).key;
        let change = params::edit(c, &params::model(c, ev), &key, &v).unwrap();
        apply(m, e, tail, vec![change])
    };
    let gyroid = pattern.choices.iter().position(|(_, w)| w == "\"gyroid\"").unwrap();
    let (_, c, ev) = set(&mut m, &mut e, &c, &ev, "#settings.sparse_infill_pattern", PValue::Choice(gyroid));
    let (_, c, ev) = set(&mut m, &mut e, &c, &ev, "#settings.sparse_infill_density", PValue::Num(0.25));
    let (_, c, ev) = set(&mut m, &mut e, &c, &ev, "#settings2.wall_loops", PValue::Num(5.0));
    let text = m.source(F).unwrap();
    assert!(text.ends_with("m.settings(core, {sparse_infill_pattern: \"gyroid\" wall_loops: 3 sparse_infill_density: \"25%\"})\nm.settings(core, {sparse_infill_density: 40 wall_loops: 5})\n"), "{text}");
    let values: Vec<Vec<(String, String)>> = ev.plate.parts[0].paint.as_ref().unwrap().settings.iter().map(|r| r.values.clone()).collect();
    let v = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
    assert_eq!(values, [v(&[("sparse_infill_density", "25%"), ("sparse_infill_pattern", "gyroid"), ("wall_loops", "3")]), v(&[("sparse_infill_density", "40%"), ("wall_loops", "5")])]);
    // A reset takes the setting out of its statement (first, middle, last, the only one): the
    // region prints with the part's again. One it does not write is refused.
    let reset = |m: &mut FabModel, e: &mut Evaluator, c: &Compiled, ev: &Evaluated, tail: &str| {
        let key = row(&rows(c, ev), tail).key;
        apply(m, e, &format!("reset {tail}"), vec![params::reset_region_setting(c, &key).unwrap()])
    };
    let (_, c, ev) = reset(&mut m, &mut e, &c, &ev, "#settings.wall_loops");
    let (_, c, ev) = reset(&mut m, &mut e, &c, &ev, "#settings2.sparse_infill_density");
    let (_, c, ev) = reset(&mut m, &mut e, &c, &ev, "#settings.sparse_infill_density");
    let text = m.source(F).unwrap();
    assert!(text.ends_with("m.settings(core, {sparse_infill_pattern: \"gyroid\"})\nm.settings(core, {wall_loops: 5})\n"), "{text}");
    assert!(params::reset_region_setting(&c, &row(&rows(&c, &ev), "#settings2.sparse_infill_density").key).is_err(), "one it does not write");
    let values: Vec<Vec<(String, String)>> = ev.plate.parts[0].paint.as_ref().unwrap().settings.iter().map(|r| r.values.clone()).collect();
    assert_eq!(values, [v(&[("sparse_infill_pattern", "gyroid")]), v(&[("wall_loops", "5")])]);
    // a statement's only setting takes the statement, the box's only statement the box
    let (_, c, ev) = reset(&mut m, &mut e, &c, &ev, "#settings2.wall_loops");
    assert!(m.source(F).unwrap().ends_with("m.settings(core, {sparse_infill_pattern: \"gyroid\"})\n"), "{}", m.source(F).unwrap());
    let (_, _, ev) = reset(&mut m, &mut e, &c, &ev, "#settings.sparse_infill_pattern");
    let text = m.source(F).unwrap();
    assert!(!text.contains("core") && text.ends_with("let body = m.box({size: [20mm 20mm 20mm] at: [0 10mm 0]})\nobject(body)\n"), "the box goes with its last setting: {text}");
    assert!(ev.plate.parts[0].paint.as_ref().is_none_or(|p| p.settings.is_empty()), "no region left");
}

/// What the views bake of a part's paint is keyed by its paint alone: another colour for its
/// material, or the part placed elsewhere on the bed, keeps the key (drawn anew, not baked);
/// painting another place moves it.
#[test]
fn a_parts_shown_paint_key_is_its_paint_not_its_colours_or_place() {
    let (mut m, mut e) = setup();
    let key = |ev: &crate::eval::Evaluated| ev.paint[0].as_ref().expect("painted").shown_key();
    let (_, _, ev) = program(&mut m, &mut e, "p1", TWO_COLOUR);
    let first = key(&ev);
    let (_, _, ev) = apply(&mut m, &mut e, "p2", vec![pl("body", PlaceArg::At(Some([120.0, 90.0])))]);
    assert_eq!(key(&ev), first, "placed elsewhere");
    let (_, _, ev) = program(&mut m, &mut e, "p3", &TWO_COLOUR.replace("albedo: [0.8 0.05 0.05]", "albedo: [0.1 0.3 0.9]"));
    assert_eq!(key(&ev), first, "another colour");
    let (_, _, ev) = program(&mut m, &mut e, "p4", &TWO_COLOUR.replace("at: [0 20mm 0]", "at: [0, -20mm, 0]"));
    assert_ne!(key(&ev), first, "painted elsewhere");
}

#[test]
fn placed_paint_pairs_override_a_material_name() {
    let (mut m, mut e) = setup();
    let body = TWO_COLOUR.replace("albedo: [0.8 0.05 0.05]", "albedo: [0.8 0.05 0.05] name: \"Red\"");
    program(&mut m, &mut e, "p1", &body);
    let key = crate::program::color_srgb([0.8, 0.05, 0.05]) | 0xff;
    let (_, c, ev) = apply(&mut m, &mut e, "p2", vec![pl("body", PlaceArg::Paint(vec![(key, "Blue".into())]))]);
    assert_eq!(c.instances[0].placed[0].paint.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(), ["Blue"]);
    let p = ev.paint[0].as_ref().expect("painted");
    assert_eq!(p.colors[1].color.as_deref(), Some("Blue"));
    assert_eq!(p.colors[1].filament, Some(2));
}

/// Renaming a print colour renames all that names it in one undo step: the project's colour, a
/// placement's `color:` and paint pair, and the `Material{name}`s of a fill and a stroke. A name
/// another colour has is refused, and so is a colour nothing names.
#[test]
fn renaming_a_colour_renames_what_names_it() {
    let (mut m, mut e) = setup();
    let body = "let body = m.box({size: [40mm 40mm 40mm]})\nobject(body)\nm.fill(m.sphere({radius: 6mm at: [0 20mm 0]}), Material{albedo: [0.8 0.05 0.05] name: \"Colour 2\"})\nm.stroke([[-10mm 20mm 10mm] [10mm 20mm 10mm]], 2mm, Material{albedo: [0.05 0.05 0.8] name: \" Colour 2 \"})\nm.fill(m.sphere({radius: 4mm at: [0, -20mm, 0]}), Material{albedo: [0.1 0.7 0.2]})\n";
    program(&mut m, &mut e, "r1", body);
    let green = crate::program::color_srgb([0.1, 0.7, 0.2]) | 0xff;
    let colour = |name: &str| Change::Color { name: name.into(), set: Some(crate::project::ColorDef::new(name)) };
    apply(&mut m, &mut e, "r2", vec![colour("Colour 1"), colour("Colour 2"), colour("Body"), pl("body", PlaceArg::Color(Some("Colour 2".into()))), pl("body", PlaceArg::Paint(vec![(green, "Colour 2".into())]))]);
    let before = m.head();
    let rename = |from: &str, to: &str| vec![Change::RenameColor { from: from.into(), to: to.into() }];
    let (applied, _, ev) = apply(&mut m, &mut e, "r3", rename("Colour 2", "Eyes"));
    assert_eq!(applied.changed, ["t"]);
    let names: Vec<&str> = m.project().colors.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Colour 1", "Eyes", "Body"]);
    let place = &m.project().places[0];
    assert_eq!((place.color.as_deref(), place.paint.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>()), (Some("Eyes"), vec!["Eyes"]));
    let source = m.source(F).unwrap();
    assert_eq!((source.matches("name: \"Eyes\"").count(), source.contains("Colour 2")), (2, false), "{source}");
    assert_eq!(ev.part_color(0), Some("Eyes"));
    let p = ev.paint[0].as_ref().expect("painted");
    assert_eq!(p.colors[1..].iter().map(|c| c.color.as_deref()).collect::<Vec<_>>(), [Some("Eyes"); 3]);
    // One undo step takes it all back.
    let (_, _, ev) = m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    assert_eq!(m.head().content, before.content);
    assert_eq!(ev.part_color(0), Some("Colour 2"));
    // A clash, a colour nothing names and an empty name change nothing.
    for (from, to, why) in [("Colour 2", "Body", "named 'Body' already"), ("Colour 2", "Colour 1", "named 'Colour 1' already"), ("Colour 9", "Hair", "no colour is named 'Colour 9'"), ("Colour 2", " ", "needs a name")] {
        let err = m.apply(&mut e, "bad", m.head(), &rename(from, to), None).unwrap_err().to_string();
        assert!(err.contains(why), "{from} -> {to}: {err}");
    }
    assert_eq!(m.head().content, before.content);
}

/// Features fill by their own solid, on every engine: a mesh part's holes, an exact part's bore.
#[test]
fn features_fill_on_mesh_and_exact_parts_and_strokes_are_one_entry() {
    let full = include_str!("../../app/resources/bracket.cad.splash");
    let bracket = &full[full.find("let m = mod.model\n").unwrap() + "let m = mod.model\n".len()..];
    let painted = format!("{bracket}\nlet red = Material{{albedo: [0.8 0.05 0.05]}}\nm.fill(holes, red)\nm.stroke([[0 6mm 10mm] [10mm 6mm 10mm] [20mm 6mm 12mm]], 2mm, red)\n");
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "b1", &painted);
    let p = ev.paint[0].as_ref().expect("the bracket paints");
    assert_eq!(p.program.layers.len(), 2, "one layer per statement");
    assert!(matches!(p.program.layers[1].region, fab_paint_field::Region::Stroke { ref points, .. } if points.len() == 3));
    assert!(prog(&c).solid("holes").is_some() || prog(&c).solid("holes[0]").is_some());
    // The hole's wall is painted, the shelf's far corner is not.
    assert!(matches!(p.program.layers[0].region, fab_paint_field::Region::Operand(_)));
    // The body, an extrusion, fills by the solid it makes.
    let (_, _, ev) = program(&mut m, &mut e, "b2", &format!("{bracket}\nm.fill(body, Material{{albedo: [0 0 1]}})\n"));
    let p = ev.paint[0].as_ref().expect("the body paints");
    assert!(matches!(p.program.layers[0].region, fab_paint_field::Region::Operand(_)));
}

/// A fill of each operand of a blend parts its fillet where the two are equally near, and the
/// print reads the paint as the views do: the slicer's sampler and the views' agree point for
/// point through the fillet's skin, and none of it is left the part's own filament.
#[test]
fn a_blend_s_fillet_prints_the_colours_the_views_show() {
    let source = "let orange = Material{albedo: [0.9 0.45 0.05]}\nlet blue = Material{albedo: [0.1 0.3 0.85]}\nlet left = m.ellipsoid({radii: [14mm 10mm 10mm] at: [-9mm 10mm 0]})\nlet right = m.ellipsoid({radii: [14mm 10mm 10mm] at: [9mm 10mm 0]})\nlet pair = m.blend([left right], 3mm)\nobject(pair)\nm.fill(left, orange)\nm.fill(right, blue)\n";
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, "f1", source);
    let p = ev.paint[0].as_ref().expect("the pair paints");
    let sliced = ev.plate.parts[0].paint.as_ref().expect("the slicer's paint");
    // through the valley's skin (its front is near z 8 at x = 0), either side of x = 0, model mm
    let qs: Vec<[f64; 3]> = [-3.0, -1.0, -0.3, -0.05, 0.05, 0.3, 1.0, 3.0].into_iter().flat_map(|x| (0..9).flat_map(move |k| (0..5).map(move |j| [x, 6.0 + 2.0 * j as f64, 6.0 + 0.5 * k as f64]))).collect();
    // as a view bakes it (the slope read within the lattice's reach)
    let shown = p.program.sample_near_many(fab_paint_field::Channel::Color, &qs, 1.0);
    let m2p = &p.to_printer;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for (q, s) in qs.iter().zip(&shown) {
        let at: [f64; 3] = std::array::from_fn(|k| m2p[k][0] * q[0] + m2p[k][1] * q[1] + m2p[k][2] * q[2] + m2p[k][3]);
        assert_eq!((sliced.sample_at)(at).slot, s.slot, "the print and the views differ at {q:?}");
        if q[0] < 0.0 { left.push(s.slot) } else { right.push(s.slot) }
    }
    let (a, b) = (left[0], right[0]);
    assert!(a != 0 && b != 0 && a != b, "the fillet takes its operands' colours, not the part's own: {a} left, {b} right");
    assert!(left.iter().all(|&s| s == a) && right.iter().all(|&s| s == b), "each side one colour: {left:?} | {right:?}");
}

/// A feature made by an op with no distance of its own fills by the solid it makes (its mesh's
/// distance): gold at each of `inside` (model mm), not at `outside`, elsewhere on the part.
fn fills_by_its_solid(rev: &str, base: &str, feature: &str, join: &str, inside: &[[f64; 3]], outside: &[[f64; 3]]) {
    let source = format!("let base = {base}\nlet feature = {feature}\nlet part = {join}(base, feature)\nobject(part)\nm.fill(feature, Material{{albedo: [0.9 0.7 0.1] name: \"Gold\"}})\n");
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, rev, &source);
    assert_eq!(slots(&ev, inside), vec![1; inside.len()], "{feature}: gold inside it");
    assert_eq!(slots(&ev, outside), vec![0; outside.len()], "{feature}: unpainted away from it");
}

/// A plate the features stand on, 4 mm thick.
const PLATE: &str = "m.box({size: [60mm 4mm 60mm] at: [0, 2mm, 0]})";

#[test]
fn an_extruded_feature_fills_by_its_solid() {
    fills_by_its_solid("x", PLATE, "m.extrude({sketch: Rect{size: [10mm 10mm]} height: 12mm both: true at: [0, 4mm, 0]})", "m.union", &[[0.0, 8.0, 0.0], [3.0, 9.0, -3.0]], &[[20.0, 2.0, 20.0], [-20.0, 2.0, 0.0]]);
}

#[test]
fn a_revolved_feature_fills_by_its_solid() {
    // a square ring 12..18 mm out, 4..10 mm up: gold round it, not in its middle
    fills_by_its_solid("r", PLATE, "m.revolve({sketch: Rect{size: [6mm 6mm] at: [15mm 7mm]}})", "m.union", &[[15.0, 7.0, 0.0], [0.0, 7.0, -15.0], [-10.6, 7.0, 10.6]], &[[0.0, 2.0, 0.0], [25.0, 2.0, 25.0]]);
}

#[test]
fn a_lofted_feature_fills_by_its_solid() {
    fills_by_its_solid("l", PLATE, "m.loft({sections: [Section{sketch: Rect{size: [16mm 16mm]} at: 4mm} Section{sketch: Circle{radius: 5mm} at: 14mm}]})", "m.union", &[[0.0, 9.0, 0.0], [5.0, 6.0, 5.0]], &[[20.0, 2.0, 20.0], [-20.0, 2.0, 0.0]]);
}

#[test]
fn a_swept_feature_fills_by_its_solid() {
    fills_by_its_solid("s", PLATE, "m.sweep({sketch: Circle{radius: 3mm} path: [[-15mm, 8mm, 0] [15mm, 8mm, 0]]})", "m.union", &[[0.0, 8.0, 0.0], [10.0, 8.0, 1.0]], &[[0.0, 2.0, 20.0], [20.0, 2.0, -20.0]]);
}

/// An exact body's feature fills by its tessellation.
#[test]
fn an_exact_feature_fills_by_its_solid() {
    fills_by_its_solid("e", "m.brep.box({size: [60mm 4mm 60mm] at: [0, 2mm, 0]})", "m.brep.revolve({sketch: Rect{size: [6mm 6mm] at: [15mm 7mm]}})", "m.brep.union", &[[15.0, 7.0, 0.0], [0.0, 7.0, -15.0]], &[[0.0, 2.0, 0.0], [25.0, 2.0, 25.0]]);
}
/// A script's body after its prelude (the tests' `src` adds it again).
fn body_of(full: &str) -> &str {
    &full[full.find("let m = mod.model\n").unwrap() + "let m = mod.model\n".len()..]
}

/// What a part's paint program holds at model points.
fn slots(ev: &Evaluated, points: &[[f64; 3]]) -> Vec<u8> {
    let p = ev.paint[0].as_ref().expect("painted");
    points.iter().map(|q| p.program.sample(fab_paint_field::Channel::Color, *q).slot).collect()
}

/// A smooth-region flood on a field (the vase, nothing meshed) spreads round the belly and up
/// the neck through its wide blend, and stops at the foot's crease, the lip's tight fillet and
/// the inner wall; a larger angle crosses the foot's crease.
#[test]
fn a_flood_fills_the_vase_belly_up_to_its_creases() {
    let vase = body_of(include_str!("../../app/resources/vase.cad.splash"));
    let (mut m, mut e) = setup();
    let fill = |angle: &str| format!("{vase}\nm.fill(Flood{{seed: [0 23.8mm 21mm] angle: {angle}}}, Material{{albedo: [0.8 0.05 0.05]}})\n");
    let (_, _, plain) = program(&mut m, &mut e, "v0", vase);
    let (_, _, ev) = program(&mut m, &mut e, "v1", &fill("30deg"));
    assert!(ev.fields[0].is_some(), "the vase is a field part");
    assert_eq!(ev.fields[0].as_ref().unwrap().key, plain.fields[0].as_ref().unwrap().key, "the field is the same painted or not");
    let p = ev.paint[0].as_ref().expect("painted");
    let fab_paint_field::Region::Patch(patch) = &p.program.layers[0].region else { panic!("the flood resolves on the vase: {:?}", p.program.layers[0].region) };
    println!("[flood] vase at 30deg: {:?}", patch.flooded());
    // round the belly, and on up the neck through its wide blend
    assert_eq!(slots(&ev, &[[0.0, 23.8, 21.0], [-21.0, 23.8, 0.0], [0.0, 23.8, -21.0], [0.0, 57.0, 9.5]]), vec![1, 1, 1, 1]);
    // not the flat foot, the inner wall, or the lip past its 1.5 mm fillet
    assert_eq!(slots(&ev, &[[8.0, 3.0, 0.0], [0.0, 23.8, 19.4], [0.0, 67.6, 10.7]]), vec![0, 0, 0]);
    assert_eq!(ev.built, 0, "a flood builds no mesh or field");
    // the foot's crease turns about 40 degrees: 45 crosses it onto the flat bottom
    let (_, _, ev) = program(&mut m, &mut e, "v2", &fill("45deg"));
    assert_eq!(slots(&ev, &[[0.0, 23.8, 21.0], [8.0, 3.0, 0.0]]), vec![1, 1], "45 degrees crosses the foot's crease");
    // a seed off the part paints nothing
    let off = format!("{vase}\nm.fill(Flood{{seed: [0 23.8mm 40mm]}}, Material{{albedo: [0.8 0.05 0.05]}})\n");
    let (_, _, ev) = program(&mut m, &mut e, "v3", &off);
    assert_eq!(slots(&ev, &[[0.0, 23.8, 21.0]]), vec![0]);
}

/// On a mesh part (the bracket, CSG) a flood from the shelf's top spreads over it and up the wall
/// through the wide inside fillet, and stops at the 1.2 mm rounded edges and at the holes.
#[test]
fn a_flood_fills_the_bracket_face_up_to_its_edges() {
    let bracket = body_of(include_str!("../../app/resources/bracket.cad.splash"));
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, "b1", &format!("{bracket}\nm.fill(Flood{{seed: [0 6mm 10mm] angle: 30deg}}, Material{{albedo: [0.8 0.05 0.05]}})\n"));
    assert!(ev.fields[0].is_none(), "the bracket is a mesh part");
    let p = ev.paint[0].as_ref().expect("painted");
    let fab_paint_field::Region::Patch(patch) = &p.program.layers[0].region else { panic!("the flood resolves on the bracket") };
    println!("[flood] bracket at 30deg: {:?}", patch.flooded());
    assert_eq!(slots(&ev, &[[0.0, 6.0, 10.0], [-25.0, 6.0, 20.0], [0.0, 40.0, -17.0]]), vec![1, 1, 1], "the shelf and the wall's face");
    assert_eq!(slots(&ev, &[[0.0, 0.0, 10.0], [0.0, 40.0, -23.0], [32.0, 3.0, 10.0], [0.0, 52.0, -20.0]]), vec![0, 0, 0, 0], "the bottom, the back, an end and the wall's top");
}

#[test]
fn placed_paint_pairs_are_checked_and_read_as_linear_rgb() {
    let source = |colors: &str| format!("use mod.fab.*\nscript(\"t.cad.splash\")\ncolor(\"Grey\")\ncolor(\"Red\")\nplace(\"t\", object: \"body\", paint: {colors})\n");
    let p = project::parse(&source("[[#808080 \"Grey\"] [#ff0000 \"Red\"]]")).unwrap();
    let placed = p.places[0].placed(0);
    let (rgb, name) = placed.paint[0].clone();
    assert_eq!(name, "Grey");
    assert!(rgb.into_iter().all(|c| (c - 0.21586).abs() < 1e-5));
    for colors in ["2", "[#ff0000 \"A\"]", "[[#ff0000]]", "[[#ff0000 \"A\" 3]]", "[[1 \"A\"]]", "[[#ff0000 2]]"] {
        assert!(project::parse(&source(colors)).is_err(), "{colors}");
    }
}

pub const CHAIN: &str = "let links = for i in 0..3 { m.box({size: [12mm 6mm 5mm] at: [i * 12mm + 6mm 3mm 0]}) }\nlet chain = m.chain(links)\nm.anchor(links[0], {at: [2.5mm 3mm 0]})\nobject(links[0], name: \"chain\")\n";

pub const PUPPET_ARM: &str = "let torso = m.box({size: [16mm 15mm 26mm], at: [0, 7.5mm, -13mm]})\nlet upper = m.box({size: [5mm 11mm 14mm], at: [13.5mm, 5.5mm, -13mm]})\nlet fore = m.box({size: [5mm 11mm 14mm], at: [13.5mm, 5.5mm, 1mm]})\nlet shoulder = m.ball(torso, upper, {at: [13.5mm, 3.9mm, -20mm], radius: 5.5mm, neck: 2mm, axis: [0, 0, 1], wall: 2mm})\nlet elbow = m.hinge(upper, fore, {at: [13.5mm, 0, -6mm], lower: -110°, upper: 110°})\nm.anchor(torso, {at: [0, 7.5mm, -13mm], fixed: true})\n";

/// A jointed chain through program, plate and slicer: the links are one object on the bed, the sim
/// names the program's joints, and on the sliced beads no joint fuses. (The design check and the sim
/// verdicts are `fab-ai`'s tool test and `csg_mech`'s chain test; the limit sweep over sliced beads,
/// `check_as_printed`, costs about 100 s per hinge: a product perf finding, so the bead gaps are read
/// instead, which is where fusing shows.)
#[test]
fn a_jointed_chain_prints_together_and_prints_free() {
    use makepad_fab_settings::profiles::{JobSetup, Preset};
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "chain", CHAIN);
    assert_eq!(prog(&c).joints.len(), 3, "2 hinges and the anchor");
    assert_eq!(ev.plate.parts.len(), 3, "the jointed links are one object of three bodies");
    let mech = ev.mech.first().expect("joints applied");
    for p in &ev.plate.parts {
        assert!(p.mesh.bounds().min[2].abs() < 1e-6, "{} lifted: one group on the bed", p.object);
    }
    let asm = mech.assembly(&[], 1.0).unwrap();
    let hang = mech.simulate(&asm, &makepad_csg_mech::sim::SimOptions { seconds: 1.0, ..Default::default() }, &|| false).unwrap();
    assert_eq!(hang.joints[0].name, "chain[0]", "the sim names the program's joints");
    // As printed: slice the plate and read the beads at every joint.
    let setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default()], filament_map: vec![1; ev.plate.parts.len()], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    let sliced = makepad_fab_slice::plate::slice_plate(&setup, &ev.plate).unwrap();
    let plate_part: Vec<Option<usize>> = mech.solids.iter().map(|s| ev.objects[*s].part).collect();
    for j in 0..mech.plan.joints.len() {
        let to_plate = ev.model_to_plate[plate_part[mech.plan.decls[j].parts().0].unwrap()];
        let p = mech.printed_joint(j, &sliced.layers(), &plate_part, &to_plate).expect("both links print beads at the joint");
        assert!(p.bead_gap > 0.1 && p.stacked_gap > 0.1, "joint {j} fuses on the sliced beads: in-layer gap {:.3}, stacked gap {:.3}", p.bead_gap, p.stacked_gap);
        assert!(!p.support_bridges, "joint {j}: support touches both links");
    }
}

/// A rail with a carriage on it (a speed drive moves it) and a base with a disc on it, each pair on
/// the bed as it stands.
const SLIDE_AND_TURN: &str = "let rail = m.box({size: [60mm 6mm 16mm] at: [30mm 3mm 0]})\nlet carriage = m.box({size: [12mm 16mm 24mm] at: [26mm 8mm 0]})\nlet way = m.slider(rail, carriage, {at: [26mm, 0, 0], axis: [1 0 0] motor: @speed})\nlet base = m.box({size: [30mm 6mm 30mm] at: [0, 3mm, 60mm]})\nlet disc = m.cylinder({radius: 9mm height: 10mm at: [0, 5mm, 60mm]})\nlet turntable = m.bearing(base, disc, {at: [0, 0, 60mm]})\n";

/// `m.slider` and `m.bearing` from a script: every part on the bed as it stands, both joints
/// free on the check (the carriage to its end stops 2 mm short of the rail's ends, the disc all
/// the way round), and the sim takes a slide with its motor (a slider's default force) and a free
/// turn. A force with no motor fails at the slider's line.
#[test]
fn a_slider_and_a_bearing_from_a_script_stand_on_the_bed_and_move() {
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "slide", SLIDE_AND_TURN);
    assert_eq!(prog(&c).joints.iter().map(|j| j.proto).collect::<Vec<_>>(), ["Slider", "Bearing"]);
    assert_eq!(ev.plate.parts.len(), 4, "two jointed pairs of two bodies");
    for p in &ev.plate.parts {
        assert!(p.mesh.bounds().min[2].abs() < 1e-6, "{} is off the bed", p.object);
    }
    let mech = ev.mech.first().expect("joints applied");
    let checks = mech.check(&|| false);
    assert_eq!(checks.len(), 2);
    for c in &checks {
        assert!(c.ok(), "the {} binds: {:?}", c.kind, c.binds);
    }
    let slide = checks.iter().find(|c| c.kind == "slider").expect("the slider is checked");
    assert!((slide.effective.0 + 18.0).abs() < 0.6 && (slide.effective.1 - 26.0).abs() < 0.6, "to its end stops: {:?}", slide.effective);
    let turn = checks.iter().find(|c| c.kind == "bearing").expect("the bearing is checked");
    assert!(turn.effective.1 - turn.effective.0 >= 2.0 * std::f64::consts::PI - 1e-6, "all the way round: {:?}", turn.effective);
    let asm = mech.assembly(&checks, 1.0).unwrap();
    assert!(matches!(asm.joints[0].kind, makepad_csg_mech::sim::JointKind::Prismatic { .. }) && asm.joints[1].kind.turns_freely(), "{:?}", asm.joints.iter().map(|j| &j.kind).collect::<Vec<_>>());
    let drive = makepad_csg_mech::sim::Motor { kind: makepad_csg_mech::MotorKind::Speed, limit: crate::mech::MOTOR_FORCE * 1e6 };
    assert_eq!((asm.joints[0].motor, asm.joints[1].motor), (Some(drive), None), "the slider's drive pushes with 10 N (g mm/s^2)");
    let bad = FabModel::with_script("t", "t.cad.splash", &src(&SLIDE_AND_TURN.replace("motor: @speed", "force: 3")));
    let err = bad.evaluate(&mut e, None).expect_err("a force with no motor fails");
    let line = SLIDE_AND_TURN.lines().position(|l| l.starts_with("let way = ")).unwrap() as u32 + 5;
    assert!(err.line == line && err.message.contains("give motor: @position or @speed"), "{err}");
}

/// A bar held by hand and a block beside it on a string, each its own object.
const STRUNG: &str = "let bar = m.box({size: [60mm 6mm 8mm] at: [0 3mm 0]})\nlet block = m.box({size: [12mm 6mm 12mm] at: [0 3mm 40mm]})\nlet s = m.string(bar, block, {from: [0 3mm 0] to: [0 3mm 40mm] length: 60mm})\nm.anchor(bar, {at: [0 3mm 0] held: true})\nobject(bar)\nobject(block)\n";

/// `m.string` from a script: the bar and the block stay two objects (a string joins nothing in print), the
/// sim has the string between them and the bar held by hand; a hole outside its part or a string shorter
/// than its path fails at the string's line, naming the part.
#[test]
fn a_string_holds_two_objects_in_the_sim_and_refuses_at_its_line() {
    use makepad_csg_mech::sim::{AnchorKind, StringSpec};
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "strung", STRUNG);
    assert_eq!(prog(&c).joints.iter().map(|j| j.proto).collect::<Vec<_>>(), ["String", "Anchor"]);
    assert_eq!(ev.plate.parts.len(), 2, "two objects, each on the bed");
    let mech = ev.mech.first().expect("a strung piece");
    let asm = mech.assembly(&[], 1.0).unwrap();
    assert_eq!(asm.strings, [StringSpec { a: 0, from: [0.0, 3.0, 0.0], b: 1, to: [0.0, 3.0, 40.0], through: Vec::new(), length: 60.0 }]);
    assert_eq!((asm.anchors.len(), &asm.anchors[0].kind), (1, &AnchorKind::Held), "the hand holds the bar");
    assert!(asm.joints.is_empty());
    let line = STRUNG.lines().position(|l| l.starts_with("let s = ")).unwrap() as u32 + 5;
    for (edit, says) in [("to: [0 3mm 40mm]", "to: [0 3mm 50mm]"), ("length: 60mm", "length: 30mm")].map(|(from, to)| (STRUNG.replace(from, to), to)) {
        let bad = FabModel::with_script("t", "t.cad.splash", &src(&edit));
        let err = bad.evaluate(&mut e, None).expect_err("a bad string fails");
        assert!(err.id == "s" && err.line == line, "{says}: at the string: {err}");
        let want = if says.starts_with("to") { "its end is not inside part 'block'".to_string() } else { "it is 30.0 mm long, but its path as printed is 40.0 mm".to_string() };
        assert!(err.message.starts_with(&want), "{says}: {err}");
    }
}

/// Three blocks on a round spine through them, a free run of it between each two; the spine
/// filled with the "Spine" colour.
const SPINE_ROW: &str = "let spine = m.cylinder({radius: 1.5mm height: 50mm at: [25mm 2.5mm 0] rotate: [0, 0, 90°]})\nlet segs = for i in 0..3 { m.box({size: [10mm 5mm 8mm] at: [i * 16mm + 9mm 2.5mm 0]}) }\nlet back = m.bond(spine, segs)\nm.fill(spine, Material{albedo: [0.2 0.3 0.9] name: \"Spine\"})\n";

/// `m.bond` from a script: a spine of a TPU colour bends the blocks bonded to it each to the next
/// where it runs free between them (two flexes, as long as those runs, round so bending every
/// way); the sim has them as soft joints and the spine riding the first block. In a rigid colour
/// the same spine holds them as one: no flexes, only welds. The parts print as one group.
#[test]
fn blocks_bonded_to_a_tpu_spine_flex_and_to_a_rigid_one_hold() {
    use makepad_csg_mech::sim::JointKind;
    let (mut m, mut e) = setup();
    let colour = |material: &str| Change::Color { name: "Spine".into(), set: Some(crate::project::ColorDef { material: Some(material.into()), ..crate::project::ColorDef::new("Spine") }) };
    let (_, c, ev) = apply(&mut m, &mut e, "bond", vec![colour("TPU"), sc(Edit::Program { source: src(SPINE_ROW) })]);
    assert_eq!(prog(&c).joints.iter().map(|j| j.proto).collect::<Vec<_>>(), ["Bond"; 3]);
    assert_eq!(ev.plate.parts.len(), 4, "the spine and three blocks, one object");
    // one group: the blocks on the bed, the spine where it runs through them, not dropped apart
    for p in &ev.plate.parts {
        let want = if p.object.ends_with("spine") { 1.0 } else { 0.0 };
        assert!((p.mesh.bounds().min[2] - want).abs() < 1e-3, "{} at {:.3}: one group on the bed", p.object, p.mesh.bounds().min[2]);
    }
    let mech = ev.mech.first().expect("bonds applied");
    assert!(mech.plan.bonds[0].flexible, "a TPU spine");
    assert!(mech.flexes().is_none(), "the build leaves the bends to be measured after it");
    let flexes = mech.measure_flexes().as_ref().unwrap();
    assert_eq!(flexes.len(), 2);
    for f in flexes {
        assert!((f.length - 6.0).abs() < 0.4 && !f.one_way(), "a free run of 6 mm, round: {f:?}");
    }
    let asm = mech.assembly(&[], 1.0).unwrap();
    let kinds: Vec<&JointKind> = asm.joints.iter().map(|j| &j.kind).collect();
    assert_eq!(kinds.iter().filter(|k| matches!(k, JointKind::Flex { one_way: false, .. })).count(), 2, "{kinds:?}");
    assert_eq!(kinds.iter().filter(|k| ***k == JointKind::Weld).count(), 1, "the spine rides the first block: {kinds:?}");
    // in a rigid colour the spine holds them as one
    let (_, _, ev) = apply(&mut m, &mut e, "bond-rigid", vec![colour("PLA")]);
    let mech = ev.mech.first().expect("bonds applied");
    assert!(!mech.plan.bonds[0].flexible && mech.measure_flexes().as_ref().unwrap().is_empty());
    assert!(mech.assembly(&[], 1.0).unwrap().joints.iter().all(|j| j.kind == JointKind::Weld));
}

/// Blocks that meet round a TPU spine leave it no free run to bend in. The plate builds (the
/// bends are measured after the build); the measure fails at the bond with the text a build
/// failed with before; the plate's report then has that text as an error, once.
#[test]
fn a_bond_with_no_free_run_builds_and_its_report_says_why() {
    let (mut m, mut e) = setup();
    let tpu = Change::Color { name: "Spine".into(), set: Some(crate::project::ColorDef { material: Some("TPU".into()), ..crate::project::ColorDef::new("Spine") }) };
    let row = SPINE_ROW.replace("i * 16mm + 9mm", "i * 8mm + 9mm");
    let (_, _, mut ev) = apply(&mut m, &mut e, "bad-bond", vec![tpu, sc(Edit::Program { source: src(&row) })]);
    let mech = ev.mech.first().expect("bonds applied").clone();
    assert!(mech.flexes().is_none(), "the build leaves the bends to be measured after it");
    let Err(f) = mech.measure_flexes() else { panic!("the blocks meet round the spine") };
    assert!(f.op == "joint" && f.message.contains("meet round"), "{f}");
    assert!(f.to_string().starts_with(&format!("{}:{}:", f.file, f.line)) && !f.file.is_empty(), "at the bond's call: {f}");
    assert!(ev.add_bend_issues(), "the report takes it");
    let said: Vec<_> = ev.printability.issues.iter().filter(|i| i.kind == eval::BAD_BOND).collect();
    assert_eq!(said.len(), 1);
    assert_eq!((said[0].severity, said[0].message.clone()), (fab_printability::Severity::Error, f.to_string()));
    assert!(said[0].object.as_deref().is_some_and(|o| o.ends_with("spine")), "at the spine: {:?}", said[0].object);
    assert!(!ev.add_bend_issues(), "once");
}

/// Two hinged plates, the object named by the first and the second filled gold.
const HINGED_FILL: &str = "let a = m.box({size: [20mm, 6mm, 20mm] at: [0, 3mm, 0]})\nlet b = m.box({size: [20mm, 6mm, 20mm] at: [22mm, 3mm, 0]})\nlet pa = m.union(a, m.cylinder({radius: 4mm height: 6mm at: [11mm, 3mm, 0]}))\nlet pb = m.union(b, m.cylinder({radius: 4mm height: 6mm at: [11mm, 3mm, 0]}))\nlet h = m.hinge(pa, pb, {at: [11mm, 0, 0]})\nobject(pa, name: \"Probe\")\nm.fill(pb, Material{albedo: [0.9 0.7 0.1] name: \"Gold\"})\n";

/// A fill of one body of a jointed object by its own name makes that body's part print in the
/// material all through, as its own colour (its first layer and its inside too, where paint
/// prints coarsely), and not the other body's: each body is a part of its own. A fill after it
/// paints over it as paint does.
#[test]
fn a_fill_of_one_jointed_body_makes_it_that_bodys_own_colour() {
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, "hinged", HINGED_FILL);
    assert_eq!(ev.plate.parts.len(), 2, "two bodies, two parts");
    let mut own: Vec<(bool, String, bool)> = (0..2).map(|i| (ev.plate.parts[i].object.ends_with("pb"), ev.part_color(i).unwrap_or_default().to_string(), ev.paint[i].is_some())).collect();
    own.sort();
    assert_eq!(own, vec![(false, "Colour 1".to_string(), false), (true, "Gold".to_string(), false)], "(is pb, its colour, painted) per part");
    // A band painted over the gold body after its fill paints over the gold, on that body.
    let over = format!("{HINGED_FILL}m.fill(m.box({{size: [60mm, 2mm, 60mm] at: [0, 5mm, 0]}}), Material{{albedo: [0.1 0.2 0.8] name: \"Blue\"}})\n");
    let (_, _, ev) = program(&mut m, &mut e, "hinged2", &over);
    let i = (0..2).find(|&i| ev.plate.parts[i].object.ends_with("pb")).unwrap();
    assert_eq!(ev.part_color(i), Some("Gold"));
    let p = ev.paint[i].as_ref().expect("the band paints over the gold body");
    let b = ev.plate.parts[i].mesh.bounds();
    let mid = |z: f64| [b.max[0] - 10.0, 0.5 * (b.min[1] + b.max[1]), z];
    let named = |q: [f64; 3]| p.colors.get(p.slot_at(q) as usize).and_then(|c| c.color.clone());
    assert_eq!(named(mid(b.max[2] - 0.5)), Some("Blue".to_string()), "the band at the top");
    assert_eq!(named(mid(b.min[2] + 1.0)), None, "the body's own gold under it");
}

/// The program's ball and hinge become joints of three bodies, their halves written into the parts.
/// (The ball's field check and sim cost seconds and are `csg_mech`'s `a_ball_shoulder_printed_on_the_bed_turns`.)
#[test]
fn puppet_arm_joints_build_from_the_program() {
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(&mut m, &mut e, "puppet", PUPPET_ARM);
    assert_eq!(prog(&c).joints.len(), 3, "ball, hinge and anchor");
    let mech = ev.mech.first().expect("joints applied");
    assert_eq!(mech.plan.joints.len(), 2);
    assert_eq!(mech.finals.len(), 3);
    for (i, f) in mech.finals.iter().enumerate() {
        assert!(f.triangles.len() > mech.originals[i].triangles.len() / 2, "part {i} lost its mesh");
    }
}

#[test]
fn a_hinge_too_thin_names_its_line() {
    let (mut m, mut e) = setup();
    let head = m.head();
    let src = src("let a = m.box({size: [10mm 1mm 5mm] at: [5mm 0.5mm 0]})\nlet b = m.box({size: [10mm 1mm 5mm] at: [15mm 0.5mm 0]})\nlet j = m.hinge(a, b, {at: [10mm 0 0]})\n");
    let r = m.apply(&mut e, "thin", head, &[sc(Edit::Program { source: src })], None);
    let err = format!("{:?}", r.err());
    assert!(err.contains("too thin for a hinge"), "{err}");
}

/// A piece left floating inside a hollow (a cube inside a closed shell) is a floating island:
/// nothing below it but the cavity's floor, far beneath.
#[test]
fn a_piece_floating_in_a_hollow_is_found() {
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, "r1", "let hollow = m.shell(m.box({size: [30mm 30mm 30mm]}), 2mm, {open: @closed})\nlet inside = m.box({size: [6mm 6mm 6mm] at: [0 4mm 0]})\nlet both = m.union(hollow, inside)\nobject(both)\n");
    let islands: Vec<&fab_printability::Issue> = ev.printability.issues.iter().filter(|i| i.kind == "floating_island").collect();
    assert_eq!(islands.len(), 1, "{:?}", ev.printability.issues);
    assert!(islands[0].message.starts_with("floating island at z = ") && islands[0].measure > 5.0, "{}", islands[0].message);
    let (_, _, ev) = program(&mut m, &mut e, "r2", "let solid = m.box({size: [30mm 30mm 30mm]})\nobject(solid)\n");
    assert!(!ev.printability.issues.iter().any(|i| i.kind == "floating_island"));
}

#[test]
fn chainmail_links_print_free_on_the_sliced_beads() {
    use makepad_fab_settings::profiles::{JobSetup, Preset};
    let (mut m, mut e) = setup();
    // Sliced here from the plate's mesh (meshed as for an export).
    e.mesh_fields = true;
    let (_, _, ev) = program(&mut m, &mut e, "mail", "let mail = m.mail(Rect{size: [40mm 40mm]})\nobject(mail)\n");
    assert_eq!(ev.weaves.len(), 1);
    let w = &ev.weaves[0];
    let setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default()], filament_map: vec![1; ev.plate.parts.len()], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    let t = std::time::Instant::now();
    let sliced = makepad_fab_slice::plate::slice_plate(&setup, &ev.plate).unwrap();
    let slice_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = std::time::Instant::now();
    let c = w.check_printed(&sliced.layers(), 0.15);
    println!(
        "mail {} links sliced {} layers in {slice_ms:.0} ms; as printed ({:.0} ms): {} beads, {} stray, min gap {:?}, fused pairs {}, bridged pairs {}",
        w.links.loops.len(),
        sliced.layers().len(),
        t.elapsed().as_secs_f64() * 1e3,
        c.beads,
        c.stray,
        c.min_gap,
        c.fused.len(),
        c.bridged.len()
    );
    for f in c.fused.iter().take(5) {
        println!("  fused {f:?}");
    }
    assert!(c.beads > 1000 && c.stray * 10 < c.beads, "{c:?}");
    assert!(c.bridged.is_empty(), "{:?}", c.bridged.first());
}

/// The demo vase's script with its parameters' defaults replaced.
fn vase_script(height: f64, neck: f64, wall: f64, belly: f64, blend: f64) -> String {
    include_str!("../../app/resources/vase.cad.splash")
        .replacen("param(70mm", &format!("param({height}mm"), 1)
        .replacen("param(9.5mm", &format!("param({neck}mm"), 1)
        .replacen("param(1.6mm", &format!("param({wall}mm"), 1)
        .replacen("param(21mm", &format!("param({belly}mm"), 1)
        .replacen("param(9mm", &format!("param({blend}mm"), 1)
}

/// The vase is open at every parameter combination: on its axis the field is outside from the
/// rim's height down to below the neck's cap, while its floor and its neck's wall stand. A neck
/// no wider than its wall has no cavity; it still builds.
#[test]
fn the_vase_mouth_is_open_at_every_size() {
    use makepad_csg_math::Vec3d;
    // One row per regime: the lowest, the default and the tallest height; the rim's neighbourhood (88, 90);
    // thin and thick walls; wide and narrow necks; a neck no wider than its wall (no cavity).
    for &(h, neck, wall, belly, blend) in &[
        (30.0, 3.0, 0.8, 21.0, 9.0),
        (70.0, 9.5, 1.6, 21.0, 9.0),
        (88.0, 9.5, 0.8, 8.0, 0.5),
        (90.0, 30.0, 5.0, 60.0, 30.0),
        (120.0, 9.5, 5.0, 8.0, 30.0),
        (200.0, 30.0, 0.8, 21.0, 9.0),
        (70.0, 3.0, 3.0, 21.0, 9.0),
        (70.0, 3.0, 5.0, 8.0, 0.5),
    ] {
        let at = format!("height {h}, neck {neck}, wall {wall}, belly {belly}, blend {blend}");
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let m = FabModel::with_script("test", F, &vase_script(h, neck, wall, belly, blend));
        let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{at}: {f}"));
        let field = ev.fields[0].as_ref().expect("the vase is a field part").shape.field();
        let inside = |r: f64, y: f64| field.distance(Vec3d::new(r, y, 0.0)) < 0.0;
        assert!(inside(0.0, 3.0 + wall / 2.0), "the floor is gone: {at}");
        if neck <= wall {
            continue;
        }
        // The mouth takes the cap and the rim's fillet under it, nothing lower.
        let solid = FabModel::with_script("test", F, &vase_script(h, neck, wall, belly, blend).replacen("m.carve(rimmed, mouth, 0, {cell: 0.9mm})", "rimmed", 1));
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let (_, uncut) = solid.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{at}: {f}"));
        let uncut = uncut.fields[0].as_ref().expect("a field part").shape.field();
        let lowest = (h * 0.95 - wall).min(h * 0.94 - 3.3) - 0.05;
        for i in 0..((neck - wall) * 2.0) as usize {
            let r = i as f64 * 0.5;
            let mut y = 0.0;
            while y < lowest {
                assert_eq!(inside(r, y), uncut.distance(Vec3d::new(r, y, 0.0)) < 0.0, "the mouth cut at r = {r}, y = {y}: {at}");
                y += 0.25;
            }
        }
        let mut y = h * 0.95 - wall - 1.0;
        while y < h + 5.0 {
            assert!(!inside(0.0, y), "a plug at y = {y} on the axis: {at}");
            y += 0.05;
        }
    }
}

/// What the slicer and an export see of the mouth: at the lowest and the highest Height no contour
/// crosses the mouth in a section through the neck's cap, and the mesh crosses the axis only at the
/// floor. (The sweep above covers the sizes in between.)
#[test]
fn the_vase_mouth_is_open_in_its_sections_and_mesh() {
    use makepad_csg_math::Vec3d;
    let (neck, wall) = (9.5, 1.6);
    for &h in &[30.0, 200.0] {
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        e.mesh_fields = true;
        let m = FabModel::with_script("test", F, &vase_script(h, neck, wall, 21.0, 9.0));
        let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
        let field = ev.fields[0].as_ref().expect("the vase is a field part").shape.field();
        for y in [h * 0.95 - wall - 0.5, h * 0.95 - wall / 2.0, h * 0.95] {
            let f = field.clone();
            let segments = makepad_fab_slicer::sdf::contour_segments(&move |x, z| f.distance(Vec3d::new(x, y, z)), [-40.0, -40.0], [40.0, 40.0], &Default::default());
            assert!(!segments.is_empty(), "the section at y = {y} of a {h} mm vase is empty");
            let nearest = segments.iter().map(|(a, _)| (a.x as f64).hypot(a.y as f64) / makepad_fab_slicer::geom::SCALE).fold(f64::MAX, f64::min);
            assert!(nearest > neck - wall - 0.2, "a contour {nearest} mm from the axis at y = {y} of a {h} mm vase");
        }
        // The mesh's triangles over the part's centre: the floor's two faces only.
        let mesh = &ev.plate.parts[0].mesh;
        let b = mesh.bounds();
        let c = [(b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0];
        for t in &mesh.triangles {
            let p = t.map(|i| mesh.positions[i as usize]);
            let cross = |a: [f64; 3], b: [f64; 3]| (a[0] - c[0]) * (b[1] - c[1]) - (a[1] - c[1]) * (b[0] - c[0]);
            let d = [cross(p[0], p[1]), cross(p[1], p[2]), cross(p[2], p[0])];
            if d.iter().all(|v| *v >= 0.0) || d.iter().all(|v| *v <= 0.0) {
                let z = (p[0][2] + p[1][2] + p[2][2]) / 3.0;
                assert!(z < wall + 0.5, "the mesh crosses the axis {z} mm up a {h} mm vase");
            }
        }
    }
}

#[test]
fn an_exact_hole_drills_from_its_face() {
    use makepad_csg_graph::edges::{Face, Role};
    use std::f64::consts::PI;

    let (mut m, mut e) = setup();
    let block = |height| format!("m.brep.box({{size: [40mm {height}mm 30mm]}})");
    let drill = |body: &str, opts: &str| format!("let b = {body}\nlet h = m.brep.hole(b, {{diameter: 4mm {opts}}})\nobject(h)\n");
    let top = "on: b.top at: [5mm 0 0]";
    let blind = format!("{top} depth: 4mm");
    let shaft_area = PI * 2.0 * 2.0;
    let bore = "kind: @counterbore bore_diameter: 8mm bore_depth: 2mm depth: 6mm at: [[5mm, -5mm, 0] [-5mm, -5mm, 0]] dir: [0 2 0]";
    let sink = "kind: @countersink sink_diameter: 8mm angle: 60deg depth: 6mm on: b.front at: [5mm 0 0]";
    let taper = (4.0 - 2.0) / 30f64.to_radians().tan();
    let frustum = PI * taper * (4.0 * 4.0 + 4.0 * 2.0 + 2.0 * 2.0) / 3.0;
    for (case, height, opts, volume) in [
        ("through", 10, top, shaft_area * 10.0),
        ("blind", 10, blind.as_str(), shaft_area * 4.0),
        ("blind follows the taller box", 20, blind.as_str(), shaft_area * 4.0),
        ("two counterbores", 10, bore, 2.0 * (PI * 4.0 * 4.0 * 2.0 + shaft_area * 4.0)),
        ("countersink on the front", 10, sink, frustum + shaft_area * (6.0 - taper)),
    ] {
        program(&mut m, &mut e, case, &drill(&block(height), opts));
        let exact = e.object_exact("t/h").unwrap().unwrap_or_else(|| panic!("{case}: drilling must keep the part exact"));
        let removed = 40.0 * f64::from(height) * 30.0 - exact.measure.volume;
        assert!((removed - volume).abs() < volume * 1e-3, "{case}: expected to remove {volume} mm³, removed {removed} mm³");
        if case == "blind" {
            let owns = |f: &makepad_csg_graph::BrepEntity, face| {
                f.names.iter().any(|n| n.role == Role::Named(face) && exact.parts.get(&n.origin).map(String::as_str) == Some("h"))
            };
            assert!(exact.faces.iter().any(|f| f.normal.is_none() && owns(f, Face::Wall)), "the cylindrical wall must be named h.wall");
            assert!(exact.faces.iter().any(|f| owns(f, Face::Floor) && f.normal.is_some() && (f.at[0] - 5.0).abs() < 1e-3 && (f.at[1] - 1.0).abs() < 1e-3),
                "the blind floor at the drilled point must be named h.floor");
        }
    }

    for (case, body, opts, words) in [
        ("tapped", block(10), "on: b.top at: [5mm 0 0] kind: @tapped", "a tapped hole on an exact part is not made yet: drill the tap size as a simple hole, or use m.hole on a field part"),
        ("curved start", "m.brep.cylinder({radius: 8mm height: 10mm})".into(), "on: b.wall at: [8mm 0 0]", "a hole starts on a flat face"),
    ] {
        let edit = sc(Edit::Program { source: src(&drill(&body, opts)) });
        let error = m.apply(&mut e, case, m.head(), &[edit], None).expect_err(case).to_string();
        assert!(error.contains(words), "{case}: expected '{words}', got '{error}'");
    }
}

#[test]
fn a_segment_fillet_stays_on_the_same_geometric_edge() {
    const PART: &str = "let section = Path{from: [5mm 0]
    LineTo{to: [12mm 0]}
    base := LineTo{to: [40mm 2mm]}
    right := LineTo{to: [40mm 22mm]}
    LineTo{to: [0 22mm]}
    LineTo{to: [0 0]}
}
let body = m.brep.extrude({sketch: section height: 10mm})
let r = m.brep.fillet(body, 1mm, {edges: m.edge(body.top, body.side(section.right))})
object(r)
";
    let (mut m, mut e) = setup();
    let mut round = |label: &str, source: &str| {
        program(&mut m, &mut e, label, source);
        e.object_exact("t/r").unwrap().expect("the fillet keeps an exact body")
    };
    let named = round("named", PART);
    let numbered = round("numbered", &PART.replace("side(section.right)", "side(2)"));
    assert_eq!(named.measure.faces, numbered.measure.faces, "index and segment give the same filleted face count");
    assert!((named.measure.volume - numbered.measure.volume).abs() < 1e-6, "index and segment remove the same volume");
    let body_key = |b: &eval::ExactBody| *b.parts.iter().find(|(_, id)| *id == "body").expect("the fillet names its extrude").0;
    assert_eq!(named.sides[&body_key(&named)][2].as_deref(), Some("section.right"), "the extrude key carries the written segment reference");
    let centre = |b: &eval::ExactBody| {
        let mut curves = b.faces.iter().filter(|f| f.normal.is_none());
        let face = curves.next().expect("rounding the edge introduces a curved face");
        assert!(curves.next().is_none(), "only the selected straight edge was rounded");
        face.at
    };
    let shifted = PART.replace("    LineTo{to: [12mm 0]}", "    LineBy{by: [0 5mm]}\n    LineTo{to: [12mm 0]}");
    let still_right = round("insert before right", &shifted);
    let old_index = round("keep the old number", &shifted.replace("side(section.right)", "side(2)"));
    let distance = |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt();
    assert!(distance(centre(&named), centre(&still_right)) < 1e-5, "the rounded face stays at the same geometric centre");
    assert!(distance(centre(&still_right), centre(&old_index)) > 1.0, "the old number now rounds a different face");
    assert_eq!(still_right.sides[&body_key(&still_right)][3].as_deref(), Some("section.right"), "the written reference follows the shifted side index");
    let builder = "m.brep.extrude({sketch: section height: 10mm})";
    for (label, replacement, can_write) in [
        ("captured Path", format!("fn make() {{ {builder} }}\nlet body = make()"), true),
        ("local Path", format!("fn make(section) {{ {builder} }}\nlet body = make(section)"), false),
        ("field expression", format!("let drawing = {{outline: section}}\nlet body = {}", builder.replace("sketch: section", "sketch: drawing.outline")), false),
    ] {
        let b = round(label, &PART.replace(&format!("let body = {builder}"), &replacement));
        assert_eq!(b.sides.contains_key(&body_key(&b)), can_write, "{label} must give the writer only a reference in scope at the pick");
    }
}

#[test]
fn placed_copies_name_their_own_faces() {
    use makepad_csg_graph::edges::{Face, FaceName, Role};
    use makepad_csg_graph::eval::brep_steps;
    use makepad_csg_graph::BrepOp;
    use std::collections::BTreeSet;
    use std::f64::consts::PI;

    const LEGS: &str = "let leg1 = m.brep.box({size: [10mm 40mm 10mm] at: [-20mm 0 0]})
let leg2 = m.brep.box({size: [10mm 40mm 10mm] at: [20mm 0 0]})
let legs = m.brep.union(leg1, leg2)
object(legs)
";
    let (mut m, mut e) = setup();
    let exact = |e: &Evaluator, solid: &str| e.object_exact(solid).unwrap().unwrap_or_else(|| panic!("{solid} must be exact"));
    let box_keys = |c: &Compiled, leg: &str| {
        let p = prog(c);
        let steps = brep_steps(&p.graph, p.solid(leg).expect("the script names the leg").node);
        let [generator, place] = steps.as_slice() else { panic!("{leg} must have a box and a placement") };
        assert!(matches!(generator.op, BrepOp::Box { .. }) && matches!(place.op, BrepOp::Place { .. }), "{leg} must place a box");
        [generator.key, place.key]
    };
    let top = |body: &eval::ExactBody, leg: &str, place, x: f64| {
        let owns_top = |n: &FaceName| n.role == Role::Named(Face::Top)
            && body.parts.get(&n.origin).map(String::as_str) == Some(leg)
            && !body.repeated.contains(&n.origin);
        let faces: Vec<_> = body.faces.iter().filter(|f| f.names.iter().any(owns_top)).collect();
        assert_eq!(faces.len(), 1, "{leg}.top must name exactly one face");
        let face = faces[0];
        assert!((face.at[0] - x).abs() < 1e-3, "{leg}.top must be at x={x}, found {:?}", face.at);
        face.names.iter().find(|n| n.origin == place && owns_top(n)).cloned().unwrap_or_else(|| panic!("{leg}.top must carry its placement's name"))
    };

    let face_at = |body: &eval::ExactBody, name: &FaceName| {
        let mut faces = body.faces.iter().filter(|f| f.names.contains(name));
        let face = faces.next().unwrap_or_else(|| panic!("the moved face must retain {name:?}"));
        assert!(faces.next().is_none(), "the moved face's name must select just one face");
        face.at
    };
    let mut check_pair = |case: &str, script: &str, left_x| {
        let (_, c, ev) = program(&mut m, &mut e, case, script);
        let base = exact(&e, "t/legs");
        let [left_box, left_place] = box_keys(&c, "leg1");
        let right_keys = box_keys(&c, "leg2");
        assert_eq!(left_box, right_keys[0], "both identical boxes must use one generator key");
        let box_top = FaceName { origin: left_box, role: Role::Named(Face::Top) };
        assert_eq!(base.faces.iter().filter(|f| f.names.contains(&box_top)).count(), 2, "both placed tops must retain the shared box name");
        top(&base, "leg1", left_place, left_x);
        let leg2_top = top(&base, "leg2", right_keys[1], 20.0);
        let parts = params::model(&c, &ev);
        for axis in 0..3 {
            let key = format!("t/leg1.size.{axis}");
            assert!(parts.iter().flat_map(|p| &p.params).any(|q| q.key == key), "{case} must expose the box size parameter {key}");
        }

        // Ask with the edited recipe before evaluating it, so a rebuild cannot make the check pass.
        let resized = script.replacen("10mm 40mm", "12mm 40mm", 1);
        let (_, edited) = m.edited(&[sc(Edit::Program { source: src(&resized) })]).expect("resizing leg1 compiles");
        let edited_right = box_keys(&edited, "leg2");
        assert_eq!(edited_right, right_keys, "resizing leg1 must preserve leg2's generator and placement keys");
        assert!(makepad_csg_brep_model::engine().built(&edited_right), "leg2's generator and placement must already be built after the edit");

        let rounded = script.replace("object(legs)", "let round = m.brep.fillet(legs, 1mm, {edges: m.edge(leg2.top, leg2.front)})\nobject(round)");
        program(&mut m, &mut e, &format!("{case} fillet"), &rounded);
        let round = exact(&e, "t/round");
        let removed = base.measure.volume - round.measure.volume;
        let one_edge = (1.0 - PI / 4.0) * 10.0;
        assert!((removed - one_edge).abs() < one_edge * 1e-3, "the fillet must remove one edge's {one_edge} mm³, removed {removed} mm³");
        assert!(round.faces.iter().any(|f| f.normal.is_none() && (f.at[0] - 20.0).abs() < 1e-3), "the rounded face must be on leg2");
        (base, leg2_top, resized)
    };
    let (base, leg2_top, resized) = check_pair("two written placements", LEGS, -20.0);
    let mixed = LEGS.replace(" at: [-20mm 0 0]", "");
    let (mixed_base, mixed_top, _) = check_pair("unplaced leg1", &mixed, 0.0);
    let pulled = mixed.replace("object(legs)", "let up = m.brep.pull(legs, {face: leg2.top by: 5mm})\nobject(up)");
    program(&mut m, &mut e, "pull one leg", &pulled);
    let up = exact(&e, "t/up");
    let added = up.measure.volume - mixed_base.measure.volume;
    assert!((added - 500.0).abs() < 1e-3, "pulling one 10 × 10 top by 5 mm must add 500 mm³, added {added}");
    let at = face_at(&up, &mixed_top);
    assert!((at[0] - 20.0).abs() < 1e-3 && (at[1] - 25.0).abs() < 1e-3, "leg2.top must move to y=25 on leg2, found {at:?}");

    for (case, script) in [
        ("leg1 resized", resized),
        ("leg0 added", format!("let leg0 = m.brep.box({{size: [10mm 40mm 10mm] at: [-40mm 0 0]}})\n{}", LEGS.replace("union(leg1, leg2)", "union(leg0, [leg1, leg2])"))),
        ("union swapped", LEGS.replace("union(leg1, leg2)", "union(leg2, leg1)")),
        ("leg1 rounded", LEGS.replace("let legs = m.brep.union(leg1,", "let leg1r = m.brep.fillet(leg1, 1mm)\nlet legs = m.brep.union(leg1r,")),
    ] {
        let (_, c, _) = program(&mut m, &mut e, case, &script);
        let name = top(&exact(&e, "t/legs"), "leg2", box_keys(&c, "leg2")[1], 20.0);
        assert_eq!(name, leg2_top, "{case} must preserve the name of leg2.top");
    }
    let moved = LEGS.replace("at: [20mm", "at: [30mm");
    let (_, c, _) = program(&mut m, &mut e, "leg2 moved", &moved);
    top(&exact(&e, "t/legs"), "leg2", box_keys(&c, "leg2")[1], 30.0);

    let drilled = LEGS.replace("let legs = m.brep.union(leg1, leg2)",
        "let leg2h = m.brep.hole(leg2, {on: leg2.top at: [0 20mm 0] diameter: 2mm})\nlet legs = m.brep.union(leg1, leg2h)");
    program(&mut m, &mut e, "drill placed leg", &drilled);
    let body = exact(&e, "t/legs");
    let removed = base.measure.volume - body.measure.volume;
    let through = PI * 40.0;
    assert!((removed - through).abs() < through * 1e-3, "the through hole must remove {through} mm³, removed {removed} mm³");
    assert!(body.faces.iter().any(|f| (f.at[0] - 20.0).abs() < 1e-3 && f.names.iter().any(|n| n.role == Role::Named(Face::Wall))),
        "the through hole must have a named wall at x=20 on leg2");

    let block = |placement: &str| format!("let b = m.brep.box({{size: [10mm 40mm 10mm] {placement}}})\n");
    let pull_front = "let p = m.brep.pull(h, {face: b.front by: 1mm})\nobject(p)\n";
    for (case, placement, selector, x) in [
        ("unplaced drilled box", "", "on: b.top", 0.0),
        ("placed drilled box", "at: [20mm 0 0]", "on: b.top", 20.0),
        ("rotated drilled box", "at: [20mm 0 0] rotate: [0 0 90deg]", "", 20.0),
    ] {
        let script = format!("{}let h = m.brep.hole(b, {{{selector} at: [0 20mm 0] diameter: 2mm}})\n{pull_front}", block(placement));
        let (_, c, _) = program(&mut m, &mut e, case, &script);
        let drilled = exact(&e, "t/h");
        let pulled = exact(&e, "t/p");
        let removed = 4000.0 - drilled.measure.volume;
        let added = pulled.measure.volume - drilled.measure.volume;
        assert!((removed - through).abs() < through * 1e-3, "{case}: the through hole must remove 40π mm³, removed {removed}");
        assert!((added - 400.0).abs() < 0.01, "{case}: pulling the intact front by 1 mm must add 400 mm³, added {added}");
        let front = FaceName { origin: box_keys(&c, "b")[1], role: Role::Named(Face::Front) };
        let at = face_at(&pulled, &front);
        assert!((at[0] - x).abs() < 1e-3 && (at[2] - 6.0).abs() < 1e-3, "{case}: b.front must reach x={x}, z=6, found {at:?}");
        if selector.is_empty() {
            let p = prog(&c);
            let steps = brep_steps(&p.graph, p.solid("h").unwrap().node);
            let axis = steps.iter().find_map(|s| match &s.op { BrepOp::Hole { dir, .. } => *dir, _ => None }).expect("the rotated default drill axis");
            assert!((axis[0] - 1.0).abs() < 1e-12 && axis[1].abs() < 1e-12 && axis[2].abs() < 1e-12, "the box's inward -Y must drill along model +X, got {axis:?}");
        }
    }

    let chain = format!("{}let h = m.brep.hole(b, {{at: [-2mm 20mm 0] diameter: 2mm}})\nlet h2 = m.brep.hole(h, {{at: [2mm 20mm 0] dir: [0, -1, 0] diameter: 2mm}})\nobject(h2)\n", block("at: [20mm 0 0]"));
    program(&mut m, &mut e, "two holes in the same frame", &chain);
    let twice = exact(&e, "t/h2");
    let removed = 4000.0 - twice.measure.volume;
    assert!((removed - 2.0 * through).abs() < 2.0 * through * 1e-3, "both through holes must cut the placed box, removed {removed} mm³");
    for x in [18.0, 22.0] {
        assert!(twice.faces.iter().any(|f| (f.at[0] - x).abs() < 1e-3 && f.names.iter().any(|n| n.role == Role::Named(Face::Wall))),
            "the chain must have a drilled wall at model x={x}");
    }

    let scaled = format!("{}let h = m.brep.hole(b, {{on: b.top at: [0 20mm 0] diameter: 2mm}})\n", block("scale: 2"));
    program(&mut m, &mut e, "scale the drilled box", &format!("{scaled}object(h)\n"));
    let removed = 8.0 * 4000.0 - exact(&e, "t/h").measure.volume;
    assert!((removed - 8.0 * through).abs() < 8.0 * through * 1e-3, "the hole's volume must scale with the box, removed {removed} mm³");
    let edit = sc(Edit::Program { source: src(&format!("{scaled}{pull_front}")) });
    let error = m.edited(&[edit]).err().expect("a scaled hole must refuse the lost source face while lowering").to_string();
    assert!(error.contains("a hole on a scaled placement takes the body's own face names with it; select the face before the hole, or place without scale"), "{error}");
    let restored = format!("{scaled}let both = m.brep.union(h, b)\n{}", pull_front.replace("pull(h,", "pull(both,"));
    assert!(m.edited(&[sc(Edit::Program { source: src(&restored) })]).is_ok(), "a union that also contains the source placement must still accept b.front");

    let bosses = "let bosses = for x in [-9mm, -3mm, 3mm, 9mm] {
    m.brep.cylinder({radius: 1mm height: 2mm at: [x, 0, 0]})
}
let row = m.brep.union(bosses[0], [bosses[1], bosses[2], bosses[3]])
object(row)
";
    program(&mut m, &mut e, "four placed bosses", bosses);
    let row = exact(&e, "t/row");
    let mut origins = BTreeSet::new();
    for x in [-9.0, -3.0, 3.0, 9.0] {
        let walls: BTreeSet<_> = row.faces.iter().filter(|f| (f.at[0] - x).abs() <= 1.0)
            .flat_map(|f| &f.names).filter(|n| n.role == Role::Named(Face::Wall) && !row.repeated.contains(&n.origin))
            .map(|n| n.origin).collect();
        assert_eq!(walls.len(), 1, "the boss at x={x} must carry one placement-specific wall name");
        assert!(origins.insert(*walls.first().unwrap()), "the boss at x={x} must have its own wall name");
    }
}

/// An exact body through the whole path: Splash builders, graph, worker, plate; failures land at the line.
#[test]
fn brep_parts_build_on_the_plate() {
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet h = param(20mm, min: 5mm, max: 60mm)\nlet body = m.brep.box({size: [40mm h 30mm] at: [0, h / 2, 0]})\nlet round = m.brep.fillet(body, 2mm, {edges: {top: true}})\nlet bore = m.brep.cylinder({radius: 5mm height: h * 3 at: [0, h / 2, 0]})\nlet part = m.brep.subtract(round, bore)\nobject(part)\n";
    let m = FabModel::with_script("t", "t.cad.splash", source);
    let mut e = crate::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
    let (_, ev) = m.evaluate(&mut e, None).expect("an exact part builds");
    assert_eq!(ev.plate.parts.len(), 1);
    assert!(!ev.render[0].indices.is_empty());
    let bad = source.replace("radius: 5mm", "radius: 0mm");
    let m = FabModel::with_script("t", "t.cad.splash", &bad);
    let err = m.evaluate(&mut e, None).expect_err("a bad radius fails");
    assert!(err.id == "bore" && err.line == 7, "{err}");
    let mixed = source.replace("let bore = m.brep.cylinder", "let bore = m.cylinder");
    let m = FabModel::with_script("t", "t.cad.splash", &mixed);
    let err = m.compile().err().map(|e| format!("{e:?}"));
    assert!(err.is_some_and(|e| e.contains("must be an m.brep part")));
    // Split for printing at a tilted plane: both halves stay exact, the source's names carry
    // over, and the face on the cut is named on each (the lid's edge along it is rounded).
    let whole = {
        let m = FabModel::with_script("t", "t.cad.splash", source);
        m.evaluate(&mut e, None).unwrap().1.objects.iter().find(|o| o.name == "t/part").and_then(|o| o.exact).expect("the whole part is measured").volume
    };
    let split = source.replace("object(part)\n", "let halves = m.split(part, {center: [0, h / 2, 0] normal: [0.2 1 0]})\nlet lid = m.brep.fillet(halves[1], 1mm, {edges: m.edge(halves[1].cut, body.front)})\nobject(halves[0], name: \"base\")\nobject(lid, name: \"lid\")\n");
    let m = FabModel::with_script("t", "t.cad.splash", &split);
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    let exact = |n: &str| ev.objects.iter().find(|o| o.name == format!("t/{n}")).and_then(|o| o.exact).unwrap_or_else(|| panic!("{n} is an exact half"));
    let (base, lid) = (exact("halves[0]"), exact("lid"));
    // the pegs the base gains are a little thinner than the holes the lid loses
    assert!(base.volume > lid.volume && lid.volume > 0.3 * whole, "{base:?} {lid:?} of {whole}");
    assert!((base.volume + lid.volume) < whole && (base.volume + lid.volume) > 0.97 * whole, "{base:?} {lid:?} of {whole}");
    // A pin the cut has no room for: the halves come without, and the build says so once, at
    // the split's line, with the pin that fits.
    let wide = split.replace("normal: [0.2 1 0]}", "normal: [0.2 1 0] pin: 9mm}");
    let m = FabModel::with_script("t", "t.cad.splash", &wide);
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    let said: Vec<&String> = ev.warnings.iter().filter(|w| w.contains("split without pins")).collect();
    assert!(said.len() == 1 && said[0].starts_with("t.cad.splash:9:") && said[0].contains("no room for a 18 mm pin; a ") && said[0].contains("mm one fits (pin: "), "{:?}", ev.warnings);
}

/// The motor mount demo builds as exact bodies and its exact mass reads back.
#[test]
fn motor_mount_builds() {
    let source = include_str!("../../app/resources/motor_mount.cad.splash");
    let m = FabModel::with_script("motor_mount", "motor_mount.cad.splash", source);
    let mut e = crate::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(ev.plate.parts.len(), 1);
    let exact = ev.objects.iter().find_map(|o| o.exact).expect("the printed body is measured");
    assert!(exact.volume > 10_000.0 && exact.faces > 10, "{exact:?}");
    // A drag's rebuild: coarse, the steps before the change kept, off the UI thread in the app.
    e.draft = Some(("motor_mount".to_string(), 4.0));
    let change = Change::Param { instance: "motor_mount".into(), name: "boss_height".into(), value: Some(0.031) };
    let (_, _, dragged) = m.preview(&mut e, &[change], None).unwrap_or_else(|f| panic!("{f:?}"));
    assert!(dragged.render.iter().any(|r| !r.indices.is_empty()), "the drag shows geometry");
    e.draft = None;
}

/// A plate mixes the engines; a boolean across them goes through the mesh and says so.
#[test]
fn engines_mix_on_a_plate() {
    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet exact = m.brep.fillet(m.brep.box({size: [20mm 10mm 20mm] at: [0, 5mm, 0]}), 2mm, {edges: {top: true}})\nlet mesh = m.cylinder({radius: 3mm height: 30mm at: [0, 5mm, 0]})\nlet cut = m.subtract(exact, mesh)\nlet blob = m.sdf({field: fn(p) { return sdf_sphere(p, 8mm) } bounds: [[-9mm, -9mm, -9mm] [9mm, 9mm, 9mm]] cell: 0.5mm})\nobject(exact)\nobject(cut)\nobject(blob)\n";
    let m = FabModel::with_script("t", "t.cad.splash", source);
    let mut e = crate::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(ev.plate.parts.len(), 3);
    assert!(ev.warnings.iter().any(|w| w.contains("goes through the mesh") && w.contains("t.cad.splash:6")), "{:?}", ev.warnings);
    let by = |n: &str| ev.objects.iter().find(|o| o.name == format!("t/{n}")).unwrap();
    assert!(by("exact").exact.is_some());
    assert!(by("cut").exact.is_none() && by("blob").exact.is_none());
    // A mesh part splits through the mesh: two halves with dowel holes, and the dowels.
    let split = source.replace("object(cut)\n", "let halves = m.split(cut, {center: [0 5mm 0] dowels: true})\nobject(halves[0], name: \"low\")\nobject(halves[1], name: \"high\")\nobject(halves[2], name: \"dowels\")\n");
    let m = FabModel::with_script("t", "t.cad.splash", &split);
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    let size = |n: &str| ev.plate.parts.iter().find(|p| p.object == format!("t/{n}")).unwrap_or_else(|| panic!("{n} is on the plate")).bounds().size();
    let (low, high, dowels) = (size("halves[0]"), size("halves[1]"), size("halves[2]"));
    assert!((low[2] - 5.0).abs() < 0.1 && (high[2] - 5.0).abs() < 0.1, "each half is half the height: {low:?} {high:?}");
    assert!(dowels[2] > 2.0 && dowels[2] < 10.0 && dowels[0] < 20.0, "the dowels stand across the cut: {dowels:?}");
    // Pins it has no room for: through the mesh too, the build says so.
    let wide = split.replace("dowels: true", "pin: 9mm").replace("object(halves[2], name: \"dowels\")\n", "");
    let m = FabModel::with_script("t", "t.cad.splash", &wide);
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    let said: Vec<&String> = ev.warnings.iter().filter(|w| w.contains("split without pins: the cut has no room for a 18 mm pin")).collect();
    assert!(said.len() == 1, "{:?}", ev.warnings);
    // and the pin it says fits stands
    let fits = said[0].split("(pin: ").nth(1).and_then(|r| r.split("mm").next()).unwrap_or_else(|| panic!("a pin fits: {}", said[0]));
    let m = FabModel::with_script("t", "t.cad.splash", &wide.replace("pin: 9mm", &format!("pin: {fits}mm")));
    let (_, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
    assert!(!ev.warnings.iter().any(|w| w.contains("split without pins")), "pin: {fits}mm stands: {:?}", ev.warnings);
}

/// One script placing a bolt and a nut: a parameter they share shapes the nut's thread tool, and
/// that tool's ghost belongs to the nut's frame, not to the first part that lists the parameter.
#[test]
fn a_shared_parameters_tool_is_framed_by_the_part_it_cuts() {
    let (mut m, mut e) = setup();
    let (_, c, ev) = program(
        &mut m,
        &mut e,
        "boltnut",
        "let d = param(8mm, min: 3mm, max: 20mm)\nlet bolt = m.thread({diameter: d length: 20mm})\nlet blank = m.cylinder({radius: d height: 6mm segments: 6})\nlet thread_tool = m.thread({diameter: d length: 8mm at: [0, -1mm, 0]})\nlet nut = m.subtract(blank, thread_tool)\nobject(bolt)\nobject(nut)\n",
    );
    let model = params::model(&c, &ev);
    assert_eq!(model.len(), 2, "{model:?}");
    let p = model.iter().flat_map(|pp| pp.params.iter()).find(|p| p.key == "t/d").expect("the shared parameter");
    let tools = params::tool_objects(&c, p);
    assert_eq!(tools.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(), vec!["thread_tool"], "{tools:?}");
    assert!(params::is_helical(&c, "t", "thread_tool"), "a thread tool shows without its folds");
    assert!(!params::is_helical(&c, "t", "blank"));
    let frames: Vec<(String, [[f64; 4]; 3])> = ev.plate.parts.iter().map(|p| p.object.clone()).zip(ev.model_to_plate.iter().copied()).collect();
    let nut = frames.iter().position(|(o, _)| o == "t/nut").expect("the nut is on the plate");
    assert_ne!(frames[0].1, frames[nut].1, "the two parts stand apart on the plate");
    assert_eq!(params::tool_frames(&model, "t", "thread_tool", &frames), vec![&frames[nut].1]);
}

fn linked_bracket() -> (FabModel, Evaluator) {
    (FabModel::with_script("brackets", "bracket.cad.splash", include_str!("../../app/resources/bracket.cad.splash")), Evaluator::new(Machine::dual_nozzle_350()))
}

fn part_size(ev: &Evaluated, id: &str) -> [f64; 3] {
    ev.plate.parts.iter().find(|p| p.object == id).unwrap().bounds().size()
}

#[test]
fn linked_clones_share_parameter_edits_from_base_and_clone() {
    let (mut m, mut e) = linked_bracket();
    let (_, before) = m.evaluate(&mut e, None).unwrap();
    // No source placements yet: both clones get defaults, with independent centres.
    let (added, c, _) = apply(&mut m, &mut e, "links", vec![
        Change::Clone { instance: "bracket".into(), at: Some([160.0, 80.0]) },
        Change::Clone { instance: "bracket".into(), at: Some([260.0, 80.0]) },
    ]);
    assert!(added.added.contains(&"bracket~2/bracket".into()));
    assert_eq!(added.changed, ["bracket~1", "bracket~2"]);
    assert_eq!(c.instance("bracket~2").unwrap().script_instance, "bracket");
    assert_eq!(c.instance("bracket~2").unwrap().link, 2);
    assert!(Arc::ptr_eq(&c.instances[0].script, &c.instances[1].script));
    assert!(Arc::ptr_eq(&c.instances[0].script, &c.instances[2].script));
    let old = c.instances[0].script.clone();
    let (changed, c, ev) = apply(&mut m, &mut e, "base-width", vec![Change::Param { instance: "bracket".into(), name: "width".into(), value: Some(0.08) }]);
    assert_eq!(changed.changed, ["bracket", "bracket~1", "bracket~2"]);
    assert!(!Arc::ptr_eq(&old, &c.instances[0].script));
    for i in &c.instances {
        assert!(Arc::ptr_eq(&c.instances[0].script, &i.script));
        assert_eq!(i.script.param("width").unwrap().0, 0.08);
        assert!(part_size(&ev, &program::part_id(&i.name, "bracket"))[0] > part_size(&before, "bracket/bracket")[0] + 10.0);
    }
    let (_, c, ev) = apply(&mut m, &mut e, "clone-width", vec![Change::Param { instance: "bracket~2".into(), name: "width".into(), value: Some(0.09) }]);
    for i in &c.instances {
        assert!(Arc::ptr_eq(&c.instances[0].script, &i.script));
        assert_eq!(i.script.param("width").unwrap().0, 0.09);
        assert!((part_size(&ev, &program::part_id(&i.name, "bracket"))[0] - 90.0).abs() < 0.01);
    }
    assert_eq!(m.project().scripts.len(), 1);
    assert_eq!(m.project().scripts[0].params, [("width".to_string(), 0.09)]);
}

#[test]
fn script_copies_fork_source_keep_params_and_can_be_cloned() {
    let (mut m, mut e) = linked_bracket();
    apply(&mut m, &mut e, "set-width", vec![Change::Param { instance: "bracket".into(), name: "width".into(), value: Some(0.08) }]);
    let original = m.source("bracket.cad.splash").unwrap().to_string();
    let (added, c, before) = apply(&mut m, &mut e, "copy", vec![Change::Copy { instance: "bracket".into(), at: Some([250.0, 100.0]) }]);
    assert!(added.added.contains(&"bracket_2/bracket".into()));
    assert_eq!(added.changed, ["bracket_2"]);
    assert_eq!(m.project().scripts[0].params, m.project().scripts[1].params);
    assert_eq!(m.source("bracket_2.cad.splash"), Some(original.as_str()));
    assert!(!Arc::ptr_eq(&c.instances[0].script, &c.instances[1].script));
    let (_, c, after) = apply(&mut m, &mut e, "copy-code", vec![Change::Script { file: "bracket_2.cad.splash".into(), edit: Edit::Set { site: "depth".into(), value: "70mm".into() } }]);
    assert_eq!(m.source("bracket.cad.splash"), Some(original.as_str()));
    assert_ne!(m.source("bracket_2.cad.splash"), Some(original.as_str()));
    assert_eq!(part_size(&before, "bracket/bracket"), part_size(&after, "bracket/bracket"));
    assert!(part_size(&after, "bracket_2/bracket")[1] > part_size(&before, "bracket_2/bracket")[1] + 20.0);
    assert_eq!(c.instance("bracket_2").unwrap().script.param("width").unwrap().0, 0.08);
    let (_, c, _) = apply(&mut m, &mut e, "copy-link", vec![Change::Clone { instance: "bracket_2".into(), at: None }]);
    assert!(Arc::ptr_eq(&c.instance("bracket_2").unwrap().script, &c.instance("bracket_2~1").unwrap().script));
    // A second independent copy gets a fresh file and a fresh display name.
    apply(&mut m, &mut e, "copy-again", vec![Change::Copy { instance: "bracket".into(), at: None }]);
    assert!(m.source("bracket_3.cad.splash").is_some());
    assert_eq!(m.project().place("bracket_3", "bracket").unwrap().name.as_deref(), Some("bracket copy 3"));
}

#[test]
fn linked_placements_copy_every_override_and_keep_multi_object_offsets() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "objects", "let a = m.box({size: [10mm 8mm 6mm]})\nlet b = m.box({size: [6mm 8mm 10mm]})\nlet c = m.box({size: [4mm 4mm 4mm]})\nobject(a)\nobject(b)\nobject(c)\n");
    apply(&mut m, &mut e, "overrides", vec![
        pl("a", PlaceArg::At(Some([60.0, 70.0]))), pl("b", PlaceArg::At(Some([90.0, 110.0]))),
        pl("a", PlaceArg::Name(Some("Handle".into()))), pl("a", PlaceArg::Rotate([10.0, 20.0, 30.0])),
        pl("a", PlaceArg::Scale([1.25; 3])), pl("a", PlaceArg::Drop(false)), 
        pl("a", PlaceArg::Color(Some("Orange".into()))), pl("a", PlaceArg::Paint(vec![(0xff0000ff, "Red".into())])),
        pl("a", PlaceArg::Print(false)),
        pl("a", PlaceArg::Show(false)), pl("a", PlaceArg::Settings(vec![("wall_loops".into(), "4".into())])),
    ]);
    let original = m.project().place("t", "a").unwrap().clone();
    apply(&mut m, &mut e, "clone", vec![Change::Clone { instance: "t".into(), at: Some([160.0, 170.0]) }]);
    let mut expected = original;
    expected.link = 1;
    expected.at = Some([160.0, 170.0]);
    expected.name = Some("Handle 2".into());
    assert_eq!(m.project().place_link("t", "a", 1), Some(&expected));
    assert_eq!(m.project().place_link("t", "b", 1).unwrap().at, Some([190.0, 210.0]));
    assert_eq!(m.project().place_link("t", "c", 1).unwrap().at, None);
    apply(&mut m, &mut e, "free-clone", vec![Change::Clone { instance: "t~1".into(), at: None }]);
    assert!(m.project().places.iter().filter(|p| p.link == 2).all(|p| p.at.is_none()));
    // Copy a clone, including hidden objects and overrides, into a new base instance.
    apply(&mut m, &mut e, "copy-clone", vec![Change::Copy { instance: "t~1".into(), at: Some([80.0, 90.0]) }]);
    expected.instance = "t_2".into();
    expected.link = 0;
    expected.at = Some([80.0, 90.0]);
    expected.name = Some("Handle 2 copy".into());
    assert_eq!(m.project().place("t_2", "a"), Some(&expected));
    assert_eq!(m.project().place("t_2", "b").unwrap().at, Some([110.0, 130.0]));
    assert_eq!(m.project().place("t_2", "c").unwrap().at, None);
    // A link named by only one placement supplies defaults for all the other objects.
    let (state, c) = m.edited(&[Change::Place { instance: "t~7".into(), object: "b".into(), arg: PlaceArg::Color(Some("Blue".into())) }]).unwrap();
    let link = c.instance("t~7").unwrap();
    assert_eq!(link.placed.len(), 3);
    let default = link.placed.iter().find(|p| p.object == "a").unwrap();
    assert_eq!(default.at, None);
    assert_eq!(default.scale, [1.0; 3]);
    assert_eq!(default.color, None);
    assert_eq!(state.project.place("t", "a").unwrap().scale, [1.25; 3]);
    let m = FabModel::open("sparse", state);
    let (_, c) = m.edited(&[Change::Clone { instance: "t~7".into(), at: None }]).unwrap();
    assert!(c.instance("t~8").is_some());
}

#[test]
fn linked_copy_measure_project_round_trip_preserves_links_and_overrides() {
    let mut m = FabModel::with_script("vases", "vase.cad.splash", include_str!("../../app/resources/vase.cad.splash"));
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    apply(&mut m, &mut e, "plate", vec![
        Change::Color { name: "Blue".into(), set: Some(crate::project::ColorDef { material: Some("TPU".into()), ..crate::project::ColorDef::new("Blue") }) },
        Change::Clone { instance: "vase".into(), at: Some([240.0, 100.0]) },
        Change::Place { instance: "vase~1".into(), object: "vase".into(), arg: PlaceArg::Color(Some("Blue".into())) },
        Change::Place { instance: "vase~1".into(), object: "vase".into(), arg: PlaceArg::Show(false) },
        Change::Place { instance: "vase~1".into(), object: "vase".into(), arg: PlaceArg::Settings(vec![("wall_loops".into(), "4".into())]) },
        Change::Copy { instance: "vase~1".into(), at: None },
        Change::MeasureAdd { measure: project::Measure { name: Some("Gap A".into()), kind: "gap".into(), points: vec![[10.0, -20.0, 0.0], [30.0, 40.0, 5.0]] } },
    ]);
    let text = m.project_text();
    assert!(text.contains("link: 1"), "{text}");
    assert!(text.contains("color(\"Blue\", material: \"TPU\")"), "a colour's material type is kept: {text}");
    let parsed = project::parse(&text).unwrap();
    assert_eq!(&parsed, m.project());
    assert_eq!(crate::eval::print_colors(&parsed)[0].material.as_deref(), Some("TPU"));
    let loaded = FabModel::load("vases", &text, m.state().scripts.clone()).unwrap();
    assert_eq!(loaded.state(), m.state());
    let names = |c: Compiled| c.instances.into_iter().map(|i| i.name).collect::<Vec<_>>();
    assert_eq!(names(m.compile().unwrap()), names(loaded.compile().unwrap()));
    let link = parsed.place_link("vase", "vase", 1).unwrap();
    assert_eq!(link.color.as_deref(), Some("Blue"));
    assert!(!link.show);
    assert_eq!(link.settings, [("wall_loops".to_string(), "4".to_string())]);
    assert!(parsed.place("vase", "vase").is_none());
    // Text edits address a particular link, and duplicate validation includes the link.
    let changes = project::changes("place(\"vase\", object: \"vase\", link: 1, show: true)\nremove(\"vase~1\")").unwrap();
    assert!(matches!(&changes[0], Change::Place { instance, arg: PlaceArg::Show(true), .. } if instance == "vase~1"));
    assert!(matches!(&changes[1], Change::RemoveInstance { instance } if instance == "vase~1"));
    let distinct = "use mod.fab.*\nscript(\"vase.cad.splash\")\nplace(\"vase\")\nplace(\"vase\", link: 1)\n";
    assert!(project::parse(distinct).is_ok());
    assert!(project::parse(&format!("{distinct}place(\"vase\", link: 1)\n")).is_err());
    for value in ["0", "-1", "1.5", "4294967296"] {
        assert!(project::parse(&format!("use mod.fab.*\nscript(\"vase.cad.splash\")\nplace(\"vase\", link: {value})\n")).is_err());
    }
}

#[test]
fn removing_base_promotes_lowest_clone_and_removing_script_removes_all_links() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "bracket", BRACKET);
    apply(&mut m, &mut e, "links", vec![
        Change::Clone { instance: "t".into(), at: Some([150.0, 80.0]) },
        Change::Clone { instance: "t".into(), at: Some([250.0, 80.0]) },
    ]);
    let mut promoted = m.project().place_link("t", "bracket", 1).unwrap().clone();
    promoted.link = 0;
    let (_, c, _) = apply(&mut m, &mut e, "remove-base", vec![Change::RemoveInstance { instance: "t".into() }]);
    assert_eq!(m.project().place("t", "bracket"), Some(&promoted));
    assert!(c.instance("t~1").is_none() && c.instance("t~2").is_some());
    assert!(m.source(F).is_some());
    apply(&mut m, &mut e, "remove-clone", vec![Change::RemoveInstance { instance: "t~2".into() }]);
    assert_eq!(m.project().place("t", "bracket"), Some(&promoted));
    apply(&mut m, &mut e, "another-link-and-ref", vec![
        Change::Clone { instance: "t".into(), at: None },
        Change::AddInstance { file: F.into(), source: None, name: Some("separate".into()), at: None },
    ]);
    let (_, c, _) = apply(&mut m, &mut e, "remove-script", vec![Change::RemoveScript { instance: "t~1".into() }]);
    assert_eq!(c.instances.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["separate"]);
    assert!(m.project().places.is_empty());
    assert!(m.source(F).is_some(), "another ScriptRef still uses the source");
    apply(&mut m, &mut e, "remove-last", vec![Change::RemoveScript { instance: "separate".into() }]);
    assert!(m.state().scripts.is_empty() && m.project().scripts.is_empty());
    m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    apply(&mut m, &mut e, "remove-last-instance", vec![Change::RemoveInstance { instance: "separate".into() }]);
    assert!(m.state().scripts.is_empty() && m.project().scripts.is_empty());
}

#[test]
fn saved_measures_add_set_remove_round_trip_and_undo_without_geometry_changes() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "bracket", BRACKET);
    let (_, before) = m.evaluate(&mut e, None).unwrap();
    let changes = project::changes("measure(points: [[1cm 20mm 0mm] [30mm 40mm 5mm]], name: \"Gap A\")").unwrap();
    let initial = m.state().clone();
    let (_, _, measured) = apply(&mut m, &mut e, "add-ruler", changes);
    assert_eq!(measured.content, before.content);
    assert_eq!(part_size(&measured, "t/bracket"), part_size(&before, "t/bracket"));
    assert_eq!(m.project().measures[0], project::Measure { name: Some("Gap A".into()), kind: "distance".into(), points: vec![[10.0, 20.0, 0.0], [30.0, 40.0, 5.0]] });
    let added = m.state().clone();
    apply(&mut m, &mut e, "edit-ruler", vec![Change::MeasureSet { index: 0, points: Some(vec![[0.0; 3], [40.0, 0.0, 0.0]]), name: Some(None) }]);
    assert_eq!(m.project().measures[0].name, None);
    assert_eq!(project::parse(&m.project_text()).unwrap(), *m.project());
    let edited = m.state().clone();
    apply(&mut m, &mut e, "remove-ruler", vec![Change::MeasureRemove { index: 0 }]);
    assert_eq!(m.state(), &initial);
    for expected in [&edited, &added, &initial] {
        m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
        assert_eq!(m.state(), expected);
    }
    let head = m.head();
    assert!(m.apply(&mut e, "bad-index", head, &[Change::MeasureRemove { index: 0 }], None).is_err());
    assert!(m.apply(&mut e, "bad-point", head, &[Change::MeasureAdd { measure: project::Measure { name: None, kind: "distance".into(), points: vec![[f64::NAN, 0.0, 0.0]] } }], None).is_err());
    assert_eq!(m.head(), head);
}

#[test]
fn an_insert_rides_on_its_host_and_never_prints() {
    let (mut m, mut e) = setup();
    let body = "let Host = m.subtract(m.box({size: [20mm 10mm 20mm]}), m.link(m.box({size: [6mm 6mm 6mm] at: [0 2mm 0]}), \"host:pocket\"))\nobject(Host)\ninsert(m.link(m.box({size: [5mm 5mm 5mm] at: [0 7mm 0] frame: Frame{name: \"seat\" at: [0 1mm 0]}}), \"host:nut\"), name: \"Nut\", into: \"Host\")\n";
    let (_, c, _) = program(&mut m, &mut e, "insert", body);
    let script = &c.instances[0].script;
    assert_eq!(script.object("Host").unwrap().link, None, "an operand's link does not label its boolean");
    assert!(script.program.solids.iter().any(|s| s.link.as_deref() == Some("host:pocket")), "a boolean operand retains its own link");
    let (_, _, at) = apply(&mut m, &mut e, "host-place", vec![
        pl("Host", PlaceArg::At(Some([100.0, 100.0]))),
        pl("Nut", PlaceArg::At(Some([20.0, 30.0]))),
        pl("Nut", PlaceArg::Rotate([90.0, 0.0, 0.0])),
        pl("Nut", PlaceArg::Drop(false)),
    ]);
    assert_eq!(at.plate.parts.len(), 1, "only the host prints");
    assert_eq!(at.plate.excluded.len(), 1, "the insert is excluded");
    let host = &at.plate.parts[0];
    let nut = &at.plate.excluded[0];
    assert_eq!(nut.link.as_deref(), Some("host:nut"), "the emitted link reaches the excluded part");
    let seat = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 8.0], [0.0, 0.0, 1.0, 0.0]];
    let near = |f: &[[f64; 4]; 3]| f.iter().flatten().zip(seat.iter().flatten()).all(|(x, y)| (x - y).abs() < 1e-9);
    assert!(nut.frame.as_ref().is_some_and(|(name, f)| name == "seat" && near(f)), "its named frame reaches the excluded part in the model frame, moved with the part: {:?}", nut.frame);
    assert_eq!(nut.insert.as_ref().unwrap().into, host.object, "into names the host's stable plate object, including its instance");
    assert_eq!(nut.insert.as_ref().unwrap().clearance, 0.2, "default clearance is millimetres");
    assert_eq!(nut.model_to_plate, at.model_to_plate[0], "an insert ignores its own placement and uses its host's exact frame");
    assert_eq!(at.plate.inserts().iter().map(|(p, _, host)| (p.name.as_str(), *host)).collect::<Vec<_>>(), [("Nut", 0)], "inserts resolve the printed host index");
    assert_eq!(at.plate.printable().parts.len(), 1, "only the host reaches the slicer");
    assert!(at.plate.printable().excluded.is_empty(), "the slicer's plate carries no insert mesh");
    let (_, _, turned) = apply(&mut m, &mut e, "host-turn", vec![pl("Host", PlaceArg::Rotate([0.0, 0.0, 90.0]))]);
    let nut = &turned.plate.excluded[0];
    assert_eq!(nut.model_to_plate, turned.model_to_plate[0], "the host's rotation also turns the insert's frame");
    assert_eq!(nut.frame, at.plate.excluded[0].frame, "its named frame is in the model frame: the host's turn reaches it through model_to_plate alone");
    for (a, b) in at.plate.excluded[0].part.mesh.positions.iter().zip(&nut.part.mesh.positions) {
        assert!((b[0] - (200.0 - a[1])).abs() < 1e-6 && (b[1] - a[0]).abs() < 1e-6 && (b[2] - a[2]).abs() < 1e-6, "each insert vertex turns about the host's placed origin");
    }
    let (_, _, off) = apply(&mut m, &mut e, "host-off", vec![pl("Host", PlaceArg::Print(false))]);
    assert!(off.plate.parts.is_empty() && off.plate.inserts().is_empty(), "a non-printed host disables insert checks");
    let nut = off.plate.excluded.iter().find(|p| p.part.name == "Nut").unwrap();
    assert!(nut.insert.is_none(), "without a printed host it is an ordinary excluded part");
    assert_eq!(nut.model_to_plate, turned.model_to_plate[0], "turning off print preserves the host frame");
    let (_, _, hidden) = apply(&mut m, &mut e, "host-hidden", vec![pl("Host", PlaceArg::Show(false))]);
    assert_eq!(hidden.plate.excluded.len(), 1, "the hidden host is used for layout only");
    assert!(hidden.plate.excluded[0].insert.is_none(), "a hidden host disables insert checks");
    assert_eq!(hidden.plate.excluded[0].model_to_plate, turned.model_to_plate[0], "the visible insert still follows its hidden host");
    for (source, message) in [
        (body.replace("into: \"Host\"", "into: \"Nope\""), "insert: 'into' names no object of this script (there is: Host)"),
        (body.replace(", into: \"Host\"", ""), "insert: it needs into: \"<object>\""),
    ] {
        let bad = FabModel::with_script("test", F, &src(&source));
        let err = bad.compile().unwrap_err();
        assert!(err.message.contains(message), "located insert error names the available host: {}", err.message);
        assert!(err.line > 1, "the error is located at the insert call");
    }
}

#[test]
fn a_solid_states_how_light_passes_through_it() {
    use fab_plate::{Optics, OpticsSource};

    let (mut m, mut e) = setup();
    let body = "let plain = m.box({size: [4mm 4mm 4mm]})\nlet lit = m.optics(m.box({size: [2mm 4mm 2mm] at: [2mm 0 0]}), {throughput: 0.2 source: @estimate})\nobject([plain, lit], name: \"Lid\")\n";
    let expected = Some(Optics { throughput: 0.2, source: OpticsSource::Estimate });
    let (_, _, lid) = program(&mut m, &mut e, "optics", body);
    assert_eq!(lid.plate.parts.len(), 2, "the object's two bodies have their own parts");
    let plain = lid.plate.parts.iter().find(|p| p.object == "t/plain").unwrap();
    let lit = lid.plate.parts.iter().find(|p| p.object == "t/lit").unwrap();
    assert_eq!(plain.optics, None, "the plain body states no optics");
    assert_eq!(lit.optics, expected, "the second body's optics reach its own plate part");
    assert_eq!(plain.body_of, lit.body_of, "both bodies belong to the same object");

    let union = body.replace("object([plain, lit], name: \"Lid\")", "object(m.union(plain, lit), name: \"Lid\")");
    let (_, compiled, joined) = program(&mut m, &mut e, "union-optics", &union);
    assert_eq!(joined.plate.parts.len(), 1, "only the union is emitted");
    assert_eq!(joined.plate.parts[0].optics, None, "a boolean does not inherit an operand's optics");
    assert_eq!(prog(&compiled).solids.iter().find(|s| s.id == "lit").unwrap().optics,
        Some(lang::Optics { throughput: 0.2, source: lang::OpticsSource::Estimate }),
        "the operand retains its own optics");

    let inserts = body.replace("object([plain, lit], name: \"Lid\")", "object(plain, name: \"Lid\")\ninsert(lit, name: \"Pipe\", into: \"Lid\")\ninsert(m.box({size: [1mm 1mm 1mm] optics: {throughput: 1 source: @measured}}), name: \"Window\", into: \"Lid\")\ninsert(m.box({size: [1mm 1mm 1mm] optics: {throughput: 0}}), name: \"Dark\", into: \"Lid\")");
    let (_, _, inserted) = program(&mut m, &mut e, "insert-optics", &inserts);
    assert_eq!(inserted.plate.parts.len(), 1, "only the host prints");
    assert_eq!(inserted.plate.parts[0].optics, None, "inserts do not tag their host");
    assert_eq!(inserted.plate.excluded.len(), 3, "each insert has its own excluded part");
    let insert = |name: &str| &inserted.plate.excluded.iter().find(|p| p.part.name == name).unwrap().part;
    assert_eq!(insert("Pipe").optics, expected, "the value call's optics survive on an insert");
    assert_eq!(insert("Window").optics, Some(Optics { throughput: 1.0, source: OpticsSource::Measured }), "a solid field accepts measured throughput at the upper bound");
    assert_eq!(insert("Dark").optics, Some(Optics { throughput: 0.0, source: OpticsSource::Estimate }), "an omitted source defaults to estimate at the lower bound");

    for (record, field) in [
        ("{throughput: 1.5}", "optics.throughput"),
        ("{throughput: -0.1}", "optics.throughput"),
        ("{throughput: \"bright\"}", "optics.throughput"),
        ("{}", "optics.throughput"),
        ("{throughput: 0.2 source: 1}", "optics.source"),
        ("{throughput: 0.2 source: @unknown}", "optics.source"),
        ("0.2", "optics"),
    ] {
        for solid in [
            format!("m.optics(m.box({{size: [1mm 1mm 1mm]}}), {record})"),
            format!("m.box({{size: [1mm 1mm 1mm] optics: {record}}})"),
        ] {
            let bad = FabModel::with_script("test", F, &src(&format!("object({solid}, name: \"Bad\")\n")));
            let err = bad.compile().unwrap_err();
            assert!(err.message.contains(field), "{solid} must diagnose {field}: {}", err.message);
            assert!(err.line > 1, "the optics diagnostic is located in the script");
        }
    }
}

#[test]
fn a_solid_keeps_board_space_clear_with_keepouts() {
    use fab_plate::{Keepout, KeepoutSide};

    let (mut m, mut e) = setup();
    let record = r#"{board: "ledavr" name: "Pipe block" area: [[0mm, 0mm], [4mm, 0mm], [4mm, 2mm], [0mm, 2mm]] z: [1mm, 5mm]}"#;
    let bottom = record.replace("name:", r#"side: @bottom feature: "port:key" room: true name:"#);
    let carried = record.replace("name:", r#"feature: "other:key" room: false name:"#);
    let records = format!("{record}, {bottom}, {carried}");
    let body = format!("let plain = m.box({{size: [4mm 4mm 4mm]}})\nlet held = m.keepouts(m.box({{size: [2mm 4mm 2mm] at: [2mm 0 0]}}), [{records}])\nobject([plain, held], name: \"Lid\")\n");
    let top = Keepout {
        board: "ledavr".into(),
        name: "Pipe block".into(),
        feature: None,
        room: false,
        area: vec![[0.0, 0.0], [4.0, 0.0], [4.0, 2.0], [0.0, 2.0]],
        side: KeepoutSide::Top,
        z: [1.0, 5.0],
    };
    let expected = [
        top.clone(),
        Keepout { side: KeepoutSide::Bottom, feature: Some("port:key".into()), room: true, ..top.clone() },
        Keepout { feature: Some("other:key".into()), ..top },
    ];
    let (_, _, lid) = program(&mut m, &mut e, "keepouts", &body);
    assert_eq!(lid.plate.parts.len(), 2, "the object's two bodies have their own parts");
    let plain = lid.plate.parts.iter().find(|p| p.object == "t/plain").unwrap();
    let held = lid.plate.parts.iter().find(|p| p.object == "t/held").unwrap();
    assert!(plain.keepouts.is_empty(), "the plain body keeps no board space clear");
    assert_eq!(held.keepouts, expected, "the second body's keepout reaches its own part in kernel units and board coordinates");
    assert_eq!(plain.body_of, held.body_of, "both bodies belong to the same object");

    let union = body.replace("object([plain, held], name: \"Lid\")", "object(m.union(plain, held), name: \"Lid\")");
    let (_, _, joined) = program(&mut m, &mut e, "union-keepouts", &union);
    assert_eq!(joined.plate.parts.len(), 1, "only the union is emitted");
    assert!(joined.plate.parts[0].keepouts.is_empty(), "a boolean does not inherit an operand's keepouts");

    let inserts = body.replace("object([plain, held], name: \"Lid\")", &format!("object(plain, name: \"Lid\")\ninsert(held, name: \"Pipe\", into: \"Lid\")\ninsert(m.box({{size: [1mm 1mm 1mm] keepouts: [{records}]}}), name: \"Field\", into: \"Lid\")"));
    let (_, _, inserted) = program(&mut m, &mut e, "insert-keepouts", &inserts);
    assert_eq!(inserted.plate.parts.len(), 1, "only the host prints");
    assert!(inserted.plate.parts[0].keepouts.is_empty(), "inserts do not tag their host");
    assert_eq!(inserted.plate.excluded.len(), 2, "each insert has its own excluded part");
    for name in ["Pipe", "Field"] {
        let part = &inserted.plate.excluded.iter().find(|p| p.part.name == name).unwrap().part;
        assert_eq!(part.keepouts, expected, "{name} keeps its records from the value call or solid field");
    }

    for (record, field) in [
        (record.replace("[[0mm, 0mm], [4mm, 0mm], [4mm, 2mm], [0mm, 2mm]]", "[[0mm, 0mm], [4mm, 0mm]]"), "keepouts.area"),
        (record.replace("[1mm, 5mm]", "[3mm, 1mm]"), "keepouts.z"),
        (bottom.replace("@bottom", "@left"), "keepouts.side"),
        (bottom.replace("@bottom", "\"bottom\""), "keepouts.side"),
        (bottom.replace("\"port:key\"", "\"\""), "keepouts.feature"),
        (bottom.replace("\"port:key\"", "@port"), "keepouts.feature"),
        (bottom.replace("room: true", "room: 1"), "keepouts.room"),
        (bottom.replace("room: true", "room: \"true\""), "keepouts.room"),
    ] {
        for solid in [
            format!("m.keepouts(m.box({{size: [1mm 1mm 1mm]}}), [{record}])"),
            format!("m.box({{size: [1mm 1mm 1mm] keepouts: [{record}]}})"),
        ] {
            let bad = FabModel::with_script("test", F, &src(&format!("object({solid}, name: \"Bad\")\n")));
            let err = bad.compile().unwrap_err();
            assert!(err.message.contains(field), "{solid} must diagnose {field}: {}", err.message);
            assert!(err.line > 1, "the keepouts diagnostic is located in the script");
        }
    }
}

#[test]
fn an_embedded_insert_reaches_the_slicer_with_its_host() {
    let (mut m, mut e) = setup();
    let body = "let Host = m.box({size: [20mm 10mm 20mm]})\nobject(Host)\ninsert(m.box({size: [5mm 3mm 5mm] at: [0 4mm 0]}), name: \"Nut\", into: \"Host\", clearance: 0.3mm, embed: true)\ninsert(m.box({size: [2mm 2mm 2mm] at: [6mm 4mm 0]}), name: \"Pin\", into: \"Host\")\n";
    let (_, _, ev) = program(&mut m, &mut e, "embed", body);
    let embedded = |ev: &fab_plate::Plate| ev.excluded.iter().map(|p| (p.part.name.clone(), p.insert.as_ref().map(|i| (i.embed, i.clearance)))).collect::<Vec<_>>();
    assert_eq!(embedded(&ev.plate), [("Nut".to_string(), Some((true, 0.3))), ("Pin".to_string(), Some((false, 0.2)))], "embed: true marks the insert, its default is false");
    let printable = ev.plate.printable();
    assert_eq!(embedded(&printable), [("Nut".to_string(), Some((true, 0.3)))], "only the embedded insert reaches the slicer");
    let nut = &printable.excluded[0];
    assert!(nut.render.indices.is_empty() && !nut.part.mesh.triangles.is_empty(), "it carries its mesh to the slicer, not its render");
    assert_eq!(nut.part.mesh, ev.plate.excluded[0].part.mesh, "in the plate frame its host prints in");
    let hosts: Vec<(usize, Vec<&str>)> = printable.embeds().iter().map(|(h, items)| (*h, items.iter().map(|(p, _)| p.name.as_str()).collect())).collect();
    assert_eq!(hosts, [(0, vec!["Nut"])], "embeds() groups the embedded items by printed host");
    let (_, _, off) = apply(&mut m, &mut e, "host-off", vec![pl("Host", PlaceArg::Print(false))]);
    assert!(off.plate.printable().excluded.is_empty(), "an item whose host does not print is not cut into anything");
    let bad = FabModel::with_script("test", F, &src(&body.replace("embed: true", "embed: 1")));
    assert!(bad.compile().unwrap_err().message.contains("insert: 'embed' must be true or false"));
}

#[test]
fn print_eye_keeps_placement_round_trips_and_clones_toggle_independently() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "body", "let body = m.box({size: [20mm 12mm 8mm]})\nobject(body)\n");
    let (_, _, before) = apply(&mut m, &mut e, "clone", vec![
        pl("body", PlaceArg::Settings(vec![("wall_loops".into(), "4".into())])),
        Change::Color { name: "Red".into(), set: Some(crate::project::ColorDef::new("Red")) },
        pl("body", PlaceArg::Paint(vec![(0xff0000ff, "Red".into())])),
        Change::Clone { instance: "t".into(), at: Some([250.0, 100.0]) },
    ]);
    let placement = m.project().place("t", "body").unwrap().clone();
    let (_, _, off) = apply(&mut m, &mut e, "off", vec![pl("body", PlaceArg::Print(false))]);
    assert_eq!(off.plate.parts.iter().map(|p| p.object.as_str()).collect::<Vec<_>>(), ["t~1/body"]);
    assert_eq!(off.plate.excluded.len(), 1);
    let excluded = &off.plate.excluded[0];
    assert_eq!(excluded.part.object, "t/body");
    assert_eq!(excluded.part.bounds(), before.plate.parts[0].bounds());
    assert_eq!(off.plate.parts[0].bounds(), before.plate.parts[1].bounds());
    assert!(!excluded.render.indices.is_empty());
    assert_eq!(excluded.handles.place.as_deref(), Some("t/body"));
    assert_eq!((off.render.len(), off.handles.len(), off.frames.len(), off.fields.len(), off.paint.len(), off.reports.len()), (1, 1, 1, 1, 1, 1));
    let mut expected = placement.clone();
    expected.print = false;
    assert_eq!(m.project().place("t", "body"), Some(&expected));
    assert!(m.project().place_link("t", "body", 1).unwrap().print);
    let text = m.project_text();
    assert!(text.contains("print: false"));
    let loaded = FabModel::load("eyes", &text, m.state().scripts.clone()).unwrap();
    assert_eq!(loaded.project(), m.project());
    let (_, reloaded) = loaded.evaluate(&mut e, None).unwrap();
    assert_eq!(reloaded.plate.excluded[0].part.bounds(), excluded.part.bounds());
    let (_, _, on) = apply(&mut m, &mut e, "on", vec![pl("body", PlaceArg::Print(true))]);
    assert!(on.plate.excluded.is_empty());
    assert_eq!(on.plate.parts.len(), 2);
    assert_eq!(m.project().place("t", "body"), Some(&placement));
    let (_, _, undone) = m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    assert_eq!(undone.plate.excluded.len(), 1);
}

/// A board's stand-in (an object linked `board:<name>`) shows and never prints, whatever its place
/// says. `hidden: true` keeps a placement on the plate and out of the views: it reaches the part's
/// handles, round-trips through the project text, and is one undoable edit that leaves the printed
/// part as it was.
#[test]
fn a_board_never_prints_and_hidden_keeps_it_on_the_plate_out_of_the_views() {
    let (mut m, mut e) = setup();
    let (_, _, ev) = program(&mut m, &mut e, "board", "let body = m.box({size: [20mm 12mm 8mm]})\nobject(body)\nlet Board = m.link(m.box({size: [30mm 2mm 20mm]}), \"board:demo\")\nobject(Board)\n");
    let names = |parts: &[fab_plate::Part]| parts.iter().map(|p| p.object.clone()).collect::<Vec<_>>();
    assert_eq!(names(&ev.plate.parts), ["t/body"], "only the body prints");
    assert_eq!(ev.plate.excluded.iter().map(|x| (x.part.object.as_str(), x.link.as_deref(), x.handles.hidden)).collect::<Vec<_>>(), [("t/Board", Some("board:demo"), false)], "the board shows, unprinted");
    let (_, _, forced) = apply(&mut m, &mut e, "print-board", vec![pl("Board", PlaceArg::Print(true))]);
    assert_eq!(names(&forced.plate.parts), ["t/body"], "a board's own `print: true` does not print it");
    let (_, _, hidden) = apply(&mut m, &mut e, "hide", vec![pl("Board", PlaceArg::Hidden(true))]);
    assert_eq!(hidden.plate.excluded.len(), 1, "a hidden board stays on the plate");
    assert!(hidden.plate.excluded[0].handles.hidden, "its handles say it is hidden");
    assert_eq!(hidden.plate.parts, forced.plate.parts, "the printed part is unchanged");
    let text = m.project_text();
    assert!(text.contains("hidden: true"), "the project keeps it: {text}");
    let loaded = FabModel::load("hidden", &text, m.state().scripts.clone()).unwrap();
    assert_eq!(loaded.project(), m.project(), "it loads back as it was saved");
    let (_, _, undone) = m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    assert!(!undone.plate.excluded[0].handles.hidden, "one undo shows it again");
}

/// The shelf pack keeps each footprint where every nozzle it prints with reaches: on the
/// dual-nozzle printer three 100 mm tiles that need both nozzles take two rows (one row of three
/// would put the outer ones in the single-nozzle strips), the bed alone still packs them in one
/// row as before, and a footprint wider than where both reach is told as having no room.
#[test]
fn arrange_keeps_each_footprint_where_its_nozzles_reach() {
    let machine = Machine::dual_nozzle_350();
    let tiles = |region: fab_plate::machine::Rect| vec![("a", [100.0, 50.0], 0.64, region), ("b", [100.0, 50.0], 0.64, region), ("c", [100.0, 50.0], 0.64, region)];
    let (slots, unplaced) = eval::arrange(&machine, &mut tiles(machine.reach(&[0, 1])), false);
    assert!(unplaced.is_empty(), "{unplaced:?}");
    for (g, c) in &slots {
        let b = fab_plate::Bounds { min: [c[0] - 50.0, c[1] - 25.0, 0.0], max: [c[0] + 50.0, c[1] + 25.0, 0.64] };
        assert_eq!(machine.nozzles_reaching(&b), vec![0, 1], "{g} at {c:?}");
    }
    let (slots, unplaced) = eval::arrange(&machine, &mut tiles(machine.bed), false);
    assert!(unplaced.is_empty(), "{unplaced:?}");
    assert_eq!(slots, vec![("a", [67.0, 160.0]), ("b", [175.0, 160.0]), ("c", [283.0, 160.0])]);
    let (slots, unplaced) = eval::arrange(&machine, &mut [("wide", [310.0, 50.0], 1.0, machine.reach(&[0, 1]))], false);
    assert_eq!((slots, unplaced), (vec![("wide", [175.0, 160.0])], vec!["wide"]));
    let why = eval::no_room(&machine, "wide", [310.0, 50.0], &[0, 1]);
    assert_eq!(why, "no room for 'wide' (310 × 50 mm) where the Left and Right nozzles both reach");

    let body = src("let a = m.box({size: [12mm 2mm 12mm]})\nlet b = m.box({size: [8mm 2mm 8mm]})\nlet fixed = m.box({size: [2mm 2mm 2mm]})\nobject(a)\nobject(b)\nobject(fixed)\n");
    let project = "use mod.fab.*\nscript(\"t.cad.splash\")\nplace(\"t\", object: \"fixed\", at: [45mm 55mm], scale: 2)\n";
    let mut m = FabModel::load("gap", project, [(F.to_string(), body)].into()).expect("the plate with a fixed part must load");
    let mut e = Evaluator::new(machine);
    let check_gap = |ev: &Evaluated, want: f64| {
        let bounds = |id: &str| ev.plate.parts.iter().find(|p| p.object == id).unwrap_or_else(|| panic!("the pack must keep {id}")).bounds();
        let (a, b) = (bounds("t/a"), bounds("t/b"));
        assert!((a.center()[1] - b.center()[1]).abs() < 0.01, "both free parts must fit on one row");
        assert!((b.min[0] - a.max[0] - want).abs() < 0.01, "free footprints must have {want} mm between them: {a:?}, {b:?}");
        let fixed = bounds("t/fixed").center();
        assert!((fixed[0] - 45.0).abs() < 0.01 && (fixed[1] - 55.0).abs() < 0.01, "changing the pack must preserve the fixed placement: {fixed:?}");
    };
    check_gap(&m.evaluate(&mut e, None).expect("the default pack must build").1, 8.0);
    let (_, mut job, ev) = apply(&mut m, &mut e, "gap", project::changes("arrange(gap: 5mm)").unwrap());
    check_gap(&ev, 5.0);
    assert_eq!(project::parse(&m.project_text()).unwrap().arrange_gap, Some(5.0), "the saved project must reload its gap in millimetres");
    let floor = e.machine.clearance.max_radius + 24.0;
    job.settings.push(("print_sequence".into(), "by object".into()));
    let ev = e.evaluate_with_gap(&job, m.project().arrange_gap, None).expect("the sequential job must build with the project gap");
    check_gap(&ev, floor);
    assert!(ev.warnings.iter().any(|w| w.contains("5 mm") && w.contains(&format!("{floor} mm")) && w.contains("clearance")), "the plate must explain the raised gap: {:?}", ev.warnings);
    assert_eq!(m.project().arrange_gap, Some(5.0), "sequential clearance must preserve the requested gap");
    let ev = e.evaluate_with_gap(&job, Some(floor + 1.0), None).expect("the sequential job must accept a wider gap");
    check_gap(&ev, floor + 1.0);
    assert!(!ev.warnings.iter().any(|w| w.contains("raised the arrange gap")), "a gap above toolhead clearance must be kept without a raise note");
    let (_, _, explicit) = apply(&mut m, &mut e, "eight", project::changes("arrange(gap: 8mm)").unwrap());
    assert_eq!(m.project_text().matches("arrange(").count(), 1, "a second gap must replace the saved statement");
    let (_, _, ev) = apply(&mut m, &mut e, "reset", project::changes("arrange()").unwrap());
    check_gap(&ev, 8.0);
    for (a, b) in explicit.plate.parts.iter().zip(&ev.plate.parts) {
        assert_eq!(a.bounds(), b.bounds(), "an explicit 8 mm gap must give {} its default position", a.object);
    }
    assert!(!m.project_text().contains("arrange("), "reset must remove the project gap statement");
    assert!(project::changes("arrange(gap: -1mm)").unwrap_err()[0].message.contains("non-negative"), "negative gaps must explain the allowed range");
    assert_eq!(project::parse("use mod.fab.*\narrange(gap: 0.005)").unwrap().arrange_gap, Some(5.0), "unitless project lengths must be metres");
}

/// The layout keeps what the job prints round the parts (its skirt, a brim) where their nozzles
/// reach: two 150 mm boxes on the Right nozzle (reach x 25..350) make a row whose centring meets
/// the reach's edge. With no rim the row runs to that edge, which put the skirt out of reach;
/// with a 2.5 mm skirt each box's footprint grown by it stays in reach, as does a free spot's.
/// An automatic brim adds by a part's height and footprint, past its gap.
#[test]
fn layout_keeps_the_jobs_rim_where_the_nozzles_reach() {
    let (mut m, mut e) = setup();
    e.mapped = std::sync::Arc::new(eval::Mapped { inks: Default::default(), tools: [("Colour 1".to_string(), (1, 1))].into() });
    let right = e.machine.nozzles[1].reach;
    let grown = |b: fab_plate::Bounds, d: f64| fab_plate::Bounds { min: [b.min[0] - d, b.min[1] - d, 0.0], max: [b.max[0] + d, b.max[1] + d, 0.0] };
    let in_reach = |b: fab_plate::Bounds| right.contains_footprint(&grown(b, 2.5), fab_plate::machine::EDGE_SLACK);
    let (_, _, bare) = program(&mut m, &mut e, "row", "let a = m.box({size: [150mm 10mm 40mm]})\nlet b = m.box({size: [150mm 10mm 40mm]})\nobject(a)\nobject(b)\n");
    assert!(bare.plate.parts.len() == 2 && bare.plate.parts.iter().all(|p| p.nozzle == Some(1)), "two boxes on the Right nozzle");
    assert!(bare.plate.parts.iter().any(|p| !in_reach(p.bounds())), "with no rim the row runs to the reach's edge");
    e.rim = eval::Rim { skirt: 2.5, brim: eval::Brim::None };
    let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    for p in &ev.plate.parts {
        assert!(in_reach(p.bounds()), "{} with its skirt ({:?}) lies where the Right nozzle reaches", p.object, p.bounds());
    }
    assert!(ev.warnings.is_empty(), "{:?}", ev.warnings);
    let spot = eval::free_spot(&ev, [30.0, 30.0], 10.0, &[1]).expect("a free spot");
    assert!(in_reach(fab_plate::Bounds { min: [spot[0] - 15.0, spot[1] - 15.0, 0.0], max: [spot[0] + 15.0, spot[1] + 15.0, 0.0] }), "a free spot keeps the skirt in reach too: {spot:?}");
    let auto = eval::Rim { skirt: 2.5, brim: eval::Brim::Auto(0.1) };
    assert_eq!(auto.past([150.0, 40.0], 10.0), 2.5, "a low part gets no automatic brim");
    assert!((auto.past([10.0, 10.0], 100.0) - 12.6).abs() < 1e-9, "a tall narrow one's brim (its larger side) and gap: {}", auto.past([10.0, 10.0], 100.0));
}

#[test]
fn excluded_field_stays_a_field_and_still_reserves_a_free_spot() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "field", "let body = m.blend([m.box({size: [20mm 12mm 8mm]}) m.sphere({radius: 8mm})], 1mm)\nobject(body)\n");
    let (_, _, on) = apply(&mut m, &mut e, "position", vec![pl("body", PlaceArg::At(Some([100.0, 160.0])))]);
    let (_, _, off) = apply(&mut m, &mut e, "exclude", vec![pl("body", PlaceArg::Print(false))]);
    assert!(off.plate.parts.is_empty());
    assert!(off.reports.is_empty() && off.fields.is_empty() && off.weaves.is_empty() && off.mech.is_empty());
    // The eye costs no mesh, and the views keep drawing the field they have.
    assert!(off.plate.excluded[0].render.indices.is_empty());
    assert_eq!(off.placed_field(0).map(|f| f.key), on.placed_field(0).map(|f| f.key));
    assert!(off.placed_field(0).is_some());
    assert_eq!(off.plate.excluded[0].part.bounds(), on.plate.parts[0].bounds());
    assert_eq!(eval::free_spot(&on, [10.0; 2], 8.0, &[]), eval::free_spot(&off, [10.0; 2], 8.0, &[]));
    // What stands on a free spot keeps it while the plate leaves it free (the boards do).
    let spot = eval::free_spot(&on, [10.0; 2], 8.0, &[]).expect("the plate has room");
    assert!(eval::spot_still_free(&on, spot, [10.0; 2]), "a free spot stays free on the same plate");
    assert!(!eval::spot_still_free(&on, [100.0, 160.0], [10.0; 2]), "a spot over a part is taken");
    // A second new part keeps off the first one placed there.
    let first = fab_plate::Bounds { min: [spot[0] - 5.0, spot[1] - 5.0, 0.0], max: [spot[0] + 5.0, spot[1] + 5.0, 8.0] };
    let off: Vec<fab_plate::Bounds> = on.plate.placed_parts().map(|p| p.bounds()).chain([first]).collect();
    let next = eval::free_spot_off(&on, &off, [10.0; 2], 8.0, &[]).expect("room for two");
    assert!((next[0] - spot[0]).abs() > 10.0 || (next[1] - spot[1]).abs() > 10.0, "{next:?} keeps off {spot:?}");
}

/// A plate filled but for a strip at the back, where the printer's keep-out zone lies: a new
/// part goes to that strip beside the zone, never over it, and a spot over it is taken.
#[test]
fn a_free_spot_keeps_off_the_printers_keep_out_zones() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "slab", "let slab = m.box({size: [340mm 5mm 240mm]})\nobject(slab)\n");
    let (_, _, ev) = apply(&mut m, &mut e, "fill", vec![pl("slab", PlaceArg::At(Some([175.0, 125.0])))]);
    let b = ev.plate.parts[0].bounds();
    assert!(b.max[0] > 330.0 && b.max[1] < 250.0, "the slab fills the plate but its back strip: {b:?}");
    let zone = ev.machine.keep_out[0];
    let size = [50.0, 50.0];
    let spot = eval::free_spot(&ev, size, 10.0, &[]).expect("room at the back");
    let over = |c: [f64; 2]| zone.overlaps_footprint(&fab_plate::Bounds { min: [c[0] - 25.0, c[1] - 25.0, 0.0], max: [c[0] + 25.0, c[1] + 25.0, 10.0] });
    assert!(!over(spot), "{spot:?} keeps off keep-out zone {zone:?}");
    assert_eq!(spot, [115.0, 290.0], "the spot nearest the bed's centre beside the zone");
    assert!(over([175.0, 290.0]) && !eval::spot_still_free(&ev, [175.0, 290.0], size), "a spot over the zone is taken");
}

/// The force study lives in the script as builder statements on the part's surface: each
/// fixture and load is one call, with its region as written and its bytes for the Force tab's
/// edits. It is no solid: the shape and the slice stay as they are.
#[test]
fn a_study_is_kept_in_the_script_and_never_touches_the_shape() {
    use crate::study::{Kind, Where};
    let plain = "let hole = m.cylinder({radius: 4mm height: 40mm})\nlet body = m.subtract(m.blend([m.box({size: [30mm 30mm 20mm]}) m.sphere({radius: 13mm})], 1mm), hole)\nobject(body)\n";
    let studied = format!(
        "{plain}m.fixed(hole)\nm.force(Face{{at: [0, 15mm, 0]}}, [0, -50, 0])\nm.pressure(m.box({{size: [10mm 10mm 10mm] at: [10mm 15mm 0]}}), mpa: 0.2)\nm.torque(HoleWall{{at: [4mm, 0, 0]}}, 300)\nfor x in [1] {{ m.fixed(Face{{at: [15mm, 0, 0]}}, rigid: true) }}\n"
    );
    let (mut m, mut e) = setup();
    let (_, _, before) = program(&mut m, &mut e, "s0", plain);
    assert!(before.placed_study(0).is_none(), "no study, no entry");
    let (_, c, ev) = program(&mut m, &mut e, "s1", &studied);
    let study = ev.placed_study(0).expect("the part's study");
    let source = c.instances[0].script.source();
    let text = |r: &Option<std::ops::Range<usize>>| r.clone().map_or("", |r| &source[r]);
    let l = &study.loads;
    assert_eq!(l.len(), 5);
    assert!(study.posed());
    assert!(matches!((&l[0].kind, &l[0].at), (Kind::Fixed, Where::Feature(_))) && l[0].names == ["hole"] && l[0].on == "hole");
    assert!(matches!((&l[1].kind, &l[1].at), (Kind::Force(f), Where::Face(p)) if *f == [0.0, -50.0, 0.0] && (p[1] - 15.0).abs() < 1e-9));
    assert!(matches!((&l[2].kind, &l[2].at), (Kind::Pressure(p), Where::Inside(_)) if *p == 0.2));
    assert!(matches!((&l[3].kind, &l[3].at), (Kind::Torque(t), Where::HoleWall(p)) if *t == 300.0 && (p[0] - 4.0).abs() < 1e-9));
    assert_eq!([text(&l[1].value_span), text(&l[2].value_span), text(&l[3].value_span)], ["[0, -50, 0]", "0.2", "300"]);
    assert_eq!(text(&l[1].span), "m.force(Face{at: [0, 15mm, 0]}, [0, -50, 0])");
    assert!(l[..4].iter().all(|l| l.top) && !l[4].top, "a call in a loop is edited in the code");
    assert!(!l[0].rigid && l[4].rigid, "a fixture is a bolt's clamp unless rigid");
    // the same shape: nothing built, nothing to slice again
    assert_eq!(ev.fields[0].as_ref().map(|f| f.key), before.fields[0].as_ref().map(|f| f.key));
    assert_eq!(ev.built, 0, "a study edit builds no mesh or field");
    assert!(ev.changes[0] == crate::eval::PartChange::Same, "the slice stays");
    // what cannot hold or load says why, at its line
    for (bad, says) in [("m.force(hole)", "newtons"), ("m.fixed(Flood{seed: [0 15mm 0]})", "Flood"), ("m.fixed(3)", "Face{at"), ("m.torque(hole, [1 2 3])", "N mm"), ("m.fixed(hole, rigid: 2)", "true or false")] {
        let head = m.head();
        let err = m.apply(&mut e, "bad", head, &[sc(Edit::Program { source: src(&format!("{plain}{bad}\n")) })], None).err().expect("refused");
        let err = format!("{err}");
        assert!(err.contains(says) && err.contains(":8:"), "{bad}: {err}");
    }
}

/// The person's numbers keep their solids and lines, judge a measurement and explain an edit.
#[test]
fn an_expect_is_parsed_judged_and_its_changes_said() {
    use crate::expect::{changes, describe, judge, Measure, Verdict};
    let (mut m, mut e) = setup();
    let plain = "let hole = m.cylinder({radius: 4mm height: 12mm})
let holes = for i in 0..4 { m.cylinder({radius: 1mm height: 12mm at: [i * 4mm, 0, 0]}) }
let body = m.subtract(m.box({size: [61mm 3mm 64mm]}), hole)
object(body)
";
    let checked = [plain, r#"m.expect("8 mm hole", hole, {x: 8mm z: 8mm})
m.expect("4 holes", holes, {count: 4})
"#].concat();
    let (_, c, ev) = program(&mut m, &mut e, "checked", &checked);
    // the build judges them on what it made: the hole's own mesh (model frame), the list's length
    let judged: Vec<(&str, Verdict)> = ev.expects.iter().map(|j| (j.expect.name.as_str(), j.verdict())).collect();
    assert_eq!(judged, [("8 mm hole", Verdict::Pass), ("4 holes", Verdict::Pass)], "both hold on the build: {:?}", ev.expects);
    assert!((ev.expects[0].seen[0].as_ref().unwrap() - 8.0).abs() < 0.05, "the hole is read from its own mesh: {:?}", ev.expects[0].seen);
    // a wider hole fails at its line: a person's edit commits with a warning, an AI's would be refused
    let head = m.head();
    let wider = src(&checked.replace("radius: 4mm height: 12mm", "radius: 5mm height: 12mm"));
    let (_, built) = m.apply(&mut e, "wider", head, &[sc(Edit::Program { source: wider })], None).expect("a person's edit commits");
    let (_, after) = built.expect("it builds");
    assert!(after.warnings.iter().any(|w| w.contains(":9:") && w.contains("m.expect \"8 mm hole\" fails: x is 10 mm, asked 8 mm")), "a warning at its line: {:?}", after.warnings);
    let refused = crate::expect::newly_failing(Some(&ev), &after, false).expect("it fails where it held");
    assert!(refused.line == 9 && refused.to_string().contains("It is the person's number"), "{refused}");
    assert!(crate::expect::newly_failing(Some(&after), &after, false).is_none(), "one already failing is not refused again");
    let script = &c.instances[0].script;
    let before = &script.expects;
    let [hole, holes] = before.as_slice() else { panic!("the script must keep both checks: {before:?}") };
    assert_eq!([hole.name.as_str(), holes.name.as_str()], ["8 mm hole", "4 holes"], "the checks keep the person's words in script order");
    assert_eq!([hole.loc.map(|p| p.0), holes.loc.map(|p| p.0)], [Some(9), Some(10)], "each check keeps its own source line");
    let ids = |indices: &[usize]| indices.iter().map(|&i| script.program.solids[i].id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids(&hole.solids), ["hole"], "the size check points at the named hole");
    assert_eq!(ids(&holes.solids), ["holes[0]", "holes[1]", "holes[2]", "holes[3]"], "the count keeps every solid in loop order");
    let [x, z] = hole.wants.as_slice() else { panic!("the hole must have two wants") };
    assert_eq!((&x.measure, &z.measure), (&Measure::X, &Measure::Z), "the checks name the two model axes");
    assert_eq!((x.value, z.value, x.tol), (Some(8.0), Some(8.0), 0.05), "lengths and their default tolerance are in millimetres");
    assert_eq!(judge(x, Ok(8.02)), Verdict::Pass, "a size within tolerance passes");
    assert_eq!(judge(x, Ok(8.2)), Verdict::Fail, "a size outside tolerance fails");
    assert_eq!(judge(x, Err("why".into())), Verdict::NotChecked("why".into()), "a missing measurement keeps its reason");
    let count = &holes.wants[0];
    assert_eq!(count.measure, Measure::Count, "the number of holes is a count");
    assert_eq!((judge(count, Ok(4.0)), judge(count, Ok(4.01))), (Verdict::Pass, Verdict::Fail), "a count has no tolerance");
    assert_eq!([describe(x), describe(count)], ["8 mm", "4"], "only the length carries millimetres");

    let edited = src(&[plain, r#"m.expect("8 mm hole", hole, {x: 10mm z: 10mm})
m.expect("fits a 60 mm fan", body, {x: {min: 60.4mm}})
m.expect("material", body, {volume_mm3: {min: 11000 max: 13000}})
m.expect("volume", body, {volume_mm3: 12000})
m.expect("wall", body, {wall: 2mm at: [0 10mm 30mm] tol: 0.2mm})
"#].concat());
    let preview = m.script(F, &edited, None, &[], &m.state, &[]).expect("the edited checks compile");
    let after = &preview.expects;
    let fan = &after[1].wants[0];
    assert_eq!(describe(fan), "at least 60.4 mm", "a lower bound keeps its fractional millimetres");
    assert_eq!((judge(fan, Ok(60.0)), judge(fan, Ok(60.5))), (Verdict::Fail, Verdict::Pass), "a minimum admits only sizes at or above it");
    let range = &after[2].wants[0];
    assert_eq!(describe(range), "11000 to 13000 mm³", "a volume range carries cubic millimetres");
    assert_eq!((judge(range, Ok(13000.0 + 5e-10)), judge(range, Ok(13000.1))), (Verdict::Pass, Verdict::Fail), "the upper bound allows rounding noise");
    let volume = &after[3].wants[0];
    assert_eq!((judge(volume, Ok(12120.0)), judge(volume, Ok(11879.0))), (Verdict::Pass, Verdict::Fail), "a volume target allows one percent");
    let wall = &after[4].wants[0];
    assert!(matches!(wall.measure, Measure::Wall { at } if at == [0.0, 10.0, 30.0]), "the wall point is in model millimetres");
    assert_eq!((judge(wall, Ok(2.15)), judge(wall, Ok(2.25))), (Verdict::Pass, Verdict::Fail), "a wall uses the written length tolerance");
    assert_eq!(changes(before, &after[..2]), [
        "changed the person's check `8 mm hole`: x 8 mm → 10 mm, z 8 mm → 10 mm",
        "removed the person's check `4 holes`",
        "added the check `fits a 60 mm fan`: x at least 60.4 mm",
    ], "each changed, removed or added check gets one line");
    assert!(changes(before, before).is_empty(), "unchanged checks stay quiet");
    let mut axes = hole.clone();
    axes.wants[1].measure = Measure::Y;
    axes.wants[1].value = Some(4.0);
    assert_eq!(changes(&before[..1], &[axes]), ["changed the person's check `8 mm hole`: z dropped, y 4 mm added"], "a check lists dropped and added measures");

    for (bad, says) in [
        ("m.expect(solid: hole, want: {x: 8mm})", "give it the person's words first"),
        ("m.expect(\"diameter\", 4, {x: 8mm})", "the second argument is the solid"),
        ("m.expect(\"diameter\", hole, {diam: 8mm})", "unknown key 'diam'"),
        ("m.expect(\"wall\", body, {wall: {min: 2mm}})", "wall needs at: [x y z]"),
        ("m.expect(\"point\", body, {x: 8mm at: [0 0 0]})", "needs wall"),
        ("m.expect(\"number\", holes, {count: {max: 3.5}})", "count.max must be a whole number"),
    ] {
        let head = m.head();
        let err = m.apply(&mut e, "bad", head, &[sc(Edit::Program { source: src(&format!("{plain}{bad}\n")) })], None).err().expect("a malformed check must refuse the edit");
        let err = err.to_string();
        assert!(err.contains(":9:") && err.contains("m.expect: ") && err.contains(says), "{bad} must say '{says}' at line 9: {err}");
    }
}

/// The demo bracket carries its study: the wall's screw holes held, the shelf pushed down.
#[test]
fn the_bracket_script_carries_its_study() {
    use crate::study::{Kind, Where};
    let (mut m, mut e) = setup();
    let body = body_of(include_str!("../../app/resources/bracket.cad.splash"));
    let (_, _, ev) = program(&mut m, &mut e, "b", body);
    let study = ev.placed_study(0).expect("the bracket's study");
    assert!(study.posed());
    let fixed: Vec<&str> = study.loads.iter().filter(|l| l.kind == Kind::Fixed).flat_map(|l| l.names.iter().map(String::as_str)).collect();
    assert_eq!(fixed, ["holes[1]", "holes[3]"]);
    assert!(study.loads.iter().any(|l| matches!((&l.kind, &l.at), (Kind::Force(f), Where::Face(_)) if f[1] < 0.0)));
    // a load edited: the same mesh part, the very mesh it had (what keys the part by it, as
    // the Force study's samples do, keeps them)
    let (_, _, again) = program(&mut m, &mut e, "b2", &body.replace("[0, -50, 0])", "[0, -80, 0])"));
    assert!(ev.fields[0].is_none(), "the bracket is a mesh part");
    assert!(std::sync::Arc::ptr_eq(&ev.frames[0].local, &again.frames[0].local), "a study edit keeps the part's mesh");
    assert!(again.changes[0] == crate::eval::PartChange::Same);
}

/// A graded part's stress outlives an edit while the next solve runs, an answer the edit has
/// outrun changes nothing, and once nothing poses the part it has none.
#[test]
fn a_graded_part_grades_by_its_last_stress_through_an_edit() {
    use makepad_model_script::lang::csg_sdf::SdfGrid;
    let web = src("let spare = param(1mm, min: 0mm, max: 9mm)\nlet web = m.voronoi(m.box({size: [40mm 40mm 40mm]}), {size: 12mm strut: 1.6mm strut_max: 3mm adapt: @stress core: @none})\nobject(web)\n");
    let mut m = FabModel::with_script("test", F, &web);
    let graded = |c: &Compiled| {
        let p = &c.instances[0].script.program;
        assert_eq!(p.stress_requests.len(), 1, "a graded part always asks");
        p.notes.is_empty()
    };
    let grid = |s: f32| Arc::new(SdfGrid::new([-20.0; 3], 20.0, [3, 3, 3], 0.0, vec![s; 27].into()));
    assert!(!graded(&m.compile().unwrap()), "ungraded until its first answer");
    assert!(m.ask_solved(F, "web", Some(1)));
    assert!(!m.ask_solved(F, "web", Some(1)), "asked once for what did not change");
    assert!(m.set_solved(F, "web", Some(1), Some(grid(0.5))));
    assert!(graded(&m.compile().unwrap()));
    let (_, edited) = m.edited(&[sc(Edit::Set { site: "spare".into(), value: "2mm".into() })]).unwrap();
    assert!(graded(&edited), "an edit grades by the last stress");
    assert!(m.ask_solved(F, "web", Some(2)));
    assert!(!m.set_solved(F, "web", Some(1), Some(grid(0.25))), "an outrun answer changes nothing");
    assert!(m.ask_solved(F, "web", None));
    assert!(m.set_solved(F, "web", None, None));
    assert!(!graded(&m.compile().unwrap()), "nothing poses it: no stress to grade by");
}

/// `object([b, a, c], name: ..)`: one object of the bodies it lists, a part each, in the list's
/// order, all three glued by one id (their placement's); a jointed object's bodies print apart.
/// A person reads the first as the object, a labelled body by its label, another by the object
/// and its id.
#[test]
fn an_object_of_listed_bodies_prints_them_as_one() {
    let (mut m, mut e) = setup();
    let body = "let a = m.box({size: [10mm 10mm 10mm] at: [0 5mm 0]})\nlet b = m.box({size: [10mm 10mm 10mm] at: [6mm 5mm 0]})\nlet c = m.box({size: [4mm 4mm 4mm] at: [-6mm 9mm 0] label: \"Cap\"})\nobject([b, a, c], name: \"trio\")\n";
    let (_, c, ev) = program(&mut m, &mut e, "trio", body);
    let objects = &c.instances[0].script.objects;
    assert_eq!(objects.len(), 1, "one object");
    let names: Vec<&str> = objects[0].bodies.iter().map(|&i| prog(&c).solids[i].id.as_str()).collect();
    assert_eq!((objects[0].name.as_str(), names), ("trio", vec!["b", "a", "c"]), "its bodies in the list's order");
    assert_eq!(objects[0].glued, objects[0].bodies, "all three glued");
    assert_eq!(ev.plate.parts.iter().map(|p| p.object.rsplit('/').next().unwrap_or_default()).collect::<Vec<_>>(), vec!["b", "a", "c"], "a part a body");
    let of: Vec<Option<&str>> = ev.plate.parts.iter().map(|p| p.body_of.as_deref()).collect();
    assert!(of[0].is_some() && of.iter().all(|o| *o == of[0]), "one id on all three: {of:?}");
    assert_eq!(ev.plate.groups(), vec![vec![0, 1, 2]], "one group, in the list's order");
    assert_eq!(ev.plate.parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["trio", "trio a", "Cap"], "the object, a body by its id, a body by its label");
    let (_, _, ev) = program(&mut m, &mut e, "chain", CHAIN);
    assert!(ev.plate.parts.len() == 3 && ev.plate.parts.iter().all(|p| p.body_of.is_none()), "a jointed chain's links print apart");
}

/// Bodies under the same layers share their paint program. Why (user): a parameter commit on a
/// plate of many bodies took seconds beyond its evaluation, measuring every layer once per body
/// as the script compiled, then dropping them all two edits on.
#[test]
fn bodies_under_the_same_paint_share_their_program() {
    let (mut m, mut e) = setup();
    let body = "let a = m.box({size: [10mm 10mm 10mm] at: [0 5mm 0]})\nlet b = m.box({size: [10mm 10mm 10mm] at: [6mm 5mm 0]})\nlet c = m.box({size: [4mm 4mm 4mm] at: [-6mm 9mm 0]})\nobject([b, a, c], name: \"trio\")\nlet red = Material{albedo: [0.8 0.05 0.05]}\nlet blue = Material{albedo: [0.05 0.2 0.85]}\nm.fill(m.sphere({radius: 6mm at: [0 5mm 0]}), red)\nm.fill(a, blue)\n";
    let (_, c, _) = program(&mut m, &mut e, "trio", body);
    let of = |name: &str| {
        let i = prog(&c).solids.iter().position(|s| s.id == name).expect("the body");
        c.instances[0].script.paint[&i].program.clone()
    };
    assert!(std::sync::Arc::ptr_eq(&of("b"), &of("c")), "the same layers: one program");
    assert!(!std::sync::Arc::ptr_eq(&of("a"), &of("b")), "a layer of its own: a program of its own");
}

fn close(a: [f64; 3], b: [f64; 3], what: &str) {
    assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6), "{what}: {a:?} against {b:?}");
}

#[test]
fn a_test_print_follows_its_part() {
    let (mut m, mut e) = setup();
    program(&mut m, &mut e, "r1", BRACKET);
    apply(&mut m, &mut e, "place", vec![pl("bracket", PlaceArg::At(Some([100.0, 100.0])))]);
    let (_, ev) = m.evaluate(&mut e, None).unwrap();
    let origin = ev.placed_frame(0).unwrap().map(|r| r[3]);
    let center = [origin[0] + 10.0, origin[1] + 5.0, origin[2] + 3.0];
    let add = project::TestEdit::Add { object: "t/bracket".into(), center, size: [20.0; 3], turn: [0.0; 3], name: Some("Corner".into()), on: true, in_model: false };
    let changes = ev.test_print_changes(m.project(), &[add]).unwrap();
    let content = ev.content;

    // It is where it was put; it changes nothing on the plate.
    let initial = m.state().clone();
    let (_, _, placed) = apply(&mut m, &mut e, "add", changes);
    assert_eq!(placed.content, content, "a cube is no change to the plate");
    let at = |ev: &Evaluated, m: &FabModel| ev.test_print_box(m.project().test_print("Corner").unwrap()).unwrap();
    let start = at(&placed, &m);
    close(start.center, center, "where it was put");
    close(start.size, [20.0; 3], "a true cube");

    // Moved: by what the part moved.
    let (_, _, moved) = apply(&mut m, &mut e, "move", vec![pl("bracket", PlaceArg::At(Some([130.0, 90.0])))]);
    close(at(&moved, &m).center, [center[0] + 30.0, center[1] - 10.0, center[2]], "moved");

    // Turned: about the part's origin, the cube's own turn with it.
    let (_, _, turned) = apply(&mut m, &mut e, "turn", vec![pl("bracket", PlaceArg::Rotate([0.0, 0.0, 90.0]))]);
    let frame = turned.placed_frame(0).unwrap();
    let o = [frame[0][3], frame[1][3], frame[2][3]];
    let box_ = at(&turned, &m);
    close(box_.center, [o[0] - 5.0, o[1] + 10.0, o[2] + 3.0], "turned");
    close(box_.turn, [0.0, 0.0, 90.0], "turned with the part");

    // Scaled: its offset and its sides.
    let (_, _, scaled) = apply(&mut m, &mut e, "scale", vec![pl("bracket", PlaceArg::Scale([2.0, 2.0, 3.0]))]);
    let frame = scaled.placed_frame(0).unwrap();
    let o = [frame[0][3], frame[1][3], frame[2][3]];
    let box_ = at(&scaled, &m);
    close(box_.center, [o[0] - 10.0, o[1] + 20.0, o[2] + 9.0], "scaled");
    close(box_.size, [40.0, 40.0, 60.0], "scaled sides");

    // Saved and read back, the same; edits are one undo step each, and undo restores it.
    let reloaded = project::parse(&m.project_text()).unwrap();
    assert_eq!(reloaded, *m.project(), "{}", m.project_text());
    let edited = project::TestPrint { turn: [10.0, -20.0, 15.0], pose: Some([90.0, 0.0, 0.0]), on: false, ..m.project().test_print("Corner").unwrap().clone() };
    let before_edit = m.state().clone();
    apply(&mut m, &mut e, "edit", vec![Change::TestPrint { name: "Corner".into(), set: Some(edited) }]);
    assert_eq!(project::parse(&m.project_text()).unwrap(), *m.project(), "{}", m.project_text());
    m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    assert_eq!(m.state(), &before_edit, "undo restores it");
    for _ in 0..4 {
        m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    }
    assert_eq!(m.state(), &initial, "undo takes each away");
}

#[test]
fn a_test_print_goes_with_its_part() {
    let (mut m, mut e) = linked_bracket();
    apply(&mut m, &mut e, "clone", vec![Change::Clone { instance: "bracket".into(), at: Some([240.0, 100.0]) }]);
    let cube = |name: &str, link: u32| project::TestPrint { name: name.into(), instance: "bracket".into(), object: "bracket".into(), link, at: [0.0; 3], size: [10.0; 3], turn: [0.0; 3], pose: None, on: true, brim: false };
    let (_, ev) = m.evaluate(&mut e, None).unwrap();
    assert!(ev.test_print_box(&cube("Base", 0)).is_some());
    apply(&mut m, &mut e, "two", vec![
        Change::TestPrint { name: "Base".into(), set: Some(cube("Base", 0)) },
        Change::TestPrint { name: "Link".into(), set: Some(cube("Link", 1)) },
    ]);

    // Two named alike, or a bad side or a missing one, are refused and change nothing.
    let head = m.head();
    let renamed = project::TestPrint { name: "Link".into(), ..cube("Base", 0) };
    assert!(m.apply(&mut e, "alike", head, &[Change::TestPrint { name: "Base".into(), set: Some(renamed) }], None).is_err());
    let thin = project::TestPrint { size: [0.0, 10.0, 10.0], ..cube("Thin", 0) };
    assert!(m.apply(&mut e, "thin", head, &[Change::TestPrint { name: "Thin".into(), set: Some(thin) }], None).is_err());
    assert!(m.apply(&mut e, "absent", head, &[Change::TestPrint { name: "None".into(), set: None }], None).is_err());
    assert_eq!(m.head(), head);

    // The original goes: its cube goes, and the link taking its place keeps its own.
    apply(&mut m, &mut e, "first", vec![Change::RemoveInstance { instance: "bracket".into() }]);
    assert_eq!(m.project().test_prints.iter().map(|t| (t.name.as_str(), t.link)).collect::<Vec<_>>(), [("Link", 0)]);

    // A script that stops emitting its object takes the cube too.
    let other = src("let other = m.box({size: [10mm 10mm 10mm]})\nobject(other)\n");
    apply(&mut m, &mut e, "other", vec![Change::Script { file: "bracket.cad.splash".into(), edit: Edit::Program { source: other } }]);
    assert!(m.project().test_prints.is_empty(), "{}", m.project_text());
    m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    assert_eq!(m.project().test_prints.len(), 1, "undo brings it back");
}

/// Thicken writes one readable chain on the body: named or inline, one place or several, a second
/// call goes on numbering it, every use of the body means the thickened one, and it thickens only
/// where it says.
#[test]
fn thicken_is_one_chain_on_the_body() {
    use crate::thicken::{thicken_change, Thicken};
    use makepad_csg_graph::Op;
    let (mut m, mut e) = setup();
    let source = "let rod = m.cylinder({radius: 3mm height: 60mm at: [0 30mm 0]})\nlet cap = m.sphere({radius: 8mm at: [0 62mm 0]})\nlet body = m.blend([rod cap], 1mm) // the body\nobject(body, name: \"post\")\n";
    let (_, c, _) = program(&mut m, &mut e, "k", source);
    let body_of = |c: &Compiled| c.instances[0].script.objects[0].solid;
    let spot = |y0: f64, y1: f64| Thicken { a: [0.0, y0, 0.0], b: [0.0, y1, 0.0], radius: 4.0 };
    let change = thicken_change(&c.instances[0], body_of(&c), "post", &[spot(10.0, 20.0), spot(30.0, 40.0)]).unwrap();
    let (_, c2, _) = apply(&mut m, &mut e, "t1", vec![change]);
    let written = c2.instances[0].script.source().to_string();
    assert!(written.contains("let body_base = m.blend([rod cap], 1mm) // the body\nlet body_thick1 = m.blend([body_base m.capsule({a: [0mm, 10mm, 0mm] b: [0mm, 20mm, 0mm] radius: 4mm})], 1.2mm)"), "{written}");
    assert!(written.contains("let body = m.blend([body_thick1 m.capsule({a: [0mm, 30mm, 0mm] b: [0mm, 40mm, 0mm]"), "{written}");
    assert!(written.contains("\nobject(body, name: \"post\")"), "the object statement stays: {written}");
    // what it reaches: thicker along the two places, as thick as it was past their radius
    let field = |c: &Compiled, x: f64, y: f64| {
        let p = prog(c);
        let Op::Field { shape, .. } = &p.graph.node(p.solids[body_of(c)].node).op else { panic!("not a field") };
        makepad_csg_sdf::distance(&shape.root, makepad_csg_math::Vec3d::new(x, y, 0.0))
    };
    assert!(field(&c, 3.9, 15.0) > 0.0 && field(&c2, 3.9, 15.0) <= 0.0, "it reaches 8 mm across on the place");
    assert!(field(&c2, 3.1, 53.0) * field(&c, 3.1, 53.0) > 0.0, "and leaves what's past its radius");
    // again, on what's there
    let change = thicken_change(&c2.instances[0], body_of(&c2), "post", &[spot(0.0, 5.0)]).unwrap();
    let (_, c3, _) = apply(&mut m, &mut e, "t2", vec![change]);
    let again = c3.instances[0].script.source().to_string();
    assert!(again.contains("let body_thick2 = m.blend([body_thick1 ") && again.contains("let body = m.blend([body_thick2 "), "{again}");
    // written in the object's own call
    let inline = "object(m.blend([m.cylinder({radius: 3mm height: 60mm at: [0 30mm 0]}) m.sphere({radius: 8mm at: [0 62mm 0]})], 1mm), name: \"post\")\n";
    let (_, c4, _) = program(&mut m, &mut e, "k2", inline);
    let change = thicken_change(&c4.instances[0], body_of(&c4), "post", &[spot(10.0, 20.0)]).unwrap();
    let (_, c5, _) = apply(&mut m, &mut e, "t3", vec![change]);
    let written = c5.instances[0].script.source().to_string();
    assert!(written.contains("let post_base = m.blend(") && written.contains("let post_thick1 = m.blend([post_base ") && written.contains("object(post_thick1, name: \"post\")"), "{written}");
}

/// A limb's joint and what goes with it come from the program's solids, and the turn writes the
/// limb's own numbers: the rest of the program stays as written, whatever one undo brings back.
#[test]
fn a_limb_turns_about_its_joint_with_what_it_carries() {
    use crate::align::limb_of;
    use crate::params::turned_about;
    use makepad_csg_graph::Op;
    let (mut m, mut e) = setup();
    let source = "let body = m.ellipsoid({radii: [9mm 14mm 8mm] at: [0 14mm 0]})\nlet arm = m.capsule({a: [7mm 22mm 0] b: [20mm 10mm 0] radius: 2.5mm})\nlet hand = m.ellipsoid({radii: [3mm 3mm 3mm] at: [21mm 9mm 0]})\nlet leg = m.capsule({a: [-4mm 3mm 0] b: [-4mm, -20mm, 0] radius: 3mm})\nlet figure = m.blend([body arm hand leg], 1.5mm)\nobject(figure, name: \"man\")\n";
    let (_, c, _) = program(&mut m, &mut e, "man", source);
    let (a, b) = ([7.0, 22.0, 0.0], [20.0, 10.0, 0.0]);
    let limb = limb_of(prog(&c), &c.instances[0].script.objects[0], "arm", a, b).unwrap();
    assert_eq!(limb.solids, ["arm", "hand"], "it carries its hand and nothing else");
    assert_eq!(limb.carried, ["hand"]);
    let joint: f64 = (0..3).map(|k| (limb.pivot[k] - a[k]).powi(2)).sum::<f64>().sqrt();
    assert!(joint < 6.0, "the joint's at the shoulder: {:?}", limb.pivot);
    assert!(limb.out[0] > 0.5 && limb.out[1] < 0.0, "out's towards the hand: {:?}", limb.out);
    let same_limb = limb_of(prog(&c), &c.instances[0].script.objects[0], "arm", b, a).unwrap();
    let flipped: f64 = (0..3).map(|k| (same_limb.pivot[k] - limb.pivot[k]).powi(2)).sum::<f64>().sqrt();
    assert!(flipped < 1e-6, "whichever way its line was given: {:?} {:?}", same_limb.pivot, limb.pivot);
    // a quarter turn about Z through the joint
    let turn = [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    let changes: Vec<Change> = limb.solids.iter().flat_map(|id| turned_about(&c, &c.instances[0].name, id, limb.pivot, &turn).unwrap()).collect();
    let (_, c2, _) = apply(&mut m, &mut e, "turn", changes);
    let p2 = prog(&c2);
    let node_of = |p: &makepad_model_script::lang::Program, id: &str| p.graph.node(p.solids.iter().find(|s| s.id == id).unwrap().node).key;
    assert_eq!(node_of(prog(&c), "body"), node_of(p2, "body"), "the body's as it was");
    assert_eq!(node_of(prog(&c), "leg"), node_of(p2, "leg"), "the leg's as it was");
    let turned = |x: [f64; 3]| -> [f64; 3] {
        let d: [f64; 3] = std::array::from_fn(|k| x[k] - limb.pivot[k]);
        std::array::from_fn(|r| limb.pivot[r] + (0..3).map(|k| turn[r][k] * d[k]).sum::<f64>())
    };
    let arm = p2.solids.iter().find(|s| s.id == "arm").unwrap();
    let Op::Field { shape, .. } = &p2.graph.node(arm.node).op else { panic!("not a field: {}", c2.instances[0].script.source()) };
    let makepad_csg_sdf::SdfNode::Id { of, .. } = &shape.root else { panic!("no id: {:?}", shape.root) };
    let makepad_csg_sdf::SdfNode::Capsule { a: a2, b: b2, radius } = &**of else { panic!("not a capsule: {of:?}") };
    for (now, was) in [(a2, a), (b2, b)] {
        let off: f64 = (0..3).map(|k| (now[k] - turned(was)[k]).powi(2)).sum::<f64>().sqrt();
        assert!(off < 0.01, "an end's where the turn takes it: {now:?} {:?}", turned(was));
    }
    assert_eq!(*radius, 2.5, "and it's the same limb");
    let hand = p2.solids.iter().position(|s| s.id == "hand").unwrap();
    let (at, rotate, _) = crate::eval::own_placement(p2, hand).unwrap();
    let off: f64 = (0..3).map(|k| (at[k] - turned([21.0, 9.0, 0.0])[k]).powi(2)).sum::<f64>().sqrt();
    assert!(off < 0.01, "the hand goes with it: {at:?}");
    assert!((rotate[2] - 90.0).abs() < 0.01, "and turns with it: {rotate:?}");
    let back = m.history(&mut e, m.head(), HistoryAction::Undo, None).unwrap();
    assert_eq!(back.1.instances[0].script.source(), src(source), "one undo, byte for byte");

    // what can't turn says so
    let (_, c, _) = program(&mut m, &mut e, "bar", "let left = m.sphere({radius: 8mm at: [-20mm 0 0]})\nlet right = m.sphere({radius: 8mm at: [20mm 0 0]})\nlet under = m.box({size: [60mm 4mm 10mm] at: [0 8mm 0]})\nlet both = m.capsule({a: [-20mm 0 0] b: [20mm 0 0] radius: 2mm})\nobject(m.blend([left right under both], 1mm), name: \"bar\")\n");
    let why = limb_of(prog(&c), &c.instances[0].script.objects[0], "both", [-20.0, 0.0, 0.0], [20.0, 0.0, 0.0]).unwrap_err();
    assert!(why.contains("both ends"), "{why}");
    let (_, c, _) = program(&mut m, &mut e, "rod", "let rod = m.capsule({a: [0 0 0] b: [0 30mm 0] radius: 2mm})\nobject(rod, name: \"rod\")\n");
    let why = limb_of(prog(&c), &c.instances[0].script.objects[0], "rod", [0.0, 0.0, 0.0], [0.0, 30.0, 0.0]).unwrap_err();
    assert!(why.contains("no solid") || why.contains("whole part") || why.contains("isn't a shape"), "{why}");
}

#[test]
fn a_parts_named_frame_reaches_the_host_in_the_model_frame_and_follows_its_placement() {
    let (mut m, mut e) = setup();
    // The frame's Y turns onto the model's -Z and its Z onto +Y (a board frame in a case).
    let body = "let f = Frame{name: \"datum\" at: [0 10mm 0] rotate: [-90° 0 0]}\nlet a = m.box({size: [10mm 10mm 10mm] frame: f})\nlet b = m.subtract(m.box({size: [20mm 10mm 20mm]}), m.box({size: [2mm 2mm 2mm]}), {rotate: [180° 0 0] at: [50mm 5mm 0] frame: f})\nobject(a)\nobject(b)\n";
    let (_, c, ev) = program(&mut m, &mut e, "frame", body);
    let solid = prog(&c).solids.iter().find(|s| s.id == "a").expect("a");
    assert_eq!(solid.frame.as_ref().map(|(n, at, r)| (n.as_str(), *at, *r)), Some(("datum", [0.0, 10.0, 0.0], [-90.0, 0.0, 0.0])), "the compiled part keeps its frame in kernel units and degrees");
    let frame = |id: &str| ev.objects.iter().find(|o| o.name.ends_with(&format!("/{id}"))).and_then(|o| o.frame.clone()).unwrap_or_else(|| panic!("{id} has its frame"));
    let near = |a: &[[f64; 4]; 3], b: &[[f64; 4]; 3]| a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() < 1e-9);
    let (name, a) = frame("a");
    assert_eq!(name, "datum");
    assert!(near(&a, &[[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 10.0], [0.0, -1.0, 0.0, 0.0]]), "an unplaced part's frame is the frame as written: {a:?}");
    // Turned half over about X and moved: the frame turns and moves with it.
    let (_, b) = frame("b");
    assert!(near(&b, &[[1.0, 0.0, 0.0, 50.0], [0.0, 0.0, -1.0, -5.0], [0.0, 1.0, 0.0, 0.0]]), "a placed part's frame follows its placement: {b:?}");
    assert!(ev.objects.iter().filter(|o| !o.name.ends_with("/a") && !o.name.ends_with("/b")).all(|o| o.frame.is_none()), "operands without a frame carry none");
    let bad = FabModel::with_script("t", "t.cad.splash", &src("object(m.box({size: [1mm 1mm 1mm] frame: [1 2 3]}))\n"));
    let err = bad.evaluate(&mut e, None).expect_err("a frame that is not a Frame fails");
    assert!(err.message.contains("'frame' must be Frame{name at rotate}"), "{err}");
}

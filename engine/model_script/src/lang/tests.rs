use super::*;

const ROOTS: &[RootFn] = &[RootFn { name: "place", args: &["solid", "at", "rotate", "name", "filament", "color"], model: false }, RootFn { name: "fixed", args: &["region", "rigid"], model: true }];

fn eval(src: &str) -> Result<Program, Vec<Diag>> {
    evaluate(src, &Options { generated: &Default::default(), assets: &Assets::new(), images: &Default::default(), extension: "fab", libraries: &[], inputs: &NO_INPUTS, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: HashMap::new() })
}

#[test]
fn a_library_and_host_inputs_reach_the_script() {
    let libraries = [Library {
        file: "shelf.splash".into(),
        source: "let m = mod.model\nfn shelf(w) { return m.box({size: [w 2mm 10mm]}) }\nlet lib = {shelf: shelf}\nlib".into(),
    }];
    let inputs = Inputs {
        kinds: vec!["board".into()],
        values: [
            (("board".into(), "b1".into()), Input { source: "{width: 30mm}".into(), hash: [1; 16] }),
            (("board".into(), "b2".into()), Input { source: "{width: 40mm}".into(), hash: [2; 16] }),
        ].into(),
        lazy: None,
    };
    let source = "use mod.fab.*\nlet b = board(\"b1\")\nlet s = shelf(b.width)\nplace(s)";
    let mut options = Options { generated: &Default::default(), assets: &Assets::new(), images: &Default::default(), extension: "fab", libraries: &libraries, inputs: &inputs, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: HashMap::new() };
    let p = evaluate(source, &options).expect("a library function accepts a host input");
    assert_eq!(p.roots.len(), 1, "the script places one root");
    assert_eq!(p.solids.len(), 1, "the library builds one solid");
    let Some(Arg::Solid(solid)) = p.roots[0].get("solid") else { panic!("the root places the library's solid") };
    let Op::Box { size, .. } = &p.graph.node(p.solids[*solid].node).op else { panic!("the library builds a box") };
    assert!((size[0] - 30.0).abs() < 1e-9, "the host width reaches the box in millimetres");
    assert_eq!(p.inputs_read, vec![("board".to_string(), "b1".to_string())], "only the requested input is recorded");
    let repeated = evaluate("use mod.fab.*\nboard(\"b2\")\nboard(\"b1\")\nboard(\"b2\")", &options).expect("repeated input reads evaluate");
    assert_eq!(repeated.inputs_read, vec![("board".to_string(), "b2".to_string()), ("board".to_string(), "b1".to_string())], "reads keep first-read order without duplicates");
    let errors = evaluate("use mod.fab.*\nboard(\"zz\")", &options).err().expect("an unknown input fails");
    assert!(errors.iter().any(|d| d.message.contains("no board named 'zz'") && d.message.contains("b1, b2")), "unknown inputs list the available names: {errors:?}");
    let errors = evaluate("use mod.fab.*\nboard(12)", &options).err().expect("an input name must be text");
    assert!(errors.iter().any(|d| d.message.contains("board(\"name\"): the name is text")), "non-text input names explain the argument: {errors:?}");
    let broken = [Library { file: "shelf.splash".into(), source: "let m = mod.model\nfn shelf(w) { return missing }\nlet lib = {shelf: shelf}\nlib".into() }];
    options.libraries = &broken;
    let errors = evaluate(source, &options).err().expect("a library function's error fails the script");
    assert!(errors.iter().any(|d| d.line == 1 && d.col == 1 && d.message.contains("shelf.splash:2")), "library errors retain their own file and line: {errors:?}");
    let invalid = [Library { file: "shelf.splash".into(), source: "42".into() }];
    options.libraries = &invalid;
    let errors = evaluate("", &options).err().expect("a library must export an object");
    assert!(errors.iter().any(|d| d.message == "library shelf.splash must end with an object of its fns"), "non-object libraries explain the export contract: {errors:?}");
    options.libraries = &libraries;
    let mut broken_inputs = inputs.clone();
    broken_inputs.values.get_mut(&("board".into(), "b1".into())).unwrap().source = "{width: missing}".into();
    options.inputs = &broken_inputs;
    let errors = evaluate(source, &options).err().expect("an input expression's error fails the script");
    assert!(errors.iter().any(|d| d.line == 1 && d.col == 1 && d.message.contains("board:b1:1:")), "input errors retain their own file and line: {errors:?}");

    // A lazy kind: registering builds nothing, a script's first read of a name builds it (once
    // however often it reads it), and a name nobody has lists the given and the lazy names.
    struct Shelves(std::sync::Mutex<Vec<String>>);
    impl Provide for Shelves {
        fn provide(&self, kind: &str, name: &str) -> Option<Input> {
            (kind == "board" && name == "lazy").then(|| {
                self.0.lock().unwrap().push(name.to_string());
                Input { source: "{width: 50mm}".into(), hash: [5; 16] }
            })
        }
        fn names(&self, _kind: &str) -> Vec<String> {
            vec!["lazy".into()]
        }
        fn hash(&self, _kind: &str, name: &str) -> Option<[u8; 16]> {
            self.0.lock().unwrap().iter().any(|n| n == name).then_some([5; 16])
        }
    }
    let shelves = std::sync::Arc::new(Shelves(Default::default()));
    let lazy = Inputs { lazy: Some(Lazy::new(shelves.clone())), ..inputs.clone() };
    options.inputs = &lazy;
    evaluate("use mod.fab.*\nboard(\"b1\")", &options).expect("a given input reads beside a lazy kind");
    assert!(shelves.0.lock().unwrap().is_empty(), "a script that reads no lazy name builds none");
    let p = evaluate("use mod.fab.*\nlet b = board(\"lazy\")\nlet s = shelf(board(\"lazy\").width)\nplace(s)", &options).expect("a lazy input builds at its first read");
    let Some(Arg::Solid(solid)) = p.roots[0].get("solid") else { panic!("the root places the library's solid") };
    let Op::Box { size, .. } = &p.graph.node(p.solids[*solid].node).op else { panic!("the library builds a box") };
    assert!((size[0] - 50.0).abs() < 1e-9, "the lazy width reaches the box in millimetres");
    assert_eq!(*shelves.0.lock().unwrap(), vec!["lazy".to_string()], "two reads of a lazy name build it once");
    assert_eq!(p.inputs_read, vec![("board".to_string(), "lazy".to_string())], "a lazy read is recorded like a given one");
    let errors = evaluate("use mod.fab.*\nboard(\"zz\")", &options).err().expect("a name nobody has fails");
    assert!(errors.iter().any(|d| d.message.contains("there is: b1, b2, lazy")), "a missing name lists the given and the lazy names: {errors:?}");
    assert_eq!(lazy.stamp(&("board".into(), "b1".into())), Some([1; 16]), "a given value stamps with its hash");
    assert_eq!(lazy.stamp(&("board".into(), "lazy".into())), Some([5; 16]), "a built lazy value stamps with its hash, as a given one would");
    let unbuilt = ("board".to_string(), "later".to_string());
    let other = Inputs { lazy: Some(Lazy::new(shelves.clone())), ..inputs.clone() };
    assert!(lazy.stamp(&unbuilt).is_some() && other.stamp(&unbuilt) != lazy.stamp(&unbuilt), "a value not built yet stamps with its provider, each provider apart");
    assert_eq!(lazy.stamp(&("other".into(), "lazy".into())), None, "a kind nobody registered has no stamp");
}

#[test]
fn input_records_key_build_and_refuse() {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct Boards(Mutex<BTreeMap<String, (usize, Input, RecordValue)>>);
    impl Provide for Boards {
        fn provide(&self, _kind: &str, _name: &str) -> Option<Input> {
            None
        }
        fn provide_with(&self, kind: &str, name: &str, record: &RecordValue) -> Result<Option<Input>, Refusal> {
            if kind != "board" || name != "lazy" {
                return Ok(None);
            }
            let RecordValue::Rec(fields) = record else { panic!("input options reach the provider as a record") };
            let Some(RecordValue::Num(height)) = fields.get("height") else {
                return Err(Refusal { key: "height".into(), message: "must be a number".into() });
            };
            if *height < 0.0 {
                return Err(Refusal { key: "height".into(), message: "must not be negative".into() });
            }
            let mut hash = [0; 16];
            hash[..8].copy_from_slice(&height.to_le_bytes());
            let input = Input { source: format!("{{width: {}mm}}", height * 1000.0 * 2.0).into(), hash };
            let mut built = self.0.lock().unwrap();
            let entry = built.entry(input_key(name, Some(record))).or_insert_with(|| (0, input.clone(), record.clone()));
            entry.0 += 1;
            Ok(Some(entry.1.clone()))
        }
        fn names(&self, _kind: &str) -> Vec<String> {
            vec!["lazy".into()]
        }
        fn hash(&self, _kind: &str, name: &str) -> Option<[u8; 16]> {
            self.0.lock().unwrap().get(name).map(|(_, input, _)| input.hash)
        }
    }
    let boards = Arc::new(Boards(Default::default()));
    let inputs = Inputs { kinds: vec!["board".into()], values: BTreeMap::new(), lazy: Some(Lazy::new(boards.clone())) };
    let mut options = Options { generated: &Default::default(), assets: &Assets::new(), images: &Default::default(), extension: "fab", libraries: &[], inputs: &inputs, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: HashMap::new() };
    let height_record = |height| RecordValue::Rec([("height".into(), RecordValue::Num(height))].into());
    let key10 = input_key("lazy", Some(&height_record(0.010)));
    let key12 = input_key("lazy", Some(&height_record(0.012)));
    let p = evaluate("use mod.fab.*\nlet m = mod.model\nlet a = board(\"lazy\", {height: 10mm})\nlet b = board(\"lazy\", {height: 10mm})\nlet c = board(\"lazy\", {height: 12mm})\nboard(\"lazy\", {height: 10mm})\nplace(m.box({size: [a.width 2mm 10mm]}))\nplace(m.box({size: [b.width 2mm 10mm]}))\nplace(m.box({size: [c.width 2mm 10mm]}))", &options).expect("records build distinct inputs on demand");
    assert_eq!(p.roots.len(), 3, "each returned width reaches a placed box");
    for (root, width) in p.roots.iter().zip([20.0, 20.0, 24.0]) {
        let Some(Arg::Solid(solid)) = root.get("solid") else { panic!("the root places a box") };
        let Op::Box { size, .. } = &p.graph.node(p.solids[*solid].node).op else { panic!("the input builds a box") };
        assert!((size[0] - width).abs() < 1e-9, "the record determines the width: expected {width}, got {}", size[0]);
    }
    assert_eq!(p.inputs_read, vec![("board".into(), key10.clone()), ("board".into(), key12.clone())], "reads keep keyed names in first-read order without duplicates");
    let hash10 = {
        let built = boards.0.lock().unwrap();
        assert_eq!(built.len(), 2, "two records build two values");
        assert_eq!((built[&key10].0, built[&key12].0), (1, 1), "repeated reads never rebuild an input");
        built[&key10].1.hash
    };
    assert_eq!(inputs.stamp(&("board".into(), key10.clone())), Some(hash10), "record reads stamp with their keyed value's hash");

    let mm = height_record(10.9 * 0.001);
    let cm = height_record(1.09 * 0.01);
    assert_eq!(mm.canonical(), cm.canonical(), "equivalent lengths have canonical text independent of unit rounding");
    assert_eq!(mm.canonical(), "{height: 1.09000000000e-2}");
    assert_eq!(RecordValue::Num(-0.0).canonical(), RecordValue::Num(0.0).canonical(), "signed zero is one key");
    let key109 = input_key("lazy", Some(&mm));
    let p = evaluate("use mod.fab.*\nboard(\"lazy\", {height: 10.9mm})\nboard(\"lazy\", {height: 1.09cm})", &options).expect("equivalent units share an input");
    assert_eq!(p.inputs_read, vec![("board".into(), key109.clone())]);
    {
        let built = boards.0.lock().unwrap();
        assert_eq!(built.len(), 3, "equivalent units add only one value");
        assert_eq!(built[&key109].0, 1, "equivalent units call the provider once");
    }

    let errors = evaluate("use mod.fab.*\n\nboard(\"lazy\", {height: -1mm})", &options).err().expect("a provider can refuse a record field");
    assert!(errors.iter().any(|d| d.line == 3 && d.message.contains("'height'") && d.message.contains("must not be negative")), "a provider refusal points at the call's line: {errors:?}");

    let p = evaluate("use mod.fab.*\nboard(\"lazy\", {usb: {kind: @micro height: 7.9mm} dc: {present: false} labels: [\"port\" \"long label\" true 2 @micro] height: 10mm})", &options).expect("nested records and lists reach the provider");
    let nested = RecordValue::Rec([
        ("height".into(), RecordValue::Num(0.010)),
        ("usb".into(), RecordValue::Rec([("kind".into(), RecordValue::Id("micro".into())), ("height".into(), RecordValue::Num(0.0079))].into())),
        ("dc".into(), RecordValue::Rec([("present".into(), RecordValue::Bool(false))].into())),
        ("labels".into(), RecordValue::List(vec![RecordValue::Text("port".into()), RecordValue::Text("long label".into()), RecordValue::Bool(true), RecordValue::Num(2.0), RecordValue::Id("micro".into())])),
    ].into());
    let nested_key = input_key("lazy", Some(&nested));
    assert_eq!(p.inputs_read, vec![("board".into(), nested_key.clone())], "nested keys are sorted and values retain their types");
    assert_eq!(boards.0.lock().unwrap()[&nested_key].2, nested, "the provider receives plain nested values");
    for invalid in ["nil", "fn() { return 1 }", "mod.model.box({size: [1mm 1mm 1mm]})"] {
        let source = format!("use mod.fab.*\n\nboard(\"lazy\", {{usb: {{height: {invalid}}}}})");
        let errors = evaluate(&source, &options).err().expect("record fields must be plain values");
        assert!(errors.iter().any(|d| d.line == 3 && d.message.contains("'usb.height' is not a number, id, text, flag, list or record")), "invalid nested fields name their dotted path: {errors:?}");
    }
    let errors = evaluate("use mod.fab.*\nboard(\"lazy\", 10)", &options).err().expect("options must be a record");
    assert!(errors.iter().any(|d| d.message.contains("board(\"lazy\", {..}): the options are a record")), "non-record options explain the argument: {errors:?}");
    let errors = evaluate("use mod.fab.*\nboard(\"missing\", {height: 10mm})", &options).err().expect("an unknown name still fails");
    assert!(errors.iter().any(|d| d.message.contains("no board named 'missing'")), "a provider's None uses the existing missing-name error: {errors:?}");

    assert_eq!(input_key("lazy", None), "lazy", "plain calls keep their original keys");
    struct Plain;
    impl Provide for Plain {
        fn provide(&self, _kind: &str, _name: &str) -> Option<Input> { None }
        fn names(&self, _kind: &str) -> Vec<String> { vec![] }
        fn hash(&self, _kind: &str, _name: &str) -> Option<[u8; 16]> { None }
    }
    let plain = Inputs { kinds: vec!["board".into()], values: BTreeMap::new(), lazy: Some(Lazy::new(Arc::new(Plain))) };
    options.inputs = &plain;
    let errors = evaluate("use mod.fab.*\n\nboard(\"lazy\", {})", &options).err().expect("providers opt in to records");
    assert!(errors.iter().any(|d| d.line == 3 && d.message.contains("board(\"lazy\", {..}): board takes no options")), "the default refusal has no empty quoted key: {errors:?}");
}

#[test]
fn prior_names_are_shared_only_for_identical_source() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Names>();
    send_sync::<makepad_script::syntax::Syntax>();

    let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet h = param(20mm, min: 5mm, max: 40mm)\nlet box_a = m.box({size: [10mm h 10mm]})\nplace(box_a)\n";
    let assets = Assets::new();
    let mut opts = Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &NO_INPUTS, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: HashMap::new() };
    let first = evaluate(source, &opts).unwrap();
    let loc = first.params[0].call.loc.unwrap();
    opts.overrides.insert(loc, 0.03);
    let same = evaluate_with_names(&source.to_owned(), &opts, Some(first.names.clone())).unwrap();
    assert!(Arc::ptr_eq(&first.names, &same.names), "value changes share the parsed source");
    assert_ne!(first.params[0].call.value, same.params[0].call.value, "sharing names must still evaluate new values");

    let renamed = source.replace("box_a", "box_b");
    assert_eq!(source.len(), renamed.len());
    let changed = evaluate_with_names(&renamed, &opts, Some(first.names.clone())).unwrap();
    assert!(!Arc::ptr_eq(&first.names, &changed.names), "equal lengths do not imply equal source");
    assert_eq!(changed.names.syntax.source(), renamed);
    assert_eq!(ids(&changed), vec!["box_b"], "changed bindings must not retain stale names");
    assert!(evaluate_with_names("let =", &opts, Some(first.names.clone())).is_err(), "cached names must not hide a parse error");
}

fn ids(p: &Program) -> Vec<&str> {
    p.solids.iter().map(|s| s.id.as_str()).collect()
}

/// A Splash field is a compiled kernel: its distances are the fn's.
#[test]
fn sdf_fields_are_compiled_splash_fns() {
    let src = "use mod.fab.*\nlet m = mod.model\nlet r = 10mm\nlet s = m.sdf({bounds: [[-11mm, -11mm, -11mm], [11mm, 11mm, 11mm]] field: fn(p) { return smooth_union(sdf_sphere(p, r), sdf_box(p - vec3(5mm, 0, 0), vec3(8mm, 4mm, 4mm)), 2mm) }})\nplace(s)\n";
    let p = eval(src).unwrap();
    let Op::Field { shape, .. } = &p.graph.node(p.solids[0].node).op else { panic!("not a field") };
    assert_eq!(p.material_filaments[&p.solids[0].node], vec![None]);
    let f = shape.field();
    // Kernel points are millimetres: the sphere's centre is 10 inside.
    assert!((f.distance(makepad_csg_math::Vec3d::new(0.0, 0.0, 0.0)) + 10.0).abs() < 1e-3);
    assert!(f.distance(makepad_csg_math::Vec3d::new(30.0, 0.0, 0.0)) > 10.0);
}

/// Parts are named by their bindings: a list's by index, a fn's by the call's binding first,
/// and an inner op of one binding by its builder.
#[test]
fn parts_are_named_by_their_bindings() {
    let src = "use mod.fab.*\nlet m = mod.model\nfn leg(x) {\n    let body = m.box({size: [4mm 20mm 4mm] at: [x 0 0]})\n    return m.fillet(body, 1mm)\n}\nlet left = leg(-10mm)\nlet right = leg(10mm)\nlet bolts = for i in 0..3 { m.cylinder({radius: 1mm height: 8mm at: [i * 5mm 0 0]}) }\nlet top = m.box({size: [30mm 2mm 10mm]})\nlet frame = m.subtract(m.union(top, [left right]), bolts)\nplace(frame)\n";
    let p = eval(src).unwrap();
    assert_eq!(ids(&p), vec!["left.body", "left", "right.body", "right", "bolts[0]", "bolts[1]", "bolts[2]", "top", "frame.union", "frame"]);
    let frame = p.solid("frame").unwrap();
    assert!(frame.named && !p.solid("left.body").unwrap().named);
    // A call that gives a list of parts names each by its place in it.
    let split = eval("use mod.fab.*\nlet m = mod.model\nlet top = m.box({size: [30mm 10mm 10mm]})\nlet halves = m.split(top, {center: [0 0 0] dowels: true})\nplace(halves[0])\nplace(halves[1])\n").unwrap();
    assert_eq!(ids(&split), vec!["top", "halves[0]", "halves[1]", "halves[2]"]);
    // Geometry is not a value: an operator on parts makes no part.
    let ops = eval("use mod.fab.*\nlet m = mod.model\nlet a = m.box({size: [1mm 1mm 1mm]})\nplace(a - a)\n");
    assert!(ops.map_or(true, |p| !p.roots[0].args.iter().any(|(_, a)| matches!(a, Arg::Solid(_)))));
    // An unknown name is an ordinary error.
    assert!(eval("use mod.fab.*\nlet m = mod.model\nplace(m.subtract(nothing, []))\n").is_err());
}

const EDIT_SRC: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet h = param(20mm, min: 5mm, max: 40mm)\nlet a = m.box({size: [10mm h 10mm] radius: 1mm})\nlet b = m.cylinder({radius: 2mm height: 30mm})\nlet c = m.subtract(a, b)\nlet d = m.fillet(c, 0.5mm)\nplace(d, at: [10mm, 20mm])\n";

fn edited(edit: edit::Edit) -> Result<String, String> {
    let p = eval(EDIT_SRC).unwrap();
    let out = edit::apply(&p, &edit)?;
    // Every edit leaves a program that still evaluates.
    eval(&out).map_err(|d| format!("{d:?}"))?;
    Ok(out)
}

#[test]
fn edits_splice_exactly_the_bytes_they_change() {
    use edit::Edit;
    let set = |site: &str, value: &str| edited(Edit::Set { site: site.into(), value: value.into() });
    assert_eq!(set("h", "25mm").unwrap(), EDIT_SRC.replace("param(20mm", "param(25mm"));
    assert_eq!(set("b.radius", "2.5mm").unwrap(), EDIT_SRC.replace("radius: 2mm", "radius: 2.5mm"));
    // A call's own argument is a site.
    assert_eq!(set("d.radius", "0.8mm").unwrap(), EDIT_SRC.replace("c, 0.5mm", "c, 0.8mm"));
    // A field written as a param's name edits the param.
    assert_eq!(set("a.size.1", "30mm").unwrap(), EDIT_SRC.replace("param(20mm", "param(30mm"));
    assert_eq!(set("a.size.0", "12mm").unwrap(), EDIT_SRC.replace("[10mm h", "[12mm h"));
    let field = edited(Edit::Field { solid: "b".into(), field: "segments".into(), code: "64".into() }).unwrap();
    assert!(field.contains("height: 30mm segments: 64}") || field.contains("height: 30mm, segments: 64}"), "{field}");
    let opts = edited(Edit::Field { solid: "d".into(), field: "edges".into(), code: "{top: true}".into() }).unwrap();
    assert!(opts.contains("m.fillet(c, 0.5mm, {edges: {top: true}})"), "{opts}");
    let add = edited(Edit::Add { code: "let lid = m.box({size: [10mm 2mm 10mm]})".into(), before: None }).unwrap();
    assert!(add.contains("let lid = m.box({size: [10mm 2mm 10mm]})\nplace(d"), "{add}");
    // A cutter's delete leaves the subtract with nothing to do: its base, under its name.
    let removed = edited(Edit::Remove { solid: "b".into() }).unwrap();
    assert_eq!(removed, EDIT_SRC.replace("let a = m.box", "let c = m.box").replace("let b = m.cylinder({radius: 2mm height: 30mm})\nlet c = m.subtract(a, b)\n", ""));
    let renamed = edited(Edit::Rename { solid: "b".into(), to: "bore".into() }).unwrap();
    assert!(renamed.contains("let bore = m.cylinder") && renamed.contains("m.subtract(a, bore)"), "{renamed}");
    let at = edited(Edit::RootArg { kind: "place".into(), solid: "d".into(), name: "at".into(), code: Some("[30mm, 40mm]".into()) }).unwrap();
    assert!(at.ends_with("place(d, at: [30mm, 40mm])\n"), "{at}");
    let fil = edited(Edit::RootArg { kind: "place".into(), solid: "d".into(), name: "filament".into(), code: Some("2".into()) }).unwrap();
    assert!(fil.contains("place(d, at: [10mm, 20mm]") && fil.contains("filament: 2"), "{fil}");
    let unplaced = edited(Edit::Root { kind: "place".into(), solid: "d".into(), add: false });
    // A program that places nothing still evaluates here (the host decides).
    assert!(!unplaced.unwrap().contains("place("));
    // A part argument is a site too: it picks another part.
    assert_eq!(set("c.a", "b").unwrap(), EDIT_SRC.replace("m.subtract(a, b)", "m.subtract(b, b)"));
    // A vector with a param in it is computed: the refusal names the param and how to set it.
    let refused = set("a.size", "[1mm 2mm 3mm]").unwrap_err();
    assert!(refused.contains("element 1 is `h`") && refused.contains("set(h, ..)") && refused.contains("set(a.size.0, ..)") && refused.contains("field(a, size, ..)"), "{refused}");
}

/// The distances of solid `id`'s field (mm) at `ps`.
fn field_at(p: &Program, id: &str, ps: &[[f64; 3]]) -> Vec<f64> {
    use makepad_csg_math::Vec3d;
    let Op::Field { shape, .. } = &p.graph.node(p.solid(id).unwrap().node).op else { panic!("'{id}' is not a field") };
    let ps: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
    let mut out = vec![0.0; ps.len()];
    shape.field().distance_many(&ps, &mut out);
    out
}

/// `rotate: {to: [x y z]}` turns a part's Y onto that direction the shortest way, as a mesh
/// part's placement and inside a field; `from` picks another axis of its own, `spin` turns it
/// about the direction after; a zero direction or another key is refused.
#[test]
fn rotate_to_lines_up_an_axis() {
    let head = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let p = eval(&format!("{head}let e = m.ellipsoid({{radii: [2mm 10mm 3mm] rotate: {{to: [1 1 0]}}}})\nlet v = m.blend([e], 0mm)\nlet k = m.box({{size: [1mm 1mm 1mm] rotate: {{to: [0 0 1] from: [1 0 0] spin: 90°}}}})\nplace(v)\nplace(k)\n")).unwrap();
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let turn = |id: &str, v: [f64; 3]| -> [f64; 3] {
        let (_, rotate, _) = match &p.graph.node(p.solid(id).unwrap().node).op {
            Op::Transform { at, rotate, scale, .. } => (*at, *rotate, *scale),
            other => panic!("'{id}' is not placed: {other:?}"),
        };
        let r = makepad_csg_graph::eval::rotation_xyz(rotate);
        std::array::from_fn(|i| (0..3).map(|j| r[i][j] * v[j]).sum())
    };
    let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9);
    assert!(near(turn("e", [0.0, 1.0, 0.0]), [s, s, 0.0]), "Y onto the direction: {:?}", turn("e", [0.0, 1.0, 0.0]));
    assert!(near(turn("e", [0.0, 0.0, 1.0]), [0.0, 0.0, 1.0]), "the shortest way: Z, across both, stays");
    assert!(near(turn("k", [1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]) && near(turn("k", [0.0, 1.0, 0.0]), [-1.0, 0.0, 0.0]), "from X onto Z, then a quarter turn about Z");
    // inside the field: the long radius along (1, 1, 0), the short one across it, Z's as it was
    let d = field_at(&p, "v", &[[10.0 * s, 10.0 * s, 0.0], [2.0 * s, -2.0 * s, 0.0], [0.0, 0.0, 3.0], [11.0 * s, 11.0 * s, 0.0]]);
    assert!(d[0].abs() < 1e-6 && d[1].abs() < 1e-6 && d[2].abs() < 1e-6 && d[3] > 0.5, "the turned ellipsoid's surface: {d:?}");
    for (bad, says) in [("{to: [0 0 0]}", "not zero"), ("{toward: [0 1 0]}", "is [x y z] angles or {to:")] {
        let err = eval(&format!("{head}let e = m.box({{size: [1mm 1mm 1mm] rotate: {bad}}})\nplace(e)\n")).err().unwrap_or_else(|| panic!("{bad} is taken"));
        assert!(err.iter().any(|d| d.message.contains(says)), "{bad}: {err:?}");
    }
}

/// A negative item after another in an array: `[0 0 -30°]` reads as `0` and `0 - 30°`, two
/// items, and `[0 0 (-30°)]` as `0` and a call of `0`; commas keep it an item of its own.
#[test]
fn a_negative_array_item_needs_commas() {
    let head = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let turned = |rotate: &str| -> Result<[f64; 3], String> {
        let p = eval(&format!("{head}let k = m.box({{size: [1mm 1mm 1mm] rotate: {rotate}}})\nplace(k)\n")).map_err(|d| format!("{d:?}"))?;
        match &p.graph.node(p.solid("k").unwrap().node).op {
            Op::Transform { rotate, .. } => Ok(*rotate),
            other => panic!("not placed: {other:?}"),
        }
    };
    let r = turned("[0, 0, -30°]").unwrap();
    assert!(r[0].abs() < 1e-9 && r[1].abs() < 1e-9 && (r[2] + 30.0).abs() < 1e-9, "{r:?}");
    for (written, says) in [("[0 0 -30°]", "'rotate'"), ("[0 0 (-30°)]", "not a function")] {
        let err = turned(written).unwrap_err();
        assert!(err.contains(says), "{written}: {err}");
    }
}

/// The parser warns at a `-` that reads like a negative item (`[10mm 10mm 12mm -2mm]` is three
/// items, the last `12mm - 2mm`): the model builds, the warning is one of its notes, where it is.
#[test]
fn a_minus_inside_brackets_builds_with_a_warning() {
    let head = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let p = eval(&format!("{head}let k = m.box({{size: [10mm 10mm 12mm -2mm]}})\nplace(k)\n")).unwrap();
    let says = "`12mm -2mm` inside [ ] reads as one value, 12mm - 2mm: write `12mm - 2mm`, or `12mm, -2mm` for two items";
    let note = p.notes.iter().find(|n| n.message == says).unwrap_or_else(|| panic!("{:?}", p.notes));
    assert_eq!(note.line, 4, "{note:?}");
    let written = eval(&format!("{head}let k = m.box({{size: [10mm, 10mm, 10mm]}})\nplace(k)\n")).unwrap();
    assert!(written.notes.is_empty(), "{:?}", written.notes);
}

/// The examples write commas where an item is negative: each array has the items it shows, and
/// the parser has nothing to warn of in any example.
#[test]
fn examples_parse_to_the_items_they_show() {
    use makepad_script::syntax::{self, Kind};
    for p in protos::PROTOS {
        assert!(makepad_script::parser::parse_warnings(p.example).is_empty(), "{}: {:?}", p.name, makepad_script::parser::parse_warnings(p.example));
    }
    let counts = |name: &str| {
        let example = protos::PROTOS.iter().find(|p| p.name == name).unwrap().example;
        let tree = syntax::parse(example).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let mut n = Vec::new();
        tree.walk(&mut |_, node| {
            if let Kind::Array { items, .. } = &node.kind {
                n.push(items.len());
            }
        });
        n
    };
    assert_eq!(counts("BrepSweep"), [5, 3, 3, 3, 3, 3, 5]);
    assert_eq!(counts("Tube"), [4, 4, 4, 4, 4]);
    assert_eq!(counts("Sdf"), [2, 3, 3]);
    assert_eq!(counts("Sketch"), [2, 2, 2]);
    assert_eq!(counts("Face"), [3, 3]);
}

/// An added statement goes where it works, read from the syntax alone: a `use` with the
/// imports (first when there are none), a `fn` or `let` before what reads it, `before` before
/// that name; and definitions a later edit reads earlier move up to it, with what they read.
#[test]
fn added_statements_go_before_their_use() {
    let src = "let m = mod.model\nlet a = m.box({size: [k(1) 2mm 2mm]})\nobject(a)\n";
    let roots = ["object"];
    assert_eq!(edit::add(src, "fn k(x) { x * 1mm }", None, &roots).unwrap(), "let m = mod.model\nfn k(x) { x * 1mm }\nlet a = m.box({size: [k(1) 2mm 2mm]})\nobject(a)\n");
    assert!(edit::add(src, "use mod.math.*", None, &roots).unwrap().starts_with("use mod.math.*\nlet m"));
    let with_use = "use mod.model.*\nlet m = mod.model\nobject(a)\n";
    assert!(edit::add(with_use, "use mod.math.*", Some("m"), &roots).unwrap().starts_with("use mod.model.*\nuse mod.math.*\nlet m"));
    assert!(edit::add(src, "let w = 2mm", None, &roots).unwrap().contains("let w = 2mm\nobject(a)"), "nothing reads it: before the object");
    assert!(edit::add(src, "let w = 2mm", Some("a"), &roots).unwrap().contains("let w = 2mm\nlet a"));
    assert!(edit::add(src, "let w = 2mm", Some("b"), &roots).unwrap_err().contains("no statement `let b` or `fn b`"));
    let late = "let a = m.box({size: [k(1) 2mm 2mm]})\nobject(a)\nfn j(x) { x }\nfn k(x) { j(x) * 1mm }\n";
    let placed = edit::place_defs(late, &["k".into(), "j".into()]).unwrap();
    assert_eq!(placed, "fn j(x) { x }\nfn k(x) { j(x) * 1mm }\nlet a = m.box({size: [k(1) 2mm 2mm]})\nobject(a)\n");
    assert_eq!(edit::place_defs(&placed, &["k".into(), "j".into()]).unwrap(), placed, "in place: nothing moves");
}

/// m.capsule is the exact capsule between its ends, on its own and blended; a radius of 0 is
/// refused.
#[test]
fn capsule_node_is_the_exact_capsule() {
    let head = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let p = eval(&format!("{head}let c = m.capsule({{a: [0 10mm 0] b: [12mm 30mm 4mm] radius: 3mm}})\nlet v = m.blend([c m.sphere({{radius: 2mm at: [0, -50mm, 0]}})], 1mm)\nplace(c)\nplace(v)\n")).unwrap();
    let (a, b, r) = ([0.0, 10.0, 0.0], [12.0, 30.0, 4.0], 3.0);
    let exact = |q: [f64; 3]| {
        let (ab, aq): ([f64; 3], [f64; 3]) = (std::array::from_fn(|k| b[k] - a[k]), std::array::from_fn(|k| q[k] - a[k]));
        let t = ((0..3).map(|k| aq[k] * ab[k]).sum::<f64>() / (0..3).map(|k| ab[k] * ab[k]).sum::<f64>()).clamp(0.0, 1.0);
        (0..3).map(|k| (aq[k] - t * ab[k]).powi(2)).sum::<f64>().sqrt() - r
    };
    let ps: Vec<[f64; 3]> = (0..64).map(|i| [(i % 4) as f64 * 7.0 - 8.0, (i / 4 % 4) as f64 * 11.0 - 2.0, (i / 16) as f64 * 5.0 - 6.0]).collect();
    for id in ["c", "v"] {
        for (q, d) in ps.iter().zip(field_at(&p, id, &ps)) {
            assert!((d - exact(*q)).abs() < 1e-9, "{id} at {q:?}: {d} against {}", exact(*q));
        }
    }
    let err = eval(&format!("{head}let c = m.capsule({{a: [0 0 0] b: [0 5mm 0] radius: 0mm}})\nplace(c)\n")).err().unwrap();
    assert!(err.iter().any(|d| d.message.contains("'radius' must be more than 0")), "{err:?}");
}

/// set() takes a vector written as a literal whole, and one element of it, of a named part and of
/// a call inside another; a value computed from others is refused, naming what computes it.
#[test]
fn set_takes_vectors_whole_and_by_element() {
    use edit::Edit;
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet h = param(4mm)\nlet lift = h * 2\nlet cut = m.box({size: [10mm 8mm 10mm] at: [0, -4mm, 0]})\nlet body = m.blend([m.ellipsoid({radii: [6mm 4mm 5mm]}) m.sphere({radius: lift + 1mm at: [0 5mm 0]})], 1mm)\nlet part = m.carve(body, [cut], 0.5mm)\nplace(part)\n";
    let p = eval(src).unwrap();
    let set = |site: &str, value: &str| -> Result<String, String> {
        let out = edit::apply(&p, &Edit::Set { site: site.into(), value: value.into() })?;
        eval(&out).map_err(|d| format!("{d:?}"))?;
        Ok(out)
    };
    assert_eq!(set("cut.size", "[12mm 6mm 12mm]").unwrap(), src.replace("[10mm 8mm 10mm]", "[12mm 6mm 12mm]"), "a vector whole");
    assert_eq!(set("cut.at", "[1mm, -3mm, 0]").unwrap(), src.replace("[0, -4mm, 0]", "[1mm, -3mm, 0]"), "a vector with a negative element whole");
    assert_eq!(set("body.ellipsoid.radii", "[7mm 4mm 5mm]").unwrap(), src.replace("[6mm 4mm 5mm]", "[7mm 4mm 5mm]"), "an inner call's vector whole");
    assert_eq!(set("body.ellipsoid.radii.1", "4.4mm").unwrap(), src.replace("[6mm 4mm 5mm]", "[6mm 4.4mm 5mm]"), "an inner call's element");
    assert_eq!(set("body.sphere.at.1", "6mm").unwrap(), src.replace("[0 5mm 0]", "[0 6mm 0]"));
    let refused = set("body.sphere.radius", "3mm").unwrap_err();
    assert!(refused.contains("computed by `lift + 1mm`") && refused.contains("`let lift` on line 5") && refused.contains("field(body.sphere, radius, ..)"), "{refused}");
}

/// Joints are not solids: they name two parts, positionally or in the options object.
#[test]
fn joint_calls_name_their_parts() {
    let head = "use mod.fab.*\nlet m = mod.model\nlet a = m.box({size: [12mm 6mm 5mm] at: [6mm 3mm 0]})\nlet b = m.box({size: [12mm 6mm 5mm] at: [18mm 3mm 0]})\n";
    for call in ["let j = m.hinge(a, b, {at: [12mm 0 0]})", "let j = m.hinge(a, b, {at: [12mm, 0, 0], upper: 90°})"] {
        let p = eval(&format!("{head}{call}\n")).unwrap_or_else(|e| panic!("{call}: {e:?}"));
        assert_eq!(p.joints.len(), 1, "{call}");
        let j = &p.joints[0];
        assert_eq!((j.proto, j.a, j.b, j.id.as_str()), ("Hinge", 0, Some(1), "j"), "{call}");
        assert_eq!(j.at, Some([12.0, 0.0, 0.0]));
        assert_eq!(ids(&p), vec!["a", "b"], "a joint is not a solid");
    }
    let p = eval(&format!("{head}let links = [a b]\nlet c = m.chain(links, {{joint: @ball cone: 30°}})\nm.anchor(a, {{at: [2mm 3mm 0] fixed: true}})\n")).unwrap();
    assert_eq!(p.joints.len(), 2);
    assert_eq!((p.joints[0].proto, p.joints[0].id.as_str(), p.joints[0].at), ("BallJoint", "c[0]", None));
    assert!((p.joints[0].value("cone").unwrap() - 30f64.to_radians()).abs() < 1e-12);
    assert_eq!((p.joints[1].proto, p.joints[1].value("fixed")), ("Anchor", Some(1.0)));
    // a bond: one call per part it bonds, in order, `a` the part they are bonded to
    let p = eval(&format!("{head}let s = m.box({{size: [30mm 2mm 2mm] at: [12mm 3mm 0]}})\nlet back = m.bond(s, [a b])\n")).unwrap();
    assert_eq!(p.joints.iter().map(|j| (j.proto, j.id.as_str(), j.a, j.b)).collect::<Vec<_>>(), [("Bond", "back", 2, Some(0)), ("Bond", "back", 2, Some(1))]);
    assert!(eval(&format!("{head}let back = m.bond(a, [a b])\n")).is_err(), "a part is not bonded to itself");
    // a string: its two parts, its two holes and its length; a hand-held anchor
    let p = eval(&format!("{head}let s = m.string(a, b, {{from: [6mm 3mm 0] to: [18mm 3mm 0] length: 40mm}})\nm.anchor(a, {{at: [6mm 3mm 0] held: true}})\n")).unwrap();
    let s = &p.joints[0];
    assert_eq!((s.proto, s.id.as_str(), s.a, s.b, s.at, s.to, s.value("length")), ("String", "s", 0, Some(1), Some([6.0, 3.0, 0.0]), Some([18.0, 3.0, 0.0]), Some(40.0)));
    assert_eq!((p.joints[1].proto, p.joints[1].value("held")), ("Anchor", Some(1.0)));
    assert_eq!(ids(&p), vec!["a", "b"], "a string is not a solid");
    assert!(eval(&format!("{head}m.string(a, a, {{from: [6mm 3mm 0] to: [8mm 3mm 0] length: 40mm}})\n")).is_err(), "a string runs between two parts");
}

/// m.voronoi on a 40mm cube: its skin is struts and holes, all of it inside the cube; inside, a
/// foam of struts, solid, or nothing, as asked; the field never changes faster than distance;
/// pointed hole tops leave more strut on an upright face than round ones; randomness past 0.7
/// (where the cells' search stops being exact) is refused.
#[test]
fn voronoi_turns_a_skin_into_struts() {
    use makepad_csg_math::Vec3d;
    let head = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet b = m.box({size: [40mm 40mm 40mm]})\n";
    // the field of m.voronoi(b, {..opts}) at each point, in one batch
    let at = |opts: &str, ps: &[[f64; 3]]| -> Vec<f64> {
        let p = eval(&format!("{head}let v = m.voronoi(b, {{size: 6mm strut: 1.5mm {opts}}})\nplace(v)\n")).unwrap_or_else(|e| panic!("{opts}: {e:?}"));
        let Op::Field { shape, .. } = &p.graph.node(p.solid("v").unwrap().node).op else { panic!("not a field") };
        let ps: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
        let mut out = vec![0.0; ps.len()];
        shape.field().distance_many(&ps, &mut out);
        out
    };
    let cube = |p: [f64; 3]| {
        let q = p.map(|x| x.abs() - 20.0);
        q.iter().map(|x| x.max(0.0) * x.max(0.0)).sum::<f64>().sqrt() + q[0].max(q[1]).max(q[2]).min(0.0)
    };
    let share = |v: &[f64]| v.iter().filter(|d| **d < 0.0).count() as f64 / v.len() as f64;
    // half way into the 3mm skin of the upright face x = 20, and a block well inside
    let skin: Vec<[f64; 3]> = (0..12 * 12).map(|i| [18.5, (i % 12) as f64 * 2.6 - 14.3, (i / 12) as f64 * 2.6 - 14.3]).collect();
    // (a spacing off the cells' pitch: at 5.9 mm in 6 mm cells it reads the same spot of each)
    let deep: Vec<[f64; 3]> = (0..64).map(|i| [(i % 4) as f64, (i / 4 % 4) as f64, (i / 16) as f64].map(|c| c * 4.3 - 6.45)).collect();
    let near: Vec<[f64; 3]> = skin.iter().chain(&deep).copied().collect();
    let d = [0.13f64, -0.21, 0.17];
    let step = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let moved: Vec<[f64; 3]> = near.iter().map(|p| [p[0] + d[0], p[1] + d[1], p[2] + d[2]]).collect();
    let foam = at("", &[near.clone(), moved].concat());
    let (here, there) = foam.split_at(near.len());
    let pointed = share(&here[..skin.len()]);
    assert!((0.15..0.85).contains(&pointed), "the skin is struts and holes: {pointed}");
    let inner = share(&here[skin.len()..]);
    assert!((0.02..0.6).contains(&inner), "a foam inside: {inner}");
    for (i, p) in near.iter().enumerate() {
        assert!(here[i] >= cube(*p) - 1e-3, "inside the cube at {p:?}: {} {}", here[i], cube(*p));
        assert!((there[i] - here[i]).abs() <= step * 1.02, "a distance bound at {p:?}: {} over {step}", (there[i] - here[i]).abs());
    }
    let round = share(&at("tops: @round", &skin));
    assert!(pointed > round + 0.02, "pointed tops close the holes' tops on an upright face: {pointed} against {round}");
    assert!(share(&at("core: @solid", &deep)) == 1.0, "a solid core");
    assert!(share(&at("core: @none", &deep)) == 0.0, "hollow");
    let err = eval(&format!("{head}let v = m.voronoi(b, {{size: 6mm strut: 1.5mm random: 0.8}})\nplace(v)\n")).err().unwrap();
    assert!(err.iter().any(|d| d.message.contains("0..0.7")), "{err:?}");
}

/// `adapt: @stress`: without the stress the struts are all `strut` and the part asks for it over
/// its source's box; with a stress rising across the box they thicken towards `strut_max` where it
/// is high, stay `strut` where it is low, and the field stays a distance bound.
#[test]
fn voronoi_grades_its_struts_by_stress() {
    use makepad_csg_math::Vec3d;
    let src = |opts: &str| format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet b = m.box({{size: [40mm 40mm 40mm]}})\nlet v = m.voronoi(b, {{size: 6mm strut: 1mm adapt: @stress core: @none {opts}}})\nplace(v)\n");
    let run = |opts: &str, stress: HashMap<String, Arc<csg_sdf::SdfGrid>>| evaluate(&src(opts), &Options { generated: &Default::default(), assets: &Assets::new(), images: &Default::default(), extension: "fab", libraries: &[], inputs: &NO_INPUTS, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: stress });
    let at = |p: &Program, ps: &[[f64; 3]]| -> Vec<f64> {
        let Op::Field { shape, .. } = &p.graph.node(p.solid("v").unwrap().node).op else { panic!("not a field") };
        let ps: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
        let mut out = vec![0.0; ps.len()];
        shape.field().distance_many(&ps, &mut out);
        out
    };
    let share = |v: &[f64]| v.iter().filter(|d| **d < 0.0).count() as f64 / v.len() as f64;
    let plain = run("strut_max: 2.5mm", HashMap::new()).unwrap();
    assert_eq!(plain.stress_requests.len(), 1);
    let r = plain.stress_requests[0].clone();
    assert_eq!(r.part, "v");
    assert!(r.lo.iter().chain(&r.hi).all(|x| (x.abs() - 20.0).abs() < 1e-6) && (r.spacing - 3.0).abs() < 1e-9, "{r:?}");
    assert!(plain.notes.iter().any(|n| n.message.contains("adapt: @stress")), "{:?}", plain.notes);
    // the stress at the reference on the +x face, none on the -x face
    let n: [u32; 3] = std::array::from_fn(|a| ((r.hi[a] - r.lo[a]) / r.spacing).ceil() as u32 + 1);
    let data: Vec<f32> = (0..n[0] * n[1] * n[2]).map(|i| ((i % n[0]) as f64 * r.spacing / (r.hi[0] - r.lo[0])).min(1.0) as f32).collect();
    let grid = csg_sdf::SdfGrid::new(r.lo.map(|x| x as f32), r.spacing as f32, n, 0.0, data.into());
    let graded = run("strut_max: 2.5mm", HashMap::from([("v".to_string(), Arc::new(grid))])).unwrap();
    assert_eq!(graded.stress_requests.len(), 1, "a graded part keeps asking, for the host to see what changed");
    assert!(graded.notes.is_empty(), "{:?}", graded.notes);
    // half way into the skins of the -x and +x faces
    let face = |x: f64| -> Vec<[f64; 3]> { (0..12 * 12).map(|i| [x, (i % 12) as f64 * 2.6 - 14.3, (i / 12) as f64 * 2.6 - 14.3]).collect() };
    let (low, high) = (face(-19.0), face(19.0));
    let (p, g) = (at(&plain, &[low.clone(), high.clone()].concat()), at(&graded, &[low.clone(), high.clone()].concat()));
    let (p_low, p_high, g_low, g_high) = (share(&p[..144]), share(&p[144..]), share(&g[..144]), share(&g[144..]));
    assert!((g_low - p_low).abs() < 0.05, "struts stay `strut` where the stress is low: {g_low} against {p_low}");
    assert!(g_high > p_high + 0.1, "thicker where it is high: {g_high} against {p_high}");
    // a distance bound
    let d = [0.13f64, -0.21, 0.17];
    let step = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let near: Vec<[f64; 3]> = high.iter().chain(&low).copied().collect();
    let moved: Vec<[f64; 3]> = near.iter().map(|p| [p[0] + d[0], p[1] + d[1], p[2] + d[2]]).collect();
    let (a, b) = (at(&graded, &near), at(&graded, &moved));
    for i in 0..near.len() {
        assert!((a[i] - b[i]).abs() <= step * 1.02, "a distance bound at {:?}: {} over {step}", near[i], (a[i] - b[i]).abs());
    }
    let err = run("strut_max: 0.5mm", HashMap::new()).err().unwrap();
    assert!(err.iter().any(|d| d.message.contains("strut_max")), "{err:?}");
}

/// `m.veins`: without the host's tubes the part asks for them over its source's box with its
/// mould as written and holds nothing; with them its field is each tube's distance (a round cone
/// cut off under the skin), the table's reach far from them, and a distance bound.
#[test]
fn veins_raise_the_tubes_the_host_passes() {
    use makepad_csg_math::Vec3d;
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet slab = m.box({size: [40mm 4mm 40mm]})\nlet v = m.veins(slab, Physarum{preset: @lace scale: 8mm seed: 2})\nplace(m.blend([slab, v], 0.5mm))\n";
    let run = |solved: HashMap<String, Arc<csg_sdf::SdfGrid>>| evaluate(src, &Options { generated: &Default::default(), assets: &Assets::new(), images: &Default::default(), extension: "fab", libraries: &[], inputs: &NO_INPUTS, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved }).unwrap();
    let at = |p: &Program, ps: &[[f64; 3]]| -> Vec<f64> {
        let Op::Field { shape, .. } = &p.graph.node(p.solid("v").unwrap().node).op else { panic!("not a field") };
        let ps: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0], p[1], p[2])).collect();
        let mut out = vec![0.0; ps.len()];
        shape.field().distance_many(&ps, &mut out);
        out
    };
    let bare = run(HashMap::new());
    assert_eq!(bare.veins_requests.len(), 1);
    let r = bare.veins_requests[0].clone();
    assert_eq!((r.part.as_str(), r.pattern.kind, r.pattern.preset), ("v", "Physarum", "lace"));
    assert_eq!(r.pattern.values, vec![("scale", 8.0), ("seed", 2.0)]);
    assert!(r.lo.iter().zip(&r.hi).zip([20.0, 2.0, 20.0]).all(|((lo, hi), h)| (lo + h).abs() < 1e-6 && (hi - h).abs() < 1e-6), "{r:?}");
    assert!(bare.notes.iter().any(|n| n.message.contains("veins rise")), "{:?}", bare.notes);
    assert!(at(&bare, &[[0.0, 2.5, 0.0], [5.0, 2.0, 5.0]]).iter().all(|d| *d >= 1.99), "nothing before the tubes come");
    let boxed = |p: &Program| {
        let Op::Field { shape, .. } = &p.graph.node(p.solid("v").unwrap().node).op else { panic!("not a field") };
        (shape.min, shape.max)
    };
    let (lo, hi) = boxed(&bare);
    assert!((0..3).all(|c| (hi[c] - lo[c]).abs() < 1e-9), "no tubes, no box: {lo:?} {hi:?}");
    // a tube along x on the top face, tapering from a 1 mm ball to a 0.5 mm one, cut off 0.8 mm
    // under the top; and one standing up off it, a ball within the other
    let tubes: Vec<f32> = vec![-8.0, 1.6, 0.0, 1.0, 8.0, 1.6, 0.0, 0.5, 0.0, 1.0, 0.0, 1.2, 0.0, 1.6, 6.0, 1.0, 0.0, 1.8, 6.0, 0.5, 0.0, 1.0, 0.0, 1.2];
    let grid = csg_sdf::SdfGrid::new([0.0; 3], 1.0, [2, 2, 2], 0.0, vec![0.0; 8].into()).with_table(tubes.into());
    let raised = run(HashMap::from([("v".to_string(), Arc::new(grid))]));
    assert!(raised.notes.is_empty(), "{:?}", raised.notes);
    // its box holds the tubes as cut, not their balls: nothing under the cut 1.2 mm up
    let (lo, hi) = boxed(&raised);
    let want: ([f64; 3], [f64; 3]) = ([-9.0, 1.2, -1.0], [8.5, 2.6, 7.0]);
    assert!((0..3).all(|c| (lo[c] - want.0[c]).abs() < 1e-3 && (hi[c] - want.1[c]).abs() < 1e-3), "the tubes' box {lo:?} {hi:?}, not {want:?}");
    // the round cone's side from (-8, 1.6) r 1 to (8, 1.6) r 0.5: its slope, and where its side is
    let s = 0.5f64 / 16.0;
    let c = (1.0 - s * s).sqrt();
    let side = |x: f64, y: f64, z: f64| -> f64 {
        let (px, rr) = (x + 8.0, ((y - 1.6) * (y - 1.6) + z * z).sqrt());
        rr * c + px * s - 1.0
    };
    let ps = [[0.0, 2.6, 0.0], [0.0, 3.0, 0.0], [-4.0, 1.6, 1.2], [4.0, 2.2, -0.9], [0.0, 1.0, 0.0], [0.0, 0.5, 0.0], [-9.5, 1.6, 0.0], [9.0, 1.6, 0.0], [0.0, 1.6, 6.0], [0.0, 3.0, 6.0], [0.0, 2.6, 15.0]];
    let want = [side(0.0, 2.6, 0.0), side(0.0, 3.0, 0.0), side(-4.0, 1.6, 1.2), side(4.0, 2.2, -0.9), side(0.0, 1.0, 0.0).max(0.2), 0.7, 0.5, 0.5, -0.4, 0.4, 2.0];
    let got = at(&raised, &ps);
    for i in 0..ps.len() {
        assert!((got[i] - want[i]).abs() < 1e-4, "at {:?}: {} against {}", ps[i], got[i], want[i]);
    }
    // a distance bound
    let d = [0.13f64, -0.21, 0.17];
    let step = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let near: Vec<[f64; 3]> = (0..400).map(|i| [(i % 20) as f64 - 10.0, 0.5 + (i / 20 % 5) as f64 * 0.7, (i / 100) as f64 * 2.5 - 1.0]).collect();
    let moved: Vec<[f64; 3]> = near.iter().map(|p| [p[0] + d[0], p[1] + d[1], p[2] + d[2]]).collect();
    let (a, b) = (at(&raised, &near), at(&raised, &moved));
    for i in 0..near.len() {
        assert!((a[i] - b[i]).abs() <= step * 1.001, "a distance bound at {:?}: {} over {step}", near[i], (a[i] - b[i]).abs());
    }
    let err = evaluate(&src.replace("Physarum{preset: @lace", "Turing{preset: @spots"), &Options { generated: &Default::default(), assets: &Assets::new(), images: &Default::default(), extension: "fab", libraries: &[], inputs: &NO_INPUTS, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: HashMap::new() }).err().unwrap();
    assert!(err.iter().any(|d| d.message.contains("slime mould")), "{err:?}");
}

/// A diamond knurl on a cylinder: cut its depth deep in a groove and untouched on the flat tops,
/// seamless round the axis, never steeper than 1, and gone on a masked top face. The other
/// patterns and mappings compile too.
#[test]
fn textures_knurl_a_cylinder() {
    use csg_sdf::TexProject;
    use makepad_csg_math::Vec3d;
    let head = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet knob = m.cylinder({radius: 15mm height: 20mm})\n";
    let p = eval(&format!("{head}let grip = m.texture(knob, {{pattern: Knurl{{pitch: 1mm angle: 30° kind: @diamond flat: 0.25}} map: @cylindrical depth: 0.4mm top_angle: 20° bottom_angle: 20°}})\nplace(grip)\n")).unwrap();
    let Op::Field { shape, .. } = &p.graph.node(p.solid("grip").unwrap().node).op else { panic!("not a field") };
    let SdfNode::Id { of, .. } = &shape.root else { panic!() };
    let SdfNode::Texture { tex, .. } = &**of else { panic!("{of:?}") };
    let l = tex.lipschitz;
    assert_eq!(tex.project, TexProject::Axis(1));
    assert!((tex.depth - 0.4).abs() < 1e-12 && (tex.step - 1.0).abs() < 1e-12, "{} {}", tex.depth, tex.step);
    // The bound from the parameters: the knurl's slope per cell (1 over the rounded V's height at
    // the groove's half width w), through the mapping at r1 (both sets), the radial fade, and the
    // masks' smoothstep through the gradient smoothed over 1mm.
    let (w, e) = (0.375f64, 0.15f64);
    let g = 1.0 / ((w * w + e * e).sqrt() - e);
    let pattern = g * (94.0 / (std::f64::consts::TAU * 14.2) + 30f64.to_radians().tan()) + 1.0 / 0.8;
    let mask = 1.5 / (20f64.to_radians().cos() - 35f64.to_radians().cos());
    assert!((l - (1.0 + 0.4 * (pattern + mask))).abs() < 1e-9, "{l}");
    // 94 cells round the 15mm radius (round(2 pi 15 / 1)): their spacing across the grooves.
    let su = std::f64::consts::TAU * 15.0 / 94.0;
    let sv = 1.0 / 30f64.to_radians().tan();
    assert!((tex.feature - 0.5 / (1.0 / su).hypot(1.0 / sv)).abs() < 1e-9, "{}", tex.feature);
    let field = shape.field();
    let f = |x: f64, y: f64, z: f64| field.distance(Vec3d::new(x, y, z));
    // At cell (u, v) round the surface, radius r.
    let at = |u: f64, v: f64, r: f64| {
        let a = std::f64::consts::TAU * u / 94.0;
        f(r * a.cos(), v * sv, r * a.sin())
    };
    // On one groove's centre line (u + v whole, u - v half way between the other set's) the
    // surface is 0.4 in; on a flat top (half a cell from both sets) it is where it was; an
    // eighth of a cell off the groove, the rounded V's depth there.
    assert!(at(0.25, -0.25, 14.6).abs() < 1e-4, "{}", at(0.25, -0.25, 14.6));
    assert!((at(0.25, -0.25, 15.0) - 0.4 / l).abs() < 1e-4);
    assert!(at(0.5, 0.0, 15.0).abs() < 1e-4, "{}", at(0.5, 0.0, 15.0));
    let flank = 1.0 - ((0.25f64 * 0.25 + e * e).sqrt() - e) / ((w * w + e * e).sqrt() - e);
    assert!((at(0.375, -0.125, 15.0) - 0.4 * flank / l).abs() < 1e-4, "{} {flank}", at(0.375, -0.125, 15.0));
    // Seamless: across the half turn where the angle wraps.
    for y in [-3.3, 0.7, 4.1] {
        let (a, b) = (f(-15.0, y, 1e-5), f(-15.0, y, -1e-5));
        assert!((a - b).abs() < 1e-5, "{y}: {a} {b}");
    }
    // The bound holds: no sampled slope above 1 across the band, its rims included.
    let e = 1e-3;
    let mut steepest = 0.0f64;
    for i in 0..640 {
        let (a, r, y) = (i as f64 * 0.7391, 13.0 + (i % 7) as f64 * 0.5, -11.0 + (i % 23) as f64);
        let (x, z) = (r * a.cos(), r * a.sin());
        let g = Vec3d::new(f(x + e, y, z) - f(x - e, y, z), f(x, y + e, z) - f(x, y - e, z), f(x, y, z + e) - f(x, y, z - e)) * (0.5 / e);
        steepest = steepest.max(g.length());
    }
    assert!(steepest <= 1.0 + 1e-3 && steepest > 0.1, "{steepest}");
    // Hexagons onto a plane, noise from three sides and a pattern of one's own (its slope measured).
    let p = eval(&format!("{head}let knurl = Knurl{{pitch: 1mm}}\nlet masked = m.texture(knob, {{pattern: knurl map: @cylindrical depth: 0.4mm top_angle: 20° smooth: 0.3mm}})\nlet bare = m.texture(knob, {{pattern: knurl map: @cylindrical depth: 0.4mm}})\nplace(masked)\nplace(bare)\nlet a = m.texture(knob, {{pattern: Hex{{size: 4mm}} map: @planar depth: -0.3mm}})\nlet b = m.texture(knob, {{pattern: Noise{{size: 2mm}} depth: 0.2mm}})\nlet c = m.texture(knob, {{pattern: Pattern{{field: fn(c) {{ return 0.5 + 0.5 * sin(c.x * TAU) }} cell: [2mm 2mm 2mm]}} map: @cylindrical depth: 0.3mm turn: 10°}})\nplace(a)\nplace(b)\nplace(c)\n")).unwrap();
    for (name, project) in [("a", TexProject::Axis(1)), ("b", TexProject::Triplanar), ("c", TexProject::Axis(1))] {
        let Op::Field { shape, .. } = &p.graph.node(p.solid(name).unwrap().node).op else { panic!("not a field") };
        let SdfNode::Id { of, .. } = &shape.root else { panic!() };
        let SdfNode::Texture { tex, .. } = &**of else { panic!() };
        assert_eq!(tex.project, project, "{name}");
        let v = shape.field().distance(Vec3d::new(15.0, 1.0, 0.0));
        assert!(v.is_finite() && v.abs() <= 0.31, "{name}: {v}");
    }
    // The top face is masked (its normal, smoothed over 0.3mm, is up there): untouched where
    // the same texture unmasked cuts in.
    let value = |name: &str, x: f64, y: f64| {
        let Op::Field { shape, .. } = &p.graph.node(p.solid(name).unwrap().node).op else { panic!() };
        shape.field().distance(Vec3d::new(x, y, 0.0))
    };
    assert!(value("masked", 13.9, 10.0).abs() < 1e-6 && value("bare", 13.9, 10.0) > 0.01, "{} {}", value("masked", 13.9, 10.0), value("bare", 13.9, 10.0));
    let raised = p.solid("a").unwrap();
    let Op::Field { shape, .. } = &p.graph.node(raised.node).op else { panic!() };
    assert!((shape.max[1] - 10.3).abs() < 1e-9, "a raised texture grows the box: {:?}", shape.max);
}

/// What the march's step on a texture's source rests on (`SdfTexture::unit_pattern`): the
/// library's patterns through their mappings stay in 0..1 everywhere. A user's own only promises.
#[test]
fn the_library_patterns_stay_in_0_to_1() {
    use makepad_csg_math::Vec3d;
    let head = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet knob = m.cylinder({radius: 15mm height: 20mm})\n";
    let patterns = [
        ("Knurl{pitch: 1.2mm angle: 30° kind: @diamond flat: 0.2}", "@cylindrical"),
        ("Knurl{pitch: 1.2mm angle: 30° kind: @diamond flat: 0}", "@cylindrical"),
        ("Knurl{pitch: 1mm angle: 45° kind: @diagonal flat: 0.9}", "@cylindrical"),
        ("Knurl{pitch: 2mm kind: @straight}", "@cylindrical"),
        ("Hex{size: 4mm groove: 0.5}", "@planar"),
        ("Hex{size: 4mm groove: 0.05}", "@cylindrical"),
        ("Noise{size: 2mm}", "@planar"),
        ("Noise{size: 0.5mm}", "@triplanar"),
    ];
    for (pattern, map) in patterns {
        let p = eval(&format!("{head}let t = m.texture(knob, {{pattern: {pattern} map: {map} depth: 0.4mm}})\nplace(t)\n")).unwrap();
        let Op::Field { shape, .. } = &p.graph.node(p.solid("t").unwrap().node).op else { panic!("not a field") };
        let SdfNode::Id { of, .. } = &shape.root else { panic!() };
        let SdfNode::Texture { tex, .. } = &**of else { panic!() };
        assert!(tex.unit_pattern, "{pattern}");
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        // the kernel's points, in its metres, on an irrational walk through the part and past it
        for i in 0..20_000 {
            let w = |k: f64| (i as f64 * k).fract() * 0.05 - 0.025;
            let v = tex.kernel.field.distance(Vec3d::new(w(0.618_033_9), w(0.754_877_7), w(0.569_840_3)));
            lo = lo.min(v);
            hi = hi.max(v);
        }
        assert!(lo >= -1.0e-5 && hi <= 1.0 + 1.0e-5, "{pattern}: {lo} .. {hi}");
    }
    let p = eval(&format!("{head}let t = m.texture(knob, {{pattern: Pattern{{field: fn(c) {{ return 2.0 * c.x }} cell: [2mm 2mm 2mm]}} map: @planar depth: 0.3mm}})\nplace(t)\n")).unwrap();
    let Op::Field { shape, .. } = &p.graph.node(p.solid("t").unwrap().node).op else { panic!("not a field") };
    let SdfNode::Id { of, .. } = &shape.root else { panic!() };
    let SdfNode::Texture { tex, .. } = &**of else { panic!() };
    assert!(!tex.unit_pattern, "a user's pattern promises only");
}

/// Instance, bone and warps lower to distance-field nodes: one Id per builder call, rates per
/// metre turned into the kernel's units.
#[test]
fn field_nodes_lower_to_csg_sdf() {
    use csg_sdf::{part_id, SdfDeform};
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet post = m.box({size: [4mm 20mm 4mm]})\nlet posts = m.instance(post, for x in [-10mm 10mm] { Place{at: [x 0 0] rotate: [0 45° 0]} })\nlet arm = m.bone(m.twist(post, 90° / 20mm), {rest: Place{at: [0 10mm 0]}})\nlet look = m.fold(m.blend([posts arm], 2mm), @x)\nplace(look)\n";
    let p = eval(src).unwrap();
    let look = p.solid("look").unwrap();
    let Op::Field { shape, .. } = &p.graph.node(look.node).op else { panic!("not a field") };
    let SdfNode::Id { id, of } = &shape.root else { panic!("{:?}", shape.root) };
    assert_eq!(*id, part_id("look"));
    let SdfNode::Deform { deform: SdfDeform::Mirror { axis: 0 }, of, .. } = &**of else { panic!() };
    let SdfNode::Id { of, .. } = &**of else { panic!() };
    let SdfNode::Combine { of: parts, .. } = &**of else { panic!() };
    let SdfNode::Id { id, of: inst } = &parts[0] else { panic!() };
    assert_eq!(*id, part_id("posts"));
    let SdfNode::Instance { places, .. } = &**inst else { panic!() };
    assert_eq!(places.len(), 2);
    assert!((places[1].at[0] - 10.0).abs() < 1e-9, "kernel units are millimetres");
    let SdfNode::Id { of, .. } = &parts[1] else { panic!() };
    let SdfNode::Bone { id, rest, of } = &**of else { panic!() };
    assert_eq!((*id, rest.at[1]), (part_id("arm"), 10.0));
    let SdfNode::Id { of, .. } = &**of else { panic!() };
    let SdfNode::Deform { deform: SdfDeform::Twist { rate }, .. } = &**of else { panic!() };
    assert!((rate - std::f64::consts::FRAC_PI_2 / 20.0).abs() < 1e-9, "radians per millimetre");
    // Instances of a meshed solid are transformed copies.
    let mesh = eval("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet peg = m.extrude({sketch: Circle{radius: 2mm} height: 5mm})\nlet pegs = m.instance(peg, [[0 0 0] [10mm 0 0]])\nplace(pegs)\n").unwrap();
    assert!(matches!(p.graph.node(look.node).op, Op::Field { .. }));
    assert!(matches!(mesh.graph.node(mesh.solid("pegs").unwrap().node).op, Op::Boolean { .. }));
}

fn closed_mesh_volume(mesh: &csg_graph::TriMesh, property: &str) -> f64 {
    assert!(!mesh.triangles.is_empty(), "{property}: the mesh has faces");
    let mut edges = HashMap::<(u32, u32), (usize, i32)>::new();
    for &[a, b, c] in &mesh.triangles {
        let (pa, pb, pc) = (mesh.vertices[a as usize], mesh.vertices[b as usize], mesh.vertices[c as usize]);
        assert!((pb - pa).cross(pc - pa).length() > 0.0, "{property}: each triangle has area");
        for (a, b) in [(a, b), (b, c), (c, a)] {
            let edge = edges.entry((a.min(b), a.max(b))).or_default();
            edge.0 += 1;
            edge.1 += if a < b { 1 } else { -1 };
        }
    }
    assert!(edges.values().all(|e| *e == (2, 0)), "{property}: each edge has two oppositely wound faces");
    makepad_csg_cad::signed_volume(mesh)
}

#[test]
fn drafted_boxes_mesh_as_prismatoids_and_plain_boxes_keep_their_keys() {
    let pre = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    for (taper_x, taper_z, shift) in [(4.0, 2.0, 3.0), (-4.0, -2.0, -3.0), (0.0, 0.0, 3.0), (4.0, 0.0, 0.0)] {
        let p = eval(&format!("{pre}let drafted = m.box({{size: [10mm 12mm 8mm] taper: [{taper_x}mm, {taper_z}mm] shift: {shift}mm}})\nplace(drafted)\n")).expect("a drafted box script lowers");
        let node = p.solid("drafted").expect("the drafted box keeps its script name").node;
        assert!(matches!(&p.graph.node(node).op, Op::Hull { points } if points.len() == 8), "a drafted box lowers to the hull of eight corners");
        let mesh = csg_graph::Evaluator::new().mesh(&p.graph, node, None).expect("a drafted box meshes");
        let a1 = 10.0 * 8.0;
        let am = (10.0 + taper_x * 0.5) * (8.0 + taper_z * 0.5);
        let a2 = (10.0 + taper_x) * (8.0 + taper_z);
        let expected = 12.0 / 6.0 * (a1 + 4.0 * am + a2);
        let volume = closed_mesh_volume(&mesh, "drafted box");
        assert!((volume - expected).abs() < 1e-9, "a drafted box has the prismatoid volume: {volume} != {expected}");
        for (y, width, depth, centre) in [(-6.0, 10.0, 8.0, 0.0), (6.0, 10.0 + taper_x, 8.0 + taper_z, shift)] {
            for x in [-0.5, 0.5] {
                for z in [-0.5, 0.5] {
                    let corner = makepad_csg_math::Vec3d::new(centre + x * width, y, z * depth);
                    assert!(mesh.vertices.iter().any(|p| p.distance(corner) < 1e-9), "drafted box face at y={y} has its sized and shifted corner {corner:?}");
                }
            }
        }
    }
    for (radius, field) in [(0.0, ""), (1.0, " radius: 1mm")] {
        let mut old = Graph::new();
        let old_node = old.push(Op::Box { size: [10.0, 12.0, 8.0], radius }).expect("the existing box op builds");
        for draft in ["", " taper: [0 0]", " shift: 0", " taper: [0 0] shift: 0"] {
            let p = eval(&format!("{pre}let plain = m.box({{size: [10mm 12mm 8mm]{field}{draft}}})\nplace(plain)\n")).expect("an undrafted box script lowers");
            let node = p.graph.node(p.solid("plain").expect("the plain box keeps its script name").node);
            let Op::Box { size, radius: actual } = &node.op else { panic!("zero draft must keep Op::Box") };
            assert_eq!((*size, *actual), ([10.0, 12.0, 8.0], radius), "zero draft keeps the existing box fields");
            assert_eq!(node.key, old.node(old_node).key, "absent or zero draft keeps the existing box key");
        }
    }
    for draft in ["taper: [1mm 0]", "shift: 1mm"] {
        let errors = eval(&format!("{pre}let bad = m.box({{size: [10mm 12mm 8mm] radius: 1mm {draft}}})\n")).err().expect("rounding a drafted box is an error");
        assert!(errors.iter().any(|e| e.message == "Box: 'radius' cannot be combined with 'taper' or 'shift'"), "rounding explains its conflict with draft: {errors:?}");
    }
    let errors = eval(&format!("{pre}let drafted = m.box({{size: [10mm 12mm 8mm] shift: 2mm}})\nlet look = m.blend([drafted m.sphere({{radius: 2mm}})], 1mm)\n")).err().expect("a drafted box cannot silently become an undrafted distance field");
    assert!(errors.iter().any(|e| e.message.contains("'taper' or 'shift' has no distance field")), "a blend explains why a drafted box is unsupported: {errors:?}");
}

#[test]
fn hull_scripts_mesh_cube_corners_and_report_kernel_errors() {
    let pre = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let p = eval(&format!("{pre}let hull = m.hull({{points: [[-5mm, -5mm, -5mm] [5mm, -5mm, -5mm] [5mm, -5mm, 5mm] [-5mm, -5mm, 5mm] [-5mm, 5mm, -5mm] [5mm, 5mm, -5mm] [5mm 5mm 5mm] [-5mm, 5mm, 5mm] [0 0 0]]}})\nplace(hull)\n")).expect("a hull script lowers");
    let node = p.solid("hull").expect("the hull keeps its script name").node;
    let mesh = csg_graph::Evaluator::new().mesh(&p.graph, node, None).expect("cube corners and an interior point mesh");
    assert!((closed_mesh_volume(&mesh, "cube hull") - 1000.0).abs() < 1e-9, "the interior point does not change the cube's volume");
    for points in ["[0 0 0] [10mm 0 0] [0 10mm 0]", "[0 0 0] [10mm 0 0] [10mm 10mm 0] [0 10mm 0]"] {
        let p = eval(&format!("{pre}let bad = m.hull({{points: [{points}]}})\nplace(bad)\n")).expect("hull geometry is validated by the kernel");
        let node = p.solid("bad").expect("the invalid hull retains its script identity").node;
        let error = csg_graph::Evaluator::new().mesh(&p.graph, node, None).expect_err("a hull needs four non-coplanar points");
        assert_eq!(error.node, node, "a hull build failure identifies the script's solid");
        assert_eq!(error.message, "a hull needs four points that are not in one plane", "hull scripts preserve the kernel's error text");
    }
    let example = protos::proto("Hull").expect("Hull is in the vocabulary").example;
    let p = eval(&format!("{pre}let example = {example}\n")).expect("the documented Hull example lowers");
    let mesh = csg_graph::Evaluator::new().mesh(&p.graph, p.solid("example").unwrap().node, None).expect("the documented Hull example meshes");
    assert!(closed_mesh_volume(&mesh, "Hull example") > 0.0, "the documented hull is outward wound");
}

#[test]
fn ring_loft_scripts_mesh_frustums_and_tilted_caps_and_report_bad_rings() {
    let pre = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let source = "let loft = m.ring_loft({rings: [LoftRing{at: [0 0 0] u: [1 0 0] v: [0, 0, -1] points: [[-5mm, -5mm] [5mm, -5mm] [5mm 5mm] [-5mm, 5mm]]} LoftRing{at: [2mm 12mm 0] u: [1 0 0] v: [0, 0, -1] points: [[-3mm, -3mm] [3mm, -3mm] [3mm 3mm] [-3mm, 3mm]] crease: true}] prism: true})\nplace(loft)\n";
    let p = eval(&format!("{pre}{source}")).expect("a ring loft script lowers");
    let node = p.solid("loft").expect("the ring loft keeps its script name").node;
    let Op::RingLoft { rings, prism } = &p.graph.node(node).op else { panic!("ring_loft lowers to Op::RingLoft") };
    assert_eq!(rings.len(), 2, "both ring objects reach the kernel");
    assert!(*prism && !rings[0].crease && rings[1].crease, "prism and crease flags retain explicit values and defaults");
    assert_eq!((rings[1].origin, rings[1].points[0]), ([2.0, 12.0, 0.0], [-3.0, -3.0]), "ring origins and outlines convert to kernel lengths");
    assert_eq!((rings[0].u, rings[0].v), ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]), "ring axes stay dimensionless");
    let mesh = csg_graph::Evaluator::new().mesh(&p.graph, node, None).expect("similar square rings mesh");
    let expected = 12.0 / 3.0 * (100.0 + 60.0 + 36.0);
    assert!((closed_mesh_volume(&mesh, "square ring frustum") - expected).abs() < 1e-9, "similar square rings have the frustum volume");

    let example = protos::proto("RingLoft").expect("RingLoft is in the vocabulary").example;
    let p = eval(&format!("{pre}let tilted = {example}\nplace(tilted)\n")).expect("the documented tilted ring loft lowers");
    let node = p.solid("tilted").expect("the tilted ring loft keeps its script name").node;
    let Op::RingLoft { rings, prism } = &p.graph.node(node).op else { panic!("the tilted example lowers to Op::RingLoft") };
    assert!(!prism && rings.iter().all(|r| !r.crease), "prism and creases default to false");
    let mesh = csg_graph::Evaluator::new().mesh(&p.graph, node, None).expect("a tilted top ring meshes");
    assert!(closed_mesh_volume(&mesh, "tilted ring loft") > 0.0, "a tilted ring loft is closed and outward wound");
    let corner = makepad_csg_math::Vec3d::new(-3.0, 18.2, 2.4);
    assert!(mesh.vertices.iter().any(|v| v.distance(corner) < 1e-9), "the top outline follows its tilted ring axes");

    for (from, to, says) in [
        (" [3mm 3mm]", "", "ring loft rings have different point counts"),
        ("u: [1 0 0]", "u: [2 0 0]", "ring loft ring 0 needs unit-length u within 1e-9"),
    ] {
        let p = eval(&format!("{pre}{}", source.replace(from, to))).expect("ring geometry is validated by the kernel");
        let node = p.solid("loft").expect("the invalid ring loft retains its script identity").node;
        let error = csg_graph::Evaluator::new().mesh(&p.graph, node, None).expect_err("invalid rings must not mesh");
        assert_eq!(error.node, node, "a ring loft build failure identifies the script's solid");
        assert_eq!(error.message, says, "ring loft scripts preserve the kernel's error text");
    }

    // A Ring parameter handle beside loft rings keeps its own schema.
    let options = Options { generated: &Default::default(), assets: &Assets::new(), images: &Default::default(), extension: "fab", libraries: &[], inputs: &NO_INPUTS, unit: 1000.0, roots: &[RootFn { name: "handle", args: &["param", "shape"], model: false }], overrides: HashMap::new(), weave: None, solved: HashMap::new() };
    let p = evaluate(&format!("{pre}let tilted = {example}\nhandle(3mm, Ring{{center: [0 20mm 0] radius: 3mm}})\n"), &options).expect("loft rings and a Ring parameter handle coexist in one script");
    let Some(Arg::Value(V::Obj(handle))) = p.roots[0].get("shape") else { panic!("a Ring handle reaches the host as a value") };
    assert_eq!(handle.proto.as_deref(), Some("Ring"), "the parameter handle keeps its existing type name");
    assert_eq!(handle.get("radius").and_then(V::num), Some(0.003), "the parameter handle keeps its radius in VM units");
    assert_eq!(protos::proto("Ring").unwrap().group, protos::Group::Handle, "Ring stays the handle schema");
    assert_eq!(protos::PROTOS.iter().filter(|p| p.name == "Ring").count(), 1, "the loft outline does not reuse the Ring name");
}

/// A cage is a mesh part: a smooth closed surface round its points, built with loops here, as
/// many levels as the print tolerance needs unless it says; inside a blend it is a distance field.
/// A cage that does not close, a face naming a point the cage lacks and levels past the most are
/// errors at the call.
#[test]
fn a_cage_is_a_smooth_mesh_part_and_a_field_in_a_blend() {
    let pre = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let horn = "let points = []\nlet faces = []\nfor i in 0..7 {\n    let s = 9mm - i * 1.2mm\n    for c in [[s, 0], [0, s], [-s, 0], [0, -s]] { points.push([i * i * 0.5mm + c[0], i * 8mm, c[1]]) }\n}\nfor i in 0..6 {\n    for k in 0..4 {\n        let j = (k + 1) % 4\n        faces.push([4 * i + k, 4 * i + 4 + k, 4 * i + 4 + j, 4 * i + j])\n    }\n}\nfaces.push([0, 1, 2, 3])\nfaces.push([27, 26, 25, 24])\nlet horn = m.cage({points: points faces: faces})\nplace(horn)\n";
    let cube = |extra: &str, faces: &str| format!("{pre}let box = m.cage({{points: [[-8mm, 0, -8mm] [8mm, 0, -8mm] [8mm, 0, 8mm] [-8mm, 0, 8mm] [-8mm, 16mm, -8mm] [8mm, 16mm, -8mm] [8mm, 16mm, 8mm] [-8mm, 16mm, 8mm]] faces: [{faces}]{extra}}})\nlet ball = m.sphere({{radius: 3mm at: [40mm 0 0]}})\nlet look = m.blend([box ball], 1mm)\nplace(look)\nplace(box)\n");
    let six = "[0 1 2 3] [4 7 6 5] [0 4 5 1] [1 5 6 2] [2 6 7 3] [3 7 4 0]";
    let p = eval(&format!("{pre}{horn}")).unwrap();
    let node = p.solid("horn").unwrap().node;
    let Op::Cage { points, faces, levels } = &p.graph.node(node).op else { panic!("horn is not a cage") };
    assert_eq!((points.len(), faces.len(), *levels), (28, 26, None));
    assert!((points[27][0] - 18.0).abs() < 1e-9 && (points[27][1] - 48.0).abs() < 1e-9, "millimetres: {:?}", points[27]);
    let mesh = makepad_csg_graph::Evaluator::new().mesh(&p.graph, node, None).unwrap();
    let level = (1..=6).find(|l| mesh.triangles.len() == 26 * 2 * 4usize.pow(*l)).expect("whole levels");
    assert!(level >= 3, "a 9 mm cage is {level} levels from 0.05 mm");
    assert_eq!(mesh.corner_normals.len(), mesh.triangles.len() * 3, "limit normals");
    // Two levels of the cube: its corners' limit points, half way to the centre.
    let p = eval(&cube(" levels: 2", six)).unwrap();
    let mesh = makepad_csg_graph::Evaluator::new().mesh(&p.graph, p.solid("box").unwrap().node, None).unwrap();
    assert_eq!(mesh.triangles.len(), 6 * 16 * 2);
    assert!(mesh.vertices[0].distance(makepad_csg_math::Vec3d::new(-4.0, 4.0, -4.0)) < 1e-9, "{:?}", mesh.vertices[0]);
    // In the blend, a distance field of the same mesh: 0 on its corner's limit point, and the
    // centre no deeper than the faces' centres' (the mesh's flat faces lie inside the surface).
    let Op::Field { shape, .. } = &p.graph.node(p.solid("look").unwrap().node).op else { panic!("look is not a field") };
    let d = |x, y, z| csg_sdf::distance(&shape.root, makepad_csg_math::Vec3d::new(x, y, z));
    assert!(d(-4.0, 4.0, -4.0).abs() < 1e-4, "{}", d(-4.0, 4.0, -4.0));
    assert!(d(0.0, 8.0, 0.0) < -6.0 && d(0.0, 8.0, 0.0) > -8.0 * 0.83950617 - 1e-6, "{}", d(0.0, 8.0, 0.0));
    for (extra, faces, says) in [
        ("", "[0 1 2 3] [4 7 6 5] [0 4 5 1] [1 5 6 2] [2 6 7 3]", "only face"),
        ("", "[0 1 2 3] [4 7 6 5] [0 4 5 1] [1 5 6 2] [2 6 7 3] [3 7 4 9]", "names point 9"),
        (" levels: 7", six, "'levels' is 1 to 6"),
        ("", "[0 1 2 3] [4 7 6 5] [0 4 5 1] [1 5 6 2] [2 6 7 3] [3 7 4 0.5]", "by index"),
    ] {
        let e = eval(&cube(extra, faces)).err().unwrap_or_else(|| panic!("{faces}{extra} builds"));
        assert!(e.iter().any(|d| d.message.contains(says)), "{says}: {e:?}");
    }
}

/// An extruded and a revolved sketch in a blend are exact distance fields of the same outline the
/// meshes use: 0 at every mesh vertex, the cap's height above the top; a revolve across its axis
/// says so.
#[test]
fn sketches_extruded_and_revolved_are_exact_fields_in_a_blend() {
    fn sketches(n: &SdfNode, out: &mut Vec<Arc<csg_sdf::SketchSolid>>) {
        match n {
            SdfNode::Sketch(s) => out.push(s.clone()),
            SdfNode::Combine { of, .. } => of.iter().for_each(|c| sketches(c, out)),
            SdfNode::Id { of, .. } | SdfNode::Transform { of, .. } | SdfNode::Material { of, .. } => sketches(of, out),
            _ => {}
        }
    }
    let pre = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let leaf = "Path{from: [0 0] segs: [SplineTo{points: [[12mm 4mm] [20mm 0]]} LineTo{to: [20mm 6mm]} SplineTo{points: [[10mm 9mm] [0 6mm]]}]}";
    let src = format!("{pre}let leaf = m.extrude({{sketch: {leaf} height: 3mm}})\nlet vase = m.revolve({{sketch: Polygon{{points: [[0 0] [8mm 0] [6mm 20mm] [0 20mm]]}}}})\nlet stem = m.capsule({{a: [10mm, 3mm, -6mm] b: [10mm, 20mm, -6mm] radius: 1.5mm}})\nlet look = m.blend([leaf stem], 1.5mm)\nlet pot = m.blend([vase m.sphere({{radius: 3mm at: [0 22mm 0]}})], 1mm)\nplace(look)\nplace(pot)\nplace(leaf)\nplace(vase)\n");
    let p = eval(&src).unwrap_or_else(|e| panic!("{e:?}"));
    for (blend, part) in [("look", "leaf"), ("pot", "vase")] {
        let Op::Field { shape, .. } = &p.graph.node(p.solid(blend).unwrap().node).op else { panic!("{blend} is not a field") };
        let mut found = Vec::new();
        sketches(&shape.root, &mut found);
        let [s] = &found[..] else { panic!("{blend}: {} sketches", found.len()) };
        let mesh = makepad_csg_graph::Evaluator::new().mesh(&p.graph, p.solid(part).unwrap().node, None).unwrap();
        let most = mesh.vertices.iter().map(|v| s.distance(*v).abs()).fold(0.0, f64::max);
        assert!(most < 1e-9, "{part}: the mesh's vertices are on the field's surface, to {most}");
    }
    let mut found = Vec::new();
    let Op::Field { shape, .. } = &p.graph.node(p.solid("look").unwrap().node).op else { unreachable!() };
    sketches(&shape.root, &mut found);
    // 1 mm above the leaf's top, over its middle: 1 mm off (the sketch's v runs along -Z)
    let d = found[0].distance(makepad_csg_math::Vec3d::new(10.0, 4.0, -6.0));
    assert!((d - 1.0).abs() < 1e-12, "{d}");
    let e = eval(&format!("{pre}let w = m.revolve({{sketch: Circle{{radius: 2mm}}}})\nlet b = m.blend([w m.sphere({{radius: 1mm}})], 1mm)\nplace(b)\n")).err().expect("a revolve across its axis");
    assert!(e.iter().any(|d| d.message.contains("+u side")), "{e:?}");
}

/// An options object may stand in any argument slot, the first included.
#[test]
fn options_objects_fill_any_slot() {
    for call in ["m.union({a: x, b: y})", "m.union({a: x b: y})", "m.union(x, {b: y})", "m.union(x, y)", "m.fillet({source: x, radius: 1mm})", "m.fillet(x, {radius: 1mm})"] {
        let src = format!("use mod.fab.*\nlet m = mod.model\nlet x = m.box({{size: [10mm 10mm 10mm]}})\nlet y = m.box({{size: [4mm 20mm 4mm]}})\nlet z = {call}\nplace(z)\n");
        let p = eval(&src).unwrap_or_else(|e| panic!("{call}: {e:?}"));
        assert!(p.solid("z").is_some(), "{call}");
    }
}

const WEAVE_FIELDS: &str = "bar: 0.8mm clearance: 0.35mm clearance_z: 0.4mm";

fn eval_weave(body: &str) -> Program {
    eval(&format!("use mod.model.*\nlet m = mod.model\n{body}\n")).unwrap()
}

fn links_of<'a>(p: &'a Program, id: &str) -> (&'a csg_sdf::Links, u32) {
    let Op::Links { links, sides } = &p.graph.node(p.solid(id).unwrap().node).op else { panic!("{id} is not Links") };
    assert!(links.loops.len() > 1, "{id} needs more than one link");
    (links, *sides)
}

#[test]
fn links_follow_points_on_the_bed() {
    let p = eval_weave(&format!("let c = m.links([[0, 30mm, 0] [120mm, -20mm, 0]], {{{WEAVE_FIELDS} sides: 16}})"));
    let (links, sides) = links_of(&p, "c");
    assert_eq!(sides, 16);
    assert!((10..20).contains(&links.loops.len()));
    assert_eq!(links.min[1], 0.0, "the path's up coordinates are ignored");
    assert!(links.max[1] < 4.0);
    assert!((links.radius - 0.8).abs() < 1e-9);
}

#[test]
fn links_follow_a_closed_shape_outline() {
    let p = eval_weave(&format!("let c = m.links(Circle{{radius: 40mm at: [10mm 20mm]}}, {{{WEAVE_FIELDS}}})"));
    let (links, sides) = links_of(&p, "c");
    assert_eq!(sides, 12);
    assert!((25..40).contains(&links.loops.len()));
    for l in &links.loops {
        let n = l.pts.len() as f64;
        let x = l.pts.iter().map(|p| p[0]).sum::<f64>() / n;
        let z = l.pts.iter().map(|p| p[2]).sum::<f64>() / n;
        assert!(((x - 10.0).hypot(z + 20.0) - 40.0).abs() < 0.03, "sketch v maps to model -z");
    }
}

#[test]
fn mail_fills_a_circle() {
    let p = eval_weave(&format!("let cloth = m.mail(Circle{{radius: 40mm}}, {{{WEAVE_FIELDS}}})"));
    let (links, _) = links_of(&p, "cloth");
    assert!((20..80).contains(&links.loops.len()));
    assert!(links.loops.iter().flat_map(|l| &l.pts).all(|p| p[0].hypot(p[2]) < 40.0));
}

#[test]
fn mail_respects_sketch_holes_and_solid_footprints() {
    let p = eval_weave(&format!(
        "let ring = m.mail(Sketch{{shapes: [Circle{{radius: 40mm}} Circle{{radius: 12mm}}]}}, {{{WEAVE_FIELDS}}})\nlet region = m.box({{size: [60mm 6mm 40mm] at: [10mm, 2mm, -20mm]}})\nlet cloth = m.mail(region, {{{WEAVE_FIELDS}}})"
    ));
    let (ring, _) = links_of(&p, "ring");
    for l in &ring.loops {
        let n = l.pts.len() as f64;
        let x = l.pts.iter().map(|p| p[0]).sum::<f64>() / n;
        let z = l.pts.iter().map(|p| p[2]).sum::<f64>() / n;
        assert!(x.hypot(z) >= 12.0, "the nested loop is a hole");
    }
    let (cloth, _) = links_of(&p, "cloth");
    assert!(cloth.min[0] >= -20.0 && cloth.max[0] <= 40.0);
    assert!(cloth.min[2] >= -40.0 && cloth.max[2] <= 0.0);
    assert_eq!(cloth.min[1], 0.0, "a solid region still generates mail on the bed");
}

#[test]
fn weave_sizes_grade_along_a_list() {
    let span = |l: &csg_sdf::LinkLoop| {
        let (lo, hi) = l.pts.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
        hi - lo
    };
    let p = eval_weave(&format!("let c = m.links([[0 0 0] [150mm 0 0]], {{{WEAVE_FIELDS} size: [5mm 7mm]}})"));
    let (links, _) = links_of(&p, "c");
    let (first, last) = (span(&links.loops[0]), span(&links.loops[links.loops.len() - 1]));
    assert!(last > first + 1.0, "{first} {last}");
    let p = eval_weave(&format!("let cloth = m.mail(Rect{{size: [70mm 70mm]}}, {{{WEAVE_FIELDS} size: [7mm 5mm 7mm] size_y: 5mm}})"));
    let (mail, _) = links_of(&p, "cloth");
    let widths: Vec<f64> = mail.loops.iter().map(span).collect();
    let (small, big) = widths.iter().fold((f64::MAX, 0.0f64), |(a, b), w| (a.min(*w), b.max(*w)));
    assert!(big > small + 2.0, "{small} {big}");
}

#[test]
fn mail_turns_in_a_rectangular_region() {
    let p = eval_weave(&format!("let cloth = m.mail(Rect{{size: [60mm 40mm]}}, {{{WEAVE_FIELDS} angle: 15deg}})"));
    let (links, _) = links_of(&p, "cloth");
    assert!((10..50).contains(&links.loops.len()));
    assert!(links.min[0] >= -30.0 && links.max[0] <= 30.0);
    assert!(links.min[2] >= -20.0 && links.max[2] <= 20.0);
    let plain = eval_weave(&format!("let cloth = m.mail(Rect{{size: [60mm 40mm]}}, {{{WEAVE_FIELDS}}})"));
    assert_ne!(links.key(), links_of(&plain, "cloth").0.key());
}

#[test]
fn drape_lifts_and_tilts_mail_over_a_sphere() {
    let p = eval_weave(&format!(
        "let flat = m.mail(Rect{{size: [60mm 60mm]}}, {{{WEAVE_FIELDS}}})\nlet dome = m.sphere({{radius: 50mm at: [0, -30mm, 0]}})\nlet skin = m.drape(flat, dome)"
    ));
    let (flat, _) = links_of(&p, "flat");
    let (skin, _) = links_of(&p, "skin");
    assert_eq!(skin.loops.len(), flat.loops.len());
    assert!(skin.max[1] > 20.0 && skin.min[1] > flat.min[1]);
    assert!(skin.loops.iter().zip(&flat.loops).any(|(a, b)| {
        a.pts.iter().zip(&b.pts).any(|(p, q)| (p[0] - q[0]).abs() > 0.1 || (p[2] - q[2]).abs() > 0.1)
    }), "the surface normal tilts links as well as lifting them");
    let missed = eval_weave(&format!(
        "let flat = m.mail(Rect{{size: [30mm 30mm]}}, {{{WEAVE_FIELDS}}})\nlet skin = m.drape(flat, m.sphere({{radius: 5mm at: [100mm 0 0]}}))"
    ));
    assert_eq!(links_of(&missed, "flat").0.key(), links_of(&missed, "skin").0.key(), "links with no surface below stay put");
}

#[test]
fn drape_rests_links_over_a_printed_form() {
    let p = eval_weave(&format!(
        "let dome = m.sphere({{radius: 50mm at: [0, -30mm, 0]}})\nlet skin = m.drape(m.mail(Rect{{size: [60mm 60mm]}}, {{{WEAVE_FIELDS}}}), dome)\nlet form = m.form(skin)"
    ));
    let (skin, _) = links_of(&p, "skin");
    // every link clearance_z (0.4) over the dome at its closest (vertices here; rest_on samples the bars too)
    let gap = |l: &csg_sdf::LinkLoop| l.pts.iter().map(|q| (q[0].hypot(q[1] + 30.0).hypot(q[2]) - 50.0) - skin.radius).fold(f64::MAX, f64::min);
    for l in &skin.loops {
        let g = gap(l);
        assert!(g > 0.39 && g < 1.5, "{g}");
    }
    let Op::Field { shape, .. } = &p.graph.node(p.solid("form").unwrap().node).op else { panic!("the form is a field") };
    assert_eq!(shape.min[1], 0.0, "the form stands on the bed");
    assert!(shape.max[0] <= skin.max[0] + 1e-9 && shape.min[2] >= skin.min[2] - 1e-9);
    let inside = csg_sdf::distance(&shape.root, makepad_csg_math::Vec3d::new(0.0, 10.0, 0.0));
    assert!(inside < 0.0, "under the middle of the drape is form: {inside}");
}

#[test]
fn weave_needs_explicit_lengths_without_printer_defaults() {
    for (fields, missing) in [("", "bar"), ("bar: 0.8mm", "clearance"), ("bar: 0.8mm clearance: 0.35mm", "clearance_z")] {
        let src = format!("use mod.model.*\nlet m = mod.model\nlet cloth = m.mail(Circle{{radius: 40mm}}, {{{fields}}})\n");
        let errors = eval(&src).err().expect("missing weave defaults must fail");
        assert!(errors.iter().any(|e| e.message.contains(&format!("'{missing}' is needed here (no printer defaults)"))), "{errors:?}");
    }
}

#[test]
fn mail_mesh_is_closed() {
    let p = eval_weave(&format!("let cloth = m.mail(Rect{{size: [30mm 30mm]}}, {{{WEAVE_FIELDS}}})"));
    links_of(&p, "cloth");
    let mesh = makepad_csg_graph::Evaluator::new().mesh(&p.graph, p.solid("cloth").unwrap().node, None).unwrap();
    assert!(!mesh.triangles.is_empty());
    let mut edges = HashMap::new();
    for t in &mesh.triangles {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a, b)).or_insert(0usize) += 1;
        }
    }
    for ((a, b), count) in &edges {
        assert_eq!(*count, 1, "each directed edge occurs once");
        assert_eq!(edges.get(&(*b, *a)), Some(&1), "every edge has its opposite");
    }
}

#[test]
fn mail_reports_when_no_link_fits() {
    let src = format!("use mod.model.*\nlet m = mod.model\nlet cloth = m.mail(Circle{{radius: 5mm}}, {{{WEAVE_FIELDS}}})\n");
    let errors = eval(&src).err().expect("the region is too small");
    assert!(errors.iter().any(|e| e.message.contains("no link fits in the region: links are")), "{errors:?}");
}

#[test]
fn weave_reports_short_paths_and_regions_without_fields() {
    for (call, message) in [
        (format!("m.links([[0 0 0] [1mm 0 0]], {{{WEAVE_FIELDS}}})"), "the path is shorter than two links (pitch"),
        (format!("m.mail(m.subtract(m.box({{size: [80mm 6mm 80mm]}}), m.cylinder({{radius: 5mm height: 10mm}})), {{{WEAVE_FIELDS}}})"), "a mail region is a Shape, a Sketch or an SDF solid"),
        (format!("m.mail(m.box({{size: [60mm 1mm 60mm]}}), {{{WEAVE_FIELDS}}})"), "no link fits in the region"),
    ] {
        let errors = eval(&format!("use mod.model.*\nlet m = mod.model\nlet cloth = {call}\n")).err().expect("invalid weave input must fail");
        assert!(errors.iter().any(|e| e.message.contains(message)), "{errors:?}");
    }
}

#[test]
fn volume_mail_fills_a_solid_standing_on_the_bed() {
    let p = eval_weave(&format!("let block = m.mail(m.box({{size: [30mm 18mm 24mm] at: [0 9mm 0]}}), {{{WEAVE_FIELDS} fill: @volume}})"));
    let (links, _) = links_of(&p, "block");
    assert!(links.loops.len() > 20, "{}", links.loops.len());
    for l in &links.loops {
        // every link's lowest point is its bottom bar on the bed; all of it inside the box
        let low = l.pts.iter().map(|p| p[1]).fold(f64::MAX, f64::min);
        assert!(low < 0.61, "a link starts {low} mm up");
        assert!(l.pts.iter().all(|p| p[0].abs() <= 15.0 && p[2].abs() <= 12.0 && p[1] <= 18.0));
    }
    let steeper = eval_weave(&format!("let block = m.mail(m.box({{size: [30mm 18mm 24mm] at: [0 9mm 0]}}), {{{WEAVE_FIELDS} fill: @volume lean: 70deg}})"));
    assert_ne!(links_of(&steeper, "block").0.loops.len(), links.loops.len());
    for (call, message) in [
        (format!("m.mail(Circle{{radius: 40mm}}, {{{WEAVE_FIELDS} fill: @volume}})"), "fill: @volume fills a solid"),
        (format!("m.mail(m.box({{size: [30mm 18mm 24mm] at: [0 9mm 0]}}), {{{WEAVE_FIELDS} fill: @volume lean: 30deg}})"), "lean"),
        (format!("m.mail(m.box({{size: [30mm 1.5mm 24mm] at: [0 0.75mm 0]}}), {{{WEAVE_FIELDS} fill: @volume}})"), "no leaning link fits"),
    ] {
        let errors = eval(&format!("use mod.model.*\nlet m = mod.model\nlet cloth = {call}\n")).err().expect("invalid volume fill must fail");
        assert!(errors.iter().any(|e| e.message.contains(message)), "{call}: {errors:?}");
    }
}

/// The guide's heart: chainmail in a closed spline outline.
#[test]
fn mail_fills_a_heart() {
    let p = eval_weave(&format!("let heart = m.mail(Path{{from: [0, -35mm] segs: [SplineTo{{points: [[30mm, -5mm] [30mm, 20mm] [15mm, 30mm] [0, 18mm]]}} SplineTo{{points: [[-15mm, 30mm] [-30mm, 20mm] [-30mm, -5mm] [0, -35mm]]}}]}}, {{{WEAVE_FIELDS} angle: 15deg}})"));
    let (links, _) = links_of(&p, "heart");
    assert!(links.loops.len() > 10, "{}", links.loops.len());
}

/// A `let` of a loop as a program's last statement, and right before a root call.
#[test]
fn a_loop_let_evaluates_last_and_before_a_root() {
    let mut failed = Vec::new();
    for pegs in ["let pegs = for i in 0..3 { m.cylinder({radius: 1mm height: 5mm at: [i * 4mm, 0, 0]}) }", "let pegs = for i in 0..3 { m.cylinder({radius: 1mm height: 5mm at: [i * 4mm 0 0]}) }"] {
        for src in [
            format!("use mod.fab.*\nlet m = mod.model\nlet plate = m.box({{size: [10mm 2mm 10mm]}})\n{pegs}\n"),
            format!("use mod.fab.*\nlet m = mod.model\nlet plate = m.box({{size: [10mm 2mm 10mm]}})\n{pegs}\nplace(plate)\n"),
            format!("use mod.fab.*\nlet m = mod.model\nlet plate = m.box({{size: [10mm 2mm 10mm]}})\n{pegs}\nplace(pegs[0])\n"),
            format!("use mod.fab.*\nlet m = mod.model\n{pegs}\nlet plate = m.box({{size: [10mm 2mm 10mm]}})\nplace(plate)\n"),
        ] {
            match eval(&src) { Ok(p) if p.solid("pegs[2]").is_some() => {}, r => failed.push(format!("{:?}\n{src}", r.err())) }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n----\n"));
}

/// A solid's mesh: its volume (mm³) and its box.
fn measured(p: &Program, name: &str) -> (f64, [f64; 3], [f64; 3]) {
    let mesh = makepad_csg_graph::Evaluator::new().mesh(&p.graph, p.solid(name).unwrap_or_else(|| panic!("no {name}: {:?}", ids(p))).node, None).unwrap();
    let mut volume = 0.0;
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|i| mesh.vertices[i as usize]);
        volume += a.x * (b.y * c.z - b.z * c.y) + a.y * (b.z * c.x - b.x * c.z) + a.z * (b.x * c.y - b.y * c.x);
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for v in &mesh.vertices {
        for (k, x) in [v.x, v.y, v.z].into_iter().enumerate() {
            lo[k] = lo[k].min(x);
            hi[k] = hi[k].max(x);
        }
    }
    (volume / 6.0, lo, hi)
}

fn near(got: f64, want: f64, tol: f64, what: &str) {
    assert!((got - want).abs() <= tol, "{what}: {got} (want {want} ± {tol})");
}

fn boxed(got: ([f64; 3], [f64; 3]), want: ([f64; 3], [f64; 3]), what: &str) {
    for k in 0..3 {
        near(got.0[k], want.0[k], 1e-9, what);
        near(got.1[k], want.1[k], 1e-9, what);
    }
}

/// Rows of holes: round where the axis stands up, teardrops (45 degree tips) where it lies level.
#[test]
fn hole_rows_are_round_upright_and_teardrops_level() {
    let src = "use mod.fab.*\nlet m = mod.model\nlet plate = m.box({size: [60mm 6mm 40mm]})\nlet down = m.hole_row({from: [-20mm, 0, 0] to: [20mm, 0, 0] count: 3 diameter: 8mm length: 10mm})\nlet flat = m.subtract(plate, down)\nlet wall = m.box({size: [60mm 40mm 6mm] at: [0 0 50mm]})\nlet across = m.hole_row({from: [-20mm, 0, 50mm] to: [20mm, 0, 50mm] count: 3 diameter: 8mm length: 10mm axis: [0 0 1]})\nlet upright = m.subtract(wall, across)\nlet plain = m.hole_row({from: [0, 0, 50mm] diameter: 8mm length: 10mm axis: [0 0 1] teardrop: false})\nplace(flat)\nplace(upright)\n";
    let p = eval(src).unwrap();
    // 48 sides on the circle: n/2 sin(2 pi/n) r²; the tip adds its kite less the polygon's quarter
    let circle = 24.0 * (std::f64::consts::PI / 24.0).sin() * 16.0;
    let tip = 16.0 - 12.0 * 0.5 * (std::f64::consts::PI / 24.0).sin() * 16.0;
    let (v, lo, hi) = measured(&p, "flat");
    near(v, 14400.0 - 3.0 * 6.0 * circle, 0.5, "plate with upright holes");
    boxed((lo, hi), ([-30.0, -3.0, -20.0], [30.0, 3.0, 20.0]), "the plate keeps its box");
    let (v, ..) = measured(&p, "upright");
    near(v, 14400.0 - 3.0 * 6.0 * (circle + tip), 0.5, "wall with teardrops");
    let (_, lo, hi) = measured(&p, "across");
    // the tip ends r sqrt 2 over the centre: the bridge it makes is a point
    boxed((lo, hi), ([-24.0, -4.0, 45.0], [24.0, 4.0 * std::f64::consts::SQRT_2, 55.0]), "the row's box");
    let (_, lo, hi) = measured(&p, "plain");
    near(hi[1], 4.0, 1e-9, "a hole told not to be a teardrop");
    near(lo[0], -4.0, 1e-9, "one hole sits at its from");
    for bad in ["axis: [0 0 0]", "count: 0"] {
        let src = format!("use mod.fab.*\nlet m = mod.model\nlet h = m.hole_row({{from: [0 0 0] diameter: 4mm length: 5mm {bad}}})\nplace(h)\n");
        assert!(eval(&src).is_err(), "{bad}");
    }
}

/// A pocket into a level face takes a 45 degree roof over its top edge; one into an upright
/// face none. Neither cuts past its depth or outside its opening.
#[test]
fn pockets_keep_their_depth_and_roof_level_faces() {
    let src = "use mod.fab.*\nlet m = mod.model\nlet wall = m.box({size: [60mm 40mm 6mm]})\nlet scoop = m.pocket({center: [0, 0, 3mm] size: [30mm 16mm] depth: 4mm axis: [0, 0, -1] corner: 0.2mm})\nlet front = m.subtract(wall, scoop)\nlet plate = m.box({size: [60mm 6mm 40mm] at: [0 0 60mm]})\nlet dish = m.pocket({center: [0, 3mm, 60mm] size: [30mm 16mm] depth: 4mm axis: [0, -1, 0] corner: 0.2mm})\nlet top = m.subtract(plate, dish)\nplace(front)\nplace(top)\n";
    let p = eval(src).unwrap();
    // the box less the opening's rounded floor edges; the roof's triangle (4 by 4 over the top
    // edge) runs the straight length
    let floor_edges = 2.0 * (30.0 + 16.0) * 0.04 * (1.0 - std::f64::consts::FRAC_PI_4);
    let (v, lo, hi) = measured(&p, "top");
    near(v, 14400.0 - 30.0 * 16.0 * 4.0 + floor_edges, 2.0, "a pocket from above");
    boxed((lo, hi), ([-30.0, -3.0, 40.0], [30.0, 3.0, 80.0]), "the plate keeps its box");
    let (v, lo, hi) = measured(&p, "front");
    near(v, 14400.0 - 30.0 * 16.0 * 4.0 + floor_edges - 8.0 * 29.6, 2.0, "a pocket into a wall, roofed");
    boxed((lo, hi), ([-30.0, -20.0, -3.0], [30.0, 20.0, 3.0]), "the wall keeps its box");
    let (_, lo, hi) = measured(&p, "scoop");
    // the roof climbs depth over the top edge at the face; the body runs 4mm in and past the face
    near(hi[1], 12.0, 1e-9, "roof top");
    near(lo[2], -1.0, 1e-9, "pocket floor");
    near(lo[1], -8.0, 1e-9, "pocket bottom edge");
    near(hi[0], 15.0, 1e-9, "pocket side");
    let flat = eval("use mod.fab.*\nlet m = mod.model\nlet s = m.pocket({center: [0, 0, 3mm] size: [30mm 16mm] depth: 4mm axis: [0, 0, -1] roof: false})\nplace(s)\n").unwrap();
    near(measured(&flat, "s").2[1], 8.0, 1e-9, "a pocket told to take no roof");
    assert!(eval("use mod.fab.*\nlet m = mod.model\nlet s = m.pocket({center: [0 0 0] size: [30mm 16mm] depth: 4mm})\nplace(s)\n").is_err(), "a pocket needs its axis");
}

/// An operand joins a boolean's second list: a list takes it at its end, a single operand
/// becomes a list, a name of a list takes it in that list's `let`.
#[test]
fn an_operand_joins_the_cutters() {
    let src = |cut: &str| format!("use mod.fab.*\nlet m = mod.model\nlet a = m.box({{size: [20mm 4mm 20mm]}})\nlet b = m.cylinder({{radius: 1mm height: 8mm}})\nlet e = m.cylinder({{radius: 1mm height: 8mm at: [5mm 0 0]}})\n{cut}\nplace(c)\n");
    let edit = edit::Edit::Operand { solid: "c".into(), code: "e".into() };
    for (before, after) in [
        ("let c = m.subtract(a, [b a])", "let c = m.subtract(a, [b a e])"),
        ("let c = m.subtract(a, [b])", "let c = m.subtract(a, [b, e])"),
        ("let c = m.subtract(a, b)", "let c = m.subtract(a, [b e])"),
        ("let cutters = [b b]\nlet c = m.subtract(a, cutters)", "let cutters = [b b e]\nlet c = m.subtract(a, cutters)"),
    ] {
        let p = eval(&src(before)).unwrap();
        let out = edit::apply(&p, &edit).unwrap();
        assert_eq!(out, src(after), "{before}");
        eval(&out).unwrap();
    }
    let p = eval(&src("let c = m.fillet(a, 1mm)")).unwrap();
    assert!(edit::apply(&p, &edit).is_err(), "a fillet has no operand list");
}

/// Deleting a solid takes it out of every use, as one edit: an operand list loses it, a
/// boolean left with nothing to do becomes what is left (under its own name, a named base's
/// code and comments where they were), a builder without its base and a statement about it go
/// too. Each result evaluates, and the deleted solid is gone from it.
#[test]
fn deleting_a_used_solid_takes_it_out_of_its_uses() {
    let head = "use mod.fab.*\nlet m = mod.model\n";
    let del = |body: &str, id: &str| -> Result<String, String> {
        let p = eval(&format!("{head}{body}")).unwrap_or_else(|d| panic!("{body}: {d:?}"));
        let out = edit::apply(&p, &edit::Edit::Remove { solid: id.into() })?;
        let after = eval(&out).unwrap_or_else(|d| panic!("deleting '{id}' leaves a program that does not evaluate: {d:?}\n{out}"));
        assert!(after.solid(id).is_none() && !out.contains(&format!("let {id} ")), "'{id}' is gone:\n{out}");
        Ok(out.strip_prefix(head).unwrap().to_string())
    };
    // The vase: a cut box out of the carve's cutters, then the mouth; the carve without
    // cutters is its base, named `vase`, its meshing cell handed on.
    let vase = "let body = m.ellipsoid({radii: [20mm 30mm 20mm]})\nlet lip = m.torus({radius: 8mm tube: 2mm at: [0, 28mm, 0]})\n// The lip blended on.\nlet rimmed = m.blend([body lip], 1.5mm)\nlet mouth = m.cylinder({radius: 6mm height: 20mm at: [0, 30mm, 0]})\nlet cut1 = m.box({size: [10mm 10mm 10mm] at: [-15mm 0 0]})\nlet vase = m.carve(rimmed, [mouth cut1], 0, {cell: 0.9mm})\nplace(vase)\n";
    let one = del(vase, "cut1").unwrap();
    assert_eq!(one, vase.replace("let cut1 = m.box({size: [10mm 10mm 10mm] at: [-15mm 0 0]})\n", "").replace("[mouth cut1]", "[mouth]"));
    let none = del(&one, "mouth").unwrap();
    assert_eq!(none, "let body = m.ellipsoid({radii: [20mm 30mm 20mm]})\nlet lip = m.torus({radius: 8mm tube: 2mm at: [0, 28mm, 0]})\n// The lip blended on.\nlet vase = m.blend([body lip], 1.5mm, {cell: 0.9mm})\nplace(vase)\n");
    // A blend of two that loses one (inline) is the other; a union that loses its first is
    // its rest, inside the op that holds it.
    let knob = "let ball = m.sphere({radius: 6mm})\nlet knob = m.blend([ball m.cylinder({radius: 2mm height: 9mm})], 2mm)\nplace(knob)\n";
    assert_eq!(del(knob, "knob.cylinder").unwrap(), "let knob = m.sphere({radius: 6mm})\nplace(knob)\n");
    let frame = "let top = m.box({size: [30mm 2mm 10mm]})\nlet left = m.box({size: [2mm 9mm 10mm] at: [-14mm 0 0]})\nlet right = m.box({size: [2mm 9mm 10mm] at: [14mm 0 0]})\nlet bolt = m.cylinder({radius: 1mm height: 20mm})\nlet frame = m.subtract(m.union(top, [left right]), bolt)\nplace(frame)\n";
    let no_right = del(frame, "right").unwrap();
    assert!(no_right.contains("m.subtract(m.union(top, [left]), bolt)"), "{no_right}");
    assert!(del(&no_right, "top").unwrap().contains("let frame = m.subtract(left, bolt)\n"));
    // A solid used twice leaves both; a load on it goes, from a list or with its statement;
    // a subtract's base used nowhere else keeps its own `let` under the subtract's name.
    let twice = "let hole = m.cylinder({radius: 2mm height: 30mm})\nlet slot = m.box({size: [8mm 30mm 2mm]})\nlet plate = m.box({size: [40mm 6mm 40mm]})\nlet a = m.subtract(plate, [hole slot])\nlet b = m.subtract(m.box({size: [20mm 6mm 20mm] at: [40mm 0 0]}), hole)\nplace(a)\nplace(b)\nm.fixed(hole)\nm.fixed([hole slot])\n";
    assert_eq!(del(twice, "hole").unwrap(), "let slot = m.box({size: [8mm 30mm 2mm]})\nlet plate = m.box({size: [40mm 6mm 40mm]})\nlet a = m.subtract(plate, [slot])\nlet b = m.box({size: [20mm 6mm 20mm] at: [40mm 0 0]})\nplace(a)\nplace(b)\nm.fixed([slot])\n");
    // Without its base a subtract goes, and what it fed goes with it, its placement too.
    assert_eq!(del("let a = m.box({size: [9mm 9mm 9mm]})\nlet b = m.cylinder({radius: 2mm height: 30mm})\nlet c = m.subtract(a, b)\nlet d = m.fillet(c, 0.5mm)\nplace(d, at: [10mm, 20mm])\n", "a").unwrap(), "let b = m.cylinder({radius: 2mm height: 30mm})\n");
    // A collapsed op's placement goes onto its base; a base placed too refuses, as does a use
    // that is no operand, naming its line.
    let placed = "let a = m.box({size: [9mm 9mm 9mm]})\nlet b = m.cylinder({radius: 2mm height: 30mm})\nlet c = m.subtract(a, b, {at: [5mm 0 0]})\nplace(c)\n";
    assert_eq!(del(placed, "b").unwrap(), "let c = m.box({size: [9mm 9mm 9mm] at: [5mm 0 0]})\nplace(c)\n");
    assert!(del(&placed.replace("[9mm 9mm 9mm]", "[9mm 9mm 9mm] at: [1mm 0 0]"), "b").unwrap_err().contains("its own `at`"));
    let fn_use = "fn doubled(s) {\n    return m.union(s, [m.box({size: [1mm 1mm 1mm]})])\n}\nlet cut1 = m.box({size: [9mm 9mm 9mm]})\nlet two = doubled(cut1)\nplace(two)\n";
    let why = del(fn_use, "cut1").unwrap_err();
    assert!(why.contains("line 7") && why.contains("doubled(cut1)"), "{why}");
}

/// The foam of m.voronoi runs on its web's own cells: where a strut reaches the skin it lies on
/// the web's faces, so no strut ends in one of the skin's holes and foam and web are one piece.
#[test]
fn voronoi_says_when_its_skin_fills_the_part() {
    let notes = |source: &str, opts: &str| -> Vec<String> {
        let p = eval(&format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n{source}\nlet v = m.voronoi(b, {{size: 10mm strut: 1.2mm {opts}}})\nplace(v)\n")).unwrap_or_else(|e| panic!("{opts}: {e:?}"));
        p.notes.iter().map(|n| n.message.clone()).collect()
    };
    let plate = "let b = m.box({size: [40mm 4mm 40mm]})";
    let said = notes(plate, "");
    assert!(said.len() == 1 && said[0].contains("skin fills this part, no foam") && said[0].contains("2.0 mm deep") && said[0].contains("2.4 mm"), "a 4 mm plate under a 2.4 mm skin: {said:?}");
    assert!(notes(plate, "skin: 1.5mm").is_empty(), "a thinner skin leaves room");
    assert!(notes(plate, "core: @none").is_empty() && notes(plate, "core: @solid").is_empty(), "only foam is asked for");
    // walls of 6 mm in a box whose 4k points lie about 3.3 mm apart, none on a wall's middle
    let l = "let block = m.box({size: [64mm 52mm 46mm] at: [0 26mm 0]})\nlet space = m.box({size: [90mm 70mm 70mm] at: [0 41mm 12mm]})\nlet b = m.carve(block, space, 0.5mm)";
    assert!(notes(l, "").is_empty(), "3 mm deep walls leave room under a 2.4 mm skin");
}

#[test]
fn voronoi_foam_is_one_piece_with_its_web() {
    use makepad_csg_math::Vec3d;
    let (h, n) = (0.75, [88usize, 16, 64]);
    let at = |x, y, z| x + n[0] * (z + n[2] * y);
    let mut ps = Vec::with_capacity(n[0] * n[1] * n[2]);
    for y in 0..n[1] {
        for z in 0..n[2] {
            for x in 0..n[0] {
                ps.push(Vec3d::new((x as f64 - 43.5) * h, (y as f64 - 7.5) * h, (z as f64 - 31.5) * h));
            }
        }
    }
    let p = eval("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet b = m.box({size: [64mm 10mm 46mm]})\nlet v = m.voronoi(b, {size: 10mm strut: 1.5mm seed: 2})\nplace(v)\n").unwrap();
    let Op::Field { shape, .. } = &p.graph.node(p.solid("v").unwrap().node).op else { panic!("not a field") };
    let mut d = vec![0.0; ps.len()];
    shape.field().distance_many(&ps, &mut d);
    let mut left: Vec<bool> = d.iter().map(|d| *d < 0.0).collect();
    let total = left.iter().filter(|s| **s).count();
    let first = left.iter().position(|s| *s).unwrap();
    let mut stack = vec![first];
    left[first] = false;
    let mut reached = 0;
    while let Some(i) = stack.pop() {
        reached += 1;
        let (y, r) = (i / (n[0] * n[2]), i % (n[0] * n[2]));
        let (z, x) = (r / n[0], r % n[0]);
        for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
            let (u, v, w) = (x as i64 + dx, y as i64 + dy, z as i64 + dz);
            if (0..n[0] as i64).contains(&u) && (0..n[1] as i64).contains(&v) && (0..n[2] as i64).contains(&w) {
                let j = at(u as usize, v as usize, w as usize);
                if left[j] {
                    left[j] = false;
                    stack.push(j);
                }
            }
        }
    }
    assert_eq!(reached, total, "a piece of the foam or web is loose");
}

/// A sweep's `scale` is its size along the way: an exact sweep's one per path point, or the end's
/// reached evenly; no sweep takes it again as a placement scale.
#[test]
fn a_sweeps_scale_sizes_it_along_the_way() {
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet path = [[0, 0, 0] [0, 10mm, 0] [0, 40mm, 0]]\nlet lock = m.brep.sweep({sketch: Circle{radius: 1mm} path: path scale: [1.2 0.8 0.5]})\nlet horn = m.brep.sweep({sketch: Circle{radius: 1mm} path: path scale: 0.2})\nlet rod = m.sweep({sketch: Circle{radius: 1mm} path: path scale: 0.5})\nplace(lock)\nplace(horn)\nplace(rod)\n";
    let p = eval(src).unwrap();
    let op = |name: &str| &p.graph.node(p.solid(name).unwrap().node).op;
    let Op::Brep { op: BrepOp::Sweep { path, scale, .. }, .. } = op("lock") else { panic!("lock: {:?}", op("lock")) };
    assert_eq!((path.len(), scale.as_slice()), (3, &[1.2, 0.8, 0.5][..]));
    let Op::Brep { op: BrepOp::Sweep { scale, .. }, .. } = op("horn") else { panic!("horn: {:?}", op("horn")) };
    assert_eq!(scale.len(), 3);
    assert!((scale[0] - 1.0).abs() < 1e-12 && (scale[1] - 0.8).abs() < 1e-12 && (scale[2] - 0.2).abs() < 1e-12, "{scale:?}");
    let Op::Sweep { scale, .. } = op("rod") else { panic!("rod: {:?}", op("rod")) };
    assert_eq!(*scale, 0.5);
}

/// A tube is a distance-field part through its points at their radii: one curve, or many in one
/// part; a radius below zero or a tube with no points is an error.
#[test]
fn a_tube_runs_through_its_points_at_their_radii() {
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet one = m.tube({points: [[0, 0, 0, 2mm] [0, 10mm, 0, 1mm]]})\nlet many = m.tube({curves: [[[0, 0, 0, 2mm] [0, 10mm, 0, 1mm]] [[20mm, 0, 0, 1mm]]] blend: 2mm})\nplace(one)\nplace(many)\n";
    let p = eval(src).unwrap();
    let field = |name: &str| {
        let Op::Field { shape, .. } = &p.graph.node(p.solid(name).unwrap().node).op else { panic!("{name} is not a field") };
        shape.root.clone()
    };
    for (name, curves) in [("one", 1), ("many", 2)] {
        let root = field(name);
        let SdfNode::Id { of, .. } = &root else { panic!("{name}: {root:?}") };
        let SdfNode::Tubes(t) = of.as_ref() else { panic!("{name}: {of:?}") };
        assert_eq!((t.curves.len(), t.blend), (curves, if curves == 2 { 2.0 } else { 0.0 }), "{name}: the blend in millimetres");
        // Kernel points are millimetres: halfway up, as deep as the radius there.
        let d = csg_sdf::distance(&root, makepad_csg_math::Vec3d::new(0.0, 5.0, 0.0));
        assert!((d + 1.5).abs() < 1e-6, "{name}: {d}");
    }
    let many = field("many");
    assert!((csg_sdf::distance(&many, makepad_csg_math::Vec3d::new(23.0, 0.0, 0.0)) - 2.0).abs() < 1e-9, "a one-point tube is a ball");
    for bad in ["m.tube({points: [[0, 0, 0, -1mm] [0, 9mm, 0, 1mm]]})", "m.tube({points: []})", "m.tube({})", "m.tube({points: [[0, 0, 0, 1mm]] blend: -1mm})"] {
        let src = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet t = {bad}\nplace(t)\n");
        assert!(eval(&src).is_err(), "{bad} is not a tube");
    }
}

/// Two loops of a bow, each a cage of `n` rings of 4 points built in loops by a fn.
const BOW_FN: &str = "fn bowLoop(k, d, u, n, len) {
    let points = []
    let faces = []
    let v = [u[1] * d[2] - u[2] * d[1], u[2] * d[0] - u[0] * d[2], u[0] * d[1] - u[1] * d[0]]
    for i in 0..n {
        let t = i / (n - 1)
        let c = [k[0] + d[0] * len * t, k[1] + d[1] * len * t, k[2] + d[2] * len * t]
        let r = 4mm * (1 - 0.5 * t)
        for q in 0..4 {
            let a = q * 90deg
            points.push([c[0] + (u[0] * cos(a) + v[0] * sin(a)) * r, c[1] + (u[1] * cos(a) + v[1] * sin(a)) * r, c[2] + (u[2] * cos(a) + v[2] * sin(a)) * r])
        }
    }
    for i in 0..n - 1 {
        for q in 0..4 {
            let j = (q + 1) % 4
            faces.push([4 * i + q, 4 * i + 4 + q, 4 * i + 4 + j, 4 * i + j])
        }
    }
    faces.push([0, 1, 2, 3])
    faces.push([4 * n - 1, 4 * n - 2, 4 * n - 3, 4 * n - 4])
    return m.cage({points: points faces: faces})
}
let bowA = bowLoop([-21mm, 115mm, 9mm], [-0.44, 0.84, -0.3], [0, 0.34, 0.94], 6, 21mm)
let bowB = bowLoop([21mm, 115mm, 9mm], [0.44, 0.84, -0.3], [0, 0.34, 0.94], 6, 21mm)
place(bowA)
place(bowB)
";

/// A cage of three rings of 66 points (198), its radius a param.
const RING_CAGE: &str = "let n = 66
let points = []
let faces = []
let r = param(12mm, min: 5mm, max: 20mm)
for ring in 0..3 {
    for k in 0..n {
        let a = k * 360deg / n
        points.push([r * (1 + 0.2 * ring) * cos(a), ring * 10mm, r * sin(a)])
    }
}
for ring in 0..2 {
    for k in 0..n {
        let j = (k + 1) % n
        faces.push([n * ring + k, n * ring + j, n * ring + n + j, n * ring + n + k])
    }
}
let bottom = []
let top = []
for k in 0..n {
    bottom.push(n - 1 - k)
    top.push(2 * n + k)
}
faces.push(bottom)
faces.push(top)
let shell = m.cage({points: points faces: faces})
place(shell)
";

/// Cages built in loops take a sliver of the script's budget, and edits of a part a fn makes
/// edit its call: the fn's code is every call's, and a call of the fn put inside it (a replace
/// of the cage it returns) would never end.
#[test]
fn cages_built_in_loops_and_fns_evaluate_far_under_the_budget() {
    let pre = "use mod.model.*\nuse mod.fab.*\nuse mod.math.*\nlet m = mod.model\n";
    let horn = format!("{pre}let points = []\nlet faces = []\nfor i in 0..7 {{\n    let s = 9mm - i * 1.2mm\n    for c in [[s, 0], [0, s], [-s, 0], [0, -s]] {{ points.push([i * i * 0.5mm + c[0], i * 8mm, c[1]]) }}\n}}\nfor i in 0..6 {{\n    for k in 0..4 {{\n        let j = (k + 1) % 4\n        faces.push([4 * i + k, 4 * i + 4 + k, 4 * i + 4 + j, 4 * i + j])\n    }}\n}}\nfaces.push([0, 1, 2, 3])\nfaces.push([27, 26, 25, 24])\nlet horn = m.cage({{points: points faces: faces}})\nplace(horn)\n");
    let well_under = |what: &str, p: &Program| {
        eprintln!("{what}: {} instructions of {EVAL_LIMIT}", p.instructions);
        assert!(p.instructions > 0 && p.instructions * 100 < EVAL_LIMIT as u64, "{what} ran {} instructions", p.instructions);
    };
    well_under("horn", &eval(&horn).unwrap());

    // A parameter edit evaluates again at the same cost.
    let ring = format!("{pre}{RING_CAGE}");
    let first = eval(&ring).unwrap();
    well_under("ring of 198 points", &first);
    let assets = Assets::new();
    let mut opts = Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &NO_INPUTS, unit: 1000.0, roots: ROOTS, overrides: HashMap::new(), weave: None, solved: HashMap::new() };
    opts.overrides.insert(first.params[0].call.loc.unwrap(), 0.015);
    let edited = evaluate_with_names(&ring, &opts, Some(first.names.clone())).unwrap();
    assert_eq!(edited.params[0].call.value, 0.015);
    well_under("ring of 198 points, edited", &edited);
    assert_eq!(edited.instructions, first.instructions);

    // Replacing a fn's part replaces its call; the fn and the other call stay as they were.
    let bows = eval(&format!("{pre}{BOW_FN}")).unwrap();
    well_under("bow", &bows);
    assert_eq!(ids(&bows), vec!["bowA", "bowB"]);
    let call = "bowLoop([-20mm, 116mm, 9mm], [-0.4, 0.85, -0.3], [0, 0.34, 0.94], 7, 22mm)";
    let new = edit::apply(&bows, &edit::Edit::Replace { solid: "bowA".into(), code: call.into() }).unwrap();
    assert_eq!(new, bows.source().replacen("bowLoop([-21mm, 115mm, 9mm], [-0.44, 0.84, -0.3], [0, 0.34, 0.94], 6, 21mm)", call, 1));
    let again = eval(&new).unwrap();
    well_under("bow, replaced", &again);
    assert_eq!(ids(&again), vec!["bowA", "bowB"]);
    // Removing or renaming it takes its `let`.
    let gone = edit::apply(&bows, &edit::Edit::Remove { solid: "bowA".into() }).unwrap();
    assert!(!gone.contains("bowA") && gone.contains("fn bowLoop") && gone.contains("return m.cage("), "{gone}");
    assert_eq!(ids(&eval(&gone).unwrap()), vec!["bowB"]);
    let renamed = edit::apply(&bows, &edit::Edit::Rename { solid: "bowA".into(), to: "bowLeft".into() }).unwrap();
    assert_eq!(ids(&eval(&renamed).unwrap()), vec!["bowLeft", "bowB"]);

    // A part made on the way inside a fn is the fn's code: replace and remove refuse it.
    let legs = eval(&format!("{pre}fn leg(x) {{\n    let foot = m.box({{size: [4mm 2mm 6mm] at: [x 0 0]}})\n    return m.union(foot, m.box({{size: [3mm 20mm 3mm] at: [x 10mm 0]}}))\n}}\nlet left = leg(-10mm)\nlet right = leg(10mm)\nplace(left)\nplace(right)\n")).unwrap();
    for e in [edit::Edit::Replace { solid: "left.foot".into(), code: "m.sphere({radius: 2mm})".into() }, edit::Edit::Remove { solid: "left.foot".into() }] {
        let err = edit::apply(&legs, &e).unwrap_err();
        assert!(err.contains("made inside leg(..)"), "{err}");
    }

    // A fn that calls itself runs the budget out, and the error says what that means.
    let Err(err) = eval(&format!("{pre}fn spin(n) {{\n    for i in 0..5000 {{ }}\n    return spin(n + 1)\n}}\nlet s = spin(0)\n")) else { panic!("a fn that calls itself evaluated") };
    assert!(err.iter().any(|d| d.message.contains("calls itself")), "{err:?}");
}

/// A chain of blends nested in blends (a figure's limbs into its body), each a cage blended into
/// the last: each blend takes the ones under it as lowered, so twice the chain compiles in about
/// twice the time (lowered again for each blend round it, it took four times).
#[test]
fn nested_blends_lower_each_part_once() {
    let six = "[0 1 2 3] [4 7 6 5] [0 4 5 1] [1 5 6 2] [2 6 7 3] [3 7 4 0]";
    let cube = "[[-4mm, 0, -4mm] [4mm, 0, -4mm] [4mm, 0, 4mm] [-4mm, 0, 4mm] [-4mm, 8mm, -4mm] [4mm, 8mm, -4mm] [4mm, 8mm, 4mm] [-4mm, 8mm, 4mm]]";
    let chain = |n: usize| {
        // five levels: each cage's field costs more than the rest of the compile
        let mut s = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet b0 = m.cage({{points: {cube} faces: [{six}] levels: 5}})\n");
        for k in 1..=n {
            s += &format!("let b{k} = m.blend([b{} m.cage({{points: {cube} faces: [{six}] levels: 5 at: [{}mm, 0, 0]}})], 1mm)\n", k - 1, k * 6);
        }
        s + &format!("place(b{n})\n")
    };
    let (six_deep, twelve_deep) = (chain(6), chain(12));
    let time = |src: &str, n: usize| {
        let t = std::time::Instant::now();
        let p = eval(src).unwrap();
        assert_eq!(p.solids.len(), 2 * n + 1);
        t.elapsed().as_secs_f64()
    };
    // the best of five, taken in turns so that other work on the machine weighs on both alike
    let (mut short, mut long) = (f64::INFINITY, f64::INFINITY);
    for _ in 0..5 {
        short = short.min(time(&six_deep, 6));
        long = long.min(time(&twelve_deep, 12));
    }
    eprintln!("blended cages: 6 deep {:.1} ms, 12 deep {:.1} ms", short * 1e3, long * 1e3);
    assert!(long < 2.5 * short, "12 deep took {:.1} ms, 6 deep {:.1} ms: the nested blends lower their parts again", long * 1e3, short * 1e3);
    // The field is the same as lowered whole: each cage's surface where it was.
    let p = eval(&chain(3)).unwrap();
    let Op::Field { shape, .. } = &p.graph.node(p.solid("b3").unwrap().node).op else { panic!("b3 is not a field") };
    let d = |x: f64| csg_sdf::distance(&shape.root, makepad_csg_math::Vec3d::new(x, 4.0, 0.0));
    assert!(d(18.0) < -1.0 && d(-6.0) > 1.0 && d(30.0) > 1.0, "{} {} {}", d(18.0), d(-6.0), d(30.0));
}

/// A value of a part a fn makes is set at the call that made it: the fn's code is every call's,
/// so an edit of one call's part leaves the other calls byte for byte as they were, and a value
/// the fn writes itself is refused with what to do.
#[test]
fn a_fn_parts_values_are_set_at_its_call_and_the_other_calls_stay() {
    let pre = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";
    let src = format!("{pre}fn post(r, at) {{\n    return m.sphere({{radius: r at: at segments: 24}})\n}}\nfn twin(r) {{\n    return m.union(post(r, [0mm, 0mm, 0mm]), post(2mm, [6mm, 0mm, 0mm]))\n}}\nlet a = post(3mm, [0mm, 0mm, 0mm])\nlet b = post(3mm, [10mm, 0mm, 0mm])\nlet t = twin(1mm)\nlet row = for k in 0..2 {{ post(1mm, [k * 5mm, 20mm, 0mm]) }}\nplace(a)\nplace(b)\nplace(t)\n");
    let p = eval(&src).unwrap();
    let set = |site: &str, value: &str| edit::apply(&p, &edit::Edit::Set { site: site.into(), value: value.into() });
    let a = "let a = post(3mm, [0mm, 0mm, 0mm])";
    let only_a = |new_a: &str| src.replacen(a, new_a, 1);
    assert_eq!(set("a.radius", "4mm").unwrap(), only_a("let a = post(4mm, [0mm, 0mm, 0mm])"));
    assert_eq!(set("a.at.0", "2mm").unwrap(), only_a("let a = post(3mm, [2mm, 0mm, 0mm])"));
    // The call's arguments by the fn's parameters, and a placement written whole.
    assert_eq!(set("a.r", "5mm").unwrap(), only_a("let a = post(5mm, [0mm, 0mm, 0mm])"));
    let moved = edit::apply(&p, &edit::Edit::Field { solid: "a".into(), field: "at".into(), code: "[1mm, 2mm, 3mm]".into() }).unwrap();
    assert_eq!(moved, only_a("let a = post(3mm, [1mm, 2mm, 3mm])"));
    assert_eq!(edit::site_span(&p, "b.radius").map(|r| &src[r]), Ok("3mm"));
    assert!(edit::site_span(&p, "b.radius").unwrap().start > src.find("let b").unwrap());
    // What the fn writes itself is every call's: refused, naming the fn.
    for err in [set("a.segments", "32").unwrap_err(), edit::apply(&p, &edit::Edit::Unset { solid: "a".into(), field: "segments".into() }).unwrap_err()] {
        assert!(err.contains("post(..)") && err.contains("parameter"), "{err}");
    }
    // Through a fn that calls the fn: its parameter is followed out to the top-level call.
    let inner: Vec<&str> = p.solids.iter().filter(|s| s.proto == "Sphere" && s.call.as_ref().is_some_and(|c| src[c.span.clone()].starts_with("twin"))).map(|s| s.id.as_str()).collect();
    let (passed, own): (Vec<_>, Vec<_>) = inner.iter().map(|id| set(&format!("{id}.radius"), "1.5mm")).partition(Result::is_ok);
    assert_eq!(passed, vec![Ok(src.replacen("twin(1mm)", "twin(1.5mm)", 1))], "{:?}", ids(&p));
    let [Err(err)] = own.as_slice() else { panic!("{own:?}") };
    assert!(err.contains("twin(..)") && err.contains("parameter"), "{err}");
    // A call in a loop passes every pass the same arguments.
    let row = p.solids.iter().find(|s| s.id.starts_with("row")).unwrap();
    let err = set(&format!("{}.radius", row.id), "2mm").unwrap_err();
    assert!(err.contains("loop"), "{err}");
    // Each edit evaluates, the other parts as they were.
    let edited = eval(&set("a.radius", "4mm").unwrap()).unwrap();
    assert_eq!(ids(&edited), ids(&p));
}

/// `m.offset` of a field is a field grown by the distance, and leaves the feature's place in the
/// script where it was; of anything else it's a mesh of the source grown.
#[test]
fn an_offset_follows_a_field_or_a_mesh_and_leaves_its_source_alone() {
    let p = eval("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet arm = m.box({size: [10mm 2mm 2mm]})\nlet body = m.blend([arm m.sphere({radius: 5mm})], 1mm)\nlet around = m.offset(arm, 1mm)\nplace(body)\n").unwrap();
    let (arm, body, around) = (p.solid("arm").unwrap(), p.solid("body").unwrap(), p.solid("around").unwrap());
    assert_eq!(around.proto, "Offset");
    assert!(around.parent.is_none(), "it stands by itself");
    assert_eq!(arm.parent.map(|i| p.solids[i].id.as_str()), Some(body.id.as_str()), "the arm is still the body's");
    let flat = eval("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet hole = m.extrude({sketch: Circle{radius: 2mm} height: 4mm})\nlet around = m.offset(hole, 1mm)\nplace(around)\n");
    assert!(flat.is_ok(), "a mesh offsets too: {:?}", flat.err());
    let wrong = eval("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet a = m.offset(m.box({size: [1mm 1mm 1mm]}))\nplace(a)\n");
    assert!(wrong.is_err(), "it needs its distance");
}

/// A top-level `let` that names a value and no solid is edited by its name: `replace` takes its
/// expression (a param, a multi-line `for`), `rename` its readers too, `remove` only with no use
/// left it cannot take out. A `fn` is no such `let`.
#[test]
fn a_value_let_takes_replace_rename_and_remove() {
    use edit::Edit;
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet w = 30mm\nlet d = w / 2\nlet spare = 1mm\nlet rim = [[0 0] [w 0] [w d]]\nlet a = m.box({size: [w d 10mm]})\nfn twice(x) { return x * 2 }\nplace(a)\n";
    let p = eval(src).unwrap();
    let slider = "param(30mm, min: 20mm, max: 40mm, label: \"Width\")";
    let new = edit::apply(&p, &Edit::Replace { solid: "w".into(), code: slider.into() }).unwrap();
    assert_eq!(new, src.replace("let w = 30mm", &format!("let w = {slider}")), "only the value's bytes change");
    let after = eval(&new).unwrap();
    assert!(after.params.iter().any(|q| q.name.as_deref() == Some("w")), "the value is a param now");
    let many_lines = "for i in 0..4 {\n    if i < 2 {\n        [i * w, 0]\n    } else {\n        [(3 - i) * w, d]\n    }\n}";
    let new = edit::apply(&after, &Edit::Replace { solid: "rim".into(), code: many_lines.into() }).unwrap();
    assert!(new.contains(&format!("let rim = {many_lines}\nlet a =")), "{new}");
    eval(&new).unwrap();
    // A solid keeps its own path: its builder call is what goes, the `let` stays.
    let solid = edit::apply(&p, &Edit::Replace { solid: "a".into(), code: "m.sphere({radius: 3mm})".into() }).unwrap();
    assert_eq!(solid, src.replace("m.box({size: [w d 10mm]})", "m.sphere({radius: 3mm})"));
    let err = edit::apply(&p, &Edit::Replace { solid: "twice".into(), code: "1".into() }).unwrap_err();
    assert!(err.contains("no solid 'twice'"), "{err}");
    let renamed = edit::apply(&p, &Edit::Rename { solid: "w".into(), to: "width".into() }).unwrap();
    assert!(renamed.contains("let width = 30mm\nlet d = width / 2") && renamed.contains("[width d 10mm]"), "{renamed}");
    eval(&renamed).unwrap();
    let gone = edit::apply(&p, &Edit::Remove { solid: "spare".into() }).unwrap();
    assert_eq!(gone, src.replace("let spare = 1mm\n", ""));
    let refused = edit::apply(&p, &Edit::Remove { solid: "w".into() }).unwrap_err();
    assert!(refused.contains("line 5"), "a use it cannot take out names its line: {refused}");
}

/// A name no solid has is refused in a few words, however many solids the script makes: each
/// name once, a list's parts and a loop's anonymous ones folded, and the values named too.
#[test]
fn the_no_solid_refusal_stays_short_and_names_values() {
    let values: String = (0..120).map(|i| format!("let value_{i} = {i}mm\n")).collect();
    let src = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet w = 30mm\nlet posts = for i in 0..200 {{ m.box({{size: [w 1mm 1mm] at: [i * 2mm 0 0]}}) }}\nlet red = Material{{albedo: [0.8 0.05 0.05]}}\nfn peg(i) {{\n    return m.box({{size: [1mm 1mm 1mm] at: [0 i * 2mm 0]}})\n}}\nlet base = m.sphere({{radius: 1mm}})\nfor i in 0..200 {{ place(peg(i)) }}\n{values}place(base)\n");
    let p = eval(&src).unwrap();
    let anonymous: Vec<&str> = p.solids.iter().map(|s| s.id.as_str()).filter(|id| id.starts_with("box@")).collect();
    assert!(anonymous.len() >= 200 && anonymous.iter().all(|id| *id == anonymous[0]), "a loop's parts share one id: {:?}", p.solids.iter().rev().map(|s| s.id.as_str()).take(4).collect::<Vec<_>>());
    let err = edit::apply(&p, &edit::Edit::Field { solid: "tail".into(), field: "radius".into(), code: "1mm".into() }).unwrap_err();
    assert!(err.len() < 800, "{} bytes: {err}", err.len());
    assert!(err.starts_with("no solid 'tail'"), "{err}");
    assert!(err.contains("base") && err.contains("posts[..]") && !err.contains("posts[1]"), "a list's parts by the list: {err}");
    assert_eq!(err.matches(anonymous[0]).count(), 1, "the loop's parts once: {err}");
    assert!(err.contains("takes: w, red, value_0"), "the values: {err}");
    assert!(err.contains(" more") && err.contains("fab.tree") && err.contains("fab.params"), "where the rest are: {err}");
}

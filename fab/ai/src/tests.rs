use super::*;
use fab_model::program::ScriptEnv;
use makepad_model_script::lang::csg_sdf::mesh_field::MeshData;

fn args(text: &str) -> Value {
    makepad_model::json::parse(text.as_bytes()).unwrap()
}

fn quoted(t: &str) -> String {
    let mut out = String::from("\"");
    for c in t.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn call(s: &mut Session, tool: &str, fields: &[(&str, &str)]) -> Reply {
    let body: Vec<String> = fields.iter().map(|(k, v)| format!("{}:{}", quoted(k), quoted(v))).collect();
    s.call(tool, &args(&format!("{{{}}}", body.join(","))), None)
}

fn head(s: &Session) -> String {
    s.model.head().text()
}

#[test]
fn code_tabs_list_the_plate_then_placed_scripts_then_their_paint_files() {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let scripts: BTreeMap<String, String> = [
        ("bracket.cad.splash", "let m = mod.model\nm.paint_file(\"bracket.paint.splash\")\n"),
        ("lid.cad.splash", "let m = mod.model\nm.paint_file(\"lid.paint.splash\")\n"),
        ("bracket.paint.splash", "// Bracket paint\n"),
        ("lid.paint.splash", "// Lid paint\n"),
        ("unused.cad.splash", "// Not placed\n"),
        ("unused.paint.splash", "// Not referenced\n"),
    ].into_iter().map(|(f, t)| (f.into(), t.into())).collect();
    let project = "use mod.fab.*\nscript(\"lid.cad.splash\", name: \"Lid\")\nscript(\"bracket.cad.splash\", name: \"Bracket\")\nscript(\"lid.cad.splash\", name: \"Spare\")\n";
    session.model = FabModel::load("Tool plate", project, scripts.clone()).unwrap();
    let files = session.source_files();
    let tabs: Vec<_> = files.tabs.iter().map(|(f, n)| (f.as_str(), n.as_str())).collect();
    assert_eq!(tabs, [
        ("Tool plate.fab.splash", "Tool plate"),
        ("lid.cad.splash", "Lid"),
        ("bracket.cad.splash", "Bracket"),
        ("lid.paint.splash", "Lid paint"),
        ("bracket.paint.splash", "Bracket paint"),
    ]);
    assert_eq!(files.texts["Tool plate.fab.splash"], session.model.project_text());
    assert_eq!(files.texts["lid.paint.splash"], scripts["lid.paint.splash"]);
    // File contents can remain in the document; only placed instances supply tabs.
    session.model = FabModel::load("Tool plate", "use mod.fab.*\nscript(\"bracket.cad.splash\")\n", scripts.clone()).unwrap();
    assert_eq!(session.source_files().tabs.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), ["Tool plate.fab.splash", "bracket.cad.splash", "bracket.paint.splash"]);
    session.model = FabModel::load("Tool plate", "use mod.fab.*\n", scripts).unwrap();
    assert_eq!(session.source_files().tabs, [("Tool plate.fab.splash".into(), "Tool plate".into())]);
}

#[test]
fn project_tab_previews_commits_replaces_and_reports_parse_lines() {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let script = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet cube = m.box({size: [10mm 10mm 10mm]})\nobject(cube)\n";
    session.model = FabModel::load("Plate", "use mod.fab.*\nscript(\"cube.cad.splash\")\n", [("cube.cad.splash".into(), script.into())].into()).unwrap();
    session.rebuild().unwrap();
    let file = session.project_file();
    let before = session.model.head();
    let text = "use mod.fab.*\nscript(\"cube.cad.splash\")\nplace(\"cube\", object: \"cube\", at: [70mm 80mm], scale: 2)\nsettings({layer_height: 0.16})\n";
    let preview = session.preview_source(&file, text, None).unwrap().unwrap();
    assert_eq!(preview.0.instances[0].placed[0].scale, [2.0; 3]);
    assert_eq!(session.model.head(), before);
    assert!(session.model.project().places.is_empty());
    let (reply, problem) = session.set_source(&file, text);
    assert!(reply.ok && problem.is_none(), "{}", reply.text);
    assert_eq!(session.model.project().places[0].scale, [2.0; 3]);
    assert_eq!(session.source("cube.cad.splash").as_deref(), Some(script));
    let saved = session.model.head();
    let invalid = "use mod.fab.*\nscript(\"cube.cad.splash\")\nplace(\"cube\", scale: )\n";
    let problem = session.preview_source(&file, invalid, None).unwrap_err();
    assert_eq!(problem.file, file);
    assert_eq!(problem.line.map(|(line, _)| line), Some(3));
    let (reply, problem) = session.set_source(&file, invalid);
    assert!(!reply.ok && problem.is_some());
    assert_eq!(session.model.head(), saved);
    // Removing statements removes their placement/settings, without duplicating instances.
    let (reply, problem) = session.set_source(&file, "use mod.fab.*\nscript(\"cube.cad.splash\")\n");
    assert!(reply.ok && problem.is_none(), "{}", reply.text);
    assert_eq!(session.model.project().scripts.len(), 1);
    assert!(session.model.project().places.is_empty());
    assert!(session.model.project().settings.is_empty());
    assert!(session.undo_redo(fab_model::HistoryAction::Undo).ok);
    assert_eq!(session.model.project().places[0].scale, [2.0; 3]);
}

#[test]
fn paint_tab_edits_use_the_paint_files_text_and_error_lines() {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let script = format!("{}let cube = m.box({{size: [10mm 10mm 10mm]}})\nobject(cube)\nm.paint_file(\"cube.paint.splash\")\n", fab_model::program::EMPTY);
    let paint = format!("{}m.fill(m.sphere({{radius: 20mm}}), Material{{albedo: [0.8 0.05 0.05]}})\n", fab_model::program::EMPTY);
    session.model = FabModel::load("Plate", "use mod.fab.*\nscript(\"cube.cad.splash\")\n", [
        ("cube.cad.splash".into(), script.clone()),
        ("cube.paint.splash".into(), paint.clone()),
    ].into()).unwrap();
    session.rebuild().unwrap();
    let before = session.model.head();
    let edited = paint.replace("0.8 0.05 0.05", "0.05 0.05 0.8");
    assert!(session.preview_source("cube.paint.splash", &edited, None).unwrap().is_some());
    assert_eq!(session.model.head(), before);
    let (reply, problem) = session.set_source("cube.paint.splash", &edited);
    assert!(reply.ok && problem.is_none(), "{}", reply.text);
    assert_eq!(session.source("cube.paint.splash").as_deref(), Some(edited.as_str()));
    assert_eq!(session.source("cube.cad.splash").as_deref(), Some(script.as_str()));
    let invalid = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nm.fill(\n";
    let problem = session.preview_source("cube.paint.splash", invalid, None).unwrap_err();
    assert_eq!(problem.file, "cube.paint.splash");
    assert!(problem.line.is_some_and(|(line, _)| line >= 4), "{problem}");
    let (reply, problem) = session.set_source("cube.paint.splash", invalid);
    assert!(!reply.ok && problem.is_some());
    assert_eq!(session.source("cube.paint.splash").as_deref(), Some(edited.as_str()));
}

#[test]
fn place_parts_moves_a_part_as_one_undo_step() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let r = call(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "cube.cad.splash"), ("source", "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet cube = m.box({size: [20mm 20mm 20mm]})\nobject(cube)\n")]);
    assert!(r.ok, "{}", r.text);
    let before = s.last.clone().unwrap().plate.parts[0].mesh.bounds();
    // 40 mm along X and lifted 15 mm (it drops back), turned 90 degrees about Z
    let c = before.center();
    let delta = [[0.0, -1.0, 0.0, c[0] + c[1] + 40.0], [1.0, 0.0, 0.0, c[1] - c[0]], [0.0, 0.0, 1.0, 15.0]];
    let r = s.place_parts(&[PartMove { object: "cube/cube".into(), delta }]);
    assert!(r.ok, "{}", r.text);
    let after = s.last.clone().unwrap().plate.parts[0].mesh.bounds();
    assert!((after.center()[0] - before.center()[0] - 40.0).abs() < 1e-3, "{:?} -> {:?}", before, after);
    assert!(after.min[2].abs() < 1e-9, "dropped onto the plate");
    // The move is the project's: the script is untouched.
    let project = s.model.project_text();
    assert!(project.contains("place(\"cube\", object: \"cube\", at: [") && project.contains("rotate: 90°"), "{project}");
    assert!(!s.source("cube.cad.splash").unwrap().contains("at:"));
    // a second move edits that placement instead of adding one
    let r = s.place_parts(&[PartMove { object: "cube/cube".into(), delta: [[1.0, 0.0, 0.0, -10.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]] }]);
    assert!(r.ok, "{}", r.text);
    assert_eq!(s.model.project_text().matches("place(").count(), 1);
    // one undo step per move
    let h = head(&s);
    assert!(call(&mut s, "history", &[("expected", &h), ("action", "undo")]).ok);
    let back = s.last.clone().unwrap().plate.parts[0].mesh.bounds();
    assert!((back.center()[0] - after.center()[0]).abs() < 1e-3);
}

#[test]
fn manifest_is_valid() {
    let m = manifest();
    m.validate().unwrap();
    assert!(m.tool("apply").is_some() && m.tool("script").is_some() && m.tool("place").is_some() && m.tool("check").is_some());
    assert!(m.tool("colors").is_some());
    assert!(m.tool("make_printable").is_some());
}

#[test]
fn a_misspelt_argument_is_refused_with_its_name() {
    let parse = |text: &str| makepad_model::json::parse(text.as_bytes()).unwrap();
    let refused = check_arguments("render", &parse(r#"{"views":["front"],"fvo":10}"#)).unwrap_err();
    assert!(refused.contains("'fvo'") && refused.contains("fov") && !refused.contains('\n'), "{refused}");
    assert!(check_arguments("render", &parse(r#"{"views":["front"],"fov":10}"#)).is_ok());
    // Every tool refuses an argument it does not take, and takes each it lists.
    for tool in &manifest().tools {
        let refused = check_arguments(&tool.name, &parse(r#"{"no_such_argument":1}"#)).unwrap_err();
        assert!(refused.contains("'no_such_argument'"), "fab.{}: {refused}", tool.name);
        let schema = parse(&tool.parameters);
        if let Some(Value::Obj(props)) = schema.get("properties") {
            let all = Value::Obj(props.iter().map(|(k, _)| (k.clone(), Value::Null)).collect());
            assert!(check_arguments(&tool.name, &all).is_ok(), "fab.{} refuses its own arguments", tool.name);
        }
    }
}

#[test]
fn colors_lists_the_two_colour_program() {
    // Keep this program identical to TWO_COLOUR in fab/model/src/tests.rs.
    const TWO_COLOUR: &str = "let red = Material{albedo: [0.8 0.05 0.05]}\nlet body = m.blend([m.box({size: [30mm 30mm 20mm]}) m.sphere({radius: 13mm})], 1mm)\nobject(body)\nm.fill(m.sphere({radius: 13mm at: [0 20mm 0]}), red)\n";
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    session.model = FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
    let (reply, _) = session.set_source("t.cad.splash", &format!("{}{TWO_COLOUR}", fab_model::program::EMPTY));
    assert!(reply.ok, "{}", reply.text);
    let before = session.model.head();
    let reply = session.call("colors", &Value::Obj(Vec::new()), None);
    assert!(reply.ok, "{}", reply.text);
    assert!(reply.text.starts_with("\"t/body\": 2 colours\n  0 #"), "{}", reply.text);
    assert!(reply.text.contains("\n  1 #e73f3f \"#e73f3f\" tool 2"), "{}", reply.text);
    assert!(reply.text.contains("\n  paint: a region in colour 1"), "{}", reply.text);
    assert_eq!(reply.text.lines().count(), 4);
    assert!(reply.update.is_none());
    assert_eq!(session.model.head(), before);
}

/// A painted colour the printer holds in another filament: fab.colors shows the tray's colour,
/// as the views do, and says what the design asks.
#[test]
fn colors_says_what_the_design_asks_where_its_tray_differs() {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    session.model = FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
    let src = "let red = Material{albedo: [0.8 0.05 0.05] name: \"Red\"}\nlet body = m.box({size: [30mm 30mm 20mm]})\nobject(body)\nm.fill(m.sphere({radius: 8mm at: [0 10mm 0]}), red)\n";
    let (reply, _) = session.set_source("t.cad.splash", &format!("{}{src}", fab_model::program::EMPTY));
    assert!(reply.ok, "{}", reply.text);
    let grey = [0.53, 0.53, 0.53];
    let mapped = fab_model::eval::Mapped { inks: [("Red".to_string(), grey)].into(), tools: [("Red".to_string(), (2, 0))].into() };
    session.map_colours(std::sync::Arc::new(mapped)).expect("mapped anew");
    let text = session.call("colors", &Value::Obj(Vec::new()), None).text;
    assert!(text.contains(&format!("\n  1 {} \"Red\" tool 2 (its tray's filament; the design asks #e73f3f)\n", color_hex(grey))), "{text}");
}

#[test]
fn check_answers_are_splash_with_a_cap_and_slice_routing() {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let reply = session.call("check", &args("{}"), None);
    assert!(reply.ok && reply.render.is_none(), "{}", reply.text);
    let v = makepad_script::syntax::parse_value(&reply.text).expect("the answer is a Splash value");
    assert_eq!(v.field("errors").and_then(|f| f.value.number()), Some(0.0));
    let reply = session.call("check", &args(r#"{"slice":true}"#), None);
    assert_eq!(reply.render, Some(RenderRequest::CheckSlice));
    let mut report = fab_printability::Report::default();
    for i in 0..47 {
        report.issues.push(fab_printability::Issue {
            kind: "bridge",
            severity: if i == 46 { fab_printability::Severity::Error } else { fab_printability::Severity::Warning },
            object: Some("beam".into()),
            at_mm: [110.0, 120.0, 5.0],
            footprint: fab_printability::Footprint { min: [104.0, 118.75], max: [116.0, 121.25] },
            layer: Some(25),
            layer_to: None,
            measure: 12.0,
            message: "12 mm bridge".into(),
            hint: "Add support beneath the span.".into(),
            fix: None,
            area_mm2: 30.0,
            lowest_z: 5.0,
            highest_z: 5.0,
            flags: 0,
        });
    }
    let text = out::text(&check_value(&report));
    let v = makepad_script::syntax::parse_value(&text).unwrap();
    assert_eq!(v.field("errors").and_then(|f| f.value.number()), Some(1.0));
    assert_eq!(v.field("warnings").and_then(|f| f.value.number()), Some(46.0));
    assert_eq!(v.field("more").and_then(|f| f.value.number()), Some(7.0));
    let issues = v.field("issues").and_then(|f| f.value.items()).unwrap();
    assert_eq!(issues.len(), 40);
    assert_eq!(issues[0].field("severity").and_then(|f| f.value.name()), Some("error"));
    assert!(text.contains("at: [110mm, 120mm, 5mm]") || text.contains("at: [110mm 120mm 5mm]"), "{text}");
}

/// fab.apply adds a support override, or any statement, above an object known by its line
/// (`box@6`: no `let` names its solid): placing it wrote that name into its code, so it keeps
/// its name and its placement.
#[test]
fn an_apply_keeps_an_unnamed_objects_placement() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let program = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet lid = m.box({size: [12mm 2mm 12mm]})\nobject(lid)\nobject(m.box({size: [10mm 10mm 10mm]}))\n";
    let r = call(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("source", program)]);
    assert!(r.ok, "{}", r.text);
    let instance = s.model.project().scripts[0].name.clone();
    let h = head(&s);
    let r = call(&mut s, "place", &[("request_id", "b"), ("expected", &h), ("edits", &format!("place(\"{instance}\", object: \"box@6\", at: [60mm 70mm])"))]);
    assert!(r.ok, "{}", r.text);
    let placed = |s: &Session| (s.model.project_text(), s.last.as_ref().unwrap().plate.parts.iter().map(|p| (p.object.clone(), p.mesh.bounds().center())).collect::<Vec<_>>());
    let before = placed(&s);
    assert!(before.0.contains("box@6") && before.1.len() == 2, "{before:?}");
    let h = head(&s);
    let r = call(&mut s, "apply", &[("request_id", "c"), ("expected", &h), ("edits", "m.support_block(m.box({size: [5mm 5mm 5mm] at: [0 5mm 0]}))\nm.settings(m.box({size: [4mm 4mm 4mm] at: [0 5mm 0]}), {sparse_infill_density: \"100%\" wall_loops: 4})\nlet peg = m.cylinder({radius: 1mm height: 4mm})")]);
    assert!(r.ok, "{}", r.text);
    let file = s.model.state().scripts.keys().next().unwrap().clone();
    let src = s.source(&file).unwrap().to_string();
    assert!(src.contains("m.support_block(m.box") && src.contains("m.settings(m.box") && src.contains("let peg"), "{src}");
    let zones: Vec<_> = s.last.as_ref().unwrap().plate.parts.iter().map(|p| p.paint.as_ref().map_or(0, |sp| sp.settings.len())).collect();
    assert!(zones.contains(&1), "{zones:?}");
    assert_eq!(placed(&s), before, "{src}");
}

/// The code panel commits its whole text (`set_source`): a line typed above an object known
/// by its line (`box@6`) leaves it where it was placed, whose name placing it wrote.
#[test]
fn a_line_typed_above_an_unnamed_object_leaves_it_in_place() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let program = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet lid = m.box({size: [12mm 2mm 12mm]})\nobject(lid)\nobject(m.box({size: [10mm 10mm 10mm]}))\n";
    let r = call(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("source", program)]);
    assert!(r.ok, "{}", r.text);
    let instance = s.model.project().scripts[0].name.clone();
    let h = head(&s);
    let r = call(&mut s, "place", &[("request_id", "b"), ("expected", &h), ("edits", &format!("place(\"{instance}\", object: \"box@6\", at: [60mm 70mm])"))]);
    assert!(r.ok, "{}", r.text);
    let placed = |s: &Session| (s.model.project_text(), s.last.as_ref().unwrap().plate.parts.iter().map(|p| (p.object.clone(), p.mesh.bounds().center())).collect::<Vec<_>>());
    let before = placed(&s);
    let file = s.model.state().scripts.keys().next().unwrap().clone();
    // What the panel shows once the placement is built, a line typed above the object.
    let typed = s.source(&file).unwrap().replace("object(lid)\n", "object(lid)\nlet peg = m.cylinder({radius: 1mm height: 4mm})\n");
    let (reply, problem) = s.set_source(&file, &typed);
    assert!(reply.ok && problem.is_none(), "{}", reply.text);
    assert_eq!(s.source(&file).as_deref(), Some(typed.as_str()));
    assert_eq!(placed(&s), before, "{typed}");
    assert!(typed.contains("name: \"box@6\""), "{typed}");
}

/// A project file that places a script without naming its object resolves the placement by
/// compiling the script, under the session's env: a case script that reads its board opens.
#[test]
fn a_project_file_opens_under_the_session_env() {
    let dir = std::env::temp_dir().join(format!("fab-ai-env-open-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("case.cad.splash"), "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet w = board(\"b\").width\nobject(m.box({size: [w 2mm 2mm]}), name: \"base\")\n").unwrap();
    let project = dir.join("case.fab.splash");
    std::fs::write(&project, "use mod.fab.*\nscript(\"case.cad.splash\")\nplace(\"case\")\n").unwrap();
    let board = fab_model::lang::Input { source: "{width: 30mm}".into(), hash: [1; 16] };
    let inputs = fab_model::lang::Inputs { kinds: vec!["board".into()], values: [(("board".to_string(), "b".to_string()), board)].into_iter().collect(), lazy: None };
    let mut session = Session::new(Machine::dual_nozzle_350(), dir.clone());
    session.model.set_env(fab_model::program::ScriptEnv::new(Vec::new(), inputs));
    let opened = session.open(&project);
    std::fs::remove_dir_all(&dir).ok();
    let ev = opened.unwrap_or_else(|e| panic!("the placement without object resolves under the session's env: {e}"));
    assert_eq!(session.model.project().places[0].object, "base", "the placement takes the script's one object");
    assert_eq!(ev.plate.parts.len(), 1, "the opened plate builds its part");
}

#[test]
fn a_session_builds_edits_exports_and_undoes() {
    let dir = std::env::temp_dir().join(format!("fab-ai-test-{}", std::process::id()));
    let mut s = Session::new(Machine::dual_nozzle_350(), dir.clone());
    s.rebuild().unwrap();
    let r = s.call("inspect", &args("{}"), None);
    assert!(r.ok && r.text.contains("project:") && r.text.contains("scripts:"), "{}", r.text);
    let h = head(&s);
    let program = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet r = param(8mm, min: 3mm, max: 15mm)\nlet base = m.box({size: [40mm 5mm 40mm]})\nlet knob = m.sdf({bounds: [[-9mm, 0, -9mm], [9mm, 17mm, 9mm]] field: fn(p) { return sdf_sphere(p - vec3(0, 8mm, 0), r) }})\nlet handle = m.union(base, knob)\nobject(handle)\n";
    let r = call(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("source", program)]);
    assert!(r.ok, "{}", r.text);
    assert!(r.update.is_some());
    assert!(r.text.contains("closed: true"), "{}", r.text);
    // Edits are Splash statements: a value site, a field, a new solid it emits; where it
    // prints is the project's.
    let file = s.model.state().scripts.keys().next().unwrap().clone();
    let instance = s.model.project().scripts[0].name.clone();
    let h = head(&s);
    let r = call(&mut s, "apply", &[("request_id", "b"), ("expected", &h), ("edits", "set(r, 6mm)\nfield(base, radius, 1mm)\nlet tag = m.box({size: [8mm 2mm 8mm] at: [30mm, 1mm, 0]})\nobject(tag)")]);
    assert!(r.ok, "{}", r.text);
    let h = head(&s);
    let r = call(&mut s, "place", &[("request_id", "b2"), ("expected", &h), ("edits", &format!("color(\"Tag\")\nplace(\"{instance}\", object: \"tag\", color: \"Tag\")"))]);
    assert!(r.ok, "{}", r.text);
    let src = s.source(&file).unwrap();
    assert!(src.contains("param(6mm") && src.contains("radius: 1mm") && src.contains("let tag = m.box") && src.contains("object(tag)"), "{src}");
    assert!(s.model.project_text().contains("object: \"tag\", color: \"Tag\""), "{}", s.model.project_text());
    assert_eq!(s.last.as_ref().unwrap().plate.parts.len(), 2);
    // Placement is refused in a script edit.
    let h = head(&s);
    let r = call(&mut s, "apply", &[("request_id", "b3"), ("expected", &h), ("edits", "place(tag, at: [10mm 10mm])")]);
    assert!(!r.ok && r.text.contains("fab.place"), "{}", r.text);
    // A stale head is refused with the current one.
    let r = call(&mut s, "apply", &[("request_id", "c"), ("expected", "0:00"), ("edits", "set(r, 7mm)")]);
    assert!(!r.ok && r.text.contains("stale head"), "{}", r.text);
    // A program error names its line and column.
    let h = head(&s);
    let r = call(&mut s, "apply", &[("request_id", "d"), ("expected", &h), ("edits", "field(base, size, [40mm 5mm])")]);
    assert!(!r.ok && r.text.contains("at: \"5:"), "{}", r.text);
    assert_eq!(s.source(&file).unwrap(), src);

    let r = s.call("export", &args(r#"{"format":"3mf"}"#), None);
    assert!(r.ok, "{}", r.text);
    let r = s.call("save", &args("{}"), None);
    assert!(r.ok, "{}", r.text);
    let h = head(&s);
    let r = call(&mut s, "history", &[("expected", &h), ("action", "undo")]);
    assert!(r.ok, "{}", r.text);
    assert_eq!(r.update.unwrap().1.plate.parts.len(), 2, "undoing the filament keeps both parts");

    // The saved project opens again and builds the same plate.
    let saved = s.path.clone().unwrap();
    let mut t = Session::new(Machine::dual_nozzle_350(), dir.clone());
    let ev = t.open(&saved).unwrap();
    assert_eq!(ev.plate.parts.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A part made from a distance field reports its solid's volume, read from its field (no mesh is
/// built for it): a ball's and a box's, within a thousandth of their exact volumes. The script
/// call answers before they are read (pending), the plate says them once read, and the same
/// shapes built again know them at once.
#[test]
fn a_field_part_reports_its_solids_volume() {
    let dir = std::env::temp_dir().join(format!("fab-ai-field-volume-{}", std::process::id()));
    let mut s = Session::new(Machine::dual_nozzle_350(), dir.clone());
    s.rebuild().unwrap();
    let h = head(&s);
    let program = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet ball = m.sdf({bounds: [[-16mm, -1mm, -16mm], [16mm, 31mm, 16mm]] cell: 0.5mm field: fn(p) { return sdf_sphere(p - vec3(0, 15mm, 0), 15mm) }})\nlet brick = m.sdf({bounds: [[-11mm, -1mm, -16mm], [11mm, 11mm, 16mm]] cell: 0.5mm field: fn(p) { return sdf_box(p - vec3(0, 5mm, 0), vec3(20mm, 10mm, 30mm)) }})\nobject(ball)\nobject(brick)\n";
    let r = call(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("source", program)]);
    assert!(r.ok, "{}", r.text);
    let ev = s.last.clone().unwrap();
    // the call answers before the volumes are read: it says they are pending
    let pending = format!("volume_mm3: {}", out::text(&out::s("pending")));
    assert!(r.text.matches(&pending).count() >= 2, "the script call says what it added, both volumes pending: {}", r.text);
    for (name, exact) in [("ball", 4.0 / 3.0 * std::f64::consts::PI * 15f64.powi(3)), ("brick", 6000.0)] {
        let i = ev.plate.parts.iter().position(|p| p.object.ends_with(&format!("/{name}"))).unwrap_or_else(|| panic!("no {name} on the plate"));
        let volume = ev.fields[i].as_ref().and_then(|f| f.volume.clone()).unwrap_or_else(|| panic!("{name} is a field part with a volume"));
        assert_eq!(volume.known(), None, "{name}'s volume is read after the build");
        let v = volume.read();
        println!("{name}: {v:.3} mm3 against {exact:.3}, {:+.4}%", (v - exact) / exact * 100.0);
        assert!((v - exact).abs() <= 0.001 * exact, "{name}: {v} mm3 against {exact}");
        let plate = out::text(&plate_value(&ev));
        let shown = format!("volume_mm3: {}", out::text(&out::n(v)));
        assert!(plate.contains(&shown), "once read, the plate says it: {plate}");
    }
    // the same shapes built again know their volumes at once
    let h = head(&s);
    let r = call(&mut s, "script", &[("request_id", "b"), ("expected", &h), ("source", &format!("{program}let spare = 1\n"))]);
    assert!(r.ok, "{}", r.text);
    let again = s.last.clone().unwrap();
    assert!(again.fields.iter().flatten().all(|f| f.volume.as_ref().is_some_and(|v| v.known().is_some())), "a shape read before knows its volume");
    let _ = std::fs::remove_dir_all(&dir);
}

/// fab.render's camera: a focus, distance or field of view that is malformed or out of range is
/// refused with what it takes, never clamped; good ones go to the render as given.
#[test]
fn render_camera_arguments_are_checked_not_clamped() {
    let dir = std::env::temp_dir().join(format!("fab-ai-render-camera-{}", std::process::id()));
    let mut s = Session::new(Machine::dual_nozzle_350(), dir.clone());
    s.rebuild().unwrap();
    let h = head(&s);
    let r = call(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("source", "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.box({size: [20mm 20mm 20mm]}))\n")]);
    assert!(r.ok, "{}", r.text);
    for (bad, says) in [
        (r#"{"fov":500}"#, "'fov' 500 is out of range: degrees across the view, 5 to 120"),
        (r#"{"fov":2}"#, "'fov' 2 is out of range"),
        (r#"{"distance":-5}"#, "'distance' -5 is out of range: mm from the camera to the middle of what it frames, 1 to 5000"),
        (r#"{"distance":"far"}"#, "'distance' is a number"),
        (r#"{"focus":[[0,0,0]]}"#, "'focus' is a box, [[x y z] [x y z]]"),
        (r#"{"focus":[[1,2,3],[1,2,3]]}"#, "'focus' box has no size"),
        (r#"{"focus":{"at":[1,2,3],"radius":0}}"#, "'focus' radius must be more than 0 mm"),
        (r#"{"ortho":1}"#, "'ortho' is true or false"),
        (r#"{"ortho":true,"fov":20}"#, "'fov' does not apply to an orthographic view"),
        (r#"{"ortho":true,"distance":200}"#, "'distance' does not apply to an orthographic view"),
        (r#"{"colours":"trays"}"#, "(the model's own colours, the default) or"),
    ] {
        let r = s.call("render", &args(bad), None);
        assert!(!r.ok && r.text.contains(says) && r.render.is_none(), "{bad}: {}", r.text);
    }
    let r = s.call("render", &args(r#"{"views":["35,-15"],"focus":{"at":[150,160,10],"radius":12},"distance":80,"fov":30}"#), None);
    assert!(r.ok, "{}", r.text);
    let Some(RenderRequest::Sheets { camera, .. }) = &r.render else { panic!("no render: {:?}", r.render) };
    assert_eq!(*camera, Camera { focus: Some(Focus::Ball([150.0, 160.0, 10.0], 12.0)), distance: Some(80.0), fov: Some(30.0), ortho: false, colours: Colours::Design });
    let r = s.call("render", &args(r#"{"focus":[[10,20,30],[0,0,0]]}"#), None);
    let Some(RenderRequest::Sheets { camera, .. }) = &r.render else { panic!("no render: {}", r.text) };
    assert_eq!(camera.focus, Some(Focus::Box([0.0, 0.0, 0.0], [10.0, 20.0, 30.0])), "a box's corners in any order");
    let r = s.call("render", &args(r#"{"views":["front"],"ortho":true}"#), None);
    let Some(RenderRequest::Sheets { camera, .. }) = &r.render else { panic!("no render: {}", r.text) };
    assert!(camera.ortho);
    let r = s.call("render", &args(r#"{"colours":"spools"}"#), None);
    let Some(RenderRequest::Sheets { camera, .. }) = &r.render else { panic!("no render: {}", r.text) };
    assert_eq!(camera.colours, Colours::Spools);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A focus naming a solid frames its box on the plate and follows it when its object moves; a
/// focus with an object is in that solid's model frame.
#[test]
fn render_focus_follows_a_named_solid() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let torso = m.ellipsoid({{radii: [10mm 20mm 10mm] at: [0 20mm 0]}})\nlet head = m.sphere({{radius: 6mm at: [0 46mm 0]}})\nlet doll = m.blend([torso head], 1mm)\nobject(doll)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "doll.cad.splash"), ("source", &source)]);
    let focus = |s: &mut Session, f: &str| -> Focus {
        let r = s.call("render", &args(&format!(r#"{{"views":["front"],"focus":{f}}}"#)), None);
        let Some(RenderRequest::Sheets { camera, .. }) = &r.render else { panic!("{f}: {}", r.text) };
        camera.focus.unwrap()
    };
    let place = |s: &mut Session, x: f64| {
        let h = head(s);
        tool_ok(s, "place", &[("request_id", &format!("p{x}")), ("expected", &h), ("edits", &format!("place(\"doll\", at: [{x}mm 120mm])"))]);
    };
    let mid = |lo: [f64; 3], hi: [f64; 3]| -> [f64; 3] { std::array::from_fn(|k| (lo[k] + hi[k]) * 0.5) };
    place(&mut s, 100.0);
    let Focus::Box(lo, hi) = focus(&mut s, r#""head""#) else { panic!("a box") };
    assert!((0..3).all(|k| (hi[k] - lo[k] - 12.0).abs() < 0.01), "the head's 12 mm ball: {lo:?} {hi:?}");
    let m1 = mid(lo, hi);
    assert!((m1[0] - 100.0).abs() < 1e-6 && (m1[1] - 120.0).abs() < 1e-6 && (m1[2] - 46.0).abs() < 0.5, "over the object's origin, 46 mm up: {m1:?}");
    place(&mut s, 150.0);
    let Focus::Box(lo, hi) = focus(&mut s, r#""doll/head""#) else { panic!("a box") };
    let m2 = mid(lo, hi);
    assert!((m2[0] - m1[0] - 50.0).abs() < 1e-6 && (m2[1] - m1[1]).abs() < 1e-6 && (m2[2] - m1[2]).abs() < 1e-6, "moved with its object: {m1:?} {m2:?}");
    let Focus::Ball(at, r) = focus(&mut s, r#"{"object":"head","at":[0,46,0],"radius":6}"#) else { panic!("a ball") };
    assert!((0..3).all(|k| (at[k] - m2[k]).abs() < 1e-6) && (r - 6.0).abs() < 1e-9, "the model frame's point is the head's middle: {at:?} {r}");
    for (bad, says) in [(r#""tail""#, "no solid 'tail'"), (r#"{"object":"head","radius":3}"#, "'focus' is a box")] {
        let r = s.call("render", &args(&format!(r#"{{"focus":{bad}}}"#)), None);
        assert!(!r.ok && r.text.contains(says), "{bad}: {}", r.text);
    }
}

/// fab.surface: the nearest point of a field solid's own surface (the head alone, or the whole
/// blend), its normal, signed distance and node; a mesh solid's from its faces.
#[test]
fn surface_finds_the_nearest_point_and_normal() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let torso = m.ellipsoid({{radii: [10mm 20mm 10mm] at: [0 20mm 0]}})\nlet head = m.sphere({{radius: 6mm at: [0 46mm 0]}})\nlet doll = m.blend([torso head], 1mm)\nobject(doll)\nlet block = m.box({{size: [20mm 10mm 20mm] at: [60mm 5mm 0]}})\nobject(block)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "doll.cad.splash"), ("source", &source)]);
    let found = |s: &mut Session, solid: &str, near: &str| -> Vec<(String, [f64; 3], [f64; 3], f64, Option<String>)> {
        let r = s.call("surface", &args(&format!(r#"{{"solid":"{solid}","near":{near}}}"#)), None);
        assert!(r.ok, "{solid} {near}: {}", r.text);
        let v = makepad_script::syntax::parse_value(&r.text).unwrap();
        let p3 = |n: &makepad_script::syntax::Node, k: &str, scale: f64| -> [f64; 3] {
            let items = n.field(k).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("{k}: {}", r.text));
            std::array::from_fn(|i| items[i].number().unwrap() * scale)
        };
        let engine = v.field("surface").and_then(|f| f.value.string()).unwrap().to_string();
        v.field("points").and_then(|f| f.value.items()).unwrap().iter().map(|p| {
            let node = p.field("node").and_then(|f| f.value.string()).map(str::to_string);
            (engine.clone(), p3(p, "point", 1000.0), p3(p, "normal", 1.0), p.field("distance").and_then(|f| f.value.number()).unwrap() * 1000.0, node)
        }).collect()
    };
    let near = |a: [f64; 3], b: [f64; 3], within: f64| (0..3).all(|k| (a[k] - b[k]).abs() < within);
    let degrees = |a: [f64; 3], b: [f64; 3]| {
        let (la, lb) = ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt(), (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt());
        ((a[0] * b[0] + a[1] * b[1] + a[2] * b[2]) / (la * lb)).clamp(-1.0, 1.0).acos().to_degrees()
    };
    // The head alone: its sphere, wherever the blend would round it.
    let r = 6.0 / 2f64.sqrt();
    let got = found(&mut s, "head", "[[0,46,20],[10,56,0],[0,46,3]]");
    for ((engine, point, normal, d, node), (want, wn, wd)) in got.into_iter().zip([([0.0, 46.0, 6.0], [0.0, 0.0, 1.0], 14.0), ([r, 46.0 + r, 0.0], [1.0, 1.0, 0.0], 200f64.sqrt() - 6.0), ([0.0, 46.0, 6.0], [0.0, 0.0, 1.0], -3.0)]) {
        assert_eq!(engine, "field");
        assert!(near(point, want, 0.1) && degrees(normal, wn) < 1.0 && (d - wd).abs() < 0.1, "the head's sphere: {point:?} {normal:?} {d} (want {want:?} {wn:?} {wd})");
        assert_eq!(node.as_deref(), Some("doll/head"));
    }
    // The whole doll: the torso's ellipsoid, off its axes too (a bound's gradient misses the
    // nearest point there), the node that makes it named.
    for at in [[0.0, 20.0, 30.0], [0.0, 35.0, 20.0], [12.0, 8.0, 5.0]] {
        let (_, point, normal, d, node) = found(&mut s, "doll/doll", &format!("{at:?}")).remove(0);
        let rel = [point[0] / 10.0, (point[1] - 20.0) / 20.0, point[2] / 10.0];
        let on = (rel[0] * rel[0] + rel[1] * rel[1] + rel[2] * rel[2]).sqrt();
        let true_normal = [point[0] / 100.0, (point[1] - 20.0) / 400.0, point[2] / 100.0];
        let way = [at[0] - point[0], at[1] - point[1], at[2] - point[2]];
        assert!((on - 1.0).abs() < 0.01, "{at:?}: {point:?} is on the ellipsoid");
        assert!(degrees(normal, true_normal) < 1.0 && degrees(way, true_normal) < 1.0, "{at:?}: the normal {normal:?} and the way out {way:?} along the true normal {true_normal:?}");
        assert!((d - (way[0] * way[0] + way[1] * way[1] + way[2] * way[2]).sqrt()).abs() < 0.1, "{at:?}: {d}");
        assert_eq!(node.as_deref(), Some("doll/torso"));
    }
    // A mesh solid: its faces, inside negative.
    let got = found(&mut s, "block", "[[63,25,2],[63,7,2]]");
    for ((engine, point, normal, d, node), wd) in got.into_iter().zip([15.0, -3.0]) {
        assert_eq!(engine, "mesh");
        assert!(near(point, [63.0, 10.0, 2.0], 0.1) && degrees(normal, [0.0, 1.0, 0.0]) < 1.0 && (d - wd).abs() < 0.1 && node.is_none(), "the block's top: {point:?} {normal:?} {d}");
    }
    for (bad, says) in [(r#"{"solid":"tail","near":[0,0,0]}"#, "no solid 'tail'"), (r#"{"solid":"head"}"#, "missing 'near'"), (r#"{"solid":"head","near":[0,1]}"#, "'near': expected 3")] {
        let r = s.call("surface", &args(bad), None);
        assert!(!r.ok && r.text.contains(says), "{bad}: {}", r.text);
    }
}

/// fab.pick: a render's camera, as its answer tells it, and a pixel name what shows there: the
/// head's sphere where the pixel's ray first meets it (perspective and orthographic), a mesh
/// solid's face, nothing above the head. fab.select on the same doll: picks named by the AI (an
/// edit, a crease, a spot) and by a pixel (the neck's crease), described with what to write; a
/// pick follows its name through an edit and drops when the name goes; a refused set changes
/// nothing.
#[test]
fn pick_names_what_a_pixel_shows() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let torso = m.ellipsoid({{radii: [10mm 20mm 10mm] at: [0 20mm 0]}})\nlet head = m.sphere({{radius: 6mm at: [0 46mm 0]}})\nlet doll = m.blend([torso head], 1mm)\nobject(doll)\nlet block = m.box({{size: [20mm 10mm 20mm] at: [60mm 5mm 0]}})\nobject(block)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "doll.cad.splash"), ("source", &source)]);
    let frame_of = |s: &Session, solid: &str| s.locate(solid).unwrap().to_plate;
    let view = camera::named_view("front").unwrap();
    // a render's camera aimed at a model point of a solid, as its answer gives it (JSON)
    let aim = |s: &Session, solid: &str, at: [f64; 3], radius: f64, ortho: bool| -> (camera::ViewCamera, String) {
        let middle = fab_model::params::xf_point(&frame_of(s, solid), at);
        let shown = camera::gltf_box(middle.map(|c| c - 50.0), middle.map(|c| c + 50.0));
        let frame = camera::Frame::of_render(shown, std::slice::from_ref(&view), &Camera { focus: Some(Focus::Ball(middle, radius)), ortho, ..Default::default() });
        let cam = frame.camera(&view, camera::SIDE);
        let told = makepad_script::syntax::parse_value(&out::text(&cam.value("front", 0, [0, 0], camera::SIDE as u32))).unwrap();
        let field = |k: &str| told.field(k).map(|f| f.value.clone()).unwrap();
        let v3 = |k: &str| { let i = field(k); let i = i.items().unwrap(); format!("[{},{},{}]", i[0].number().unwrap(), i[1].number().unwrap(), i[2].number().unwrap()) };
        let lens = if ortho { format!(r#""across":{}"#, field("across").number().unwrap()) } else { format!(r#""fov":{}"#, field("fov").number().unwrap()) };
        (cam, format!(r#"{{"view":"front","sheet":0,"cell":[0,0],"px":{},"eye":{},"forward":{},"right":{},"up":{},{lens}}}"#, camera::SIDE, v3("eye"), v3("forward"), v3("right"), v3("up")))
    };
    let pick = |s: &mut Session, told: &str, x: f64, y: f64| -> Reply { s.call("pick", &args(&format!(r#"{{"camera":{told},"x":{x},"y":{y}}}"#)), None) };
    let read = |r: &Reply| {
        assert!(r.ok, "{}", r.text);
        let v = makepad_script::syntax::parse_value(&r.text).unwrap();
        let hit = v.field("hit").map(|f| f.value.clone()).unwrap();
        let p3 = |k: &str, scale: f64| -> [f64; 3] { let i = hit.field(k).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("{k}: {}", r.text)).to_vec(); std::array::from_fn(|a| i[a].number().unwrap() * scale) };
        let text = |k: &str| hit.field(k).and_then(|f| f.value.string()).map(str::to_string);
        (text("solid"), text("node"), p3("point", 1000.0), p3("normal", 1.0))
    };
    let degrees = |a: [f64; 3], b: [f64; 3]| {
        let (la, lb) = ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt(), (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt());
        ((a[0] * b[0] + a[1] * b[1] + a[2] * b[2]) / (la * lb)).clamp(-1.0, 1.0).acos().to_degrees()
    };
    let centre = [0.0, 46.0, 0.0];
    for ortho in [false, true] {
        let (cam, told) = aim(&s, "head", centre, 8.0, ortho);
        // a pixel off the head's middle, so the ray meets the sphere aslant
        let (px, py) = cam.pixel_of(fab_model::params::xf_point(&frame_of(&s, "head"), [2.0, 48.0, 0.0])).unwrap();
        let (x, y) = (px as f64 + 0.5, py as f64 + 0.5);
        let (solid, node, point, normal) = read(&pick(&mut s, &told, x, y));
        assert_eq!((solid.as_deref(), node.as_deref()), (Some("doll/doll"), Some("doll/head")), "ortho {ortho}");
        // where the told ray meets the sphere first, in the model frame
        let (from, dir) = cam.ray_mm(x as f32, y as f32);
        let to_model = fab_model::weave::invert(&frame_of(&s, "head"));
        let (o, d) = (fab_model::params::xf_point(&to_model, from), fab_model::params::xf_dir(&to_model, dir));
        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let d = d.map(|c| c / l);
        let oc = [o[0] - centre[0], o[1] - centre[1], o[2] - centre[2]];
        let b = oc[0] * d[0] + oc[1] * d[1] + oc[2] * d[2];
        let t = -b - (b * b - (oc[0] * oc[0] + oc[1] * oc[1] + oc[2] * oc[2] - 36.0)).sqrt();
        let want: [f64; 3] = std::array::from_fn(|k| o[k] + d[k] * t);
        let want_normal: [f64; 3] = std::array::from_fn(|k| (want[k] - centre[k]) / 6.0);
        assert!((0..3).all(|k| (point[k] - want[k]).abs() < 0.1) && degrees(normal, want_normal) < 1.0, "ortho {ortho}: {point:?} {normal:?}, want {want:?} {want_normal:?}");
    }
    // a mesh solid: the face of the block that faces the camera
    let (cam, told) = aim(&s, "block", [60.0, 5.0, 0.0], 16.0, false);
    let (px, py) = cam.pixel_of(fab_model::params::xf_point(&frame_of(&s, "block"), [62.0, 6.0, 1.0])).unwrap();
    let (solid, node, point, normal) = read(&pick(&mut s, &told, px as f64 + 0.5, py as f64 + 0.5));
    assert_eq!((solid.as_deref(), node), (Some("doll/block"), None));
    let axis = (0..3).max_by(|a, b| normal[*a].abs().total_cmp(&normal[*b].abs())).unwrap();
    let face = [60.0, 5.0, 0.0][axis] + normal[axis].signum() * [10.0, 5.0, 10.0][axis];
    assert!(normal[axis].abs() > 0.9999 && (point[axis] - face).abs() < 0.01, "on the block's face: {point:?} {normal:?}");
    // above the head: nothing
    let (_, told) = aim(&s, "head", centre, 8.0, false);
    let r = pick(&mut s, &told, camera::SIDE as f64 / 2.0, 1.0);
    assert!(r.ok && r.text.contains("hit: nil"), "{}", r.text);
    for (bad, says) in [(r#"{"x":1,"y":1}"#.to_string(), "missing 'camera'"), (format!(r#"{{"camera":{},"x":1,"y":1}}"#, told.replace(r#""fov""#, r#""zoom""#)), "'fov'"), (format!(r#"{{"camera":{told},"x":-1,"y":1}}"#), "outside the view")] {
        let r = s.call("pick", &args(&bad), None);
        assert!(!r.ok && r.text.contains(says), "{bad}: {}", r.text);
    }

    // fab.select: the AI names what it means and reads back what the person would see as chips.
    let select = |s: &mut Session, json: &str| -> Vec<(String, String, String, String, String)> {
        let r = s.call("select", &args(json), None);
        assert!(r.ok, "{json}: {}", r.text);
        let v = makepad_script::syntax::parse_value(&r.text).unwrap();
        let picks = v.field("picks").and_then(|f| f.value.items()).unwrap_or_else(|| panic!("{json}: {}", r.text)).to_vec();
        // a string, or a list of strings joined
        let text = |p: &makepad_script::syntax::Node, k: &str| p.field(k).map(|f| f.value.string().map(str::to_string).unwrap_or_else(|| f.value.items().unwrap_or(&[]).iter().filter_map(|i| i.string()).collect::<Vec<_>>().join("; "))).unwrap_or_default();
        picks.iter().map(|p| (text(p, "id"), text(p, "kind"), text(p, "label"), text(p, "write"), text(p, "what"))).collect()
    };
    let got = select(&mut s, r#"{"set":["head",["torso","head"],{"solid":"head","at":[0,60,0]}]}"#);
    let kinds: Vec<(&str, &str)> = got.iter().map(|p| (p.1.as_str(), p.2.as_str())).collect();
    assert_eq!(kinds, [("edit", "head"), ("crease", "torso + head"), ("spot", "on head")]);
    assert!(got[0].3.contains("`head` at doll.cad.splash:") && got[0].3.contains("m.sphere") && got[0].4.contains("sphere 12.0 x 12.0 x 12.0 mm"), "the edit says what to write and what it is: {:?}", got[0]);
    assert!(got[1].3.contains("joined by `doll` at doll.cad.splash:") && got[1].3.contains("m.blend"), "a crease names the statement joining the two: {:?}", got[1]);
    assert!(got[2].4.contains("its top (+Y)"), "a spot snaps onto the surface and says which side: {:?}", got[2]);
    // the view tints the edits a pick names, on a part drawn from its field: the edit's own, a
    // crease's two at half; a spot none
    let tag = |name: &str| makepad_csg_sdf::part_id(name);
    let tints: Vec<Vec<(u32, f32)>> = s.picks.list.iter().map(|p| p.tint.clone()).collect();
    assert_eq!(tints, [vec![(tag("head"), 1.0)], vec![(tag("torso"), 0.5), (tag("head"), 0.5)], vec![]], "the tint of each pick");
    // the view's pixel at the neck, where head and torso meet: the crease, joined by a 1 mm blend
    let neck = [0.0, 40.0, 0.0];
    let (cam, told) = aim(&s, "head", neck, 6.0, false);
    let (px, py) = cam.pixel_of(fab_model::params::xf_point(&frame_of(&s, "head"), neck)).unwrap();
    let got = select(&mut s, &format!(r#"{{"camera":{told},"x":{},"y":{},"kind":"crease"}}"#, px as f64 + 0.5, py as f64 + 0.5));
    assert!(got.len() == 1 && got[0].1 == "crease" && got[0].2.contains("head") && got[0].2.contains("torso") && got[0].4.contains("blended 1.00 mm") && got[0].4.contains("concave"), "{got:?}");
    // its tick runs along the neck's ring: level, at the neck
    let tick = s.picks.list[0].tick.expect("a crease's tick");
    assert!(tick.1[1].abs() < 0.2 && (tick.0[1] - 40.0).abs() < 1.5, "the tick along the crease: {tick:?}");
    // the head's front, as it lands: the edit, in place of what was picked; Shift on it takes it off
    let (cam, told) = aim(&s, "head", centre, 8.0, false);
    let (px, py) = cam.pixel_of(fab_model::params::xf_point(&frame_of(&s, "head"), [0.0, 46.0, 6.0])).unwrap();
    let at_head = format!(r#""camera":{told},"x":{},"y":{}"#, px as f64 + 0.5, py as f64 + 0.5);
    let got = select(&mut s, &format!("{{{at_head}}}"));
    assert!(got.len() == 1 && got[0].1 == "edit" && got[0].2 == "head" && got[0].4.contains("+Z side"), "{got:?}");
    assert!(select(&mut s, &format!("{{{at_head},\"add\":true}}")).is_empty(), "picked again with add: taken off");
    // A pick follows its name through an edit, and drops with a note when the name goes.
    let id = select(&mut s, &format!("{{{at_head}}}"))[0].0.clone();
    let h = head(&s);
    tool_ok(&mut s, "script", &[("request_id", "b"), ("expected", &h), ("script", "doll.cad.splash"), ("source", &source.replace("radius: 6mm", "radius: 7mm"))]);
    let got = select(&mut s, "{}");
    assert!(got.len() == 1 && got[0].0 == id && got[0].4.contains("sphere 14.0 x 14.0 x 14.0 mm"), "described again on the new build: {got:?}");
    let h = head(&s);
    tool_ok(&mut s, "script", &[("request_id", "c"), ("expected", &h), ("script", "doll.cad.splash"), ("source", &source.replace("let head =", "let skull =").replace("[torso head]", "[torso skull]"))]);
    let r = s.call("select", &args("{}"), None);
    assert!(r.ok && r.text.contains("dropped") && r.text.contains(&id) && r.text.contains("nothing is picked"), "{}", r.text);
    // Every pick of a set resolves, or nothing changes; remove and clear.
    let got = select(&mut s, r#"{"set":["skull","torso"]}"#);
    assert_eq!(got.len(), 2);
    for (bad, says) in [(r#"{"set":["skull","tail"]}"#, "pick 2: no solid 'tail'"), (r#"{"set":[["skull"]]}"#, "a crease is two solids"), (r#"{"remove":"p99"}"#, "no pick p99"), (r#"{"set":[{"solid":"skull","kind":"bend"}]}"#, "'kind' is one of")] {
        let r = s.call("select", &args(bad), None);
        assert!(!r.ok && r.text.contains(says), "{bad}: {}", r.text);
    }
    assert_eq!(select(&mut s, "{}").len(), 2, "a refused set changes nothing");
    let first = got[0].0.clone();
    assert_eq!(select(&mut s, &format!(r#"{{"remove":"{first}"}}"#)).len(), 1);
    assert!(select(&mut s, r#"{"clear":true}"#).is_empty());
}

/// The AI's loop on a jointed chain: write it, check its joints, run it, read the verdicts.
/// (The mechanics themselves, including a two-armed ball-and-hinge puppet, are csg_mech's tests.)
#[test]
fn ai_loop_chain_checks_and_runs() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet links = for i in 0..3 { m.box({size: [12mm 6mm 5mm] at: [i * 12mm + 6mm 3mm 0]}) }\nlet chain = m.chain(links)\nm.anchor(links[0], {at: [2.5mm 3mm 0]})\n";
    let r = call(&mut s, "script", &[("request_id", "p"), ("expected", &h), ("source", src)]);
    assert!(r.ok, "{}", r.text);
    let r = call(&mut s, "joints", &[]);
    assert!(r.ok && r.text.replace(char::is_whitespace, "").starts_with("{\"ok\":true"), "fab.joints: {}", r.text);
    for scenario in ["hang", "shake"] {
        let r = call(&mut s, "simulate", &[("scenario", scenario)]);
        let t = r.text.replace(char::is_whitespace, "");
        assert!(r.ok && t.contains("articulates:true") && t.contains("hangs_right:true"), "fab.simulate {scenario}: {}", r.text);
    }
}

/// The AI's loop on chainmail: write a heart of mail, check its links, drape and hang it, read the verdicts.
#[test]
fn ai_loop_weave_checks_and_runs() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet heart = m.mail(Path{from: [0, -35mm] segs: [SplineTo{points: [[30mm, -5mm] [30mm, 20mm] [15mm, 30mm] [0, 18mm]]} SplineTo{points: [[-15mm, 30mm] [-30mm, 20mm] [-30mm, -5mm] [0, -35mm]]}]})\nobject(heart)\n";
    let r = call(&mut s, "script", &[("request_id", "p"), ("expected", &h), ("source", src)]);
    assert!(r.ok, "{}", r.text);
    let t = std::time::Instant::now();
    let r = call(&mut s, "weave", &[]);
    println!("fab.weave {:.0} ms: {} chars\n{}", t.elapsed().as_secs_f64() * 1e3, r.text.len(), r.text);
    let compact = r.text.replace(char::is_whitespace, "");
    assert!(r.ok && compact.starts_with("{\"ok\":true") && compact.contains("pieces:1"), "{}", r.text);
    let t = std::time::Instant::now();
    let r = s.call("weave", &args("{\"as_printed\":true}"), None);
    println!("fab.weave as printed {:.0} ms:\n{}", t.elapsed().as_secs_f64() * 1e3, r.text);
    assert!(r.ok && r.text.replace(char::is_whitespace, "").starts_with("{\"ok\":true"), "{}", r.text);
    for scenario in ["drape", "hang", "drop"] {
        let r = call(&mut s, "simulate", &[("scenario", scenario), ("seconds", "3")]);
        println!("fab.simulate {scenario}: {}", r.text);
        assert!(r.ok && r.text.replace(char::is_whitespace, "").contains("holds_together:true"), "{}", r.text);
    }
}

/// One volume-fill candidate as a plate part (the links frame is Y up, the printer Z up, centred on the bed).
fn weave_plate(w: &makepad_csg_mech::weave::Weave) -> (fab_plate::Plate, Vec<fab_model::weave::WeavePart>) {
    let links = std::sync::Arc::new(w.links());
    let to_plate = [[1.0, 0.0, 0.0, 175.0], [0.0, 0.0, -1.0, 175.0], [0.0, 1.0, 0.0, 0.0]];
    let mesh = links.mesh(12);
    let positions = mesh.vertices.iter().map(|v| fab_model::weave::affine(&to_plate, [v.x, v.y, v.z])).collect();
    let part = fab_plate::Part { body_of: None, print_mesh: None, paint: None,
        object: "fill".into(),
        name: "fill".into(),
        filament: 1,
        color: [0.8, 0.8, 0.8, 1.0],
        nozzle: None,
        mesh: fab_plate::PartMesh { positions, triangles: mesh.triangles.clone() },
        field_bounds: None, field_floor: None,
        print: Vec::new(),
        ..Default::default()
    };
    (fab_plate::Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part] }, { let slots = vec![0; links.loops.len()]; vec![fab_model::weave::WeavePart::new(0, links, to_plate, slots)] })
}

/// Volume-fill candidates judged by the slicer (Fab's default process): the stacked tiers of flat mail print
/// tier two on air; the leaning links (every one standing on the bed) print with no support at every lean
/// tried (45 and the default 60), nothing fuses, the links move in the sim, and the default lean (60deg) is checked hardest.
#[test]
fn volume_mail_candidates_judged_by_the_slicer() {
    use makepad_csg_mech::weave::{self as wv, Frame, GapClass, LeanLink, SquareLink, WeaveProfile, WeaveScene};
    let profile = makepad_csg_mech::Profile::default();
    let wp = WeaveProfile::new(&profile, None);
    let frame = Frame::new([0.0; 3], [0.0, 1.0, 0.0]);
    let machine = Machine::dual_nozzle_350();
    let (lo, hi) = ([-12.0, -12.0, 0.0], [12.0, 12.0, 12.0]);
    let mut candidates: Vec<(String, wv::Weave)> = Vec::new();
    let flat = wv::mail(&wp, SquareLink::new(&wp, None), frame, [lo[0], lo[1]], [hi[0], hi[1]], 0.0, &|_| -1.0);
    // tiers: the flat mail again one thickness and a layer gap higher, half a cell over
    let mut tiers = flat.clone();
    let lift = flat.link.thickness() + wp.c_z;
    let n = flat.links.len() as u32;
    for l in &flat.links {
        let mut up = l.clone();
        up.pts = l.pts.iter().map(|p| frame.to_model({ let q = frame.to_local(*p); [q[0] + 0.5 * flat.link.s, q[1], q[2] + lift] })).collect();
        tiers.links.push(up);
    }
    tiers.pairs.extend(flat.pairs.iter().map(|(a, b)| (a + n, b + n)));
    candidates.push(("flat mail (one layer)".into(), flat));
    candidates.push(("stacked tiers".into(), tiers));
    for deg in [45.0f64, 60.0] {
        let link = LeanLink::new(&wp, deg.to_radians());
        candidates.push((format!("leaning {deg:.0}deg"), wv::trellis(&wp, link, frame, lo, hi, 0.0, 0.0, &|_| -1.0)));
    }
    let mut rows = Vec::new();
    for (name, w) in &candidates {
        let c = wv::check(w, &wp);
        let (pieces, _) = w.pieces();
        let (plate, weaves) = weave_plate(w);
        let t = std::time::Instant::now();
        let p = weave::judge(&machine, &plate, &weaves, &weave::default_setup(&machine, 1), &[], 0.15).expect("slices").remove(0);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let sim = wv::simulate(w, fab_model::weave::DENSITY, WeaveScene::Drop, 2.0, &|| false).unwrap();
        println!(
            "{name:22} links {:3} interlocks {:4} pieces {pieces} | design {:?} gap xy {:.2} z {:.2} bridge {:.1} | sliced {} layers: floating {} unanchored {} merged {} bridge {:.1} steep(bed/bridge/support) {:.0}/{:.0}/{:.1} mm2 | fused {} bridged {} bodies {} | sim unlinked {} pairs moved {:.2} mm | {:.0} ms",
            w.links.len(), w.pairs.len(), c.verdict, c.min_gap_xy.0, c.min_gap_z.0, c.bridge, p.layers, p.islands.floating.len(), p.islands.unanchored.len(), p.islands.merged.len(), p.islands.longest_bridge.0,
            p.steep.on_bed, p.steep.bridge, p.steep.support, p.beads.fused.len(), p.beads.bridged.len(), p.bodies, sim.unlinked, sim.pair_motion, ms
        );
        rows.push((name.clone(), c.verdict == GapClass::Ok && pieces == 1, p.support_free(), p.moves(), sim.unlinked == 0 && sim.pair_motion > wp.c_xy, p.islands.floating.len(), w.pairs.len()));
    }
    let row = |n: &str| rows.iter().find(|r| r.0 == n).unwrap().clone();
    // the flat mail is the control: one layer prints free
    let flat = row("flat mail (one layer)");
    assert!(flat.1 && flat.2 && flat.3, "{flat:?}");
    // tiers print on air (or on the tier below, which fuses)
    assert!(row("stacked tiers").5 > 0, "{:?}", row("stacked tiers"));
    for deg in ["45", "60"] {
        let r = row(&format!("leaning {deg}deg"));
        assert!(r.1 && r.2 && r.3 && r.4, "leaning {deg}deg: design ok, support free, moves, sim moves: {r:?}");
    }
}

/// The language's volume fill: a box filled with leaning links, checked as printed through fab.weave.
#[test]
fn volume_mail_prints_without_support() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let src = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = m.mail(m.box({size: [24mm, 14mm, 18mm], at: [0, 7mm, 0]}), {fill: @volume})\nobject(block)\n";
    let r = call(&mut s, "script", &[("request_id", "p"), ("expected", &h), ("source", src)]);
    assert!(r.ok, "{}", r.text);
    let r = call(&mut s, "weave", &[]);
    println!("fab.weave volume:\n{}", r.text);
    assert!(r.ok && r.text.replace(char::is_whitespace, "").contains("pieces:1"), "{}", r.text);
    let t = std::time::Instant::now();
    let r = s.call("weave", &args("{\"as_printed\":true}"), None);
    println!("fab.weave volume as printed {:.0} ms:\n{}", t.elapsed().as_secs_f64() * 1e3, r.text);
    let compact = r.text.replace(char::is_whitespace, "");
    assert!(r.ok && compact.starts_with("{\"ok\":true") && compact.contains("moves:true") && compact.contains("free:true"), "{}", r.text);
    let r = call(&mut s, "simulate", &[("scenario", "drop"), ("seconds", "2")]);
    println!("fab.simulate volume drop: {}", r.text);
    assert!(r.ok && r.text.replace(char::is_whitespace, "").contains("holds_together:true"), "{}", r.text);
}

const PRELUDE: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n";

/// The guide's fenced blocks of one kind.
fn guide_blocks(kind: &str) -> Vec<String> {
    let fence = format!("```{kind}\n");
    GUIDE.split(fence.as_str()).skip(1).map(|b| b.split("```").next().unwrap().to_string()).collect()
}

fn tool_ok(s: &mut Session, tool: &str, fields: &[(&str, &str)]) -> Reply {
    let r = call(s, tool, fields);
    assert!(r.ok, "{tool} {fields:?}: {}", r.text);
    r
}

/// The guide fits a tool result in every part, and each part is reachable.
#[test]
fn guide_fits_in_tool_results() {
    const LIMIT: usize = 16 * 1024;
    let core = guide_section(None).unwrap();
    assert!(core.len() < 8 * 1024, "the core is {} bytes", core.len());
    let names = core.rsplit("## Sections").next().unwrap().to_string();
    let listed: Vec<&str> = names.split('`').skip(1).step_by(2).collect();
    assert!(listed.len() >= 10, "{listed:?}");
    let vocabulary = guide_section(Some("vocabulary")).unwrap();
    let groups: Vec<String> = vocabulary.split(": ").nth(1).unwrap().split(". ").next().unwrap().split(", ").map(|g| format!("vocabulary {g}")).collect();
    assert!(groups.len() > 8, "{vocabulary}");
    for name in listed.iter().copied().chain(groups.iter().map(String::as_str)) {
        let text = guide_section(Some(name)).unwrap_or_else(|e| panic!("{e}"));
        assert!(text.len() < LIMIT, "section {name} is {} bytes", text.len());
    }
    assert!(guide_section(Some("nothing")).is_err());
}

/// What the guide names is what the code has, both ways.
#[test]
fn guide_names_the_actual_api() {
    use makepad_model_script::lang::protos::{Group, PROTOS, VALUE_CALLS};
    let core = guide_section(None).unwrap();
    for p in PROTOS.iter().filter(|p| p.group == Group::Brep) {
        assert!(core.contains(p.call), "the guide's core lacks m.brep.{}", p.call);
    }
    let mut known: std::collections::HashSet<String> = PROTOS.iter().filter(|p| !p.call.is_empty()).map(|p| p.path()).chain(VALUE_CALLS.iter().map(|c| c.to_string())).collect();
    // the paint statements are the model's root calls, not builders
    known.extend(fab_model::program::ROOTS.iter().filter(|r| r.model).map(|r| r.name.to_string()));
    let text = GUIDE.replace("m.brep.*", "");
    for piece in text.split("m.").skip(1) {
        let name: String = piece.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.').collect();
        let name = name.trim_end_matches('.');
        // A call, written as code (`m.box(`, `m.brep.fillet(`).
        if piece[name.len()..].starts_with('(') {
            assert!(known.contains(name), "the guide names m.{name}, which does not exist");
        }
    }
}

/// An item printed into a part reads back with the part it goes into and the room the slice
/// leaves for it; a plain insert has no room; a heat-set insert reads back with its hole and what
/// its part has round and under it.
#[test]
fn an_embedded_item_reads_back_with_its_room() {
    use makepad_script::syntax::{parse_value, Node};
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    // the app's preludes, as every document builds with
    s.model.set_env(fab_elec_mech::bridge::CaseInputs::default().env());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let a = heat_insert(\"M3\", {{at: [8mm 6mm 8mm]}})\nlet body = heat_seat(m.box({{size: [30mm 12mm 30mm]}}), [a])\nobject(body)\ninsert(m.box({{size: [6mm 2mm 6mm] at: [0 4mm 0]}}), name: \"M3 nut\", into: \"body\", clearance: 0.25mm, embed: true)\ninsert(m.box({{size: [2mm 2mm 2mm] at: [8mm 4mm 0]}}), name: \"Pin\", into: \"body\")\ninsert(a.part, name: \"M3 insert\", into: \"body\", melt: a)\n");
    tool_ok(&mut s, "script", &[("request_id", "e"), ("expected", &h), ("script", "embed.cad.splash"), ("source", &source)]);
    let ev = s.last.clone().unwrap();
    let nut = ev.plate.excluded.iter().find(|p| p.part.name == "M3 nut").unwrap().part.bounds();
    let text = s.call("inspect", &args(r#"{"program":false}"#), None).text;
    let root = parse_value(&text).unwrap_or_else(|e| panic!("{e:?}: {text}"));
    let field = |n: &Node, k: &str| n.field(k).unwrap_or_else(|| panic!("no {k} in {text}")).value.clone();
    let parts = field(&field(&root, "plate"), "parts");
    let part = |name: &str| parts.items().unwrap().iter().find(|p| p.field("name").is_some_and(|f| f.value.text(&text) == format!("\"{name}\""))).unwrap_or_else(|| panic!("{name} in {text}")).clone();
    let mm = |n: &Node| n.text(&text).trim_end_matches("mm").parse::<f64>().unwrap_or_else(|_| panic!("{} in {text}", n.text(&text)));
    let point = |n: Node| -> Vec<f64> { n.items().unwrap().iter().map(mm).collect() };
    let (nut_part, pin_part) = (part("M3 nut"), part("Pin"));
    assert_eq!(field(&nut_part, "into").text(&text), field(&part("body"), "solid").text(&text), "into names its part as the plate does: {text}");
    let embed = field(&nut_part, "embed");
    assert_eq!(mm(&field(&embed, "clearance")), 0.25);
    let near = |a: Vec<f64>, b: [f64; 3]| a.len() == 3 && a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.006);
    assert!(near(point(field(&embed, "cavity_min")), [nut.min[0] - 0.25, nut.min[1] - 0.25, nut.min[2]]), "the room starts at the item's bottom, its clearance round it: {text}");
    assert!(near(point(field(&embed, "cavity_max")), [nut.max[0] + 0.25, nut.max[1] + 0.25, nut.max[2] + 0.25]), "and keeps its clearance over it: {text}");
    assert!(pin_part.field("into").is_some() && pin_part.field("embed").is_none() && pin_part.field("melt").is_none(), "a plain insert has no room: {text}");
    let melt = field(&part("M3 insert"), "melt");
    assert_eq!(field(&melt, "size").text(&text), "\"M3\"", "{text}");
    assert_eq!((mm(&field(&melt, "hole")), mm(&field(&melt, "depth")), mm(&field(&melt, "wants_wall")), mm(&field(&melt, "wants_floor"))), (4.0, 5.0, 2.0, 1.2), "the size's hole and what it wants: {text}");
    assert!(mm(&field(&melt, "wall")) >= 2.0 && (mm(&field(&melt, "floor")) - 7.0).abs() < 0.05 && melt.field("no_hole").is_none(), "measured: the 12 mm body holds the 5 mm hole with 7 mm under it: {text}");
    assert!(field(&root, "plate").field("warnings").is_none(), "a seated insert warns of nothing: {text}");
}

/// Every object script in the guide builds, in every section.
#[test]
fn guide_examples_build() {
    let blocks = guide_blocks("splash");
    assert!(blocks.len() >= 3, "{}", blocks.len());
    for (i, block) in blocks.iter().enumerate() {
        let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
        // the app's preludes (`heat_insert`, `enclosure`), as every document builds with
        s.model.set_env(fab_elec_mech::bridge::CaseInputs::default().env());
        s.rebuild().unwrap();
        let h = head(&s);
        let source = format!("{PRELUDE}{block}");
        let r = call(&mut s, "script", &[("request_id", "g"), ("expected", &h), ("script", &format!("g{i}.cad.splash")), ("source", &source)]);
        assert!(r.ok, "guide example {i}:\n{block}\n{}", r.text);
    }
}

fn edit_base() -> Session {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let height = param(20mm, min: 5mm, max: 90mm)\nlet width = 30mm\nlet hole = m.cylinder({{radius: 3mm height: height}})\nlet lid = m.box({{size: [10mm 2mm 10mm]}})\nlet lightening = m.cylinder({{radius: 1mm height: 8mm}})\nlet bracket = m.subtract(m.box({{size: [20mm 6mm 20mm]}}), [hole])\nlet ears = m.blend([m.ellipsoid({{radii: [3mm 5mm 2mm] at: [-6mm 0 0]}}) m.ellipsoid({{radii: [3mm 5mm 2mm] at: [6mm 0 0]}})], 1mm)\nobject(hole)\nlet red = Material{{albedo: [0.8 0.05 0.05]}}\nlet blue = Material{{albedo: [0.1 0.3 0.8]}}\nm.fill(hole, blue)\nm.fill(hole, red)\nm.stroke([[0 0 3mm] [5mm 0 3mm]], 1mm, red)\n");
    tool_ok(&mut s, "script", &[("request_id", "b"), ("expected", &h), ("script", "bracket.cad.splash"), ("source", &source)]);
    s
}

/// A `-` inside `[ ]` that reads like a negative item (`[10mm 10mm 12mm -2mm]` is three items,
/// the last `12mm - 2mm`) builds, and the parser's warning shows where script warnings do: in
/// the plate the AI reads back, and in the build's warnings (Fab's status line).
#[test]
fn a_minus_inside_brackets_warns_the_ai_and_fab() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let toy = m.box({{size: [10mm 10mm 12mm -2mm]}})\nobject(toy)\n");
    let r = tool_ok(&mut s, "script", &[("request_id", "w"), ("expected", &h), ("script", "toy.cad.splash"), ("source", &source)]);
    let says = "inside [ ] reads as one value, 12mm - 2mm: write `12mm - 2mm`, or `12mm, -2mm` for two items";
    assert!(r.text.contains(says), "{}", r.text);
    let ev = s.last.clone().unwrap();
    assert!(ev.warnings.iter().any(|w| w.starts_with("toy.cad.splash:4:") && w.contains(says)), "{:?}", ev.warnings);
}

/// fab.place renames a print colour and all that names it, the materials in the scripts too, as
/// one undo step; a name taken is refused. fab.params and fab.inspect list the palette: each
/// colour with what uses it, a default name marked.
#[test]
fn a_colour_rename_reaches_the_scripts_and_the_palette_lists_it() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let toy = m.box({{size: [20mm 20mm 20mm]}})\nobject(toy)\nm.fill(m.sphere({{radius: 3mm at: [0 10mm 0]}}), Material{{albedo: [0.1 0.1 0.8] name: \"Colour 2\"}})\nm.stroke([[-5mm 10mm 5mm] [5mm 10mm 5mm]], 1mm, Material{{albedo: [0.8 0.1 0.1] name: \"Colour 2\"}})\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "toy.cad.splash"), ("source", &source)]);
    let h = head(&s);
    tool_ok(&mut s, "place", &[("request_id", "b"), ("expected", &h), ("edits", "color(\"Base\")\ncolor(\"Colour 2\")\nplace(\"toy\", color: \"Colour 2\")")]);
    let entry = |text: &str, name: &str| -> String {
        let at = text.find(&format!("name: \"{name}\"")).unwrap_or_else(|| panic!("no {name} in the palette: {text}"));
        text[at..].split('}').next().unwrap().to_string()
    };
    let params = tool_ok(&mut s, "params", &[]).text;
    let listed = entry(&params[params.find("palette:").expect("params lists the palette")..], "Colour 2");
    for want in ["default_name: true", "\"toy/toy\"", "\"toy/fill#1\"", "\"toy/stroke#1\""] {
        assert!(listed.contains(want), "{want} in {listed}");
    }
    let h = head(&s);
    let renamed = tool_ok(&mut s, "place", &[("request_id", "c"), ("expected", &h), ("edits", "rename(color: \"Colour 2\", to: \"Eyes\")")]);
    assert!(renamed.text.contains("changed: [\"toy\"]"), "{}", renamed.text);
    let script = s.scripts()["toy.cad.splash"].clone();
    assert_eq!((script.matches("name: \"Eyes\"").count(), script.contains("Colour 2")), (2, false), "{script}");
    let inspect = tool_ok(&mut s, "inspect", &[]).text;
    let listed = entry(&inspect[inspect.find("palette:").expect("inspect lists the palette")..], "Eyes");
    assert!(listed.contains("\"toy/fill#1\"") && !listed.contains("default_name"), "{listed}");
    assert!(!inspect.contains("Colour 2"), "{inspect}");
    // A name another colour has is refused; one undo puts the old name back everywhere.
    let h = head(&s);
    let clash = call(&mut s, "place", &[("request_id", "d"), ("expected", &h), ("edits", "rename(color: \"Eyes\", to: \"Base\")")]);
    assert!(!clash.ok && clash.text.contains("named 'Base' already"), "{}", clash.text);
    assert!(call(&mut s, "history", &[("expected", &h), ("action", "undo")]).ok);
    assert!(s.scripts()["toy.cad.splash"].contains("name: \"Colour 2\""));
}

/// The guide's edits apply, each on a fresh script, the three that belong together as one.
#[test]
fn guide_edits_apply() {
    for block in guide_blocks("splash-edit") {
        let lines: Vec<&str> = block.lines().map(|l| l.split("//").next().unwrap().trim()).filter(|l| !l.is_empty()).collect();
        let (adds, single): (Vec<&str>, Vec<&str>) = lines.iter().partition(|l| l.starts_with("let ") || l.contains("badge"));
        let batches: Vec<String> = single.iter().map(|l| l.to_string()).chain([adds.join("\n")]).collect();
        for batch in batches {
            let mut s = edit_base();
            let h = head(&s);
            let r = call(&mut s, "apply", &[("request_id", "e"), ("expected", &h), ("edits", &batch)]);
            assert!(r.ok, "guide edit:\n{batch}\n{}", r.text);
        }
    }
}

/// set() takes the keys fab.params lists as written, with their instance: an element of an inner
/// call's vector (`radii.1` is one site, not `radii` set to `.1`) and a vector whole, one batch.
#[test]
fn apply_sets_the_keys_params_lists() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let cut = m.box({{size: [30mm 10mm 30mm] at: [0, -2mm, 0]}})\nlet body = m.carve(m.blend([m.ellipsoid({{radii: [6mm 4mm 5mm] at: [0 5mm 0]}}) m.sphere({{radius: 3mm at: [0 9mm 0]}})], 1mm), [cut], 0.5mm)\nobject(body)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "toy.cad.splash"), ("source", &source)]);
    let params = tool_ok(&mut s, "params", &[("prefix", "body.ellipsoid.radii")]);
    let key = params.text.split('"').find(|k| k.ends_with("body.ellipsoid.radii.1")).unwrap_or_else(|| panic!("{}", params.text)).to_string();
    let h = head(&s);
    tool_ok(&mut s, "apply", &[("request_id", "b"), ("expected", &h), ("edits", &format!("set({key}, 4.4mm)\nset(cut.size, [40mm 10mm 40mm])"))]);
    let after = s.model.source("toy.cad.splash").unwrap();
    assert!(after.contains("radii: [6mm 4.4mm 5mm]") && after.contains("size: [40mm 10mm 40mm]"), "{after}");
    let h = head(&s);
    let r = call(&mut s, "apply", &[("request_id", "c"), ("expected", &h), ("edits", "set(cut.size)")]);
    assert!(!r.ok && r.text.contains("set takes (site, value)"), "{}", r.text);
}

/// set() takes every key fab.params prints, exactly as printed: the second of two ellipsoids of
/// one part (`#2`), whole and by element, written bare or quoted.
#[test]
fn apply_sets_a_numbered_site_as_params_prints_it() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let ears = m.blend([m.ellipsoid({{radii: [3mm 5mm 2mm] at: [-6mm 0 0]}}) m.ellipsoid({{radii: [3mm 5mm 2mm] at: [6mm 0 0]}})], 1mm)\nobject(ears)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "toy.cad.splash"), ("source", &source)]);
    let listed = tool_ok(&mut s, "params", &[("prefix", "ears.ellipsoid#2.radii")]).text;
    let keys: Vec<&str> = listed.split('"').filter(|k| k.contains("#2.radii")).collect();
    assert_eq!(keys, ["toy/ears.ellipsoid#2.radii.0", "toy/ears.ellipsoid#2.radii.1", "toy/ears.ellipsoid#2.radii.2"], "{listed}");
    let whole = keys[0].trim_end_matches(".0");
    for (k, (edits, want)) in [
        (format!("set({whole}, [4mm 6mm 2mm])"), "[4mm 6mm 2mm]"),
        (format!("set({}, 7mm)", keys[1]), "[4mm 7mm 2mm]"),
        (format!("set(\"{}\", 2.5mm)", keys[2]), "[4mm 7mm 2.5mm]"),
    ]
    .into_iter()
    .enumerate()
    {
        let h = head(&s);
        tool_ok(&mut s, "apply", &[("request_id", &format!("b{k}")), ("expected", &h), ("edits", &edits)]);
        let after = s.model.source("toy.cad.splash").unwrap();
        assert!(after.contains("radii: [3mm 5mm 2mm] at: [-6mm 0 0]") && after.contains(&format!("radii: {want} at: [6mm 0 0]")), "{edits}: {after}");
    }
}

/// Fills have no name: fab.params lists them by number in script order (filtered by prefix or by
/// a solid they paint), and fab.apply sets either argument, moves one to another's place and
/// removes one by that number; a material only a fill uses names the fill to remove.
#[test]
fn apply_reaches_fills_by_number() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let body = m.box({{size: [20mm 10mm 20mm]}})\nlet knob = m.sphere({{radius: 4mm at: [0 7mm 0]}})\nlet toy = m.union(body, [knob])\nobject(toy)\nlet cream = Material{{albedo: [0.9 0.85 0.7]}}\nlet red = Material{{albedo: [0.8 0.05 0.05]}}\nlet blue = Material{{albedo: [0.1 0.3 0.8]}}\nm.fill(toy, cream)\nm.fill(knob, red)\nm.fill(body, blue)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "toy.cad.splash"), ("source", &source)]);
    let fills = |s: &mut Session, args: &[(&str, &str)]| -> Vec<String> {
        let text = tool_ok(s, "params", args).text;
        text.lines().filter(|l| l.contains("fill#")).map(|l| l.trim().to_string()).collect()
    };
    assert_eq!(
        fills(&mut s, &[]),
        [
            r#"{key: "toy/fill#1" part: "toy/toy" region: "toy" material: "cream"}"#,
            r#"{key: "toy/fill#2" part: "toy/toy" region: "knob" material: "red"}"#,
            r#"{key: "toy/fill#3" part: "toy/toy" region: "body" material: "blue"}"#
        ]
    );
    assert_eq!(fills(&mut s, &[("prefix", "fill#2.mat")]), [r#"{key: "toy/fill#2" part: "toy/toy" region: "knob" material: "red"}"#]);
    assert_eq!(fills(&mut s, &[("solid", "knob")]).len(), 2, "the union holds the knob: fill#1 and fill#2 paint it");
    let apply = |s: &mut Session, edits: &str| {
        let h = head(s);
        call(s, "apply", &[("request_id", edits), ("expected", &h), ("edits", edits)])
    };
    let fill_lines = |s: &Session| -> Vec<String> { s.model.source("toy.cad.splash").unwrap().lines().filter(|l| l.starts_with("m.fill")).map(str::to_string).collect() };
    let steps: [(&str, &[&str]); 4] = [
        ("set(toy/fill#3.material, cream)\nset(fill#2.region, m.sphere({radius: 2mm at: [0 11mm 0]}))", &["m.fill(toy, cream)", "m.fill(m.sphere({radius: 2mm at: [0 11mm 0]}), red)", "m.fill(body, cream)"]),
        ("move(fill#3, fill#1)", &["m.fill(body, cream)", "m.fill(toy, cream)", "m.fill(m.sphere({radius: 2mm at: [0 11mm 0]}), red)"]),
        ("move(fill#1, fill#3)", &["m.fill(toy, cream)", "m.fill(m.sphere({radius: 2mm at: [0 11mm 0]}), red)", "m.fill(body, cream)"]),
        ("remove(toy/fill#2)", &["m.fill(toy, cream)", "m.fill(body, cream)"]),
    ];
    for (edits, want) in steps {
        let r = apply(&mut s, edits);
        assert!(r.ok, "{edits}: {}", r.text);
        assert_eq!(fill_lines(&s), want, "{edits}");
    }
    let r = apply(&mut s, "remove(red)");
    assert!(r.ok, "red is no longer used: {}", r.text);
    let r = apply(&mut s, "remove(cream)");
    assert!(!r.ok && r.text.contains("remove(fill#1)"), "{}", r.text);
    let r = apply(&mut s, "remove(fill#3)");
    assert!(!r.ok && r.text.contains("no fill#3: the file has 2"), "{}", r.text);
    let r = apply(&mut s, "move(fill#1, knob)");
    assert!(!r.ok && r.text.contains("not both numbered"), "{}", r.text);
}

/// Strokes are numbered as fills are: fab.params lists each with the part nearest its first point
/// and its points, radius and material as written (filtered by prefix or by a solid it touches),
/// and fab.apply sets an argument, a point or one coordinate, moves one and removes one.
#[test]
fn apply_reaches_strokes_by_number() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let body = m.box({{size: [20mm 10mm 20mm]}})\nobject(body)\nlet lid = m.box({{size: [10mm 2mm 10mm] at: [40mm 0 0]}})\nobject(lid)\nlet red = Material{{albedo: [0.8 0.05 0.05]}}\nlet blue = Material{{albedo: [0.1 0.3 0.8]}}\nm.stroke([[0 5mm 0] [5mm 5mm 0]], 1mm, red)\nm.stroke([[-10mm 0 0] [-10mm 2mm 3mm]], 0.5mm, blue)\nm.stroke([[40mm 1mm 0] [42mm 1mm 0]], 1mm, red)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "toy.cad.splash"), ("source", &source)]);
    let strokes = |s: &mut Session, args: &[(&str, &str)]| -> Vec<String> {
        let text = tool_ok(s, "params", args).text;
        text.lines().filter(|l| l.contains("stroke#")).map(|l| l.trim().to_string()).collect()
    };
    let all = [
        r#"{key: "toy/stroke#1" part: "toy/body" points: "[[0 5mm 0] [5mm 5mm 0]]" radius: "1mm" material: "red"}"#,
        r#"{key: "toy/stroke#2" part: "toy/body" points: "[[-10mm 0 0] [-10mm 2mm 3mm]]" radius: "0.5mm" material: "blue"}"#,
        r#"{key: "toy/stroke#3" part: "toy/lid" points: "[[40mm 1mm 0] [42mm 1mm 0]]" radius: "1mm" material: "red"}"#,
    ];
    assert_eq!(strokes(&mut s, &[]), all);
    assert_eq!(strokes(&mut s, &[("prefix", "stroke#2")]), [all[1]]);
    assert_eq!(strokes(&mut s, &[("solid", "lid")]), [all[2]]);
    let apply = |s: &mut Session, id: &str, edits: &str| {
        let h = head(s);
        let r = call(s, "apply", &[("request_id", id), ("expected", &h), ("edits", edits)]);
        assert!(r.ok, "{edits}: {}", r.text);
        s.model.source("toy.cad.splash").unwrap().lines().filter(|l| l.starts_with("m.stroke")).map(str::to_string).collect::<Vec<_>>()
    };
    assert_eq!(
        apply(&mut s, "b", "set(stroke#1.radius, 2mm)\nset(stroke#1.points.1, [6mm 5mm 0])\nset(toy/stroke#2.material, red)\nset(stroke#2.points.0.2, 1mm)"),
        ["m.stroke([[0 5mm 0] [6mm 5mm 0]], 2mm, red)", "m.stroke([[-10mm 0 1mm] [-10mm 2mm 3mm]], 0.5mm, red)", "m.stroke([[40mm 1mm 0] [42mm 1mm 0]], 1mm, red)"]
    );
    assert_eq!(apply(&mut s, "c", "remove(stroke#3)\nmove(stroke#2, stroke#1)"), ["m.stroke([[-10mm 0 1mm] [-10mm 2mm 3mm]], 0.5mm, red)", "m.stroke([[0 5mm 0] [6mm 5mm 0]], 2mm, red)"]);
}

/// fab.apply puts what it adds where it works, whatever order the batch writes it in: a `use`
/// with the other imports, a `fn` or `let` before the first statement that reads it, and
/// `before(name)` before that name's statement.
#[test]
fn apply_adds_statements_before_their_use() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let base = m.box({{size: [20mm 4mm 20mm] at: [0 2mm 0]}})\nlet knob = m.sphere({{radius: 4mm at: [0 8mm 0]}})\nlet toy = m.union(base, knob)\nobject(toy)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "toy.cad.splash"), ("source", &source)]);
    let order = |s: &Session, marks: &[&str]| -> Vec<usize> {
        let text = s.model.source("toy.cad.splash").unwrap();
        marks.iter().map(|m| text.find(m).unwrap_or_else(|| panic!("no `{m}` in {text}"))).collect()
    };
    // the use first, then what reads it: the fn and its import go above the replaced knob
    let h = head(&s);
    tool_ok(&mut s, "apply", &[("request_id", "b"), ("expected", &h), ("edits", "replace(knob, m.sphere({radius: bump(2) at: [0 8mm 0]}))\nfn bump(k) { k * 2mm + sin(0) * 1mm }\nuse mod.math.*")]);
    let at = order(&s, &["use mod.fab.*", "use mod.math.*", "let m = mod.model", "fn bump", "let knob", "object(toy)"]);
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{}", s.model.source("toy.cad.splash").unwrap());
    // the fn first, then its use: it moves up to the statement that reads it
    let h = head(&s);
    tool_ok(&mut s, "apply", &[("request_id", "c"), ("expected", &h), ("edits", "fn wide(k) { k * 10mm }\nreplace(base, m.box({size: [wide(3) 4mm 20mm] at: [0 2mm 0]}))")]);
    let at = order(&s, &["fn wide", "let base", "let toy"]);
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{}", s.model.source("toy.cad.splash").unwrap());
    // before(name): a let nothing reads yet, above base rather than above the object
    let h = head(&s);
    tool_ok(&mut s, "apply", &[("request_id", "d"), ("expected", &h), ("edits", "before(base)\nlet spare = 3mm")]);
    let at = order(&s, &["let spare", "let base"]);
    assert!(at[0] < at[1], "{}", s.model.source("toy.cad.splash").unwrap());
    let h = head(&s);
    let r = call(&mut s, "apply", &[("request_id", "e"), ("expected", &h), ("edits", "before(base)")]);
    assert!(!r.ok && r.text.contains("none follows"), "{}", r.text);
}

/// fab.params unfiltered is a summary (declared params in full, each solid's name and count);
/// `solid` lists one solid's keys and those of the calls inside it, `prefix` the keys starting
/// so, both together narrow both ways; a name that is neither part nor solid is refused.
#[test]
fn params_filter_by_solid_and_prefix() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let source = format!("{PRELUDE}let w = param(30mm, min: 10mm, max: 60mm)\nlet base = m.box({{size: [w 6mm 20mm] at: [0 3mm 0]}})\nlet head = m.blend([m.ellipsoid({{radii: [6mm 7mm 6mm] at: [0 20mm 0]}}) m.sphere({{radius: 2mm at: [0 26mm 5mm]}})], 1mm)\nlet toy = m.blend([base head], 2mm)\nobject(toy)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "toy.cad.splash"), ("source", &source)]);
    let keys = |text: &str| -> Vec<String> { text.split("key: \"").skip(1).map(|k| k.split('"').next().unwrap().to_string()).collect() };
    let summary = tool_ok(&mut s, "params", &[]).text;
    assert_eq!(keys(&summary), ["toy/w"], "unfiltered: the declared params alone in full\n{summary}");
    assert!(summary.contains("{solid: \"toy/head.ellipsoid\" fields: 6}") && summary.contains("{solid: \"toy/head.sphere\" fields: 4}"), "each solid's count\n{summary}");
    let of_head = keys(&tool_ok(&mut s, "params", &[("solid", "head")]).text);
    assert!(!of_head.is_empty() && of_head.iter().all(|k| k.starts_with("toy/head.")), "the head and its operands: {of_head:?}");
    assert!(of_head.contains(&"toy/head.ellipsoid.radii.1".to_string()) && of_head.contains(&"toy/head.radius".to_string()), "{of_head:?}");
    assert_eq!(keys(&tool_ok(&mut s, "params", &[("prefix", "head.sphere.at")]).text), ["toy/head.sphere.at.0", "toy/head.sphere.at.1", "toy/head.sphere.at.2"]);
    assert_eq!(keys(&tool_ok(&mut s, "params", &[("solid", "toy/head"), ("prefix", "toy/head.ellipsoid.radii")]).text), ["toy/head.ellipsoid.radii.0", "toy/head.ellipsoid.radii.1", "toy/head.ellipsoid.radii.2"]);
    let r = call(&mut s, "params", &[("solid", "tail")]);
    assert!(!r.ok && r.text.contains("no solid 'tail'"), "{}", r.text);
    let r = call(&mut s, "params", &[("part", "toy/head")]);
    assert!(!r.ok && r.text.contains("solid: \\\"toy/head\\\""), "{}", r.text);
}

/// The guide's project statements apply, each on a fresh project.
#[test]
fn guide_placements_apply() {
    let set = format!("{PRELUDE}let lid = m.box({{size: [10mm 2mm 10mm]}})\nlet cup = m.box({{size: [10mm 8mm 10mm]}})\nobject(lid)\nobject(cup)\n");
    let vase = format!("{PRELUDE}let vase_height = param(60mm, min: 10mm, max: 90mm)\nlet vase = m.cylinder({{radius: 10mm height: vase_height}})\nobject(vase)\n");
    for block in guide_blocks("splash-place") {
        for line in block.lines().map(|l| l.split("//").next().unwrap().trim()).filter(|l| !l.is_empty()) {
            let mut s = edit_base();
            let h = head(&s);
            tool_ok(&mut s, "script", &[("request_id", "s"), ("expected", &h), ("script", "set.cad.splash"), ("source", &set)]);
            let h = head(&s);
            tool_ok(&mut s, "script", &[("request_id", "v"), ("expected", &h), ("script", "vase.cad.splash"), ("source", &vase)]);
            let h = head(&s);
            if line.starts_with("remove(") {
                tool_ok(&mut s, "place", &[("request_id", "t"), ("expected", &h), ("edits", "script(\"vase.cad.splash\", name: \"vase_tall\")")]);
            }
            let h = head(&s);
            let r = call(&mut s, "place", &[("request_id", "p"), ("expected", &h), ("edits", line)]);
            assert!(r.ok, "guide placement:\n{line}\n{}", r.text);
        }
    }
}

/// The AI's tool flow on an exact part, as a model would run it (a stand-in for the model):
/// read the guide, write a bracket, read its edges, fillet by what it read, bolt it, check it.
#[test]
fn a_filleted_bracket_through_the_tools() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let guide = call(&mut s, "guide", &[]);
    assert!(guide.ok && guide.text.contains("m.brep.fillet"), "{}", guide.text);
    let h = head(&s);
    let source = format!("{PRELUDE}let t = param(6mm, min: 3mm, max: 12mm)\nlet body = m.brep.extrude({{sketch: Polygon{{points: [[-20mm, 0] [20mm, 0] [20mm, t] [-14mm, t] [-14mm, 46mm] [-20mm, 46mm]]}} plane: @yz height: 60mm both: true}})\nobject(body)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "bracket.cad.splash"), ("source", &source)]);
    // Exact questions: one concave edge, the true volume of the L.
    let exact = tool_ok(&mut s, "exact", &[("object", "bracket/body")]);
    assert!(exact.text.contains("volume_mm3") && exact.text.contains("convex: false"), "{}", exact.text);
    // Each face's names as the script writes them, each edge's selector by its two faces.
    assert!(exact.text.contains("body.side(2)") && exact.text.contains("m.edge(body.side(2), body.side(3))"), "{}", exact.text);
    let edges = tool_ok(&mut s, "edges", &[("object", "bracket/body")]);
    assert!(edges.text.contains("exact: true") && edges.text.contains("line"), "{}", edges.text);
    // Fillet what it read, bolt it, emit the result.
    let h = head(&s);
    let edits = "let corner = m.brep.fillet(body, 3mm, {edges: {concave: true}})\nlet bolts = for x in [-20mm, 20mm] { m.brep.cylinder({radius: 2.5mm height: 20mm at: [x, 3mm, 8mm]}) }\nlet bracket = m.brep.subtract(corner, bolts)\nunobject(body)\nobject(bracket)";
    tool_ok(&mut s, "apply", &[("request_id", "b"), ("expected", &h), ("edits", edits)]);
    let exact = tool_ok(&mut s, "exact", &[("object", "bracket/bracket")]);
    assert!(exact.text.contains("cylinder") || exact.text.contains("cone"), "{}", exact.text);
    // The mesh tools take it as they take any part.
    let mesh = tool_ok(&mut s, "check_mesh", &[]);
    assert!(mesh.text.contains("closed: true") || !mesh.text.contains("closed: false"), "{}", mesh.text);
    let size = tool_ok(&mut s, "measure", &[("what", "size"), ("object", "bracket/bracket")]);
    assert!(size.text.contains("exact_volume_mm3"), "{}", size.text);
    // What it weighs and how it balances, as Fab prints it without a job.
    let mass = s.call("measure", &args(r#"{"what":"mass","object":"bracket/bracket","outline":true}"#), None);
    assert!(mass.ok, "{}", mass.text);
    let mass = mass.text;
    assert!(mass.contains("grams:") && mass.contains("centre_of_mass") && mass.contains("principal") && mass.contains("Fab's defaults"), "{mass}");
    assert!(mass.contains("stands: true") && mass.contains("tip_angle") && mass.contains("outline"), "an L on its long foot stands: {mass}");
    // Round the outer bend by the two faces it joins, as read: the names hold through the bolts' cut.
    let h = head(&s);
    tool_ok(&mut s, "apply", &[("request_id", "n"), ("expected", &h), ("edits", "let outer = m.brep.fillet(bracket, 2mm, {edges: m.edge(body.side(0), body.side(5))})\nunobject(bracket)\nobject(outer)")]);
    let exact = tool_ok(&mut s, "exact", &[("object", "bracket/outer")]);
    assert!(exact.text.contains("outer.face(m.edge(body.side(0), body.side(5)))"), "{}", exact.text);
    // Picks on it by what a script writes: the rounded bend's face, and the chain of edges that runs
    // round it on the end cap; each says what to write and outlines what it is.
    let r = s.call("select", &args(r#"{"set":[{"solid":"bracket/outer","select":"outer.face(m.edge(body.side(0), body.side(5)))"},{"solid":"bracket/outer","select":"m.edge(body.top, body.side(0), {chain: true})"}]}"#), None);
    assert!(r.ok && r.text.contains("kind: \"face\"") && r.text.contains("kind: \"chain\""), "{}", r.text);
    assert!(r.text.contains("select: \"m.edge(body.side(0), body.top, {chain: true})\"") && r.text.contains("A chain of 3 edges"), "the chain runs round the bend: {}", r.text);
    let picks = s.picks.list.clone();
    assert!(picks.len() == 2 && picks[0].outline.len() >= 4 && picks[1].outline.len() == 3, "a face outlined by its edges, a chain by its three: {:?}", picks.iter().map(|p| p.outline.len()).collect::<Vec<_>>());
    let r = s.call("select", &args(r#"{"set":[{"solid":"bracket/outer","select":"m.edge(body.side(0), body.side(4))"}]}"#), None);
    assert!(!r.ok && r.text.contains("no longer meet"), "faces that do not meet are no edge: {}", r.text);
    // A failure is an answer at the source line, not a crash: an edit that cannot build commits nothing.
    let h = head(&s);
    let r = call(&mut s, "apply", &[("request_id", "c"), ("expected", &h), ("edits", "let bad = m.brep.fillet(bracket, 400mm)\nobject(bad)")]);
    assert!(!r.ok && r.text.contains("bad"), "{}", r.text);
    tool_ok(&mut s, "check", &[]);
    // The selector report: a bore moved onto the edge a fillet names splits it in two; the build
    // stands and says so (by the solid, for the AI and the tree), once: the next build compares
    // with this one.
    let h = head(&s);
    let block = format!("{PRELUDE}let at = param(0mm, min: 0mm, max: 15mm)\nlet b = m.brep.box({{size: [40mm 20mm 30mm]}})\nlet bore = m.brep.cylinder({{radius: 5mm height: 60mm at: [0 0 at]}})\nlet cut = m.brep.subtract(b, bore)\nlet r = m.brep.fillet(cut, 1mm, {{edges: m.edge(b.top, b.front)}})\nobject(r)\n");
    tool_ok(&mut s, "script", &[("request_id", "d"), ("expected", &h), ("script", "block.cad.splash"), ("source", &block)]);
    assert!(s.last.as_ref().unwrap().drifted.is_empty(), "a new solid has nothing to drift from");
    let h = head(&s);
    let r = tool_ok(&mut s, "apply", &[("request_id", "e"), ("expected", &h), ("script", "block.cad.splash"), ("edits", "replace(at, param(15mm, min: 0mm, max: 15mm))")]);
    assert!(r.text.contains("`r`'s edge(top, front) took 1 line at the build before, now 2 lines"), "the split edge is reported: {}", r.text);
    let drifted = &s.last.as_ref().unwrap().drifted;
    assert!(drifted.len() == 1 && drifted[0].part.ends_with("/r") && drifted[0].short == "edge(top, front) took 2 lines, was 1 line", "{drifted:?}");
    let h = head(&s);
    let r = tool_ok(&mut s, "apply", &[("request_id", "f"), ("expected", &h), ("script", "block.cad.splash"), ("edits", "replace(at, param(14mm, min: 0mm, max: 15mm))")]);
    assert!(!r.text.contains("took") && s.last.as_ref().unwrap().drifted.is_empty(), "two pieces again is no drift: {}", r.text);
    // A draft that takes the whole top off loses the fillet's edge: it builds the fillet as its
    // input and says so; its commit is refused at the fillet; the selector named again commits.
    let capped = s.model.source("block.cad.splash").unwrap().replace("let cut = m.brep.subtract(b, bore)", "let cut = m.brep.subtract(m.brep.subtract(b, bore), m.brep.box({size: [60mm 4mm 60mm] at: [0 10mm 0]}))");
    let (c, ev, _) = s.preview_source("block.cad.splash", &capped, None).unwrap().unwrap();
    assert!(matches!(ev.lost.as_slice(), [l] if l.id == "r" && l.index == 0 && l.kind == "edge" && l.hint == "`r`'s fillet lost its edge: pick the edge it should round"), "{:?}", ev.lost);
    assert!(s.draft.is_some() && !ev.plate.parts.is_empty(), "the draft shows, picks are taken on it");
    let (r, problem) = s.set_source("block.cad.splash", &capped);
    assert!(!r.ok && r.text.contains("lost: \"edge\"") && r.text.contains("draft: \"`r`'s fillet lost its edge\"") && r.text.contains("`r`'s fillet lost its edge (") && r.text.contains("pick the edge it should round, or name a new selector"), "{}", r.text);
    assert!(problem.is_some_and(|p| p.line.is_some_and(|(line, _)| capped.lines().nth(line as usize - 1).is_some_and(|l| l.starts_with("let r =")))), "the problem is at the fillet's line");
    let prog = &c.instances.iter().find(|i| i.script.file == "block.cad.splash").unwrap().script.program;
    use makepad_model_script::lang::edit::selector_site;
    let site = selector_site(prog, "r", 0).unwrap();
    assert!(&capped[site.span.clone()] == "m.edge(b.top, b.front)" && site.shared.is_none() && site.names.is_none(), "{site:?}");
    let named = format!("{}m.edge(b.left, b.front){}", &capped[..site.span.start], &capped[site.span.end..]);
    let (r, problem) = s.set_source("block.cad.splash", &named);
    assert!(r.ok && problem.is_none() && s.draft.is_none(), "named again, it commits: {}", r.text);
    // Selectors written off the step's line: one a script fn's call passes (named again where the
    // call writes it), one in the list a top-level `let` binds (its item there), and one a fn's
    // body writes, named again there in that code's names for every call.
    let h = head(&s);
    let fns = format!("{PRELUDE}fn rounded(part) {{\n    m.brep.fillet(part, 1mm, {{edges: m.edge(part.top, part.front)}})\n}}\nfn round_by(part, e) {{\n    m.brep.fillet(part, 1mm, {{edges: e}})\n}}\nlet block = m.brep.box({{size: [40mm 20mm 30mm]}})\nlet r = rounded(block)\nlet other = m.brep.box({{size: [20mm 20mm 20mm] at: [60mm 0 0]}})\nlet r2 = round_by(other, m.edge(other.top, other.front))\nlet cup = m.brep.box({{size: [20mm 20mm 20mm] at: [-60mm 0 0]}})\nlet opens = [cup.top]\nlet s3 = m.brep.shell(cup, 2mm, {{open: opens}})\nobject(r)\nobject(r2)\nobject(s3)\n");
    tool_ok(&mut s, "script", &[("request_id", "g"), ("expected", &h), ("script", "fns.cad.splash"), ("source", &fns)]);
    let capped = fns.replace("    m.brep.fillet(part, 1mm, {edges: m.edge(part.top, part.front)})", "    let cut = m.brep.subtract(part, m.brep.box({size: [60mm 4mm 60mm] at: [0 10mm 0]}))\n    m.brep.fillet(cut, 1mm, {edges: m.edge(part.top, part.front)})");
    let (c, ev, _) = s.preview_source("fns.cad.splash", &capped, None).unwrap().unwrap();
    assert!(matches!(ev.lost.as_slice(), [l] if l.id == "r" && l.index == 0 && l.kind == "edge"), "{:?}", ev.lost);
    let prog = &c.instances.iter().find(|i| i.script.file == "fns.cad.splash").unwrap().script.program;
    let passed = selector_site(prog, "r2", 0).unwrap();
    assert!(&capped[passed.span.clone()] == "m.edge(other.top, other.front)" && passed.shared.is_none() && passed.names.is_none(), "the call writes it: {passed:?}");
    let listed = selector_site(prog, "s3", 0).unwrap();
    assert!(&capped[listed.span.clone()] == "cup.top" && listed.names.is_none(), "the `let` binds it: {listed:?}");
    let body = selector_site(prog, "r", 0).unwrap();
    assert_eq!(&capped[body.span.clone()], "m.edge(part.top, part.front)");
    let names = body.names.clone().unwrap_or_default();
    assert!(body.shared.as_deref() == Some("rounded") && names.contains(&("block".into(), "part".into())) && names.iter().any(|(_, n)| n == "cut"), "the fn's names, its parameter's and its `let`'s: {body:?}");
    let r = s.call("select", &args(r#"{"set":[{"solid":"fns/r","select":"m.edge(block.left, block.front)"}]}"#), None);
    assert!(r.ok, "{}", r.text);
    let written = s.picks.list.last().and_then(|p| p.written.clone()).expect("an exact pick carries its names");
    let text = written.text(|id| names.iter().find(|(part, _)| part == id).map(|(_, n)| n.clone())).unwrap();
    assert_eq!(text, "m.edge(part.front, part.left)");
    let named = format!("{}{text}{}", &capped[..body.span.start], &capped[body.span.end..]);
    let (r, problem) = s.set_source("fns.cad.splash", &named);
    assert!(r.ok && problem.is_none() && s.draft.is_none(), "named again in the fn, it commits: {}", r.text);
}

/// fab.fill paints a feature by name: one m.fill statement, the shape untouched.
#[test]
fn fill_paints_a_feature_by_name() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let bracket = include_str!("../../app/resources/bracket.cad.splash");
    let h = head(&s);
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "bracket.cad.splash"), ("source", bracket)]);
    let field_before = s.last.as_ref().unwrap().plate.parts[0].mesh.triangles.len();
    let h = head(&s);
    let args = Value::Obj(vec![
        ("request_id".into(), Value::Str("f".into())),
        ("expected".into(), Value::Str(h)),
        ("script".into(), Value::Str("bracket.cad.splash".into())),
        ("region".into(), Value::Str("holes".into())),
        ("albedo".into(), Value::Arr(vec![Value::F64(0.8), Value::F64(0.05), Value::F64(0.05)])),
        ("color".into(), Value::Str("Red".into())),
    ]);
    let r = s.call("fill", &args, None);
    assert!(r.ok, "{}", r.text);
    let src = s.source("bracket.cad.splash").unwrap();
    assert!(src.contains("m.fill(holes, Material{albedo: [0.8 0.05 0.05] name: \"Red\"})"), "{src}");
    let ev = s.last.clone().unwrap();
    assert_eq!(ev.plate.parts[0].mesh.triangles.len(), field_before, "the shape is untouched");
    let p = ev.paint[0].as_ref().expect("painted");
    assert_eq!(p.colors[1].color.as_deref(), Some("Red"));
    assert_eq!(p.colors[1].filament, Some(2), "a second colour prints with a second tool");
}

/// fab.fill with a seed writes a smooth-region flood, resolved on the part.
#[test]
fn fill_with_a_seed_floods_the_smooth_region() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let bracket = include_str!("../../app/resources/bracket.cad.splash");
    let h = head(&s);
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "bracket.cad.splash"), ("source", bracket)]);
    let h = head(&s);
    let args = Value::Obj(vec![
        ("request_id".into(), Value::Str("f".into())),
        ("expected".into(), Value::Str(h)),
        ("script".into(), Value::Str("bracket.cad.splash".into())),
        ("seed".into(), Value::Arr(vec![Value::F64(0.0), Value::F64(6.0), Value::F64(10.0)])),
        ("angle".into(), Value::Int(25)),
        ("albedo".into(), Value::Arr(vec![Value::F64(0.8), Value::F64(0.05), Value::F64(0.05)])),
    ]);
    let r = s.call("fill", &args, None);
    assert!(r.ok, "{}", r.text);
    let src = s.source("bracket.cad.splash").unwrap();
    assert!(src.contains("m.fill(Flood{seed: [0mm, 6mm, 10mm] angle: 25deg}, Material{albedo: [0.8 0.05 0.05]})"), "{src}");
    let ev = s.last.clone().unwrap();
    let p = ev.paint[0].as_ref().expect("painted");
    assert!(matches!(p.program.layers[0].region, fab_paint_field::Region::Patch(_)), "{:?}", p.program.layers[0].region);
    let colors = tool_ok(&mut s, "colors", &[]).text;
    assert!(colors.contains("smooth-region flood"), "{colors}");
}

/// A cube as the hub writes meshes: each face its own four vertices (UV seams split them),
/// wound outward, off the origin, in glTF's units.
fn seamed_cube_glb(side: f32, at: [f32; 3]) -> Vec<u8> {
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    for axis in 0..3 {
        for sign in [-1.0f32, 1.0] {
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            let base = positions.len() as u32;
            for (a, b) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let mut p = [0.0f32; 3];
                p[axis] = sign;
                p[u] = a;
                p[v] = b;
                positions.push(std::array::from_fn(|i| p[i] * side * 0.5 + at[i]));
            }
            let quad = if sign > 0.0 { [0, 1, 2, 0, 2, 3] } else { [0, 2, 1, 0, 3, 2] };
            indices.extend(quad.iter().map(|i| base + i));
        }
    }
    makepad_gltf::write_glb_mesh(&positions, &indices)
}

#[test]
fn a_hub_mesh_is_welded_sized_and_stood_on_the_ground() {
    let mesh = img2mesh::mesh_from_glb(&seamed_cube_glb(2.0, [3.0, 1.5, -2.0])).unwrap();
    assert_eq!((mesh.positions.len(), mesh.triangles.len()), (24, 12));
    let f = img2mesh::fit(&mesh, 40.0).unwrap();
    assert!(f.watertight(), "{f:?}");
    assert_eq!((f.mesh.positions.len(), f.shells, f.flipped), (8, 1, false));
    for i in 0..3 {
        assert!((f.size[i] - 40.0).abs() < 1e-3, "{:?}", f.size);
    }
    let low = f.mesh.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
    let mid_x = f.mesh.positions.iter().map(|p| p[0]).sum::<f32>() / 8.0;
    assert!(low.abs() < 1e-4 && mid_x.abs() < 1e-4);
    let mut wide = f.mesh.clone();
    for p in &mut wide.positions { *p = [p[0] * 2.0 + 7.0, p[1] + 8.0, p[2] - 9.0]; }
    let stood = img2mesh::restand(wide, 60.0).unwrap();
    assert_eq!(stood.mesh.triangles, f.mesh.triangles, "restand does not alter repaired topology");
    assert_eq!(stood.size, [60.0, 30.0, 30.0], "largest dimension, not height");
    assert_eq!(stood.mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min), 0.0);
    assert!(stood.watertight() && stood.shells == 1 && !stood.flipped);
    // a thin slab under it (the picture's table, made solid by the model) is left out
    let mut with_slab = mesh.clone();
    let slab = img2mesh::mesh_from_glb(&seamed_cube_glb(1.0, [0.0; 3])).unwrap();
    let base = with_slab.positions.len() as u32;
    with_slab.positions.extend(slab.positions.iter().map(|p| [p[0] * 6.0 + 3.0, p[1] * 0.01, p[2] * 6.0 - 2.0]));
    with_slab.triangles.extend(slab.triangles.iter().map(|t| t.map(|i| i + base)));
    let s = img2mesh::fit(&with_slab, 40.0).unwrap();
    assert_eq!((s.dropped_sheets, s.shells, s.mesh.positions.len()), (1, 1, 8), "{s:?}");
    assert!((s.size[0] - 40.0).abs() < 1e-3 && s.watertight(), "{s:?}");
    // inside out: turned
    let mut inverted = mesh.clone();
    inverted.triangles.iter_mut().for_each(|t| t.swap(1, 2));
    assert!(img2mesh::fit(&inverted, 40.0).unwrap().flipped);
    // an open box is not watertight
    let mut open = mesh;
    open.triangles.truncate(10);
    assert!(!img2mesh::fit(&open, 40.0).unwrap().watertight());
    assert!(img2mesh::mesh_from_glb(b"glTF but not really").is_err());
}

/// A figure (a box 20 x 20 x 60) on the generator's ground plate (100 x 100 x 1, fused: the
/// plate's top face is a ring of quads around the figure's foot), one closed mesh, Y up.
fn figure_on_plate(plate: bool) -> MeshData {
    let mut m = MeshData { positions: Vec::new(), triangles: Vec::new() };
    let v = |m: &mut MeshData, x: f32, y: f32, z: f32| {
        m.positions.push([x, y, z]);
        m.positions.len() as u32 - 1
    };
    let quad = |m: &mut MeshData, a: u32, b: u32, c: u32, d: u32| {
        m.triangles.push([a, b, c]);
        m.triangles.push([a, c, d]);
    };
    let (f0, f1, h) = (-10.0, 10.0, 60.0);
    let base = if plate { 1.0 } else { 0.0 };
    // the figure: walls from `base` to `h + base`, a top, and (no plate) a bottom
    let ring: Vec<[f32; 2]> = vec![[f0, f0], [f1, f0], [f1, f1], [f0, f1]];
    let lo: Vec<u32> = ring.iter().map(|p| v(&mut m, p[0], base, p[1])).collect();
    let hi: Vec<u32> = ring.iter().map(|p| v(&mut m, p[0], h + base, p[1])).collect();
    for k in 0..4 {
        let n = (k + 1) % 4;
        // outward: counter-clockwise seen from outside
        quad(&mut m, lo[n], lo[k], hi[k], hi[n]);
    }
    quad(&mut m, hi[3], hi[2], hi[1], hi[0]);
    if !plate {
        quad(&mut m, lo[0], lo[1], lo[2], lo[3]);
        return m;
    }
    // the plate: bottom, sides, and a top ring joining the figure's foot
    let corner = |m: &mut MeshData, y: f32| -> Vec<u32> { [[-50.0, -50.0], [50.0, -50.0], [50.0, 50.0], [-50.0, 50.0]].iter().map(|p| v(m, p[0], y, p[1])).collect() };
    let pb = corner(&mut m, 0.0);
    let pt = corner(&mut m, 1.0);
    quad(&mut m, pb[0], pb[1], pb[2], pb[3]);
    for k in 0..4 {
        let n = (k + 1) % 4;
        quad(&mut m, pb[n], pb[k], pt[k], pt[n]);
    }
    for k in 0..4 {
        let n = (k + 1) % 4;
        // top ring: outer edge pt[k]->pt[n], inner edge lo[n]->lo[k], facing up
        quad(&mut m, pt[k], lo[k], lo[n], pt[n]);
    }
    m
}

#[test]
fn a_fused_ground_plate_is_cut_away_and_the_figure_stands_on_its_own_foot() {
    let with = img2mesh::fit(&figure_on_plate(true), 60.0).unwrap();
    assert!(with.ground_plate, "{with:?}");
    assert!(with.watertight(), "{with:?}");
    // sized by the figure alone, standing at 0, no wider than the figure
    assert!((with.size[1] - 60.0).abs() < 1.0 || (with.size[0].max(with.size[1]).max(with.size[2]) - 60.0).abs() < 1e-3, "{:?}", with.size);
    let ys: Vec<f32> = with.mesh.positions.iter().map(|p| p[1]).collect();
    assert!(ys.iter().cloned().fold(f32::MAX, f32::min).abs() < 1e-4);
    let widest = with.mesh.positions.iter().map(|p| p[0].abs().max(p[2].abs())).fold(0.0, f32::max);
    assert!(widest < 12.0, "the plate is still there: half width {widest}");
    // a figure that has no plate is untouched
    let without = img2mesh::fit(&figure_on_plate(false), 60.0).unwrap();
    assert!(!without.ground_plate && without.watertight(), "{without:?}");
    assert_eq!(without.mesh.triangles.len(), 12);
}

#[test]
fn a_flat_sheet_from_the_hub_is_refused_in_words() {
    // The user's first result: 69.3 x 100 mm and under a millimetre thick.
    let mut mesh = img2mesh::mesh_from_glb(&seamed_cube_glb(1.0, [0.0; 3])).unwrap();
    for p in &mut mesh.positions {
        *p = [p[0] * 0.693, p[1], p[2] * 0.0086];
    }
    let f = img2mesh::fit(&mesh, 100.0).unwrap();
    let why = img2mesh::flat_sheet(&f).expect("a sheet");
    assert!(why.contains("flat sheet") && why.contains("100.0"), "{why}");
    // a solid cube is fine
    let cube = img2mesh::fit(&img2mesh::mesh_from_glb(&seamed_cube_glb(1.0, [0.0; 3])).unwrap(), 30.0).unwrap();
    assert!(img2mesh::flat_sheet(&cube).is_none());
}

#[test]
fn image_to_mesh_arguments_and_pictures_are_checked() {
    let r = img2mesh::parse_args(&args(r#"{"image":"fox.png","size_mm":80,"detail":512,"name":"Red Fox!","seed":7}"#)).unwrap();
    assert_eq!((r.size_mm, r.detail, r.name.as_deref(), r.seed), (80.0, 512, Some("red_fox"), Some(7)));
    let d = img2mesh::parse_args(&args(r#"{"image":"fox.png"}"#)).unwrap();
    assert!(d.printable);
    assert!(!img2mesh::parse_args(&args(r#"{"image":"fox.png","printable":false}"#)).unwrap().printable);
    assert!(img2mesh::parse_args(&args(r#"{"image":"fox.png","printable":"yes"}"#)).is_err());
    assert_eq!((d.size_mm, d.detail, d.name), (img2mesh::SIZE_DEFAULT_MM, img2mesh::DETAIL_DEFAULT, None));
    assert!(img2mesh::parse_args(&args(r#"{}"#)).unwrap_err().contains("image"));
    assert!(img2mesh::parse_args(&args(r#"{"image":"a","size_mm":1000}"#)).unwrap_err().contains("size_mm"));
    assert!(img2mesh::parse_args(&args(r#"{"image":"a","detail":20}"#)).unwrap_err().contains("detail"));
    assert_eq!(img2mesh::ident("2 Cats.png"), "m_2_cats_png");
    assert!(img2mesh::prepare_image(b"hello").unwrap_err().contains("not a picture Fab reads"));
    let limits = makepad_model::Limits::default();
    let tiny = makepad_model::RgbaImage { width: 8, height: 8, pixels: vec![200; 8 * 8 * 4] }.to_png(&limits).unwrap();
    assert!(img2mesh::prepare_image(&tiny).unwrap_err().contains("at least"));
    let big = makepad_model::RgbaImage { width: 2000, height: 1000, pixels: vec![128; 2000 * 1000 * 4] }.to_png(&makepad_model::Limits { max_texture_dimension: 4096, ..limits }).unwrap();
    let (png, size) = img2mesh::prepare_image(&big).unwrap();
    assert_eq!(size, [1024, 512]);
    assert!(png.starts_with(b"\x89PNG"));
}

#[test]
fn an_imported_mesh_is_an_object_script_on_the_plate_and_survives_a_crash() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.print_setup = Some(Arc::new(|ev| {
        let mut setup = weave::default_setup(&ev.machine, ev.plate.parts.len());
        setup.process.values.set_str("wall_loops", "8");
        setup.process.values.set_str("sparse_infill_density", "25.0%");
        setup.process.values.set_str("enable_support", "1");
        Ok((setup, Vec::new(), "figure process".into()))
    }));
    s.rebuild().unwrap();
    let f = img2mesh::fit(&img2mesh::mesh_from_glb(&seamed_cube_glb(1.0, [0.0; 3])).unwrap(), 30.0).unwrap();
    assert!(s.commit("profile", vec![Change::PlateSettings { set: vec![("enable_support".into(), "0".into()), ("wall_loops".into(), "3".into())], reset: Vec::new() }]).ok);
    let before_import = s.model.project_text();
    let from = img2mesh::Provenance { image: "fox.png".into(), model: img2mesh::MODEL.into() };
    let r = s.import_mesh("Fox", &f, &from, None, vec![("made_on", out::s("node"))]);
    assert!(r.ok, "{}", r.text);
    assert!(r.text.contains("watertight: true") && r.text.contains("made_on"), "{}", r.text);
    let Some(RenderRequest::Sheets { objects, .. }) = &r.render else { panic!("no render: {:?}", r.render) };
    assert_eq!(objects, &vec!["fox/fox".to_string()]);
    let src = s.source("fox.cad.splash").unwrap();
    assert!(src.contains("m.import(\"fox\"") && src.contains("param(30mm"), "{src}");
    let ev = s.last.clone().unwrap();
    let part = ev.plate.parts.iter().find(|p| p.object == "fox/fox").unwrap();
    let size = part.mesh.bounds().size();
    assert!((size[0] - 30.0).abs() < 0.1 && (size[2] - 30.0).abs() < 0.1, "{size:?}");
    assert!(part.mesh.bounds().min[2].abs() < 0.01, "on the bed: {:?}", part.mesh.bounds());
    let project = project::parse(&s.model.project_text()).unwrap();
    let place = project.places.iter().find(|p| p.instance == "fox").expect("the import has its own place line");
    let process = s.print_setup.as_ref().unwrap()(&ev).unwrap().0.process;
    for &(key, value) in img2mesh::PRINT_DEFAULTS {
        let inherited = project.settings.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
            .or_else(|| process.values.get(key).map(|v| v.at(0)))
            .is_some_and(|v| project::same_setting(v, value));
        assert!(place.settings.iter().any(|(k, v)| k == key && project::same_setting(v, value)) || inherited, "missing placement default {key}: {value}");
        assert!(part.print.iter().any(|(k, v)| k == key && project::same_setting(v, value)) || inherited, "missing part default {key}: {value}");
    }
    assert!(!place.settings.iter().any(|(k, _)| k == "wall_loops" || k == "sparse_infill_density"), "plate and process defaults need no placement override");
    assert!(place.settings.contains(&("enable_support".into(), "1".into())), "the plate takes precedence over the process");
    assert!(s.undo_redo(HistoryAction::Undo).ok);
    assert_eq!(s.model.project_text(), before_import, "one undo removes the instance and its settings");
    assert!(s.last.as_ref().unwrap().plate.parts.is_empty());
    assert!(s.undo_redo(HistoryAction::Redo).ok);
    // a second one gets its own name; Size scales it
    let r2 = s.import_mesh("fox", &f, &from, None, vec![]);
    assert!(r2.ok && r2.text.contains("fox_2"), "{}", r2.text);
    let h = head(&s);
    let r3 = call(&mut s, "place", &[("request_id", "p"), ("expected", &h), ("edits", "params(\"fox_2\", {size: 60mm})")]);
    assert!(r3.ok, "{}", r3.text);
    let ev = s.last.clone().unwrap();
    let big = ev.plate.parts.iter().find(|p| p.object == "fox_2/fox_2").unwrap().mesh.bounds().size();
    assert!((big[0] - 60.0).abs() < 0.2, "{big:?}");
    // the plate's working copy keeps the meshes
    let dir = std::env::temp_dir().join(format!("fab-img2mesh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = crate::plates::PlateStore::under(&dir.join("fab"), &dir.join("home"));
    s.store = Some(store.clone());
    s.autosave().unwrap();
    let mut back = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    back.store = Some(store);
    let ev = back.recover().unwrap().unwrap();
    assert_eq!(ev.plate.parts.len(), 2);
    assert!(back.model.assets.contains_key("fox") && back.model.assets.contains_key("fox_2"));
    let _ = std::fs::remove_dir_all(&dir);
    // the tool itself runs in the app
    assert!(!call(&mut s, img2mesh::TOOL, &[("image", "fox.png")]).ok);
}

/// One subdivided icosahedron with two small, separate holes.
fn holed_icosphere() -> makepad_model_script::lang::csg_sdf::mesh_field::MeshData {
    let phi = (1.0 + 5.0f32.sqrt()) * 0.5;
    let mut positions: Vec<[f32; 3]> = vec![
        [-1.0, phi, 0.0], [1.0, phi, 0.0], [-1.0, -phi, 0.0], [1.0, -phi, 0.0],
        [0.0, -1.0, phi], [0.0, 1.0, phi], [0.0, -1.0, -phi], [0.0, 1.0, -phi],
        [phi, 0.0, -1.0], [phi, 0.0, 1.0], [-phi, 0.0, -1.0], [-phi, 0.0, 1.0],
    ];
    let faces: [[u32; 3]; 20] = [
        [0,11,5], [0,5,1], [0,1,7], [0,7,10], [0,10,11], [1,5,9], [5,11,4], [11,10,2], [10,7,6], [7,1,8],
        [3,9,4], [3,4,2], [3,2,6], [3,6,8], [3,8,9], [4,9,5], [2,4,11], [6,2,10], [8,6,7], [9,8,1],
    ];
    let unit = |p: [f32; 3]| { let len = p.iter().map(|x| x*x).sum::<f32>().sqrt(); p.map(|v| v / len) };
    for p in &mut positions { *p = unit(*p); }
    let mut edges = std::collections::HashMap::new();
    let mut triangles = Vec::new();
    for [a, b, c] in faces {
        let mut midpoint = |a: u32, b: u32| *edges.entry((a.min(b), a.max(b))).or_insert_with(|| {
            let point = unit(std::array::from_fn(|i| positions[a as usize][i] + positions[b as usize][i]));
            let index = positions.len() as u32;
            positions.push(point);
            index
        });
        let (ab, bc, ca) = (midpoint(a, b), midpoint(b, c), midpoint(c, a));
        triangles.extend([[a,ab,ca], [b,bc,ab], [c,ca,bc], [ab,bc,ca]]);
    }
    triangles.remove(31);
    triangles.remove(7);
    makepad_model_script::lang::csg_sdf::mesh_field::MeshData { positions, triangles }
}

#[test]
fn make_printable_replaces_the_asset_at_current_size_and_undo_restores_it() {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let fitted = img2mesh::fit(&holed_icosphere(), 12.0).unwrap();
    assert!(fitted.open_edges > 0);
    let from = img2mesh::Provenance { image: "figure.png".into(), model: img2mesh::MODEL.into() };
    let imported = session.import_mesh("figure", &fitted, &from, None, vec![]);
    assert!(imported.ok, "{}", imported.text);
    let placed = session.commit("size", vec![
        Change::Param { instance: "figure".into(), name: "size".into(), value: Some(0.024) },
        Change::Place { instance: "figure".into(), object: "figure".into(), arg: PlaceArg::Scale([1.5; 3]) },
        Change::Place { instance: "figure".into(), object: "figure".into(), arg: PlaceArg::Setting { key: "wall_loops".into(), value: Some("9".into()) } },
        Change::Place { instance: "figure".into(), object: "figure".into(), arg: PlaceArg::Setting { key: "support_threshold_angle".into(), value: Some("42".into()) } },
    ]);
    assert!(placed.ok, "{}", placed.text);
    assert!(printable_tool::is_imported_mesh(session.compiled.as_ref().unwrap(), "figure/figure"));
    let original = session.model.assets["figure"].clone();
    let source = session.source("figure.cad.splash").unwrap();
    let project = session.model.project_text();
    let reply = session.call("make_printable", &args(r#"{"object":"figure/figure","cell_mm":0.6,"orient":false,"flat_base":false}"#), None);
    assert!(reply.ok, "{}", reply.text);
    assert_eq!(session.source("figure.cad.splash").unwrap(), source.replace("m.import(\"figure\"", "m.import(\"figure_printable\""));
    assert!(Arc::ptr_eq(&original, &session.model.assets["figure"]), "the original asset stays immutable");
    let repaired = &session.model.assets["figure_printable"].data;
    let stats = printable::stats(repaired);
    assert_eq!(stats.open_edges, 0);
    assert_eq!(stats.nonmanifold_edges, 0);
    let parsed = makepad_script::syntax::parse_value(&reply.text).unwrap();
    let report = &parsed.field("printable").unwrap().value;
    let before_volume = report.field("before").unwrap().value.field("volume_mm3").unwrap().value.number().unwrap();
    let expected = printable::stats(&fitted.mesh).volume_mm3 * 27.0;
    assert!((before_volume - expected).abs() < expected.abs() * 1e-4, "repair uses Size and placement scale: {before_volume} != {expected}");
    let after_volume = report.field("after").unwrap().value.field("volume_mm3").unwrap().value.number().unwrap();
    assert!((stats.volume_mm3 * 27.0 - after_volume).abs() < after_volume.abs() * 1e-4, "scale is not applied twice");
    let project_after = project::parse(&session.model.project_text()).unwrap();
    let settings = &project_after.places.iter().find(|p| p.instance == "figure").unwrap().settings;
    assert!(settings.contains(&("support_threshold_angle".into(), "42".into())), "unrelated overrides survive");
    assert!(settings.contains(&("wall_loops".into(), "3".into())));
    let check = call(&mut session, "check_mesh", &[("object", "figure/figure")]);
    assert!(check.ok && check.text.contains("closed: true"), "{}", check.text);
    let ev = session.last.as_ref().unwrap();
    let report = fab_printability::analyse_plate(&ev.machine, &ev.plate, &session.eval.materials);
    assert!(!report.issues.iter().any(|i| i.kind == "open_edges" || i.kind == "nonmanifold_edges"));
    assert!(session.undo_redo(HistoryAction::Undo).ok);
    assert_eq!(session.source("figure.cad.splash").unwrap(), source);
    assert_eq!(session.model.project_text(), project);
    assert!(session.undo_redo(HistoryAction::Redo).ok);
    assert!(session.source("figure.cad.splash").unwrap().contains("m.import(\"figure_printable\""));
}

#[test]
fn open_mesh_printability_offers_make_printable_and_import_keeps_repair_errors() {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let fitted = img2mesh::fit(&holed_icosphere(), 12.0).unwrap();
    let from = img2mesh::Provenance { image: "figure.png".into(), model: img2mesh::MODEL.into() };
    let repair = Err("repair could not close this mesh".to_string());
    let reply = session.import_mesh("figure", &fitted, &from, Some(&repair), vec![]);
    assert!(reply.ok, "{}", reply.text);
    let parsed = makepad_script::syntax::parse_value(&reply.text).unwrap();
    assert_eq!(parsed.field("printable").unwrap().value.field("error").unwrap().value.string(), Some("repair could not close this mesh"));
    assert_eq!(session.model.assets["figure"].data, fitted.mesh);
    let ev = session.last.as_ref().unwrap();
    let report = fab_printability::analyse_plate(&ev.machine, &ev.plate, &session.eval.materials);
    let issue = report.issues.iter().find(|i| i.kind == "open_edges").expect("open mesh is reported");
    assert_eq!(issue.object.as_deref(), Some("figure/figure"));
    assert_eq!(issue.fix, Some("make_printable"));
}

/// The guide's bolt and nut: sizes from the table, clean ends, and a fit check that says what a
/// clearance will do.
#[test]
fn the_guides_bolt_and_nut_builds_clean_and_checks_its_fit() {
    let section = guide_section(Some("threads")).unwrap();
    let block = section.split("```splash\n").nth(1).and_then(|b| b.split("```").next()).expect("a script in the section");
    let source = format!("{PRELUDE}{block}");
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", "bolts.cad.splash"), ("source", &source)]);

    // M8 from the table: a 13 mm hex, 5.3 + 25 mm long, the nut 6.8 mm high.
    let size = tool_ok(&mut s, "measure", &[("what", "size"), ("object", "bolts/nut")]).text;
    assert!(size.contains("6.8"), "{size}");
    let size = tool_ok(&mut s, "measure", &[("what", "size"), ("object", "bolts/bolt")]).text;
    assert!(size.contains("30.3"), "{size}");

    // The thread ends leave no slivers.
    for object in ["bolts/bolt", "bolts/nut"] {
        let check = tool_ok(&mut s, "check_mesh", &[("object", object)]).text;
        assert!(check.contains("closed: true"), "{object}: {check}");
        let findings: Vec<&str> = check.lines().filter(|l| l.contains("message") || l.contains(" at:")).collect();
        assert!(!check.contains("thin_feature") && !check.contains("speck"), "{object}: {findings:#?}");
    }

    let check = tool_ok(&mut s, "check", &[]).text;
    assert!(check.replace(char::is_whitespace, "").contains("errors:0"), "{check}");

    let fit = tool_ok(&mut s, "measure", &[("what", "thread_fit"), ("a", "bolts/bolt"), ("b", "bolts/nut")]).text;
    assert!(fit.contains("verdict: \"fits\"") && fit.contains("untested"), "0.3 mm on an M8: {fit}");
    let h = head(&s);
    let tighter = source.replace("param(0.3mm", "param(0.2mm");
    tool_ok(&mut s, "script", &[("request_id", "b"), ("expected", &h), ("script", "bolts.cad.splash"), ("source", &tighter)]);
    let fit = tool_ok(&mut s, "measure", &[("what", "thread_fit"), ("a", "bolts/nut"), ("b", "bolts/bolt")]).text;
    assert!(fit.contains("verdict: \"tight\""), "0.2 mm on an M8: {fit}");

    // A size the table does not have says which it does.
    let h = head(&s);
    let odd = source.replace("param(8mm", "param(7mm");
    let r = call(&mut s, "script", &[("request_id", "c"), ("expected", &h), ("script", "bolts.cad.splash"), ("source", &odd)]);
    assert!(!r.ok && r.text.contains("M8"), "{}", r.text);
}

fn mutation(s: &mut Session, tool: &str, id: &str, fields: &str) -> Reply {
    let body = format!("{{\"request_id\":{},\"expected\":{},{fields}}}", quoted(id), quoted(&head(s)));
    s.call(tool, &args(&body), None)
}

#[test]
fn duplicate_tools_link_copy_and_inspect_script_ownership() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.model = FabModel::with_script("brackets", "bracket.cad.splash", include_str!("../../app/resources/bracket.cad.splash"));
    s.rebuild().unwrap();
    let before = s.last.clone().unwrap();
    let b = before.plate.parts[0].bounds();
    let wanted = free_spot(&before, [b.size()[0], b.size()[1]], b.size()[2], &before.nozzles_for(&before.part_colours(0))).unwrap();
    let r = mutation(&mut s, "duplicate", "linked", r#""object":"bracket/bracket","mode":"linked""#);
    assert!(r.ok, "{}", r.text);
    assert!(r.text.contains("bracket~1/bracket"), "{}", r.text);
    assert_eq!(s.model.project().place_link("bracket", "bracket", 1).unwrap().at, Some(wanted));
    let c = s.compiled.as_ref().unwrap();
    assert!(Arc::ptr_eq(&c.instance("bracket").unwrap().script, &c.instance("bracket~1").unwrap().script));
    let r = mutation(&mut s, "duplicate", "copy", r#""object":"bracket~1/bracket","mode":"copy","at":[100,240]"#);
    assert!(r.ok && r.text.contains("bracket_2/bracket"), "{}", r.text);
    assert_eq!(s.model.project().place("bracket_2", "bracket").unwrap().at, Some([100.0, 240.0]));
    assert_eq!(s.source("bracket.cad.splash"), s.source("bracket_2.cad.splash"));
    let c = s.compiled.as_ref().unwrap();
    assert!(!Arc::ptr_eq(&c.instance("bracket").unwrap().script, &c.instance("bracket_2").unwrap().script));
    let r = s.call("inspect", &args(r#"{"program":false}"#), None);
    assert!(r.ok, "{}", r.text);
    let inspected = makepad_script::syntax::parse_value(&r.text).unwrap();
    let scripts = inspected.field("scripts").unwrap().value.items().unwrap();
    assert_eq!(scripts.len(), 2);
    let bracket = scripts.iter().find(|script| script.field("file").unwrap().value.string() == Some("bracket.cad.splash")).unwrap();
    let instances = bracket.field("instances").unwrap().value.items().unwrap();
    assert_eq!(instances[0].field("base").unwrap().value.string(), Some("bracket"));
    assert_eq!(instances[0].field("links").unwrap().value.items().unwrap()[0].string(), Some("bracket~1"));
    assert!(inspected.field("measures").unwrap().value.items().unwrap().is_empty());
    assert!(manifest().tool("duplicate").is_some() && manifest().tool("remove").is_some());
    // Preview creates neither a link nor a file and leaves history untouched.
    let head = s.model.head();
    let r = mutation(&mut s, "duplicate", "preview", r#""object":"bracket/bracket","mode":"copy","at":[100,160],"dry_run":true"#);
    assert!(r.ok, "{}", r.text);
    assert_eq!(s.model.head(), head);
    assert!(s.source("bracket_3.cad.splash").is_none());
    let r = mutation(&mut s, "remove", "remove-link", r#""object":"bracket~1/bracket""#);
    assert!(r.ok, "{}", r.text);
    assert!(s.compiled.as_ref().unwrap().instance("bracket").is_some());
    assert!(s.compiled.as_ref().unwrap().instance("bracket~1").is_none());
    let r = mutation(&mut s, "duplicate", "relink", r#""object":"bracket/bracket","mode":"linked","at":[240,100]"#);
    assert!(r.ok, "{}", r.text);
    let r = mutation(&mut s, "remove", "remove-script", r#""object":"bracket","scope":"script""#);
    assert!(r.ok, "{}", r.text);
    assert!(s.source("bracket.cad.splash").is_none());
    assert!(s.source("bracket_2.cad.splash").is_some());
    assert_eq!(s.compiled.as_ref().unwrap().instances.len(), 1);
}

#[test]
fn ruler_tools_add_set_remove_inspect_and_undo_preserve_geometric_measurements() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let r = mutation(&mut s, "measure", "add", r#""action":"add","points":[[10,20,0],[30,20,0]],"name":"Gap A""#);
    assert!(r.ok, "{}", r.text);
    assert_eq!(s.model.project().measures[0].kind, "distance");
    let r = s.call("inspect", &args(r#"{"program":false}"#), None);
    assert!(r.ok, "{}", r.text);
    let inspected = makepad_script::syntax::parse_value(&r.text).unwrap();
    let measures = inspected.field("measures").unwrap().value.items().unwrap();
    assert_eq!(measures[0].field("index").unwrap().value.number(), Some(0.0));
    assert_eq!(measures[0].field("name").unwrap().value.string(), Some("Gap A"));
    assert_eq!(measures[0].field("kind").unwrap().value.name(), Some("distance"));
    assert!(r.text.contains("10mm") && r.text.contains("30mm"), "{}", r.text);
    let r = mutation(&mut s, "measure", "set", r#""action":"set","index":0,"points":[[10,20,0],[40,20,0]],"name":null"#);
    assert!(r.ok, "{}", r.text);
    assert_eq!(s.model.project().measures[0].name, None);
    assert_eq!(s.model.project().measures[0].points[1], [40.0, 20.0, 0.0]);
    let r = mutation(&mut s, "measure", "remove", r#""action":"remove","index":0"#);
    assert!(r.ok && s.model.project().measures.is_empty(), "{}", r.text);
    let h = head(&s);
    let r = call(&mut s, "history", &[("expected", &h), ("action", "undo")]);
    assert!(r.ok, "{}", r.text);
    assert_eq!(s.model.project().measures[0].points[1], [40.0, 20.0, 0.0]);
    // The pre-existing read-only measurement mode still works without a transaction envelope.
    let before = s.model.head();
    let r = s.call("measure", &args(r#"{"what":"distance","a":[0,0,0],"b":[3,4,0]}"#), None);
    assert!(r.ok, "{}", r.text);
    assert_eq!(s.model.head(), before);
    let r = mutation(&mut s, "measure", "invalid", r#""action":"set","index":-1,"name":"bad""#);
    assert!(!r.ok);
    assert_eq!(s.model.head(), before);
}

#[test]
fn inspect_and_place_keep_excluded_clones_editable() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.model = FabModel::with_script("eyes", "body.cad.splash", &format!("{}let body = m.box({{size: [20mm 12mm 8mm]}})\nobject(body)\n", fab_model::program::EMPTY));
    s.rebuild().unwrap();
    let r = s.commit("clone", vec![Change::Clone { instance: "body".into(), at: Some([240.0, 160.0]) }]);
    assert!(r.ok, "{}", r.text);
    let h = head(&s);
    let r = call(&mut s, "place", &[("request_id", "off"), ("expected", &h), ("edits", "place(\"body\", link: 1, print: false)")]);
    assert!(r.ok, "{}", r.text);
    let inspect = s.call("inspect", &args("{\"program\":false}"), None);
    assert!(inspect.ok && inspect.text.contains("print: false") && inspect.text.contains("print: true"), "{}", inspect.text);
    let old = s.last.as_ref().unwrap().plate.excluded[0].part.bounds();
    let c = old.center();
    let delta = [[0.0, -1.0, 0.0, c[0] + c[1] + 10.0], [1.0, 0.0, 0.0, c[1] - c[0]], [0.0, 0.0, 1.0, 0.0]];
    let r = s.place_parts(&[PartMove { object: "body~1/body".into(), delta }]);
    assert!(r.ok, "{}", r.text);
    let ev = s.last.as_ref().unwrap();
    assert_eq!(ev.plate.parts.len(), 1);
    let moved = ev.plate.excluded[0].part.bounds();
    assert!((moved.center()[0] - c[0] - 10.0).abs() < 1e-3);
    assert!((moved.size()[0] - old.size()[1]).abs() < 1e-3);
    assert!(!s.model.project().place_link("body", "body", 1).unwrap().print);
}

/// A closed cube as an imported mesh asset.
fn cube_mesh() -> makepad_csg_sdf::mesh_field::MeshData {
    let positions: Vec<[f32; 3]> = (0..8).map(|i| [(i & 1) as f32 * 20.0, (i >> 1 & 1) as f32 * 20.0, (i >> 2 & 1) as f32 * 20.0]).collect();
    let triangles = vec![
        [0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4],
        [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5],
    ];
    makepad_csg_sdf::mesh_field::MeshData { positions, triangles }
}

/// The working copy carries the picture, and a relaunch brings it back; emptied, the plate
/// drops it at the next autosave. An image-to-mesh result is kept as a saved plate of its own.
#[test]
fn the_working_copy_keeps_the_viewport_picture_across_a_relaunch() {
    let root = std::env::temp_dir().join(format!("fab-thumb-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = crate::plates::PlateStore::under(&root.join("fab"), &root.join("documents"));
    let mut s = Session::new(Machine::dual_nozzle_350(), root.join("documents"));
    s.store = Some(store.clone());
    s.rebuild().unwrap();
    s.import_mesh("cube", &img2mesh::fit(&cube_mesh(), 40.0).unwrap(), &img2mesh::Provenance { image: "p.png".into(), model: img2mesh::MODEL.into() }, None, vec![]);
    let image = makepad_model::RgbaImage { width: 2, height: 2, pixels: vec![40; 16] };
    s.set_thumbnail(Arc::from(image.to_png(&makepad_model::Limits::default()).unwrap()));
    s.autosave().unwrap();
    let id = s.plate.clone().unwrap();
    assert!(store.picture(&id).is_file());
    // the image-to-mesh result is a plate of its own too, saved in the plates home
    let made: Vec<_> = store.list().into_iter().filter(|p| p.origin == crate::plates::Origin::Made).collect();
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].file.as_ref().is_some_and(|f| f.is_file() && f.starts_with(root.join("documents"))), "{made:?}");
    let mut t = Session::new(Machine::dual_nozzle_350(), root.join("documents"));
    t.store = Some(store.clone());
    assert!(t.recover().unwrap().is_ok());
    assert!(t.thumb.is_some());
    // emptied: its picture goes at the next autosave
    let r = t.commit("clear", vec![Change::RemoveScript { instance: "cube".into() }]);
    assert!(r.ok, "{}", r.text);
    t.autosave().unwrap();
    assert!(!store.picture(&id).exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// The viewport's frame becomes a small opaque PNG.
#[test]
fn a_viewport_frame_becomes_a_small_opaque_picture() {
    let (w, h) = (1280u32, 640u32);
    let frame: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, 40, 90, 0]).collect();
    let png = crate::plates::thumbnail_png(w, h, &frame).unwrap();
    let back = makepad_model::RgbaImage::from_png(&png, &makepad_model::Limits::default()).unwrap();
    assert_eq!((back.width, back.height), (320, 160));
    assert!(back.pixels.chunks_exact(4).all(|p| p[3] == 255));
    assert!(back.pixels[0] < 8 && back.pixels[(319 * 4) as usize] > 247, "the picture keeps the frame's left to right ramp");
    assert!(crate::plates::thumbnail_png(w, h, &frame[1..]).is_err());
}

/// A session on the dual-nozzle printer with four print colours and `tiles` 100 mm colour
/// tiles of a picture in those four filaments (fab.picture); the filaments alternate between
/// the nozzles, so each tile prints with both.
fn colour_tiles(tag: &str, tiles: usize) -> Session {
    let dir = std::env::temp_dir().join(format!("fab-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (16, 8);
    let image = fab_colour::Image { width: w, height: h, pixels: (0..w * h).map(|i| [(i % w) as f32 / w as f32, 0.3, 0.7]).collect() };
    let path = dir.join("ramp.png");
    std::fs::write(&path, image.png().unwrap()).unwrap();
    let mut s = Session::new(Machine::dual_nozzle_350(), dir.clone());
    s.rebuild().unwrap();
    let colour = |name: &str, rgb: u32| Change::Color { name: name.into(), set: Some(fab_model::project::ColorDef { rgb: Some(rgb), ..fab_model::project::ColorDef::new(name) }) };
    let r = s.commit("colours", vec![colour("Cyan", 0x00a0e0ff), colour("Magenta", 0xe0208cff), colour("Yellow", 0xf8e000ff), colour("White", 0xf4f4f0ff)]);
    assert!(r.ok, "{}", r.text);
    for _ in 0..tiles {
        let r = call(&mut s, "picture", &[("image", path.to_str().unwrap())]);
        assert!(r.ok, "{}", r.text);
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    s
}

/// The tile parts of an evaluated plate.
fn tile_parts(ev: &fab_model::eval::Evaluated) -> Vec<usize> {
    (0..ev.plate.parts.len()).filter(|&i| ev.plate.parts[i].object.starts_with("ramp")).collect()
}

/// Three 100 mm colour tiles of a picture in four filaments on the dual-nozzle printer: the
/// filaments alternate between the nozzles, so each tile needs both, and each lands where both
/// reach (one row of three would put the third past the left nozzle's reach). The job maps
/// them with no part outside its nozzle's reach.
#[test]
fn three_colour_tiles_land_where_both_nozzles_reach() {
    let s = colour_tiles("tiles", 3);
    let ev = s.last.clone().unwrap();
    assert!(!ev.warnings.iter().any(|w| w.contains("no room")), "{:?}", ev.warnings);
    let tiles = tile_parts(&ev);
    assert_eq!(tiles.len(), 3, "{:?}", ev.plate.parts.iter().map(|p| &p.object).collect::<Vec<_>>());
    for &i in &tiles {
        let b = ev.plate.parts[i].bounds();
        assert_eq!(ev.nozzles_for(&ev.part_colours(i)), vec![0, 1], "{:?}", ev.part_colours(i));
        assert_eq!(ev.machine.nozzles_reaching(&b), vec![0, 1], "{} at {:?}", ev.plate.parts[i].object, b);
        // the plate's check tests the nozzles the layout placed it for
        assert_eq!(ev.plate.parts[i].nozzles(), vec![0, 1], "{:?}", ev.plate.parts[i].paint.as_ref().map(|p| &p.colors));
    }
    let tools = fab_model::eval::job_tools(&ev.mapped, &ev.colors, &ev.plate.parts, &ev.part_colors);
    let mut plate = ev.plate.clone();
    plate.map_colors(&ev.part_colors, &|name| tools.get(name).copied());
    let reports = fab_plate::machine::check(&ev.machine, &plate);
    assert!(!reports.iter().flat_map(|r| &r.issues).any(|i| matches!(i, fab_plate::machine::Issue::OutOfReach { .. })), "{reports:?}");
}

/// A colour tile placed where the shelf pack once put the first of three (X 17 to 117): its own
/// colour's nozzle (the left) reaches all of it, the right one its picture also prints with
/// does not, and the plate's check says so.
#[test]
fn a_tile_where_one_of_its_nozzles_does_not_reach_is_flagged() {
    let mut s = colour_tiles("tile-x17", 1);
    let instance = s.model.project().scripts.iter().find(|i| i.name.starts_with("ramp")).unwrap().name.clone();
    let h = head(&s);
    let r = call(&mut s, "place", &[("request_id", "x17"), ("expected", &h), ("edits", &format!("place(\"{instance}\", at: [67mm 160mm])"))]);
    assert!(r.ok, "{}", r.text);
    let ev = s.last.clone().unwrap();
    let i = tile_parts(&ev)[0];
    let part = &ev.plate.parts[i];
    let b = part.bounds();
    assert!((b.min[0] - 17.0).abs() < 1e-6 && (b.max[0] - 117.0).abs() < 1e-6, "{b:?}");
    assert_eq!(part.nozzle, Some(0), "its own colour prints on the left nozzle, which reaches it");
    assert_eq!(part.nozzles(), vec![0, 1]);
    let issues = &ev.reports.iter().find(|r| r.part == i).unwrap().issues;
    assert_eq!(issues, &vec![fab_plate::machine::Issue::OutOfReach { nozzle: 1 }]);
}

/// A photo placed with fab.picture lands as a tile part: the picture in the project, the part
/// the width asked (100 mm by default), as deep as the picture's aspect, four 0.16 mm layers
/// thick, printing at that layer height.
#[test]
fn a_photo_lands_as_a_colour_tile() {
    let dir = std::env::temp_dir().join(format!("fab-picture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (16, 8);
    let image = fab_colour::Image { width: w, height: h, pixels: (0..w * h).map(|i| if i % w < w / 2 { [0.8, 0.1, 0.1] } else { [0.1, 0.2, 0.8] }).collect() };
    let path = dir.join("halves.png");
    std::fs::write(&path, image.png().unwrap()).unwrap();
    let mut s = Session::new(Machine::dual_nozzle_350(), dir.clone());
    s.rebuild().unwrap();
    let r = call(&mut s, "picture", &[("image", path.to_str().unwrap())]);
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    assert!(r.ok, "{}", r.text);
    assert!(s.model.pictures.contains_key("halves.png"), "{:?}", s.model.pictures.keys().collect::<Vec<_>>());
    let ev = s.last.clone().unwrap();
    let part = ev.plate.parts.iter().find(|p| p.object.starts_with("halves")).unwrap_or_else(|| panic!("{:?}", ev.plate.parts.iter().map(|p| &p.object).collect::<Vec<_>>()));
    let lo = part.mesh.positions.iter().fold([f64::MAX; 3], |a, p| [a[0].min(p[0]), a[1].min(p[1]), a[2].min(p[2])]);
    let hi = part.mesh.positions.iter().fold([f64::MIN; 3], |a, p| [a[0].max(p[0]), a[1].max(p[1]), a[2].max(p[2])]);
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    assert!((size[0] - 100.0).abs() < 1e-6 && (size[1] - 50.0).abs() < 1e-6 && (size[2] - 0.64).abs() < 1e-6, "{size:?}");
    assert!(part.print.iter().any(|(k, v)| k == "layer_height" && v == "0.16"), "{:?}", part.print);
}

/// A session with the plate's colour swatch of four filaments, and a photo of it (sRGB RGB
/// bytes, its width and height) as other filaments than the first guess print: those, and the
/// guess.
fn swatch_photo(docs: &Path) -> (Session, (Vec<u8>, usize, usize), fab_colour::FilamentSet, fab_colour::FilamentSet) {
    let size = fab_model::picture::swatch_size(4);
    let src = format!("{}let cyan = Material{{albedo: #00a0e0 name: \"Cyan\"}}\nlet magenta = Material{{albedo: #e0007a name: \"Magenta\"}}\nlet yellow = Material{{albedo: #ffe000 name: \"Yellow\"}}\nlet white = Material{{albedo: #f2f2f2 name: \"White\"}}\nlet swatch = m.colour_swatch({{size: [{}mm {}mm] filaments: [cyan magenta yellow white]}})\nobject(swatch)\n", fab_model::program::EMPTY, size[0], size[1]);
    let mut s = Session::new(Machine::dual_nozzle_350(), docs.to_path_buf());
    s.model = FabModel::with_script("swatch", "swatch.cad.splash", &src);
    s.rebuild().unwrap();
    // the swatch as its filaments really print: a little off the guess, scattering less
    let compiled = s.compiled.clone().unwrap();
    let spec = fab_model::picture::specs(&compiled.instances[0].script)[0];
    let guess = fab_model::picture::set_of(spec, &[], &Default::default()).unwrap();
    let mut truth = guess.clone();
    for (f, c) in truth.filaments.iter_mut().zip([[0, 140, 200], [205, 20, 120], [250, 205, 30], [236, 236, 228]]) {
        *f = fab_colour::Filament { scatter: [1.8; 3], ..fab_colour::Filament::guess(&f.name, c) };
    }
    let (_, tile) = fab_model::picture::grid(spec, &truth, s.eval.profile.nozzle);
    // photographed square on, 10 px a mm, on paper
    let [nx, ny] = tile.size;
    let (px, margin) = (10.0, 12.0);
    let (w, h) = (((nx as f64 * tile.cell + 2.0 * margin) * px) as usize, ((ny as f64 * tile.cell + 2.0 * margin) * px) as usize);
    let pixels = (0..w * h)
        .map(|i| {
            let q = [(i % w) as f64 / px - margin, (i / w) as f64 / px - margin];
            let (cu, cv) = ((q[0] / tile.cell).floor(), (q[1] / tile.cell).floor());
            if cu >= 0.0 && cv >= 0.0 && (cu as usize) < nx && (cv as usize) < ny { tile.shown[(ny - 1 - cv as usize) * nx + nx - 1 - cu as usize].map(|v| v * 0.9) } else { [fab_colour::read::PAPER * 0.9; 3] }
        })
        .collect::<Vec<[f32; 3]>>();
    let rgb = pixels.iter().flat_map(|p| fab_colour::oklab::srgb8_of_linear(*p)).collect();
    (s, (rgb, w, h), truth, guess)
}

/// Each stack's worst difference from what it truly shows: as `kept` read it, as first guessed.
fn swatch_read_off(kept: &fab_colour::FilamentSet, truth: &fab_colour::FilamentSet, guess: &fab_colour::FilamentSet) -> (f32, f32) {
    let worst = |shown: &dyn Fn(&fab_colour::palette::Measured) -> [f32; 3]| kept.measured.iter().map(|m| fab_colour::oklab::delta_e(fab_colour::oklab::oklab(shown(m)), fab_colour::oklab::oklab(truth.colour_of(&m.stack, m.background).0))).fold(0.0f32, f32::max);
    (worst(&|m| m.colour), worst(&|m| guess.colour_of(&m.stack, m.background).0))
}

/// A photo of the plate's colour swatch, read back, is a calibrated set kept under its name: its
/// stacks as the photo shows them (here as other filaments than the first guess print them), and
/// a new session finds it.
#[test]
fn a_swatch_photo_is_kept_as_a_calibrated_set() {
    let docs = std::env::temp_dir().join(format!("fab-colour-read-{}", std::process::id()));
    std::fs::create_dir_all(&docs).unwrap();
    let (mut s, (rgb, w, h), truth, guess) = swatch_photo(&docs);
    let photo = docs.join("swatch.png");
    std::fs::write(&photo, makepad_model::Texture { width: w as u32, height: h as u32, rgb }.to_png(&makepad_model::Limits::default()).unwrap()).unwrap();
    let r = call(&mut s, "colour_read", &[("image", photo.to_str().unwrap()), ("name", "Test CMYW")]);
    assert!(r.ok, "{}", r.text);
    assert!(r.text.contains("set: \"Test CMYW\"") && r.text.contains("stacks: "), "{}", r.text);
    let kept = Session::new(Machine::dual_nozzle_350(), docs.clone()).eval.calibrated.get("Test CMYW").cloned().expect("a new session finds the set");
    std::fs::remove_file(&photo).unwrap();
    std::fs::remove_file(fab_model::colour_sets::path(&docs, "Test CMYW").unwrap()).unwrap();
    std::fs::remove_dir(docs.join("calibration")).unwrap();
    std::fs::remove_dir(&docs).unwrap();
    assert_eq!(kept.filaments, guess.filaments);
    let (read_off, guess_off) = swatch_read_off(&kept, &truth, &guess);
    assert!(kept.measured.len() >= 16 && read_off < 0.03 && guess_off > 2.0 * read_off, "{} stacks, read {read_off:.4} off at worst, the guess {guess_off:.4}", kept.measured.len());
}

/// `fab.colour_read` reads every picture the chat takes, through the one decoder the chat uses:
/// the swatch photo as a WebP and as a GIF (lossless here: the photo's own colours) reads as
/// the PNG does.
#[test]
fn a_swatch_photo_reads_as_a_webp_and_as_a_gif() {
    let docs = std::env::temp_dir().join(format!("fab-colour-formats-{}", std::process::id()));
    std::fs::create_dir_all(&docs).unwrap();
    let (mut s, (rgb, w, h), truth, guess) = swatch_photo(&docs);
    let mut webp = Vec::new();
    makepad_webp::WebPEncoder::new(&mut webp).encode(&rgb, w as u32, h as u32, makepad_webp::ColorType::Rgb8).unwrap();
    let mut palette: Vec<u8> = Vec::new();
    let mut index: std::collections::HashMap<[u8; 3], u8> = std::collections::HashMap::new();
    let indices: Vec<u8> = rgb
        .chunks_exact(3)
        .map(|p| {
            let colour = [p[0], p[1], p[2]];
            *index.entry(colour).or_insert_with(|| {
                palette.extend_from_slice(&colour);
                (palette.len() / 3 - 1) as u8
            })
        })
        .collect();
    assert!(palette.len() <= 256 * 3, "{} colours: more than a GIF holds", palette.len() / 3);
    let mut gif = Vec::new();
    {
        let mut encoder = makepad_gif::Encoder::new(&mut gif, w as u16, h as u16, &palette).unwrap();
        encoder.write_frame(&makepad_gif::Frame { width: w as u16, height: h as u16, buffer: indices.into(), ..Default::default() }).unwrap();
    }
    for (file, bytes, name) in [("swatch.webp", webp, "WebP CMYW"), ("swatch.gif", gif, "GIF CMYW")] {
        let photo = docs.join(file);
        std::fs::write(&photo, bytes).unwrap();
        let r = call(&mut s, "colour_read", &[("image", photo.to_str().unwrap()), ("name", name)]);
        assert!(r.ok, "{file}: {}", r.text);
        let kept = Session::new(Machine::dual_nozzle_350(), docs.clone()).eval.calibrated.get(name).cloned().expect("a new session finds the set");
        let (read_off, guess_off) = swatch_read_off(&kept, &truth, &guess);
        assert!(kept.measured.len() >= 16 && read_off < 0.03 && guess_off > 2.0 * read_off, "{file}: {} stacks, read {read_off:.4} off at worst, the guess {guess_off:.4}", kept.measured.len());
        std::fs::remove_file(&photo).unwrap();
        std::fs::remove_file(fab_model::colour_sets::path(&docs, name).unwrap()).unwrap();
    }
    std::fs::remove_dir(docs.join("calibration")).unwrap();
    std::fs::remove_dir(&docs).unwrap();
}

/// What a thread does as it polls while it waits (a preview's check of a newer value) runs on a
/// CPU permit of its own, its own token lent meanwhile, whether the thread waited on a lent token
/// or not: the budget counts the check once, and the thread's token is its own again after.
#[test]
fn a_check_while_waiting_runs_on_a_permit_of_its_own() {
    use makepad_cpu_budget::{admit, lend, release, take_back, tokens, Tokens, APP, INTERACTIVE};
    let holder = |t: &Tokens, label: &str| t.holders.iter().find(|h| h.label == label).map(|h| h.lent);
    let job = admit(INTERACTIVE, APP, "fab test preview");
    for waiting in [true, false] {
        let lent = waiting && lend();
        assert_eq!(lent, waiting, "the test job's token was not lent");
        let during = on_own_permit("fab test check", tokens);
        if lent {
            take_back();
        }
        assert_eq!(holder(&during, "fab test check"), Some(false), "the check ran on no permit of its own (waiting {waiting})");
        assert_eq!(holder(&during, "fab test preview"), Some(true), "the thread kept its own token beside the check's (waiting {waiting})");
        let after = tokens();
        assert_eq!(holder(&after, "fab test check"), None, "the check's permit outlived it");
        assert_eq!(holder(&after, "fab test preview"), Some(false), "the thread's token is not its own again (waiting {waiting})");
    }
    release(job);
}

#[test]
fn the_test_print_tool_places_edits_and_reads_back_in_one_undo_step_each() {
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.model = FabModel::with_script("brackets", "bracket.cad.splash", include_str!("../../app/resources/bracket.cad.splash"));
    s.rebuild().unwrap();
    let b = s.last.as_ref().unwrap().plate.parts[0].bounds();
    let mid = |k: usize| (b.min[k] + b.max[k]) / 2.0;
    let before = s.model.state().clone();
    // Two in one call are one undo step; with no name, the first free one.
    let fields = format!(r#""action":"add","boxes":[{{"object":"bracket/bracket","at":[{},{},{}],"size":20,"name":"Neck"}},{{"object":"bracket/bracket","at":[{},{},{}],"size":[10,12,14],"turn":30,"on":false}}]"#, mid(0), mid(1), mid(2), mid(0) + 5.0, mid(1), mid(2));
    let r = mutation(&mut s, "test_print", "add", &fields);
    assert!(r.ok, "{}", r.text);
    let names: Vec<(&str, bool)> = s.model.project().test_prints.iter().map(|t| (t.name.as_str(), t.on)).collect();
    assert_eq!(names, [("Neck", true), ("Test 1", false)]);
    // Read back where it stands, on the plate.
    let r = s.call("inspect", &args(r#"{"program":false}"#), None);
    let inspected = makepad_script::syntax::parse_value(&r.text).unwrap();
    let listed = inspected.field("test_prints").unwrap().value.items().unwrap();
    assert_eq!(listed[0].field("name").unwrap().value.string(), Some("Neck"));
    assert_eq!(listed[0].field("object").unwrap().value.string(), Some("bracket/bracket"));
    assert_eq!(listed[1].field("on").unwrap().value.ident(), Some("false"));
    assert!(s.model.project_text().contains("test_print(\"Neck\""), "the project keeps it: {}", s.model.project_text());
    // Set: what is given of it, the rest stays.
    let r = mutation(&mut s, "test_print", "set", r#""action":"set","name":"Neck","size":30,"to":"Upper neck""#);
    assert!(r.ok, "{}", r.text);
    let ev = s.last.clone().unwrap();
    let t = s.model.project().test_print("Upper neck").unwrap();
    let placed = ev.test_print_box(t).unwrap();
    assert!((placed.size[0] - 30.0).abs() < 1e-9 && (placed.center[0] - mid(0)).abs() < 1e-6, "{placed:?}");
    // What is wrong says which, and what would put it right; it changes nothing.
    let head_now = s.model.head();
    let r = mutation(&mut s, "test_print", "taken", &format!(r#""action":"add","boxes":[{{"object":"bracket/bracket","at":[0,0,0],"size":10,"name":"Upper neck"}}]"#));
    assert!(!r.ok && r.text.contains("Upper neck") && r.text.contains("taken"), "{}", r.text);
    let r = mutation(&mut s, "test_print", "no part", r#""action":"add","boxes":[{"object":"nothing/here","at":[0,0,0],"size":10}]"#);
    assert!(!r.ok && r.text.contains("nothing/here") && r.text.contains("fab.inspect"), "{}", r.text);
    let r = mutation(&mut s, "test_print", "none", r#""action":"remove","name":"Absent""#);
    assert!(!r.ok && r.text.contains("Absent"), "{}", r.text);
    assert_eq!(s.model.head(), head_now);
    // Undo through the undo tool: each call one step.
    let r = mutation(&mut s, "test_print", "remove", r#""action":"remove","name":"Test 1""#);
    assert!(r.ok, "{}", r.text);
    for left in [2, 2, 0] {
        let h = head(&s);
        assert!(call(&mut s, "history", &[("expected", &h), ("action", "undo")]).ok);
        assert_eq!(s.model.project().test_prints.len(), left);
    }
    assert_eq!(s.model.state(), &before);
    assert!(manifest().tool("test_print").is_some());
}

/// A small asymmetric demo: a rectangular base with a raised square boss.
fn lay_flat_demo() -> Session {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    let source = format!("{}let base = m.box({{size: [20mm 8mm 12mm] at: [0 4mm 0]}})\nlet boss = m.box({{size: [6mm 10mm 6mm] at: [2mm 10mm 1mm]}})\nlet part = m.union(base, boss)\nobject(part)\n", fab_model::program::EMPTY);
    session.model = FabModel::load("Demo", "use mod.fab.*\nscript(\"demo.cad.splash\")\nplace(\"demo\", object: \"part\", at: [70mm 90mm], rotate: 30deg)\n", [("demo.cad.splash".into(), source)].into()).unwrap();
    session.rebuild().unwrap();
    session
}

fn lay_flat_face(session: &Session) -> ([f64; 3], [f64; 3]) {
    let frame = session.last.as_ref().unwrap().placed_frame(0).unwrap();
    (params::xf_point(frame, [10.0, 4.0, 0.0]), params::xf_dir(frame, [1.0, 0.0, 0.0]))
}

fn lay_flat_normal(session: &mut Session) -> Reply {
    let (_, normal) = lay_flat_face(session);
    mutation(session, "lay_flat", "flat", &format!(r#""object":"demo/part","normal":{normal:?}"#))
}

fn flat_close(a: [f64; 3], b: [f64; 3], what: &str) {
    assert!((0..3).all(|k| (a[k] - b[k]).abs() < 1e-3), "{what}: {a:?} != {b:?}");
}

#[test]
fn lay_flat_by_normal_writes_what_the_ui_move_writes() {
    let (mut tool, mut ui) = (lay_flat_demo(), lay_flat_demo());
    let (_, normal) = lay_flat_face(&ui);
    let movement = lay_flat_move(ui.last.as_ref().unwrap(), "demo/part", normal).unwrap();
    let r = ui.place_parts(&[movement]);
    assert!(r.ok, "UI move: {}", r.text);
    let r = lay_flat_normal(&mut tool);
    assert!(r.ok, "tool: {}", r.text);
    assert_eq!(tool.model.project_text(), ui.model.project_text(), "UI and tool write the same placement");
    let inspect = tool.call("inspect", &args(r#"{"program":false}"#), None);
    let plate = |text: &str| {
        let value = makepad_script::syntax::parse_value(text).unwrap();
        text[value.field("plate").unwrap().value.span.clone()].to_string()
    };
    assert_eq!(plate(&r.text), plate(&inspect.text), "the reply gives the same rotation and plate as inspect");
    assert!(r.text.contains("rotate:"), "the new rotation is reported: {}", r.text);
    assert!(manifest().tool("lay_flat").is_some());
}

#[test]
fn lay_flat_turns_that_face_down() {
    let mut session = lay_flat_demo();
    let r = lay_flat_normal(&mut session);
    assert!(r.ok, "{}", r.text);
    let ev = session.last.as_ref().unwrap();
    let frame = ev.placed_frame(0).unwrap();
    flat_close(params::xf_dir(frame, [1.0, 0.0, 0.0]), [0.0, 0.0, -1.0], "the chosen face points down");
    let point = params::xf_point(frame, [10.0, 4.0, 0.0]);
    assert!(point[2].abs() < 1e-3, "the chosen surface is on the bed: {point:?}");
    assert!(ev.plate.parts[0].bounds().min[2].abs() < 1e-3, "the whole part stands on the bed");
}

#[test]
fn lay_flat_by_point_reads_the_surface_there() {
    let (mut point_session, mut normal_session) = (lay_flat_demo(), lay_flat_demo());
    // Use the actual public surface answer, including its plate-coordinate rounding.
    let surface = point_session.call("surface", &args(r#"{"solid":"demo/part","near":[10,4,0]}"#), None);
    assert!(surface.ok, "{}", surface.text);
    let value = makepad_script::syntax::parse_value(&surface.text).unwrap();
    let found = &value.field("points").unwrap().value.items().unwrap()[0];
    let point: [f64; 3] = std::array::from_fn(|k| found.field("plate").unwrap().value.items().unwrap()[k].number().unwrap() * 1000.0);
    let r = mutation(&mut point_session, "lay_flat", "point", &format!(r#""object":"demo/part","point":{point:?}"#));
    assert!(r.ok, "{}", r.text);
    let r = lay_flat_normal(&mut normal_session);
    assert!(r.ok, "{}", r.text);
    assert_eq!(point_session.model.project_text(), normal_session.model.project_text(), "the surface point selects that face's normal");
}

#[test]
fn lay_flat_is_one_undo_step() {
    let mut session = lay_flat_demo();
    let before = session.model.project_text();
    let source = session.model.source("demo.cad.splash").unwrap().to_string();
    let r = lay_flat_normal(&mut session);
    assert!(r.ok, "{}", r.text);
    assert_ne!(session.model.project_text(), before);
    assert_eq!(session.model.source("demo.cad.splash"), Some(source.as_str()), "laying flat changes only the project");
    let h = head(&session);
    let r = call(&mut session, "history", &[("expected", &h), ("action", "undo")]);
    assert!(r.ok, "{}", r.text);
    assert_eq!(session.model.project_text(), before, "one undo restores the exact project text");
    assert!(!session.model.can_undo(), "the move added exactly one undo step");
}

#[test]
fn lay_flat_keeps_what_a_test_print_cuts() {
    let mut session = lay_flat_demo();
    let ev = session.last.as_ref().unwrap();
    let before_frame = *ev.placed_frame(0).unwrap();
    let center = params::xf_point(&before_frame, [3.0, 6.0, 2.0]);
    let r = mutation(&mut session, "test_print", "cut", &format!(r#""action":"add","boxes":[{{"object":"demo/part","at":{center:?},"size":[4,6,8],"turn":[10,20,30],"name":"Sample"}}]"#));
    assert!(r.ok, "{}", r.text);
    let placed = |session: &Session| session.last.as_ref().unwrap().test_print_box(session.model.project().test_print("Sample").unwrap()).unwrap();
    let before = placed(&session);
    let (_, normal) = lay_flat_face(&session);
    let movement = lay_flat_move(session.last.as_ref().unwrap(), "demo/part", normal).unwrap();
    let r = lay_flat_normal(&mut session);
    assert!(r.ok, "{}", r.text);
    let after = placed(&session);
    flat_close(after.size, before.size, "the cut's side lengths stay fixed");
    // The effective rigid delta includes the final drop onto the bed.
    let after_frame = session.last.as_ref().unwrap().placed_frame(0).unwrap();
    let moved_origin = params::xf_point(&movement.delta, before_frame.map(|row| row[3]));
    let drop = after_frame[2][3] - moved_origin[2];
    let mut wanted_center = params::xf_point(&movement.delta, before.center);
    wanted_center[2] += drop;
    flat_close(after.center, wanted_center, "the cut centre follows the part's turn and bed drop");
    let (old_axes, new_axes) = (fab_model::eval::rotation_xyz(before.turn), fab_model::eval::rotation_xyz(after.turn));
    for k in 0..3 {
        let old_axis = std::array::from_fn(|i| old_axes[i][k]);
        flat_close(std::array::from_fn(|i| new_axes[i][k]), params::xf_dir(&movement.delta, old_axis), "each cut axis follows the part's rotation");
    }
    let dir = std::env::temp_dir().join(format!("fab-lay-flat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("demo.fab.splash");
    project::save(&session.model, &path).unwrap();
    let mut reloaded = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    reloaded.model = project::load(&path, &ScriptEnv::default()).unwrap();
    reloaded.rebuild().unwrap();
    assert_eq!(reloaded.model.project_text(), session.model.project_text(), "save and reload keep the placement and cut");
    let saved = placed(&reloaded);
    flat_close(saved.center, after.center, "reload keeps the cut centre");
    flat_close(saved.size, after.size, "reload keeps the cut sides");
    let saved_axes = fab_model::eval::rotation_xyz(saved.turn);
    for k in 0..3 {
        flat_close(std::array::from_fn(|i| saved_axes[i][k]), std::array::from_fn(|i| new_axes[i][k]), "reload keeps the cut axes");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn lay_flat_errors_and_preview_leave_the_project_unchanged() {
    let mut session = lay_flat_demo();
    let before = session.model.project_text();
    let initial_head = head(&session);
    for (id, fields, says) in [
        ("neither", r#""object":"demo/part""#, "exactly one"),
        ("both", r#""object":"demo/part","normal":[1,0,0],"point":[0,0,0]"#, "exactly one"),
        ("unknown", r#""object":"absent/part","normal":[1,0,0]"#, "demo/part"),
        ("zero", r#""object":"demo/part","normal":[0,0,0]"#, "zero"),
        ("distant", r#""object":"demo/part","point":[1000,1000,1000]"#, "not near"),
    ] {
        let r = mutation(&mut session, "lay_flat", id, fields);
        assert!(!r.ok && r.text.contains(says), "{id}: {}", r.text);
        assert_eq!(head(&session), initial_head, "{id} must not mutate");
    }
    let (_, normal) = lay_flat_face(&session);
    let fields = format!(r#""object":"demo/part","normal":{normal:?},"dry_run":true"#);
    let r = mutation(&mut session, "lay_flat", "preview", &fields);
    assert!(r.ok && r.text.contains("committed: false") && r.text.contains("rotate:"), "{}", r.text);
    assert_eq!(session.model.project_text(), before, "preview commits nothing");
    assert_eq!(head(&session), initial_head);
    assert!(!session.model.can_undo(), "errors and preview add no undo steps");
    let r = call(&mut session, "lay_flat", &[("request_id", "stale"), ("expected", "0:stale"), ("object", "demo/part")]);
    assert!(!r.ok && r.text.contains("head"), "a stale head is rejected: {}", r.text);
}

/// A plain value becomes a slider by name, as one batch: `replace` of a top-level `let` with a
/// param, a later replace that reads it, and the stock jellyfish's multi-line rim (the demo itself
/// takes half a minute to compile five times under the JIT). The dry run commits nothing; the
/// commit shows in fab.params with its group.
#[test]
fn apply_turns_a_value_let_into_a_param() {
    let jellyfish = include_str!("../../app/resources/demos/flex_jellyfish/flex_jellyfish.cad.splash");
    let from = jellyfish.find("let rim = ").unwrap() + "let rim = ".len();
    let rim = &jellyfish[from..from + jellyfish[from..].find("\nfn bell_out").unwrap()];
    let mut s = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    s.rebuild().unwrap();
    let h = head(&s);
    let file = "toy.cad.splash";
    let source = format!("use mod.math.*\n{PRELUDE}let bell_w = 38.5mm\nlet bell_h = 36mm\nlet mouth = 3mm\nlet lappet = 2.4mm\nlet rim = [[0 0] [bell_w 0] [bell_w bell_h]]\nlet toy = m.extrude({{sketch: Polygon{{points: rim}} height: 5mm}})\nobject(toy)\n");
    tool_ok(&mut s, "script", &[("request_id", "a"), ("expected", &h), ("script", file), ("source", &source)]);
    let edits = format!("replace(bell_w, param(38.5mm, min: 32mm, max: 46mm, step: 0.5mm, label: \"Bell width\", group: \"Bell\", hint: \"how wide\"))\nreplace(bell_h, bell_w * 0.9)\nreplace(rim, {rim})");
    let dry = mutation(&mut s, "apply", "b", &format!("\"edits\":{},\"dry_run\":true", quoted(&edits)));
    assert!(dry.ok, "{}", dry.text);
    assert_eq!(s.model.source(file).unwrap(), source, "a dry run commits nothing");
    let h = head(&s);
    tool_ok(&mut s, "apply", &[("request_id", "c"), ("expected", &h), ("edits", &edits)]);
    let after = s.model.source(file).unwrap();
    assert!(after.contains("let bell_w = param(38.5mm,") && after.contains("let bell_h = bell_w * 0.9\n") && after.contains(&format!("let rim = {rim}\n")), "{after}");
    let params = tool_ok(&mut s, "params", &[]).text;
    assert!(params.contains("key: \"toy/bell_w\"") && params.contains("group: \"Bell\""), "{params}");
}

/// Object scripts dragged over the plate preview where their drop puts them: two files at once,
/// each under a name of its own, nothing committed; the drop commits that same plate as one undo
/// step. A part the plate has no room for says so before it is dropped.
#[test]
fn dragged_scripts_preview_the_plate_their_drop_commits() {
    let dir = std::env::temp_dir().join(format!("fab-ai-drop-{}", std::process::id()));
    let script = |folder: &str, name: &str, size: &str| {
        let path = dir.join(folder).join(format!("{name}.cad.splash"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.box({{size: [{size}]}}), name: \"{name}\")\n")).unwrap();
        path
    };
    let paths = vec![script("a", "peg", "30mm 20mm 30mm"), script("b", "peg", "20mm 10mm 20mm")];
    let slab = script("c", "slab", "500mm 5mm 500mm");
    let mut s = Session::new(Machine::dual_nozzle_350(), dir.clone());
    s.rebuild().unwrap();
    // A part placed by hand in the bed's centre, where the shelf pack puts what comes next to
    // its slot: the drop keeps off it.
    let base = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nobject(m.box({size: [40mm 10mm 40mm]}), name: \"base\")\n";
    let r = s.commit("base", vec![Change::AddInstance { file: "base.cad.splash".into(), source: Some(base.into()), name: Some("base".into()), at: Some([175.0, 160.0]) }]);
    assert!(r.ok, "{}", r.text);
    let before = (head(&s), s.last.as_ref().unwrap().plate.parts.len());
    let bounds = |ev: &Evaluated| -> Vec<(String, [f64; 3], [f64; 3])> { ev.plate.parts.iter().map(|p| (p.object.clone(), p.bounds().min, p.bounds().max)).collect() };

    let (c, ev, _) = s.preview_script_files(&paths, None).unwrap();
    assert_eq!(ev.plate.parts.len(), before.1 + 2, "both files' parts are on the previewed plate");
    let files: BTreeSet<&str> = c.instances.iter().map(|i| i.script.file.as_str()).collect();
    assert_eq!(files.len(), c.instances.len(), "two files of one name are two scripts: {files:?}");
    assert_eq!(head(&s), before.0, "a preview commits nothing");
    assert!(!ev.warnings.iter().any(|w| w.starts_with("no room")), "{:?}", ev.warnings);
    let previewed = bounds(&ev);
    for (i, (a, amin, amax)) in previewed.iter().enumerate() {
        for (b, bmin, bmax) in &previewed[i + 1..] {
            let apart = amax[0] <= bmin[0] || bmax[0] <= amin[0] || amax[1] <= bmin[1] || bmax[1] <= amin[1];
            assert!(apart, "{a} {amin:?}..{amax:?} and {b} {bmin:?}..{bmax:?} overlap on the previewed plate");
        }
    }

    let r = s.add_script_files(&paths);
    assert!(r.ok, "{}", r.text);
    assert_eq!(bounds(s.last.as_ref().unwrap()), previewed, "the drop commits the plate its preview showed");
    let h = head(&s);
    let r = call(&mut s, "history", &[("expected", &h), ("action", "undo")]);
    assert!(r.ok, "{}", r.text);
    assert_eq!(r.update.unwrap().1.plate.parts.len(), before.1, "one undo takes both files off");

    let (_, ev, _) = s.preview_script_files(&[slab], None).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(ev.warnings.iter().any(|w| w.starts_with("no room") && w.contains("slab")), "{:?}", ev.warnings);
}


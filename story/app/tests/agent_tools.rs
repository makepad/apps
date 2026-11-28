//! Every tool Story offers its agent runs once on a sample document and
//! answers without a tool error: a tool that fails on every call leaves the
//! agent going round in circles. Story is driven over its `app` service
//! (the path the chat, MCP and the code agent take), hidden and muted, with
//! its home, drafts and asset store in the test's own folder.
//!
//! The sample: a new cut with a song, a Motion graphic (shapes, text and a
//! 3D scene), a title, a voice-over and a storyboard; then a new deck. Each
//! tool gets its arguments from [`ARGS`] (`"$name"` is a value an earlier
//! call returned) or, when it is not listed, the smallest its schema takes.
//! A tool answers OK, or is in one of three tables, each entry with why:
//! [`SKIP`] (it acts outside the test or reads what only such a tool
//! makes), [`ABSENT`] (this test's Story lacks what it needs: a model, the
//! effect pack) and [`BROKEN`] (known broken, to fix).
//!
//! The picture tools (`frame`, `strip`, `onion`) run on every box, from this
//! source: under the JIT Story's loader runs `makepad-edit-tools`; under
//! cargo the test builds it, as the harness builds Story.

use makepad_ai_services::wire::{AppLine, ToolOutcome};
use makepad_remote::{Client, Event};
use makepad_strict_json::{self as json, Value};
use makepad_studio_api::services::{targets_of, Product};
use makepad_test::{run_with_config, TestApp, TestConfig};
use std::collections::HashMap;
use std::path::Path;

/// The sample Motion document: shapes, text and a 3D scene.
const MOTION: &str = "use mod.motion.*\nuse mod.scene\nMotion{size: vec2(640, 360) duration: 2.0 layers: [\n    Rect{size: vec2(640, 360) fill: #203040}\n    Text{text: \"Guard\" size: 48 at: vec2(40, 40) color: #ffffff}\n    Scene3D{camera: Camera{position: vec3(0, 0, 6)} children: [scene.Mesh{name: \"box\" geometry: scene.BoxGeometry{} material: scene.MeshStandardMaterial{color: #ff8000}}]}\n]}\n";

/// The sample panel: one slider on the graphic's text size.
const PANEL: &str = "let editor_panel = fn() { View{flow: Down Slider{text: \"Size\" path: \"layers[1].size\" min: 12 max: 96}} }";

/// The sample app run: validated, never run.
const APP_RUN: &str = "use mod.apprun.*\nAppRun{label: \"guard\" app: \"counter\" output: {width: 1080 height: 1920 dpi_scale: 3 fps: 60} script: fn(app) { app.hold(1) }}\n";

/// Tools not run, with why.
const SKIP: &[(&str, &str)] = &[
    ("open_file", "opens a file in the system's app"),
    ("reveal_file", "shows a file in the system's file manager"),
    ("render_cut", "renders the film to a file (minutes)"),
    ("export_web", "compiles the film for the web (minutes)"),
    ("run_app_run", "builds and runs an app"),
    ("app_dump", "launches an app"),
    ("speak", "generates speech on a voice model"),
    ("new_video", "makes a whole video with a model"),
    ("make_video", "makes a video with a model"),
    ("generate_run", "generates on the AI Hub"),
    // What only a skipped tool makes.
    ("app_run_status", "follows a run_app_run job"),
    ("read_app_run", "reads a run_app_run render"),
    ("run_facts", "reads an app-run clip (run_app_run)"),
    ("bind_overlay", "binds to an app-run clip (run_app_run)"),
    ("make_video_note", "talks to a make_video run"),
    ("new_video_redo", "redoes a new_video step"),
    ("new_video_cancel", "stops a new_video step"),
    ("generate_pick", "picks a generate_run take"),
    ("generate_cancel", "stops a generate_run"),
    ("storyboard_look", "looks at the board's pictures with a model; the sample has none"),
    ("storyboard_plan_update", "compares the cut with the board make_video built it from"),
    ("storyboard_apply_changes", "updates the cut make_video built from the board"),
];

/// Tools this test's Story cannot serve, with the error they answer and why.
const ABSENT: &[(&str, &str, &str)] = &[
    ("cut_panel_make", "uses no model", "the launch says no model (MAKEPAD_AI_PROVIDER=none), even where `claude` is installed"),
    ("editor_deep", "no deep make has run", "reads what editor_make (a model) made"),
    ("write_effect", "needs the VJ effect pack", "Story carries no effect pack"),
    ("read_effect", "no preset", "no effect pack: list_effects is empty"),
    ("add_effect", "guard", "no effect pack: list_effects is empty"),
    ("add_transition", "neither meet at a cut", "the sample's blocks (a title, a voice-over, a sound) meet at no cut on one track"),
    ];

/// Known broken, to fix: the tool, what it answers (empty: it never
/// answers, so it is not called) and why.
const BROKEN: &[(&str, &str, &str)] = &[
    ("save_cut", "", "never answers: the call waits with the app idle"),
    ("open_cut", "", "opens what save_cut saves"),
    // No agent tool puts a Graphic on the cut: the director's and the
    // catalogue's texts name `add_graphic`, which Story does not implement,
    // so the graphic desk has nothing to open.
    ("graphic_open", "is not a Graphic clip", "no tool places a Graphic"),
    ("variants", "no graphic is open", "no tool places a Graphic"),
    ("scene_ask", "no graphic is open", "no tool places a Graphic"),
    ("apply_variant", "no variant", "no tool places a Graphic"),
    ("history_list", "no graphic is open", "no tool places a Graphic"),
    ("history_read", "no graphic is open", "no tool places a Graphic"),
    ("history_diff", "no graphic is open", "no tool places a Graphic"),
    ("history_pick", "no graphic is open", "no tool places a Graphic"),
    ("merge_from_history", "no graphic is open", "no tool places a Graphic"),
    ("graphic_palette", "no graphic is open", "no tool places a Graphic"),
    ("palette_moves", "no graphic is open", "no tool places a Graphic"),
    ("palette_save", "no graphic is open", "no tool places a Graphic"),
    ("palette_apply", "no graphic is open", "no tool places a Graphic"),
    ("editor_make", "no graphic is open", "no tool places a Graphic"),
    ("editor_write", "no graphic is open", "no tool places a Graphic"),
    ("editor_paths", "no graphic is open", "no tool places a Graphic"),
    ("editor_fit", "no graphic is open", "no tool places a Graphic"),
    ("editor_compare", "no graphic is open", "no tool places a Graphic"),
    ("editor_drive", "no gesture", "no tool places a Graphic"),
    ("graphic_source", "no graphic is open", "no tool places a Graphic"),
    ("swaps", "no graphic is open", "no tool places a Graphic"),
    ("swap_variants", "no graphic is open", "no tool places a Graphic"),
];

/// The tools run first, in order: they make the sample.
const SETUP: &[&str] = &["new_cut", "import_media", "write_motion", "add_title", "add_voiceover", "add_clip", "storyboard_new", "storyboard_add", "storyboard_mark_built", "cut_panel_gesture"];
/// Then everything else, then these (they undo or take away).
const LAST: &[&str] = &["cut_panel_drive", "doc_undo", "delete_block", "storyboard_delete", "storyboard_write"];
/// Then a new deck (a tree of files) and its tools, these first.
const DECK: &[&str] = &["new_deck", "add_slide", "doc_create", "doc_read", "doc_write", "doc_patch", "doc_rename", "doc_delete", "add_layer", "set_slide", "set_deck_target"];

/// Arguments by tool (`"$name"`: a value an earlier call returned).
const ARGS: &[(&str, &str)] = &[
    ("new_cut", r#"{"format": "16:9", "duration": 10, "label": "guard"}"#),
    // Not `music`: that bakes its beats and words with models.
    ("import_media", r#"{"path": "$wav", "title": "guard song"}"#),
    ("write_motion", r#"{"name": "guard", "source": "$motion_source"}"#),
    ("add_title", r#"{"text": "Guard", "start": 3, "length": 2}"#),
    ("add_voiceover", r#"{"lines": [{"text": "A guard line", "at": 1}]}"#),
    ("add_clip", r#"{"kind": "audio", "media": "$audio", "start": 0}"#),
    ("set_voice_lines", r#"{"node": "$voice", "lines": [{"text": "Another guard line", "at": 1}]}"#),
    ("add_subtitles", r#"{"voice": "$voice"}"#),
    ("add_transition", r#"{"a": "$title", "b": "$voice"}"#),
    ("add_effect", r#"{"node": "$title", "effect": "guard"}"#),
    ("move_block", r#"{"node": "$title", "start": 4}"#),
    ("trim_block", r#"{"node": "$title", "start": 4, "length": 1.5}"#),
    ("blade", r#"{"node": "$title", "at": 4.5}"#),
    ("set_param", r#"{"node": "$title", "key": "text", "value": "\"Guarded\""}"#),
    ("set_header", r#"{"label": "guard cut"}"#),
    ("set_audio", r#"{"node": "$clip", "fade_in": 0.5}"#),
    ("asset_info", r#"{"asset_id": "$audio_id"}"#),
    ("preview_asset", r#"{"asset": "$audio"}"#),
    ("analyze_music", r#"{"music": "$audio"}"#),
    ("onion", r#"{"from": 0, "to": 1}"#),
    ("storyboard_update", r#"{"id": "scene1", "title": "a guard"}"#),
    ("storyboard_move", r#"{"moves": [{"id": "scene1", "start": 1}]}"#),
    ("storyboard_row", r#"{"action": "add", "kind": "notes", "label": "guard"}"#),
    ("storyboard_set", r#"{"title": "guard board"}"#),
    ("storyboard_link", r#"{"from": "scene1", "to": "scene2"}"#),
    ("storyboard_add", r#"{"items": [{"kind": "scene", "id": "scene1", "start": 0, "length": 3}, {"kind": "scene", "id": "scene2", "start": 5, "length": 2}]}"#),
    ("storyboard_mark_built", r#"{"parts": [{"item": "scene1", "nodes": ["$title"]}]}"#),
    ("storyboard_delete", r#"{"ids": ["scene2"]}"#),
    ("generate_add", r#"{"start": 0, "length": 1}"#),
    ("delete_block", r#"{"node": "$title"}"#),
    ("read_motion", r#"{"name": "guard"}"#),
    ("patch_motion", r#"{"name": "guard", "find": "\"Guard\"", "replace": "\"Guarded\""}"#),
    ("palette", r#"{"file": "guard"}"#),
    ("set_palette", r##"{"file": "guard", "map": {"#203040": "#102030"}}"##),
    ("palette_apply", r##"{"colours": [{"name": "ground", "color": "#102030"}]}"##),
    ("editor_write", r#"{"code": "$panel"}"#),
    ("editor_compare", r#"{"source": "$motion_source"}"#),
    ("validate_motion", r#"{"source": "$motion_source"}"#),
    ("validate_app_run", r#"{"source": "$app_run"}"#),
    ("cut_panel_read", r#"{"path": "edit.$title.span"}"#),
    ("cut_panel_gesture", r#"{"path": "edit.$title.span", "values": [[0.5, 3]]}"#),
    ("cut_panel_drive", r#"{"path": "edit.$title.span", "values": [[1, 3]]}"#),
    ("cut_panel_write", r#"{"modules": "PanelModule{title: \"Guard\"}"}"#),
    ("storyboard_write", r#"{"source": "Storyboard{title: \"guard\" duration: 10 rows: [] items: []}"}"#),
    // The deck.
    ("new_deck", r#"{"title": "Guard deck"}"#),
    ("set_deck_output_language", r#"{"language": "zh"}"#),
    ("set_demo", r#"{"index": 0, "steps": [{"call": "pause"}]}"#),
    ("doc_create", r#"{"path": "slides/guard_plate.splash", "text": "$motion_source"}"#),
    ("doc_read", r#"{"path": "slides/guard_plate.splash"}"#),
    ("doc_write", r#"{"path": "slides/guard_plate.splash", "text": "$motion_source"}"#),
    ("doc_patch", r#"{"path": "slides/guard_plate.splash", "edits": [{"old": "Text{text: \"Guard\" size: 48 at: vec2(40, 40) color: #ffffff}", "new": "Text{text: \"Guarded\" size: 48 at: vec2(40, 40) color: #ffffff}"}]}"#),
    ("doc_rename", r#"{"from": "slides/guard_plate.splash", "to": "slides/guarded_plate.splash"}"#),
    ("add_slide", r#"{"kind": "blank"}"#),
    ("add_layer", r#"{"index": 0, "kind": "camera", "device": "@test_pattern"}"#),
    ("set_slide", r#"{"index": 0, "target_seconds": 30}"#),
    ("set_deck_target", r#"{"seconds": 60}"#),
    ("doc_delete", r#"{"path": "slides/guarded_plate.splash"}"#),
    ("motion_fields", r#"{"type": "Rect", "field": "radius"}"#),
    // new_deck's title slide and add_slide's.
    ("reorder_slides", r#"{"slides": ["slide1", "title"]}"#),
    ("delete_slide", r#"{"index": 1}"#),
];

struct Tool {
    service: String,
    name: String,
    schema: Value,
}

/// One `app` request: its answer lines (one message is one line). It
/// waits as the harness's waits do: an answer that leaves the app idle (a
/// call that waits on nothing) fails, naming the request.
fn request(app: &TestApp, client: &Client, request: &str) -> Vec<AppLine> {
    let channel = client.open("app", request).unwrap_or_else(|e| panic!("{request}: {e:?}"));
    let mut lines = Vec::new();
    app.wait_until(|| loop {
        match channel.try_recv() {
            Ok(Event::Data(data)) => {
                let line = String::from_utf8_lossy(&data);
                lines.push(AppLine::parse(line.trim()).unwrap_or_else(|| panic!("{request}: not an answer line: {line}")));
            }
            Ok(Event::Closed(..)) | Err(false) => return None,
            Ok(_) => {}
            Err(true) => return Some(format!("no answer to `{}`", request.replace('\n', " ").chars().take(200).collect::<String>())),
        }
    });
    lines
}

/// The agent's tools: every tool of Story's services (the chat's, which
/// carry every tool Story implements but the generation tools; see the
/// host's `the_hosts_tools_make_valid_services_for_every_target`).
fn tools(app: &TestApp, client: &Client) -> Vec<Tool> {
    let story: Vec<&str> = targets_of(&[Product::Story]).map(|target| target.id).collect();
    let mut tools: Vec<Tool> = Vec::new();
    for line in request(app, client, "list") {
        let AppLine::Instance { manifest, .. } = line else { continue };
        if !story.contains(&manifest.id.as_str()) {
            continue;
        }
        for tool in manifest.tools {
            if tools.iter().any(|t| t.name == tool.name) {
                continue;
            }
            let schema = json::parse(tool.parameters.as_bytes()).unwrap_or_else(|e| panic!("{}: its schema does not parse: {e}", tool.name));
            tools.push(Tool { service: manifest.id.clone(), name: tool.name, schema });
        }
    }
    tools
}

/// A call's outcome, its text and its JSON.
fn call(app: &TestApp, client: &Client, tool: &Tool, args: &Value) -> (ToolOutcome, String, Value) {
    let line = format!("call {}.{}\n{}", tool.service, tool.name, args.to_json());
    for answer in request(app, client, &line) {
        if let AppLine::Result(result) = answer {
            let value = json::parse(result.text.as_bytes()).unwrap_or(Value::Null);
            return (result.outcome, result.text, value);
        }
    }
    panic!("{}: no result", tool.name)
}

/// The smallest arguments the schema takes: its required properties, each
/// with a sample value.
fn sample(schema: &Value) -> Value {
    if let Some(Value::Arr(values)) = schema.get("enum") {
        return values[0].clone();
    }
    let min = schema.get("minimum").and_then(|v| match v {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    });
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let required: Vec<&str> = schema.get("required").and_then(Value::as_arr).map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let props = schema.get("properties");
            Value::Obj(required.iter().map(|name| (name.to_string(), props.and_then(|p| p.get(name)).map_or(Value::Null, sample))).collect())
        }
        Some("array") => Value::Arr(vec![schema.get("items").map_or(Value::Null, sample)]),
        Some("integer") => Value::Int(min.unwrap_or(0.0) as i64),
        Some("number") => Value::F64(min.unwrap_or(0.0).max(0.5)),
        Some("boolean") => Value::Bool(false),
        _ => Value::Str("guard".into()),
    }
}

/// `"$name"` strings (whole, or inside a string) with the values found so far.
fn bind(value: &Value, vars: &HashMap<String, Value>) -> Value {
    match value {
        Value::Str(s) if s.starts_with('$') && vars.contains_key(&s[1..]) => vars[&s[1..]].clone(),
        Value::Str(s) if s.contains('$') => {
            let mut out = s.clone();
            for (name, value) in vars {
                if let Value::Str(text) = value {
                    out = out.replace(&format!("${name}"), text);
                }
            }
            Value::Str(out)
        }
        Value::Arr(items) => Value::Arr(items.iter().map(|v| bind(v, vars)).collect()),
        Value::Obj(pairs) => Value::Obj(pairs.iter().map(|(k, v)| (k.clone(), bind(v, vars))).collect()),
        other => other.clone(),
    }
}

/// The first value under `key`, depth first.
fn find<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v).or_else(|| pairs.iter().find_map(|(_, v)| find(v, key))),
        Value::Arr(items) => items.iter().find_map(|v| find(v, key)),
        _ => None,
    }
}

/// The first `{asset_id, revision}`, depth first.
fn asset_ref(value: &Value) -> Option<Value> {
    match value {
        Value::Obj(pairs) => match (value.get("asset_id"), value.get("revision")) {
            (Some(id @ Value::Str(_)), Some(rev @ Value::Str(_))) => Some(json::obj(vec![("asset_id", id.clone()), ("revision", rev.clone())])),
            _ => pairs.iter().find_map(|(_, v)| asset_ref(v)),
        },
        Value::Arr(items) => items.iter().find_map(asset_ref),
        _ => None,
    }
}

/// What a call returned that later calls name.
fn capture(tool: &str, result: &Value, vars: &mut HashMap<String, Value>) {
    // A change names what it touched, the new block last.
    let added = find(result, "changed").and_then(Value::as_arr).and_then(|c| c.last()).cloned();
    let mut set = |name: &str, value: Option<Value>| {
        if let Some(value) = value {
            vars.insert(name.to_string(), value);
        }
    };
    match tool {
        "import_media" => {
            let audio = asset_ref(result);
            set("audio_id", audio.as_ref().and_then(|a| a.get("asset_id")).cloned());
            set("audio", audio);
        }
        "write_motion" => set("motion", asset_ref(result)),
        "add_title" => set("title", added.clone()),
        "add_voiceover" => set("voice", added.clone()),
        "add_clip" => set("clip", added.clone()),
        "storyboard_read" => set("board", find(result, "source").cloned()),
        _ => {}
    }
}

/// A second of a 440 Hz tone, 16-bit mono at 8 kHz.
fn write_wav(path: &Path) {
    let samples: Vec<i16> = (0..8000).map(|i| ((i as f64 * 440.0 * std::f64::consts::TAU / 8000.0).sin() * 8000.0) as i16).collect();
    let data = samples.len() as u32 * 2;
    let mut wav = Vec::new();
    for part in [&b"RIFF"[..], &(36 + data).to_le_bytes(), b"WAVEfmt ", &16u32.to_le_bytes(), &1u16.to_le_bytes(), &1u16.to_le_bytes(), &8000u32.to_le_bytes(), &16000u32.to_le_bytes(), &2u16.to_le_bytes(), &16u16.to_le_bytes(), b"data", &data.to_le_bytes()] {
        wav.extend_from_slice(part);
    }
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, wav).unwrap();
}

/// The app's own folders and the sample's files (the harness clears the
/// artifacts folder when it launches).
fn scratch() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("story-agent-tools-{}", std::process::id()))
}

fn config() -> TestConfig {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "agent_tools").unwrap();
    let scratch = scratch();
    for (key, dir) in [("MAKEPAD_HOME", "home"), ("STAGE_ASSET_ROOT", "store"), ("STAGE_EDIT_DRAFTS", "drafts")] {
        let dir = scratch.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        config.env.insert(key.to_string(), dir.display().to_string());
    }
    config.env.insert("MAKEPAD_MUTE".to_string(), "1".to_string());
    config.env.insert("MAKEPAD_AI_PROVIDER".to_string(), "none".to_string());
    // The installable fonts the cut cards' plates set text in (Archivo, Cormorant), as a person
    // who installed them has: the fresh home has none, and a plate naming one is an error.
    config.env.insert("MAKEPAD_FONTS_DIR".to_string(), concat!(env!("CARGO_MANIFEST_DIR"), "/../../../apps/engine/motion/tests/fonts").to_string());
    // Under cargo the picture tools are built here (under the JIT Story's loader runs them).
    if std::env::var_os("RELAX_LOADER").is_none() {
        let tools = makepad_test::build_release_binary(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../libs/edit_tools"),
            "makepad-story-edit-tools",
            Some("makepad-edit-tools"),
            &config.artifacts_dir,
            config.env.get("CARGO_TARGET_DIR").map(String::as_str),
        )
        .unwrap_or_else(|error| panic!("the picture tools (makepad-edit-tools) did not build: {error}"));
        config.env.insert("STAGE_EDIT_TOOLS".to_string(), tools.display().to_string());
    }
    config
}

#[test]
fn every_agent_tool_answers_on_a_sample_document() {
    let config = config();
    let wav = scratch().join("guard.wav");
    write_wav(&wav);
    run_with_config(config, |app: TestApp| {
        let client = Client::connect(&app.pid().to_string()).expect("the app's endpoint");
        // Story's services link on the chat's first pump.
        app.wait_until(|| (tools(&app, &client).len() < 100).then(|| "Story's services are not linked".to_string()));
        let tools = tools(&app, &client);
        let named = |name: &str| tools.iter().position(|t| t.name == name).unwrap_or_else(|| panic!("{name} is not one of Story's tools"));
        for name in SKIP.iter().map(|(n, _)| n).chain(ABSENT.iter().map(|(n, _, _)| n)).chain(BROKEN.iter().map(|(n, _, _)| n)) {
            named(name);
        }
        // The order: the sample, everything else, what undoes or takes
        // away, then the deck (its own tools first), deleting a slide last.
        let runs = |t: &&Tool| !SKIP.iter().any(|(name, _)| *name == t.name) && !BROKEN.iter().any(|(name, says, _)| *name == t.name && says.is_empty());
        let listed = |t: &Tool| SETUP.contains(&t.name.as_str()) || LAST.contains(&t.name.as_str()) || DECK.contains(&t.name.as_str()) || t.name == "delete_slide";
        let mut order: Vec<&Tool> = SETUP.iter().map(|name| &tools[named(name)]).collect();
        order.extend(tools.iter().filter(|t| !listed(t) && t.service != "stage_deck").filter(runs));
        order.extend(LAST.iter().chain(DECK).map(|name| &tools[named(name)]));
        order.extend(tools.iter().filter(|t| !listed(t) && t.service == "stage_deck").filter(runs));
        order.push(&tools[named("delete_slide")]);

        let mut vars: HashMap<String, Value> = HashMap::new();
        for (name, text) in [("wav", wav.display().to_string()), ("motion_source", MOTION.to_string()), ("panel", PANEL.to_string()), ("app_run", APP_RUN.to_string())] {
            vars.insert(name.to_string(), Value::Str(text));
        }
        let (mut report, mut ok) = (Vec::new(), 0);
        for tool in &order {
            let args = match ARGS.iter().find(|(name, _)| *name == tool.name) {
                Some((_, text)) => bind(&json::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{}: ARGS: {e}", tool.name)), &vars),
                None => sample(&tool.schema),
            };
            let (outcome, text, result) = call(&app, &client, tool, &args);
            let head: String = text.chars().take(240).collect::<String>().replace('\n', " ");
            println!("{} {:?} {} => {head}", tool.name, outcome, args.to_json().chars().take(160).collect::<String>());
            capture(&tool.name, &result, &mut vars);
            let absent = ABSENT.iter().find(|(name, _, _)| *name == tool.name);
            let broken = BROKEN.iter().find(|(name, _, _)| *name == tool.name);
            match (outcome, absent, broken) {
                (ToolOutcome::Ok, None, None) => ok += 1,
                (ToolOutcome::Ok, Some((name, _, why)), _) => report.push(format!("{name} answers now ({why} no longer holds): take it out of ABSENT")),
                (ToolOutcome::Ok, _, Some((name, _, _))) => report.push(format!("{name} answers now: take it out of BROKEN")),
                (_, Some((_, says, _)), _) if text.contains(says) => {}
                (_, _, Some((_, says, _))) if text.contains(says) => {}
                _ => report.push(format!("{} {:?} {}: {head}", tool.name, outcome, args.to_json().chars().take(200).collect::<String>())),
            }
        }
        println!("{ok} of {} tools answered OK; {} skipped, {} absent here, {} known broken", tools.len(), SKIP.len(), ABSENT.len(), BROKEN.len());
        assert!(report.is_empty(), "{} tools answered with an error:\n{}", report.len(), report.join("\n"));
    })
    .unwrap();
    std::fs::remove_dir_all(scratch()).unwrap();
}

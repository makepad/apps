//! Settings cards end to end at the engine level, without a widget: a
//! scripted model asks for a request tool of a Stage target, the engine
//! parks the call as the card its schema and layout make, the person's edits
//! are applied to the card's values, Confirm sends the edited arguments to
//! the target's service, and a fake host checks them against the tool's
//! schema and answers. Every catalogue tool must build a card whose
//! confirmed arguments pass its schema.

use makepad_ai_services::card::{Card, Field, MAX_FIELDS};
use makepad_ai_services::port::ServiceLink;
use makepad_ai_services::wire::{canonical_name, HostedUp, ServiceDown, ServiceUp, ToolResult};
use makepad_ai_services::{EngineCore, Model, ModelEvent, ServiceRegistry, Target as ChatTarget, ToolDefinition};
use makepad_studio_api::services::{self, TARGETS};
use makepad_studio_api::{find, schema, CallResult, ErrorKind, Registry, ToolError, ToolOutput, TOOLS};
use makepad_strict_json::Value;

/// The stand-in host: every handler answers with the arguments it received.
type Handler = fn(&Value) -> CallResult;

fn echo(args: &Value) -> CallResult {
    Ok(ToolOutput::json(args.clone()))
}

fn registry() -> Registry<Handler> {
    let mut registry: Registry<Handler> = Registry::new();
    for tool in TOOLS.iter().filter(|tool| tool.request) {
        registry.register(tool.name, echo);
    }
    registry
}

/// The card a request tool makes for `prefill` (an object of arguments).
fn card_for(tool: &str, prefill: &Value) -> Result<Card, String> {
    let spec = find(tool).ok_or_else(|| format!("there is no tool `{tool}`"))?;
    let layout = spec.layout.as_ref().map(services::card_layout);
    Card::for_schema(tool, spec.description, spec.input, layout.as_ref(), prefill)
}

/// A model that asks for one tool call on the first line and says it is done
/// when the result comes back.
struct AsksOnce {
    name: String,
    args: String,
    out: Vec<ModelEvent>,
}

impl Model for AsksOnce {
    fn label(&self) -> String {
        "Scripted".into()
    }
    fn configure(&mut self, _system: &str, _tools: &[ToolDefinition]) -> Result<(), String> {
        Ok(())
    }
    fn send_user(&mut self, _text: &str, _dynamic_context: &str) {
        self.out.push(ModelEvent::ToolCall { call_id: "m1".into(), name: self.name.clone(), args: self.args.clone() });
        self.out.push(ModelEvent::TurnDone { tool_calls: 1 });
    }
    fn send_tool_result(&mut self, _call_id: &str, _text: &str, _is_error: bool) {
        self.out.push(ModelEvent::Delta("Done.".into()));
        self.out.push(ModelEvent::TurnDone { tool_calls: 0 });
    }
    fn cancel(&mut self) {}
    fn reset(&mut self) {}
    fn poll(&mut self) -> Vec<ModelEvent> {
        std::mem::take(&mut self.out)
    }
}

/// One chat turn: the model asks for `tool` with `prefill` (a JSON object),
/// the engine parks it as a card on the tool's target, the person's edits
/// (an argument and the option label to pick or the text to type) go on the
/// card, Confirm sends the edited arguments to the service, and the host
/// checks and runs them.
fn turn(tool: &str, prefill: &str, edits: &[(&str, &str)]) -> Result<Value, ToolError> {
    let spec = find(tool).unwrap_or_else(|| panic!("no tool {tool}"));
    let target = TARGETS.iter().find(|target| target.has(spec)).unwrap_or_else(|| panic!("{tool} is in no target"));
    let manifest = target.manifest(TOOLS.iter()).unwrap();
    let (link, host) = ServiceLink::pair(manifest.clone());
    host.up.send(HostedUp { from: None, msg: ServiceUp::Register { manifest, port_tag: 1 } }).unwrap();
    let reg = ServiceRegistry::new();
    reg.register(link, "the test host", None).unwrap();
    let model = AsksOnce { name: canonical_name(target.id, tool), args: prefill.to_string(), out: Vec::new() };
    let mut core = EngineCore::new(reg, Box::new(model), None, 1);
    core.set_target(Some(ChatTarget { label: target.label.into(), app: target.id.into(), context: String::new(), attachments: Vec::new() }));
    core.send("make it", 0.0);
    core.pump(0.1);
    assert!(host.down.try_iter().all(|frame| !matches!(frame.msg, ServiceDown::Call(_))), "{tool}: a request must wait for the person");
    let card = core.state().tool("m1").unwrap_or_else(|| panic!("{tool}: the call is in the transcript")).card.clone().unwrap_or_else(|| panic!("{tool}: a request parks as a card"));

    let mut values = card.values();
    let (mut pick, mut line) = (0, 0);
    for field in &card.fields {
        let edit = edits.iter().find(|(arg, _)| *arg == field.arg()).map(|(_, text)| *text);
        match field {
            Field::Pick { options, label, .. } => {
                if let Some(text) = edit {
                    values.picks[pick] = options.iter().position(|(option, _)| option == text).unwrap_or_else(|| panic!("{label}: no option `{text}` in {options:?}"));
                }
                pick += 1;
            }
            Field::Line { .. } => {
                if let Some(text) = edit {
                    values.lines[line] = text.to_string();
                }
                line += 1;
            }
        }
    }
    for (arg, _) in edits {
        assert!(card.fields.iter().any(|field| field.arg() == *arg), "the card has no field `{arg}`");
    }
    core.confirm_with("m1", true, Some(card.args(&values).to_json()), 0.2).map_err(ToolError::invalid)?;

    let call = host.down.try_iter().find_map(|frame| match frame.msg {
        ServiceDown::Call(call) => Some(call),
        _ => None,
    });
    let call = call.unwrap_or_else(|| panic!("{tool}: the confirmed call reached the service"));
    assert_eq!(call.tool, tool);
    let registry = registry();
    let checked = registry.check(tool, schema::parse(&call.args).unwrap())?;
    let output = (checked.handler)(&checked.args)?;
    host.up.send(HostedUp { from: None, msg: ServiceUp::Result(ToolResult::ok(&call.call_id, output.json.to_json(), "ran")) }).unwrap();
    core.pump(0.3);
    Ok(output.json)
}

/// Objects equal regardless of field order.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Obj(a), Value::Obj(b)) => a.len() == b.len() && a.iter().all(|(key, value)| b.iter().any(|(other, expected)| other == key && same(value, expected))),
        (Value::Arr(a), Value::Arr(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b)),
        _ => a == b,
    }
}

fn expect(got: Result<Value, ToolError>, want: &str) {
    let got = got.unwrap_or_else(|error| panic!("{}", error.message));
    let want = schema::parse(want).unwrap();
    assert!(same(&got, &want), "got {} want {}", got.to_json(), want.to_json());
}

// ---------------------------------------------------------------- (a)

/// A value that satisfies `schema`.
fn synthesise(schema: &Value) -> Value {
    if let Some(Value::Arr(allowed)) = schema.get("enum") {
        return allowed[0].clone();
    }
    if let Some(Value::Arr(options)) = schema.get("anyOf") {
        return synthesise(&options[0]);
    }
    let kind = match schema.get("type") {
        Some(Value::Str(name)) => name.clone(),
        Some(Value::Arr(names)) => names[0].as_str().unwrap().to_string(),
        _ => "string".into(),
    };
    let number = |key: &str| match schema.get(key) {
        Some(Value::Int(value)) => Some(*value as f64),
        Some(Value::F64(value)) => Some(*value),
        _ => None,
    };
    match kind.as_str() {
        "boolean" => Value::Bool(true),
        "integer" | "number" => {
            let mut value = number("minimum").unwrap_or(1.0).max(1.0);
            if let Some(maximum) = number("maximum") {
                value = value.min(maximum);
            }
            if kind == "integer" || value.fract() == 0.0 { Value::Int(value as i64) } else { Value::F64(value) }
        }
        "array" => {
            let count = schema.get("minItems").and_then(Value::as_u64).unwrap_or(0) as usize;
            let item = schema.get("items").map(synthesise).unwrap_or(Value::Int(1));
            Value::Arr(vec![item; count])
        }
        "object" => {
            let mut fields = Vec::new();
            if let (Some(Value::Arr(required)), Some(Value::Obj(properties))) = (schema.get("required"), schema.get("properties")) {
                for name in required.iter().filter_map(Value::as_str) {
                    fields.push((name.to_string(), synthesise(&properties.iter().find(|(key, _)| key == name).unwrap().1)));
                }
            }
            Value::Obj(fields)
        }
        "null" => Value::Null,
        _ => Value::Str("x".repeat(schema.get("minLength").and_then(Value::as_u64).unwrap_or(1) as usize)),
    }
}

#[test]
fn every_catalogue_tool_has_a_card_that_confirms_into_valid_arguments() {
    assert!(TOOLS.len() > 170);
    for tool in TOOLS {
        // Nothing prefilled: the card builds, within the slot limits.
        let empty = card_for(tool.name, &Value::Obj(Vec::new())).unwrap_or_else(|error| panic!("{}: {error}", tool.name));
        let picks = empty.fields.iter().filter(|field| matches!(field, Field::Pick { .. })).count();
        assert!(empty.fields.len() <= MAX_FIELDS, "{}: {} fields", tool.name, empty.fields.len());
        assert!(!empty.title.is_empty() && !empty.confirm.is_empty() && !empty.why.is_empty(), "{}", tool.name);
        assert_eq!(empty.values().picks.len(), picks);

        // Every required field prefilled: the confirmed arguments fit the schema.
        let input = tool.input_schema();
        let mut prefill = Vec::new();
        if let (Some(Value::Arr(required)), Some(Value::Obj(properties))) = (input.get("required"), input.get("properties")) {
            for name in required.iter().filter_map(Value::as_str) {
                prefill.push((name.to_string(), synthesise(&properties.iter().find(|(key, _)| key == name).unwrap().1)));
            }
        }
        let prefill = Value::Obj(prefill);
        let card = card_for(tool.name, &prefill).unwrap();
        let args = card.args(&card.values());
        if let Err(errors) = schema::validate(&input, &args) {
            panic!("{}: prefill {} gave {}: {}", tool.name, prefill.to_json(), args.to_json(), errors.join("; "));
        }
        for (name, value) in match &prefill { Value::Obj(fields) => fields.as_slice(), _ => &[] } {
            // Typed back as it was given (a whole number stays whole).
            let got = args.get(name).unwrap_or_else(|| panic!("{}: `{name}` lost", tool.name));
            let numeric_equal = matches!((got, value), (Value::Int(_) | Value::F64(_), Value::Int(_) | Value::F64(_))) && schema::validate(&input, &args).is_ok();
            assert!(same(got, value) || numeric_equal, "{}: `{name}` {} became {}", tool.name, value.to_json(), got.to_json());
        }
    }
}

#[test]
fn unknown_tools_and_the_catalogue_additions() {
    assert!(card_for("no_such_tool", &Value::Null).is_err());
    for name in ["new_video", "new_video_status", "new_video_redo", "generate_add", "generate_run", "generate_status", "generate_pick", "make_video"] {
        assert!(makepad_studio_api::find(name).is_some(), "{name}");
    }
    let make = makepad_studio_api::find("make_video").unwrap();
    assert!(make.mutates && make.backed_by == "CUT video maker");
}

// ---------------------------------------------------------------- (b)

#[test]
fn gen_run_card_edit_confirm() {
    let prefill = r#"{"preset": "studio:flux", "prompt": "a red fox"}"#;
    expect(turn("gen_run", prefill, &[("seed", "7")]), r#"{"preset": "studio:flux", "prompt": "a red fox", "seed": 7}"#);
}

/// The lines of a card by argument, as the person sees them.
fn lines(card: &Card) -> Vec<(String, String)> {
    card.fields.iter().filter_map(|field| match field {
        Field::Line { arg, value, .. } => Some((arg.clone(), value.clone())),
        _ => None,
    }).collect()
}

#[test]
fn gen_run_card_of_a_preset_shows_its_prompt_inputs_and_parameters() {
    // What the FLOW page's preset card opens: the host's prefill of the preset (id, prompt,
    // its settings as `params`); the input assets are for the person to give.
    let prefill = r#"{"preset": "flux-schnell-fast", "prompt": "a neon koi", "params": {"width": 1024, "height": 576, "steps": 4}}"#;
    let card = card_for("gen_run", &schema::parse(prefill).unwrap()).unwrap();
    let order: Vec<&str> = card.fields.iter().map(Field::arg).collect();
    assert_eq!(order, ["preset", "prompt", "inputs", "params.width", "params.height", "params.steps", "seed"]);
    let lines = lines(&card);
    assert!(lines.contains(&("params.steps".to_string(), "4".to_string())), "{lines:?}");
    assert!(lines.contains(&("inputs".to_string(), String::new())));
    assert_eq!(card.title, "Generate with a preset");
    // Edited in the card: steps to 30 (a number), a new size, an input asset, a seed.
    expect(
        turn("gen_run", prefill, &[("params.steps", "30"), ("params.width", "576"), ("inputs", "ast_1@2"), ("seed", "9")]),
        r#"{"preset": "flux-schnell-fast", "prompt": "a neon koi", "inputs": ["ast_1@2"], "params": {"width": 576, "height": 576, "steps": 30}, "seed": 9}"#,
    );
    // Untouched, the preset's own settings go through as they were.
    expect(turn("gen_run", prefill, &[]), prefill);
    // A word in a number field is refused naming the field; nothing runs.
    let refused = turn("gen_run", prefill, &[("params.steps", "many")]).unwrap_err();
    assert!(refused.message.contains("params.steps"), "{}", refused.message);
}

#[test]
fn gen_save_preset_card_edit_confirm() {
    let prefill = r#"{"preset": "flux-schnell-fast", "name": "Flux fast (custom)", "prompt": "a neon koi", "params": {"steps": 4}}"#;
    let card = card_for("gen_save_preset", &schema::parse(prefill).unwrap()).unwrap();
    assert_eq!((card.title.as_str(), card.confirm.as_str()), ("Save as preset", "Save"));
    let order: Vec<&str> = card.fields.iter().map(Field::arg).collect();
    assert_eq!(order, ["preset", "name", "prompt", "seed", "params.steps"]);
    expect(
        turn("gen_save_preset", prefill, &[("name", "Koi poster"), ("params.steps", "8")]),
        r#"{"preset": "flux-schnell-fast", "name": "Koi poster", "prompt": "a neon koi", "params": {"steps": 8}}"#,
    );
    // The preset is the one required field.
    assert!(turn("gen_save_preset", r#"{"name": "x"}"#, &[]).is_err());
}

#[test]
fn gen_cancel_card_stops_the_named_job() {
    let card = card_for("gen_cancel", &schema::parse(r#"{"id": "7"}"#).unwrap()).unwrap();
    assert_eq!((card.title.as_str(), card.confirm.as_str()), ("Stop the generation", "Stop"));
    expect(turn("gen_cancel", r#"{"id": "7"}"#, &[]), r#"{"id": "7"}"#);
}

#[test]
fn new_video_card_edit_confirm() {
    let (tool, prefill) = ("new_video", r#"{"ask": "a lyric video about night trains", "seconds": 60}"#);
    expect(turn(tool, prefill, &[("seconds", "90")]), r#"{"ask": "a lyric video about night trains", "seconds": 90}"#);
}

#[test]
fn generate_run_card_edit_confirm() {
    let (tool, prefill) = ("generate_run", r#"{"node": "gen1", "takes": 2}"#);
    expect(turn(tool, prefill, &[("preview", "yes")]), r#"{"node": "gen1", "takes": 2, "preview": true}"#);
}

#[test]
fn make_video_card_edit_confirm() {
    let (tool, prefill) = ("make_video", r#"{"prompt": "a cat learns to surf", "aspect": "9:16", "seconds": 30}"#);
    expect(turn(tool, prefill, &[("music", "none"), ("voice", "yes")]), r#"{"prompt": "a cat learns to surf", "aspect": "9:16", "seconds": 30, "music": "none", "voice": "yes"}"#);
}

/// The arguments the host's `make_video` handler reads (`brief_from_args`):
/// prompt, aspect, seconds, voice, music, media, authorship, style. Edits of
/// every one reach it typed as the handler expects; fields left at their
/// default are omitted (the handler's defaults apply).
#[test]
fn make_video_card_gives_exactly_the_handlers_arguments() {
    let (tool, prefill) = ("make_video", r#"{"prompt": "launch video for Notes", "aspect": "9:16", "seconds": 30}"#);
    expect(turn(tool, prefill, &[]), r#"{"prompt": "launch video for Notes", "aspect": "9:16", "seconds": 30}"#);
    let edits = [("seconds", "45"), ("aspect", "16:9"), ("voice", "none"), ("music", "library"), ("media", "library"), ("authorship", "director"), ("style", "bold type"), ("prompt", "launch video, longer")];
    expect(
        turn(tool, prefill, &edits),
        r#"{"prompt": "launch video, longer", "aspect": "16:9", "seconds": 45, "voice": "none", "music": "library", "media": "library", "authorship": "director", "style": "bold type"}"#,
    );
}

#[test]
fn add_subtitles_card_edit_confirm() {
    expect(turn("add_subtitles", r#"{"voice": "vo1"}"#, &[("voice", "vo2")]), r#"{"voice": "vo2"}"#);
}

#[test]
fn speak_card_edit_confirm() {
    // "Voice the lines": the chip prefills the voiceover; the voice is picked on the card.
    expect(turn("speak", r#"{"node": "voice1"}"#, &[("voice", "bm_daniel")]), r#"{"node": "voice1", "voice": "bm_daniel"}"#);
    expect(turn("speak", r#"{"node": "voice1"}"#, &[]), r#"{"node": "voice1"}"#);
}

#[test]
fn generate_over_selection_cards_edit_confirm() {
    // Selected clip, no Generate yet: generate_add prefilled with the clip.
    expect(
        turn("generate_add", r#"{"clip": "c1"}"#, &[("prompt", "the hands become birds"), ("preset", "bundled:image-to-video"), ("seed", "5")]),
        r#"{"clip": "c1", "prompt": "the hands become birds", "preset": "bundled:image-to-video", "seed": 5}"#,
    );
    // An in-out range.
    expect(turn("generate_add", r#"{"start": 2.5, "length": 4}"#, &[("prompt", "rain")]), r#"{"start": 2.5, "length": 4, "prompt": "rain"}"#);
    // Control-R on a segment that needs takes: generate_run for its node.
    expect(turn("generate_run", r#"{"node": "gen1", "takes": 1}"#, &[("takes", "3"), ("preview", "yes")]), r#"{"node": "gen1", "takes": 3, "preview": true}"#);
    // The Stop chip while it runs.
    expect(turn("generate_cancel", r#"{"node": "gen1"}"#, &[]), r#"{"node": "gen1"}"#);
}

#[test]
fn new_video_cancel_card_confirm() {
    // The Stop chip while a song video runs: no arguments.
    expect(turn("new_video_cancel", "{}", &[]), "{}");
}

#[test]
fn set_voice_lines_and_subtitles_cards() {
    // The lines are not on the card (typing them stays in the inspector); they pass through.
    let (tool, prefill) = ("set_voice_lines", r#"{"node": "voice1", "lines": [{"text": "Hello", "at": 0}]}"#);
    expect(turn(tool, prefill, &[("node", "voice2")]), r#"{"node": "voice2", "lines": [{"text": "Hello", "at": 0}]}"#);
    expect(turn("add_subtitles", r#"{"voice": "voice1"}"#, &[]), r#"{"voice": "voice1"}"#);
}

#[test]
fn storyboard_update_card_edit_confirm() {
    expect(turn("storyboard_apply_changes", "{}", &[("close_gap", "yes")]), r#"{"close_gap": true}"#);
    expect(turn("storyboard_apply_changes", "{}", &[]), "{}");
}

/// The render dialog is gone: its every setting is an argument of the
/// card's tool, named as the Render button's code names them.
#[test]
fn render_cut_card_edit_confirm() {
    expect(turn("render_cut", r#"{}"#, &[("quality", "draft")]), r#"{"quality": "draft"}"#);
    expect(turn("render_cut", r#"{}"#, &[]), "{}");
    expect(
        turn("render_cut", r#"{"quality": "final", "path": "~/Movies/Launch.mp4"}"#,
            &[("fps", "24"), ("blend", "yes"), ("bitrate", "high"), ("loudness", "social"), ("burn_captions", "no"), ("publish", "yes")],
        ),
        r#"{"quality": "final", "fps": 24, "blend": true, "bitrate": "high", "loudness": "social", "burn_captions": false, "path": "~/Movies/Launch.mp4", "publish": true}"#,
    );
    let spec = card_for("render_cut", &Value::Obj(Vec::new())).unwrap();
    assert_eq!((spec.title.as_str(), spec.confirm.as_str(), spec.fields.len(), spec.fixed.len()), ("Render the cut", "Render", 8, 0));
    let error = turn("render_cut", r#"{}"#, &[("fps", "0")]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidArgs);
}

/// `export_cut` was a second way to say "write the cut to a file": the
/// render card's `path` does it, so the tool is gone.
#[test]
fn export_to_a_file_is_the_render_card_not_a_second_tool() {
    assert!(makepad_studio_api::catalog::find("export_cut").is_none());
    let schema = makepad_studio_api::catalog::find("render_cut").unwrap().input_schema();
    assert!(schema.get("properties").and_then(|properties| properties.get("path")).is_some());
}

/// The web export dialog's options: folder, device, pack and its knobs,
/// the film.js sections, music, network, credits.
#[test]
fn export_web_card_edit_confirm() {
    expect(turn("export_web", r#"{}"#, &[]), "{}");
    expect(
        turn("export_web", r#"{"out_dir": "~/Movies/Launch web"}"#,
            &[("preset", "mobile"), ("pack", "app"), ("fonts", "full"), ("shaders", "keep_compiler"), ("kernels", "aot_only"), ("assets", "external"), ("include_music", "no"), ("lan", "yes"), ("credits", "Song: A"), ("strip_js", "clipboard, history")],
        ),
        r#"{"out_dir": "~/Movies/Launch web", "preset": "mobile", "pack": "app", "fonts": "full", "shaders": "keep_compiler", "kernels": "aot_only", "assets": "external", "strip_js": ["clipboard", "history"], "include_music": false, "lan": true, "credits": "Song: A"}"#,
    );
    let spec = card_for("export_web", &Value::Obj(Vec::new())).unwrap();
    assert!(spec.fixed.is_empty() && spec.confirm == "Export", "{spec:?}");
}

/// New cut: the format is a pick; the last format is the host's default.
#[test]
fn new_cut_card_edit_confirm() {
    expect(turn("new_cut", r#"{}"#, &[]), "{}");
    expect(turn("new_cut", r#"{"format": "9:16"}"#, &[("label", "Launch"), ("duration", "15")]), r#"{"format": "9:16", "label": "Launch", "duration": 15}"#);
    expect(turn("new_cut", r#"{"reframe": true}"#, &[("format", "9:16")]), r#"{"format": "9:16", "reframe": true}"#);
    expect(turn("new_cut", r#"{}"#, &[("width", "1080"), ("height", "1920")]), r#"{"width": 1080, "height": 1920}"#);
}

/// Project settings: the rare ones (name, length, tempo) as a card; the
/// output object passes through.
#[test]
fn set_header_card_edit_confirm() {
    expect(turn("set_header", r#"{"label": "Cut A"}"#, &[("bpm", "96"), ("duration", "42.5")]), r#"{"label": "Cut A", "duration": 42.5, "bpm": 96}"#);
    let spec = card_for("set_header", &schema::parse(r#"{"output": {"width": 1080, "height": 1920, "fps": 30}}"#).unwrap()).unwrap();
    assert_eq!((spec.title.as_str(), spec.confirm.as_str(), spec.fixed.len()), ("Project settings", "Apply", 1));
}

/// Grab from the web: the address and the range are lines, `stream` a pick;
/// `in` and `out` stay as typed (seconds or m:ss).
#[test]
fn web_grab_card_edit_confirm() {
    expect(
        turn("web_grab_start", r#"{"url": "https://youtu.be/abc"}"#, &[("in", "0:30"), ("out", "1:10")]),
        r#"{"url": "https://youtu.be/abc", "in": "0:30", "out": "1:10"}"#,
    );
    expect(turn("web_grab_start", r#"{"url": "https://youtu.be/abc"}"#, &[("stream", "yes")]), r#"{"url": "https://youtu.be/abc", "stream": true}"#);
    let error = turn("web_grab_start", r#"{}"#, &[]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidArgs);
    expect(turn("web_stream_snapshot", r#"{}"#, &[("job", "3")]), r#"{"job": 3}"#);
}

#[test]
fn score_new_card_edit_confirm() {
    expect(turn("score_new", r#"{"template": "techno"}"#, &[("bpm", "128")]), r#"{"template": "techno", "bpm": 128}"#);
}

/// The arguments the host's `score_batch` reads (`batch_spec`): prompt,
/// markers (kept as the model gave them), seconds (a pick of the lengths),
/// count.
#[test]
fn score_batch_card_edit_confirm() {
    let (tool, prefill) = ("score_batch", r#"{"prompt": "night drive", "markers": ["synthwave", "dark"], "seconds": 60}"#);
    expect(
        turn(tool, prefill, &[("seconds", "120"), ("count", "3")]),
        r#"{"prompt": "night drive", "markers": ["synthwave", "dark"], "seconds": 120, "count": 3}"#,
    );
    expect(turn("score_batch", r#"{"prompt": "calm"}"#, &[]), r#"{"prompt": "calm"}"#);
}

#[test]
fn score_learn_card_confirms_with_no_arguments() {
    expect(turn("score_learn", r#"{}"#, &[]), "{}");
}

#[test]
fn score_render_card_edit_confirm() {
    let (tool, prefill) = ("score_render", r#"{"title": "Theme", "seconds": 45}"#);
    expect(turn(tool, prefill, &[("seconds", "90"), ("seed", "7")]), r#"{"title": "Theme", "seconds": 90, "seed": 7}"#);
    let spec = card_for("score_render", &schema::parse(r#"{"title": "T", "source": "Score{}"}"#).unwrap()).unwrap();
    assert_eq!((spec.title.as_str(), spec.confirm.as_str()), ("Render to audio", "Render"));
    assert_eq!(spec.fixed.len(), 1, "the source passes through unshown: {:?}", spec.fixed);
}

#[test]
fn a_missing_required_field_is_the_checkers_error_not_a_panic() {
    let error = turn("generate_run", r#"{}"#, &[]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidArgs);
    assert!(error.message.contains("`node` must not be empty"), "{}", error.message);
}

#[test]
fn fixed_arguments_pass_through() {
    // `wait` (hidden boolean) passes through fixed; `inputs` (array of strings) is an
    // editable comma line and each key of the prefilled `params` map its own field.
    let (tool, prefill) = ("gen_run", r#"{"preset": "p", "inputs": ["a", "b"], "params": {"seconds": 8}, "wait": false}"#);
    let spec = card_for("gen_run", &schema::parse(prefill).unwrap()).unwrap();
    assert_eq!(spec.fixed.len(), 1, "only wait should be fixed: {:?}", spec.fixed);
    expect(turn(tool, prefill, &[]), prefill);
}

#[test]
fn prefill_selects_options_and_fills_lines() {
    let spec = card_for("make_video", &schema::parse(r#"{"prompt": "p", "aspect": "1:1", "music": "library", "seconds": 12.5, "bogus": 1, "style": null}"#).unwrap()).unwrap();
    let values = spec.values();
    let Field::Pick { options, selected, arg, .. } = &spec.fields[1] else { panic!("{:?}", spec.fields) };
    assert_eq!((arg.as_str(), options[*selected].0.as_str()), ("aspect", "1:1"));
    assert!(spec.fixed.is_empty());
    assert_eq!(values.lines, vec!["p".to_string(), "12.5".to_string(), String::new(), String::new()]);
    // Required first (layout order), and optional enums lead with "default".
    assert_eq!(spec.fields[0].arg(), "prompt");
    assert!(matches!(&spec.fields[1], Field::Pick { options, .. } if options[0] == ("default".to_string(), None)));
    assert_eq!(spec.confirm, "Make it");
    assert_eq!(card_for("timeline", &Value::Null).unwrap().confirm, "Make it");
}

// ---------------------------------------------------------------- (d)

#[test]
fn an_optional_enum_left_at_default_is_omitted() {
    // No template said: the pick stays at "default" and the arg is left out.
    let got = turn("score_new", r#"{"bpm": 100}"#, &[]).unwrap();
    assert!(got.get("template").is_none() && got.get("bpm") == Some(&Value::Int(100)), "{}", got.to_json());
    // Choosing and then going back to "default" leaves it out again.
    let got = turn("score_new", r#"{"template": "techno"}"#, &[("template", "default")]).unwrap();
    assert!(same(&got, &Value::Obj(Vec::new())), "{}", got.to_json());
    // An empty optional line is omitted too; a cleared one as well.
    let got = turn("score_new", r#"{"label": "night"}"#, &[("label", "")]).unwrap();
    assert!(got.get("label").is_none(), "{}", got.to_json());
}

#[test]
fn score_batch_markers_string_array_is_editable_line() {
    // Prefilled markers array shows as a comma-separated line
    let spec = card_for("score_batch", &schema::parse(r#"{"prompt": "calm", "markers": ["lofi", "warm"]}"#).unwrap()).unwrap();
    let markers_field = spec.fields.iter().find(|f| f.arg() == "markers").expect("markers field exists");
    match markers_field {
        Field::Line { value, .. } => assert_eq!(value, "lofi, warm"),
        other => panic!("expected Field::Line, got {:?}", other),
    }

    // Editing the line splits and trims: "lofi, , dark " -> ["lofi", "dark"]
    let result = turn("score_batch", r#"{"prompt": "calm", "markers": ["lofi", "warm"]}"#, &[("markers", "lofi, , dark ")]).unwrap();
    let markers = result.get("markers").expect("markers in result").as_arr().expect("markers is array");
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0].as_str(), Some("lofi"));
    assert_eq!(markers[1].as_str(), Some("dark"));
}

#[test]
fn score_batch_empty_markers_becomes_empty_array() {
    // Optional array: empty line is omitted
    let got = turn("score_batch", r#"{"prompt": "calm", "markers": ["lofi"]}"#, &[("markers", "")]).unwrap();
    assert!(got.get("markers").is_none(), "{}", got.to_json());
}

#[test]
fn the_editor_cards_hide_the_models_own_arguments_and_take_the_element() {
    // The graphic desk's "Make the editor" chip: the card asks what the editor is like and for which element.
    let spec = card_for("editor_make", &schema::parse("{}").unwrap()).unwrap();
    assert_eq!(spec.title, "Make the editor");
    assert_eq!(spec.fields.iter().map(Field::arg).collect::<Vec<_>>(), ["brief", "element", "quick", "doc_only"]);
    let got = turn("editor_make", r#"{}"#, &[("element", "the horn"), ("brief", "like a mixing desk")]).unwrap();
    expect(Ok(got), r#"{"brief": "like a mixing desk", "element": "the horn"}"#);
    // The film's editor chip.
    let spec = card_for("cut_panel_make", &schema::parse("{}").unwrap()).unwrap();
    assert_eq!((spec.title.as_str(), spec.fields.iter().map(Field::arg).collect::<Vec<_>>()), ("Make the film's editor", vec!["brief"]));
    // Stopping a batch is a plain tool of the SCORE page, not a card.
    assert!(find("score_batch_stop").is_some_and(|spec| !spec.request && spec.mutates));
}

// ---------------------------------------------------------------- music bake

#[test]
fn music_bake_card_edit_confirm() {
    let id = "0123456789abcdef0123456789abcdef";
    // The model names the track and the parts; the person adds stems on the line and confirms.
    let got = turn("music_bake", &format!(r#"{{"asset": "{id}", "what": ["beats", "lyrics"]}}"#), &[("what", "beats, lyrics, stems")]).unwrap();
    expect(Ok(got), &format!(r#"{{"asset": "{id}", "what": ["beats", "lyrics", "stems"]}}"#));
    // The card opens with the track and a stop switch the person can flip.
    let spec = card_for("music_bake", &schema::parse(&format!(r#"{{"asset": "{id}"}}"#)).unwrap()).unwrap();
    assert_eq!(spec.title, "Bake a track");
    assert_eq!(spec.fields.iter().map(Field::arg).collect::<Vec<_>>(), ["asset", "what", "stop"]);
    let got = turn("music_bake", &format!(r#"{{"asset": "{id}"}}"#), &[("stop", "yes")]).unwrap();
    expect(Ok(got), &format!(r#"{{"asset": "{id}", "stop": true}}"#));
    // No track: the checker's error, not a card.
    assert!(turn("music_bake", r#"{}"#, &[]).is_err());
}

// ---------------------------------------------------------------- VJ palette

#[test]
fn vj_palette_set_card_edit_confirm() {
    // The model offers four colours; the person changes the follow mode and the fade.
    let (tool, prefill) = ("vj_palette_set", r##"{"colors": ["#101020", "#ff2060", "#20c0ff", "#ffe000"], "fade": 2}"##);
    let spec = card_for("vj_palette_set", &schema::parse(prefill).unwrap()).unwrap();
    assert_eq!(spec.title, "Show palette");
    assert_eq!(spec.fields[0].arg(), "colors");
    match &spec.fields[0] {
        Field::Line { value, .. } => assert_eq!(value, "#101020, #ff2060, #20c0ff, #ffe000"),
        other => panic!("{other:?}"),
    }
    let got = turn(tool, prefill, &[("follow", "manual"), ("fade", "0")]).unwrap();
    expect(Ok(got), r##"{"colors": ["#101020", "#ff2060", "#20c0ff", "#ffe000"], "follow": "manual", "fade": 0}"##);
    // A row place and a seed stay numbers and text.
    let got = turn("vj_palette_set", r##"{"suggestion": 2, "seed": "#00ffaa"}"##, &[("suggestion", "5")]).unwrap();
    expect(Ok(got), r##"{"suggestion": 5, "seed": "#00ffaa"}"##);
}

#[test]
fn vj_palette_animate_card_keeps_the_keyframes_and_edits_the_loop() {
    let (tool, prefill) = ("vj_palette_animate", r##"{"bars": 4, "keys": [{"lane": "primary", "beat": 0, "color": "#ff0000"}], "follow": true}"##);
    let got = turn(tool, prefill, &[("bars", "8"), ("snap", "half"), ("clear", "yes")]).unwrap();
    expect(Ok(got), r##"{"bars": 8, "snap": "half", "clear": true, "follow": true, "keys": [{"lane": "primary", "beat": 0, "color": "#ff0000"}]}"##);
    // The loop is a dropdown of the allowed lengths.
    let spec = card_for("vj_palette_animate", &schema::parse("{}").unwrap()).unwrap();
    match spec.fields.iter().find(|field| field.arg() == "bars") {
        Some(Field::Pick { options, .. }) => assert_eq!(options.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>(), ["default", "1", "2", "4", "8", "16", "32"]),
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------- DJ

#[test]
fn dj_set_list_card_edit_confirm() {
    let a = "00000000000000000000000000000001";
    let b = "00000000000000000000000000000002";
    // The model proposes an add; the person picks another list and drops a track from the line.
    let prefill = format!(r#"{{"action": "add", "list": "Night", "tracks": ["{a}", "{b}"]}}"#);
    let got = turn("dj_set_list", &prefill, &[("tracks", a)]).unwrap();
    expect(Ok(got), &format!(r#"{{"action": "add", "list": "Night", "tracks": ["{a}"]}}"#));
    // The action is a dropdown of the verbs and comes first.
    let spec = card_for("dj_set_list", &schema::parse(r#"{"action": "move", "from": 3, "to": 0}"#).unwrap()).unwrap();
    assert_eq!(spec.title, "Set list");
    match &spec.fields[0] {
        Field::Pick { arg, options, selected, .. } => {
            assert_eq!(arg, "action");
            assert_eq!(options[*selected].0, "move");
            assert_eq!(options.len(), 8);
        }
        other => panic!("{other:?}"),
    }
    let got = turn("dj_set_list", r#"{"action": "move", "from": 3, "to": 0}"#, &[("to", "1")]).unwrap();
    expect(Ok(got), r#"{"action": "move", "from": 3, "to": 1}"#);
    // An action is required: the dropdown opens on the first verb, so a bare card makes a list.
    let got = turn("dj_set_list", r#"{}"#, &[("name", "Warm up")]).unwrap();
    expect(Ok(got), r#"{"action": "create", "name": "Warm up"}"#);
}

#[test]
fn dj_auto_card_edit_confirm() {
    let got = turn("dj_auto", r#"{"on": true, "secs": 8}"#, &[("eq", "yes"), ("secs", "12")]).unwrap();
    expect(Ok(got), r#"{"on": true, "secs": 12, "eq": true}"#);
    // Nothing set: the card confirms with no arguments (the tool then only reports).
    let got = turn("dj_auto", r#"{}"#, &[]).unwrap();
    assert!(same(&got, &Value::Obj(Vec::new())), "{}", got.to_json());
}

mod registry_session {
    use makepad_studio_api::{beats, CallResult, ErrorKind, Registry, ToolError, ToolOutput};
    use makepad_strict_json::{self as json, Value};

    /// The stand-in cut: blocks `(id, start, length)`, its revision and tempo.
    #[derive(Default)]
    struct Fake {
        open: bool,
        revision: i64,
        bpm: f64,
        blocks: Vec<(String, f64, f64)>,
        library: Vec<String>,
    }

    type Handler = fn(&mut Fake, &Value) -> CallResult;

    fn ok(value: Value) -> CallResult {
        Ok(ToolOutput::json(value))
    }

    fn changed(fake: &mut Fake) -> CallResult {
        fake.revision += 1;
        ok(json::obj(vec![("revision", Value::Int(fake.revision)), ("diff", json::s("@@ …"))]))
    }

    fn num(value: Option<&Value>) -> f64 {
        match value {
            Some(Value::Int(value)) => *value as f64,
            Some(Value::F64(value)) => *value,
            _ => 0.0,
        }
    }

    fn registry() -> Registry<Handler> {
        let mut registry: Registry<Handler> = Registry::new();
        registry.register("new_cut", |fake, args| {
            fake.open = true;
            fake.bpm = num(args.get("bpm")).max(1.0);
            fake.blocks.clear();
            changed(fake)
        });
        registry.register("gen_run", |fake, args| {
            let preset = args.get("preset").and_then(Value::as_str).unwrap_or("");
            if !preset.starts_with("studio:flux") {
                return Err(ToolError::refused(format!("no preset `{preset}`")));
            }
            let id = format!("ast_{:026}", fake.library.len() + 1);
            fake.library.push(id.clone());
            ok(json::obj(vec![("id", json::s("1")), ("phase", json::s("done")), ("assets", Value::Arr(vec![json::obj(vec![("asset_id", json::s(id)), ("revision", json::s("arev_1"))])]))]))
        });
        registry.register("add_clip", |fake, args| {
            if !fake.open {
                return Err(ToolError::not_found("no cut is open in the CUT page: call new_cut"));
            }
            let media = args.get("media").and_then(|media| media.get("asset_id")).and_then(Value::as_str).unwrap_or("");
            if !fake.library.iter().any(|id| id == media) {
                return Err(ToolError::not_found(format!("no asset `{media}`")));
            }
            let id = format!("still{}", fake.blocks.len() + 1);
            fake.blocks.push((id, num(args.get("start")), num(args.get("length")).max(0.1)));
            changed(fake)
        });
        registry.register("snap_to_beats", |fake, _| {
            let grid = beats::Grid { bpm: fake.bpm, offset: 0.0, beats_per_bar: 4 };
            for (id, start, length) in beats::snap_spans(&fake.blocks, &grid, beats::Unit::Beat, beats::Edges::Both) {
                if let Some(block) = fake.blocks.iter_mut().find(|block| block.0 == id) {
                    *block = (id, start, length);
                }
            }
            changed(fake)
        });
        registry.register("timeline", |fake, _| {
            ok(json::obj(vec![("blocks", Value::Arr(fake.blocks.iter().map(|(id, start, length)| json::obj(vec![("node", json::s(id.clone())), ("span", Value::Arr(vec![Value::F64(*start), Value::F64(*length)]))])).collect()))]))
        });
        registry.register("inspect", |fake, _| {
            let warnings: Vec<Value> = if fake.blocks.is_empty() { vec![json::s("the cut is empty")] } else { Vec::new() };
            ok(json::obj(vec![("errors", Value::Arr(Vec::new())), ("warnings", Value::Arr(warnings))]))
        });
        registry
    }

    fn dispatch(registry: &Registry<Handler>, fake: &mut Fake, name: &str, args: Value) -> CallResult {
        let checked = registry.check(name, args)?;
        (checked.handler)(fake, &checked.args)
    }

    /// The registry session: create a cut, generate an image
    /// with a preview-generator job, place it, try a Motion graphic, snap to
    /// the beat, inspect, ask for a strip. Returns what each step said.
    fn script(call: &mut dyn FnMut(&str, Value) -> CallResult) {
        let parse = |text: &str| makepad_studio_api::schema::parse(text).unwrap();
        let edit = call("new_cut", parse(r#"{"label": "Launch", "width": 1080, "height": 1920, "fps": 30, "duration": 12, "bpm": 120}"#)).expect("new_cut");
        assert_eq!(edit.json.get("revision"), Some(&Value::Int(1)));
        let job = call("gen_run", parse(r#"{"preset": "studio:flux-schnell-fast", "prompt": "orange poster", "params": {"width": 576, "height": 1024}}"#)).expect("gen_run");
        let image = job.json.get("assets").and_then(Value::as_arr).and_then(|items| items.first()).cloned().expect("a published image");
        let add = call("add_clip", json::obj(vec![("media", image.clone()), ("kind", json::s("image")), ("start", Value::F64(0.1)), ("length", Value::F64(3.8))])).expect("add_clip");
        assert_eq!(add.json.get("revision"), Some(&Value::Int(2)));
        // A bad call is refused with the field and the fix, and changes nothing.
        let bad = call("add_clip", json::obj(vec![("media", image), ("kind", json::s("image")), ("start", json::s("2.5s"))])).unwrap_err();
        assert_eq!(bad.kind, ErrorKind::InvalidArgs);
        assert!(bad.message.contains("without quotes: 2.5"), "{}", bad.message);
        let graphic = call("add_graphic", parse(r#"{"motion": {"asset_id": "ast_x", "revision": "arev_x"}, "start": 1}"#)).unwrap_err();
        assert_eq!(graphic.kind, ErrorKind::NotAvailable);
        assert!(graphic.message.contains("M1"), "{}", graphic.message);
        call("snap_to_beats", parse(r#"{"to": "beat"}"#)).expect("snap");
        let timeline = call("timeline", Value::Null).expect("timeline");
        let span = timeline.json.get("blocks").and_then(Value::as_arr).and_then(|blocks| blocks.first()).and_then(|block| block.get("span")).cloned();
        assert_eq!(span, Some(Value::Arr(vec![Value::F64(0.0), Value::F64(4.0)])), "snapped to 0 s and 4 s at 120 bpm");
        let inspect = call("inspect", Value::Null).expect("inspect");
        assert_eq!(inspect.json.get("errors"), Some(&Value::Arr(Vec::new())));
        let strip = call("strip", Value::Null).unwrap_err();
        assert_eq!(strip.kind, ErrorKind::NotAvailable);
        let unknown = call("render_movie", Value::Null).unwrap_err();
        assert_eq!(unknown.kind, ErrorKind::UnknownTool);
    }

    #[test]
    fn one_session_through_the_registry() {
        let registry = registry();
        let mut fake = Fake::default();
        script(&mut |name, args| dispatch(&registry, &mut fake, name, args));
    }

    /// The generation tools are the shared gen.* tools, byte for byte: the
    /// same description and argument schema Sandbox's game chat offers.
    #[test]
    fn generation_tools_are_the_shared_gen_tools_byte_for_byte() {
        for shared in makepad_gen_tools::TOOLS {
            let spec = makepad_studio_api::catalog::TOOLS.iter().find(|spec| spec.name == shared.api_name)
                .unwrap_or_else(|| panic!("{} is not registered", shared.api_name));
            assert_eq!(spec.description, shared.description, "{}", shared.name);
            assert_eq!(spec.input, shared.input, "{}", shared.name);
        }
        assert!(!makepad_studio_api::catalog::TOOLS.iter().any(|spec| spec.name.starts_with("flow_")), "the flow_* job tools are gone");
    }
}

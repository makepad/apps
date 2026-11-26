//! Tools as the agent sees them: the host's tools (Stage's control API,
//! by family: edit, motion, assets and generation, app runs, audio,
//! feedback) plus three the agent keeps for itself — `storyboard` (the
//! plan the panel shows), `critique` (what the model saw and will change)
//! and `finish` (refused until the work is clean).
//!
//! Every backend delivers a call as a name and its arguments as text (the
//! Claude Code and hub protocols write parameters as raw text; the Claude
//! API's JSON input is flattened the same way, see [`Args::from_json`]), so
//! a Splash document never needs JSON escaping. [`Args::to_json`] types
//! them back by the tool's schema before the host sees them, and says
//! exactly what is wrong in words the model can act on.

use makepad_strict_json::{self as json, Value};

/// One tool as the model is told about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// A JSON-schema object, as text.
    pub parameters: String,
    /// It changes a document (the cut, a Motion, the library).
    pub mutates: bool,
    /// It answers with pictures (frame, strip, onion).
    pub images: bool,
    /// It is an inspect: its answer says whether the work is clean.
    pub inspects: bool,
}

impl ToolDef {
    pub fn new(name: &str, description: &str, parameters: &str) -> Self {
        ToolDef { name: name.into(), description: description.into(), parameters: parameters.into(), mutates: false, images: false, inspects: false }
    }

    pub fn mutating(mut self) -> Self {
        self.mutates = true;
        self
    }

    pub fn with_images(mut self) -> Self {
        self.images = true;
        self
    }

    pub fn inspecting(mut self) -> Self {
        self.inspects = true;
        self
    }
}

/// A scene of the storyboard.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub id: String,
    pub start: f64,
    pub length: f64,
    /// The one idea the scene carries.
    pub idea: String,
    /// What is on screen and how it moves.
    pub look: String,
    /// The voiceover line spoken over it.
    pub voice: String,
    /// Where it sits in the music: `bars 1–4 (intro)`, `on the drop`.
    pub music: String,
    /// Media it needs generated or found: `b-roll: a phone on a desk`.
    pub media: String,
    /// Its layer stack, back to front: plate, illustration or instanced
    /// detail, type, 3D; with the named kits each layer uses.
    pub stack: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Storyboard {
    pub title: String,
    /// Palette, type and motion language, in a few lines.
    pub style: String,
    /// The style kit it builds on (`@swiss`, `@neon_noir`, …), if any.
    pub kit: String,
    pub scenes: Vec<Scene>,
}

impl Storyboard {
    pub fn duration(&self) -> f64 {
        self.scenes.iter().map(|scene| scene.start + scene.length).fold(0.0, f64::max)
    }
}

/// What the model saw and what it will do about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Critique {
    /// What it looked at: `strip of the cut`, `scene hook`.
    pub subject: String,
    /// The problems, one per line, most important first.
    pub issues: Vec<String>,
    /// `revise` or `ship`.
    pub verdict: String,
}

/// The agent's own tools.
#[derive(Clone, Debug, PartialEq)]
pub enum Own {
    Storyboard(Storyboard),
    Critique(Critique),
    Finish { summary: String },
    /// Stop and ask the person to choose (interactive generations only).
    Ask { question: String, options: Vec<String> },
}

pub const STORYBOARD: &str = "storyboard";
pub const CRITIQUE: &str = "critique";
pub const FINISH: &str = "finish";
pub const ASK: &str = "ask";

/// The agent's own tools, as the model reads them.
pub fn own_tools() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            STORYBOARD,
            "Plan the video before writing anything, and re-plan when the person asks for changes. Pick a style kit from the menus (`kit`) and say how its palette, type and motion language are used (`style`). Give the scenes on the cut's timeline, each carrying ONE idea, cut on downbeats and timed to the voiceover lines and the music's sections. Give each scene's layer stack, back to front — plate, illustration or instanced detail, type, 3D where it fits — naming the menu items it uses (`stack`, e.g. \"plate: @neon_noir plate · detail: Repeat dot grid + icon draw-on · type: Anton @mask_up · 3D: Text3D @chrome_title\"), and the media it needs generated or found. Shown to the person as the storyboard.",
            r#"{"type": "object", "properties": {"title": {"type": "string"}, "kit": {"type": "string", "description": "the style kit, e.g. @swiss, or none"}, "style": {"type": "string", "description": "palette (hex), typefaces and weights, motion language, in a few lines"}, "scenes": {"type": "array", "description": "[{id, start, length, idea, stack, look, voice, music, media}]; seconds on the cut's timeline", "items": {"type": "object", "properties": {"id": {"type": "string"}, "start": {"type": "number"}, "length": {"type": "number"}, "idea": {"type": "string"}, "stack": {"type": "string", "description": "plate · detail · type · 3D, with the named kits"}, "look": {"type": "string"}, "voice": {"type": "string"}, "music": {"type": "string"}, "media": {"type": "string"}}, "required": ["id", "start", "length", "idea", "stack"]}}}, "required": ["kit", "scenes"]}"#,
        ),
        ToolDef::new(
            CRITIQUE,
            "After looking at pictures (a strip, frames, an onion trail) or an inspect report, record what you see against the design rules — hierarchy, legibility, safe zones, pacing against the voice and the beat, easing, contrast, one idea per scene — and whether you revise or ship. Shown to the person; be specific (scene, time, layer).",
            r#"{"type": "object", "properties": {"subject": {"type": "string", "description": "what you looked at"}, "issues": {"type": "array", "items": {"type": "string"}, "description": "problems, most important first; empty when there are none"}, "verdict": {"type": "string", "enum": ["revise", "ship"]}}, "required": ["subject", "issues", "verdict"]}"#,
        ),
        ToolDef::new(
            ASK,
            "Stop and ask the person to choose before you go on: which treatment, which character look, whether the test shot works. Give the options you prepared (on the storyboard too). End your reply after this call; their answer comes as the next message.",
            r#"{"type": "object", "properties": {"question": {"type": "string"}, "options": {"type": "array", "items": {"type": "string"}, "description": "the choices, each one line"}}, "required": ["question"]}"#,
        ),
        ToolDef::new(
            FINISH,
            "End the work with a short summary for the person. Refused while the last inspect of the cut reports errors, while anything changed since it was inspected, or (for a model that sees pictures) before you looked at the finished edit.",
            r#"{"type": "object", "properties": {"summary": {"type": "string"}}, "required": ["summary"]}"#,
        ),
    ]
}

/// A call's arguments as text, in the order they came.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args(pub Vec<(String, String)>);

impl Args {
    /// A JSON object's fields: strings as they are, anything else as its
    /// JSON text (typed back by [`Args::to_json`]).
    pub fn from_json(text: &str) -> Result<Args, String> {
        let value = json::parse_depth(text.trim().as_bytes(), 32).map_err(|error| format!("the arguments are not a JSON object ({error})"))?;
        let Value::Obj(fields) = value else { return Err("the arguments are not a JSON object".into()) };
        Ok(Args(
            fields
                .into_iter()
                .map(|(key, value)| {
                    let text = match value {
                        Value::Str(text) => text,
                        other => other.to_json(),
                    };
                    (key, text)
                })
                .collect(),
        ))
    }

    pub fn raw(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str())
    }

    fn text(&self, key: &str) -> String {
        self.raw(key).map(|value| value.trim().to_string()).unwrap_or_default()
    }

    /// The arguments as a JSON object typed by the tool's schema: numbers,
    /// booleans, arrays and objects parsed from their text, strings kept
    /// verbatim. A missing required argument or a value that is not what
    /// the schema says is an error naming the argument.
    pub fn to_json(&self, schema: &str) -> Result<Value, String> {
        let schema = json::parse_depth(schema.as_bytes(), 32).unwrap_or(Value::Null);
        let properties = schema.get("properties");
        let mut fields = Vec::new();
        for (key, raw) in &self.0 {
            let ty = properties.and_then(|p| p.get(key)).and_then(|p| p.get("type")).and_then(Value::as_str);
            fields.push((key.clone(), typed(key, raw, ty)?));
        }
        if let Some(required) = schema.get("required").and_then(Value::as_arr) {
            for key in required.iter().filter_map(Value::as_str) {
                if self.raw(key).is_none() {
                    return Err(format!("`{key}` is missing"));
                }
            }
        }
        Ok(Value::Obj(fields))
    }
}

/// One argument's text as the type its schema names.
fn typed(key: &str, raw: &str, ty: Option<&str>) -> Result<Value, String> {
    let trimmed = raw.trim();
    let number = || trimmed.trim_end_matches('s').trim().parse::<f64>().ok().filter(|v| v.is_finite());
    Ok(match ty {
        Some("string") => Value::Str(raw.to_string()),
        Some("number") => Value::F64(number().ok_or_else(|| format!("`{key}` must be a number, not `{trimmed}`"))?),
        Some("integer") => match number() {
            Some(v) if v.fract() == 0.0 && v.abs() < 1e15 => Value::Int(v as i64),
            _ => return Err(format!("`{key}` must be a whole number, not `{trimmed}`")),
        },
        Some("boolean") => match trimmed {
            "true" | "1" | "yes" => Value::Bool(true),
            "false" | "0" | "no" => Value::Bool(false),
            _ => return Err(format!("`{key}` must be true or false, not `{trimmed}`")),
        },
        Some(ty @ ("array" | "object")) => {
            let value = json::parse_depth(trimmed.as_bytes(), 32).map_err(|error| format!("`{key}` must be a JSON {ty} ({error})"))?;
            match (&value, ty) {
                (Value::Arr(_), "array") | (Value::Obj(_), "object") => value,
                _ => return Err(format!("`{key}` must be a JSON {ty}")),
            }
        }
        // Untyped: JSON when it reads as a non-string value, else text.
        _ => match json::parse_depth(trimmed.as_bytes(), 32) {
            Ok(value @ (Value::Int(_) | Value::F64(_) | Value::Bool(_) | Value::Arr(_) | Value::Obj(_))) => value,
            _ => Value::Str(raw.to_string()),
        },
    })
}

/// Seconds as the model wrote them, `s` allowed.
fn seconds(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(*value as f64),
        Value::F64(value) => Some(*value),
        Value::Str(text) => text.trim().trim_end_matches('s').trim().parse().ok(),
        _ => None,
    }
}

fn storyboard(args: &Args) -> Result<Storyboard, String> {
    let raw = args.raw("scenes").ok_or("`scenes` is missing")?;
    let value = json::parse_depth(raw.trim().as_bytes(), 16)
        .map_err(|error| format!("`scenes` is not a JSON array ({error}); write it as [{{\"id\": \"hook\", \"start\": 0, \"length\": 4, \"idea\": \"…\", \"look\": \"…\", \"voice\": \"…\"}}, …]"))?;
    let Value::Arr(items) = value else { return Err("`scenes` must be a JSON array of scene objects".into()) };
    if items.is_empty() {
        return Err("`scenes` is empty: a storyboard has at least one scene".into());
    }
    let mut scenes = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let field = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string();
        let id = field("id");
        let id = if id.is_empty() { format!("scene{}", index + 1) } else { id };
        let start = item.get("start").and_then(seconds).ok_or_else(|| format!("scene {} (`{id}`) has no `start` in seconds", index + 1))?;
        let length = item.get("length").and_then(seconds).ok_or_else(|| format!("scene {} (`{id}`) has no `length` in seconds", index + 1))?;
        if start < 0.0 || length <= 0.0 {
            return Err(format!("scene `{id}`: start must be ≥ 0 and length > 0 (got {start} and {length})"));
        }
        scenes.push(Scene { id, start, length, idea: field("idea"), look: field("look"), voice: field("voice"), music: field("music"), media: field("media"), stack: field("stack") });
    }
    Ok(Storyboard { title: args.text("title"), style: args.text("style"), kit: args.text("kit"), scenes })
}

fn critique(args: &Args) -> Result<Critique, String> {
    let issues = match args.raw("issues").map(str::trim) {
        None | Some("") | Some("[]") => Vec::new(),
        Some(raw) => match json::parse_depth(raw.as_bytes(), 8) {
            Ok(Value::Arr(items)) => items.iter().map(|item| item.as_str().map(str::to_string).unwrap_or_else(|| item.to_json())).collect(),
            // A list written as lines is fine too.
            _ => raw.lines().map(|line| line.trim().trim_start_matches(['-', '*', '•']).trim().to_string()).filter(|line| !line.is_empty()).collect(),
        },
    };
    let verdict = args.text("verdict").to_ascii_lowercase();
    if verdict != "revise" && verdict != "ship" {
        return Err(format!("`verdict` must be `revise` or `ship`, not `{verdict}`"));
    }
    Ok(Critique { subject: args.text("subject"), issues, verdict })
}

/// One of the agent's own calls, or `None` when the name is the host's.
pub fn parse_own(name: &str, args: &Args) -> Option<Result<Own, String>> {
    Some(match name {
        STORYBOARD => storyboard(args).map(Own::Storyboard),
        CRITIQUE => critique(args).map(Own::Critique),
        FINISH => Ok(Own::Finish { summary: args.text("summary") }),
        ASK => {
            let options = match args.raw("options").map(str::trim) {
                None | Some("") => Vec::new(),
                Some(raw) => match json::parse_depth(raw.as_bytes(), 8) {
                    Ok(Value::Arr(items)) => items.iter().map(|item| item.as_str().map(str::to_string).unwrap_or_else(|| item.to_json())).collect(),
                    _ => raw.lines().map(|line| line.trim().to_string()).filter(|line| !line.is_empty()).collect(),
                },
            };
            let question = args.text("question");
            if question.is_empty() { Err("`question` is empty".to_string()) } else { Ok(Own::Ask { question, options }) }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> Args {
        Args(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn own_schemas_are_json_objects() {
        for tool in own_tools() {
            let schema = json::parse_depth(tool.parameters.as_bytes(), 16).unwrap_or_else(|error| panic!("{}: {error}", tool.name));
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"), "{}", tool.name);
        }
        assert!(parse_own("write_motion", &Args::default()).is_none());
    }

    #[test]
    fn a_storyboard_reads_seconds_and_names_what_is_missing() {
        let Some(Ok(Own::Storyboard(board))) = parse_own(STORYBOARD, &args(&[("title", "Notes"), ("kit", "@swiss"), ("scenes", r#"[{"id":"hook","start":0,"length":"3.5s","idea":"one tap","music":"bars 1-2","stack":"plate: kit · type: @mask_up"},{"start":3.5,"length":4}]"#)])) else { panic!() };
        assert_eq!((board.kit.as_str(), board.scenes[0].stack.as_str()), ("@swiss", "plate: kit · type: @mask_up"));
        assert_eq!(board.scenes.len(), 2);
        assert_eq!(board.scenes[0].length, 3.5);
        assert_eq!(board.scenes[0].music, "bars 1-2");
        assert_eq!(board.scenes[1].id, "scene2");
        assert_eq!(board.duration(), 7.5);
        let error = parse_own(STORYBOARD, &args(&[("scenes", r#"[{"id":"a","start":0}]"#)])).unwrap().unwrap_err();
        assert!(error.contains("`a`") && error.contains("length"), "{error}");
        assert!(parse_own(STORYBOARD, &args(&[("scenes", "not json")])).unwrap().unwrap_err().contains("JSON array"));
    }

    #[test]
    fn a_critique_takes_json_or_lines() {
        let Some(Ok(Own::Critique(c))) = parse_own(CRITIQUE, &args(&[("subject", "strip"), ("issues", "- title too small\n- cut late"), ("verdict", "Revise")])) else { panic!() };
        assert_eq!(c.issues, vec!["title too small", "cut late"]);
        assert_eq!(c.verdict, "revise");
        assert!(parse_own(CRITIQUE, &args(&[("verdict", "maybe")])).unwrap().is_err());
    }

    #[test]
    fn arguments_are_typed_by_the_schema() {
        let schema = r#"{"type":"object","properties":{"target":{"type":"string"},"count":{"type":"integer"},"at":{"type":"number"},"loop":{"type":"boolean"},"keys":{"type":"array"}},"required":["target"]}"#;
        let value = args(&[("target", " 42 "), ("count", "8"), ("at", "1.5s"), ("loop", "true"), ("keys", "[1,2]"), ("extra", "3")]).to_json(schema).unwrap();
        assert_eq!(value.get("target"), Some(&Value::Str(" 42 ".into())));
        assert_eq!(value.get("count"), Some(&Value::Int(8)));
        assert_eq!(value.get("at"), Some(&Value::F64(1.5)));
        assert_eq!(value.get("loop"), Some(&Value::Bool(true)));
        assert_eq!(value.get("extra"), Some(&Value::Int(3)));
        assert!(args(&[("count", "8")]).to_json(schema).unwrap_err().contains("`target` is missing"));
        assert!(args(&[("target", "x"), ("count", "2.5")]).to_json(schema).unwrap_err().contains("whole number"));
        assert!(args(&[("target", "x"), ("keys", "{}")]).to_json(schema).unwrap_err().contains("JSON array"));
    }

    #[test]
    fn json_arguments_flatten_to_text() {
        let args = Args::from_json(r#"{"name": "a", "count": 3, "scenes": [{"id": "x"}]}"#).unwrap();
        assert_eq!(args.raw("name"), Some("a"));
        assert_eq!(args.raw("count"), Some("3"));
        assert_eq!(args.raw("scenes"), Some(r#"[{"id":"x"}]"#));
        assert!(Args::from_json("[1]").is_err());
    }
}

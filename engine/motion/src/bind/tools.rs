//! The bindable demo overlays and the calls the AI places them with
//! (EDITS.md §10.4, "bind overlays"): "highlight the save button when it's
//! clicked" becomes `bind_overlay{overlay: "highlight", clip: "demo",
//! target: "save", when: {call: "click", target: "save"}}`, which resolves
//! against the clip's run into a `Graphic` over the right stretch of the
//! edit, bound to the clip.
//!
//! The overlays read everything from the run (see the module docs of
//! [`crate::bind`]); the span a tool writes is the only thing taken from
//! the run's timing at placement, and [`place`] run again after an update
//! re-derives it.

use super::run::RunData;
use super::when::{ClipTiming, When};
use makepad_strict_json::{self as json, Value};

/// A bindable overlay document.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlay {
    /// `overlay/<id>`: the built-in document's name.
    pub id: &'static str,
    pub label: &'static str,
    /// What it shows, for the AI's tool description.
    pub doc: &'static str,
    /// Its `target` input names a widget (empty means the focus where the
    /// overlay allows it).
    pub target: TargetUse,
    /// Seconds it shows before and after the moment it marks, by default.
    pub lead: f64,
    pub tail: f64,
    pub source: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetUse {
    /// No target: it follows presses, keys or marks.
    None,
    /// A widget, or the focus when none is given.
    Optional,
    /// A widget is required.
    Required,
}

pub const OVERLAYS: &[Overlay] = &[
    Overlay {
        id: "highlight",
        label: "Highlight box",
        doc: "An outline box on a widget (target) or, without a target, on where the action is (the focus track); eases between targets.",
        target: TargetUse::Optional,
        lead: 0.35,
        tail: 0.8,
        source: include_str!("overlays/highlight.splash"),
    },
    Overlay {
        id: "spotlight",
        label: "Spotlight",
        doc: "Dims everything except a rounded hole over a widget (target) or the focus.",
        target: TargetUse::Optional,
        lead: 0.4,
        tail: 0.8,
        source: include_str!("overlays/spotlight.splash"),
    },
    Overlay {
        id: "callout",
        label: "Callout",
        doc: "A speech bubble with `text` pointing at a widget (target), above or below it as space allows.",
        target: TargetUse::Required,
        lead: 0.4,
        tail: 1.5,
        source: include_str!("overlays/callout.splash"),
    },
    Overlay {
        id: "lens",
        label: "Zoom lens",
        doc: "A round magnifier (zoom, radius) over the focus, showing the app magnified.",
        target: TargetUse::None,
        lead: 0.3,
        tail: 0.8,
        source: include_str!("overlays/lens.splash"),
    },
    Overlay {
        id: "ripple",
        label: "Click ripples",
        doc: "A ring expanding from every real click of the run, in sync with it.",
        target: TargetUse::None,
        lead: 0.0,
        tail: 0.7,
        source: include_str!("overlays/ripple.splash"),
    },
    Overlay {
        id: "keys",
        label: "Keystroke badge",
        doc: "A keycap badge showing each shortcut the run pressed (⌘ S), for a moment after the press.",
        target: TargetUse::None,
        lead: 0.0,
        tail: 1.3,
        source: include_str!("overlays/keys.splash"),
    },
    Overlay {
        id: "steps",
        label: "Step counter",
        doc: "A badge counting the run's marks as steps (\"2 / 5\"), popping at each mark.",
        target: TargetUse::None,
        lead: 0.0,
        tail: 0.0,
        source: include_str!("overlays/steps.splash"),
    },
];

pub fn overlay(id: &str) -> Option<&'static Overlay> {
    let id = id.strip_prefix("overlay/").unwrap_or(id);
    OVERLAYS.iter().find(|o| o.id == id)
}

/// The built-in document `overlay/<id>`.
pub fn builtin(name: &str) -> Option<&'static str> {
    overlay(name.strip_prefix("overlay/")?).map(|o| o.source)
}

/// What the AI asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayRequest {
    pub overlay: String,
    /// The Clip (node id) that plays the run.
    pub clip: String,
    /// A widget path (suffix of whole segments: "save" finds "main.save").
    pub target: Option<String>,
    /// When it shows; `None`: when the target is acted on (clicked, typed
    /// into, dragged, …), or, without a target, for the whole clip.
    pub when: Option<When>,
    pub lead: Option<f64>,
    pub tail: Option<f64>,
    /// Other inputs (`text`, `color`, `zoom`, …) as Splash values.
    pub params: Vec<(String, String)>,
}

/// A placed overlay: one Graphic per stretch.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayPlacement {
    /// `[start, length]` in edit seconds, one per Graphic.
    pub spans: Vec<(f64, f64)>,
    /// The Graphic's `params` entries (`target`, then the request's).
    pub params: Vec<(String, String)>,
    /// Notes for the AI (the target resolved to a longer path, a stretch
    /// the clip does not show, …).
    pub notes: Vec<String>,
}

fn splash_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
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

/// Resolve a request against the clip's run and timing. Errors say what
/// the run has instead (paths, calls, marks), so the AI can fix the call.
pub fn place(request: &OverlayRequest, run: &RunData, clip: ClipTiming) -> Result<OverlayPlacement, String> {
    let overlay = overlay(&request.overlay).ok_or_else(|| {
        format!("no overlay `{}`; the overlays are: {}", request.overlay, OVERLAYS.iter().map(|o| o.id).collect::<Vec<_>>().join(", "))
    })?;
    let mut notes = Vec::new();
    let target = match (&request.target, overlay.target) {
        (Some(_), TargetUse::None) => {
            notes.push(format!("`{}` follows the run by itself; the target was not used", overlay.id));
            None
        }
        (None, TargetUse::Required) => return Err(format!("`{}` needs a target: a widget path of the run, e.g. {}", overlay.id, example_paths(run))),
        (Some(t), _) => {
            let t = t.trim_start_matches('@');
            let keys = run.widget_keys(t);
            let Some(first) = keys.first() else {
                return Err(format!("the run has no widget `{t}`; its widgets: {}", example_paths(run)));
            };
            let path = first.rsplit_once('#').map_or(*first, |(b, _)| b).to_string();
            let distinct: Vec<&str> = keys.iter().map(|k| k.rsplit_once('#').map_or(*k, |(b, _)| b)).collect();
            if distinct.iter().any(|p| *p != path) {
                notes.push(format!("`{t}` names several widgets ({}); using `{path}`, name another by a longer path", distinct.join(", ")));
            }
            Some(path)
        }
        (None, _) => None,
    };
    let when = match (&request.when, &target) {
        (Some(when), _) => when.clone(),
        (None, Some(path)) => {
            // The calls that act on the target, whatever they are.
            let acting = run.calls.iter().find(|c| c.call != "focus" && c.target.as_deref().is_some_and(|p| same_widget(p, path)));
            match acting {
                Some(call) => When::Call { call: call.call.clone(), target: call.target.clone(), nth: None },
                None => {
                    notes.push(format!("no call acts on `{path}`; shown for the whole clip"));
                    When::Always
                }
            }
        }
        (None, None) => When::Always,
    };
    let lead = request.lead.unwrap_or(overlay.lead);
    let tail = request.tail.unwrap_or(overlay.tail);
    let run_spans = when.resolve(run, lead, tail)?;
    let mut spans = Vec::new();
    for span in run_spans {
        match clip.edit_span(span) {
            Some(s) => spans.push(s),
            None => notes.push(format!("the run's {:.2}..{:.2} s is outside the clip; nothing placed there", span.0, span.1)),
        }
    }
    if spans.is_empty() {
        return Err(format!(
            "the clip shows the run's {:.2}..{:.2} s, and the overlay's moments are all outside it{}",
            clip.source_in,
            clip.source_in + clip.length * clip.speed,
            if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) }
        ));
    }
    let mut params = Vec::new();
    if let Some(path) = &target {
        params.push(("target".to_string(), splash_string(path)));
    }
    for (k, v) in &request.params {
        if k == "target" {
            continue;
        }
        params.push((k.clone(), v.clone()));
    }
    Ok(OverlayPlacement { spans, params, notes })
}

/// A Graphic node's Splash for one placed span: `src` is how the edit
/// names the overlay document (a library `asset(…)` or the built-in).
pub fn graphic_splash(id: &str, src: &str, clip: &str, span: (f64, f64), params: &[(String, String)]) -> String {
    let params = if params.is_empty() {
        String::new()
    } else {
        format!(" params: {{{}}}", params.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(" "))
    };
    format!("let {id} = Graphic{{src: {src} span: [{}, {}] bind: @{clip}{params}}}", fmt(span.0), fmt(span.1))
}

fn fmt(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r.fract() == 0.0 { format!("{r:.1}") } else { format!("{r}") }
}

/// A call's target and a recorded path name the same widget: one is the
/// other or a whole-segment suffix of it.
fn same_widget(a: &str, b: &str) -> bool {
    let suffix = |long: &str, short: &str| long == short || (long.len() > short.len() && long.ends_with(short) && long.as_bytes()[long.len() - short.len() - 1] == b'.');
    suffix(a, b) || suffix(b, a)
}

fn example_paths(run: &RunData) -> String {
    // The targets the script acted on first, then other paths.
    let mut paths: Vec<String> = run.calls.iter().filter_map(|c| c.target.clone()).collect();
    paths.dedup();
    for p in run.widget_paths() {
        if paths.len() >= 12 {
            break;
        }
        if !paths.iter().any(|x| x == p) && !p.contains(':') {
            paths.push(p.to_string());
        }
    }
    paths.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ")
}

/// What an AI needs to know about a run before placing overlays: its
/// length, marks, the calls with their targets and times, and the widget
/// paths it saw. JSON, for the `run_facts` tool.
pub fn run_facts(run: &RunData) -> Value {
    let secs = |f: u64| round(f as f64 / run.fps);
    json::obj(vec![
        ("label", json::s(&run.label)),
        ("duration", Value::F64(round(run.duration()))),
        ("size", Value::Arr(vec![Value::F64(run.size.0), Value::F64(run.size.1)])),
        ("marks", Value::Obj(run.marks.iter().map(|(n, f)| (n.clone(), Value::F64(secs(*f)))).collect())),
        (
            "calls",
            Value::Arr(
                run.calls
                    .iter()
                    .filter(|c| !matches!(c.call.as_str(), "mark" | "focus" | "focus_rect" | "wide"))
                    .map(|c| {
                        let mut fields = vec![("call", json::s(&c.call)), ("from", Value::F64(secs(c.from))), ("to", Value::F64(secs(c.to)))];
                        if let Some(t) = &c.target {
                            fields.push(("target", json::s(t)));
                        }
                        if let Some(t) = &c.text {
                            fields.push(("text", json::s(t)));
                        }
                        json::obj(fields)
                    })
                    .collect(),
            ),
        ),
        ("shortcuts", Value::Arr(run.keys.iter().filter(|k| k.is_shortcut()).map(|k| json::obj(vec![("at", Value::F64(secs(k.frame()))), ("key", json::s(k.label()))])).collect())),
        ("widgets", Value::Arr(run.widget_paths().into_iter().filter(|p| !p.contains(':')).map(json::s).collect())),
        ("recorded_bind_data", Value::Bool(!run.derived)),
    ])
}

pub(crate) fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// The tool definitions (name, description, JSON schema), for the control
/// API's registry (§10.4) and M3's agent loop.
pub fn tool_defs() -> Vec<(&'static str, String, String)> {
    let overlays: Vec<String> = OVERLAYS.iter().map(|o| format!("\"{}\"", o.id)).collect();
    let described: Vec<String> = OVERLAYS.iter().map(|o| format!("{}: {}", o.id, o.doc)).collect();
    vec![
        (
            "run_facts",
            "What an app-run clip recorded: its marks, the script's calls with their targets and times (seconds of the run), shortcuts pressed and the widget paths it saw. Read it before placing overlays.".to_string(),
            r#"{"type":"object","properties":{"clip":{"type":"string","description":"The id of the Clip that plays the app run"}},"required":["clip"]}"#.to_string(),
        ),
        (
            "bind_overlay",
            format!(
                "Place a demo overlay that follows an app-run clip (it reads the run's recorded data, so it stays right when the run is updated and under reframing crops). Overlays: {}. `when` defaults to when the target is acted on, or the whole clip.",
                described.join(" ")
            ),
            format!(
                r##"{{"type":"object","properties":{{"overlay":{{"enum":[{}]}},"clip":{{"type":"string","description":"The id of the Clip that plays the app run"}},"target":{{"type":"string","description":"A widget path of the run (a suffix of whole segments: \"save\" finds \"main.save\")"}},"when":{{"type":"object","description":"One of {{\"call\": \"click\", \"target\": \"save\", \"nth\": 0}}, {{\"mark\": \"saved\", \"hold\": 2.0}}, {{\"from_mark\": \"a\", \"to_mark\": \"b\"}}, {{\"always\": true}}"}},"lead":{{"type":"number","description":"Seconds shown before the moment"}},"tail":{{"type":"number","description":"Seconds shown after it"}},"params":{{"type":"object","description":"Overlay inputs as Splash values, e.g. {{\"text\": \"\\\"Saves instantly\\\"\", \"color\": \"#ff5a36\"}}"}}}},"required":["overlay","clip"]}}"##,
                overlays.join(",")
            ),
        ),
    ]
}

/// Read a `bind_overlay` call's arguments.
pub fn request_from_json(args: &Value) -> Result<OverlayRequest, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let num = |v: Option<&Value>| match v {
        Some(Value::Int(i)) => Some(*i as f64),
        Some(Value::F64(f)) => Some(*f),
        _ => None,
    };
    let when = match args.get("when") {
        None | Some(Value::Null) => None,
        Some(w) => Some(if let Some(call) = w.get("call").and_then(Value::as_str) {
            When::Call { call: call.to_string(), target: w.get("target").and_then(Value::as_str).map(str::to_string), nth: num(w.get("nth")).map(|n| n.max(0.0) as usize) }
        } else if let Some(mark) = w.get("mark").and_then(Value::as_str) {
            When::Mark { name: mark.trim_start_matches('@').to_string(), hold: num(w.get("hold")).unwrap_or(2.0) }
        } else if let (Some(a), Some(b)) = (w.get("from_mark").and_then(Value::as_str), w.get("to_mark").and_then(Value::as_str)) {
            When::Between { from: a.trim_start_matches('@').to_string(), to: b.trim_start_matches('@').to_string() }
        } else if w.get("always").is_some() {
            When::Always
        } else {
            return Err("`when` is {\"call\": …, \"target\": …}, {\"mark\": …, \"hold\": …}, {\"from_mark\": …, \"to_mark\": …} or {\"always\": true}".into());
        }),
    };
    let params = match args.get("params") {
        Some(Value::Obj(fields)) => fields
            .iter()
            .map(|(k, v)| match v {
                Value::Str(s) => (k.clone(), s.clone()),
                other => (k.clone(), other.to_json()),
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(OverlayRequest {
        overlay: s("overlay").ok_or("`overlay` is required")?,
        clip: s("clip").ok_or("`clip` is required: the id of the Clip that plays the app run")?,
        target: s("target"),
        when,
        lead: num(args.get("lead")),
        tail: num(args.get("tail")),
        params,
    })
}

//! The effect catalogue as plain data, for tools and models (the control
//! API's `render_kit_effects`): every effect with its description, its
//! parameters (kind, range, default, choices, whether they can be keyed)
//! and a ready-to-use example, plus the same as JSON.

use crate::anim::Value;
use crate::catalog::{hex, ParamKind, CAMERA, EFFECTS};

#[derive(Clone, Debug, PartialEq)]
pub struct ParamEntry {
    pub name: &'static str,
    /// `number`, `vec2`, `vec3`, `color`, `bool`, `choice`, `text`,
    /// `layer`, `points`, `stops`, `curve`.
    pub kind: &'static str,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// The default as written in Splash (`8`, `vec2(0.5, 0.5)`, `#ffffff`,
    /// `@both`, `""`).
    pub default: String,
    pub choices: Vec<&'static str>,
    /// Interpolates between keys (numbers, vectors, colours, points,
    /// stops). Choices, flags, text and layers can still be keyed but
    /// hold until the next key.
    pub keyable: bool,
    pub description: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectEntry {
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    /// Renders the layer at other times (motion blur, echo).
    pub temporal: bool,
    pub params: Vec<ParamEntry>,
    /// A Splash example with the effect's most telling parameters.
    pub example: String,
    /// How its preview strip is made (`makepad-render-kit strip`).
    pub strip: StripPreset,
    pub strip_command: String,
}

fn num(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".into() } else { s.into() }
}

fn splash(value: &Value) -> String {
    match value {
        Value::Num(v) => num(*v),
        Value::Vec2(v) => format!("vec2({}, {})", num(v[0]), num(v[1])),
        Value::Vec3(v) => format!("vec3({}, {}, {})", num(v[0]), num(v[1]), num(v[2])),
        Value::Color(c) => hex(c),
        Value::Bool(b) => b.to_string(),
        Value::Choice(c) => format!("@{c}"),
        Value::Text(t) => format!("{t:?}"),
        Value::Layer(l) => if l.is_empty() { "@layer_name".into() } else { format!("@{l}") },
        Value::Points(p) => format!("[{}]", p.iter().map(|p| format!("vec2({}, {})", num(p[0]), num(p[1]))).collect::<Vec<_>>().join(", ")),
        Value::Stops(s) => format!("[{}]", s.iter().map(|(p, c)| format!("[{}, {}]", num(*p), hex(c))).collect::<Vec<_>>().join(", ")),
    }
}

fn param(p: &crate::catalog::ParamInfo) -> ParamEntry {
    let (kind, min, max, choices, keyable) = match p.kind {
        ParamKind::Num { min, max, .. } => ("number", Some(min), Some(max), Vec::new(), true),
        ParamKind::Vec2 { .. } => ("vec2", None, None, Vec::new(), true),
        ParamKind::Vec3 { .. } => ("vec3", None, None, Vec::new(), true),
        ParamKind::Color { .. } => ("color", None, None, Vec::new(), true),
        ParamKind::Bool { .. } => ("bool", None, None, Vec::new(), false),
        ParamKind::Choice { choices, .. } => ("choice", None, None, choices.to_vec(), false),
        ParamKind::Text => ("text", None, None, Vec::new(), false),
        ParamKind::Layer => ("layer", None, None, Vec::new(), false),
        ParamKind::Points => ("points", None, None, Vec::new(), true),
        ParamKind::Stops { .. } => ("stops", None, None, Vec::new(), true),
        ParamKind::Curve => ("curve", None, None, Vec::new(), true),
    };
    let default = match (&p.kind, p.kind.default_value()) {
        (ParamKind::Text, _) => "\"\"".into(),
        (ParamKind::Layer, _) => "none".into(),
        (_, v) => splash(&v),
    };
    ParamEntry { name: p.name, kind, min, max: max.filter(|m| *m < 1e8), default, choices, keyable, description: p.doc }
}

/// A hand-picked example per effect: its telling parameters with values
/// that show the effect, one of them keyed.
fn example(name: &str) -> String {
    let body = match name {
        "gaussian_blur" => "radius: tween(0, 24, 0.0, 1.0, @ease_out)",
        "directional_blur" => "angle: 0 length: 60",
        "radial_blur" => "center: vec2(0.5, 0.5) angle: 12",
        "zoom_blur" => "center: vec2(0.5, 0.5) amount: tween(0.4, 0, 0.0, 0.8, @ease_out_cubic)",
        "glow" => "threshold: 0.6 radius: 24 intensity: 1.2",
        "bloom" => "threshold: 0.75 radius: 64 intensity: 0.8",
        "halation" => "threshold: 0.7 radius: 12 amount: 0.6",
        "drop_shadow" => "distance: 16 radius: 12 opacity: 0.6",
        "inner_shadow" => "distance: 6 radius: 8",
        "stroke" => "width: 6 color: #ff8c33 position: @outside",
        "bevel" => "size: 12 depth: 2 angle: 120",
        "fill" => "color: #ff5a36 blend: @multiply",
        "gradient" => "kind: @linear start: vec2(0, 0.5) end: vec2(1, 0.5) stops: [[0.0, #0b0d12], [1.0, #ff5a36]] clip: false",
        "noise" => "kind: @fbm scale: 200 evolution: 0.5 blend: @soft_light opacity: 0.6",
        "stroke_path" => "svg: \"M 100 500 C 400 200, 800 800, 1100 500\" width: 12 taper_start: 0.3 trim_end: tween(0, 1, 0.0, 2.0, @ease_in_out)",
        "particles" => "emitter: @point position: vec2(0.5, 0.8) rate: 200 speed: 300 spread: 40",
        "displace" => "map: @map_layer amount: vec2(30, 30)",
        "turbulence" => "amount: 12 size: 150",
        "slices" => "dir: @cycle count: 12 amp: 0.08 period: 0.5",
        "ripple" => "center: vec2(0.5, 0.5) wavelength: 60 amplitude: 8 speed: 120",
        "blend" => "layer: @texture_layer mode: @overlay opacity: 0.8",
        "mask" => "shape: @rounded_rect size: tween(vec2(0, 0.5), vec2(0.8, 0.5), 0.0, 1.0, @ease_out_cubic) radius: 40 feather: 12",
        "matte" => "layer: @title mode: @alpha",
        "transform_3d" => "rotation: keys([0.0, vec3(60, -20, 0)], [1.2, vec3(0, 0, 0), @ease_out_back])",
        "motion_blur" => "shutter_angle: 180 samples: 0",
        "echo" => "count: 6 delay: tween(0.02, 0.06, 0.0, 2.0) decay: 0.6",
        "curves" => "master: [vec2(0, 0.05), vec2(0.5, 0.45), vec2(1, 1)]",
        "levels" => "in_black: 0.05 in_white: 0.95 gamma: 1.1",
        "hue_saturation" => "hue: tween(0, 180, 0.0, 4.0) saturation: 0.2",
        "white_balance" => "temperature: 30",
        "lut" => "file: \"looks/teal_orange.cube\" intensity: 0.8",
        "chromatic_aberration" => "amount: 4",
        "grain" => "amount: 0.08 size: 1.4",
        "vignette" => "amount: 0.5 feather: 0.6",
        "exposure" => "exposure: 0.5",
        "brightness_contrast" => "brightness: 0.05 contrast: 0.2",
        "tint" => "black: #1a0b2e white: #ffd9a0",
        "invert" => "mix: tween(0, 1, 0.0, 0.5)",
        _ => "",
    };
    format!("Effect{{name: @{name} {body}}}").replace("  ", " ").replace(" }", "}")
}

/// How a preview strip shows an effect: the test input (`moving` is a
/// disc on a curve and a travelling word, a pure function of t, so time
/// effects show their motion), the times, and the settings. A setting
/// written `from..to` ramps across the strip's times.
#[derive(Clone, Debug, PartialEq)]
pub struct StripPreset {
    pub input: &'static str,
    pub times: &'static [f64],
    pub set: &'static [(&'static str, &'static str)],
}

/// The test inputs a strip can use (512×256).
pub const STRIP_INPUTS: &[&str] = &[
    "moving", "checker", "ramp", "colors", "square", "grey_square", "spot", "half", "line", "grey50", "grey25", "white", "red",
];

const T4: &[f64] = &[0.0, 0.5, 1.0, 1.5];
const TFAST: &[f64] = &[0.2, 0.45, 0.7, 0.95];

pub fn strip_preset(name: &str) -> StripPreset {
    let p = |input, times, set| StripPreset { input, times, set };
    match name {
        "gaussian_blur" => p("moving", T4, &[("radius", "0..16")]),
        "directional_blur" => p("moving", T4, &[("length", "0..60"), ("angle", "20")]),
        "radial_blur" => p("checker", T4, &[("angle", "0..20")]),
        "zoom_blur" => p("checker", T4, &[("amount", "0..0.3")]),
        "glow" => p("moving", T4, &[("threshold", "0.5"), ("radius", "4..24"), ("intensity", "1.5")]),
        "bloom" => p("moving", T4, &[("threshold", "0.5"), ("radius", "16..80"), ("intensity", "1.2")]),
        "halation" => p("moving", T4, &[("threshold", "0.5"), ("radius", "12"), ("amount", "0.5..2")]),
        "drop_shadow" => p("moving", T4, &[("distance", "4..24"), ("radius", "2..12"), ("opacity", "0.8")]),
        "inner_shadow" => p("moving", T4, &[("distance", "2..10"), ("radius", "2..6"), ("opacity", "0.9")]),
        "stroke" => p("moving", T4, &[("width", "1..8"), ("color", "#ff8c33")]),
        "bevel" => p("moving", T4, &[("size", "4..14"), ("depth", "2")]),
        "fill" => p("moving", T4, &[("color", "#ff5a36"), ("opacity", "0..1")]),
        "gradient" => p("moving", T4, &[("kind", "@conic"), ("start", "vec2(0.5, 0.5)"), ("clip", "true"), ("opacity", "0.3..1")]),
        "noise" => p("white", T4, &[("scale", "60"), ("evolution", "1.5")]),
        "stroke_path" => p("white", T4, &[("svg", "\"M 40 200 C 160 20, 350 240, 470 60\""), ("width", "14"), ("taper_start", "0.3"), ("taper_end", "0.3"), ("trim_end", "0.1..1"), ("color", "#ff5a36"), ("layer", "@only")]),
        "particles" => p("moving", T4, &[("emitter", "@circle"), ("size", "vec2(0.1, 0.1)"), ("rate", "300"), ("speed", "160"), ("spread", "360"), ("gravity", "vec2(0, 120)"), ("particle_size", "6")]),
        "displace" => p("checker", T4, &[("map", "@ramp"), ("amount", "vec2(0, 0)..vec2(24, 0)")]),
        "turbulence" => p("checker", T4, &[("amount", "10"), ("size", "80"), ("evolution", "1")]),
        "slices" => p("checker", T4, &[("dir", "@cycle"), ("count", "10"), ("amp", "0.1"), ("period", "1")]),
        "ripple" => p("checker", T4, &[("amplitude", "8"), ("wavelength", "40"), ("speed", "80")]),
        "blend" => p("colors", T4, &[("layer", "@ramp"), ("mode", "@overlay"), ("opacity", "0..1")]),
        "mask" => p("moving", T4, &[("shape", "@ellipse"), ("size", "vec2(0.2, 0.4)..vec2(0.9, 0.9)"), ("feather", "12")]),
        "matte" => p("checker", T4, &[("layer", "@ramp"), ("mode", "@luma")]),
        "transform_3d" => p("checker", T4, &[("rotation", "vec3(0, 0, 0)..vec3(50, 35, 10)")]),
        "motion_blur" => p("moving", TFAST, &[("shutter_angle", "360")]),
        "echo" => p("moving", TFAST, &[("count", "6"), ("delay", "0.03"), ("decay", "0.7")]),
        "curves" => p("ramp", T4, &[("master", "[vec2(0, 0), vec2(0.5, 0.2), vec2(1, 1)]")]),
        "levels" => p("ramp", T4, &[("gamma", "0.5..2")]),
        "hue_saturation" => p("colors", T4, &[("hue", "0..180")]),
        "white_balance" => p("grey50", T4, &[("temperature", "-100..100")]),
        "lut" => p("colors", T4, &[("intensity", "0..1")]),
        "chromatic_aberration" => p("moving", T4, &[("amount", "0..10")]),
        "grain" => p("grey50", T4, &[("amount", "0.15")]),
        "vignette" => p("white", T4, &[("amount", "0..1")]),
        "exposure" => p("ramp", T4, &[("exposure", "-2..2")]),
        "brightness_contrast" => p("ramp", T4, &[("contrast", "-0.5..0.5")]),
        "tint" => p("ramp", T4, &[("black", "#1a0b2e"), ("white", "#ffd9a0"), ("amount", "0..1")]),
        "invert" => p("ramp", T4, &[("mix", "0..1")]),
        _ => p("moving", T4, &[]),
    }
}

/// The command that renders an effect's preview strip.
pub fn strip_command(name: &str) -> String {
    let preset = strip_preset(name);
    let mut cmd = format!("MAKEPAD_HIDE_WINDOWS=1 makepad-render-kit strip --effect {name} --input {}", preset.input);
    for (k, v) in preset.set {
        let v = if v.contains(' ') { format!("'{v}'") } else { v.to_string() };
        cmd.push_str(&format!(" --set {k}={v}"));
    }
    cmd.push_str(&format!(" --times {} --out {name}.png", preset.times.iter().map(|t| num(*t)).collect::<Vec<_>>().join(",")));
    cmd
}

/// Every effect, in catalog order.
pub fn catalogue() -> Vec<EffectEntry> {
    EFFECTS
        .iter()
        .map(|e| EffectEntry {
            name: e.name,
            category: e.category.title(),
            description: e.doc,
            temporal: e.temporal,
            params: e.params.iter().map(param).collect(),
            example: example(e.name),
            strip: strip_preset(e.name),
            strip_command: strip_command(e.name),
        })
        .collect()
}

/// The comp camera's parameters (`camera: Camera{...}`).
pub fn camera_params() -> Vec<ParamEntry> {
    CAMERA.iter().map(param).collect()
}

/// A value as a tool or a command line writes it: `12.5`, `vec2(1, 2)` or
/// `1,2`, `vec3(..)` or `1,2,3`, `#ff8800`, `true`, `@choice`, a quoted
/// string, or anything else as text. Parameter kinds are then coerced by
/// [`crate::Effect::build`] (a `@name` becomes a layer where one is wanted).
pub fn parse_value(text: &str) -> Value {
    let t = text.trim();
    let nums = |inner: &str| inner.split(',').map(|v| v.trim().parse::<f64>()).collect::<Result<Vec<_>, _>>().ok();
    let inner = t
        .strip_prefix("vec2(")
        .or_else(|| t.strip_prefix("vec3("))
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or(t);
    if let Ok(v) = t.parse::<f64>() {
        return Value::Num(v);
    }
    match nums(inner).as_deref() {
        Some([x, y]) => return Value::Vec2([*x, *y]),
        Some([x, y, z]) => return Value::Vec3([*x, *y, *z]),
        _ => {}
    }
    if let Some(list) = t.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        let points: Option<Vec<[f64; 2]>> = list
            .split("vec2(")
            .filter(|p| !p.trim().trim_matches(',').trim().is_empty())
            .map(|p| {
                let inner = p.trim().trim_end_matches(',').trim().trim_end_matches(')');
                match nums(inner).as_deref() {
                    Some([x, y]) => Some([*x, *y]),
                    _ => None,
                }
            })
            .collect();
        if let Some(points) = points {
            return Value::Points(points);
        }
    }
    if let Some(c) = t.starts_with('#').then(|| crate::anim::parse_hex_color(t)).flatten() {
        return Value::Color(c);
    }
    match t {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ if t.starts_with('@') => Value::Choice(t[1..].to_string()),
        _ if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') => Value::Text(t[1..t.len() - 1].to_string()),
        _ => Value::Text(t.to_string()),
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_param(p: &ParamEntry) -> String {
    let opt = |v: Option<f64>| v.map(num).unwrap_or_else(|| "null".into());
    format!(
        "{{\"name\":{},\"kind\":{},\"min\":{},\"max\":{},\"default\":{},\"choices\":[{}],\"keyable\":{},\"description\":{}}}",
        json_str(p.name),
        json_str(p.kind),
        opt(p.min),
        opt(p.max),
        json_str(&p.default),
        p.choices.iter().map(|c| json_str(c)).collect::<Vec<_>>().join(","),
        p.keyable,
        json_str(p.description)
    )
}

/// The catalogue as JSON: `{"format":"makepad-render-kit-catalogue",
/// "version":1,"effects":[…],"camera":[…]}`.
pub fn catalogue_json() -> String {
    let effects: Vec<String> = catalogue()
        .iter()
        .map(|e| {
            format!(
                "{{\"name\":{},\"category\":{},\"description\":{},\"temporal\":{},\"example\":{},\"strip\":{{\"input\":{},\"times\":[{}],\"set\":{{{}}},\"command\":{}}},\"params\":[{}]}}",
                json_str(e.name),
                json_str(e.category),
                json_str(e.description),
                e.temporal,
                json_str(&e.example),
                json_str(e.strip.input),
                e.strip.times.iter().map(|t| num(*t)).collect::<Vec<_>>().join(","),
                e.strip.set.iter().map(|(k, v)| format!("{}:{}", json_str(k), json_str(v))).collect::<Vec<_>>().join(","),
                json_str(&e.strip_command),
                e.params.iter().map(json_param).collect::<Vec<_>>().join(",")
            )
        })
        .collect();
    format!(
        "{{\"format\":\"makepad-render-kit-catalogue\",\"version\":1,\"effects\":[{}],\"camera\":[{}]}}",
        effects.join(","),
        camera_params().iter().map(json_param).collect::<Vec<_>>().join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Anim;
    use crate::stack::Effect;

    #[test]
    fn every_effect_is_described_with_a_working_example() {
        let all = catalogue();
        assert_eq!(all.len(), EFFECTS.len());
        for e in &all {
            assert!(!e.description.is_empty() && !e.params.is_empty(), "{}", e.name);
            assert!(e.example.starts_with(&format!("Effect{{name: @{} ", e.name)), "{}: {}", e.name, e.example);
            // Every parameter the example names exists on the effect.
            let body = &e.example[e.example.find(' ').unwrap()..];
            for word in body.split_whitespace().filter(|w| w.ends_with(':') && !w.starts_with('@')) {
                let name = word.trim_end_matches(':');
                if name == "name" {
                    continue;
                }
                assert!(e.params.iter().any(|p| p.name == name), "{}: example names unknown `{name}`", e.name);
            }
            for p in &e.params {
                if p.kind == "number" {
                    assert!(p.min.is_some());
                    let d: f64 = p.default.parse().unwrap_or_else(|_| panic!("{}.{} default {}", e.name, p.name, p.default));
                    assert!(d >= p.min.unwrap() && p.max.is_none_or(|m| d <= m));
                }
                if p.kind == "choice" {
                    assert!(p.choices.contains(&p.default.trim_start_matches('@')), "{}.{}", e.name, p.name);
                }
            }
            // Defaults build without diagnostics.
            assert!(Effect::build(e.name, Vec::<(&str, Anim)>::new()).is_ok());
        }
        let glow = all.iter().find(|e| e.name == "glow").unwrap();
        let color = glow.params.iter().find(|p| p.name == "color").unwrap();
        assert_eq!((color.kind, color.default.as_str(), color.keyable), ("color", "#ffffff", true));
        let dir = &all.iter().find(|e| e.name == "gaussian_blur").unwrap().params[1];
        assert_eq!((dir.kind, dir.default.as_str(), dir.keyable), ("choice", "@both", false));
        assert_eq!(dir.choices, vec!["both", "horizontal", "vertical"]);
    }

    #[test]
    fn every_strip_preset_builds() {
        for e in catalogue() {
            assert!(STRIP_INPUTS.contains(&e.strip.input), "{}: {}", e.name, e.strip.input);
            assert!(!e.strip.times.is_empty());
            let params: Vec<(&str, Anim)> = e
                .strip
                .set
                .iter()
                .map(|(k, v)| (*k, Anim::Const(parse_value(v.split("..").last().unwrap()))))
                .collect();
            if let Err(d) = Effect::build(e.name, params) {
                panic!("{}: {:?}", e.name, d.errors);
            }
            assert!(e.strip_command.contains(&format!("--effect {} --input {}", e.name, e.strip.input)));
        }
        // Time effects preview on the moving input.
        for name in ["motion_blur", "echo", "particles", "directional_blur"] {
            assert_eq!(strip_preset(name).input, "moving");
        }
    }

    #[test]
    fn values_parse_as_written() {
        assert_eq!(parse_value("12.5"), Value::Num(12.5));
        assert_eq!(parse_value("vec2(1, 2)"), Value::Vec2([1.0, 2.0]));
        assert_eq!(parse_value("1,2,3"), Value::Vec3([1.0, 2.0, 3.0]));
        assert_eq!(parse_value("#ff0000"), Value::Color([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_value("@outside"), Value::Choice("outside".into()));
        assert_eq!(parse_value("\"a b\""), Value::Text("a b".into()));
        assert_eq!(parse_value("true"), Value::Bool(true));
        assert_eq!(parse_value("[vec2(0, 0), vec2(0.5, 0.2), vec2(1, 1)]"), Value::Points(vec![[0.0, 0.0], [0.5, 0.2], [1.0, 1.0]]));
        let (effect, _) = Effect::build("matte", [("layer", Anim::Const(parse_value("@logo")))]).unwrap();
        assert_eq!(effect.at(0.0).layer_refs(), vec!["logo"]);
    }

    #[test]
    fn the_json_is_well_formed() {
        let json = catalogue_json();
        // Balanced brackets outside strings, and every effect named once.
        let (mut depth, mut in_str, mut escape) = (0i32, false, false);
        for c in json.chars() {
            if in_str {
                match (escape, c) {
                    (true, _) => escape = false,
                    (false, '\\') => escape = true,
                    (false, '"') => in_str = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0);
        }
        assert_eq!(depth, 0);
        assert!(!in_str);
        for e in EFFECTS {
            assert_eq!(json.matches(&format!("\"name\":\"{}\",\"category\"", e.name)).count(), 1, "{}", e.name);
        }
        assert!(json.contains("\"max\":null"), "open-ended ranges are null");
        assert!(json.contains("\\\"M 100 500"), "strings are escaped");
    }
}

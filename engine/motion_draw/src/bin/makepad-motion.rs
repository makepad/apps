//! `makepad-motion`: Motion documents on the real GPU, in a (normally
//! hidden) window: frames as PNG with their RGBA hashes, and contact
//! sheets. What the pixel tests and people checking a document use;
//! `makepad-edit-tools` builds its agent loop on the same library.
//!
//! ```text
//! makepad-motion check <doc.splash>                        load and print every diagnostic
//! MAKEPAD_HIDE_WINDOWS=1 makepad-motion frames <doc.splash> --at 0,0.5,1.25 --out <dir>
//!     [--width 960] [--ambient <s>] [--param name=value]... [--image <id>:<rev>=<file>]... [--svg <id>:<rev>=<file>]...
//!     [--model <id>:<rev>=<file>]... [--audio synthetic | <features.json>] [--lyrics id=file.json[@offset]] [--lang zh]
//! MAKEPAD_HIDE_WINDOWS=1 makepad-motion strip <doc.splash> --count 8 --out strip.png [--width 480] [same options]
//! MAKEPAD_HIDE_WINDOWS=1 makepad-motion live <doc.splash> --at 2.0 --set 1738:41=#ff8800 [--set line:col=value]... --out <dir> [same options]
//! ```
//!
//! `live` is an editor's drag on literals, measured: the frame at `--at`,
//! then each `--set` literal (1-based line and column of its first
//! character, a number or `#colour`) patched in the running document and
//! its compiled shaders (edit mode's live literals), the frame again, then
//! the document loaded again from the edited text and the frame a third
//! time. It prints what each patch reached, the shaders and GPU programs
//! compiled between the frames, and how long the patched frame took.
//! `--folded` runs it without edit mode (literals folded into shaders), to
//! compare.
//!
//! `frames` prints `frame <t> <rgba hash> <file>` per frame; equal hashes
//! are equal pictures, and after the last one `draw calls <n>`, the calls
//! that frame recorded. `--param` fills an input (numbers, `#rrggbb`,
//! `x,y` for a vec2, anything else is text). `--audio synthetic` gives the
//! document a synthetic voice `vo` and music `m` (AudioFeatures::synthetic).

use makepad_motion_draw::{decode_image, MotionDraw};
use makepad_motion::*;
// Both globs export a `Media`; the motion document's is meant.
use makepad_motion::Media;
use makepad_widgets::makepad_zune_png::makepad_zune_core::bit_depth::BitDepth;
use makepad_widgets::makepad_zune_png::makepad_zune_core::colorspace::ColorSpace;
use makepad_widgets::makepad_zune_png::makepad_zune_core::options::EncoderOptions;
use makepad_widgets::makepad_zune_png::PngEncoder;
use makepad_widgets::*;
use std::path::PathBuf;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.MotionHostBase = #(MotionHost::register_widget(vm))
    mod.widgets.MotionHost = set_type_default() do mod.widgets.MotionHostBase{
        width: Fill
        height: Fill
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Motion"
                window.inner_size: vec2(480, 300)
                body +: {
                    host := mod.widgets.MotionHost{}
                }
            }
        }
    }
}

enum Output {
    /// Each frame as its own PNG in this directory.
    Frames(PathBuf),
    /// One contact sheet.
    Strip(PathBuf),
    /// No pictures: how long each frame takes (evaluate, record, paint).
    Bench,
}

/// `live`: the literals to patch between the first and second frame, the
/// document's text they are in, and the document loaded from the edited
/// text (drawn third).
struct Live {
    /// Each literal's site, its new value, and what it was and is written as.
    patches: Vec<(LiteralSite, LiteralValue, String)>,
    /// `--edit` that changed more than literals: the reload only.
    structural: bool,
    /// The edited text, written where `load` reads it, and the options.
    reload: Option<(PathBuf, Vec<String>)>,
    /// `--folded`: edit mode off.
    folded: bool,
    /// (Splash shaders, GPU programs) when the patch was made.
    counts: (usize, usize),
}

struct Job {
    doc: MotionDoc,
    times: Vec<f64>,
    width: u32,
    output: Output,
    images: Vec<(Media, PathBuf)>,
    svgs: Vec<(Media, PathBuf)>,
    models: Vec<(Media, PathBuf)>,
    files: Vec<(Media, PathBuf)>,
    /// Seconds one frame may take before the run gives up (`--stall`).
    stall: f64,
    /// Draw as a preview does (`--realtime`): one sample per frame, no
    /// shutter; for measuring the preview.
    realtime: bool,
    /// `--warm <step>` (with `--realtime`): the first `warm` times are drawn
    /// ahead and unseen, as Stage's preview draws a slide ahead before it
    /// shows it; they are not timed.
    warm: usize,
    live: Option<Live>,
    /// `--ambient <seconds>`: `live()` reads this fixed clock in every frame
    /// instead of the frame's own time, so only the document's own timeline
    /// moves between frames (an audit of holds compares frames of one ambient).
    ambient: Option<f64>,
    /// `--pick x,y` (document units, once per place): after the last frame,
    /// what drew at each place, from the renderer's pick ids, and how long
    /// the id image took; then the last frame again (`after the pick: frame
    /// <t> <rgba hash>`), which the pick must have left as it was.
    pick: Vec<[f64; 2]>,
}

/// `--set line:col=value`: the bytes of the literal starting at that
/// 1-based place (a number's digits, a colour's `#` and hex digits) and
/// its new value.
fn live_set(text: &str, arg: &str) -> Result<(std::ops::Range<usize>, makepad_motion::schema::Literal, String), String> {
    use makepad_motion::schema::Literal;
    let (at, value) = arg.split_once('=').ok_or_else(|| format!("--set {arg}: expected line:col=value"))?;
    let (line, col) = at.split_once(':').and_then(|(l, c)| Some((l.parse::<usize>().ok()?, c.parse::<usize>().ok()?))).ok_or_else(|| format!("--set {arg}: expected line:col"))?;
    let line_start = if line <= 1 { 0 } else { text.match_indices('\n').nth(line - 2).map(|(i, _)| i + 1).ok_or_else(|| format!("--set {arg}: no line {line}"))? };
    let start = line_start + text[line_start..].chars().take(col.saturating_sub(1)).map(char::len_utf8).sum::<usize>();
    let rest = &text[start..];
    let len = if rest.starts_with('#') {
        1 + rest[1..].chars().take_while(|c| c.is_ascii_hexdigit()).count()
    } else {
        rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').count()
    };
    if len == 0 || (rest.starts_with('#') && len == 1) {
        return Err(format!("--set {arg}: no literal at {line}:{col} (`{}`)", rest.chars().take(12).collect::<String>()));
    }
    let literal = match param_value(value) {
        InputValue::Number(n) => Literal::Number(n),
        InputValue::Color(c) => Literal::Color([c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]),
        _ => return Err(format!("--set {arg}: the value is a number or #rrggbb[aa]")),
    };
    Ok((start..start + len, literal, value.to_string()))
}

type GpuByPass = std::collections::HashMap<String, (f64, usize)>;

fn usage() -> String {
    "usage: makepad-motion check <doc.splash> | frames <doc.splash> --at t,t,... --out <dir> [--width px] [--stall seconds per frame, 120] [--param name=value]... \
     [--image id:rev=file]... [--svg id:rev=file]... [--model id:rev=file]... [--file id:rev=file]... [--module id:rev=file.splash]... [--audio synthetic [--voice \"text\"] [--wps 2.5] [--voice-start 0.5] [--bpm 120]] \
     | strip <doc.splash> --count n --out <file.png> [--width px] [same options] \
     | thumbnail <doc.splash> --out <file.png> [--width px] [--pick x,y]... [same options] \
     | bench <doc.splash> [--width px] [--realtime (as a preview draws)] [--warm s (realtime: draw ahead, unseen, every s seconds and at every key, then time)] [same options] (every frame: evaluate, record and paint times) | kinetic (lists the KineticText presets) | text3d (lists the Text3D presets) | scene3d <doc.splash> (prints the Scene3D each Text3D layer lowers to) | fields [type [field]] (the fields a type takes: kind, default, range, unit) | characters (lists the Character kit) | character-svg <kit> [look=value]... (a kit character as a character SVG) | fonts (lists the bundled fonts) | icons [filter] (lists the bundled icons) | animators (lists the text animators) | styles (lists the style kits) | style <kit> [WxH] (prints a kit's parts as Splash)"
        .into()
}

fn finish(result: Result<String, String>) -> ! {
    match result {
        Ok(report) => {
            println!("{report}");
            std::process::exit(0)
        }
        Err(error) => {
            eprintln!("motion: FAILED: {error}");
            std::process::exit(1)
        }
    }
}

fn param_value(text: &str) -> InputValue {
    if let Ok(n) = text.parse::<f64>() {
        return InputValue::Number(n);
    }
    if let Some(hex) = text.strip_prefix('#') {
        let digits: Vec<u32> = hex.chars().filter_map(|c| c.to_digit(16)).collect();
        if digits.len() == 6 || digits.len() == 8 {
            let byte = |i: usize| (digits[i * 2] * 16 + digits[i * 2 + 1]) as f64 / 255.0;
            return InputValue::Color([byte(0), byte(1), byte(2), if digits.len() == 8 { byte(3) } else { 1.0 }]);
        }
    }
    if let Some((x, y)) = text.split_once(',') {
        if let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>()) {
            return InputValue::Vec2([x, y]);
        }
    }
    if let Some((id, rev)) = text.split_once(':').filter(|(id, _)| id.starts_with("ast_")) {
        return InputValue::Asset(AssetRef { id: id.into(), rev: rev.into() });
    }
    InputValue::Text(text.to_string())
}

fn media_arg(arg: &str) -> Result<(Media, PathBuf), String> {
    let (key, file) = arg.split_once('=').ok_or_else(|| format!("`{arg}` is id:rev=file"))?;
    let (id, rev) = key.split_once(':').ok_or_else(|| format!("`{key}` is id:rev"))?;
    Ok((Media::Asset(AssetRef { id: id.into(), rev: rev.into() }), PathBuf::from(file)))
}

fn load(path: &str, rest: &[String]) -> Result<MotionDoc, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut params = Vec::new();
    for pair in rest.windows(2).filter(|w| w[0] == "--param") {
        let (name, value) = pair[1].split_once('=').ok_or_else(|| format!("--param {}: expected name=value", pair[1]))?;
        params.push((makepad_motion::input_id(name), param_value(value)));
    }
    // `--lyrics id=file.json[@offset]`: a vj-lyrics document as voice `@id`
    // (its track starting `offset` seconds into the document).
    let mut lyric_voices = Vec::new();
    for pair in rest.windows(2).filter(|w| w[0] == "--lyrics") {
        let (id, file) = pair[1].split_once('=').ok_or_else(|| format!("--lyrics {}: expected id=file.json[@offset]", pair[1]))?;
        let (file, offset) = match file.rsplit_once('@') {
            Some((f, o)) if o.parse::<f64>().is_ok() => (f, o.parse::<f64>().unwrap()),
            _ => (file, 0.0),
        };
        let text = std::fs::read_to_string(file).map_err(|e| format!("--lyrics {file}: {e}"))?;
        lyric_voices.push((id.to_string(), makepad_motion::lyrics::voice_from_lyrics(&text, offset).map_err(|e| format!("--lyrics {file}: {e}"))?));
    }
    let with_lyrics = |mut features: AudioFeatures| {
        features.voices.extend(lyric_voices.iter().cloned());
        features
    };
    let context = match rest.windows(2).find(|w| w[0] == "--audio").map(|w| w[1].as_str()) {
        None if !lyric_voices.is_empty() => MotionContext::analyzed(with_lyrics(AudioFeatures::default()), None, 0.0),
        Some("synthetic") => {
            // `--voice "text" --wps 2.5 --voice-start 0.5 --bpm 120`: what the
            // synthetic voice says, how fast, from when, and the music's tempo.
            let value = |name: &str| rest.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
            let number = |name: &str, default: f64| value(name).and_then(|v| v.parse::<f64>().ok()).unwrap_or(default);
            let text = value("--voice").unwrap_or_else(|| "words the voice says here and there on the beat".into());
            MotionContext::analyzed(with_lyrics(AudioFeatures::synthetic(&text, number("--voice-start", 0.5), number("--wps", 2.5), number("--bpm", 120.0), 60.0)), None, 0.0)
        }
        // `--audio take.json`: real features (AudioFeatures::to_json), e.g. a
        // Kokoro take's words and envelope.
        Some(file) if file.ends_with(".json") => {
            let text = std::fs::read_to_string(file).map_err(|e| format!("--audio {file}: {e}"))?;
            MotionContext::analyzed(with_lyrics(AudioFeatures::from_json(&text)?), None, 0.0)
        }
        Some(other) => return Err(format!("--audio {other}: `synthetic` or a features `.json`")),
        None => MotionContext::default(),
    };
    // `--module id:rev=file`: a Splash module the document (or a module)
    // imports as `import(asset("id", "rev"))`.
    let mut modules = Vec::new();
    for pair in rest.windows(2).filter(|w| w[0] == "--module") {
        let (key, file) = pair[1].split_once('=').ok_or_else(|| format!("--module {}: expected id:rev=file", pair[1]))?;
        let (id, rev) = key.split_once(':').ok_or_else(|| format!("--module {key}: expected id:rev"))?;
        let text = std::fs::read_to_string(file).map_err(|e| format!("--module {file}: {e}"))?;
        modules.push((id.to_string(), rev.to_string(), text));
    }
    // `--lang zh`: the language the document renders in (`lang()`; default as written).
    let lang = rest.windows(2).find(|w| w[0] == "--lang").map(|pair| std::sync::Arc::<str>::from(pair[1].as_str()));
    // `--tree <dir>`: the document is a file of a DECK/CUT tree rooted at <dir>: imports and
    // `graphic("media/...")` resolve in that folder's snapshot (the engine's own tree reader).
    let modules = match tree_context(path, rest)? {
        Some(context) => makepad_motion::modules::Modules::from_tree(context),
        None => makepad_motion::modules::Modules::from_sources(modules.iter().map(|(i, r, t)| (i.as_str(), r.as_str(), t.as_str()))),
    };
    let context = MotionContext { modules, lang, ..context };
    MotionDoc::load_with(&source, &params, context).map_err(|diags| {
        diags.iter().map(|d| format!("{path}:{d}")).collect::<Vec<_>>().join("\n")
    })
}

fn parse(args: &[String]) -> Result<Job, String> {
    let mut args = args.iter().skip(1).filter(|a| !a.starts_with("--remote"));
    let mode = args.next().ok_or_else(usage)?.clone();
    if mode == "icons" {
        // The bundled icons (`<set>/<name>`), those containing a filter when given.
        let filter = args.next().cloned().unwrap_or_default().to_lowercase();
        let names: Vec<&str> = makepad_motion::icons::icon_names().filter(|n| n.contains(&filter)).collect();
        finish(Ok(names.join("\n")));
    }
    if mode == "animators" {
        // The named text animators: `Text{animate: @name}`.
        let lines: Vec<String> = makepad_motion::typography::animators().iter().map(|(name, look, suits)| format!("@{name}: {look} (suits {suits})")).collect();
        finish(Ok(lines.join("\n")));
    }
    if mode == "styles" {
        // The style kits: `Motion{style: @name}`.
        let lines: Vec<String> = makepad_motion::style::kits().iter().map(|k| format!("@{}: {} (suits {})", k.name, k.look, k.suits)).collect();
        finish(Ok(lines.join("\n")));
    }
    if mode == "style" {
        // One kit's parts as Splash, to paste in and edit as plain layers.
        let name = args.next().cloned().unwrap_or_default();
        let name = name.trim_start_matches('@');
        let size = match args.next().and_then(|s| s.split_once('x')).and_then(|(w, h)| Some([w.parse::<f64>().ok()?, h.parse::<f64>().ok()?])) {
            Some(size) => size,
            None => [1920.0, 1080.0],
        };
        match makepad_motion::style::detached_source(name, size) {
            Some(text) => finish(Ok(text)),
            None => {
                let names: Vec<String> = makepad_motion::style::kits().iter().map(|k| format!("@{}", k.name)).collect();
                finish(Err(format!("no style kit `{name}`; the kits are {}", names.join(" "))));
            }
        }
    }
    if mode == "fields" {
        // The fields a type takes (every Motion layer type, the document,
        // the 3D scene types): what each takes, its default, range and unit.
        let name = args.next().cloned();
        let field = args.next().cloned();
        finish(makepad_motion::field_registry::describe(name.as_deref(), field.as_deref()));
    }
    if mode == "characters" {
        // The Character kit: characters, poses, expressions, costumes, emotes, visemes, look fields.
        finish(Ok(makepad_motion::character::catalog()));
    }
    if mode == "character-svg" {
        // A kit character (with look changes `key=value`) as a character SVG with every part id.
        let rest: Vec<String> = std::env::args().skip(3).filter(|a| !a.starts_with("--remote")).collect();
        let name = std::env::args().nth(2).unwrap_or_default();
        finish(makepad_motion::character::kit_svg(&name, &rest));
    }
    if mode == "fonts" {
        // The bundled fonts and what each is for, then the families Stage
        // installs on demand (its Resources panel) and whether they are here.
        let mut lines: Vec<String> = Vec::new();
        for family in makepad_doc_fonts::FAMILIES {
            let aliases = if family.aliases.is_empty() { String::new() } else { format!("; alias {}", family.aliases.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", ")) };
            if family.bundled {
                lines.push(format!("{}: {}{aliases}", family.name, family.role));
            } else {
                let state = if makepad_doc_fonts::installed(family) { "installed".to_string() } else { "not installed: install it in Stage's Resources".to_string() };
                lines.push(format!("{}: {}{aliases} (external, {state})", family.name, family.role));
            }
        }
        finish(Ok(lines.join("\n")));
    }
    if mode == "kinetic" {
        // The kinetic type presets a KineticText can name.
        let names: Vec<&str> = makepad_content_packs::effects::presets().into_iter().map(|(name, _)| name)
            .filter(|name| makepad_content_packs::effects::is_text(name)).collect();
        finish(Ok(names.join("\n")));
    }
    if mode == "text3d" {
        // The Text3D presets (a text pack's) and materials.
        use makepad_motion::text3d::{presets, Family};
        let all = presets();
        let mut lines = Vec::new();
        if all.is_empty() {
            lines.push("no Text3D presets: they come with a text pack (Stage's kinetic text pack), which this tool has not installed".to_string());
        }
        for family in Family::ALL {
            lines.push(format!("## {}", family.name()));
            lines.extend(all.iter().filter(|p| p.family == family).map(|p| p.describe()));
        }
        lines.push(format!("materials: {}", makepad_motion::text3d::MATERIALS.iter().map(|m| format!("@{m}")).collect::<Vec<_>>().join(" ")));
        finish(Ok(lines.join("\n")));
    }
    let rest: Vec<String> = args.cloned().collect();
    let path = rest.first().ok_or_else(usage)?.clone();
    let value = |name: &str| rest.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
    let list = |name: &str| rest.windows(2).filter(|w| w[0] == name).map(|w| media_arg(&w[1])).collect::<Result<Vec<_>, _>>();
    // An editor's documents keep their literals as values of their own.
    let doc = if mode == "live" { makepad_motion::editable(|| load(&path, &rest))? } else { load(&path, &rest)? };
    let live = if mode == "live" {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        let doc_file = makepad_motion::schema::CodeFile::Doc;
        let (edited, patches, structural) = match value("--edit") {
            // The edited text as an editor's buffer gives it: what only
            // changed literals patches, anything else loads.
            Some(file) => {
                let edited = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
                match makepad_widgets::makepad_script::literal::literal_edits(&text, &edited) {
                    Some(edits) => {
                        let patches = edits.into_iter().map(|e| (LiteralSite { file: makepad_motion::DOC_CODE_FILE.into(), line: e.line, col: e.col }, e.value, format!("{:?}", e.value))).collect();
                        (edited, patches, false)
                    }
                    None => (edited, Vec::new(), true),
                }
            }
            None => {
                let sets = rest.windows(2).filter(|w| w[0] == "--set").map(|w| live_set(&text, &w[1])).collect::<Result<Vec<_>, _>>()?;
                if sets.is_empty() && !rest.iter().any(|a| a == "--edit-module") {
                    return Err("live: give the literals to change with --set line:col=value or --edit <edited copy>".into());
                }
                let mut edited = text.clone();
                let mut sorted: Vec<_> = sets.iter().collect();
                sorted.sort_by_key(|(range, _, _)| std::cmp::Reverse(range.start));
                for (range, _, written) in sorted {
                    edited.replace_range(range.clone(), written);
                }
                let patches = sets.iter().filter_map(|(range, value, written)| {
                    makepad_motion::schema::site_value(&doc_file, &text, range.clone(), *value).map(|(site, value)| (site, value, format!("`{}` -> {written}", &text[range.clone()])))
                }).collect();
                (edited, patches, false)
            }
        };
        // `--edit-module id:rev=file`: an edited copy of a module given with
        // `--module id:rev=…`: its literal-only change patched (sites in the
        // module, `module:id@rev`), and the reload reads the copy.
        let mut patches = patches;
        let mut structural = structural;
        let mut reload_rest = rest.clone();
        for pair in rest.windows(2).filter(|w| w[0] == "--edit-module") {
            let (key, file) = pair[1].split_once('=').ok_or_else(|| format!("--edit-module {}: expected id:rev=file", pair[1]))?;
            let original = rest.windows(2).find(|w| w[0] == "--module" && w[1].split_once('=').is_some_and(|(k, _)| k == key)).and_then(|w| w[1].split_once('=').map(|(_, f)| f.to_string()))
                .ok_or_else(|| format!("--edit-module {key}: give the module with --module {key}=file"))?;
            let old = std::fs::read_to_string(&original).map_err(|e| format!("{original}: {e}"))?;
            let new = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
            let (id, rev) = key.split_once(':').ok_or_else(|| format!("--edit-module {key}: expected id:rev"))?;
            match makepad_widgets::makepad_script::literal::literal_edits(&old, &new) {
                Some(edits) => patches.extend(edits.into_iter().map(|e| (LiteralSite { file: format!("module:{id}@{rev}"), line: e.line, col: e.col }, e.value, format!("module {key}: {:?}", e.value)))),
                None => structural = true,
            }
            for w in reload_rest.iter_mut() {
                if w.split_once('=').is_some_and(|(k, _)| k == key) && w.ends_with(original.as_str()) {
                    *w = format!("{key}={file}");
                }
            }
        }
        let scratch = std::env::temp_dir().join(format!("makepad-motion-live-{}.splash", std::process::id()));
        std::fs::write(&scratch, &edited).map_err(|e| format!("{}: {e}", scratch.display()))?;
        Some(Live { patches, structural, reload: Some((scratch, reload_rest)), folded: rest.iter().any(|a| a == "--folded"), counts: (0, 0) })
    } else {
        None
    };
    if mode == "scene3d" {
        // What every Text3D and Seq3D layer draws: its motion3d scene source
        // and the inputs the layer gives it (a Text3D's resolved look, read
        // by its preset in mod.motion.modules.text3d). A Scene3D the
        // document wrote is part of the document itself.
        let mut out = Vec::new();
        for layer in doc.all_layers() {
            if let makepad_motion::LayerKind::Scene3dDoc { scene, loc, .. } = &layer.kind {
                let name = layer.name.clone().unwrap_or_else(|| layer.kind.type_name().to_string());
                match scene {
                    makepad_motion::SceneSource::Template { source, inputs } => {
                        out.push(format!("// {name}\n{source}"));
                        for (name, value) in inputs {
                            out.push(format!("// in(@{name}) = {value:?}"));
                        }
                    }
                    makepad_motion::SceneSource::Object(_) => out.push(format!("// {name}: the document's own Scene3D, written at line {} column {}", loc.line, loc.col)),
                }
            }
        }
        finish(Ok(out.join("\n")));
    }
    if mode == "check" {
        let mut report = format!("{path}: ok, {} layers, {}x{} units, {} s at {} fps", doc.all_layers().len(), doc.size[0], doc.size[1], doc.duration, doc.fps);
        for warning in &doc.warnings {
            report.push_str(&format!("\n{path}:{warning}"));
        }
        // Layout checks on resolved frames (text overlaps, lines through
        // text, fills over text, text under the minimum size, cut off).
        let times = makepad_motion::inspect::sample_times(&doc, 10.0);
        // The 3D scenes as motion3d reads them (a field a scene type does
        // not read, a file that does not load), with the files given as
        // `--model`/`--image`/`--file id:rev=file`.
        let mut assets = std::collections::HashMap::new();
        for (media, file) in list("--model")?.into_iter().chain(list("--image")?).chain(list("--file")?) {
            if let Media::Asset(asset) = media {
                assets.insert(format!("{}@{}", asset.id, asset.rev), file.clone());
                assets.insert(asset.id, file);
            }
        }
        let base = std::path::Path::new(&path).parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        let resolver = makepad_stage_motion3d::doc::FileResolver { base, assets, library: None };
        for diagnostic in makepad_motion::scene::scene_diagnostics(&doc, &times, &resolver) {
            report.push_str(&format!("\n{path}:{diagnostic}"));
        }
        // Errors frames report while evaluating, once each.
        let mut seen = std::collections::BTreeSet::new();
        for &t in &times {
            for error in doc.eval(t).errors {
                let text = error.to_string();
                if seen.insert(text.clone()) {
                    report.push_str(&format!("\n{path}: error at {t:.2} s: {text}"));
                }
            }
        }
        let places: std::collections::HashMap<String, (u32, u32)> = doc.all_layers().iter().map(|l| (l.path.clone(), (l.loc.line, l.loc.col))).collect();
        for issue in makepad_motion::inspect::layout_checks(&doc, &times, makepad_motion::inspect::DEFAULT_MIN_TEXT_PX) {
            let (line, col) = places.get(&issue.path).or_else(|| issue.path.split_once('#').and_then(|(base, _)| places.get(base))).copied().unwrap_or((0, 0));
            let other = issue.other.as_ref().map(|o| format!(" (with `{o}`)")).unwrap_or_default();
            report.push_str(&format!("\n{path}:{line}:{col}: warning[{}] `{}`{other} {:.2}-{:.2} s: {}\n    fix: {}", issue.code, issue.path, issue.from, issue.to, issue.message, issue.hint));
        }
        finish(Ok(report));
    }
    let (times, output, default_width) = match mode.as_str() {
        "frames" => {
            let times = value("--at").ok_or_else(usage)?.split(',').map(|t| t.trim().parse::<f64>().map_err(|_| format!("--at: `{t}`"))).collect::<Result<Vec<_>, _>>()?;
            (times, Output::Frames(PathBuf::from(value("--out").ok_or_else(usage)?)), 960)
        }
        "strip" => {
            let count = value("--count").map_or(Ok(8), |c| c.parse::<usize>().map_err(|_| format!("--count: `{c}`")))?.max(1);
            let times = (0..count).map(|k| doc.duration * (k as f64 + 0.5) / count as f64).collect();
            (times, Output::Strip(PathBuf::from(value("--out").ok_or_else(usage)?)), 480)
        }
        // The library's thumbnail of a Motion asset: the frame at half its duration.
        "thumbnail" => (vec![doc.duration * 0.5], Output::Strip(PathBuf::from(value("--out").ok_or_else(usage)?)), 480),
        // Every frame in order, as a preview plays them.
        "bench" => (doc.frame_times(), Output::Bench, 1920),
        // The frame, patched, and from the edited text.
        "live" => {
            // One time (drawn three times), or the three frames' own times.
            let at = value("--at").ok_or_else(usage)?.split(',').map(|t| t.trim().parse::<f64>().map_err(|_| format!("--at: `{t}`"))).collect::<Result<Vec<_>, _>>()?;
            let times = match at[..] {
                [t] => vec![t, t, t],
                [a, b, c] => vec![a, b, c],
                _ => return Err("live: --at t or --at t1,t2,t3".into()),
            };
            (times, Output::Frames(PathBuf::from(value("--out").ok_or_else(usage)?)), 960)
        }
        _ => return Err(usage()),
    };
    let width = value("--width").map_or(Ok(default_width), |w| w.parse::<u32>().map_err(|_| format!("--width: `{w}`")))?;
    let svgs = list("--svg")?;
    // A `Character{src: asset(…)}` is built from its SVG at load time.
    let mut doc = doc;
    let problems = doc.resolve_characters(&|asset| {
        svgs.iter().find(|(media, _)| *media == Media::Asset(asset.clone())).and_then(|(_, file)| std::fs::read_to_string(file).ok())
    });
    if let Some(problem) = problems.first() {
        return Err(format!("{problem} (pass it with --svg id:rev=file)"));
    }
    let stall = value("--stall").map_or(Ok(120.0), |s| s.parse::<f64>().map_err(|_| format!("--stall: `{s}`")))?.max(1.0);
    let realtime = std::env::args().any(|a| a == "--realtime");
    let pick = rest.windows(2).filter(|w| w[0] == "--pick").map(|w| {
        let p = &w[1];
        p.split_once(',').and_then(|(x, y)| Some([x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?])).ok_or_else(|| format!("--pick {p}: expected x,y"))
    }).collect::<Result<Vec<_>, _>>()?;
    let mut times = times;
    let warm = match value("--warm") {
        Some(step) => {
            let step = step.parse::<f64>().ok().filter(|s| *s > 0.0).ok_or_else(|| format!("--warm: `{step}`"))?;
            // Every `step` s and each key and span boundary (as `inspect` samples).
            let ahead = makepad_motion::inspect::sample_times(&doc, 1.0 / step);
            let n = ahead.len();
            times.splice(0..0, ahead);
            n
        }
        None => 0,
    };
    let ambient = value("--ambient").map(|s| s.parse::<f64>().map_err(|_| format!("--ambient: `{s}` is not a number"))).transpose()?;
    Ok(Job { doc, times, width, output, images: { let mut images = list("--image")?; images.extend(tree_images(&rest)?); images }, svgs, models: list("--model")?, files: list("--file")?, stall, realtime, warm, live, ambient, pick })
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn write_png(path: &std::path::Path, rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    let options = EncoderOptions::default().set_width(width as usize).set_height(height as usize).set_depth(BitDepth::Eight).set_colorspace(ColorSpace::RGBA);
    let mut png = Vec::new();
    PngEncoder::new(rgba, options).encode(&mut png).map_err(|e| format!("png: {e:?}"))?;
    std::fs::write(path, png).map_err(|e| format!("{}: {e}", path.display()))
}

enum Step {
    Record,
    Paint,
    Read(Option<ReadbackTicket>),
    /// `--pick`: the last frame's id image asked for.
    Pick(PickTicket),
}

#[derive(Script, ScriptHook, Widget)]
pub struct MotionHost {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawQuad,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    job: Option<Job>,
    #[rust]
    motion: Option<MotionDraw>,
    #[rust]
    index: usize,
    #[rust(Step::Record)]
    step: Step,
    #[rust]
    pixels: (u32, u32),
    #[rust]
    report: Vec<String>,
    #[rust]
    sheet: Vec<Vec<u8>>,
    /// Per frame: evaluate ms, record ms (every recording), record→painted ms.
    #[rust]
    timings: Vec<(f64, f64, f64)>,
    #[rust]
    frame_started: f64,
    #[rust]
    started: f64,
    /// The frame's readback waits at the context's request limit: said once, and the per-frame
    /// stall does not count the wait.
    #[rust]
    readback_wait_logged: bool,
    // `MAKEPAD_MOTION_GPU=1`: GPU ms per pass name (total, samples).
    #[rust]
    gpu: GpuByPass,
    // `MAKEPAD_MOTION_FRAMES=1`: GPU ms per frame (all passes), for the per-frame rows.
    #[rust]
    gpu_frames: Vec<f64>,
    /// Each frame's RGBA hash, in order.
    #[rust]
    hashes: Vec<u64>,
    /// `--pick`: when the id image was asked for.
    #[rust]
    pick_asked: f64,
    /// `--pick`: the last frame is drawn again after the pick, which must leave it as it was.
    #[rust]
    after_pick: bool,
    /// Realtime: timed frames shown before everything they draw was ready
    /// (a glyph or draw program first used), as a preview shows them.
    #[rust]
    pops: Vec<f64>,
    /// `MAKEPAD_MOTION_GLYPHS=1`: the font service's counters at the last
    /// frame, for the per-frame glyph rows.
    #[rust]
    glyph_stats: Option<GlyphCounters>,
}

/// What the glyph rows compare frame to frame: the host's font service
/// (requests, curve sets made, evictions, slug store occupancy) and this
/// session's glyph client (font instances, entries held, waits, blanks).
#[derive(Clone, Copy, Default)]
struct GlyphCounters {
    requests: u64,
    rasters: u64,
    evictions: u64,
    evicted: u64,
    epoch: u64,
    blank_frames: u64,
}

/// One `glyphs <t> ...` row: this frame's deltas and the store's state.
fn glyph_row(cx: &mut Cx, t: f64, last: &mut Option<GlyphCounters>) -> String {
    let host = makepad_widgets::makepad_platform::makepad_hal::Hal::global().font_stats();
    let fonts = cx.get_global::<std::rc::Rc<std::cell::RefCell<makepad_widgets::text::fonts::Fonts>>>().clone();
    let fonts = fonts.borrow();
    let r = fonts.rasterizer().borrow();
    let client = r.client();
    let now = GlyphCounters { requests: host.glyph_requests, rasters: host.rasters, evictions: host.evictions, evicted: host.evicted_slots, epoch: r.epoch(), blank_frames: cx.blank_glyph_frames() };
    let was = last.replace(now).unwrap_or(now);
    format!(
        "glyphs {t:.3} requested +{} made +{} evictions +{} evicted +{} epoch +{} blank_frames +{} | store {} glyphs {}/{} curves | instances {} held {} pending {}",
        now.requests - was.requests, now.rasters - was.rasters, now.evictions - was.evictions, now.evicted - was.evicted, now.epoch - was.epoch, now.blank_frames - was.blank_frames,
        host.slug_glyphs, host.slug_curves, host.slug_capacity, client.fonts_open(), client.held(), client.pending()
    )
}

impl MotionHost {
    fn start(&mut self, cx: &mut Cx, job: Job) {
        if !job.pick.is_empty() {
            cx.set_pick_ids(true);
        }
        if job.live.as_ref().is_some_and(|live| !live.folded) {
            cx.set_live_literals(vec![makepad_motion::DOC_CODE_FILE.to_string()]);
        }
        let mut motion = MotionDraw::new(cx);
        for (media, path) in &job.images {
            let bytes = std::fs::read(path).unwrap_or_else(|e| finish(Err(format!("{}: {e}", path.display()))));
            let (pixels, w, h) = decode_image(&bytes).unwrap_or_else(|e| finish(Err(format!("{}: {e}", path.display()))));
            motion.set_image(cx, media.clone(), pixels, w, h);
        }
        for (media, path) in &job.svgs {
            let markup = std::fs::read_to_string(path).unwrap_or_else(|e| finish(Err(format!("{}: {e}", path.display()))));
            motion.set_svg(cx, media.clone(), &markup);
        }
        for (media, path) in &job.models {
            let bytes = std::fs::read(path).unwrap_or_else(|e| finish(Err(format!("{}: {e}", path.display()))));
            if let Media::Asset(asset) = media {
                motion.set_file(asset.clone(), bytes.clone());
            }
            if let Err(error) = motion.set_model(cx, media.clone(), &bytes) {
                finish(Err(format!("{}: {error}", path.display())));
            }
        }
        // `--file id:rev=path`: any library file a full Scene3D names.
        for (media, path) in &job.files {
            let bytes = std::fs::read(path).unwrap_or_else(|e| finish(Err(format!("{}: {e}", path.display()))));
            if let Media::Asset(asset) = media {
                motion.set_file(asset.clone(), bytes);
            }
        }
        let w = job.width.max(2);
        let h = ((w as f64 * job.doc.size[1] / job.doc.size[0]).round() as u32).max(2);
        self.pixels = (w, h);
        if let Output::Frames(dir) = &job.output {
            let _ = std::fs::create_dir_all(dir);
        }
        self.motion = Some(motion);
        self.job = Some(job);
        self.started = Cx::monotonic_now();
        self.next_frame = cx.new_next_frame();
    }

    fn done(&mut self) -> ! {
        let job = self.job.as_ref().unwrap();
        if let Some(motion) = &self.motion {
            for error in motion.errors() {
                self.report.push(format!("error {error}"));
            }
        }
        if let Output::Strip(path) = &job.output {
            let (w, h) = self.pixels;
            let cols = self.sheet.len().min(4).max(1);
            let rows = self.sheet.len().div_ceil(cols);
            let (sw, sh) = (w as usize * cols, h as usize * rows);
            let mut sheet = vec![0u8; sw * sh * 4];
            for (k, frame) in self.sheet.iter().enumerate() {
                let (cx0, cy0) = ((k % cols) * w as usize, (k / cols) * h as usize);
                for y in 0..h as usize {
                    let src = &frame[y * w as usize * 4..(y + 1) * w as usize * 4];
                    let at = ((cy0 + y) * sw + cx0) * 4;
                    sheet[at..at + src.len()].copy_from_slice(src);
                }
            }
            if let Err(error) = write_png(path, &sheet, sw as u32, sh as u32) {
                finish(Err(error));
            }
            self.report.push(format!("strip {} frames {}x{} -> {}", self.sheet.len(), sw, sh, path.display()));
        }
        if !self.timings.is_empty() {
            // The first frames compile shaders and rasterise glyphs: steady state after them.
            let skip = if job.warm > 0 { job.warm } else if self.timings.len() > 4 { 2 } else { 0 };
            let steady: Vec<&(f64, f64, f64)> = self.timings.iter().skip(skip).collect();
            let n = steady.len().max(1) as f64;
            let mean = |f: &dyn Fn(&(f64, f64, f64)) -> f64| steady.iter().map(|t| f(t)).sum::<f64>() / n;
            let mut frames: Vec<f64> = steady.iter().map(|t| t.2).collect();
            frames.sort_by(f64::total_cmp);
            let p95 = frames.get(((frames.len() as f64 * 0.95) as usize).min(frames.len().saturating_sub(1))).copied().unwrap_or(0.0);
            let evals: Vec<f64> = steady.iter().map(|t| t.0).collect();
            let max_eval = evals.iter().copied().fold(0.0, f64::max);
            self.report.push(format!(
                "timing {} frames: evaluate {:.2} ms (max {:.2}), record {:.2} ms, record to painted {:.2} ms (p95 {:.2})",
                steady.len(), mean(&|t| t.0), max_eval, mean(&|t| t.1), mean(&|t| t.2), p95
            ));
            if job.realtime {
                let at: Vec<String> = self.pops.iter().take(12).map(|t| format!("{t:.2}")).collect();
                self.report.push(format!("realtime: {} frames shown before a glyph or draw program they use was ready{}{}",
                    self.pops.len(), if at.is_empty() { "" } else { " at " }, at.join(", ")));
            }
        }
        if std::env::var_os("MAKEPAD_MOTION_FRAMES").is_some() {
            // One row per frame: `frame <t> <evaluate> <record> <painted> <gpu>` (ms;
            // gpu from MAKEPAD_MOTION_GPU, attributed to the frame being made when
            // the samples arrived).
            for (k, t) in self.timings.iter().enumerate().skip(job.warm) {
                let at = job.times.get(k).copied().unwrap_or(0.0);
                let gpu = self.gpu_frames.get(k).copied().unwrap_or(0.0);
                self.report.push(format!("frame {at:.3} {:.2} {:.2} {:.2} {gpu:.2}", t.0, t.1, t.2));
            }
        }
        if !self.gpu.is_empty() {
            let mut rows: Vec<(&String, &(f64, usize))> = self.gpu.iter().collect();
            rows.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0));
            let frames = self.index.max(1) as f64;
            self.report.push(format!("gpu per frame by pass (ms, {} frames):", self.index));
            for (name, (total, n)) in rows.iter().take(30) {
                self.report.push(format!("  {:>8.3} ms  {:>6} samples  {name}", total / frames, n));
            }
        }
        self.report.push(format!("motion: done {} frames {}x{}", self.index, self.pixels.0, self.pixels.1));
        finish(Ok(self.report.join("\n")))
    }

    /// Time every pass on the GPU and gather its samples by name.
    fn gpu_times(&mut self, cx: &mut Cx) {
        let mut samples = Vec::new();
        for id in cx.passes.id_iter().collect::<Vec<_>>() {
            cx.passes[id].set_gpu_timing_enabled(true);
            samples.clear();
            cx.passes[id].drain_gpu_time_samples(&mut samples);
            if !samples.is_empty() {
                let e = self.gpu.entry(cx.passes[id].debug_name.clone()).or_insert((0.0, 0));
                for (_, ms) in &samples {
                    e.0 += ms;
                    e.1 += 1;
                }
                let sum: f64 = samples.iter().map(|(_, ms)| ms).sum();
                if self.gpu_frames.len() <= self.index {
                    self.gpu_frames.resize(self.index + 1, 0.0);
                }
                self.gpu_frames[self.index] += sum;
            }
        }
    }

    fn check(&mut self, cx: &mut Cx) {
        if std::env::var_os("MAKEPAD_MOTION_GPU").is_some() {
            self.gpu_times(cx);
        }
        let Some(job) = &self.job else { return };
        // Per frame: a long run of frames is fine, one frame stuck is not.
        if !self.readback_wait_logged && Cx::monotonic_now() - self.started > job.stall {
            let waiting = self.motion.as_ref().map_or(Vec::new(), |m| m.waiting_for(&{ let t = job.times[self.index.min(job.times.len() - 1)]; job.doc.eval_live(t, job.ambient.unwrap_or(t)) }));
            let state = self.motion.as_ref().map_or(String::new(), |m| m.debug_state(cx));
            finish(Err(format!("no frame after {} s (waiting for {waiting:?}; {state}); a frame that takes longer (many motion blur samples at a large size) can be given --stall <seconds>", job.stall)));
        }
        if let Step::Pick(ticket) = self.step {
            match cx.take_pick(ticket) {
                Some(Ok(image)) => {
                    self.report.push(format!("pick: the id image came {:.1} ms after it was asked for", (Cx::monotonic_now() - self.pick_asked) * 1000.0));
                    self.picked(&image);
                    self.after_pick = true;
                    self.index -= 1;
                    self.started = Cx::monotonic_now();
                    self.step = Step::Record;
                    return;
                }
                Some(Err(error)) => finish(Err(format!("pick: {error:?}"))),
                None => return,
            }
        }
        let Step::Read(Some(ticket)) = self.step else { return };
        let Some(result) = cx.try_take_texture_readbacks_for(&[ticket]).into_iter().next() else { return };
        let data = match result.data {
            Ok(data) => data,
            Err(error) => finish(Err(format!("readback: {error}"))),
        };
        let rgba = data.to_vec();
        let t = job.times[self.index];
        let hash = fnv1a(&rgba);
        if self.after_pick {
            self.report.push(format!("after the pick: frame {t} {hash:016x}"));
            self.done();
        }
        self.hashes.push(hash);
        match &job.output {
            Output::Frames(dir) => {
                let file = dir.join(format!("frame_{:03}_{:06.3}.png", self.index, t));
                if let Err(error) = write_png(&file, &rgba, result.width as u32, result.height as u32) {
                    finish(Err(error));
                }
                self.report.push(format!("frame {t} {hash:016x} {}", file.display()));
            }
            Output::Strip(_) => {
                self.report.push(format!("frame {t} {hash:016x}"));
                self.sheet.push(rgba);
            }
            Output::Bench => {}
        }
        self.index += 1;
        self.started = Cx::monotonic_now();
        self.step = Step::Record;
        self.live_step(cx);
        let Some(job) = &self.job else { return };
        if self.index >= job.times.len() {
            // What the last frame recorded: pick ids must not split it into more draw calls.
            let calls: usize = cx.gpu_inventory().draw_lists.iter().map(|l| l.draw_calls).sum();
            self.report.push(format!("draw calls {calls}"));
            if !job.pick.is_empty() {
                if let Some(output) = self.motion.as_ref().and_then(|m| m.output().cloned()) {
                    self.step = Step::Pick(cx.request_pick(&output));
                    self.started = Cx::monotonic_now();
                    self.pick_asked = self.started;
                    return;
                }
            }
            self.done();
        }
    }

    /// `--pick`: the ids under each place asked for, and what they name: the
    /// item, the Draw2D op and the code that drew it.
    fn picked(&mut self, image: &makepad_widgets::PickImage) {
        let Some(job) = &self.job else { return };
        let t = *job.times.last().unwrap_or(&0.0);
        let frame = job.doc.eval_traced(t);
        for p in job.pick.clone() {
            self.picked_at(image, &frame, p);
        }
    }

    fn picked_at(&mut self, image: &makepad_widgets::PickImage, frame: &makepad_motion::MotionFrame, p: [f64; 2]) {
        let Some(job) = &self.job else { return };
        let (px, py) = ((p[0] / frame.size[0] * image.width as f64) as usize, (p[1] / frame.size[1] * image.height as f64) as usize);
        let ids = image.ids_near(px, py, 2);
        let covered = image.bytes.chunks(4).filter(|c| c[0] != 0 || c[1] != 0 || c[2] != 0).count();
        self.report.push(format!("pick at {},{} (pixel {px},{py} of {}x{}): ids {:?} ({} of the picture's pixels named)", p[0], p[1], image.width, image.height, ids, covered));
        let Some(motion) = &self.motion else { return };
        for id in ids.iter().take(3) {
            let Some((path, op)) = motion.pick_name(*id) else {
                self.report.push(format!("pick id {id}: names nothing"));
                continue;
            };
            let mut code = String::new();
            if let (Some(op), Some(item)) = (op, find_item(frame, path)) {
                if let makepad_motion::ItemKind::Draw2d { picture, .. } = &item.kind {
                    let list = &picture.list;
                    if let Some(mark) = list.marks.iter().rev().find(|m| (m.op as usize) <= *op) {
                        for at in job.doc.code_at(&mark.sites).into_iter().flatten() {
                            code.push_str(&format!(" <- {:?} {}:{} `{}`", at.file, at.line, at.col, at.line_text.as_deref().unwrap_or("").trim()));
                        }
                    }
                    if let Some(o) = list.ops.get(*op) {
                        let what = match o {
                            makepad_motion::draw2d::Op::Text(t) => format!("text {:?}", t.text),
                            makepad_motion::draw2d::Op::Segs { start, end, .. } => format!("{} segs", end - start),
                            makepad_motion::draw2d::Op::Fill { .. } => "fill".into(),
                            _ => "op".into(),
                        };
                        code = format!(" ({what}){code}");
                    }
                }
            }
            self.report.push(format!("pick id {id}: {path} op {op:?}{code}"));
        }
    }

    /// `live`: what happens between its frames. After the first, the
    /// literals are patched (the running document and its shader slots);
    /// after the second, the document loaded from the edited text takes
    /// over. Each says what was compiled since.
    fn live_step(&mut self, cx: &mut Cx) {
        let Some(job) = &mut self.job else { return };
        let Some(live) = &mut job.live else { return };
        let counts = (cx.draw_shaders.shaders.len(), cx.gpu_pipeline_count());
        match self.index {
            1 => {
                let sites = cx.shader_literal_sites();
                let mut lines: Vec<u32> = sites.iter().map(|(_, s, _)| s.line + 1).collect();
                lines.dedup();
                self.report.push(format!("live sites in shaders: {} (lines {:?})", sites.len(), lines));
                if live.structural {
                    self.report.push("live edit: not only literals changed: the reload shows it".into());
                }
                let started = Cx::monotonic_now();
                for (site, value, what) in &live.patches {
                    let reach = cx.patch_literal(site, *value);
                    let reload = if reach.needs_reload() { format!(", reload: {}", reach.reload.join("; ")) } else { String::new() };
                    self.report.push(format!("live set {}:{} {what}: {} places{reload}", site.line + 1, site.col + 1, reach.places));
                }
                // What the loader copied out of the patched values (a layer's
                // static fields): the model read again from the same VM.
                let reread_at = Cx::monotonic_now();
                match job.doc.reread() {
                    Ok(doc) => job.doc = doc,
                    Err(diags) => finish(Err(format!("{diags:?}"))),
                }
                self.report.push(format!("live model reread took {:.3} ms", (Cx::monotonic_now() - reread_at) * 1000.0));
                self.report.push(format!("live patch took {:.3} ms", (Cx::monotonic_now() - started) * 1000.0));
                live.counts = counts;
            }
            2 => {
                let painted = self.timings.get(1).map_or(0.0, |t| t.2);
                self.report.push(format!(
                    "live patched frame: shaders compiled {} (GPU programs {}), its record to painted {:.2} ms",
                    counts.0 - live.counts.0, counts.1 - live.counts.1, painted
                ));
                live.counts = counts;
                if let Some((path, rest)) = live.reload.take() {
                    let loading = Cx::monotonic_now();
                    match makepad_motion::editable(|| load(&path.to_string_lossy(), &rest)) {
                        Ok(doc) => job.doc = doc,
                        Err(error) => finish(Err(error)),
                    }
                    self.report.push(format!("live reload: the edited text loaded in {:.1} ms", (Cx::monotonic_now() - loading) * 1000.0));
                    let _ = std::fs::remove_file(&path);
                }
            }
            3 => {
                let painted = self.timings.get(2).map_or(0.0, |t| t.2);
                self.report.push(format!(
                    "live reloaded frame: shaders compiled {} (GPU programs {}), its record to painted {:.2} ms",
                    counts.0 - live.counts.0, counts.1 - live.counts.1, painted
                ));
                // The code is the truth: the text loaded again draws what the
                // patches drew.
                let same_time = job.times.get(1) == job.times.get(2);
                if let (true, Some(patched), Some(loaded)) = (same_time, self.hashes.get(1), self.hashes.get(2)) {
                    self.report.push(format!("live check: the patched frame {} the frame the edited text loads", if patched == loaded { "equals" } else { "DIFFERS from" }));
                }
            }
            _ => {}
        }
    }
}

impl Widget for MotionHost {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Some(motion) = &mut self.motion {
            motion.handle_event(cx, event);
        }
        if self.next_frame.is_event(event).is_some() {
            self.check(cx);
            self.draw_bg.redraw(cx);
            self.next_frame = cx.new_next_frame();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.draw_bg.draw_walk(cx, walk);
        let (Some(job), Some(motion)) = (&self.job, &mut self.motion) else { return DrawStep::done() };
        match self.step {
            Step::Record => {
                let t0 = Cx::monotonic_now();
                let frame = { let t = job.times[self.index]; job.doc.eval_live(t, job.ambient.unwrap_or(t)) };
                let t1 = Cx::monotonic_now();
                if !frame.errors.is_empty() {
                    for error in &frame.errors {
                        self.report.push(format!("error at {}: {error}", frame.t));
                    }
                }
                let missing = motion.missing(&frame);
                if !missing.is_empty() {
                    finish(Err(format!("the document needs {missing:?}: pass them with --image/--svg/--model")));
                }
                if !motion.ready() {
                    return DrawStep::done();
                }
                // Frames are pictures of their exact time (locked time): an
                // HDR document with motion blur samples its shutter from the
                // document.
                motion.set_locked_time(!job.realtime);
                let doc = job.doc.clone();
                let ambient = job.ambient;
                motion.set_frames(Some(std::sync::Arc::new(move |t| doc.eval_live(t, ambient.unwrap_or(t)))));
                motion.draw(cx, &frame, self.pixels);
                if std::env::var_os("MAKEPAD_MOTION_BOUNDS").is_some() {
                    // Each item's box as an editor selects it (lane M5's
                    // selection test): `bounds <t> <path> x0 y0 x1 y1`.
                    for (_, item) in frame.walk() {
                        if let Some(b) = motion.item_bounds(cx, item) {
                            self.report.push(format!("bounds {} {} {:.2} {:.2} {:.2} {:.2}", frame.t, item.path, b[0], b[1], b[2], b[3]));
                        }
                    }
                }
                let t2 = Cx::monotonic_now();
                // The frame drawn again after a pick is not timed.
                if !self.after_pick {
                    if self.timings.len() <= self.index {
                        self.timings.push((0.0, 0.0, 0.0));
                        self.frame_started = t0;
                    }
                    let timing = &mut self.timings[self.index];
                    timing.0 += (t1 - t0) * 1000.0;
                    timing.1 += (t2 - t1) * 1000.0;
                }
                self.step = Step::Paint;
            }
            Step::Paint => {
                if motion.painted(cx) {
                    // Glyphs a face draws for the first time arrive the frame
                    // after: record again until nothing was missing. A
                    // realtime bench moves on as a playing preview does (it
                    // shows the frame) and counts it; a warm frame (drawn
                    // ahead, unseen) is done once painted.
                    let exact = motion.exact(cx);
                    let warm = self.index < job.warm;
                    if !exact && job.realtime && !warm && !self.after_pick {
                        self.pops.push(job.times[self.index]);
                    }
                    // Realtime frames are taken as a playing preview shows
                    // them: painted once, exact or not.
                    let accept = exact || job.realtime;
                    self.step = if accept { Step::Read(None) } else { Step::Record };
                    if accept {
                        if std::env::var_os("MAKEPAD_MOTION_GLYPHS").is_some() {
                            let row = glyph_row(cx, job.times[self.index], &mut self.glyph_stats);
                            self.report.push(row);
                        }
                        if let Some(timing) = self.timings.get_mut(self.index).filter(|_| !self.after_pick) {
                            timing.2 = (Cx::monotonic_now() - self.frame_started) * 1000.0;
                        }
                        if matches!(job.output, Output::Bench) {
                            self.index += 1;
                            self.started = Cx::monotonic_now();
                            self.step = Step::Record;
                            if self.index >= job.times.len() {
                                self.done();
                            }
                            self.draw_bg.redraw(cx);
                            return DrawStep::done();
                        }
                    }
                    if !accept {
                        self.draw_bg.redraw(cx);
                    }
                } else {
                    motion.reattach(cx);
                }
            }
            Step::Read(None) => {
                let Some(texture) = motion.output().cloned() else { return DrawStep::done() };
                match texture.read_back(cx, ReadbackRequest::default()) {
                    Ok(ticket) => {
                        if self.readback_wait_logged {
                            self.started = Cx::monotonic_now();
                        }
                        self.readback_wait_logged = false;
                        self.step = Step::Read(Some(ticket));
                    }
                    // the context's request limit is reached: asked again at the next draw (a
                    // frame over the byte budget gets its ticket and waits for room)
                    Err(ReadbackError::Backpressure) => {
                        if !self.readback_wait_logged {
                            log!("makepad-motion: context readback request limit reached; frame {} waits", self.index);
                            self.readback_wait_logged = true;
                        }
                    }
                    Err(error) => finish(Err(format!("readback: {error}"))),
                }
            }
            Step::Read(Some(_)) => {}
            Step::Pick(_) => motion.reattach(cx),
        }
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let args: Vec<String> = std::env::args().collect();
        let job = match parse(&args) {
            Ok(job) => job,
            Err(error) => finish(Err(error)),
        };
        if let Some(mut host) = self.ui.widget(cx, ids!(host)).borrow_mut::<MotionHost>() {
            host.start(cx, job);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        // `--pick`: every shader compiles its pick variant.
        if std::env::args().any(|a| a == "--pick") {
            vm.host.cx_mut().enable_pick_variants();
        }
        makepad_widgets::script_mod(vm);
        makepad_content_packs::script_mod(vm);
        makepad_motion_draw::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

/// The item at `path` in `frame`: its layers, the pictures its scenes and
/// shaders draw (data layers, a Scene3D's `layers:`), and inside groups.
fn find_item<'a>(frame: &'a makepad_motion::MotionFrame, path: &str) -> Option<&'a makepad_motion::Item> {
    fn go<'a>(items: &'a [makepad_motion::Item], path: &str) -> Option<&'a makepad_motion::Item> {
        for item in items {
            if item.path == path {
                return Some(item);
            }
            let inner = match &item.kind {
                makepad_motion::ItemKind::Group { children, mask, .. } => go(children, path).or_else(|| mask.as_deref().and_then(|m| go(std::slice::from_ref(m), path))),
                makepad_motion::ItemKind::Scene3dDoc { layers, .. } => layers.iter().find_map(|(_, items)| go(items, path)),
                makepad_motion::ItemKind::Fly { inner, .. } | makepad_motion::ItemKind::Anchored { inner, .. } => go(std::slice::from_ref(inner.as_ref()), path),
                _ => None,
            };
            if inner.is_some() {
                return inner;
            }
        }
        None
    }
    go(&frame.items, path).or_else(|| frame.data_layers.iter().find_map(|(_, items)| go(items, path)))
}


/// `--tree <dir>`: the folder's snapshot, with this document as the one being loaded.
fn tree_context(path: &str, rest: &[String]) -> Result<Option<makepad_motion::tree::TreeContext>, String> {
    let Some(root) = rest.windows(2).find(|w| w[0] == "--tree").map(|w| PathBuf::from(&w[1])) else { return Ok(None) };
    let doc = std::fs::canonicalize(path).ok()
        .and_then(|p| std::fs::canonicalize(&root).ok().and_then(|r| p.strip_prefix(&r).ok().map(|q| q.to_string_lossy().replace('\\', "/"))))
        .ok_or_else(|| format!("{path} is not inside the tree {}", root.display()))?;
    let tree = makepad_motion::tree::read_folder(&root, "deck.splash", makepad_asset_data::tree::TreeRole::Deck)?;
    Ok(Some(makepad_motion::tree::TreeContext { resolver: std::sync::Arc::new(tree), document: doc }))
}

/// The tree's images as `--image` media, so `graphic("media/x.png")` draws its pixels.
fn tree_images(rest: &[String]) -> Result<Vec<(Media, PathBuf)>, String> {
    let Some(root) = rest.windows(2).find(|w| w[0] == "--tree").map(|w| PathBuf::from(&w[1])) else { return Ok(Vec::new()) };
    let tree = makepad_motion::tree::read_folder(&root, "deck.splash", makepad_asset_data::tree::TreeRole::Deck)?;
    let mut out = Vec::new();
    for (logical, entry) in &tree.tree.entries {
        if makepad_motion::tree::file_kind(logical) != makepad_asset_data::AssetKind::Texture { continue }
        if let Some(reference) = entry.reference() {
            out.push((Media::Asset(AssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() }), root.join(logical)));
        }
    }
    Ok(out)
}

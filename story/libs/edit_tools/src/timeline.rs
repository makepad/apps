//! `timeline`: a cut as its author sees it on the CUT page, as text or
//! JSON: the output format, scenes (the stretches between span edges, each
//! with what plays in it), the blocks of every lane and track, effects and
//! transitions, the sound (clips, voice lines placed at their times,
//! captions) and the picture's wiring. Pure.

use crate::inspect::num;
use makepad_story_edit_render::audio::{fps_ratio, frame_count};
use makepad_story_cut::*;
use makepad_strict_json::{obj, s, Value};
use makepad_motion::InputValue;

/// One row entry: a node with a span (or always on) in a lane.
#[derive(Clone, Debug)]
pub struct Block {
    pub node: String,
    pub kind: &'static str,
    /// `video`, `audio`, `voice`, `captions` or `effects`.
    pub lane: &'static str,
    pub track: u32,
    pub start: f64,
    pub end: f64,
    /// One line of what it is (`asset ast_… in 2.00 fit fill`).
    pub detail: String,
    /// The same, as JSON fields.
    pub fields: Vec<(&'static str, Value)>,
}

/// A stretch between two span edges and the nodes active in it.
#[derive(Clone, Debug)]
pub struct Scene {
    pub start: f64,
    pub end: f64,
    pub nodes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pub label: String,
    pub size: (u32, u32),
    pub fps: f64,
    pub frames: u64,
    pub duration: f64,
    pub bpm: f64,
    pub scenes: Vec<Scene>,
    pub blocks: Vec<Block>,
    /// `(node, inputs)`: what each Stack, Transition, Fx, Reframe and the
    /// output read.
    pub wiring: Vec<(String, Vec<String>)>,
    /// `(node, run id, marker name, offset)`.
    pub markers: Vec<(String, String, String, f64)>,
    /// Every revision the cut pins, as `kind id rev`.
    pub dependencies: Vec<String>,
    /// A motion document's inputs and their values (`name`, value as
    /// written); empty for a cut.
    pub inputs: Vec<(String, String)>,
}

fn short(id: &str) -> String {
    if id.len() > 14 {
        format!("{}…", &id[..14])
    } else {
        id.to_string()
    }
}

fn media_text(media: &MediaRef, nested: bool) -> String {
    match media {
        MediaRef::MissingOptional(path) => format!("media not present: {path} (plays silent)"),
        MediaRef::Asset(asset) if nested => format!("edit {}@{}", short(&asset.id), short(&asset.rev)),
        MediaRef::Asset(asset) => format!("asset {}@{}", short(&asset.id), short(&asset.rev)),
        MediaRef::Run(run) => format!("run {} render {}@{}", short(&run.id), short(&run.render_id), short(&run.render)),
    }
}

fn media_json(media: &MediaRef, nested: bool) -> Value {
    match media {
        MediaRef::MissingOptional(path) => obj(vec![("kind", s("missing_optional")), ("path", s(path.clone()))]),
        MediaRef::Asset(asset) => obj(vec![("kind", s(if nested { "cut" } else { "asset" })), ("id", s(asset.id.clone())), ("rev", s(asset.rev.clone()))]),
        MediaRef::Run(run) => obj(vec![
            ("kind", s("run")),
            ("id", s(run.id.clone())),
            ("rev", s(run.rev.clone())),
            ("render_id", s(run.render_id.clone())),
            ("render", s(run.render.clone())),
        ]),
    }
}

fn source_time(time: &SourceTime) -> (String, Value) {
    match time {
        SourceTime::Seconds(t) => (format!("{t:.2}"), num(*t)),
        SourceTime::Marker { name, offset } => (format!("marker(@{name}, {offset})"), obj(vec![("marker", s(name.clone())), ("offset", num(*offset))])),
    }
}

fn animated(value: &Animated<1>) -> (String, Value) {
    match value {
        Animated::Const([v]) => (format!("{v}"), num(*v)),
        Animated::Keys(keys) => (
            format!("keys({})", keys.points.len()),
            Value::Arr(keys.points.iter().map(|p| Value::Arr(vec![num(p.t), num(p.v[0]), s(p.ease.id())])).collect()),
        ),
    }
}

fn wire_text(wire: &Option<Wire>) -> String {
    wire.as_ref().map(|w| w.node.clone()).unwrap_or_else(|| "-".into())
}

/// The timeline of `doc`, with voice lines placed by `eval` (pass
/// `CutEval::new(doc)` when nothing more is known).
pub fn timeline(doc: &CutDoc, eval: &CutEval<'_>) -> Timeline {
    let mut blocks = Vec::new();
    let mut wiring = Vec::new();
    for node in &doc.nodes {
        let (start, end) = node.span.map(|span| (span.start, span.end())).unwrap_or((0.0, doc.duration));
        let block = |lane: &'static str, detail: String, fields: Vec<(&'static str, Value)>| Block {
            node: node.id.clone(),
            kind: node.kind.type_name(),
            lane,
            track: node.track,
            start,
            end,
            detail,
            fields,
        };
        match &node.kind {
            CutNodeKind::Clip { src, source_in, speed, fit, opacity, audio, gain, nested, .. } => {
                let (in_text, in_json) = source_time(source_in);
                let (op_text, op_json) = animated(opacity);
                let mut detail = format!("{} in {in_text} fit {}", media_text(src, *nested), fit_id(*fit));
                if *speed != 1.0 {
                    detail.push_str(&format!(" speed {speed}"));
                }
                if opacity.is_keyed() || opacity.at1(0.0) != 1.0 {
                    detail.push_str(&format!(" opacity {op_text}"));
                }
                if *audio {
                    detail.push_str(&format!(" sound gain {}", animated(gain).0));
                }
                blocks.push(block(
                    "video",
                    detail,
                    vec![("src", media_json(src, *nested)), ("in", in_json), ("speed", num(*speed)), ("fit", s(fit_id(*fit))), ("opacity", op_json), ("audio", Value::Bool(*audio)), ("gain", animated(gain).1)],
                ));
            }
            CutNodeKind::Still { src, fit, opacity } => {
                blocks.push(block(
                    "video",
                    format!("{} fit {} opacity {}", media_text(src, false), fit_id(*fit), animated(opacity).0),
                    vec![("src", media_json(src, false)), ("fit", s(fit_id(*fit))), ("opacity", animated(opacity).1)],
                ));
            }
            CutNodeKind::Title { text, anchor, font_size, style } => {
                blocks.push(block(
                    "video",
                    format!("\"{}\" at ({:.2}, {:.2}) {} px{}", text.replace('\n', "\\n"), anchor[0], anchor[1], animated(font_size).0, if style.is_empty() || style == "none" { String::new() } else { format!(" style {style}") }),
                    vec![("text", s(text.clone())), ("anchor", Value::Arr(vec![num(anchor[0]), num(anchor[1])])), ("font_size", animated(font_size).1), ("style", s(style.clone()))],
                ));
            }
            CutNodeKind::Fx { input, effect, params, speed } => {
                let name = match effect {
                    EffectRef::Preset(name) => name.clone(),
                    EffectRef::Asset(asset) => format!("asset {}@{}", short(&asset.id), short(&asset.rev)),
                };
                let keyed: Vec<String> = ["p1", "p2", "p3", "p4"].iter().zip(params).filter(|(_, p)| p.is_keyed() || p.at1(0.0) >= 0.0).map(|(n, p)| format!("{n} {}", animated(p).0)).collect();
                blocks.push(block(
                    "effects",
                    format!("{name} on {}{}{}", wire_text(input), if keyed.is_empty() { String::new() } else { format!(" {}", keyed.join(" ")) }, if *speed != 1.0 { format!(" speed {speed}") } else { String::new() }),
                    vec![("effect", s(name)), ("input", s(wire_text(input))), ("params", Value::Arr(params.iter().map(|p| animated(p).1).collect())), ("speed", num(*speed))],
                ));
                wiring.push((node.id.clone(), input.iter().map(|w| w.node.clone()).collect()));
            }
            CutNodeKind::Transition { a, b, effect, mix, .. } => {
                let name = if effect.is_empty() { "dissolve".to_string() } else { effect.clone() };
                blocks.push(block(
                    "effects",
                    format!("{name} from {} to {}{}", wire_text(a), wire_text(b), mix.as_ref().map(|m| format!(" mix {}", animated(m).0)).unwrap_or_default()),
                    vec![("effect", s(name)), ("a", s(wire_text(a))), ("b", s(wire_text(b)))],
                ));
                wiring.push((node.id.clone(), a.iter().chain(b.iter()).map(|w| w.node.clone()).collect()));
            }
            CutNodeKind::Reframe { input, aspect, mode, safe, crop, .. } => {
                let mode = match mode {
                    ReframeMode::Auto => "auto",
                    ReframeMode::Keys => "keys",
                };
                blocks.push(block(
                    "effects",
                    format!(
                        "reframe {} to {} ({mode}{}) safe @{}",
                        wire_text(input),
                        aspect.map(|[w, h]| format!("{w}:{h}")).unwrap_or_else(|| "the output".into()),
                        crop.as_ref().map(|c| if c.is_keyed() { ", keyed crop" } else { ", fixed crop" }).unwrap_or(""),
                        safe.id()
                    ),
                    vec![("input", s(wire_text(input))), ("mode", s(mode)), ("safe", s(safe.id()))],
                ));
                wiring.push((node.id.clone(), input.iter().map(|w| w.node.clone()).collect()));
            }
            CutNodeKind::Stack { layers } => {
                wiring.push((node.id.clone(), layers.iter().map(|(w, blend)| if *blend == Blend::Normal { w.node.clone() } else { format!("{} ({})", w.node, blend_id(*blend)) }).collect()));
            }
            CutNodeKind::AudioClip { src, source_in, gain, pan, shape } => {
                let (in_text, in_json) = source_time(source_in);
                blocks.push(block(
                    "audio",
                    format!("{} in {in_text} gain {} pan {}{}", media_text(src, false), animated(gain).0, animated(pan).0, shape_text(shape)),
                    vec![("src", media_json(src, false)), ("in", in_json), ("gain", animated(gain).1), ("pan", animated(pan).1), ("fade_in", num(shape.fade_in)), ("fade_out", num(shape.fade_out))],
                ));
            }
            CutNodeKind::VoiceOver { voice, lines, gain, shape, .. } => {
                let placed = eval.voice_lines(&node.id);
                let mut rows = Vec::new();
                let mut json = Vec::new();
                for (index, line) in lines.iter().enumerate() {
                    let at = placed.iter().find(|p| p.index == index);
                    let when = at.map(|p| format!("{:.2}–{:.2}", p.start, p.end)).unwrap_or_else(|| "unplaced".into());
                    rows.push(format!("line {index} {when} \"{}\"{}", line.text, if line.take.is_none() { " (no take)" } else { "" }));
                    json.push(obj(vec![
                        ("text", s(line.text.clone())),
                        ("start", at.map(|p| num(p.start)).unwrap_or(Value::Null)),
                        ("end", at.map(|p| num(p.end)).unwrap_or(Value::Null)),
                        ("take", Value::Bool(line.take.is_some())),
                    ]));
                }
                blocks.push(block(
                    "voice",
                    format!("{} {} gain {}{}; {}", voice.engine.id(), voice.id, animated(gain).0, shape_text(shape), rows.join("; ")),
                    vec![("voice", s(format!("{} {}", voice.engine.id(), voice.id))), ("lines", Value::Arr(json))],
                ));
            }
            CutNodeKind::Subtitles { source, style, safe, max_words, .. } => {
                let count = eval.captions(&node.id).len();
                blocks.push(block(
                    "captions",
                    format!(
                        "of {} style {} safe @{} max_words {max_words}{}",
                        wire_text(source),
                        if style.is_empty() || style == "none" { "@movie" } else { style },
                        safe.id(),
                        if count > 0 { format!(", {count} captions") } else { String::new() }
                    ),
                    vec![("source", s(wire_text(source))), ("style", s(style.clone())), ("safe", s(safe.id())), ("captions", Value::Int(count as i64))],
                ));
            }
            CutNodeKind::AppView { app, fit, .. } => {
                blocks.push(block("video", format!("live app {app} fit {}", fit_id(*fit)), vec![("app", s(app.clone())), ("fit", s(fit_id(*fit)))]));
            }
            CutNodeKind::Code(spec) => {
                let lines = spec.lines.map_or_else(String::new, |(first, last)| format!(" lines {first}-{last}"));
                blocks.push(block(
                    "video",
                    format!("live code {}{lines} fit {}", spec.file, fit_id(spec.fit)),
                    vec![("app", s(spec.app.clone())), ("file", s(spec.file.clone())), ("fit", s(fit_id(spec.fit)))],
                ));
            }
            CutNodeKind::Camera(spec) => {
                let device = spec.device.label();
                blocks.push(block(
                    "video",
                    format!("live camera {device} {}x{}@{} fit {}", spec.width, spec.height, spec.fps, fit_id(spec.fit)),
                    vec![("device", s(device)), ("size", s(format!("{}x{}", spec.width, spec.height))), ("fps", num(spec.fps)), ("fit", s(fit_id(spec.fit)))],
                ));
            }
            CutNodeKind::Output { frame, audio } => {
                let mut inputs: Vec<String> = frame.iter().map(|w| w.node.clone()).collect();
                inputs.extend(audio.iter().map(|w| format!("{} (sound)", w.node)));
                wiring.push((node.id.clone(), inputs));
            }
        }
    }
    for graphic in &doc.graphics {
        let (start, end) = graphic.span.map(|span| (span.start, span.end())).unwrap_or((0.0, doc.duration));
        let src = match &graphic.src {
            GraphicSrc::Asset(asset) => format!("motion {}@{}", short(&asset.id), short(&asset.rev)),
            GraphicSrc::Builtin(name) => format!("built-in \"{name}\""),
            GraphicSrc::Tree(tree) => tree.document.clone(),
        };
        let params: Vec<String> = graphic.params.iter().map(|(name, value)| format!("{name}={}", param_text(value))).collect();
        blocks.push(Block {
            node: graphic.id.clone(),
            kind: "Graphic",
            lane: "video",
            track: graphic.track,
            start,
            end,
            detail: format!("{src} fit {}{}", fit_id(graphic.fit), if params.is_empty() { String::new() } else { format!(" {}", params.join(" ")) }),
            fields: vec![
                ("src", s(src)),
                ("fit", s(fit_id(graphic.fit))),
                ("params", Value::Arr(graphic.params.iter().map(|(n, v)| obj(vec![("name", s(n.clone())), ("value", s(param_text(v)))])).collect())),
            ],
        });
    }
    blocks.sort_by(|a, b| lane_rank(a.lane).cmp(&lane_rank(b.lane)).then(a.track.cmp(&b.track)).then(a.start.total_cmp(&b.start)));

    // Scenes: cut at every span edge.
    let mut cuts = vec![0.0, doc.duration];
    for node in &doc.nodes {
        if let Some(span) = node.span {
            cuts.push(span.start.clamp(0.0, doc.duration));
            cuts.push(span.end().clamp(0.0, doc.duration));
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let scenes = cuts
        .windows(2)
        .map(|pair| {
            let mid = (pair[0] + pair[1]) / 2.0;
            let nodes = blocks.iter().filter(|b| b.start <= mid && mid < b.end && doc.node(&b.node).is_some_and(|n| n.span.is_some())).map(|b| b.node.clone()).collect();
            Scene { start: pair[0], end: pair[1], nodes }
        })
        .collect();
    let markers = doc.markers().into_iter().map(|m| (m.node, m.run.id, m.name, m.offset)).collect();
    let dependencies = dependencies(doc)
        .into_iter()
        .map(|dep| match dep {
            Dep::Asset(a) => format!("asset {} {}", a.id, a.rev),
            Dep::AppRun(a) => format!("app_run {} {}", a.id, a.rev),
            Dep::Render { run, render } => format!("render {} {} of run {}", render.id, render.rev, run.id),
            Dep::Cut(a) => format!("edit {} {}", a.id, a.rev),
        })
        .collect();
    Timeline {
        label: doc.label.clone(),
        size: (doc.output.width, doc.output.height),
        fps: doc.output.fps,
        frames: frame_count(doc.duration, fps_ratio(doc.output.fps)),
        duration: doc.duration,
        bpm: doc.bpm,
        scenes,
        blocks,
        wiring,
        markers,
        dependencies,
        inputs: Vec::new(),
    }
}

fn param_text(value: &InputValue) -> String {
    match value {
        InputValue::Text(text) => format!("\"{text}\""),
        InputValue::Number(n) => format!("{n}"),
        InputValue::Bool(b) => format!("{b}"),
        InputValue::Vec2([x, y]) => format!("[{x}, {y}]"),
        InputValue::Asset(asset) => format!("asset({})", short(&asset.id)),
        other => format!("{other:?}"),
    }
}

fn lane_rank(lane: &str) -> u8 {
    match lane {
        "video" => 0,
        "effects" => 1,
        "captions" => 2,
        "audio" => 3,
        "layers" => 5,
        _ => 4,
    }
}

fn fit_id(fit: Fit) -> &'static str {
    match fit {
        Fit::Fill => "fill",
        Fit::Fit => "fit",
        Fit::None => "none",
    }
}

fn blend_id(blend: Blend) -> &'static str {
    match blend {
        Blend::Normal => "normal",
        Blend::Add => "add",
        Blend::Screen => "screen",
        Blend::Multiply => "multiply",
    }
}

fn shape_text(shape: &AudioShape) -> String {
    let mut out = String::new();
    if shape.fade_in > 0.0 {
        out.push_str(&format!(" fade_in {}", shape.fade_in));
    }
    if shape.fade_out > 0.0 {
        out.push_str(&format!(" fade_out {}", shape.fade_out));
    }
    if let Some(duck) = &shape.duck {
        out.push_str(&format!(" ducks {} dB under {}", duck.depth_db, duck.under.join(",")));
    }
    out
}

impl Timeline {
    pub fn to_json(&self) -> Value {
        let block = |b: &Block| {
            let mut pairs = vec![("node", s(b.node.clone())), ("type", s(b.kind)), ("lane", s(b.lane)), ("track", Value::Int(b.track as i64)), ("start", num(b.start)), ("end", num(b.end))];
            pairs.extend(b.fields.iter().cloned());
            obj(pairs)
        };
        obj(vec![
            ("label", s(self.label.clone())),
            ("width", Value::Int(self.size.0 as i64)),
            ("height", Value::Int(self.size.1 as i64)),
            ("fps", num(self.fps)),
            ("frames", Value::Int(self.frames as i64)),
            ("duration", num(self.duration)),
            ("bpm", num(self.bpm)),
            (
                "scenes",
                Value::Arr(self.scenes.iter().map(|sc| obj(vec![("start", num(sc.start)), ("end", num(sc.end)), ("nodes", Value::Arr(sc.nodes.iter().cloned().map(Value::Str).collect()))])).collect()),
            ),
            ("blocks", Value::Arr(self.blocks.iter().map(block).collect())),
            (
                "wiring",
                Value::Arr(self.wiring.iter().map(|(node, inputs)| obj(vec![("node", s(node.clone())), ("inputs", Value::Arr(inputs.iter().cloned().map(Value::Str).collect()))])).collect()),
            ),
            (
                "markers",
                Value::Arr(self.markers.iter().map(|(node, run, name, offset)| obj(vec![("node", s(node.clone())), ("run", s(run.clone())), ("marker", s(name.clone())), ("offset", num(*offset))])).collect()),
            ),
            ("dependencies", Value::Arr(self.dependencies.iter().cloned().map(Value::Str).collect())),
            ("inputs", Value::Arr(self.inputs.iter().map(|(name, value)| obj(vec![("name", s(name.clone())), ("value", s(value.clone()))])).collect())),
        ])
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "`{}`: {}x{} at {} fps, {:.2} s = {} frames, bpm {}\n",
            self.label, self.size.0, self.size.1, self.fps, self.duration, self.frames, self.bpm
        );
        if !self.inputs.is_empty() {
            out.push_str(&format!("inputs: {}\n", self.inputs.iter().map(|(n, v)| format!("{n} = {v}")).collect::<Vec<_>>().join(", ")));
        }
        out.push_str("scenes:\n");
        for scene in &self.scenes {
            out.push_str(&format!("  {:6.2}–{:6.2}  {}\n", scene.start, scene.end, if scene.nodes.is_empty() { "(nothing)".into() } else { scene.nodes.join(" ") }));
        }
        let width = self.blocks.iter().map(|b| b.node.chars().count()).max().unwrap_or(4);
        let mut lane = "";
        for b in &self.blocks {
            if b.lane != lane {
                lane = b.lane;
                out.push_str(&format!("{lane}:\n"));
            }
            let row = match b.lane {
                "video" => format!("V{}", b.track + 1),
                "audio" => format!("A{}", b.track + 1),
                "voice" => "VO".into(),
                "captions" => "CC".into(),
                "layers" => format!("L{}", b.track),
                _ => "FX".into(),
            };
            out.push_str(&format!("  {row:<3} {:<width$}  {:<10} {:6.2}–{:6.2}  {}\n", b.node, b.kind, b.start, b.end, b.detail));
        }
        if !self.wiring.is_empty() {
            out.push_str("wiring:\n");
            for (node, inputs) in &self.wiring {
                out.push_str(&format!("  {node} <- {}\n", inputs.join(", ")));
            }
        }
        if !self.markers.is_empty() {
            out.push_str("markers:\n");
            for (node, run, name, offset) in &self.markers {
                out.push_str(&format!("  {node}: marker(@{name}, {offset}) of run {}\n", short(run)));
            }
        }
        out.push_str(&format!("pins {} revisions\n", self.dependencies.len()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::tests::{asset_ref, edit};

    #[test]
    fn the_timeline_lists_scenes_lanes_and_wiring() {
        let nodes = format!(
            "let a = Clip{{src: {a} span: [0.0, 2.5] fit: @fill}}\n\
             let b = Clip{{src: {b} in: 0.5 span: [1.5, 2.5] fit: @fit track: 1}}\n\
             let w = Transition{{a: a.out(@frame) b: b.out(@frame) effect: \"\" span: [1.5, 1.0]}}\n\
             let title = Title{{text: \"Hello\" span: [0.5, 3.0] anchor: vec2(0.5, 0.85) font_size: 28 track: 2}}\n\
             let top = Stack{{layers: [w.out(@frame), title.out(@frame)]}}\n\
             let music = AudioClip{{src: {a} span: [0.0, 4.0] gain: keys([0.0, 0.0], [0.5, 1.0])}}\n\
             let out = CutOutput{{frame: top.out(@frame) audio: [conn(1, music.out(@audio))]}}",
            a = asset_ref(0xa1),
            b = asset_ref(0xb2)
        );
        let doc = edit(&nodes, "a, b, w, title, top, music, out", "{width: 640 height: 360 fps: 30}", 4.0).load().unwrap();
        let eval = CutEval::new(&doc);
        let timeline = timeline(&doc, &eval);
        assert_eq!(timeline.frames, 120);
        let cuts: Vec<(f64, f64)> = timeline.scenes.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(cuts, vec![(0.0, 0.5), (0.5, 1.5), (1.5, 2.5), (2.5, 3.5), (3.5, 4.0)]);
        assert_eq!(timeline.scenes[2].nodes, vec!["a", "b", "title", "w", "music"]);
        let text = timeline.to_text();
        assert!(text.contains("V2  b"), "{text}");
        assert!(text.contains("dissolve from a to b"), "{text}");
        assert!(text.contains("top <- w, title"), "{text}");
        assert!(text.contains("gain keys(2)"), "{text}");
        let json = timeline.to_json().to_json();
        assert!(json.contains("\"lane\":\"audio\""), "{json}");
        assert!(json.contains("\"frames\":120"), "{json}");
    }
}

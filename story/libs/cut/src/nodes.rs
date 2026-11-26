//! The cut node types. Every parameter is authored: an
//! edit changes only by source edits, never by live endpoints, so a render
//! is a pure function of the source. Frame and audio ports carry the
//! shared `stage.frame` and `stage.audio` types.
//!
//! `ParamDescriptor` has no `animatable` flag yet; [`animatable`] is the
//! list until it does (see the lane report).

use makepad_flow::document::types::{Carrier, Rate, TypeKey, TypeSchema};
use makepad_flow::document::{InputDescriptor, ParamDescriptor, ParamType, Registry, TypeDescriptor};
use makepad_flow::engine::providers::ExecutorRegistry;
use makepad_flow::{Literal, Port, PortType, MAX_SEQUENCE};

pub const OWNER: &str = "stage.edit";
pub const FRAME: &str = "stage.frame";
pub const AUDIO: &str = "stage.audio";
/// The loader's carrier for the `Flow{}` header (`output`, `duration`,
/// `bpm`), which the flow grammar has no field for. `CutDoc::load` removes
/// it from the graph it returns. Only the loader's private registry has it.
pub const METADATA: &str = "CutMetadata";
/// Layers one `Stack` composes and audio entries one `CutOutput` mixes:
/// the flow grammar's own bound on an ordered input.
pub const MAX_LAYERS: u32 = MAX_SEQUENCE;
/// The `mod.cut` prelude: node names, `asset()`, `run()`, `keys()`,
/// `marker()`, the eases and the loader's `finish`.
pub const PRELUDE: &str = include_str!("prelude.splash");

/// Whether `param` of cut node `node` (its registered type name) may be
/// written as `keys(...)`: its animatable parameters.
pub fn animatable(node: &str, param: &str) -> bool {
    matches!((node, param),
        ("Clip" | "Still", "opacity")
        | ("Title", "font_size")
        | ("Fx", "p1" | "p2" | "p3" | "p4")
        | ("Transition", "p1" | "p2" | "p3" | "mix")
        | ("Reframe", "crop")
        | ("Clip" | "AudioClip" | "VoiceOver", "gain" | "pan")
        | ("Subtitles", "font_size")
        | ("Graphic", "opacity"))
}

fn ty(key: &str) -> PortType {
    PortType::Registered(TypeKey::new(key))
}

fn input(name: &str, key: &str, many: bool) -> InputDescriptor {
    InputDescriptor {
        name: name.into(), ty: ty(key), many: many.then_some(MAX_LAYERS),
        default: if many { Literal::Arr(Vec::new()) } else { Literal::Null }, flexible: false,
    }
}

fn output(name: &str, key: &str) -> Port {
    Port { name: name.into(), ty: ty(key), many: None }
}

fn param(name: &str, default: Literal, doc: &str) -> ParamDescriptor {
    ParamDescriptor::authored(name, ParamType::Literal, default, doc)
}

fn num(name: &str, value: f64, doc: &str) -> ParamDescriptor {
    param(name, Literal::Num(value), doc)
}

fn ident(name: &str, value: &str, doc: &str) -> ParamDescriptor {
    param(name, Literal::Id(value.into()), doc)
}

fn span() -> ParamDescriptor {
    param("span", Literal::Null, "`[start, length]` in seconds of the cut; active on [start, start + length). Nil: always active.")
}

fn node(name: &str, doc: &str, mut params: Vec<ParamDescriptor>, inputs: Vec<InputDescriptor>, outputs: Vec<Port>) -> TypeDescriptor {
    let mut descriptor = TypeDescriptor::provider(name, OWNER, inputs, outputs);
    if name != "CutOutput" && name != METADATA {
        params.push(num("track", 0.0, "The timeline track the block sits on."));
    }
    descriptor.params = params;
    descriptor.doc = doc.into();
    descriptor
}

/// Register the cut types and nodes, and name `stage.edit` a live owner so
/// a run knows its nodes are not jobs (a cut is evaluated by `CutEval`).
pub fn register(registry: &mut Registry, executors: &mut ExecutorRegistry) -> Result<(), String> {
    register_document(registry)?;
    executors.register_live(OWNER)
}

/// Register the types and node descriptors only: what loading a cut
/// needs. The type schemas equal the owning providers' (`live_frame`,
/// `live_audio`), so registration can happen in either order.
pub fn register_document(registry: &mut Registry) -> Result<(), String> {
    for (key, carrier, rate, doc) in [
        (FRAME, Carrier::Frame, Rate::Frame, "A main-thread rendered texture, accessed through its owning frame provider."),
        (AUDIO, Carrier::AudioPort, Rate::Block, "A stereo audio stream inside the audio owner. It never leaves the device callback's schedule."),
    ] {
        registry.register_type(TypeSchema {
            key: TypeKey::new(key), schema_rev: 1, carrier, rate, local_only: true,
            serial: None, audio_lowering: None, doc: doc.into(),
        }).map_err(|error| error.to_string())?;
    }
    let src = |doc: &str| param("src", Literal::Null, doc);
    let source_in = || num("in", 0.0, "Source time at the span's start, in seconds, or `marker(@name, offset)` of the app run it plays.");
    let fit = || ident("fit", "fill", "@fill crops to cover, @fit letterboxes, @none places 1:1 centred. A source of the output's size is always 1:1.");
    let opacity = || num("opacity", 1.0, "How much of the picture shows, 0..1. Animatable.");
    // Every audio source's gain, pan, fades and ducking.
    let audio_params = || vec![
        num("gain", 1.0, "Linear gain. Animatable."), num("pan", 0.0, "-1 left .. 1 right. Animatable."),
        num("fade_in", 0.0, "Seconds of fade from silence at the start."), num("fade_out", 0.0, "Seconds of fade to silence at the end."),
        param("duck", Literal::Null, "`{under: [@voice…], depth_db: -12, attack: 0.08, release: 0.4}`: drop under the named sources while they speak."),
    ];
    let with = |mut params: Vec<ParamDescriptor>, more: Vec<ParamDescriptor>| {
        params.extend(more);
        params
    };
    let effect_param = |index: u32| num(&format!("p{index}"), -1.0, "An effect dial, 0..1; below 0 leaves the effect document's value. Animatable.");
    let descriptors = vec![
        node("Clip", "A video or app-run render on the timeline. Source time is `in + (t - start) * speed`.",
            with(vec![src("The exact source: `asset(id, rev)` of a Video, `run(id, rev, render_id, render_rev)` of an app run, or `cut(id, rev)` of another cut (a compound clip)."), source_in(), span(),
                num("speed", 1.0, "Source seconds per edit second."), fit(), opacity(),
                param("audio", Literal::Null, "Mix the video's own sound: true or false. Nil: on for library videos, off for app runs."),
                param("subtitles", Literal::Bool(false), "A nested cut's own captions: 1 shows them, placed in this cut's safe zone; off by default.")], audio_params()),
            vec![], vec![output("frame", FRAME), output("audio", AUDIO)]),
        node("Still", "An image held for its span.",
            vec![src("The exact image: `asset(id, rev)`."), span(), fit(), opacity()],
            vec![], vec![output("frame", FRAME)]),
        node("Title", "Kinetic text over the cut.",
            vec![param("text", Literal::Str(String::new()), "The text."), span(),
                param("anchor", Literal::Null, "`vec2(x, y)` in output fractions; nil centres it. (`at` is the node's canvas position.)"),
                num("font_size", 48.0, "Size in output pixels. Animatable."),
                ident("style", "none", "The kinetic-text preset.")],
            vec![], vec![output("frame", FRAME)]),
        node("Fx", "An effect over a picture, rendered with its time set: local_time = (t - start) * speed, beat = t * bpm / 60.",
            vec![param("effect", Literal::Str(String::new()), "A render-kit effect (`kit/<name>`), an installed effect pack's preset by name, or `asset(id, rev)` of a library effect."),
                effect_param(1), effect_param(2), effect_param(3), effect_param(4),
                num("speed", 1.0, "How fast the effect's own clock runs."), span()],
            vec![input("input", FRAME, false)], vec![output("frame", FRAME)]),
        node("Transition", "A transition from `a` to `b`; `mix` ramps 0→1 over the span unless keyed, and drives the preset's fourth dial.",
            vec![param("effect", Literal::Str(String::new()), "The transition preset by name (empty is a dissolve), or `asset(id, rev)` of a library VjEffect."), span(),
                effect_param(1), effect_param(2), effect_param(3),
                num("speed", 1.0, "How fast the transition's own clock runs."),
                param("mix", Literal::Null, "Nil ramps 0→1 over the span; `keys(...)` shapes it. Animatable.")],
            vec![input("a", FRAME, false), input("b", FRAME, false)], vec![output("frame", FRAME)]),
        node("Stack", "Composes ordered layers over transparent, the first at the bottom.",
            vec![param("blend", Literal::Arr(Vec::new()), "Per-layer blend, in layer order: @normal, @add, @screen or @multiply. Missing entries are @normal."), span()],
            vec![input("layers", FRAME, true)], vec![output("frame", FRAME)]),
        node("Reframe", "Crops a moving window out of a large picture for another aspect ratio.",
            vec![param("aspect", Literal::Null, "`[w, h]`; nil is the output's aspect."),
                param("crop", Literal::Null, "`[x, y, w, h]` in source pixels, usually `keys(...)` the solver wrote. Animatable."),
                ident("mode", "auto", "@auto re-solves `crop` from `focus`; @keys keeps the authored crop."),
                param("focus", Literal::Null, "The focus track: `run(...)` or `asset(...)`."),
                ident("safe", "none", "Safe-zone preset: @reels, @tiktok, @shorts or @none."), span()],
            vec![input("input", FRAME, false)], vec![output("frame", FRAME)]),
        node("AudioClip", "Audio on the timeline, mixed offline for renders.",
            with(vec![src("The exact source: `asset(id, rev)` of Audio or Video, or `run(id, rev, render_id, render_rev)`."), source_in(), span()], audio_params()),
            vec![], vec![output("audio", AUDIO)]),
        node("VoiceOver", "Spoken lines: the text is the truth, each line's take is a pinned Audio asset placed at the line's start.",
            with(vec![param("voice", Literal::Null, "`{engine: @kokoro id: \"bm_daniel\" speed: 1.0}`; @indextts takes `reference: asset(id, rev)`."),
                param("lines", Literal::Arr(Vec::new()), "`[line(text, at, take, key), …]`: `at` is edit seconds or `marker(@name, offset)` of `run`."),
                param("run", Literal::Null, "The app run whose markers the lines use; nil is the one run the cut's clips play.")], audio_params()),
            vec![], vec![output("audio", AUDIO)]),
        node("Subtitles", "Captions of an audio source, burned in word by word: a VoiceOver's script, else its transcript.",
            vec![ident("style", "none", "The kinetic-text preset (e.g. \"karaoke/pop\")."),
                param("anchor", Literal::Null, "`vec2(x, y)` in output fractions, kept inside `safe`; nil is vec2(0.5, 0.8)."),
                ident("safe", "none", "Safe-zone preset: @reels, @tiktok, @shorts or @none."),
                num("max_words", 6.0, "Words one caption shows at most."),
                param("transcript", Literal::Null, "`asset(id, rev)` of the source's transcript, for sources without a script."),
                param("language", Literal::Str("en".into()), "The spoken language, for transcribing."),
                num("font_size", 64.0, "Size in output pixels. Animatable."), span()],
            vec![input("source", AUDIO, false)], vec![output("frame", FRAME)]),
        node("Generate", "A part of the timeline piped through an AI Hub generation flow and played as a clip: its chosen take stretched over its span, its input while it has none, and its input outside its span.",
            vec![span(),
                param("flow", Literal::Null, "The FLOW preset that generates: `preset(\"bundled:image-to-video\")`."),
                param("frame_at", Literal::Null, "Image-to-video: the cut second whose input frame starts the clip. Nil: video-to-video over the span."),
                param("prompt", Literal::Str(String::new()), "What to make of the input."),
                param("negative", Literal::Str(String::new()), "What to keep out."),
                num("seed", 0.0, "The seed of the next take; further takes count up from it."),
                param("params", Literal::Null, "Generator parameters, e.g. `{frames: 124 steps: 30 width: 640 height: 352}`."),
                fit(),
                param("takes", Literal::Arr(Vec::new()), "`[take(asset(id, rev), seed, key, seconds, model), …]`: every generated result, pinned."),
                num("take", 0.0, "The index of the take the clip plays.")],
            vec![input("input", FRAME, false)], vec![output("frame", FRAME)]),
        node("Graphic", "A Motion document (a `Motion` asset: text, shapes, SVG, images, shaders, 3D, UI) drawn over its span, its clock starting at the span's start.",
            vec![src("The exact document: `asset(id, rev)` of a Motion asset."), span(),
                ident("fit", "fit", "How the document's own size sits in the output: @fit letterboxes, @fill crops to cover, @none places 1:1 centred. Equal sizes are 1:1."),
                param("params", Literal::Null, "`{name: value}`: the document's inputs (text, numbers, `\"#rrggbb\"` colours, `[x, y]`, `asset(id, rev)`)."),
                param("bind", Literal::Null, "The app run the document's `run()` follows: `@clip` (the Clip that plays it; follows that clip's updates) or `run(id, rev, render_id, render_rev)`."),
                opacity()],
            vec![], vec![output("frame", FRAME)]),
        node("AppView", "A live app a deck hosts, as a layer: the picture of the entry of the slide's `apps` that `app` names, placed by `fit`. Outside DECK it is a placeholder frame naming the app; a cut never launches a process.",
            vec![param("app", Literal::Null, "The name of an entry in the slide's `apps` (`@counter`)."), span(),
                param("rect", Literal::Null, "`[x, y, w, h]` in fractions of the output: where the app goes (`fit` places it in that rectangle). Nil: the whole output."),
                ident("fit", "fit", "How the app's window sits in the output: @fit letterboxes, @fill crops to cover, @none places 1:1 centred.")],
            vec![], vec![output("frame", FRAME)]),
        node("Code", "A source file of a live app the slide hosts, as a layer: drawn with the code editor (syntax colours), following the file on disk, with a caret and a soft highlight on the range being edited while a demo `edit` types. Outside DECK it is a placeholder frame naming the file.",
            vec![param("app", Literal::Null, "The name of an entry in the slide's `apps` (`@counter`) the file belongs to."),
                param("file", Literal::Null, "The file, a path relative to the workspace root (\"examples/counter/src/main.rs\")."),
                param("lines", Literal::Null, "`[first, last]`: the lines shown (1-based, inclusive). Nil: the whole file."),
                num("width", 900.0, "The code picture's width in pixels, which `fit` places in the output."),
                num("height", 700.0, "The code picture's height in pixels."),
                num("font_size", 22.0, "The text size in pixels of the code picture."),
                param("rect", Literal::Null, "`[x, y, w, h]` in fractions of the output: where the code goes (`fit` places it in that rectangle). Nil: the whole output."),
                span(),
                ident("fit", "fit", "How the code picture sits in the output: @fit letterboxes, @fill crops to cover, @none places 1:1 centred.")],
            vec![], vec![output("frame", FRAME)]),
        node("Camera", "A live camera a deck hosts, as a layer: a document camera, a phone under it, a webcam. DECK opens the device when the slide is next and streams it only while the slide shows. Outside DECK it is a placeholder frame naming the device; a cut never opens a camera. Crop, turn, mirror and adjust run on the GPU as the picture is placed.",
            vec![param("device", Literal::Null, "Part of the camera's name (\"OBSBOT\"), its number (0, 1, ...), or nil for the first camera."),
                num("width", 1280.0, "The capture width asked for, in pixels (the nearest the camera offers is used)."),
                num("height", 720.0, "The capture height asked for, in pixels."),
                num("fps", 30.0, "The capture rate asked for."),
                param("crop", Literal::Null, "`[x, y, w, h]` as fractions of the picture: the part shown. Nil: all of it."),
                num("rotate", 0.0, "Clockwise turn of the picture: 0, 90, 180 or 270 degrees."),
                param("mirror", Literal::Bool(false), "Flip left to right (after the turn)."),
                param("adjust", Literal::Null, "`{brightness: 0 contrast: 1 saturation: 1}`: brightness -1..1 is added, contrast 0..4 scales around grey, saturation 0..4 scales the colour."),
                span(),
                ident("fit", "fit", "How the picture sits in the output: @fit letterboxes, @fill crops to cover, @none places 1:1 centred."),
                param("rect", Literal::Null, "`[x, y, w, h]` as fractions of the output: where the picture goes, `fit` placing it inside. Nil: the whole output.")],
            vec![], vec![output("frame", FRAME)]),
        node("CutOutput", "The edit's picture and its ordered audio mix.",
            vec![], vec![input("frame", FRAME, false), input("audio", AUDIO, true)], vec![]),
    ];
    for descriptor in descriptors {
        registry.register(descriptor).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The document registry plus the loader's header carrier, for
/// `CutDoc::load`'s private registry only: it never reaches a catalog.
pub(crate) fn register_loader(registry: &mut Registry) -> Result<(), String> {
    register_document(registry)?;
    let carrier = node(METADATA, "Internal: carries a cut's Flow header while it loads. Never authored.",
        vec![param("cut_kind", Literal::Null, "The Flow's kind."), param("output", Literal::Null, "The output format."),
            param("duration", Literal::Null, "Seconds."), param("bpm", Literal::Null, "Tempo."),
            param("synthetic", Literal::Null, "`[stack id, entry id]` of the layer entries the loader numbered.")],
        vec![], vec![]);
    registry.register(carrier).map_err(|error| error.to_string())
}

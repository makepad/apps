//! `CutDoc::load`: evaluate a cut in a flow VM and extract the typed
//! model, collecting every problem with its source location.
//!
//! The flow grammar has no `Flow{}` fields for `output`, `duration` and
//! `bpm`, so the evaluated Flow goes through `mod.cut.finish` inside the
//! VM, which moves them into a carrier node the loader removes again. The
//! graph is extracted from the finished value, and every location is the
//! source's own.

use crate::model::*;
use makepad_motion::tree::{TreeContext, TreeLookup};
use crate::nodes::{self, METADATA};
use crate::reframe::SafeZone;
use makepad_asset_data::{AssetId, AssetRevisionId};
use makepad_flow::document::Registry;
use makepad_flow::{Graph, Literal, Loc, Node, NodeInputValue};
use makepad_script::*;
use std::collections::{HashMap, HashSet};

/// Decoders a cut may hold open at one time (8 sources, 2 stay for
/// the VJ decks).
pub const MAX_ACTIVE_CLIPS: usize = 6;
/// Span arithmetic tolerance, in seconds.
const EPSILON: f64 = 1e-9;

fn eval_diagnostic(error: makepad_flow::EvalError) -> Vec<Diagnostic> {
    vec![Diagnostic::new(DiagnosticKind::Eval, Loc { line: error.line, col: error.col }, None, error.message)]
}

/// A flow VM holding the edit vocabulary (and `extra_prelude`'s
/// declarations after it), under the flow evaluator's budgets. `f` gets it
/// and the registry its nodes were installed from.
pub fn with_cut_vm<R>(extra_prelude: &str, f: impl FnOnce(&mut ScriptVm<'_>, &Registry) -> R) -> Result<R, makepad_flow::EvalError> {
    let file = "<cut-vocabulary>";
    let mut registry = Registry::with_builtins();
    nodes::register_loader(&mut registry).map_err(|message| makepad_flow::EvalError { file: file.into(), line: 1, col: 1, message })?;
    let mut host = ScriptVmHost::new(0i32, ());
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    vm.set_script_domain("cut");
    vm.capture_errors();
    makepad_flow::graph::within_limits(&mut vm, file, |vm| {
        makepad_flow::graph::install_prelude(vm, &registry)?;
        // A module's last statement is its result, so a prelude ends in `nil`.
        makepad_flow::graph::evaluate_in(vm, &format!("{}\nnil\n", nodes::PRELUDE), "<cut-prelude>")?;
        if !extra_prelude.is_empty() {
            makepad_flow::graph::evaluate_in(vm, extra_prelude, "<extra-prelude>")?;
        }
        Ok(f(vm, &registry))
    })?
}

impl CutDoc {
    pub fn load(source: &str) -> Result<CutDoc, Vec<Diagnostic>> {
        Self::load_named(source, "cut.splash")
    }

    /// Load with the file name the VM reports errors under.
    pub fn load_named(source: &str, file_name: &str) -> Result<CutDoc, Vec<Diagnostic>> {
        Self::load_context(source, file_name, None)
    }

    pub fn load_tree(source: &str, tree: TreeContext) -> Result<CutDoc, Vec<Diagnostic>> {
        Self::load_context(source, &tree.document.clone(), Some(tree))
    }

    fn load_context(source: &str, file_name: &str, tree: Option<TreeContext>) -> Result<CutDoc, Vec<Diagnostic>> {
        if let Some(tree) = &tree { check_tree(source, tree)?; }
        with_cut_vm("", |vm, registry| {
            let value = makepad_flow::graph::evaluate_in(vm, source, file_name).map_err(eval_diagnostic)?;
            Self::from_value_with_tree(vm, registry, value, source, file_name, tree)
        })
        .map_err(eval_diagnostic)?
    }

    /// The cut that is `value`, an evaluated `Flow{...}` of `source` (named
    /// `file_name`) in a VM from [`with_cut_vm`]: finished by
    /// `mod.cut.finish` there, extracted, checked. A deck calls this
    /// for each slide's Flow.
    pub fn from_value(vm: &mut ScriptVm<'_>, registry: &Registry, value: ScriptValue, source: &str, file_name: &str) -> Result<CutDoc, Vec<Diagnostic>> {
        Self::from_value_with_tree(vm, registry, value, source, file_name, None)
    }

    /// Extract an already evaluated value; the owning document loader validates
    /// its source once before evaluating it (including all inline slides).
    pub fn from_value_with_tree(vm: &mut ScriptVm<'_>, registry: &Registry, value: ScriptValue, source: &str, file_name: &str, tree: Option<TreeContext>) -> Result<CutDoc, Vec<Diagnostic>> {
        let Some(object) = value.as_object() else {
            return Err(vec![Diagnostic::new(DiagnosticKind::Header, Loc { line: 1, col: 1 }, None, "a cut ends with one `Flow{...}`")]);
        };
        // 1-based, where the Flow was written.
        let flow_loc = vm.bx.code_ref().ip_to_loc(vm.bx.heap_ref().made_at(object)).map_or(Loc { line: 1, col: 1 }, |loc| Loc { line: loc.line() + 1, col: loc.col() + 1 });
        let module = vm.module(id!(cut));
        let finish = vm.bx.heap_ref().value(module, id!(finish).into(), NoTrap);
        vm.capture_errors();
        let finished = vm.call(finish, &[value]);
        if let Some(error) = vm.take_errors().into_iter().next() {
            vm.capture_errors();
            return Err(vec![Diagnostic::new(DiagnosticKind::Eval, flow_loc, None, error)]);
        }
        vm.capture_errors();
        let graph = makepad_flow::graph::graph_of(vm, finished, source, file_name, registry).map_err(eval_diagnostic)?;
        Self::from_graph_with_tree(source, graph, flow_loc, tree)
    }

    /// The edit of an extracted `graph` (its node locations the script's
    /// 0-based row and column in `source`) whose `Flow{` is at `flow_loc`
    /// (1-based).
    pub fn from_graph(source: &str, graph: Graph, flow_loc: Loc) -> Result<CutDoc, Vec<Diagnostic>> {
        Self::from_graph_with_tree(source, graph, flow_loc, None)
    }

    fn from_graph_with_tree(source: &str, mut graph: Graph, flow_loc: Loc, tree: Option<TreeContext>) -> Result<CutDoc, Vec<Diagnostic>> {
        let metadata = graph.nodes.iter().position(|node| node.type_name == METADATA).map(|index| graph.nodes.remove(index));
        let mut diags = Vec::new();
        let header = Header::read(source, flow_loc.clone(), metadata.as_ref(), &mut diags);
        let missing_media = tree.as_ref().map_or_else(Vec::new, |tree| tree.resolver.missing_optional());
        let mut loader = Loader { tree, source, graph: &graph, diags: &mut diags, duration: header.duration, generates: Vec::new(), graphics: Vec::new(), bpm: header.bpm, bindings: Vec::new() };
        let mut nodes = graph.nodes.iter().filter_map(|node| loader.node(node)).collect::<Vec<_>>();
        loader.check_audio(&mut nodes);
        let output_node = loader.check_graph(&nodes, flow_loc);
        let generates = std::mem::take(&mut loader.generates);
        let time_bindings = std::mem::take(&mut loader.bindings);
        let graphics = std::mem::take(&mut loader.graphics);
        let synthetic_layers = synthetic_layers(metadata.as_ref());
        if !diags.is_empty() {
            diags.sort_by_key(|diag| (diag.line, diag.col));
            return Err(diags);
        }
        Ok(CutDoc {
            missing_media,
            source: source.to_string(),
            label: graph.label.clone(),
            output: header.output,
            duration: header.duration,
            bpm: header.bpm,
            nodes,
            output_node: output_node.unwrap_or_default(),
            graph,
            synthetic_layers,
            generates,
            graphics,
            time_bindings,
        })
    }
}

struct Header {
    output: OutputFormat,
    duration: f64,
    bpm: f64,
}

impl Header {
    fn read(source: &str, flow_loc: Loc, metadata: Option<&Node>, diags: &mut Vec<Diagnostic>) -> Header {
        let get = |name: &str| metadata.and_then(|node| param(node, name)).unwrap_or(&Literal::Null);
        let at = |name: &str| field_loc(source, flow_loc.clone(), name).unwrap_or_else(|| flow_loc.clone());
        let mut err = |name: &str, message: &str| diags.push(Diagnostic::new(DiagnosticKind::Header, at(name), None, message));
        if !matches!(get("cut_kind"), Literal::Id(kind) if kind == "cut") {
            err("kind", "a cut's Flow has `kind: @cut`");
        }
        let mut output = OutputFormat::default();
        match get("output") {
            Literal::Obj(fields) => {
                let value = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
                let field = |name: &str| value(name).and_then(num);
                match (field("width"), field("height"), field("fps")) {
                    (Some(width), Some(height), Some(fps))
                        if width >= 1.0 && height >= 1.0 && width.fract() == 0.0 && height.fract() == 0.0 && width <= 16384.0 && height <= 16384.0 && fps > 0.0 && fps <= 1000.0 =>
                    {
                        output = OutputFormat { width: width as u32, height: height as u32, fps, ..OutputFormat::default() };
                    }
                    _ => err("output", "`output` is {width height fps}: whole pixel sizes and a positive frame rate"),
                }
                match value("matrix") {
                    None | Some(Literal::Null) => {}
                    Some(Literal::Id(id)) if id == "bt709" => output.matrix = OutputMatrix::Bt709,
                    Some(Literal::Id(id)) if id == "bt601" => output.matrix = OutputMatrix::Bt601,
                    Some(Literal::Id(id)) if id == "bt2020" => output.matrix = OutputMatrix::Bt2020,
                    Some(_) => err("output", "`output.matrix` is @bt709 (the default), @bt601 or @bt2020"),
                }
                match value("range") {
                    None | Some(Literal::Null) => {}
                    Some(Literal::Id(id)) if id == "limited" => output.range = OutputRange::Limited,
                    Some(Literal::Id(id)) if id == "full" => output.range = OutputRange::Full,
                    Some(_) => err("output", "`output.range` is @limited (the default) or @full"),
                }
                match value("alpha") {
                    None | Some(Literal::Null) => {}
                    Some(Literal::Bool(alpha)) => output.alpha = *alpha,
                    Some(_) => err("output", "`output.alpha` is true (the picture keeps its transparency) or false"),
                }
                match value("loudness") {
                    None | Some(Literal::Null) => {}
                    Some(loudness) => match num(loudness) {
                        Some(lufs) if (-70.0..=0.0).contains(&lufs) => output.loudness = Some(lufs),
                        _ => err("output", "`output.loudness` is the target LUFS, from -70 to 0 (e.g. -14)"),
                    },
                }
            }
            _ => err("output", "a cut declares `output: {width height fps}`"),
        }
        let duration = match num(get("duration")) {
            Some(duration) if duration > 0.0 && duration.is_finite() => duration,
            _ => {
                err("duration", "a cut declares a positive `duration` in seconds");
                0.0
            }
        };
        let bpm = match get("bpm") {
            Literal::Null => 120.0,
            value => match num(value) {
                Some(bpm) if bpm > 0.0 && bpm.is_finite() => bpm,
                _ => {
                    err("bpm", "`bpm` is a positive tempo");
                    120.0
                }
            },
        };
        Header { output, duration, bpm }
    }
}

struct Loader<'a> {
    tree: Option<TreeContext>,
    source: &'a str,
    graph: &'a Graph,
    diags: &'a mut Vec<Diagnostic>,
    duration: f64,
    /// `Generate` nodes, which `node` sets aside (CutDoc::generates).
    generates: Vec<crate::generate::GenerateNode>,
    /// `Graphic` nodes, set aside the same way (CutDoc::graphics).
    graphics: Vec<crate::graphic::GraphicNode>,
    /// The cut's tempo: musical spans resolve against it until the host
    /// gives the music's grid (`CutDoc::with_timing`).
    bpm: f64,
    /// Spans written with musical or spoken time.
    bindings: Vec<crate::timing::TimeBinding>,
}

/// One node's extraction: its id, location and the fields it wrote.
struct At<'a> {
    node: &'a Node,
}

impl<'a> Loader<'a> {
    fn push(&mut self, kind: DiagnosticKind, at: &At, field: Option<&str>, message: impl Into<String>) {
        let loc = field.and_then(|field| field_loc(self.source, node_loc(at.node), field)).unwrap_or_else(|| node_loc(at.node));
        self.diags.push(Diagnostic::new(kind, loc, Some(&at.node.id), message));
    }

    fn node(&mut self, node: &'a Node) -> Option<CutNode> {
        let at = At { node };
        let id = &node.id;
        let span = self.span(&at);
        let wire = |port: &str| input_wire(node, port);
        let kind = match node.type_name.as_str() {
            "Clip" => {
                let src = self.media(&at, "src", true)?;
                let nested = is_edit_ref(param(node, "src"));
                let audio = match param(node, "audio") {
                    // A library video's own sound plays; an app run's only
                    // when its audio is wired into the output.
                    None | Some(Literal::Null) => {
                        matches!(src, MediaRef::Asset(_)) || self.graph.edges.iter().any(|edge| edge.from_node == *id && edge.from_port == "audio")
                    }
                    Some(Literal::Bool(on)) => *on,
                    // The flag may also be written as a number: `audio: 1` or `audio: 0`.
                    Some(Literal::Num(on)) if *on == 0.0 || *on == 1.0 => *on == 1.0,
                    Some(_) => {
                        self.push(DiagnosticKind::InvalidParam, &at, Some("audio"), format!("`{id}.audio` is true or false (or 1 or 0)"));
                        false
                    }
                };
                CutNodeKind::Clip {
                    source_in: self.source_in(&at, &src),
                    src,
                    speed: self.positive(&at, "speed"),
                    fit: self.id_enum(&at, "fit", Fit::from_id, "@fill, @fit or @none"),
                    opacity: self.animated1(&at, "opacity"),
                    audio,
                    gain: self.animated1(&at, "gain"),
                    pan: self.animated1(&at, "pan"),
                    shape: self.shape(&at),
                    subtitles: match param(node, "subtitles") {
                        None | Some(Literal::Null) => false,
                        Some(Literal::Bool(on)) => *on,
                        Some(Literal::Num(on)) if *on == 0.0 || *on == 1.0 => *on == 1.0,
                        Some(_) => {
                            self.push(DiagnosticKind::InvalidParam, &at, Some("subtitles"), format!("`{id}.subtitles` is 1 or 0 (true or false)"));
                            false
                        }
                    },
                    nested,
                }
            }
            "AppView" => CutNodeKind::AppView {
                app: match name_of(param(node, "app")) {
                    Some(app) if !app.is_empty() => app,
                    _ => {
                        self.push(DiagnosticKind::InvalidParam, &at, Some("app"), format!("`{id}.app` names an entry of the slide's `apps` (`@counter`)"));
                        String::new()
                    }
                },
                fit: self.id_enum(&at, "fit", Fit::from_id, "@fill, @fit or @none"),
                rect: self.rect(&at),
            },
            "Camera" => CutNodeKind::Camera(self.camera(&at)),
            "Code" => CutNodeKind::Code(self.code(&at)),
            "Still" => CutNodeKind::Still {
                src: self.media(&at, "src", true)?,
                fit: self.id_enum(&at, "fit", Fit::from_id, "@fill, @fit or @none"),
                opacity: self.animated1(&at, "opacity"),
            },
            "Title" => CutNodeKind::Title {
                text: match param(node, "text") {
                    Some(Literal::Str(text)) => text.clone(),
                    _ => {
                        self.push(DiagnosticKind::InvalidParam, &at, Some("text"), format!("`{id}.text` is a string"));
                        String::new()
                    }
                },
                anchor: self.anchor(&at),
                font_size: self.animated1(&at, "font_size"),
                style: name_of(param(node, "style")).unwrap_or_default(),
            },
            "Fx" => CutNodeKind::Fx {
                input: {
                    let input = wire("input");
                    if input.is_none() {
                        self.push(DiagnosticKind::Graph, &at, None, format!("effect `{id}` needs its `input` wired"));
                    }
                    input
                },
                effect: self.effect(&at),
                params: std::array::from_fn(|i| self.animated1(&at, &format!("p{}", i + 1))),
                speed: self.positive(&at, "speed"),
            },
            "Transition" => {
                let (a, b) = (wire("a"), wire("b"));
                if a.is_none() || b.is_none() {
                    self.push(DiagnosticKind::TransitionInputs, &at, None, format!("transition `{id}` needs both `a` and `b` wired"));
                }
                let mix = match param(node, "mix") {
                    None | Some(Literal::Null) => None,
                    Some(_) => Some(self.animated1(&at, "mix")),
                };
                if span.is_none() && !mix.as_ref().is_some_and(Animated::is_keyed) {
                    self.push(DiagnosticKind::TransitionInputs, &at, None, format!("transition `{id}` needs a `span` (or keyed `mix`) to time its mix"));
                }
                CutNodeKind::Transition {
                    a,
                    b,
                    effect: match param(node, "effect") {
                        Some(Literal::Str(effect)) => effect.clone(),
                        Some(Literal::Obj(_)) => String::new(),
                        _ => {
                            self.push(DiagnosticKind::InvalidParam, &at, Some("effect"), format!("`{id}.effect` names a transition preset or is `asset(id, rev)` of a VjEffect"));
                            String::new()
                        }
                    },
                    // A library transition: `effect: asset(id, rev)`.
                    effect_asset: match param(node, "effect") {
                        Some(Literal::Obj(_)) => match self.media(&at, "effect", true) {
                            Some(MediaRef::Asset(asset)) if !is_edit_ref(param(node, "effect")) => Some(asset),
                            Some(_) => {
                                self.push(DiagnosticKind::InvalidRef, &at, Some("effect"), format!("`{id}.effect` is a preset name or `asset(id, rev)` of a VjEffect"));
                                None
                            }
                            None => None,
                        },
                        _ => None,
                    },
                    params: std::array::from_fn(|i| self.animated1(&at, &format!("p{}", i + 1))),
                    speed: self.positive(&at, "speed"),
                    mix,
                }
            }
            "Stack" => {
                let blends = match param(node, "blend") {
                    Some(Literal::Arr(items)) => items.iter().map(|item| name_of(Some(item)).and_then(|name| Blend::from_id(&name))).collect::<Vec<_>>(),
                    None | Some(Literal::Null) => Vec::new(),
                    Some(_) => vec![None],
                };
                if blends.iter().any(Option::is_none) {
                    self.push(DiagnosticKind::InvalidParam, &at, Some("blend"), format!("`{id}.blend` lists @normal, @add, @screen or @multiply per layer"));
                }
                let layers = input_wires(node, "layers");
                if blends.len() > layers.len() {
                    self.push(
                        DiagnosticKind::InvalidParam, &at, Some("blend"),
                        format!("`{id}.blend` lists {} blends for {} layers", blends.len(), layers.len()),
                    );
                }
                CutNodeKind::Stack {
                    layers: layers.into_iter().enumerate().map(|(i, wire)| (wire, blends.get(i).copied().flatten().unwrap_or_default())).collect(),
                }
            }
            "Reframe" => {
                let input = wire("input");
                if input.is_none() {
                    self.push(DiagnosticKind::Graph, &at, None, format!("reframe `{id}` needs its `input` wired"));
                }
                let mode = self.id_enum(&at, "mode", |id| match id {
                    "auto" => Some(ReframeMode::Auto),
                    "keys" => Some(ReframeMode::Keys),
                    _ => None,
                }, "@auto or @keys");
                let crop = match param(node, "crop") {
                    None | Some(Literal::Null) => None,
                    Some(_) => Some(self.animated::<4>(&at, "crop")),
                };
                if mode == ReframeMode::Keys && crop.is_none() {
                    self.push(DiagnosticKind::InvalidParam, &at, Some("mode"), format!("reframe `{id}` in @keys mode needs a `crop`"));
                }
                if let Some(crop) = &crop {
                    let degenerate = |v: &[f64; 4]| !(v[2] > 0.0 && v[3] > 0.0);
                    let bad = match crop {
                        Animated::Const(v) => degenerate(v),
                        Animated::Keys(keys) => keys.points.iter().any(|point| degenerate(&point.v)),
                    };
                    if bad {
                        self.push(DiagnosticKind::InvalidParam, &at, Some("crop"), format!("`{id}.crop` windows have a positive width and height"));
                    }
                }
                // The focus track is per source: the automatic camera reframes
                // exactly one clip.
                let input_type = input.as_ref().and_then(|wire| self.graph.nodes.iter().find(|node| node.id == wire.node)).map(|node| node.type_name.as_str());
                if mode == ReframeMode::Auto && input.is_some() && input_type != Some("Clip") {
                    self.push(DiagnosticKind::Graph, &at, Some("input"), format!("reframe `{id}` in @auto mode takes a `Clip` as its input"));
                }
                let aspect = match param(node, "aspect") {
                    None | Some(Literal::Null) => None,
                    Some(value) => match nums::<2>(value) {
                        Some(aspect) if aspect[0] > 0.0 && aspect[1] > 0.0 => Some(aspect),
                        _ => {
                            self.push(DiagnosticKind::InvalidParam, &at, Some("aspect"), format!("`{id}.aspect` is `[w, h]`"));
                            None
                        }
                    },
                };
                CutNodeKind::Reframe {
                    input,
                    aspect,
                    crop,
                    mode,
                    focus: self.media(&at, "focus", false),
                    safe: self.id_enum(&at, "safe", SafeZone::from_id, "@reels, @tiktok, @shorts or @none"),
                }
            }
            "AudioClip" => {
                let src = self.media(&at, "src", true)?;
                CutNodeKind::AudioClip {
                    source_in: self.source_in(&at, &src),
                    src,
                    gain: self.animated1(&at, "gain"),
                    pan: self.animated1(&at, "pan"),
                    shape: self.shape(&at),
                }
            }
            "VoiceOver" => CutNodeKind::VoiceOver {
                voice: self.voice(&at),
                lines: self.lines(&at),
                run: match self.media(&at, "run", false) {
                    Some(MediaRef::Run(run)) => Some(run),
                    Some(MediaRef::Asset(_) | MediaRef::MissingOptional(_)) => {
                        self.push(DiagnosticKind::InvalidRef, &at, Some("run"), format!("`{id}.run` is `run(id, rev, render_id, render_rev)`"));
                        None
                    }
                    None => None,
                },
                gain: self.animated1(&at, "gain"),
                pan: self.animated1(&at, "pan"),
                shape: self.shape(&at),
            },
            "Subtitles" => {
                let source = wire("source");
                if source.is_none() {
                    self.push(DiagnosticKind::Graph, &at, None, format!("subtitles `{id}` need their `source` wired: `source: voice.out(@audio)`"));
                }
                let max_words = match param(node, "max_words").and_then(num) {
                    Some(words) if words >= 1.0 && words.fract() == 0.0 && words <= 64.0 => words as u32,
                    None => 6,
                    Some(_) => {
                        self.push(DiagnosticKind::InvalidParam, &at, Some("max_words"), format!("`{id}.max_words` is a whole number from 1 to 64"));
                        6
                    }
                };
                let transcript = match self.media(&at, "transcript", false) {
                    Some(MediaRef::Asset(asset)) => Some(asset),
                    Some(MediaRef::Run(_) | MediaRef::MissingOptional(_)) => {
                        self.push(DiagnosticKind::InvalidRef, &at, Some("transcript"), format!("`{id}.transcript` is `asset(id, rev)` of a text asset"));
                        None
                    }
                    None => None,
                };
                CutNodeKind::Subtitles {
                    source,
                    style: name_of(param(node, "style")).unwrap_or_default(),
                    anchor: match param(node, "anchor") {
                        None | Some(Literal::Null) => [0.5, 0.8],
                        Some(_) => self.anchor(&at),
                    },
                    safe: self.id_enum(&at, "safe", SafeZone::from_id, "@reels, @tiktok, @shorts or @none"),
                    max_words,
                    transcript,
                    language: match param(node, "language") {
                        None | Some(Literal::Null) => "en".into(),
                        Some(Literal::Str(language)) if !language.trim().is_empty() => language.trim().to_string(),
                        Some(_) => {
                            self.push(DiagnosticKind::InvalidParam, &at, Some("language"), format!("`{id}.language` is a language code such as \"en\""));
                            "en".into()
                        }
                    },
                    font_size: self.animated1(&at, "font_size"),
                }
            }
            "Graphic" => {
                if let Some(graphic) = self.graphic(&at, span) {
                    self.graphics.push(graphic);
                }
                return None;
            }
            "Generate" => {
                if let Some(generate) = self.generate(&at, span) {
                    self.generates.push(generate);
                }
                return None;
            }
            "CutOutput" => {
                let frame = wire("frame");
                if frame.is_none() {
                    self.push(DiagnosticKind::Graph, &at, None, format!("`{id}` needs its `frame` wired"));
                }
                CutNodeKind::Output { frame, audio: input_wires(node, "audio") }
            }
            other => {
                self.push(DiagnosticKind::Graph, &at, None, format!("`{id}` is a `{other}`, which is not a cut node"));
                return None;
            }
        };
        let track = match param(node, "track").and_then(num) {
            Some(track) if track >= 0.0 && track.fract() == 0.0 && track <= u32::MAX as f64 => track as u32,
            None => 0,
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, &at, Some("track"), format!("`{id}.track` is a whole number from 0"));
                0
            }
        };
        Some(CutNode { id: id.clone(), span, track, loc: node_loc(node), kind })
    }

    fn span(&mut self, at: &At) -> Option<Span> {
        let value = param(at.node, "span")?;
        if matches!(value, Literal::Null) {
            return None;
        }
        let id = &at.node.id;
        let Some([start, length]) = nums::<2>(value).or_else(|| self.musical_span(at, value)) else {
            self.push(DiagnosticKind::SpanOutOfRange, at, Some("span"),
                format!("`{id}.span` is `[start, length]` in seconds; the start may be beat(n), bar(n, beat), downbeat(n), onset(n), section(@name) or word(@voice, i), the length beats(x)"));
            return None;
        };
        if start < 0.0 || length <= 0.0 || (self.duration > 0.0 && start + length > self.duration + EPSILON) {
            self.push(
                DiagnosticKind::SpanOutOfRange, at, Some("span"),
                format!("`{id}.span` [{start}, {length}] lies outside the cut's 0..{} seconds", self.duration),
            );
        }
        Some(Span { start, length })
    }

    /// `span: [beat(8), beats(4)]` and the like: recorded as a
    /// binding and placed now on a steady grid at the cut's `bpm`.
    fn musical_span(&mut self, at: &At, value: &Literal) -> Option<[f64; 2]> {
        use crate::timing::{TimeBinding, TimeSlot};
        use makepad_motion::timing::{parse_time_fields, BeatGrid, Length, TimeArg, TimeExpr, TimeValue, Timing};
        let Literal::Arr(parts) = value else { return None };
        if parts.len() != 2 {
            return None;
        }
        let arg = |fields: &Vec<(String, Literal)>, name: &str| match fields.iter().find(|(k, _)| k == name).map(|(_, v)| v) {
            Some(Literal::Num(n)) => TimeArg::Num(*n),
            Some(Literal::Id(id)) => TimeArg::Id(id.clone()),
            Some(Literal::Str(text)) => TimeArg::Text(text.clone()),
            None | Some(Literal::Null) => TimeArg::Nil,
            _ => TimeArg::Other,
        };
        let read = |part: &Literal| -> Option<Result<TimeValue, String>> {
            match part {
                Literal::Num(n) => Some(Ok(TimeValue::Point(TimeExpr::Seconds(*n)))),
                Literal::Obj(fields) => parse_time_fields(&|name| arg(fields, name)),
                _ => None,
            }
        };
        let (start, length) = (read(&parts[0])?, read(&parts[1])?);
        let (start, length) = match (start, length) {
            (Ok(TimeValue::Point(start)), Ok(length)) => (start, length),
            (Err(message), _) | (_, Err(message)) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("span"), format!("`{}.span`: {message}", at.node.id));
                return None;
            }
            _ => return None,
        };
        let length = match length {
            TimeValue::Length(length) => length,
            TimeValue::Point(TimeExpr::Seconds(s)) => Length::Seconds(s),
            TimeValue::Point(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("span"), format!("`{}.span`'s length is seconds or `beats(x)`, not a moment", at.node.id));
                return None;
            }
        };
        let timing = Timing { grid: Some(BeatGrid::steady(self.bpm, 0.0)), ..Default::default() };
        let start_s = timing.resolve(&start, self.bpm).unwrap_or(0.0);
        let length_s = timing.length(length, start_s, self.bpm);
        self.bindings.push(TimeBinding { node: at.node.id.clone(), slot: TimeSlot::Span { start: start.clone(), length } });
        Some([start_s, length_s])
    }

    fn path_file(&mut self, at: &At, field: &str) -> Option<Option<TreeLookup>> {
        self.path_value(at, field, param(at.node, field))
    }

    fn path_value(&mut self, at: &At, field: &str, value: Option<&Literal>) -> Option<Option<TreeLookup>> {
        let path = match value {
            Some(Literal::Str(path)) if self.tree.is_some() => path.clone(),
            Some(Literal::Obj(fields)) if fields.iter().any(|(key, value)| key == "ref" && matches!(value, Literal::Id(kind) if kind == "graphic")) => {
                match fields.iter().find(|(key, _)| key == "path").map(|(_, value)| value) {
                    Some(Literal::Str(path)) => path.clone(),
                    _ => { self.push(DiagnosticKind::InvalidRef, at, Some(field), "graphic takes one tree path string"); return Some(None); }
                }
            },
            _ => return None,
        };
        let resolved = self.tree.as_ref().ok_or_else(|| "graphic(path) requires a tree context".to_string()).and_then(|tree| tree.lookup(&path));
        match resolved {
            Ok(TreeLookup::File(file)) => {
                if at.node.type_name == "Still" && file.kind != makepad_asset_data::AssetKind::Texture {
                    self.push(DiagnosticKind::InvalidRef, at, Some(field), "a Still's graphic must be a texture");
                    return Some(None);
                }
                Some(Some(TreeLookup::File(file)))
            },
            Ok(TreeLookup::MissingOptional(path)) => Some(Some(TreeLookup::MissingOptional(path))),
            Err(error) => { self.push(DiagnosticKind::InvalidRef, at, Some(field), error); Some(None) }
        }
    }

    fn media(&mut self, at: &At, field: &str, required: bool) -> Option<MediaRef> {
        let id = &at.node.id;
        let value = param(at.node, field);
        if let Some(file) = self.path_file(at, field) {
            return file.and_then(|file| match file {
                TreeLookup::MissingOptional(path) if field == "src" && matches!(at.node.type_name.as_str(), "Clip" | "Still" | "AudioClip") => Some(MediaRef::MissingOptional(path)),
                TreeLookup::MissingOptional(path) => { self.push(DiagnosticKind::InvalidRef, at, Some(field), format!("media not present: {path}")); None },
                TreeLookup::File(file) => match file.reference {
                    Some(reference) => Some(MediaRef::Asset(AssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() })),
                    None => { self.push(DiagnosticKind::InvalidRef, at, Some(field), format!("{}: media has not been imported", file.path)); None }
                },
            });
        }
        let Some(Literal::Obj(fields)) = value else {
            if !matches!(value, None | Some(Literal::Null)) {
                self.push(DiagnosticKind::InvalidRef, at, Some(field), format!("`{id}.{field}` is `asset(id, rev)` or `run(id, rev, render_id, render_rev)`"));
            } else if required {
                self.push(DiagnosticKind::MissingRef, at, Some(field), format!("`{id}` needs `{field}`: `asset(id, rev)` or `run(id, rev, render_id, render_rev)`"));
            }
            return None;
        };
        let text = |name: &str| fields.iter().find(|(key, _)| key == name).and_then(|(_, value)| match value {
            Literal::Str(text) => Some(text.clone()),
            _ => None,
        });
        let kind = fields.iter().find(|(key, _)| key == "ref").and_then(|(_, value)| name_of(Some(value)));
        let valid_id = |text: &Option<String>| text.as_deref().is_some_and(|text| text.parse::<AssetId>().is_ok());
        let valid_rev = |text: &Option<String>| text.as_deref().is_some_and(|text| text.parse::<AssetRevisionId>().is_ok());
        let (asset_id, rev, render_id, render) = (text("id"), text("rev"), text("render_id"), text("render"));
        if self.tree.is_some() && matches!(kind.as_deref(), Some("asset" | "run" | "cut")) {
            self.push(DiagnosticKind::InvalidRef, at, Some(field), "library references are forbidden inside a tree; use graphic(path)");
            return None;
        }
        let media = match kind.as_deref() {
            Some("asset") if valid_id(&asset_id) && valid_rev(&rev) => MediaRef::Asset(AssetRef { id: asset_id.unwrap(), rev: rev.unwrap() }),
            Some("run") if valid_id(&asset_id) && valid_rev(&rev) && valid_id(&render_id) && valid_rev(&render) => {
                MediaRef::Run(RunRef { id: asset_id.unwrap(), rev: rev.unwrap(), render_id: render_id.unwrap(), render: render.unwrap() })
            }
            // `cut(id, rev)`: only a Clip's source nests a cut.
            Some("cut") if valid_id(&asset_id) && valid_rev(&rev) => {
                if at.node.type_name != "Clip" || field != "src" {
                    self.push(DiagnosticKind::InvalidRef, at, Some(field), format!("`{id}.{field}` cannot be a cut; only a Clip's `src` nests one"));
                    return None;
                }
                MediaRef::Asset(AssetRef { id: asset_id.unwrap(), rev: rev.unwrap() })
            }
            Some("asset" | "run" | "cut") => {
                self.push(
                    DiagnosticKind::InvalidRef, at, Some(field),
                    format!("`{id}.{field}` names an asset as `ast_…` and every revision as `arev_…`"),
                );
                return None;
            }
            _ => {
                self.push(DiagnosticKind::InvalidRef, at, Some(field), format!("`{id}.{field}` is `asset(id, rev)` or `run(id, rev, render_id, render_rev)`"));
                return None;
            }
        };
        Some(media)
    }

    fn source_in(&mut self, at: &At, src: &MediaRef) -> SourceTime {
        let id = &at.node.id;
        match param(at.node, "in") {
            Some(Literal::Obj(fields)) => {
                let name = fields.iter().find(|(key, _)| key == "marker").and_then(|(_, value)| name_of(Some(value)));
                let offset = fields.iter().find(|(key, _)| key == "offset").and_then(|(_, value)| num(value));
                match (name, offset) {
                    (Some(name), Some(offset)) if offset.is_finite() => {
                        if !matches!(src, MediaRef::Run(_)) {
                            self.push(DiagnosticKind::InvalidRef, at, Some("in"), format!("`{id}.in` uses a marker, which only a `run()` source has"));
                        }
                        SourceTime::Marker { name, offset }
                    }
                    _ => {
                        self.push(DiagnosticKind::InvalidParam, at, Some("in"), format!("`{id}.in` is seconds or `marker(@name, offset)`"));
                        SourceTime::Seconds(0.0)
                    }
                }
            }
            value => match value.and_then(num) {
                Some(seconds) if seconds >= 0.0 && seconds.is_finite() => SourceTime::Seconds(seconds),
                _ => {
                    self.push(DiagnosticKind::InvalidParam, at, Some("in"), format!("`{id}.in` is seconds from 0 or `marker(@name, offset)`"));
                    SourceTime::Seconds(0.0)
                }
            },
        }
    }

    fn positive(&mut self, at: &At, field: &str) -> f64 {
        match param(at.node, field) {
            Some(value) => match num(value) {
                Some(value) if value > 0.0 && value.is_finite() => value,
                _ => {
                    let message = if matches!(value, Literal::Arr(_)) {
                        format!("`{}.{field}` is not animatable", at.node.id)
                    } else {
                        format!("`{}.{field}` is a positive number", at.node.id)
                    };
                    self.push(DiagnosticKind::InvalidParam, at, Some(field), message);
                    1.0
                }
            },
            None => 1.0,
        }
    }

    /// `rect: [x, y, w, h]`, fractions of the output inside it, or None.
    fn rect(&mut self, at: &At) -> Option<[f64; 4]> {
        let id = &at.node.id;
        match param(at.node, "rect") {
            None | Some(Literal::Null) => None,
            Some(value) => match nums::<4>(value) {
                Some(r) if r[0] >= 0.0 && r[1] >= 0.0 && r[2] > 0.0 && r[3] > 0.0 && r[0] + r[2] <= 1.0 + 1e-9 && r[1] + r[3] <= 1.0 + 1e-9 => Some(r),
                _ => {
                    self.push(DiagnosticKind::InvalidParam, at, Some("rect"), format!("`{id}.rect` is `[x, y, w, h]` as fractions of the output, inside it (the whole output is `[0, 0, 1, 1]`)"));
                    None
                }
            },
        }
    }

    fn code(&mut self, at: &At) -> CodeSpec {
        let id = &at.node.id;
        let node = at.node;
        let app = match name_of(param(node, "app")) {
            Some(app) if !app.is_empty() => app,
            _ => {
                self.push(DiagnosticKind::InvalidParam, at, Some("app"), format!("`{id}.app` names an entry of the slide's `apps` (`@counter`)"));
                String::new()
            }
        };
        let file = match param(node, "file") {
            Some(Literal::Str(file)) if !file.is_empty() && !file.starts_with('/') && !file.split('/').any(|part| part == "..") => file.clone(),
            _ => {
                self.push(DiagnosticKind::InvalidParam, at, Some("file"), format!("`{id}.file` is a path inside the workspace, relative to its root (\"examples/counter/src/main.rs\")"));
                String::new()
            }
        };
        let lines = match param(node, "lines") {
            None | Some(Literal::Null) => None,
            Some(value) => match nums::<2>(value) {
                Some([first, last]) if first >= 1.0 && last >= first && first.fract() == 0.0 && last.fract() == 0.0 => Some((first as u32, last as u32)),
                _ => {
                    self.push(DiagnosticKind::InvalidParam, at, Some("lines"), format!("`{id}.lines` is `[first, last]`, whole line numbers from 1 with first <= last"));
                    None
                }
            },
        };
        let mut size = (900, 700);
        for (field, slot) in [("width", &mut size.0), ("height", &mut size.1)] {
            match param(node, field) {
                None | Some(Literal::Null) => {}
                Some(Literal::Num(value)) if *value >= 16.0 && *value <= 8192.0 && value.fract() == 0.0 => *slot = *value as u32,
                Some(_) => self.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` is a whole number of pixels from 16 to 8192")),
            }
        }
        let font_size = match param(node, "font_size") {
            None | Some(Literal::Null) => 22.0,
            Some(Literal::Num(size)) if *size >= 6.0 && *size <= 200.0 => *size,
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("font_size"), format!("`{id}.font_size` is a text size in pixels from 6 to 200"));
                22.0
            }
        };
        let mut fit = self.id_enum(at, "fit", Fit::from_id, "@fill, @fit or @none");
        // An unwritten `fit` letterboxes: code is never cropped by surprise.
        if param(node, "fit").is_none_or(|value| matches!(value, Literal::Null)) {
            fit = Fit::Fit;
        }
        CodeSpec { app, file, lines, size, font_size, fit, rect: self.rect(at) }
    }

    fn camera(&mut self, at: &At) -> CameraSpec {
        let id = &at.node.id;
        let mut spec = CameraSpec::default();
        let node = at.node;
        spec.device = match param(node, "device") {
            None | Some(Literal::Null) => CameraDevice::First,
            Some(Literal::Str(name)) if !name.trim().is_empty() => CameraDevice::Name(name.trim().to_string()),
            Some(Literal::Num(index)) if *index >= 0.0 && index.fract() == 0.0 && *index < 64.0 => CameraDevice::Index(*index as u32),
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("device"), format!("`{id}.device` is part of the camera's name (\"OBSBOT\"), its number (0, 1, ...) or nil for the first"));
                CameraDevice::First
            }
        };
        for (field, slot, max) in [("width", &mut spec.width, 16384.0), ("height", &mut spec.height, 16384.0)] {
            match param(node, field) {
                None | Some(Literal::Null) => {}
                Some(Literal::Num(value)) if *value >= 1.0 && *value <= max && value.fract() == 0.0 => *slot = *value as u32,
                Some(_) => self.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` is a whole number of pixels")),
            }
        }
        match param(node, "fps") {
            None | Some(Literal::Null) => {}
            Some(Literal::Num(value)) if *value > 0.0 && *value <= 240.0 => spec.fps = *value,
            Some(_) => self.push(DiagnosticKind::InvalidParam, at, Some("fps"), format!("`{id}.fps` is a frame rate from above 0 to 240")),
        }
        match param(node, "crop") {
            None | Some(Literal::Null) => {}
            Some(value) => match nums::<4>(value) {
                Some(crop) if crop[0] >= 0.0 && crop[1] >= 0.0 && crop[2] > 0.0 && crop[3] > 0.0 && crop[0] + crop[2] <= 1.0 + 1e-9 && crop[1] + crop[3] <= 1.0 + 1e-9 => spec.crop = crop,
                _ => self.push(DiagnosticKind::InvalidParam, at, Some("crop"), format!("`{id}.crop` is `[x, y, w, h]` as fractions of the picture, inside it (the whole picture is `[0, 0, 1, 1]`)")),
            },
        }
        match param(node, "rotate") {
            None | Some(Literal::Null) => {}
            Some(Literal::Num(degrees)) if [0.0, 90.0, 180.0, 270.0].contains(degrees) => spec.rotate = *degrees as u32,
            Some(_) => self.push(DiagnosticKind::InvalidParam, at, Some("rotate"), format!("`{id}.rotate` is 0, 90, 180 or 270 (degrees clockwise)")),
        }
        match param(node, "mirror") {
            None | Some(Literal::Null) => {}
            Some(Literal::Bool(on)) => spec.mirror = *on,
            Some(Literal::Num(on)) if *on == 0.0 || *on == 1.0 => spec.mirror = *on == 1.0,
            Some(_) => self.push(DiagnosticKind::InvalidParam, at, Some("mirror"), format!("`{id}.mirror` is true or false")),
        }
        match param(node, "adjust") {
            None | Some(Literal::Null) => {}
            Some(Literal::Obj(fields)) => {
                for (key, value) in fields {
                    let (slot, range) = match key.as_str() {
                        "brightness" => (&mut spec.adjust.brightness, -1.0..=1.0),
                        "contrast" => (&mut spec.adjust.contrast, 0.0..=4.0),
                        "saturation" => (&mut spec.adjust.saturation, 0.0..=4.0),
                        other => {
                            self.push(DiagnosticKind::InvalidParam, at, Some("adjust"), format!("`{id}.adjust` has `brightness`, `contrast` and `saturation`, not `{other}`"));
                            continue;
                        }
                    };
                    match num(value) {
                        Some(v) if range.contains(&v) => *slot = v,
                        _ => self.push(DiagnosticKind::InvalidParam, at, Some("adjust"), format!("`{id}.adjust.{key}` is a number from {} to {}", range.start(), range.end())),
                    }
                }
            }
            Some(_) => self.push(DiagnosticKind::InvalidParam, at, Some("adjust"), format!("`{id}.adjust` is `{{brightness: 0 contrast: 1 saturation: 1}}`")),
        }
        spec.fit = self.id_enum(at, "fit", Fit::from_id, "@fill, @fit or @none");
        // An unwritten `fit` letterboxes: a document camera's picture is never cropped by surprise.
        if param(node, "fit").is_none_or(|value| matches!(value, Literal::Null)) {
            spec.fit = Fit::Fit;
        }
        spec.rect = self.rect(at);
        spec
    }

    fn id_enum<T: Default>(&mut self, at: &At, field: &str, parse: impl Fn(&str) -> Option<T>, expected: &str) -> T {
        match param(at.node, field) {
            None | Some(Literal::Null) => T::default(),
            value => match name_of(value).and_then(|name| parse(&name)) {
                Some(value) => value,
                None => {
                    self.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{}.{field}` is {expected}", at.node.id));
                    T::default()
                }
            },
        }
    }

    fn anchor(&mut self, at: &At) -> [f64; 2] {
        match param(at.node, "anchor") {
            None | Some(Literal::Null) => [0.5, 0.5],
            Some(value) => nums::<2>(value).unwrap_or_else(|| {
                self.push(DiagnosticKind::InvalidParam, at, Some("anchor"), format!("`{}.anchor` is `vec2(x, y)`", at.node.id));
                [0.5, 0.5]
            }),
        }
    }

    fn effect(&mut self, at: &At) -> EffectRef {
        match param(at.node, "effect") {
            Some(Literal::Str(name)) if !name.is_empty() => EffectRef::Preset(name.clone()),
            Some(Literal::Obj(_)) => match self.media(at, "effect", true) {
                Some(MediaRef::Asset(asset)) => EffectRef::Asset(asset),
                Some(MediaRef::Run(_) | MediaRef::MissingOptional(_)) => {
                    self.push(DiagnosticKind::InvalidRef, at, Some("effect"), format!("`{}.effect` is a preset name or `asset(id, rev)` of a VjEffect", at.node.id));
                    EffectRef::Preset(String::new())
                }
                None => EffectRef::Preset(String::new()),
            },
            _ => {
                self.push(DiagnosticKind::InvalidParam, at, Some("effect"), format!("`{}.effect` is a preset name or `asset(id, rev)` of a VjEffect", at.node.id));
                EffectRef::Preset(String::new())
            }
        }
    }

    /// `fade_in`, `fade_out` and `duck`. Whether `duck.under` names
    /// real sources is checked with the whole graph.
    fn shape(&mut self, at: &At) -> AudioShape {
        let id = &at.node.id;
        let seconds = |this: &mut Self, field: &str| match param(at.node, field) {
            None | Some(Literal::Null) => 0.0,
            Some(value) => match num(value) {
                Some(value) if value >= 0.0 && value.is_finite() => value,
                _ => {
                    this.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` is seconds from 0"));
                    0.0
                }
            },
        };
        let fade_in = seconds(self, "fade_in");
        let fade_out = seconds(self, "fade_out");
        let duck = match param(at.node, "duck") {
            None | Some(Literal::Null) => None,
            Some(Literal::Obj(fields)) => {
                let mut duck = Duck::default();
                let mut bad = None;
                for (key, value) in fields {
                    match (key.as_str(), value) {
                        ("under", Literal::Arr(items)) => {
                            duck.under = items.iter().filter_map(|item| name_of(Some(item))).collect();
                            if duck.under.len() != items.len() {
                                bad = Some("`under` lists node names: [@voice, …]");
                            }
                        }
                        ("under", value) if name_of(Some(value)).is_some() => duck.under = vec![name_of(Some(value)).unwrap()],
                        ("depth_db", value) if num(value).is_some_and(|db| db <= 0.0 && db >= -96.0) => duck.depth_db = num(value).unwrap(),
                        ("attack", value) if num(value).is_some_and(|s| s >= 0.0 && s.is_finite()) => duck.attack = num(value).unwrap(),
                        ("release", value) if num(value).is_some_and(|s| s >= 0.0 && s.is_finite()) => duck.release = num(value).unwrap(),
                        ("depth_db", _) => bad = Some("`depth_db` is a drop in dB from 0 to -96"),
                        ("attack" | "release", _) => bad = Some("`attack` and `release` are seconds from 0"),
                        _ => bad = Some("`duck` has `under`, `depth_db`, `attack` and `release`"),
                    }
                }
                if duck.under.is_empty() && bad.is_none() {
                    bad = Some("`under` names the sources to duck under: [@voice, …]");
                }
                match bad {
                    Some(message) => {
                        self.push(DiagnosticKind::InvalidParam, at, Some("duck"), format!("`{id}.duck`: {message}"));
                        None
                    }
                    None => Some(duck),
                }
            }
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("duck"), format!("`{id}.duck` is `{{under: [@voice] depth_db: -12 attack: 0.08 release: 0.4}}`"));
                None
            }
        };
        AudioShape { fade_in, fade_out, duck }
    }

    /// `voice: {engine: @kokoro id: "bm_daniel" speed: 1.0}`.
    fn voice(&mut self, at: &At) -> VoiceSpec {
        let id = &at.node.id;
        let mut voice = VoiceSpec::default();
        let fields = match param(at.node, "voice") {
            None | Some(Literal::Null) => return voice,
            Some(Literal::Obj(fields)) => fields,
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("voice"), format!("`{id}.voice` is `{{engine: @kokoro id: \"bm_daniel\" speed: 1.0}}`"));
                return voice;
            }
        };
        let mut bad = None;
        for (key, value) in fields {
            match key.as_str() {
                "engine" => match name_of(Some(value)).and_then(|name| VoiceEngine::from_id(&name)) {
                    Some(engine) => voice.engine = engine,
                    None => bad = Some("`engine` is @kokoro or @indextts".to_string()),
                },
                "id" => match value {
                    Literal::Str(name) | Literal::Id(name) if !name.trim().is_empty() => voice.id = name.trim().to_string(),
                    _ => bad = Some("`id` names the voice, e.g. \"bm_daniel\"".to_string()),
                },
                "speed" => match num(value) {
                    Some(speed) if speed >= 0.25 && speed <= 4.0 => voice.speed = speed,
                    _ => bad = Some("`speed` is from 0.25 to 4".to_string()),
                },
                "reference" => match value {
                    Literal::Obj(ref_fields) => {
                        let text = |name: &str| ref_fields.iter().find(|(key, _)| key == name).and_then(|(_, value)| match value {
                            Literal::Str(text) => Some(text.clone()),
                            _ => None,
                        });
                        match (text("id"), text("rev")) {
                            (Some(asset), Some(rev)) if asset.parse::<AssetId>().is_ok() && rev.parse::<AssetRevisionId>().is_ok() => {
                                voice.reference = Some(AssetRef { id: asset, rev });
                            }
                            _ => bad = Some("`reference` is `asset(id, rev)` of a recording".to_string()),
                        }
                    }
                    Literal::Null => {}
                    _ => bad = Some("`reference` is `asset(id, rev)` of a recording".to_string()),
                },
                other => bad = Some(format!("`{other}` is not a voice field (engine, id, speed, reference)")),
            }
        }
        if voice.engine == VoiceEngine::IndexTts && voice.reference.is_none() && bad.is_none() {
            bad = Some("@indextts clones a `reference: asset(id, rev)` recording".to_string());
        }
        if let Some(message) = bad {
            self.push(DiagnosticKind::InvalidParam, at, Some("voice"), format!("`{id}.voice`: {message}"));
        }
        voice
    }

    /// `lines: [line(text, at, take, key), …]`.
    fn lines(&mut self, at: &At) -> Vec<VoiceLine> {
        let id = &at.node.id;
        let items = match param(at.node, "lines") {
            None | Some(Literal::Null) => return Vec::new(),
            Some(Literal::Arr(items)) => items,
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("lines"), format!("`{id}.lines` is `[line(text, at), …]`"));
                return Vec::new();
            }
        };
        let mut lines = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let Literal::Obj(fields) = item else {
                self.push(DiagnosticKind::InvalidParam, at, Some("lines"), format!("`{id}.lines[{index}]` is `line(text, at)`"));
                continue;
            };
            let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
            let Some(Literal::Str(text)) = field("line") else {
                self.push(DiagnosticKind::InvalidParam, at, Some("lines"), format!("`{id}.lines[{index}]` is `line(text, at)` with the text as a string"));
                continue;
            };
            if text.trim().is_empty() {
                self.push(DiagnosticKind::InvalidParam, at, Some("lines"), format!("`{id}.lines[{index}]` has no text"));
            }
            let when = match field("at") {
                Some(Literal::Obj(marker)) => {
                    let name = marker.iter().find(|(key, _)| key == "marker").and_then(|(_, value)| name_of(Some(value)));
                    let offset = marker.iter().find(|(key, _)| key == "offset").and_then(|(_, value)| num(value));
                    match (name, offset) {
                        (Some(name), Some(offset)) if offset.is_finite() => SourceTime::Marker { name, offset },
                        _ => {
                            self.push(DiagnosticKind::InvalidParam, at, Some("lines"), format!("`{id}.lines[{index}]` starts at seconds or `marker(@name, offset)`"));
                            SourceTime::Seconds(0.0)
                        }
                    }
                }
                value => match value.and_then(num) {
                    Some(seconds) if seconds >= 0.0 && seconds.is_finite() => {
                        if self.duration > 0.0 && seconds >= self.duration {
                            self.push(
                                DiagnosticKind::SpanOutOfRange, at, Some("lines"),
                                format!("`{id}.lines[{index}]` starts at {seconds} s, after the cut's {} seconds", self.duration),
                            );
                        }
                        SourceTime::Seconds(seconds)
                    }
                    _ => {
                        self.push(DiagnosticKind::InvalidParam, at, Some("lines"), format!("`{id}.lines[{index}]` starts at seconds from 0 or `marker(@name, offset)`"));
                        SourceTime::Seconds(0.0)
                    }
                },
            };
            let take = if let Some(file) = self.path_value(at, "lines", field("take")) {
                file.and_then(|file| {
                    let file = match file {
                        TreeLookup::File(file) => file,
                        TreeLookup::MissingOptional(_) => return None,
                    };
                    if file.kind != makepad_asset_data::AssetKind::Audio {
                        self.push(DiagnosticKind::InvalidRef, at, Some("lines"), format!("{}: a voice take must be audio", file.path));
                        return None;
                    }
                    match file.reference {
                        Some(reference) => Some(AssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() }),
                        None => { self.push(DiagnosticKind::InvalidRef, at, Some("lines"), format!("{}: import the voice take before using it", file.path)); None }
                    }
                })
            } else { match field("take") {
                None | Some(Literal::Null) => None,
                Some(Literal::Obj(take)) => {
                    let text = |name: &str| take.iter().find(|(key, _)| key == name).and_then(|(_, value)| match value {
                        Literal::Str(text) => Some(text.clone()),
                        _ => None,
                    });
                    let kind = take.iter().find(|(key, _)| key == "ref").and_then(|(_, value)| name_of(Some(value)));
                    match (kind.as_deref(), text("id"), text("rev")) {
                        (Some("asset"), Some(asset), Some(rev)) if asset.parse::<AssetId>().is_ok() && rev.parse::<AssetRevisionId>().is_ok() => {
                            Some(AssetRef { id: asset, rev })
                        }
                        _ => {
                            self.push(DiagnosticKind::InvalidRef, at, Some("lines"), format!("`{id}.lines[{index}]`'s take is `asset(id, rev)` of an Audio asset"));
                            None
                        }
                    }
                }
                Some(_) => {
                    self.push(DiagnosticKind::InvalidRef, at, Some("lines"), format!("`{id}.lines[{index}]`'s take is `asset(id, rev)` of an Audio asset"));
                    None
                }
            } };
            let key = match field("key") {
                None | Some(Literal::Null) => None,
                Some(Literal::Str(key)) => Some(key.clone()),
                Some(_) => {
                    self.push(DiagnosticKind::InvalidParam, at, Some("lines"), format!("`{id}.lines[{index}]`'s key is a string"));
                    None
                }
            };
            lines.push(VoiceLine { text: text.clone(), at: when, take, key });
        }
        lines
    }

    fn animated1(&mut self, at: &At, field: &str) -> Animated<1> {
        self.animated::<1>(at, field)
    }

    /// A constant (a number, or `[a, b, …]` of `N` numbers) or `keys(...)`.
    fn animated<const N: usize>(&mut self, at: &At, field: &str) -> Animated<N> {
        let id = &at.node.id;
        let default = || Animated::Const([0.0; N]);
        let Some(value) = param(at.node, field) else { return default() };
        let constant = |value: &Literal| if N == 1 { num(value).map(|value| [value; N]) } else { nums::<N>(value) };
        if let Some(value) = constant(value) {
            if value.iter().all(|value| value.is_finite()) {
                return Animated::Const(value);
            }
        }
        let Literal::Arr(points) = value else {
            self.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` is a number or `keys(...)`"));
            return default();
        };
        if !nodes::animatable(&at.node.type_name, field) {
            self.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` is not animatable"));
            return default();
        }
        let shape = if N == 1 { "a number".to_string() } else { format!("[{} numbers]", N) };
        let mut keys = Vec::with_capacity(points.len());
        for point in points {
            let Literal::Arr(parts) = point else {
                self.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` keys are `[t, value]` or `[t, value, @ease]`"));
                return default();
            };
            let (t, value) = match (parts.first().and_then(num), parts.get(1).and_then(constant)) {
                (Some(t), Some(value)) if t.is_finite() && value.iter().all(|value| value.is_finite()) => (t, value),
                _ => {
                    self.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` keys are `[t, value]` with a time and {shape}"));
                    return default();
                }
            };
            // After `[t, v]`: an optional ease, then an optional `@pin`.
            let tail = parts[2..].iter().map(|part| name_of(Some(part))).collect::<Vec<_>>();
            let pinned = tail.last().is_some_and(|last| last.as_deref() == Some("pin"));
            let ease_part = &tail[..tail.len() - pinned as usize];
            let ease = match ease_part {
                [] => Ease::Linear,
                [Some(name)] if Ease::from_id(name).is_some() => Ease::from_id(name).unwrap(),
                _ => {
                    self.push(
                        DiagnosticKind::UnknownEase, at, Some(field),
                        format!("`{id}.{field}` key at {t} is `[t, value, @ease, @pin]`; eases are @linear, @ease_in, @ease_out, @ease_in_out or @hold"),
                    );
                    Ease::Linear
                }
            };
            keys.push(KeyPoint { t, v: value, ease, pinned });
        }
        if keys.is_empty() {
            self.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` has no keys"));
            return default();
        }
        if let Some(pair) = keys.windows(2).find(|pair| pair[1].t <= pair[0].t) {
            self.push(
                DiagnosticKind::KeysNotSorted, at, Some(field),
                format!("`{id}.{field}` key times must increase: {} comes after {}", pair[1].t, pair[0].t),
            );
            return default();
        }
        Animated::Keys(Keys { points: keys })
    }

    /// Audio wiring: `duck.under` names other audio sources, a
    /// voiceover whose lines use markers knows its run (the one run the
    /// clips play when it names none), and subtitles caption an audio
    /// source.
    fn check_audio(&mut self, nodes: &mut [CutNode]) {
        let audio_ids = nodes.iter()
            .filter(|node| matches!(node.kind, CutNodeKind::Clip { .. } | CutNodeKind::AudioClip { .. } | CutNodeKind::VoiceOver { .. }))
            .map(|node| node.id.clone())
            .collect::<HashSet<_>>();
        let mut clip_runs = Vec::new();
        for node in nodes.iter() {
            if let CutNodeKind::Clip { src: MediaRef::Run(run), .. } = &node.kind {
                if !clip_runs.contains(run) {
                    clip_runs.push(run.clone());
                }
            }
        }
        for node in nodes.iter_mut() {
            let graph_node = self.graph.nodes.iter().find(|graph_node| graph_node.id == node.id);
            let Some(graph_node) = graph_node else { continue };
            let at = At { node: graph_node };
            match &mut node.kind {
                CutNodeKind::Clip { shape, .. } | CutNodeKind::AudioClip { shape, .. } | CutNodeKind::VoiceOver { shape, .. } => {
                    if let Some(duck) = &shape.duck {
                        for under in &duck.under {
                            if under == &node.id || !audio_ids.contains(under) {
                                self.push(
                                    DiagnosticKind::Graph, &at, Some("duck"),
                                    format!("`{}.duck` ducks under `{under}`, which is not another audio source of this cut", node.id),
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
            match &mut node.kind {
                CutNodeKind::VoiceOver { lines, run, .. } => {
                    let uses_markers = lines.iter().any(|line| matches!(line.at, SourceTime::Marker { .. }));
                    if uses_markers && run.is_none() {
                        match clip_runs.as_slice() {
                            [only] => *run = Some(only.clone()),
                            [] => self.push(DiagnosticKind::InvalidRef, &at, Some("lines"), format!("`{}`'s lines use markers, but no clip plays an app run", node.id)),
                            _ => self.push(
                                DiagnosticKind::InvalidRef, &at, Some("lines"),
                                format!("`{}`'s lines use markers and the clips play several runs: name one with `run: run(…)`", node.id),
                            ),
                        }
                    }
                    if let Some(run) = run.as_ref().filter(|_| uses_markers) {
                        if !clip_runs.contains(run) {
                            self.push(DiagnosticKind::InvalidRef, &at, Some("run"), format!("`{}.run` is not played by any clip, so its markers have no place in the cut", node.id));
                        }
                    }
                }
                CutNodeKind::Subtitles { source: Some(source), .. } => {
                    if !audio_ids.contains(&source.node) || source.port != "audio" {
                        self.push(DiagnosticKind::Graph, &at, Some("source"), format!("`{}.source` is the `audio` of a VoiceOver, AudioClip or Clip", node.id));
                    }
                }
                _ => {}
            }
        }
    }

    /// Whole-graph checks: one output, no cycles, decoder concurrency.
    fn check_graph(&mut self, nodes: &[CutNode], flow_loc: Loc) -> Option<String> {
        let outputs = nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Output { .. })).collect::<Vec<_>>();
        match outputs.as_slice() {
            [_] => {}
            [] => self.diags.push(Diagnostic::new(DiagnosticKind::Graph, flow_loc.clone(), None, "a cut has one `CutOutput`")),
            [_, extra, ..] => self.diags.push(Diagnostic::new(DiagnosticKind::Graph, extra.loc.clone(), Some(&extra.id), "a cut has only one `CutOutput`")),
        }
        if let Some(node) = cycle(self.graph) {
            let loc = self.graph.nodes.iter().find(|candidate| candidate.id == node).map(node_loc).unwrap_or(flow_loc);
            self.diags.push(Diagnostic::new(DiagnosticKind::Cycle, loc, Some(&node), format!("the wiring loops back through `{node}`")));
        }
        self.check_clip_limit(nodes);
        outputs.first().map(|node| node.id.clone())
    }

    /// No more than [`MAX_ACTIVE_CLIPS`] clips play at any time.
    fn check_clip_limit(&mut self, nodes: &[CutNode]) {
        let mut events = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            if !matches!(node.kind, CutNodeKind::Clip { .. }) || matches!(node.kind, CutNodeKind::Clip { src: MediaRef::MissingOptional(_), .. }) {
                continue;
            }
            let span = node.span.unwrap_or(Span { start: 0.0, length: self.duration.max(EPSILON) });
            // Ends sort before starts at one time: spans are half-open.
            events.push((span.start, 1, index));
            events.push((span.end(), 0, index));
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut active = 0usize;
        for (time, start, index) in events {
            if start == 0 {
                active -= 1;
                continue;
            }
            active += 1;
            if active > MAX_ACTIVE_CLIPS {
                let node = &nodes[index];
                self.diags.push(Diagnostic::new(
                    DiagnosticKind::TooManyClips, node.loc.clone(), Some(&node.id),
                    format!("{active} clips are active at {time} s; a cut plays at most {MAX_ACTIVE_CLIPS} at once"),
                ));
                return;
            }
        }
    }
}

impl Loader<'_> {
    /// `Generate{input span flow frame_at prompt negative seed params fit
    /// takes take track}`.
    fn generate(&mut self, at: &At, span: Option<Span>) -> Option<crate::generate::GenerateNode> {
        use crate::generate::{GenTake, GenerateNode};
        let node = at.node;
        let id = &node.id;
        let Some(span) = span else {
            self.push(DiagnosticKind::SpanOutOfRange, at, None, format!("generate `{id}` needs a `span: [start, length]`"));
            return None;
        };
        let input = input_wire(node, "input");
        match &input {
            None => self.push(DiagnosticKind::Graph, at, None, format!("generate `{id}` needs its `input` wired: `input: clip.out(@frame)`")),
            Some(wire) if wire.port != "frame" => {
                self.push(DiagnosticKind::Graph, at, Some("input"), format!("`{id}.input` is a picture: `{}.out(@frame)`", wire.node));
            }
            Some(_) => {}
        }
        let preset = match param(node, "flow") {
            Some(Literal::Str(key)) if !key.trim().is_empty() => key.trim().to_string(),
            Some(Literal::Obj(fields)) => match fields.iter().find(|(key, _)| key == "preset").map(|(_, value)| value) {
                Some(Literal::Str(key)) if !key.trim().is_empty() => key.trim().to_string(),
                _ => {
                    self.push(DiagnosticKind::InvalidParam, at, Some("flow"), format!("`{id}.flow` is `preset(\"key\")`"));
                    String::new()
                }
            },
            _ => {
                self.push(DiagnosticKind::MissingRef, at, Some("flow"), format!("generate `{id}` needs `flow: preset(\"key\")`, e.g. preset(\"bundled:image-to-video\")"));
                String::new()
            }
        };
        let frame_at = match param(node, "frame_at") {
            None | Some(Literal::Null) => None,
            Some(value) => match num(value) {
                Some(at_time) if at_time >= 0.0 && at_time.is_finite() && (self.duration <= 0.0 || at_time < self.duration) => Some(at_time),
                _ => {
                    self.push(DiagnosticKind::InvalidParam, at, Some("frame_at"), format!("`{id}.frame_at` is a cut second inside the cut, or nil"));
                    None
                }
            },
        };
        let text = |this: &mut Self, field: &str| match param(node, field) {
            None | Some(Literal::Null) => String::new(),
            Some(Literal::Str(text)) => text.clone(),
            Some(_) => {
                this.push(DiagnosticKind::InvalidParam, at, Some(field), format!("`{id}.{field}` is a string"));
                String::new()
            }
        };
        let prompt = text(self, "prompt");
        let negative = text(self, "negative");
        let whole = |value: Option<&Literal>, limit: f64| match value.and_then(num) {
            Some(value) if value >= 0.0 && value.fract() == 0.0 && value <= limit => Some(value as u64),
            _ => None,
        };
        const MAX_SEED: f64 = 9_007_199_254_740_991.0;
        let seed = match param(node, "seed") {
            None | Some(Literal::Null) => 0,
            value => whole(value, MAX_SEED).unwrap_or_else(|| {
                self.push(DiagnosticKind::InvalidParam, at, Some("seed"), format!("`{id}.seed` is a whole number from 0"));
                0
            }),
        };
        let scalar = |value: &Literal| matches!(value, Literal::Num(_) | Literal::Str(_) | Literal::Bool(_) | Literal::Id(_));
        let mut params = match param(node, "params") {
            None | Some(Literal::Null) => Vec::new(),
            Some(Literal::Obj(fields)) if fields.iter().all(|(_, value)| scalar(value) || matches!(value, Literal::Arr(items) if items.iter().all(scalar))) => fields.clone(),
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("params"), format!("`{id}.params` is `{{name: value …}}` of numbers, strings or lists of them"));
                Vec::new()
            }
        };
        params.sort_by(|a, b| a.0.cmp(&b.0));
        let fit = self.id_enum(at, "fit", Fit::from_id, "@fill, @fit or @none");
        fn field<'f>(fields: &'f [(String, Literal)], name: &str) -> Option<&'f Literal> {
            fields.iter().find(|(key, _)| key == name).map(|(_, value)| value)
        }
        let mut takes = Vec::new();
        match param(node, "takes") {
            None | Some(Literal::Null) => {}
            Some(Literal::Arr(items)) => {
                for (index, item) in items.iter().enumerate() {
                    let parsed = match item {
                        Literal::Obj(fields) => {
                            let video = match field(fields, "take") {
                                Some(Literal::Obj(video)) => {
                                    let text = |name: &str| field(video, name).and_then(|value| match value {
                                        Literal::Str(text) => Some(text.clone()),
                                        _ => None,
                                    });
                                    match (field(video, "ref").and_then(|value| name_of(Some(value))).as_deref(), text("id"), text("rev")) {
                                        (Some("asset"), Some(asset), Some(rev)) if asset.parse::<AssetId>().is_ok() && rev.parse::<AssetRevisionId>().is_ok() => Some(AssetRef { id: asset, rev }),
                                        _ => None,
                                    }
                                }
                                _ => None,
                            };
                            let key = match field(fields, "key") {
                                None | Some(Literal::Null) => Some(None),
                                Some(Literal::Str(key)) => Some(Some(key.clone())),
                                Some(_) => None,
                            };
                            let seconds = match field(fields, "seconds") {
                                None | Some(Literal::Null) => Some(None),
                                Some(Literal::Num(seconds)) if *seconds > 0.0 && seconds.is_finite() => Some(Some(*seconds)),
                                Some(_) => None,
                            };
                            let model = match field(fields, "model") {
                                None | Some(Literal::Null) => Some(String::new()),
                                Some(Literal::Str(model)) => Some(model.clone()),
                                Some(_) => None,
                            };
                            match (video, whole(field(fields, "seed"), MAX_SEED), key, seconds, model) {
                                (Some(video), Some(seed), Some(key), Some(seconds), Some(model)) => Some(GenTake { video, seed, key, seconds, model }),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    match parsed {
                        Some(take) => takes.push(take),
                        None => self.push(
                            DiagnosticKind::InvalidRef, at, Some("takes"),
                            format!("`{id}.takes[{index}]` is `take(asset(id, rev), seed, key, seconds, model)` of a Video"),
                        ),
                    }
                }
            }
            Some(_) => self.push(DiagnosticKind::InvalidParam, at, Some("takes"), format!("`{id}.takes` is `[take(…), …]`")),
        }
        let take = match param(node, "take") {
            None | Some(Literal::Null) => 0,
            value => match whole(value, u32::MAX as f64) {
                Some(take) if (take as usize) < takes.len() || (take == 0 && takes.is_empty()) => take as usize,
                _ => {
                    self.push(DiagnosticKind::InvalidParam, at, Some("take"), format!("`{id}.take` is the index of one of its {} takes", takes.len()));
                    0
                }
            },
        };
        let track = match param(node, "track").and_then(num) {
            Some(track) if track >= 0.0 && track.fract() == 0.0 && track <= u32::MAX as f64 => track as u32,
            None => 0,
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("track"), format!("`{id}.track` is a whole number from 0"));
                0
            }
        };
        Some(GenerateNode { id: id.clone(), span, track, loc: node_loc(node), input, frame_at, preset, prompt, negative, seed, params, fit, takes, take })
    }
}

impl Loader<'_> {
    /// `Graphic{src span fit params opacity track}`.
    fn graphic(&mut self, at: &At, span: Option<Span>) -> Option<crate::graphic::GraphicNode> {
        use crate::graphic::{GraphicNode, GraphicSrc};
        let node = at.node;
        let id = node.id.clone();
        let src = if let Some(file) = self.path_file(at, "src") {
            let file = match file? {
                TreeLookup::File(file) => file,
                TreeLookup::MissingOptional(path) => {
                    self.push(DiagnosticKind::InvalidRef, at, Some("src"), format!("media not present: {path}; a Graphic requires Motion source"));
                    return None;
                },
            };
            if file.kind != makepad_asset_data::AssetKind::Motion {
                self.push(DiagnosticKind::InvalidRef, at, Some("src"), "a Graphic's graphic must be a Motion document");
                return None;
            }
            let context = self.tree.as_ref()?.at(&file.path);
            let modules = makepad_motion::modules::Modules::from_tree(context.clone());
            let checked = file.text().and_then(|source| makepad_motion::modules::resolve(source, &*modules.0).map(|_| ()).map_err(|e| e.join("\n")));
            if let Err(error) = checked {
                self.push(DiagnosticKind::InvalidRef, at, Some("src"), error);
                return None;
            }
            GraphicSrc::Tree(context)
        } else { match self.media(at, "src", true)? {
            MediaRef::Asset(asset) if !is_edit_ref(param(node, "src")) => GraphicSrc::Asset(asset),
            _ => {
                self.push(DiagnosticKind::InvalidRef, at, Some("src"), format!("`{id}.src` is `asset(id, rev)` of a Motion asset"));
                return None;
            }
        } };
        let fit = self.id_enum(at, "fit", Fit::from_id, "@fill, @fit or @none");
        // A graphic letterboxes unless told otherwise (a clip fills).
        let fit = if param(node, "fit").is_none() { Fit::Fit } else { fit };
        let mut problems = Vec::new();
        let params = crate::graphic::params(&id, param(node, "params"), self.tree.as_ref(), &mut problems);
        for (field, kind, message) in problems {
            self.push(kind, at, field, message);
        }
        let opacity = match param(node, "opacity") {
            None => Animated::scalar(1.0),
            Some(_) => self.animated1(at, "opacity"),
        };
        let track = match param(node, "track").and_then(num) {
            Some(track) if track >= 0.0 && track.fract() == 0.0 => track as u32,
            _ => 0,
        };
        // `bind: @clip` or `bind: run(…)` (lane M4, crate::bind).
        let bind = match param(node, "bind") {
            None | Some(Literal::Null) => None,
            Some(Literal::Id(clip)) | Some(Literal::Str(clip)) => Some(crate::bind::BindTarget::Clip(clip.clone())),
            Some(Literal::Obj(_)) => match self.media(at, "bind", false) {
                Some(MediaRef::Run(run)) => Some(crate::bind::BindTarget::Run(run)),
                Some(_) => {
                    self.push(DiagnosticKind::InvalidRef, at, Some("bind"), format!("`{id}.bind` is an app run: `@clip` (the Clip that plays it) or `run(id, rev, render_id, render_rev)`"));
                    None
                }
                None => None,
            },
            Some(_) => {
                self.push(DiagnosticKind::InvalidParam, at, Some("bind"), format!("`{id}.bind` is `@clip` (the Clip that plays the app run) or `run(id, rev, render_id, render_rev)`"));
                None
            }
        };
        Some(GraphicNode { id, span, track, loc: node_loc(node), src, fit, params, opacity, bind })
    }
}

/// The node on a cycle of the graph's edges, if there is one.
fn cycle(graph: &Graph) -> Option<String> {
    let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        next.entry(edge.from_node.as_str()).or_default().push(edge.to_node.as_str());
    }
    // Iterative three-colour depth-first search.
    let mut done = HashSet::new();
    for root in graph.nodes.iter().map(|node| node.id.as_str()) {
        if done.contains(root) {
            continue;
        }
        let mut on_path = HashSet::from([root]);
        let mut stack = vec![(root, 0usize)];
        while let Some((node, child)) = stack.last_mut() {
            let children = next.get(node).map(Vec::as_slice).unwrap_or(&[]);
            if let Some(&to) = children.get(*child) {
                *child += 1;
                if on_path.contains(to) {
                    return Some(to.to_string());
                }
                if !done.contains(to) {
                    on_path.insert(to);
                    stack.push((to, 0));
                }
            } else {
                on_path.remove(*node);
                done.insert(*node);
                stack.pop();
            }
        }
    }
    None
}

/// The `[stack, id]` pairs the prelude's `finish` recorded.
fn synthetic_layers(metadata: Option<&Node>) -> Vec<(String, u64)> {
    let Some(Literal::Arr(pairs)) = metadata.and_then(|node| param(node, "synthetic")) else { return Vec::new() };
    pairs.iter().filter_map(|pair| match pair {
        Literal::Arr(pair) => match (name_of(pair.first()), pair.get(1).and_then(num)) {
            (Some(stack), Some(id)) => Some((stack, id as u64)),
            _ => None,
        },
        _ => None,
    }).collect()
}

/// Whether a `src` value is `cut(id, rev)`.
fn is_edit_ref(value: Option<&Literal>) -> bool {
    let Some(Literal::Obj(fields)) = value else { return false };
    fields.iter().any(|(key, value)| key == "ref" && name_of(Some(value)).as_deref() == Some("cut"))
}

/// A graph node's 1-based location.
fn node_loc(node: &Node) -> Loc {
    Loc { line: node.loc.line + 1, col: node.loc.col + 1 }
}

fn param<'n>(node: &'n Node, name: &str) -> Option<&'n Literal> {
    node.params.iter().find(|(key, _)| key == name).map(|(_, value)| value)
}

fn num(value: &Literal) -> Option<f64> {
    match value {
        Literal::Num(value) => Some(*value),
        _ => None,
    }
}

fn nums<const N: usize>(value: &Literal) -> Option<[f64; N]> {
    let Literal::Arr(items) = value else { return None };
    if items.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, item) in out.iter_mut().zip(items) {
        *slot = num(item)?;
    }
    Some(out)
}

/// An `@id` or a string, as text.
fn name_of(value: Option<&Literal>) -> Option<String> {
    match value? {
        Literal::Id(name) | Literal::Str(name) => Some(name.clone()),
        _ => None,
    }
}

fn input_wire(node: &Node, port: &str) -> Option<Wire> {
    match &node.inputs.iter().find(|input| input.port == port)?.value {
        NodeInputValue::Edge(edge) => Some(Wire { node: edge.from_node.clone(), port: edge.from_port.clone() }),
        _ => None,
    }
}

fn input_wires(node: &Node, port: &str) -> Vec<Wire> {
    match node.inputs.iter().find(|input| input.port == port).map(|input| &input.value) {
        Some(NodeInputValue::Edges(entries)) => entries.iter().map(|entry| Wire { node: entry.from.from_node.clone(), port: entry.from.from_port.clone() }).collect(),
        Some(NodeInputValue::Edge(edge)) => vec![Wire { node: edge.from_node.clone(), port: edge.from_port.clone() }],
        _ => Vec::new(),
    }
}

/// Skip a string or comment starting at `index`; the index after it.
fn skip_literal(bytes: &[u8], index: usize) -> Option<usize> {
    match (bytes[index], bytes.get(index + 1)) {
        (b'"', _) => {
            let mut i = index + 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            Some((i + 1).min(bytes.len()))
        }
        (b'/', Some(b'/')) => Some(bytes[index..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |end| index + end)),
        (b'/', Some(b'*')) => Some(source_find(bytes, index + 2, b"*/").map_or(bytes.len(), |end| end + 2)),
        _ => None,
    }
}

fn source_find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes.get(from..)?.windows(needle.len()).position(|window| window == needle).map(|at| from + at)
}

fn ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The byte range of the source's last top-level `Flow{...}`: the `F` and
/// the closing brace.
pub(crate) fn final_flow(source: &str) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let (mut depth, mut index, mut found, mut open) = (0usize, 0usize, None, None);
    while index < bytes.len() {
        if let Some(next) = skip_literal(bytes, index) {
            index = next;
            continue;
        }
        match bytes[index] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(start) = open.take() {
                        found = Some((start, index));
                    }
                }
            }
            b'F' if depth == 0 && source[index..].starts_with("Flow")
                && (index == 0 || !(ident_byte(bytes[index - 1]) || bytes[index - 1] == b'.')) =>
            {
                let rest = &source[index + 4..];
                if rest.trim_start().starts_with('{') && !rest.bytes().next().is_some_and(ident_byte) {
                    open = Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    found
}

fn line_col(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset];
    let line = before.matches('\n').count() as u32 + 1;
    let col = before.rsplit('\n').next().map_or(0, |line| line.chars().count()) as u32 + 1;
    (line, col)
}

fn offset_of(source: &str, loc: &Loc) -> Option<usize> {
    let mut offset = 0;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        if index + 1 == loc.line as usize {
            let col = line.char_indices().nth(loc.col.saturating_sub(1) as usize).map_or(line.len(), |(at, _)| at);
            return Some(offset + col);
        }
        offset += line.len();
    }
    None
}

/// Where `field:` is written in the object whose construction is at `loc`
/// (a node's `Type{`, or the `Flow{`), at that object's own brace depth.
pub(crate) fn field_loc(source: &str, loc: Loc, field: &str) -> Option<Loc> {
    let bytes = source.as_bytes();
    let from = offset_of(source, &loc)?;
    let mut index = from + bytes.get(from..)?.iter().position(|&byte| byte == b'{')?;
    let mut depth = 0usize;
    while index < bytes.len() {
        if let Some(next) = skip_literal(bytes, index) {
            index = next;
            continue;
        }
        match bytes[index] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                depth -= 1;
                if depth == 0 {
                    return None;
                }
            }
            byte if depth == 1 && ident_byte(byte) && !ident_byte(bytes[index - 1]) => {
                let end = index + bytes[index..].iter().position(|&byte| !ident_byte(byte)).unwrap_or(bytes.len() - index);
                if &source[index..end] == field && source[end..].trim_start().starts_with(':') {
                    let (line, col) = line_col(source, index);
                    return Some(Loc { line, col });
                }
                index = end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_final_top_level_flow() {
        let source = "let a = X{s: \"Flow{\"}\n// Flow{\nFlow{ a }\n";
        let (start, end) = final_flow(source).unwrap();
        assert_eq!(&source[start..=end], "Flow{ a }");
        assert!(final_flow("let a = 1").is_none());
        assert!(final_flow("let f = FlowX{}").is_none());
    }

    #[test]
    fn finds_fields_at_the_objects_own_depth() {
        let source = "let a = Clip{src: asset(\"x\", \"y\")\n    span: [0.0, {span: 1}]}\n";
        assert_eq!(field_loc(source, Loc { line: 1, col: 9 }, "span"), Some(Loc { line: 2, col: 5 }));
        assert_eq!(field_loc(source, Loc { line: 1, col: 9 }, "fit"), None);
    }

    #[test]
    fn cycles_are_found() {
        let mut graph = makepad_flow::graph::evaluate("use mod.flow.*\nFlow{}", "t").unwrap();
        for (from, to) in [("a", "b"), ("b", "c"), ("c", "a")] {
            graph.edges.push(makepad_flow::Edge { from_node: from.into(), from_port: "o".into(), to_node: to.into(), to_port: "i".into(), conn: None });
        }
        assert!(cycle(&graph).is_none(), "edges between unlisted nodes are not walked");
        for id in ["a", "b", "c"] {
            let mut node = makepad_flow::graph::evaluate("use mod.flow.*\nlet n = Input{}\nFlow{n}", "t").unwrap().nodes.remove(0);
            node.id = id.into();
            graph.nodes.push(node);
        }
        assert!(cycle(&graph).is_some());
    }
}

fn check_tree(source: &str, tree: &TreeContext) -> Result<(), Vec<Diagnostic>> {
    makepad_motion::tree::validate(source, tree).map_err(|diags| diags.into_iter().map(|d|
        Diagnostic::new(DiagnosticKind::InvalidRef, Loc { line: d.line, col: d.col }, None, format!("{}: {}", tree.document, d.message))
    ).collect())
}

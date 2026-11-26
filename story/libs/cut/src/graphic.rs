//! Motion graphics in a cut: `Graphic{src: asset(id, rev)
//! span: [s, l] fit: @fit params: {headline: "Hi" accent: "#ff5a36"}}`
//! draws a Motion document (a `Motion` asset) over its span, its clock
//! starting at the span's start. `params` fill the document's declared
//! inputs; colours are written as `"#rrggbb"` strings (the cut grammar has
//! no colour literal), assets as `asset(id, rev)`.
//!
//! `Title` lowers to a Graphic of the built-in "title" document
//! ([`title_params`]), so the cut renderer draws every graphic, titles
//! included, through one path.
//!
//! Graphic nodes live in [`crate::CutDoc::graphics`], beside the other
//! nodes, so consumers that match every [`crate::CutNodeKind`] keep
//! building (as `Generate` does); the frame plan, the dependencies and the
//! graph see them.

use crate::model::*;
use makepad_flow::{Literal, Loc};
pub use makepad_motion::InputValue;
use makepad_motion::AssetRef as MotionAssetRef;

/// What a Graphic draws.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphicSrc {
    /// A document in the current tree, including an unpublished working file.
    Tree(makepad_motion::tree::TreeContext),
    /// A `Motion` asset revision.
    Asset(AssetRef),
    /// A document built into the renderer (`makepad_motion::builtin`).
    Builtin(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphicNode {
    pub id: String,
    /// `None`: always active.
    pub span: Option<Span>,
    pub track: u32,
    /// 1-based, like the other nodes.
    pub loc: Loc,
    pub src: GraphicSrc,
    /// How the document's own size sits in the output: @fit (letterbox,
    /// the default), @fill (crop to cover) or @none (1:1 centred).
    pub fit: Fit,
    /// Input values by name.
    pub params: Vec<(String, InputValue)>,
    pub opacity: Animated<1>,
    /// `bind: @clip` / `bind: run(…)`: the app run the document's `run()`
    /// follows (lane M4, [`crate::bind`]).
    pub bind: Option<crate::bind::BindTarget>,
}

impl GraphicNode {
    pub fn active(&self, t: f64) -> bool {
        t.is_finite() && self.span.map_or(true, |span| span.contains(t))
    }

    /// Seconds since the span's start: the document's clock.
    pub fn local(&self, t: f64) -> f64 {
        t - self.span.map_or(0.0, |span| span.start)
    }

    /// Every library revision the graphic names: its document and any
    /// asset params.
    pub fn assets(&self) -> Vec<AssetRef> {
        let mut out = Vec::new();
        if let GraphicSrc::Asset(asset) = &self.src {
            out.push(asset.clone());
        }
        for (_, value) in &self.params {
            if let InputValue::Asset(asset) = value {
                let asset = AssetRef { id: asset.id.clone(), rev: asset.rev.clone() };
                if !out.contains(&asset) {
                    out.push(asset);
                }
            }
        }
        out
    }
}

/// A cut's input values (named in its text) as the motion engine takes
/// them: by identifier.
pub fn motion_inputs(params: &[(String, InputValue)]) -> Vec<(makepad_motion::LiveId, InputValue)> {
    params.iter().map(|(name, value)| (makepad_motion::input_id(name), value.clone())).collect()
}

impl CutDoc {
    /// A slide can name a cut or a Motion graphic. Motion is lowered from its
    /// parsed model directly to one graphic and one output, never to Splash text.
    pub fn from_graphic(file: &makepad_motion::tree::TreeFile, tree: makepad_motion::tree::TreeContext, output: (u32, u32)) -> Result<Self, Vec<Diagnostic>> {
        let error = |message: String| vec![Diagnostic::new(DiagnosticKind::InvalidRef, Loc { line: 1, col: 1 }, None, message)];
        let source = file.text().map_err(error)?;
        let tree = tree.at(&file.path);
        if file.kind == makepad_asset_data::AssetKind::Cut { return Self::load_tree(source, tree); }
        if file.kind != makepad_asset_data::AssetKind::Motion { return Err(error(format!("{}: a slide graphic must be Cut or Motion source", file.path))); }
        let context = makepad_motion::MotionContext { modules: makepad_motion::modules::Modules::from_tree(tree.clone()), ..Default::default() };
        let motion = makepad_motion::MotionDoc::load_with(source, &[], context).map_err(|diags| error(diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")))?;
        let loc = Loc { line: 1, col: 1 };
        Ok(Self {
            missing_media: tree.resolver.missing_optional(),
            source: source.into(), label: motion.label.clone(),
            output: OutputFormat { width: output.0, height: output.1, fps: motion.fps, ..OutputFormat::default() },
            duration: motion.duration, bpm: motion.bpm,
            nodes: vec![CutNode { id: "output".into(), span: None, track: 0, loc: loc.clone(), kind: CutNodeKind::Output { frame: Some(Wire { node: "graphic".into(), port: "frame".into() }), audio: Vec::new() } }],
            output_node: "output".into(),
            // There is no Flow source to patch for a Motion-root slide.
            graph: makepad_flow::Graph { revision: 0, label: motion.label, brief: String::new(), trigger: String::new(), concurrency: 1, autostart: false, nodes: Vec::new(), edges: Vec::new(), tools: Vec::new(), flow_ui_src: None, warnings: Vec::new(), components: Vec::new() },
            synthetic_layers: Vec::new(), generates: Vec::new(), time_bindings: Vec::new(),
            graphics: vec![GraphicNode { id: "graphic".into(), span: None, track: 0, loc,
                src: GraphicSrc::Tree(tree),
                fit: Fit::Fit, params: Vec::new(), opacity: Animated::scalar(1.0), bind: None }],
        })
    }
}

/// The inputs of the built-in "title" document for a cut `Title` in a
/// `width` x `height` output: its text, the frame, the anchor in pixels
/// and the size.
pub fn title_params(text: &str, anchor: [f64; 2], font_size: f64, width: u32, height: u32) -> Vec<(String, InputValue)> {
    vec![
        ("text".into(), InputValue::Text(text.to_string())),
        ("frame".into(), InputValue::Vec2([width as f64, height as f64])),
        ("position".into(), InputValue::Vec2([anchor[0] * width as f64, anchor[1] * height as f64])),
        ("font_size".into(), InputValue::Number(font_size)),
    ]
}

/// A param value from the cut's Splash.
fn param_value(value: &Literal, tree: Option<&makepad_motion::tree::TreeContext>) -> Result<InputValue, String> {
    Ok(match value {
        Literal::Str(text) => InputValue::Text(text.clone()),
        Literal::Num(n) if n.is_finite() => InputValue::Number(*n),
        Literal::Bool(b) => InputValue::Bool(*b),
        Literal::Id(id) => InputValue::Text(id.clone()),
        Literal::Arr(items) => match items.as_slice() {
            [Literal::Num(x), Literal::Num(y)] => InputValue::Vec2([*x, *y]),
            // A list input's items (a Motion `Repeat{source: in(@rows)}`).
            _ => InputValue::List(items.iter().map(|value| item_value(value, tree)).collect::<Result<_, _>>()?),
        },
        Literal::Obj(fields) => {
            let text = |name: &str| fields.iter().find(|(key, _)| key == name).and_then(|(_, value)| match value {
                Literal::Str(text) => Some(text.clone()),
                _ => None,
            });
            if fields.iter().any(|(key, value)| key == "ref" && matches!(value, Literal::Id(kind) if kind == "graphic")) {
                let path = text("path").ok_or("graphic takes a tree path")?;
                let file = match tree.ok_or("graphic requires a project tree")?.lookup(&path)? {
                    makepad_motion::tree::TreeLookup::File(file) => file,
                    makepad_motion::tree::TreeLookup::MissingOptional(_) => return Ok(InputValue::Empty),
                };
                let reference = file.reference.ok_or_else(|| format!("{}: media has not been imported", file.path))?;
                return Ok(InputValue::Asset(MotionAssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() }));
            }
            match (text("id"), text("rev")) {
                (Some(id), Some(rev)) if fields.iter().any(|(key, value)| key == "ref" && matches!(value, Literal::Id(kind) if kind == "asset")) => {
                    InputValue::Asset(MotionAssetRef { id, rev })
                }
                _ => return Err("an asset param is `asset(id, rev)`".into()),
            }
        }
        _ => return Err("params are text, numbers, bools, `[x, y]`, `\"#rrggbb\"` colours, `asset(id, rev)` or lists of them".into()),
    })
}

/// An item of a list param: a param value, or a record of them
/// (`{name: "Ada" score: 12}`).
fn item_value(value: &Literal, tree: Option<&makepad_motion::tree::TreeContext>) -> Result<InputValue, String> {
    match value {
        Literal::Obj(fields) if !fields.iter().any(|(key, _)| key == "ref") => {
            Ok(InputValue::Record(fields.iter().map(|(key, value)| Ok((key.clone(), item_value(value, tree)?))).collect::<Result<_, String>>()?))
        }
        other => param_value(other, tree),
    }
}

/// Everything wrong with a Graphic's own fields, as (field, kind, message).
pub(crate) type Problems = Vec<(Option<&'static str>, DiagnosticKind, String)>;

/// `params: {…}` of node `id`.
pub(crate) fn params(id: &str, value: Option<&Literal>, tree: Option<&makepad_motion::tree::TreeContext>, problems: &mut Problems) -> Vec<(String, InputValue)> {
    match value {
        None | Some(Literal::Null) => Vec::new(),
        Some(Literal::Obj(fields)) => fields.iter().filter_map(|(name, value)| match param_value(value, tree) {
            Ok(value) => Some((name.clone(), value)),
            Err(message) => {
                problems.push((Some("params"), DiagnosticKind::InvalidParam, format!("`{id}.params.{name}`: {message}")));
                None
            }
        }).collect(),
        Some(_) => {
            problems.push((Some("params"), DiagnosticKind::InvalidParam, format!("`{id}.params` is `{{name: value}}`, the document's inputs")));
            Vec::new()
        }
    }
}

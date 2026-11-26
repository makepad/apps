//! Source-preserving writes of a cut: a save changes only the text of what was edited.
//!
//! Nodes are added, removed and rewired through the flow crate's patchers
//! (`patch_add_nodes_closure_with_registry`, `patch_inputs_…`,
//! `patch_remove_nodes_…`). Those evaluate the source with a plain
//! `FlowVm`, which knows neither `mod.cut` nor the cut's header fields, so
//! they run on a *patch view* of the cut: the `mod.cut` prelude in front,
//! and the `output`/`duration`/`bpm` header entries inside comments. The
//! view is undone exactly afterwards; the patchers never touch the prelude
//! or a comment, so every other byte of the cut stays as written.
//!
//! Parameter values (spans, `in`, `track`, the header) have no flow patcher
//! yet (the timeline wants one); [`Patcher::set_params`] replaces exactly
//! the value text of each field and proves the result: the cut loads, and
//! its graph equals the old graph with those parameters set.
//!
//! Every write evaluates Splash (several times); run it on a worker.

use super::source::{self, constructor, final_flow};
use makepad_flow::document::closure::Closure;
use makepad_flow::document::Registry;
use makepad_flow::graph::{
    catalog_with_registry, patch_add_nodes_closure_with_registry, patch_inputs_closure_with_registry,
    patch_remove_nodes_closure_with_registry, FlowVm,
};
use makepad_flow::wire::NodeTypeCatalog;
use makepad_flow::{Graph, Literal, Loc};
use makepad_flow_ui::graph_edit;
use makepad_story_cut::{register_document, CutDoc, PRELUDE};

const VIEW_FILE: &str = "<edit-patch>";
const HEADER_OPEN: &str = "/*<edit-header>";
const HEADER_CLOSE: &str = "</edit-header>*/";
/// The `Flow{}` entries only a cut has.
const HEADER_FIELDS: [&str; 3] = ["output", "duration", "bpm"];

/// A parameter value: the text written into the source and the literal it
/// evaluates to, which the write is checked against.
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    pub text: String,
    pub literal: Literal,
}

impl Value {
    pub fn num(value: f64) -> Self {
        let text = source::number(value);
        Self { literal: Literal::Num(text.parse().unwrap_or(value)), text }
    }
    pub fn span(start: f64, length: f64) -> Self {
        let (start, length) = (Self::num(start), Self::num(length));
        Self { text: format!("[{}, {}]", start.text, length.text), literal: Literal::Arr(vec![start.literal, length.literal]) }
    }
    pub fn string(text: &str) -> Self {
        Self { text: source::string_literal(text), literal: Literal::Str(text.to_string()) }
    }
    pub fn id(id: &str) -> Self {
        Self { text: format!("@{id}"), literal: Literal::Id(id.to_string()) }
    }
    pub fn nil() -> Self {
        Self { text: "nil".into(), literal: Literal::Null }
    }
    /// `keys([t, v], [t, v, @ease], …)`.
    pub fn keys(points: &[(f64, f64, Option<&str>)]) -> Self {
        let mut texts = Vec::new();
        let mut literals = Vec::new();
        for (t, v, ease) in points {
            let (t, v) = (Self::num(*t), Self::num(*v));
            let mut literal = vec![t.literal, v.literal];
            let mut text = format!("[{}, {}", t.text, v.text);
            if let Some(ease) = ease {
                text.push_str(&format!(", @{ease}"));
                literal.push(Literal::Id(ease.to_string()));
            }
            text.push(']');
            texts.push(text);
            literals.push(Literal::Arr(literal));
        }
        Self { text: format!("keys({})", texts.join(", ")), literal: Literal::Arr(literals) }
    }
    /// A list of values: `[a, b]`.
    pub fn list(items: Vec<Value>) -> Self {
        Self { text: format!("[{}]", items.iter().map(|item| item.text.as_str()).collect::<Vec<_>>().join(", ")), literal: Literal::Arr(items.into_iter().map(|item| item.literal).collect()) }
    }
    /// An object `{key: value …}`, in order.
    pub fn object(fields: Vec<(&str, Value)>) -> Self {
        Self {
            text: format!("{{{}}}", fields.iter().map(|(key, value)| format!("{key}: {}", value.text)).collect::<Vec<_>>().join(" ")),
            literal: Literal::Obj(fields.into_iter().map(|(key, value)| (key.to_string(), value.literal)).collect()),
        }
    }
}

/// A node to add: its fresh id, registered type, parameters, and the wires
/// into its own inputs (`(input, from node, from port)`).
#[derive(Clone, Debug)]
pub struct NewNode {
    pub id: String,
    pub type_name: String,
    pub at: (f64, f64),
    pub params: Vec<(String, Value)>,
    pub wires: Vec<(String, String, String)>,
}

pub struct Patcher {
    registry: Registry,
    catalog: Vec<NodeTypeCatalog>,
    pub tree: Option<(std::sync::Arc<makepad_asset_data::tree::TreeDraft>, String)>,
}

/// A cut with its header folded into comments and the prelude in front.
struct View {
    text: String,
    prefix: String,
}

impl View {
    fn new(source: &str) -> Result<Self, String> {
        let flow = final_flow(source).ok_or("a cut ends with one `Flow{...}`")?;
        let mut folded = source.to_string();
        // Back to front, so earlier spans keep their offsets.
        let mut header = source::fields(source, flow).into_iter()
            .filter(|field| field.name.as_deref().is_some_and(|name| HEADER_FIELDS.contains(&name)))
            .collect::<Vec<_>>();
        header.sort_by_key(|field| std::cmp::Reverse(field.start));
        for field in header {
            let text = &source[field.start..field.value.1];
            if text.contains("*/") || text.contains(HEADER_OPEN) {
                return Err("the cut's header holds a comment; edit its source".into());
            }
            folded.insert_str(field.value.1, HEADER_CLOSE);
            folded.insert_str(field.start, HEADER_OPEN);
        }
        let prefix = format!("{PRELUDE}\n");
        Ok(Self { text: format!("{prefix}{folded}"), prefix })
    }

    fn undo(&self, text: &str) -> Result<String, String> {
        let body = text.strip_prefix(self.prefix.as_str()).ok_or("the write changed the cut prelude")?;
        Ok(body.replace(HEADER_OPEN, "").replace(HEADER_CLOSE, ""))
    }
}

fn plain(graph: &Graph) -> Graph {
    let mut graph = graph.clone();
    graph.revision = 0;
    graph.warnings.clear();
    for node in &mut graph.nodes {
        node.loc = Loc::default();
    }
    graph
}

impl Patcher {
    pub fn document(source: &str, edits: &[makepad_studio_api::document::Edit]) -> Result<String, String> {
        makepad_studio_api::document::patch(source, edits)
    }
    pub fn new() -> Result<Self, String> {
        let mut registry = Registry::with_builtins();
        register_document(&mut registry)?;
        let catalog = catalog_with_registry(&registry).map_err(|error| error.to_string())?;
        Ok(Self { registry, catalog, tree: None })
    }

    pub fn load(&self, source: &str) -> Result<CutDoc, String> {
        match &self.tree {
            Some((tree, path)) => CutDoc::load_tree(source, tree.context(path)),
            None => CutDoc::load(source),
        }.map_err(diagnostics)
    }

    pub fn media_value(&self, media: &makepad_story_cut::MediaRef) -> Value {
        use makepad_story_cut::MediaRef;
        if let Some((tree, _)) = &self.tree {
            let (id, rev) = match media {
                MediaRef::Asset(asset) => (&asset.id, &asset.rev),
                MediaRef::Run(run) => (&run.render_id, &run.render),
                MediaRef::MissingOptional(_) => return super::ops::media_value(media),
            };
            if let Some((path, _)) = tree.files.iter().find(|(_, file)| file.reference.is_some_and(|reference| reference.asset_id.to_string() == *id && reference.revision.to_string() == *rev)) {
                return Value { text: format!("graphic({})", source::string_literal(path)), literal: Literal::Obj(vec![("ref".into(), Literal::Id("graphic".into())), ("path".into(), Literal::Str(path.clone()))]) };
            }
        }
        super::ops::media_value(media)
    }

    fn view_graph(&self, view: &View) -> Result<Graph, String> {
        FlowVm::load_with_registry(&view.text, VIEW_FILE, &Closure::default(), &self.registry)
            .map(|(_, graph)| graph)
            .map_err(|error| format!("the cut cannot be patched: {}", error.message))
    }

    /// Whether `id` is free: no word of the cut or its prelude is `id`.
    pub fn fresh(&self, source: &str, id: &str) -> bool {
        let words = |text: &str| text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')).any(|word| word == id);
        !words(source) && !words(PRELUDE)
    }

    /// The first `<stem><n>` from 1 that is free.
    pub fn fresh_id(&self, source: &str, stem: &str) -> String {
        (1..).map(|n| format!("{stem}{n}")).find(|id| self.fresh(source, id)).unwrap()
    }

    /// Append `nodes` (each with its own input wires) as new constructors
    /// and Flow members.
    pub fn add_nodes(&self, source: &str, nodes: &[NewNode]) -> Result<String, String> {
        let view = View::new(source)?;
        let before = self.view_graph(&view)?;
        let mut wanted = before.clone();
        for node in nodes {
            let catalog = self.catalog.iter().find(|catalog| catalog.type_name == node.type_name)
                .ok_or_else(|| format!("node type `{}` is not registered", node.type_name))?;
            wanted.nodes.push(graph_edit::node_from_catalog(node.id.clone(), catalog, node.at));
            for (key, value) in &node.params {
                wanted = graph_edit::set_param(&wanted, &node.id, key, value.literal.clone());
            }
            for (input, from, port) in &node.wires {
                wanted = graph_edit::connect(&wanted, from, port, &node.id, input)?;
            }
        }
        let (written, _) = patch_add_nodes_closure_with_registry(&view.text, &wanted, &Closure::default(), &self.registry)
            .map_err(|error| error.to_string())?;
        let mut written = view.undo(&written)?;
        // The writer spells new constructors `mod.flow.Type{...}` and media
        // references as their object values; a cut says `Clip{...}` and
        // `asset(...)`. Same values, so the load below proves it.
        for node in nodes {
            written = self.tidy_constructor(&written, node)?;
        }
        Ok(written)
    }

    fn tidy_constructor(&self, source: &str, node: &NewNode) -> Result<String, String> {
        let (open, _) = constructor(source, &node.id)?;
        let let_at = source[..open].rfind("let ").ok_or("no constructor")?;
        let head = &source[let_at..open];
        let short = if node.type_name == "CutText" { "Text" } else { node.type_name.as_str() };
        let tidy_head = head.replace(&format!("mod.flow.{}", node.type_name), short);
        let mut written = format!("{}{}{}", &source[..let_at], tidy_head, &source[open..]);
        for (key, value) in &node.params {
            if matches!(value.literal, Literal::Obj(_)) {
                let object = constructor(&written, &node.id)?;
                written = source::set_field(&written, object, key, &value.text);
            }
        }
        Ok(written)
    }

    /// Change input wires with `edit` over the evaluated graph.
    pub fn rewire(&self, source: &str, edit: impl FnOnce(&Graph) -> Result<Graph, String>) -> Result<String, String> {
        let view = View::new(source)?;
        let before = self.view_graph(&view)?;
        let wanted = edit(&before)?;
        if wanted == before {
            return Ok(source.to_string());
        }
        let (written, _) = patch_inputs_closure_with_registry(&view.text, &wanted, &Closure::default(), &self.registry)
            .map_err(|error| error.to_string())?;
        view.undo(&written)
    }

    /// Remove nodes nothing else reads.
    pub fn remove_nodes(&self, source: &str, ids: &[String]) -> Result<String, String> {
        let view = View::new(source)?;
        let written = patch_remove_nodes_closure_with_registry(&view.text, ids, &Closure::default(), &self.registry)
            .map_err(|error| error.to_string())?;
        view.undo(&written)
    }

    /// Set authored parameters of node `id` (or of the header, for `None`)
    /// by replacing exactly their value text; then prove it: the cut
    /// loads and its graph is the old graph with only these values set.
    pub fn set_params(&self, source: &str, id: Option<&str>, values: &[(String, Value)]) -> Result<String, String> {
        let before = self.load(source)?;
        let mut written = source.to_string();
        for (key, value) in values {
            let object = match id {
                Some(id) => constructor(&written, id)?,
                None => final_flow(&written).ok_or("a cut ends with one `Flow{...}`")?,
            };
            if source::field_text(&written, object, key).is_some_and(|text| !source::plain_literal(text)) {
                return Err(format!("`{}.{key}` is an expression; edit its source", id.unwrap_or("Flow")));
            }
            written = source::set_field(&written, object, key, &value.text);
        }
        let after = self.load(&written)?;
        match id {
            Some(id) => {
                let mut expected = before.graph.clone();
                for (key, value) in values {
                    expected = graph_edit::set_param(&expected, id, key, value.literal.clone());
                }
                if plain(&expected) != plain(&after.graph) {
                    return Err(format!("`{id}`'s parameters are computed in its source; edit the source"));
                }
            }
            None => {
                let same = values.iter().all(|(key, value)| match (key.as_str(), &value.literal) {
                    ("duration", Literal::Num(v)) => after.duration == *v,
                    ("bpm", Literal::Num(v)) => after.bpm == *v,
                    ("label", Literal::Str(v)) => after.label == *v,
                    // The output format (A1: size, fps, loudness).
                    ("output", Literal::Obj(fields)) => {
                        let get = |name: &str| fields.iter().find(|(key, _)| key == name).and_then(|(_, value)| match value {
                            Literal::Num(value) => Some(*value),
                            _ => None,
                        });
                        get("width") == Some(after.output.width as f64) && get("height") == Some(after.output.height as f64)
                            && get("fps") == Some(after.output.fps) && get("loudness") == after.output.loudness
                    }
                    _ => false,
                });
                if !same || plain(&before.graph).nodes != plain(&after.graph).nodes {
                    return Err("the cut's header is computed in its source; edit the source".into());
                }
            }
        }
        Ok(written)
    }
}

pub fn diagnostics(diags: Vec<makepad_story_cut::Diagnostic>) -> String {
    let mut text = diags.iter().take(3).map(ToString::to_string).collect::<Vec<_>>().join("; ");
    if diags.len() > 3 {
        text.push_str(&format!(" (+{} more)", diags.len() - 3));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_flow::NodeInputValue;

    pub(crate) const EMPTY: &str = "use mod.flow.*\nuse mod.cut.*\n\n// The picture: every video block, bottom first.\nlet main = Stack{layers: []}\nlet out = CutOutput{frame: main.out(@frame) audio: []}\n\nFlow{\n    kind: @cut\n    label: \"Test\"\n    output: {width: 1080 height: 1920 fps: 60}\n    duration: 20.0\n    bpm: 120\n    main, out\n}\n";
    const A: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
    const R: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

    fn src(id: &str, rev: &str) -> Value {
        Value {
            text: format!("asset(\"{id}\", \"{rev}\")"),
            literal: Literal::Obj(vec![("ref".into(), Literal::Id("asset".into())), ("id".into(), Literal::Str(id.into())), ("rev".into(), Literal::Str(rev.into()))]),
        }
    }

    fn clip(id: &str, start: f64) -> NewNode {
        NewNode {
            id: id.into(),
            type_name: "Clip".into(),
            at: (start * 40.0, 0.0),
            params: vec![("src".into(), src(A, R)), ("span".into(), Value::span(start, 4.0)), ("track".into(), Value::num(0.0))],
            wires: vec![],
        }
    }

    #[test]
    fn the_empty_template_loads() {
        let doc = CutDoc::load(EMPTY).unwrap();
        assert_eq!(doc.output.height, 1920);
        assert_eq!(doc.nodes.len(), 2);
    }

    #[test]
    fn adds_a_clip_and_stacks_it_without_touching_the_rest() {
        let patcher = Patcher::new().unwrap();
        let added = patcher.add_nodes(EMPTY, &[clip("clip1", 1.0)]).unwrap();
        let doc = CutDoc::load(&added).unwrap_or_else(|diags| panic!("{}\n{added}", diagnostics(diags)));
        assert!(added.contains("let clip1 = Clip{"), "{added}");
        assert!(added.contains(&format!("asset(\"{A}\", \"{R}\")")), "{added}");
        assert!(added.contains("    output: {width: 1080 height: 1920 fps: 60}\n"), "{added}");
        assert!(added.contains("// The picture: every video block, bottom first."));
        assert_eq!(doc.node("clip1").unwrap().span.unwrap().start, 1.0);
        let stacked = patcher.rewire(&added, |graph| graph_edit::connect(graph, "clip1", "frame", "main", "layers")).unwrap();
        let doc = CutDoc::load(&stacked).unwrap_or_else(|diags| panic!("{}\n{stacked}", diagnostics(diags)));
        let main = doc.graph.nodes.iter().find(|node| node.id == "main").unwrap();
        assert!(matches!(&main.inputs[0].value, NodeInputValue::Edges(entries) if entries.len() == 1));
        assert!(stacked.starts_with(&added[..added.find("let main").unwrap()]));
    }

    #[test]
    fn param_writes_change_only_their_value_and_refuse_computed_ones() {
        let patcher = Patcher::new().unwrap();
        let added = patcher.add_nodes(EMPTY, &[clip("clip1", 1.0)]).unwrap();
        let moved = patcher.set_params(&added, Some("clip1"), &[("span".into(), Value::span(2.5, 4.0)), ("track".into(), Value::num(1.0))]).unwrap();
        let doc = CutDoc::load(&moved).unwrap();
        let node = doc.node("clip1").unwrap();
        assert_eq!((node.span.unwrap().start, node.track), (2.5, 1));
        let longer = patcher.set_params(&moved, None, &[("duration".into(), Value::num(30.0))]).unwrap();
        assert_eq!(CutDoc::load(&longer).unwrap().duration, 30.0);
        assert!(longer.contains("duration: 30.0"));
        let computed = moved.replace("let main = Stack", "let t0 = 2.0\nlet main = Stack").replace("span: [2.5, 4.0]", "span: [t0, 4.0]");
        assert!(CutDoc::load(&computed).is_ok());
        let unrelated = patcher.set_params(&computed, Some("clip1"), &[("track".into(), Value::num(2.0))]);
        assert!(unrelated.is_ok(), "an unrelated literal is still writable: {unrelated:?}");
        assert!(patcher.set_params(&computed, Some("clip1"), &[("span".into(), Value::span(1.0, 4.0))]).is_err());
    }

    #[test]
    fn removes_after_unstacking() {
        let patcher = Patcher::new().unwrap();
        let added = patcher.add_nodes(EMPTY, &[clip("clip1", 1.0)]).unwrap();
        let stacked = patcher.rewire(&added, |graph| graph_edit::connect(graph, "clip1", "frame", "main", "layers")).unwrap();
        assert!(patcher.remove_nodes(&stacked, &["clip1".into()]).is_err());
        let unstacked = patcher.rewire(&stacked, |graph| Ok(graph_edit::disconnect(graph, "main", "layers", Some(1)))).unwrap();
        let removed = patcher.remove_nodes(&unstacked, &["clip1".into()]).unwrap();
        let doc = CutDoc::load(&removed).unwrap();
        assert!(doc.node("clip1").is_none());
        assert!(!removed.contains("clip1"));
    }

    #[test]
    fn fresh_ids_avoid_every_word() {
        let patcher = Patcher::new().unwrap();
        assert_eq!(patcher.fresh_id(EMPTY, "clip"), "clip1");
        let added = patcher.add_nodes(EMPTY, &[clip("clip1", 1.0)]).unwrap();
        assert_eq!(patcher.fresh_id(&added, "clip"), "clip2");
    }
}

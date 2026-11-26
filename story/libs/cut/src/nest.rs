//! Nested edits: `Clip{src: cut(id, rev)}` plays another
//! edit inside this one. [`CutTree::load`] loads a cut with every cut it
//! nests (through a resolver that returns a cut revision's source),
//! refusing cycles and nesting deeper than [`MAX_NEST_DEPTH`];
//! [`CutTree::eval`] builds the `CutEval` that recurses into them.

use crate::eval::CutEval;
use crate::model::*;
use makepad_flow::Loc;
use std::collections::HashMap;

/// Edits nest at most this deep below the cut being loaded.
pub const MAX_NEST_DEPTH: usize = 4;

/// A cut and every cut revision it nests, at any depth, each loaded once.
#[derive(Clone, Debug)]
pub struct CutTree {
    pub root: CutDoc,
    pub edits: HashMap<AssetRef, CutDoc>,
}

impl CutTree {
    /// Load `source` (the cut asset `own`, when it has one, so nesting
    /// itself is caught) and, through `resolve`, every cut it nests.
    /// Problems in nested cuts are reported at the compound clip of this
    /// edit that leads to them.
    pub fn load(source: &str, own: Option<&str>, resolve: &mut dyn FnMut(&AssetRef) -> Result<String, String>) -> Result<CutTree, Vec<Diagnostic>> {
        Self::from_doc(CutDoc::load(source)?, own, resolve)
    }

    /// The tree of an edit that is already loaded (a slide of a
    /// show is evaluated with its file and has no source of its
    /// own to evaluate again): the same checks and nested edits as `load`.
    pub fn from_doc(root: CutDoc, own: Option<&str>, resolve: &mut dyn FnMut(&AssetRef) -> Result<String, String>) -> Result<CutTree, Vec<Diagnostic>> {
        let source = root.source.clone();
        let source = source.as_str();
        let mut tree = CutTree { root, edits: HashMap::new() };
        let mut diags = Vec::new();
        let path = own.map(|own| vec![own.to_string()]).unwrap_or_default();
        let clips = tree.root.nodes.iter().filter_map(|node| node.kind.nested_edit().map(|edit| (node.id.clone(), node.loc.clone(), edit.clone()))).collect::<Vec<_>>();
        for (node, loc, edit) in clips {
            let at = crate::load::field_loc(source, loc.clone(), "src").unwrap_or(loc);
            let mut walk = Walk { edits: &mut tree.edits, resolve, diags: &mut diags, at, node };
            walk.visit(&edit, path.clone(), 1);
        }
        if diags.is_empty() {
            Ok(tree)
        } else {
            diags.sort_by_key(|diag| (diag.line, diag.col));
            diags.dedup();
            Err(diags)
        }
    }

    /// The root's evaluator with every nested cut's evaluator attached.
    pub fn eval(&self) -> CutEval<'_> {
        self.eval_of(&self.root)
    }

    fn eval_of<'a>(&'a self, doc: &'a CutDoc) -> CutEval<'a> {
        let mut eval = CutEval::new(doc);
        for (_, edit) in doc.nested_edits() {
            if let Some(child) = self.edits.get(&edit) {
                eval.set_nested(edit, self.eval_of(child));
            }
        }
        eval
    }
}

struct Walk<'w> {
    edits: &'w mut HashMap<AssetRef, CutDoc>,
    resolve: &'w mut dyn FnMut(&AssetRef) -> Result<String, String>,
    diags: &'w mut Vec<Diagnostic>,
    /// The root's compound clip every problem is reported at.
    at: Loc,
    node: String,
}

impl Walk<'_> {
    /// Load `cut`, reached through the cut ids on `path` at nesting
    /// `depth` (1 for the root's own clips), and walk what it nests. Every
    /// path is walked (depth is bounded), so a shared edit is checked for
    /// cycles and depth on each way it is reached.
    fn visit(&mut self, edit: &AssetRef, mut path: Vec<String>, depth: usize) {
        let route = |path: &[String], last: &str| path.iter().map(String::as_str).chain([last]).collect::<Vec<_>>().join(" → ");
        if path.contains(&edit.id) {
            let message = format!("`{}` nests a cut that contains it: {}", self.node, route(&path, &edit.id));
            self.push(DiagnosticKind::Cycle, message);
            return;
        }
        if depth > MAX_NEST_DEPTH {
            let message = format!("`{}` nests edits {depth} deep; at most {MAX_NEST_DEPTH}: {}", self.node, route(&path, &edit.id));
            self.push(DiagnosticKind::NestDepth, message);
            return;
        }
        if !self.edits.contains_key(edit) {
            let source = match (self.resolve)(edit) {
                Ok(source) => source,
                Err(error) => {
                    let message = format!("`{}` nests edit {}:{}, which cannot be read: {error}", self.node, edit.id, edit.rev);
                    self.push(DiagnosticKind::MissingRef, message);
                    return;
                }
            };
            match CutDoc::load(&source) {
                Ok(doc) => {
                    self.edits.insert(edit.clone(), doc);
                }
                Err(errors) => {
                    let first = errors.first().map_or(String::new(), |error| error.to_string());
                    let message = format!("`{}` nests edit {}:{}, which does not load: {first}", self.node, edit.id, edit.rev);
                    self.push(DiagnosticKind::Eval, message);
                    return;
                }
            }
        }
        let children = self.edits[edit].nested_edits();
        path.push(edit.id.clone());
        for (_, child) in children {
            self.visit(&child, path.clone(), depth + 1);
        }
    }

    fn push(&mut self, kind: DiagnosticKind, message: String) {
        self.diags.push(Diagnostic { kind, line: self.at.line, col: self.at.col, node: Some(self.node.clone()), message });
    }
}

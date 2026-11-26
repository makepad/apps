//! A Motion document read as its parts: the `let` bindings in order and the
//! top-level values, over the framework's syntax tree
//! (`makepad_script::syntax`). Nothing is evaluated.

use makepad_script::syntax::{Kind, Node, Syntax};

/// The vocabularies a document names a type through (`post.Glow`).
const QUALIFIERS: &[&str] = &["mod.motion.", "mod.scene.", "mod.post.", "mod.kernel.", "mod.forms.", "motion.", "scene.", "post.", "kernel.", "forms."];

/// `Glow` for `post.Glow` / `mod.post.Glow`: the type a qualified name is.
pub fn unqualified(path: &str) -> &str {
    QUALIFIERS.iter().find_map(|q| path.strip_prefix(q)).unwrap_or(path)
}

/// The type an object value is constructed as (`Rect`, `Glow` for `post.Glow{..}`).
pub fn type_name(node: &Node) -> Option<&str> {
    match &node.kind {
        Kind::Object { ty: Some(ty), .. } => Some(unqualified(ty)),
        _ => None,
    }
}

/// A document: `let` bindings in order, then the final value.
#[derive(Clone, Debug, Default)]
pub struct Document<'a> {
    pub lets: Vec<(&'a str, &'a Node)>,
    /// Top-level values that are not `let`s, in order (the last is the
    /// document's value).
    pub values: Vec<&'a Node>,
}

impl<'a> Document<'a> {
    /// `use` lines and statements the VM runs at the top level (`for`, `if`,
    /// functions) are the VM's and not listed; `mod.x.Y = value` lists its value.
    pub fn of(syntax: &'a Syntax) -> Self {
        let mut document = Document::default();
        for statement in syntax.statements() {
            match &statement.kind {
                Kind::Let { names, value: Some(value), .. } if names.len() == 1 => document.lets.push((names[0].as_str(), value)),
                Kind::Let { .. } | Kind::Use { .. } | Kind::If { .. } | Kind::For { .. } | Kind::While { .. } | Kind::Loop { .. } | Kind::Match { .. } | Kind::Fn { .. } | Kind::Jump { .. } => {}
                Kind::Assign { value, .. } => document.values.push(value),
                _ => document.values.push(statement),
            }
        }
        document
    }

    /// `name`'s last `let` value.
    pub fn let_value(&self, name: &str) -> Option<&'a Node> {
        self.lets.iter().rev().find(|(n, _)| *n == name).map(|(_, node)| *node)
    }

    /// The `Motion{...}`: the last top-level object with `layers`, or the
    /// last `Motion` constructor.
    pub fn motion(&self) -> Option<&'a Node> {
        self.values.iter().rev().copied().chain(self.lets.iter().rev().map(|(_, node)| *node)).find(|node| {
            node.object().is_some() && (type_name(node) == Some("Motion") || node.field("layers").is_some())
        })
    }
}

//! An opened graphic: its Motion document's source (the truth), the
//! outline read from it, the loaded model (from the checker's thread, for
//! times written in beats and for values at the playhead), its undo stack,
//! and the pure edits the three views make.
//!
//! Every edit here is a source patch (`makepad_motion::patch`): it
//! runs on the UI thread because it only reads the text (no Splash is
//! evaluated here; loading the model is the checker's job), in time linear
//! in the document's length.

use makepad_motion::plain::{EllipseProps, GroupProps, PathProps, RectProps, TextProps};
use makepad_motion::patch::{self, LayerOutline, Outline, PropForm};
use makepad_motion::schema::{self, Owner, Site};
use makepad_motion::{sample_track, Layer, LayerKind, MotionDoc, Prop};
use std::collections::HashSet;
use std::sync::Arc;

/// What is selected in the graphic: a layer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerSelection {
    pub layer: Option<String>,
}

/// A layer's role hue by type (design §4.9).
pub fn layer_hue(ty: &str) -> u32 {
    match ty {
        "Text" => 0xb98af0ff,
        // Vector art, one family: shape lime (green is audio, cyan voice).
        "Rect" | "Ellipse" | "Path" | "Svg" => 0xa3c95aff,
        "Image" | "Video" => 0x4f8feaff,
        "Group" => 0xc2ccd8ff,
        "Shader" => 0xd05fd0ff,
        "Scene3D" | "Widget" => 0x7c83f5ff,
        _ => 0x8e9aa7ff,
    }
}

/// `a` mixed toward `b` by `amount` of `a` (RGBA words).
pub fn mix(a: u32, b: u32, amount: f64) -> u32 {
    let channel = |shift: u32| {
        let (x, y) = (((a >> shift) & 255) as f64, ((b >> shift) & 255) as f64);
        ((x * amount + y * (1.0 - amount)).round() as u32) << shift
    };
    channel(24) | channel(16) | channel(8) | 0xff
}

/// The list a layer path is in (`layers`, `layers[2].layers`).
pub fn parent_path(path: &str) -> &str {
    path.rfind('[').map_or(path, |open| &path[..open])
}

/// The document and its modules at one moment (an undo step).
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub source: String,
    pub modules: Vec<(String, String)>,
}

/// What a history step did.
#[derive(Clone, Debug, PartialEq)]
pub enum StepKind {
    /// The document as it opened.
    Opened,
    /// A change (an edit, a gesture, the AI).
    Edit,
    /// Cmd-Z: back to the state before a change.
    Undo,
    /// Shift-Cmd-Z: a change again.
    Redo,
    /// The history's own "go to": the document as it was at that step.
    GoTo(usize),
}

/// One entry of the document's history: what was done, by whom, when, and
/// the document as it was after it. The history only grows: an undo, a
/// redo, a "go to" are entries too, and nothing a later edit makes is
/// dropped (every state stays readable and reachable).
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryStep {
    pub label: String,
    /// Who made it ("You · panel", "AI · …"); filled by the desk when the
    /// change arrives from it.
    pub source: String,
    /// Seconds since the epoch.
    pub time: f64,
    pub kind: StepKind,
    pub after: Snapshot,
    /// What it touched in the document's text after it (bytes), when it
    /// touched the document.
    pub range: Option<std::ops::Range<usize>>,
}

/// A library module the document imports, open for editing.
#[derive(Clone, Debug)]
pub struct ModuleDoc {
    /// `id@rev`: how the document's VM names it (`module:<key>`).
    pub key: String,
    pub id: String,
    pub rev: String,
    /// What the document calls it (`let open = import(…)`).
    pub name: String,
    pub source: String,
    /// The text of revision `rev`.
    pub saved: String,
    pub code: schema::code::CodeOutline,
}

/// The id a built-in module (shipped with Stage, read only) is open under.
pub const BUILTIN_ID: &str = "builtin";

impl ModuleDoc {
    /// Shipped with Stage: read as code, not edited.
    pub fn builtin(&self) -> bool {
        self.id == BUILTIN_ID
    }

    fn new(key: String, id: String, rev: String, name: String, text: String) -> Self {
        let code = schema::code::outline(&text);
        Self { key, id, rev, name, saved: text.clone(), source: text, code }
    }

    fn set_source(&mut self, text: String) {
        self.code = schema::code::outline(&text);
        self.source = text;
    }
}

/// One edit of a Motion document.
#[derive(Clone, Debug, PartialEq)]
pub enum MotionOp {
    /// A property's plain value (Splash text).
    Value { layer: String, prop: String, value: String },
    /// A key at `t` on the layer's clock (added, or re-valued).
    Key { layer: String, prop: String, t: f64, value: String },
    /// Keys removed: `(layer, prop, index)`.
    DeleteKeys(Vec<(String, String, usize)>),
    /// A span on the parent's clock; `keep_keys` for a trim.
    Span { layer: String, start: f64, length: f64, keep_keys: bool },
    /// A layer field set or removed (`name`, `visible`, `locked`, …).
    Field { layer: String, name: String, value: Option<String> },
    Remove { layer: String },
    Duplicate { layer: String },
    /// An input's default.
    Input { name: String, value: String },
    /// One value's bytes replaced (a site: `makepad_motion::schema`), as
    /// the inline controls, data cells and record entries write; `what`
    /// names it for the history.
    Splice { range: std::ops::Range<usize>, text: String, what: String },
}

/// The next source after `op`.
pub fn apply(source: &str, op: &MotionOp) -> Result<String, String> {
    match op {
        MotionOp::Value { layer, prop, value } => patch::set_value(source, layer, prop, value),
        MotionOp::Key { layer, prop, t, value } => patch::set_key(source, layer, prop, *t, value, 1e-3),
        MotionOp::DeleteKeys(keys) => {
            let mut keys = keys.clone();
            // Later indices first, so earlier ones stay valid.
            keys.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)).then(b.2.cmp(&a.2)));
            let mut next = source.to_string();
            for (layer, prop, index) in keys {
                next = patch::delete_key(&next, &layer, &prop, index)?;
            }
            Ok(next)
        }
        MotionOp::Span { layer, start, length, keep_keys } => patch::set_span(source, layer, *start, *length, *keep_keys),
        MotionOp::Field { layer, name, value } => patch::set_field(source, layer, name, value.as_deref()),
        MotionOp::Remove { layer } => patch::remove_layer(source, layer),
        MotionOp::Duplicate { layer } => patch::duplicate_layer(source, layer, None),
        MotionOp::Input { name, value } => patch::set_input(source, name, value),
        MotionOp::Splice { range, text, .. } => makepad_motion::schema::splice(source, range.clone(), text),
    }
}

/// What the op is, for the undo history and the Source view.
pub fn describe(op: &MotionOp) -> String {
    match op {
        MotionOp::Value { prop, .. } => format!("set {prop}"),
        MotionOp::Key { prop, .. } => format!("key {prop}"),
        MotionOp::DeleteKeys(keys) => format!("delete {} key{}", keys.len(), if keys.len() == 1 { "" } else { "s" }),
        MotionOp::Span { keep_keys: true, .. } => "trim layer".into(),
        MotionOp::Span { .. } => "move layer".into(),
        MotionOp::Field { name, .. } => format!("set {name}"),
        MotionOp::Remove { .. } => "delete layer".into(),
        MotionOp::Duplicate { .. } => "duplicate layer".into(),
        MotionOp::Input { name, .. } => format!("set input {name}"),
        MotionOp::Splice { what, .. } => format!("set {what}"),
    }
}

/// An opened graphic.
pub struct MotionSession {
    /// The Graphic node of the open edit that shows it, when opened from one.
    pub node: Option<String>,
    /// The library revision it was opened from (or last autosaved as).
    pub asset: Option<makepad_asset_data::AssetRevisionRef>,
    pub tree_path: Option<String>,
    pub project: Option<makepad_asset_data::AssetId>,
    pub title: String,
    pub source: String,
    pub outline: Outline,
    /// Why the source does not outline (it is shown as it is; the views
    /// that need the outline say so).
    pub outline_error: Option<String>,
    /// Every value the source writes (`schema::sites`), for the editors.
    pub sites: Vec<Site>,
    /// The model of the source revision `doc_of`, from the checker.
    pub doc: Option<Arc<MotionDoc>>,
    pub doc_of: u64,
    /// The model's errors for the current revision (it did not load) and
    /// warnings.
    pub problems: Vec<super::checker::Problem>,
    pub warnings: Vec<super::checker::Problem>,
    pub undo: Vec<(Snapshot, String)>,
    /// The live gesture whose steps join one undo step.
    pub burst: Option<String>,
    pub redo: Vec<(Snapshot, String)>,
    /// Every state the document has been in, in order (append-only).
    pub history: Vec<HistoryStep>,
    /// The library modules the document imports (`import(asset(id, rev))`),
    /// editable like the document: a change is published as the module's
    /// next revision and the import re-pointed at it.
    pub modules: Vec<ModuleDoc>,
    /// The document read as code (every literal, its lets, functions,
    /// objects and draw calls).
    pub code: schema::code::CodeOutline,
    /// Bumped on every change of `source`.
    pub revision: u64,
    /// The source as last published.
    pub saved: String,
    pub open: HashSet<String>,
    /// Layers hidden or locked in this editor only, while the document
    /// model has no `visible` / `locked` fields (see [`flags_in_source`]).
    pub hidden: HashSet<String>,
    pub locked: HashSet<String>,
    pub selection: LayerSelection,
    pub playhead: f64,
    /// Playing: since this real time, from this document time.
    pub playing: Option<(f64, f64)>,
    /// Whether a layer's position is its own, per revision (see
    /// [`Self::placement`]).
    placed: std::cell::RefCell<std::collections::HashMap<(u64, String), Option<Placement>>>,
}

/// What places a layer whose position is not its own: the layer that lays
/// it out (a Repeat, a sequencer, a chart, a stack) when there is one, and
/// the hint the editor shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub owner: Option<String>,
    pub hint: String,
}

impl MotionSession {
    pub fn new(source: String, title: String, node: Option<String>, asset: Option<makepad_asset_data::AssetRevisionRef>) -> Self {
        let mut session = Self {
            node,
            asset,
            tree_path: None,
            project: None,
            title,
            saved: source.clone(),
            source: String::new(),
            outline: Outline::default(),
            outline_error: None,
            sites: Vec::new(),
            doc: None,
            doc_of: 0,
            problems: Vec::new(),
            warnings: Vec::new(),
            undo: Vec::new(),
            burst: None,
            redo: Vec::new(),
            history: Vec::new(),
            modules: Vec::new(),
            code: Default::default(),
            revision: 0,
            open: HashSet::new(),
            hidden: HashSet::new(),
            locked: HashSet::new(),
            selection: LayerSelection::default(),
            playhead: 0.0,
            playing: None,
            placed: Default::default(),
        };
        session.set_source(source);
        let after = session.snapshot();
        session.history.push(HistoryStep { label: "Opened".into(), source: String::new(), time: now(), kind: StepKind::Opened, after, range: None });
        session
    }

    /// A new history entry for the state the document is in now.
    fn record(&mut self, label: String, kind: StepKind, before: Option<&str>) {
        let after = self.snapshot();
        let range = before.and_then(|b| changed_range(b, &after.source));
        let label = match (&kind, before) {
            (StepKind::Edit, Some(b)) => describe_change(b, &self.source, &self.code).unwrap_or(label),
            _ => label,
        };
        self.history.push(HistoryStep { label, source: String::new(), time: now(), kind, after, range });
    }

    /// The last entry follows the document (a gesture's later steps),
    /// labelled against the state before the gesture.
    fn record_more(&mut self, before: &str) {
        let after = self.snapshot();
        let n = self.history.len();
        if n == 0 {
            return;
        }
        let first = if n >= 2 { self.history[n - 2].after.source.clone() } else { before.to_string() };
        let label = describe_change(&first, &self.source, &self.code);
        let range = changed_range(&first, &after.source);
        let last = &mut self.history[n - 1];
        if let Some(l) = label {
            last.label = l;
        }
        last.range = range;
        last.after = after;
    }

    /// The document as it was at history entry `n`, as a new entry (the
    /// way back stays: Cmd-Z returns to where it was).
    pub fn go_to(&mut self, n: usize) -> bool {
        let Some(step) = self.history.get(n).cloned() else { return false };
        self.burst = None;
        if step.after == self.snapshot() {
            return false;
        }
        self.undo.push((self.snapshot(), format!("go to {}", step.label)));
        self.redo.clear();
        self.restore(step.after);
        self.record(format!("Back to: {}", step.label), StepKind::GoTo(n), None);
        true
    }

    /// The entry whose state the document is in now (the latest that
    /// matches it).
    pub fn current_step(&self) -> Option<usize> {
        let now = self.snapshot();
        (0..self.history.len()).rev().find(|&i| self.history[i].after == now)
    }

    pub fn dirty(&self) -> bool {
        self.source != self.saved || self.modules.iter().any(|m| m.source != m.saved)
    }

    fn set_source(&mut self, source: String) {
        match patch::outline(&source) {
            Ok(outline) => {
                self.outline = outline;
                self.outline_error = None;
            }
            Err(error) => self.outline_error = Some(error),
        }
        if let Ok(sites) = schema::sites(&source) {
            self.sites = sites;
        }
        self.code = schema::code::outline(&source);
        self.source = source;
        self.revision += 1;
        // A selection of something that is gone is dropped.
        let paths: HashSet<String> = self.outline.all().iter().map(|layer| layer.path.clone()).collect();
        if self.selection.layer.as_ref().is_some_and(|layer| !paths.contains(layer)) {
            self.selection = LayerSelection::default();
        }
    }

    /// The document and its modules as they are (an undo step).
    fn snapshot(&self) -> Snapshot {
        Snapshot { source: self.source.clone(), modules: self.modules.iter().map(|m| (m.key.clone(), m.source.clone())).collect() }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        for (key, text) in snapshot.modules {
            if let Some(module) = self.modules.iter_mut().find(|m| m.key == key) {
                if module.source != text {
                    module.set_source(text);
                }
            }
        }
        if snapshot.source != self.source {
            self.set_source(snapshot.source);
        } else {
            self.revision += 1;
        }
    }

    /// Replace the source (an edit, a typed draft, the AI) as one undo step.
    pub fn replace(&mut self, source: String, what: &str) -> bool {
        self.burst = None;
        if source == self.source {
            return false;
        }
        self.undo.push((self.snapshot(), what.to_string()));
        self.redo.clear();
        let before = std::mem::take(&mut self.source);
        self.source = before.clone();
        self.set_source(source);
        self.record(what.to_string(), StepKind::Edit, Some(&before));
        true
    }

    /// A module's text replaced (`key`: its `id@rev`), as one undo step, or
    /// a step of the live gesture `burst`.
    pub fn replace_module(&mut self, key: &str, text: String, what: &str, burst: Option<&str>) -> bool {
        let Some(index) = self.modules.iter().position(|m| m.key == key) else { return false };
        if self.modules[index].builtin() {
            return false;
        }
        if self.modules[index].source == text {
            return false;
        }
        let first = burst.is_none() || self.burst.as_deref() != burst;
        if first {
            self.undo.push((self.snapshot(), what.to_string()));
            self.burst = burst.map(str::to_string);
        }
        self.redo.clear();
        let name = self.modules[index].name.clone();
        self.modules[index].set_source(text);
        // The model loads again with the module as it is now.
        self.revision += 1;
        if first {
            self.record(format!("{name}: {what}"), StepKind::Edit, None);
        } else if let Some(last) = self.history.last_mut() {
            last.after = Snapshot { source: self.source.clone(), modules: self.modules.iter().map(|m| (m.key.clone(), m.source.clone())).collect() };
        }
        true
    }

    /// Imports resolved by the checker against the same snapshot as the
    /// loaded document. Discovery and library reads stay off the UI thread.
    pub fn loaded_modules(&mut self, loaded: &[makepad_motion::modules::LoadedModule]) {
        use makepad_motion::modules::ModuleRef;
        let mut found = Vec::new();
        for module in loaded {
            let (key, id) = match &module.reference {
                ModuleRef::Named(name) => (name.clone(), BUILTIN_ID.to_string()),
                ModuleRef::Asset { id, .. } => (module.reference.key(), id.clone()),
                ModuleRef::Path { .. } => (module.source.name.clone(), module.source.name.clone()),
            };
            if found.iter().any(|m: &ModuleDoc| m.key == key) { continue; }
            let open = self.modules.iter().find(|m| m.key == key);
            found.push(open.cloned().unwrap_or_else(|| ModuleDoc::new(key, id, module.source.version.clone(), module.source.name.clone(), module.source.text.to_string())));
        }
        // Removing an import must not discard an unsaved module edit.
        for module in &self.modules {
            if module.source != module.saved && !found.iter().any(|m| m.key == module.key) { found.push(module.clone()); }
        }
        self.modules = found;
    }

    /// Module `key` was published as revision `rev` (its text then:
    /// `published`): the imports of it, in the document and in the other
    /// modules, follow the new revision.
    pub fn module_published(&mut self, key: &str, published: &str, rev: &str) {
        let Some(index) = self.modules.iter().position(|m| m.key == key) else { return };
        let (id, old) = (self.modules[index].id.clone(), self.modules[index].rev.clone());
        let repoint = |text: &str| -> String {
            text.lines().map(|line| if line.contains(&id) { line.replace(&format!("\"{old}\""), &format!("\"{rev}\"")) } else { line.to_string() })
                .collect::<Vec<_>>().join("\n") + if text.ends_with('\n') { "\n" } else { "" }
        };
        {
            let module = &mut self.modules[index];
            module.saved = published.to_string();
            module.rev = rev.to_string();
            module.key = format!("{id}@{rev}");
        }
        for i in 0..self.modules.len() {
            if i != index {
                let text = repoint(&self.modules[i].source);
                if text != self.modules[i].source {
                    self.modules[i].set_source(text);
                }
            }
        }
        let next = repoint(&self.source);
        if next != self.source {
            self.set_source(next);
        }
    }

    /// The modules changed since they were opened: `(id, rev, text)`, what
    /// the model loads in their place.
    pub fn edited_modules(&self) -> Vec<(String, String, String)> {
        self.modules.iter().filter(|m| m.source != m.saved).map(|m| (m.id.clone(), m.rev.clone(), m.source.clone())).collect()
    }

    /// A step of a live gesture (typing, a drag) under `key`: the first
    /// step is an undo step, the rest of the burst joins it, so the whole
    /// gesture undoes at once. [`Self::end_burst`] closes it.
    pub fn replace_burst(&mut self, source: String, what: &str, key: &str) -> bool {
        if self.burst.as_deref() != Some(key) {
            let changed = self.replace(source, what);
            self.burst = changed.then(|| key.to_string());
            return changed;
        }
        if source == self.source {
            return false;
        }
        self.redo.clear();
        let before = self.source.clone();
        self.set_source(source);
        self.record_more(&before);
        true
    }

    /// Where a property's new value goes: a key at the playhead when it is
    /// keyed, the input's default when it reads an input (the binding
    /// stays), else the value itself.
    pub fn write_op(&self, layer: &str, prop: &str, value: String) -> MotionOp {
        let form = self.outline.layer(layer).and_then(|l| l.prop(prop)).map(|p| &p.form);
        match form {
            Some(PropForm::Keys { .. }) => {
                let t = self.playhead - self.clock_of(layer);
                MotionOp::Key { layer: layer.into(), prop: prop.into(), t: t.max(0.0), value }
            }
            Some(PropForm::Input(name)) => MotionOp::Input { name: name.clone(), value },
            _ => MotionOp::Value { layer: layer.into(), prop: prop.into(), value },
        }
    }

    /// The sites a layer's field is written with (its value, or each key's
    /// time, value and ease, or each entry of a record or list).
    pub fn field_sites(&self, layer: &str, field: &str) -> Vec<&Site> {
        self.sites.iter().filter(|site| match &site.owner {
            Owner::Field { layer: l, field: f, .. } | Owner::Key { layer: l, field: f, .. } => l == layer && (f == field || f.strip_prefix(field).is_some_and(|rest| rest.starts_with('.'))),
            _ => false,
        }).collect()
    }

    /// A layer field's one plain value, when it is written as one.
    pub fn value_site(&self, layer: &str, field: &str) -> Option<&Site> {
        self.sites.iter().find(|site| matches!(&site.owner, Owner::Field { layer: l, field: f, part } if l == layer && f == field && part.is_empty()))
    }

    /// An input's default.
    pub fn input_site(&self, name: &str) -> Option<&Site> {
        self.sites.iter().find(|site| matches!(&site.owner, Owner::Input { name: n } if n == name))
    }

    /// What a site's value really is: an `in(@x)` reads the input's default.
    pub fn resolved<'a>(&'a self, site: &'a Site) -> &'a Site {
        match &site.kind {
            schema::ValueKind::Input(name) => self.input_site(name).unwrap_or(site),
            _ => site,
        }
    }

    pub fn end_burst(&mut self) {
        self.burst = None;
    }

    /// `op` as a step of the live gesture `key`.
    pub fn apply_burst(&mut self, op: &MotionOp, key: &str) -> Result<bool, String> {
        let next = apply(&self.source, op)?;
        Ok(self.replace_burst(next, &describe(op), key))
    }

    pub fn apply(&mut self, op: &MotionOp) -> Result<bool, String> {
        let next = apply(&self.source, op)?;
        let changed = self.replace(next, &describe(op));
        if changed {
            // The editor's own per-layer state follows layers that moved.
            let index_of = |path: &str| path.rfind('[').and_then(|open| path[open + 1..].trim_end_matches(']').parse::<usize>().ok());
            match op {
                MotionOp::Remove { layer } => {
                    let list = parent_path(layer).to_string();
                    if let Some(gone) = index_of(layer) {
                        self.remap(&list, |i| if i == gone { None } else if i > gone { Some(i - 1) } else { Some(i) });
                    }
                }
                MotionOp::Duplicate { layer } => {
                    let list = parent_path(layer).to_string();
                    if let Some(at) = index_of(layer) {
                        self.remap(&list, |i| Some(if i > at { i + 1 } else { i }));
                    }
                }
                _ => {}
            }
        }
        Ok(changed)
    }

    /// Re-key the per-layer sets for items of `list` moved by `map`
    /// (index to new index; `None`: gone). Paths below them follow.
    fn remap(&mut self, list: &str, map: impl Fn(usize) -> Option<usize>) {
        let rekey = |path: &str| -> Option<String> {
            let rest = path.strip_prefix(list)?.strip_prefix('[')?;
            let close = rest.find(']')?;
            let index = rest[..close].parse::<usize>().ok()?;
            Some(match map(index) {
                Some(new) => format!("{list}[{new}]{}", &rest[close + 1..]),
                None => String::new(),
            })
        };
        for set in [&mut self.hidden, &mut self.locked, &mut self.open] {
            let old: Vec<String> = set.drain().collect();
            for path in old {
                match rekey(&path) {
                    Some(new) if new.is_empty() => {}
                    Some(new) => {
                        set.insert(new);
                    }
                    None => {
                        set.insert(path);
                    }
                }
            }
        }
        if let Some(layer) = self.selection.layer.clone() {
            self.selection.layer = match rekey(&layer) {
                Some(new) if new.is_empty() => None,
                Some(new) => Some(new),
                None => Some(layer),
            };
        }
    }

    pub fn undo(&mut self, redo: bool) -> Option<String> {
        self.burst = None;
        let now = self.snapshot();
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let (snapshot, what) = from.pop()?;
        to.push((now, what.clone()));
        self.restore(snapshot);
        // Recorded too: the history never loses a state.
        let (label, kind) = if redo { (format!("Redo: {what}"), StepKind::Redo) } else { (format!("Undo: {what}"), StepKind::Undo) };
        self.record(label, kind, None);
        Some(what)
    }

    pub fn duration(&self) -> f64 {
        self.doc.as_ref().map(|doc| doc.duration).or(self.outline.duration).unwrap_or(5.0).max(0.1)
    }

    pub fn fps(&self) -> f64 {
        self.doc.as_ref().map(|doc| doc.fps).or(self.outline.fps).unwrap_or(60.0).max(1.0)
    }

    /// The model layer at `path` (when the model is of this revision's
    /// layout).
    pub fn model_layer(&self, path: &str) -> Option<&Layer> {
        let doc = self.doc.as_ref()?;
        doc.all_layers().into_iter().find(|layer| layer.path == path)
    }

    /// Where a layer's own clock starts, on the document's clock.
    pub fn clock_of(&self, path: &str) -> f64 {
        let mut clock = 0.0;
        let mut prefix = String::new();
        for part in path.split('.') {
            if !prefix.is_empty() {
                prefix.push('.');
            }
            prefix.push_str(part);
            clock += self.span_of(&prefix).map_or(0.0, |(start, _)| start);
        }
        clock
    }

    /// A layer's span on its parent's clock, as the model resolved it (or
    /// as written).
    pub fn span_of(&self, path: &str) -> Option<(f64, f64)> {
        if let Some(layer) = self.model_layer(path) {
            return layer.span.as_ref().map(|span| (span.start.value, span.length.value));
        }
        self.outline.layer(path).and_then(|layer| layer.span)
    }

    pub fn label_of(&self, layer: &LayerOutline) -> String {
        if let Some(name) = &layer.name {
            return name.clone();
        }
        let text = match layer.prop("text").map(|prop| &prop.form) {
            Some(PropForm::Value(text)) => Some(text.clone()),
            Some(PropForm::Input(input)) => self.outline.inputs.iter().find(|(name, _)| name == input).map(|(_, value)| value.clone()),
            _ => None,
        };
        match text {
            Some(text) => {
                let text = text.trim_matches('"');
                let short: String = text.chars().take(28).collect();
                format!("{} “{}{}”", layer.ty, short, if text.chars().count() > 28 { "…" } else { "" })
            }
            // A readable kind and place, never the internal path.
            None => match layer.path.rsplit('.').next().unwrap_or("") {
                "layer" => format!("{} · each copy", layer.ty),
                "mask" => format!("{} · mask", layer.ty),
                last => match last.rsplit('[').next().and_then(|tail| tail.strip_suffix(']')).and_then(|index| index.parse::<usize>().ok()) {
                    Some(index) => format!("{} {}", layer.ty, index + 1),
                    None => layer.ty.clone(),
                },
            },
        }
    }

    /// The layout that places a layer, when one does (its position is then
    /// derived, not the layer's own): the nearest enclosing layer that is
    /// not a plain Group. `(owner path, owner label, owner type)`.
    pub fn placed_by(&self, path: &str) -> Option<(String, String, String)> {
        let mut at = path;
        while let Some(dot) = at.rfind('.') {
            at = &at[..dot];
            if let Some(owner) = self.outline.layer(at) {
                if owner.ty != "Group" {
                    return Some((owner.path.clone(), self.label_of(owner), owner.ty.clone()));
                }
            }
        }
        None
    }

    /// Whether something else places the layer (a layout it runs in), as
    /// the evaluator says: its position moved by a step does not move what
    /// it draws, or it draws as instances only. `None`: it moves itself.
    /// Asked of the model once per revision and layer.
    pub fn placement(&self, path: &str) -> Option<Placement> {
        let key = (self.revision, path.to_string());
        if let Some(known) = self.placed.borrow().get(&key) {
            return known.clone();
        }
        if self.doc.is_none() || self.doc_of != self.revision {
            // The model of this revision is still loading: ask again then.
            return None;
        }
        let placed = self.placed_by_model(path);
        let found = placed.then(|| {
            let owner = self.placed_by(path);
            let hint = match &owner {
                Some((_, label, ty)) => format!("positioned by {ty} ‘{label}’"),
                None => "positioned by its layout".to_string(),
            };
            Placement { owner: owner.map(|(path, _, _)| path), hint }
        });
        self.placed.borrow_mut().insert(key, found.clone());
        found
    }

    fn placed_by_model(&self, path: &str) -> bool {
        let Some(doc) = self.doc.as_ref().filter(|_| self.doc_of == self.revision) else { return false };
        let origin = |frame: &makepad_motion::MotionFrame| {
            frame.walk().into_iter().find(|(_, item)| item.path == path).map(|(_, item)| item.world.apply([0.0, 0.0]))
        };
        let frame = doc.eval(self.playhead);
        let Some(before) = origin(&frame) else {
            // Drawn only as instances (`path#k`): a layout repeats it.
            let prefix = format!("{path}#");
            return frame.walk().iter().any(|(_, item)| item.path.starts_with(&prefix));
        };
        let at = self.value_now(path, "at").unwrap_or_else(|| vec![0.0, 0.0]);
        const STEP: f64 = 37.0;
        let op = self.write_op(path, "at", patch::fmt_values(&[at.first().copied().unwrap_or(0.0) + STEP, at.get(1).copied().unwrap_or(0.0)], None));
        // Loaded as the shown document was (its folder's imports, its library).
        let Some(moved) = apply(&self.source, &op).ok().and_then(|source| MotionDoc::load_with(&source, &[], (*doc.context).clone()).ok()) else { return false };
        match origin(&moved.eval(self.playhead)) {
            Some(after) => (after[0] - before[0]).hypot(after[1] - before[1]) < 0.5,
            None => false,
        }
    }

    /// A property's value at the playhead, as numbers, when the model has
    /// it.
    pub fn value_now(&self, layer: &str, prop: &str) -> Option<Vec<f64>> {
        let model = self.model_layer(layer)?;
        value_at(model, prop, self.playhead - self.clock_of(layer))
    }
}

/// `frame` without the layers hidden in the editor (and their children).
pub fn without_hidden(frame: &mut makepad_motion::MotionFrame, hidden: &HashSet<String>) {
    fn strip(items: &mut Vec<makepad_motion::Item>, hidden: &HashSet<String>) {
        items.retain(|item| !hidden.contains(&item.path));
        for item in items {
            if let makepad_motion::ItemKind::Group { children, .. } = &mut item.kind {
                strip(children, hidden);
            }
        }
    }
    if !hidden.is_empty() {
        strip(&mut frame.items, hidden);
    }
}

/// A property of the model's layer, by name.
enum PropRef<'a> {
    One(&'a Prop<1>),
    Two(&'a Prop<2>),
    Four(&'a Prop<4>),
}

fn prop_ref<'a>(layer: &'a Layer, name: &str) -> Option<PropRef<'a>> {
    let transform = &layer.transform;
    Some(match name {
        "at" => PropRef::Two(&transform.at),
        "x" => PropRef::One(&transform.x),
        "y" => PropRef::One(&transform.y),
        "scale" => PropRef::Two(&transform.scale),
        "rotate" => PropRef::One(&transform.rotate),
        "opacity" => PropRef::One(&layer.opacity),
        _ => match (&layer.kind, name) {
            (LayerKind::Rect(RectProps { size, .. }) | LayerKind::Ellipse(EllipseProps { size, .. }), "size") => PropRef::Two(size),
            (LayerKind::Rect(RectProps { radius, .. }), "radius") => PropRef::One(radius),
            (LayerKind::Rect(RectProps { fill: Some(fill), .. }) | LayerKind::Ellipse(EllipseProps { fill: Some(fill), .. }) | LayerKind::Path(PathProps { fill: Some(fill), .. }), "fill") => PropRef::Four(fill),
            (LayerKind::Rect(RectProps { stroke: Some(stroke), .. }) | LayerKind::Ellipse(EllipseProps { stroke: Some(stroke), .. }) | LayerKind::Path(PathProps { stroke: Some(stroke), .. }), "stroke") => PropRef::Four(stroke),
            (LayerKind::Rect(RectProps { width, .. }) | LayerKind::Ellipse(EllipseProps { width, .. }) | LayerKind::Path(PathProps { width, .. }), "width") => PropRef::One(width),
            (LayerKind::Path(PathProps { trim_start, .. }), "trim_start") => PropRef::One(trim_start),
            (LayerKind::Path(PathProps { trim_end, .. }), "trim_end") => PropRef::One(trim_end),
            (LayerKind::Text { props: TextProps { size, .. }, .. }, "size") => PropRef::One(size),
            (LayerKind::Text { props: TextProps { color, .. }, .. }, "color") => PropRef::Four(color),
            (LayerKind::Svg { size: Some(size), .. } | LayerKind::Image { size: Some(size), .. } | LayerKind::Video { size: Some(size), .. } | LayerKind::Group { props: GroupProps { size: Some(size), .. }, .. } | LayerKind::Shader { size: Some(size), .. }, "size") => PropRef::Two(size),
            (LayerKind::Shader { params, .. }, name) => PropRef::One(&params.iter().find(|(param, _)| param == name)?.1),
            _ => return None,
        },
    })
}

/// A property's value at `t` on its layer's clock.
pub fn value_at(layer: &Layer, name: &str, t: f64) -> Option<Vec<f64>> {
    fn sample<const N: usize>(prop: &Prop<N>, t: f64) -> Option<Vec<f64>> {
        match prop {
            Prop::Const(value) => Some(value.to_vec()),
            Prop::Track(track) => Some(sample_track(track, t).to_vec()),
            Prop::Fn(_) | Prop::Blend(_) => None,
        }
    }
    match prop_ref(layer, name)? {
        PropRef::One(prop) => sample(prop, t),
        PropRef::Two(prop) => sample(prop, t),
        PropRef::Four(prop) => sample(prop, t),
    }
}

/// Whether a property holds a colour.
pub fn is_color(name: &str) -> bool {
    matches!(name, "fill" | "stroke" | "color" | "highlight_color" | "background")
}

/// A value for a header or a field: numbers, or a colour as hex.
pub fn show(name: &str, value: &[f64]) -> String {
    if is_color(name) && value.len() == 4 {
        return patch::fmt_color([value[0], value[1], value[2], value[3]]);
    }
    if name == "scale" && value.len() == 2 {
        return format!("{:.0}×{:.0} %", value[0] * 100.0, value[1] * 100.0);
    }
    let one = |v: f64| {
        if name == "opacity" || name.starts_with("trim") {
            format!("{:.0}%", v * 100.0)
        } else if name == "rotate" {
            format!("{}°", patch::fmt_num((v * 10.0).round() / 10.0))
        } else {
            patch::fmt_num((v * 10.0).round() / 10.0)
        }
    };
    value.iter().map(|v| one(*v)).collect::<Vec<_>>().join(", ")
}

/// Seconds since the epoch, now.
fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The bytes of `after` that differ from `before` (between their common
/// start and end).
pub fn changed_range(before: &str, after: &str) -> Option<std::ops::Range<usize>> {
    if before == after {
        return None;
    }
    let (a, b) = (before.as_bytes(), after.as_bytes());
    let mut start = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let mut end_a = a.len();
    let mut end_b = b.len();
    while end_a > start && end_b > start && a[end_a - 1] == b[end_b - 1] {
        end_a -= 1;
        end_b -= 1;
    }
    while start > 0 && !after.is_char_boundary(start) {
        start -= 1;
    }
    while end_b < b.len() && !after.is_char_boundary(end_b) {
        end_b += 1;
    }
    Some(start..end_b)
}

/// A change said in the document's own names: "LINE_LOOK.prim.heat 0.8 →
/// 1.2", "BLOOM_STRENGTH 0.72 → 0.9", "bypassed Bloom", "added Glow";
/// `None` when it is not one value (a typed passage, the AI's rewrite).
pub fn describe_change(before: &str, after: &str, code: &schema::code::CodeOutline) -> Option<String> {
    let mut r = changed_range(before, after)?;
    let d = before.len() as isize - after.len() as isize;
    // Out to whole tokens (a number, a colour, a name): "120 → 130", not "2 → 3".
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b'#' || c == b'_' || c == b'-';
    let ab = after.as_bytes();
    while r.start > 0 && word(ab[r.start - 1]) {
        r.start -= 1;
    }
    while r.end < ab.len() && word(ab[r.end]) {
        r.end += 1;
    }
    let old = before.get(r.start..(r.end as isize + d) as usize)?.trim();
    let new = after.get(r.clone())?.trim();
    if old.len() > 48 || new.len() > 48 || old.contains('\n') || new.contains('\n') {
        return None;
    }
    if new.starts_with("on: false") {
        let ty: String = after[r.end..].trim_start().chars().take(0).collect();
        let _ = ty;
        let owner = after[..r.start].rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '{')).next().unwrap_or("").trim_end_matches('{').to_string();
        return Some(format!("bypassed {owner}").trim().to_string());
    }
    if old.starts_with("on: false") {
        let owner = after[..r.start].rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '{')).next().unwrap_or("").trim_end_matches('{').to_string();
        return Some(format!("{owner} on again").trim().to_string());
    }
    // Its name: the literal's label (a field, a call's argument) inside the
    // `let` (or the layer) it is written in.
    let lit = code.literal_at(r.start).map(|l| &code.literals[l]);
    let owner = code.regions.iter().filter(|g| g.kind == schema::code::RegionKind::Let && g.depth == 0 && g.range.start <= r.start && r.start < g.range.end).map(|g| g.name.clone()).next();
    let field = lit.map(|l| l.label.clone()).filter(|l| !l.is_empty());
    let name = match (owner, field) {
        (Some(o), Some(f)) if f != o => format!("{o}.{f}"),
        (Some(o), _) => o,
        (None, Some(f)) => f,
        (None, None) => return None,
    };
    let short = |t: &str| -> String { if t.chars().count() > 20 { format!("{}…", t.chars().take(19).collect::<String>()) } else { t.to_string() } };
    Some(match (old.is_empty(), new.is_empty()) {
        (true, false) => format!("{name}: added {}", short(new)),
        (false, true) => format!("{name}: removed {}", short(old)),
        _ => format!("{name} {} → {}", short(old), short(new)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"use mod.motion.*; Motion{
    size: vec2(1920, 1080) duration: 6.0
    inputs: {headline: "Notes, instantly"}
    layers: [
        Rect{name: "bg" at: vec2(0, 0) size: vec2(1920, 1080) fill: #0b0d12}
        Text{text: in(@headline) size: 120 at: vec2(160, 460) span: [1.0, 4.0]
             opacity: tween(0, 1, 0.0, 0.4, @ease_out)}
    ]
}
"#;

    #[test]
    fn its_code_and_clocks_are_read() {
        let session = MotionSession::new(DOC.into(), "t".into(), None, None);
        // The tween's keys, on the text layer's clock (it starts at 1 s).
        assert_eq!(session.code.times.len(), 1);
        assert_eq!(session.code.times[0].keys.iter().map(|k| k.t_value).collect::<Vec<_>>(), vec![Some(0.0), Some(0.4)]);
        assert_eq!(session.clock_of("layers[1]"), 1.0);
        assert!(session.code.literals.iter().any(|l| l.text == "120"));
    }

    #[test]
    fn edits_undo_and_redo() {
        let mut session = MotionSession::new(DOC.into(), "t".into(), None, None);
        assert!(session.apply(&MotionOp::Key { layer: "layers[1]".into(), prop: "opacity".into(), t: 2.0, value: "0.5".into() }).unwrap());
        assert!(session.source.contains("keys([0.0, 0], [0.4, 1, @ease_out], [2, 0.5])"));
        assert!(session.dirty());
        assert_eq!(session.undo(false).as_deref(), Some("key opacity"));
        assert_eq!(session.source, DOC);
        session.undo(true);
        assert!(session.source.contains("[2, 0.5]"));
        session.apply(&MotionOp::DeleteKeys(vec![("layers[1]".into(), "opacity".into(), 0), ("layers[1]".into(), "opacity".into(), 2)])).unwrap();
        assert!(session.source.contains("opacity: keys([0.4, 1])"), "{}", session.source);
    }

    #[test]
    fn editor_state_follows_moved_layers() {
        let mut session = MotionSession::new(DOC.into(), "t".into(), None, None);
        session.hidden.insert("layers[1]".into());
        session.apply(&MotionOp::Remove { layer: "layers[0]".into() }).unwrap();
        // The layer above moved down a place: its editor state with it.
        assert!(session.hidden.contains("layers[0]"));
        session.apply(&MotionOp::Remove { layer: "layers[0]".into() }).unwrap();
        assert!(session.hidden.is_empty());
    }

    #[test]
    fn a_burst_is_one_undo_step() {
        let mut session = MotionSession::new(DOC.into(), "t".into(), None, None);
        for size in ["121", "122", "123"] {
            session.apply_burst(&MotionOp::Value { layer: "layers[1]".into(), prop: "size".into(), value: size.into() }, "size").unwrap();
        }
        session.end_burst();
        assert!(session.source.contains("size: 123"));
        assert_eq!(session.undo.len(), 1);
        session.undo(false);
        assert_eq!(session.source, DOC);
    }

    #[test]
    fn multi_line_text_round_trips() {
        let mut session = MotionSession::new(DOC.into(), "t".into(), None, None);
        let text = "A MARKER\nON EVERY\nBEAT \"now\"";
        let op = session.write_op("layers[1]", "text", makepad_motion::patch::fmt_text(text));
        session.apply(&op).unwrap();
        // `in(@headline)`: the input's default holds it, escaped.
        assert!(session.source.contains(r#"headline: "A MARKER\nON EVERY\nBEAT \"now\"""#), "{}", session.source);
        let doc = MotionDoc::load(&session.source).unwrap();
        assert_eq!(doc.input(makepad_motion::LiveId::from_str("headline")), Some(&makepad_motion::InputValue::Text(text.into())));
    }

    #[test]
    fn a_layer_a_layout_places_is_known_by_the_model() {
        let doc = r#"use mod.motion.*; Motion{
    size: vec2(640, 360) duration: 2.0
    layers: [
        Rect{name: "free" size: vec2(40, 40) fill: #ffffff at: vec2(10, 10)}
        Repeat{name: "grid" count: 3 layout: @grid columns: 3 spacing: vec2(60, 0) at: vec2(100, 100)
            layer: Rect{size: vec2(40, 40) fill: #ff0000}}
    ]
}
"#;
        let mut session = MotionSession::new(doc.into(), "t".into(), None, None);
        session.doc = Some(Arc::new(MotionDoc::load(doc).expect("loads")));
        session.doc_of = session.revision;
        assert_eq!(session.placement("layers[0]"), None);
        let placed = session.placement("layers[1].layer");
        assert!(placed.is_some(), "the Repeat's layer is placed by it");
        let placed = placed.unwrap();
        assert_eq!(placed.owner.as_deref(), Some("layers[1]"));
        assert_eq!(placed.hint, "positioned by Repeat ‘grid’");
    }

    /// A plate that imports from its folder (`import("shared/…")`) is asked
    /// in its folder too: the moved copy loads with the shown document's
    /// modules, so the layout that places a layer is still found.
    #[test]
    fn a_plate_with_a_folder_import_is_placed_through_its_folder() {
        struct Folder;
        impl makepad_motion::modules::ModuleResolver for Folder {
            fn path(&self, _from: &str, path: &str) -> Result<makepad_motion::modules::ModuleSource, String> {
                (path == "shared/grid.splash").then(|| makepad_motion::modules::ModuleSource::new(path, "1", "{columns: 3 gap: 60}")).ok_or_else(|| format!("no {path} in the folder"))
            }
        }
        let doc = r#"use mod.motion.*
let grid = import("shared/grid.splash")
Motion{
    size: vec2(640, 360) duration: 2.0
    layers: [
        Rect{name: "free" size: vec2(40, 40) fill: #ffffff at: vec2(10, 10)}
        Repeat{name: "grid" count: 3 layout: @grid columns: grid.columns spacing: vec2(grid.gap, 0) at: vec2(100, 100)
            layer: Rect{size: vec2(40, 40) fill: #ff0000}}
    ]
}
"#;
        let context = makepad_motion::MotionContext { modules: makepad_motion::modules::Modules(Arc::new(Folder)), ..Default::default() };
        let mut session = MotionSession::new(doc.into(), "t".into(), None, None);
        session.doc = Some(Arc::new(MotionDoc::load_with(doc, &[], context).expect("loads in its folder")));
        session.doc_of = session.revision;
        assert_eq!(session.placement("layers[0]"), None, "a free layer moves itself");
        let placed = session.placement("layers[1].layer").expect("the Repeat's layer is placed by it");
        assert_eq!(placed.owner.as_deref(), Some("layers[1]"));
    }

    #[test]
    fn values_show_in_their_units() {
        assert_eq!(show("scale", &[1.0, 1.0]), "100×100 %");
        assert_eq!(show("opacity", &[0.5]), "50%");
        assert_eq!(show("fill", &[1.0, 0.0, 0.0, 1.0]), "#ff0000");
        assert_eq!(show("at", &[160.0, 460.25]), "160, 460.3");
    }

    #[test]
    fn the_history_only_grows() {
        let mut session = MotionSession::new(DOC.into(), "t".into(), None, None);
        session.apply(&MotionOp::Value { layer: "layers[1]".into(), prop: "size".into(), value: "130".into() }).unwrap();
        let edited = session.source.clone();
        session.undo(false);
        assert_eq!(session.source, DOC);
        // A new edit after the undo: the undone state is still in the history.
        session.apply(&MotionOp::Value { layer: "layers[1]".into(), prop: "size".into(), value: "140".into() }).unwrap();
        let kinds: Vec<&StepKind> = session.history.iter().map(|h| &h.kind).collect();
        assert_eq!(kinds, [&StepKind::Opened, &StepKind::Edit, &StepKind::Undo, &StepKind::Edit]);
        assert_eq!(session.history[1].after.source, edited);
        assert!(session.history[1].label.contains("120 → 130"), "{}", session.history[1].label);
        // And reachable: going to it is one more entry.
        assert!(session.go_to(1));
        assert_eq!(session.source, edited);
        assert_eq!(session.history.last().unwrap().kind, StepKind::GoTo(1));
        assert_eq!(session.current_step(), Some(session.history.len() - 1));
        // Cmd-Z from there returns to where it was.
        session.undo(false);
        assert!(session.source.contains("size: 140"));
        assert_eq!(session.history.len(), 6);
    }

    #[test]
    fn the_agents_history_reads_an_undone_step() {
        let mut session = MotionSession::new(DOC.into(), "t".into(), None, None);
        session.apply(&MotionOp::Value { layer: "layers[1]".into(), prop: "size".into(), value: "130".into() }).unwrap();
        session.undo(false);
        session.apply(&MotionOp::Value { layer: "layers[1]".into(), prop: "size".into(), value: "140".into() }).unwrap();
        let (label, texts) = super::super::history::texts_at(&session, "1").unwrap();
        assert!(texts[0].1.contains("size: 130"), "{label}");
        let listing = super::super::history::listing(&session, 10);
        assert!(listing.contains("step 3") && listing.contains("now"), "{listing}");
        let diff = super::super::history::diff(&session, "1", "now").unwrap();
        assert!(diff.contains("130") && diff.contains("140"), "{diff}");
    }
}

//! The scene's structure, right of the picture (STRUCTURE.md §3): the
//! scene as a tree read from its code (the post stack first, then the
//! layers), and under it the selected node's attached stacks (its
//! `effects:`, a scene's or group's `post:`: bypass, reorder, add, remove)
//! and its parameters, drawn with the editor panel's own controls (so they
//! drag live, key with the key button and write one undo step each).
//!
//! The code is the truth: the tree is read again from the document each
//! time it changes (`makepad_motion::structure`), and every change here is
//! a text edit through the desk's code edits.

use makepad_edit_values::ValueKind;
use super::super::VideoCut;
use makepad_motion::structure::{self, Form, Generated, Node, NodeKind, Param};
use crate::structure::{StructureAction, StructureTree, TreeRow};
use makepad_stage_motion_pickers::effects::PanelEffectsWidgetRefExt;
use makepad_edit_values::Edit;
use makepad_widgets::*;
use std::collections::HashMap;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    /** The column: the tree, then the selected node's parameters. */
    mod.widgets.StructureColumn = View{width: Fill height: Fill flow: Down
        st_scroll := ScrollYView{width: Fill height: Fill flow: Down
            st_tree := mod.widgets.StructureTree{}
            // The selected node's attached stacks (its post, its effects):
            // the panel's one effects-stack widget, its parameters shown
            // below with the node's (`details: false`).
            View{width: Fill height: Fit flow: Down spacing: 6 padding: Inset{left: 6 right: 6 top: 2}
                st_stack0 := mod.widgets.PanelEffects{details: false visible: false}
                st_stack1 := mod.widgets.PanelEffects{details: false visible: false}
            }
            // A post entry written as a call (film_post(…)): written out
            // as the passes it makes, on demand.
            View{width: Fill height: Fit padding: Inset{left: 8 top: 4}
                st_expand := StageButton{width: Fit height: 22 margin: 0 padding: Inset{left: 10 right: 10} visible: false text: "Expand into its passes" draw_text +: {text_style: theme.font_regular{font_size: 8}}}}
            st_params := Splash{width: Fill height: Fit}
            View{width: Fill height: 24}
        }
    }
}

// ---------------------------------------------------------------- the desk's side

/// The column's state on the page.
#[derive(Default)]
pub struct Structure {
    /// The revision the tree was read at.
    read_rev: Option<u64>,
    root: Option<Node>,
    /// Each layer's generators (the calls its drawing code makes).
    gens: Vec<(String, Vec<Generated>)>,
    /// How many things each generator drew in the picture at the playhead.
    counts: std::collections::HashMap<String, usize>,
    /// The picks those counts were taken from (model, time).
    counted: Option<(u64, f64)>,
    /// The person's layout was read (once).
    layout_read: bool,
    /// The top pane shows the history (else the structure).
    pub history_tab: bool,
    /// Folded nodes (paths).
    folded: std::collections::HashSet<String>,
    pub selected: Option<String>,
    /// What code written as a call or a name made at load, per node: the
    /// running model's layers under it, by where each was constructed.
    made: Vec<(String, Vec<Made>)>,
    /// The load the `made` rows were read from.
    made_of: Option<u64>,
    /// The parameters' program as mounted.
    params_body: Option<String>,
    /// What the model was last given for (revision, selection, folds).
    shown: Option<(u64, Option<String>, usize)>,
}

/// Layers a node's code made at load, of one kind, constructed at one
/// place (STRUCTURE.md §1.4: made-by provenance).
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    /// `<node path>~<line>:<kind>`.
    pub path: String,
    pub label: String,
    pub line: u32,
    /// The model's paths of the layers made there.
    pub layers: Vec<String>,
}

/// The layers each code node (a call or a name in a layer list) made, from
/// the running model: grouped by where each was constructed and its kind.
fn made_layers(root: &Node, doc: &makepad_motion::MotionDoc) -> Vec<(String, Vec<Made>)> {
    let all = doc.all_layers();
    let mut out = Vec::new();
    for n in structure::flatten(root) {
        if !matches!(n.kind, NodeKind::Code | NodeKind::Generator) || !n.path.starts_with("Motion.layers") {
            continue;
        }
        let model = n.path.trim_start_matches("Motion.");
        let mut groups: Vec<Made> = Vec::new();
        for l in all.iter().filter(|l| l.path == model || l.path.starts_with(&format!("{model}.")) || l.path.starts_with(&format!("{model}#")) || l.path.starts_with(&format!("{model}["))) {
            let kind = l.kind.type_name();
            let key = format!("{}~{}:{kind}", n.path, l.loc.line);
            match groups.iter_mut().find(|g| g.path == key) {
                Some(g) => g.layers.push(l.path.clone()),
                None => groups.push(Made { path: key, label: l.name.clone().unwrap_or_else(|| kind.to_string()), line: l.loc.line, layers: vec![l.path.clone()] }),
            }
        }
        for g in &mut groups {
            if g.layers.len() > 1 {
                g.label = format!("{} ×{}", g.label, g.layers.len());
            }
        }
        if !groups.is_empty() {
            out.push((n.path.clone(), groups));
        }
    }
    out
}

/// The node the column shows for a selected path: itself, or the node a
/// selected stack entry is attached to (and the entry).
fn owner_of<'a>(root: &'a Node, path: &str) -> Option<(&'a Node, Option<&'a Node>)> {
    // A row of what a node's code made: that node.
    let path = path.split('~').next().unwrap_or(path);
    if path == "Motion.post" {
        return Some((root, None));
    }
    for n in structure::flatten(root) {
        for s in &n.stacks {
            if let Some(item) = s.items.iter().find(|i| i.path == path) {
                return Some((n, Some(item)));
            }
        }
    }
    structure::find(root, path).map(|n| (n, None))
}

fn tree_rows(root: &Node, folded: &std::collections::HashSet<String>, gens: &[(String, Vec<Generated>)], counts: &std::collections::HashMap<String, usize>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    rows.push(TreeRow { path: root.path.clone(), depth: 0, label: root.label.clone(), detail: "scene".into(), kind: NodeKind::Scene, on: true, open: None });
    if let Some(post) = root.stacks.iter().find(|s| s.field == "post") {
        let detail = if post.items.is_empty() { "none".to_string() } else { format!("{}", post.items.len()) };
        rows.push(TreeRow { path: post.path.clone(), depth: 1, label: "Post".into(), detail, kind: NodeKind::Post, on: true, open: None });
    }
    type Gens<'a> = (&'a [(String, Vec<Generated>)], &'a std::collections::HashMap<String, usize>);
    fn go(n: &Node, depth: usize, folded: &std::collections::HashSet<String>, g: Gens, rows: &mut Vec<TreeRow>) {
        let made: &[Generated] = g.0.iter().find(|(p, _)| *p == n.path).map_or(&[], |(_, v)| v.as_slice());
        let effects: Vec<String> = n.stacks.iter().flat_map(|s| s.items.iter()).map(|i| if i.on { i.ty.clone() } else { format!("({})", i.ty) }).collect();
        let mut detail = match n.kind {
            NodeKind::Generator => format!("{}(…)", n.ty),
            NodeKind::Code => "code".to_string(),
            _ if n.label != n.ty => n.ty.clone(),
            _ => String::new(),
        };
        if !effects.is_empty() {
            detail = format!("✦ {}  {detail}", effects.join(" "));
        }
        let open = (!n.children.is_empty() || !made.is_empty()).then(|| !folded.contains(&n.path));
        rows.push(TreeRow { path: n.path.clone(), depth, label: n.label.clone(), detail: detail.trim().to_string(), kind: n.kind, on: n.on, open });
        if open == Some(true) {
            for m in made {
                let count = g.1.get(&m.path).copied().unwrap_or(0);
                let mut detail = if count > 0 { format!("×{count}") } else { String::new() };
                if let Some((var, over)) = m.loops.last() {
                    detail = format!("for {var} in {over}  {detail}");
                }
                rows.push(TreeRow { path: m.path.clone(), depth: depth + 1, label: format!("{}(…)", m.callee), detail: detail.trim().to_string(), kind: NodeKind::Generator, on: true, open: None });
            }
            for c in &n.children {
                go(c, depth + 1, folded, g, rows);
            }
        }
    }
    for c in &root.children {
        go(c, 1, folded, (gens, counts), &mut rows);
    }
    rows
}

/// A parameter as the panel's controls write it.
fn control_of(p: &Param, node: &Node) -> Option<String> {
    let label = &p.name;
    let path = p.path.replace('"', "");
    let written = p.written.as_deref().unwrap_or("").trim();
    // The range the effect's catalogue, or the layer's reader, keeps it in.
    let range = match node.kind {
        NodeKind::Effect => structure::effect_range(&node.ty, &p.name),
        _ => structure::field_range(node, &p.name),
    };
    let range = range.map(|(min, max)| format!(" min: {} max: {}", makepad_motion::patch::fmt_num(min), makepad_motion::patch::fmt_num(max))).unwrap_or_default();
    let choices = || structure::effect_choices(&node.ty, &p.name).map(|c| c.join(" "));
    Some(match p.form {
        Form::Unwritten => return None,
        Form::Expression => {
            let short: String = written.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(48).collect();
            format!("View{{width: Fill height: 20 flow: Right padding: Inset{{left: 8}} align: Align{{y: 0.5}} Label{{width: 104 text: \"{label}\" draw_text +: {{color: mod.stage.label text_style: theme.font_regular{{font_size: 8}}}}}} Label{{width: Fill text: \"ƒ {}\" draw_text +: {{color: mod.stage.muted text_style: theme.font_code{{font_size: 8}}}}}}}}", short.replace('\\', "").replace('"', "'"))
        }
        Form::Modulated => format!("PanelMod{{path: \"{path}\" label: \"{label}\"}}"),
        _ => match p.kind {
            ValueKind::Colour => format!("PanelColor{{path: \"{path}\" label: \"{label}\"}}"),
            ValueKind::Bool => format!("PanelToggle{{path: \"{path}\" label: \"{label}\"}}"),
            ValueKind::Font => format!("PanelFont{{path: \"{path}\" label: \"{label}\"}}"),
            ValueKind::Text => format!("PanelText{{path: \"{path}\" label: \"{label}\"}}"),
            ValueKind::Choice | ValueKind::Ease => match choices() {
                Some(c) => format!("PanelChoice{{path: \"{path}\" label: \"{label}\" choices: \"{c}\"}}"),
                None => format!("View{{width: Fill height: 20 flow: Right padding: Inset{{left: 8}} align: Align{{y: 0.5}} Label{{width: 104 text: \"{label}\" draw_text +: {{color: mod.stage.label text_style: theme.font_regular{{font_size: 8}}}}}} Label{{width: Fill text: \"{}\" draw_text +: {{color: mod.stage.text text_style: theme.font_regular{{font_size: 8}}}}}}}}", written.replace('"', "'")),
            },
            // A vector: a row of value fields, one per part.
            _ if written.starts_with("vec3(") || written.starts_with("vec2(") => format!("PanelVec3{{path: \"{path}\" label: \"{label}\"}}"),
            _ if p.form == Form::Curve || matches!(p.kind, ValueKind::Number | ValueKind::Time | ValueKind::Angle | ValueKind::Length) => {
                format!("PanelSlider{{path: \"{path}\" label: \"{label}\"{range}}}")
            }
            _ => format!("View{{width: Fill height: 20 flow: Right padding: Inset{{left: 8}} align: Align{{y: 0.5}} Label{{width: 104 text: \"{label}\" draw_text +: {{color: mod.stage.label text_style: theme.font_regular{{font_size: 8}}}}}} Label{{width: Fill text: \"{}\" draw_text +: {{color: mod.stage.text text_style: theme.font_regular{{font_size: 8}}}}}}}}", written.chars().take(40).collect::<String>().replace('"', "'")),
        },
    })
}

/// The parameters' program for a node: its written parameters as the
/// panel's controls (a name written in this document is edited where it is
/// defined).
fn params_body(text: &str, node: &Node) -> String {
    let mut controls = Vec::new();
    let mut computed = Vec::new();
    for mut p in structure::params(text, node) {
        // `strength: BLOOM`: the control edits BLOOM (its `let`).
        if p.form == Form::Name {
            let name = p.written.clone().unwrap_or_default();
            if let Some(at) = text.find(&format!("\nlet {name} = ")) {
                let value_at = at + 7 + name.len();
                let end = text[value_at..].find('\n').map_or(text.len(), |e| value_at + e);
                let written = text[value_at..end].split("//").next().unwrap_or("").trim().to_string();
                p.form = structure::form_of(&written);
                p.kind = makepad_motion::paths::kind_of(&written, |_| false);
                p.path = name.clone();
                p.name = format!("{} → {name}", p.name);
                p.written = Some(written);
            } else {
                p.form = Form::Expression;
            }
        }
        if let Some(c) = control_of(&p, node) {
            // Computed values after the editable ones, folded.
            if p.form == Form::Expression {
                computed.push(c);
            } else {
                controls.push(c);
            }
        }
    }
    if !computed.is_empty() {
        controls.push(format!("PanelGroup{{title: \"computed · {}\"\n        {}\n    }}", computed.len(), computed.join("\n        ")));
    }
    if controls.is_empty() {
        return String::new();
    }
    let title = match node.kind {
        NodeKind::Effect | NodeKind::Post => node.ty.clone(),
        _ => node.label.clone(),
    };
    format!("View{{width: Fill height: Fit flow: Down spacing: 4 padding: Inset{{left: 6 right: 6 top: 6}}\n    Label{{text: \"{}\" draw_text +: {{color: mod.stage.secondary text_style: theme.font_bold{{font_size: 8}}}}}}\n    {}\n}}", title.replace('"', "'"), controls.join("\n    "))
}

/// Whether a pick was drawn by generator `g` (its call is on the pick's
/// code stack).
fn drawn_by(p: &super::picking::Pickable, g: &Generated) -> bool {
    p.code.iter().any(|(f, b)| *f == makepad_motion::schema::CodeFile::Doc && g.range.start <= *b && *b < g.range.end)
}

/// The generator a path names (`<layer>#callee@byte`, or `…[i]` an
/// instance of it).
fn generator_of<'a>(gens: &'a [(String, Vec<Generated>)], path: &str) -> Option<(&'a Generated, Option<usize>)> {
    let (base, instance) = match path.strip_suffix(']').and_then(|p| p.rsplit_once('[')) {
        Some((b, i)) if b.contains('#') => (b, i.parse::<usize>().ok()),
        _ => (path, None),
    };
    gens.iter().flat_map(|(_, g)| g.iter()).find(|g| g.path == base).map(|g| (g, instance))
}

/// The controls of a generator: the values its call reads (the records
/// and knobs its arguments and its function name, as "what fed it"), or
/// one instance's own record when the loop runs over a list written here.
fn generator_body(text: &str, code: &makepad_motion::schema::code::CodeOutline, g: &Generated, instance: Option<usize>, doc: Option<&makepad_motion::MotionDoc>) -> String {
    let files = [makepad_motion::paths::File { name: "", text, code }];
    let entries = makepad_motion::paths::list(&files);
    let mut wanted: Vec<String> = Vec::new();
    let mut title = format!("{}(…)", g.callee);
    let shared = || makepad_motion::schema::code::gather(code, text, g.range.start, 12).into_iter().filter(|f| f.depth > 0).map(|f| f.name).fold(Vec::new(), |mut v: Vec<String>, n| {
        if !v.contains(&n) {
            v.push(n);
        }
        v
    });
    let own = instance.and_then(|i| g.instance_path(i));
    let mut note = String::new();
    match &own {
        Some(at) if entries.iter().any(|e| e.path.starts_with(&format!("{at}."))) => {
            title = format!("{title} · {at}");
            wanted.push(at.clone());
        }
        _ => {
            if let Some(i) = instance {
                title = format!("{title} · #{i}");
                note = String::new();
            }
            wanted = shared();
        }
    }
    let node = Node { kind: NodeKind::Generator, path: g.path.clone(), ty: g.callee.clone(), label: g.callee.clone(), comment: None, range: g.range.clone(), on: true, children: Vec::new(), stacks: Vec::new() };
    // What it reads, value by value: the editable ones first; a record or
    // list it reads folds to one summary row; computed values (ƒ) after
    // them, behind a fold, dimmed; a sung word shows as its word.
    let mut controls = Vec::new();
    let mut computed = Vec::new();
    let row = |label: &str, detail: &str| format!("View{{width: Fill height: 18 flow: Right padding: Inset{{left: 8}} align: Align{{y: 0.5}} Label{{width: 104 text: \"{}\" draw_text +: {{color: mod.stage.muted text_style: theme.font_regular{{font_size: 8}}}}}} Label{{width: Fill text: \"{}\" draw_text +: {{color: mod.stage.dim text_style: theme.font_regular{{font_size: 7.5}}}}}}}}", label.replace('"', "'"), detail.replace('"', "'").replace('\\', ""));
    // The words a record of sung words holds, with their times (the
    // running document's, as it loaded).
    let words: HashMap<String, (Option<String>, Option<f64>)> = doc.map(|d| wanted.iter().flat_map(|w| d.record_words_of(w).into_iter().map(move |(k, t, s)| (format!("{w}.{k}"), (t, s)))).collect()).unwrap_or_default();
    let clock = |t: f64| format!("{}:{:04.1}", (t / 60.0).floor() as i64, t % 60.0);
    let control = |e: &makepad_motion::paths::Entry, label: String| -> Option<(bool, String)> {
        let written = e.value.split(" (").next().unwrap_or(&e.value).to_string();
        let form = structure::form_of(&written);
        if matches!(form, Form::Name) {
            return None;
        }
        if form == Form::Expression {
            // A sung word: its word, not its formula.
            if written.trim_start().starts_with("sung(") {
                let (text, start) = words.get(&e.path).cloned().unwrap_or((None, None));
                let word = text.unwrap_or(label);
                let when = start.map(clock).unwrap_or_default();
                return Some((false, row(&word, &when)));
            }
            let short: String = written.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
            return Some((false, row(&label, &format!("ƒ {short}"))));
        }
        let p = Param { name: label, path: e.path.clone(), written: Some(written), default: None, form, kind: e.kind, doc: None };
        control_of(&p, &node).map(|c| (true, c))
    };
    for w in &wanted {
        let under: Vec<&makepad_motion::paths::Entry> = entries.iter().filter(|e| e.path == *w || e.path.starts_with(&format!("{w}.")) || e.path.starts_with(&format!("{w}["))).take(80).collect();
        if under.is_empty() {
            continue;
        }
        let short = |e: &makepad_motion::paths::Entry| e.path.strip_prefix(&format!("{w}.")).unwrap_or(&e.path).to_string();
        if under.len() > 3 {
            let mut edit = Vec::new();
            let mut comp = Vec::new();
            for e in &under {
                if let Some((editable, c)) = control(e, short(e)) {
                    if editable { edit.push(c) } else { comp.push(c) }
                }
            }
            let sung = under.iter().filter(|e| e.value.trim_start().starts_with("sung(")).count();
            let summary = if sung * 2 > under.len() { format!("{w} · {sung} sung words") } else { format!("{w} · {} values", under.len()) };
            let body: Vec<String> = edit.into_iter().chain(comp).collect();
            let group = format!("PanelGroup{{title: \"{}\"\n        {}\n    }}", summary.replace('"', "'"), body.join("\n        "));
            controls.push(group);
            continue;
        }
        for e in under {
            if let Some((editable, c)) = control(e, e.path.clone()) {
                if editable { controls.push(c) } else { computed.push(c) }
            }
        }
    }
    if !computed.is_empty() {
        controls.push(format!("PanelGroup{{title: \"computed · {}\"\n        {}\n    }}", computed.len(), computed.join("\n        ")));
    }
    if controls.is_empty() {
        note = String::new();
    }
    format!("View{{width: Fill height: Fit flow: Down spacing: 4 padding: Inset{{left: 6 right: 6 top: 6}}\n    Label{{text: \"{}\" draw_text +: {{color: mod.stage.secondary text_style: theme.font_bold{{font_size: 8}}}}}}{note}\n    {}\n}}", title.replace('"', "'"), controls.join("\n    "))
}

/// Every pump while a graphic is open: the tree read again when the code
/// changed, the column's model and parameters given.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    if !page.motion.structure.layout_read && page.motion.desk.session.is_some() {
        restore_layout(page, cx, &desk_ref);
    }
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let st = &mut page.motion.structure;
    if st.read_rev != Some(session.revision) {
        st.read_rev = Some(session.revision);
        // A document that does not read keeps the tree it had.
        if let Some(root) = structure::read(&session.source) {
            st.gens = structure::flatten(&root).into_iter().filter(|n| n.kind == NodeKind::Layer).map(|n| (n.path.clone(), structure::generators(&session.source, &session.code, n))).filter(|(_, g)| !g.is_empty()).collect();
            st.root = Some(root);
        }
    }
    // How many things each generator drew (from the picture's picks).
    let picks_at = page.motion.desk.picks_key();
    let st = &mut page.motion.structure;
    if picks_at.is_some() && st.counted != picks_at {
        st.counted = picks_at;
        let mut counts = std::collections::HashMap::new();
        if let Some(picks) = super::desk::picked_things(page) {
            let st = &page.motion.structure;
            for (_, gens) in &st.gens {
                for g in gens {
                    let n = picks.iter().filter(|p| p.content && drawn_by(p, g)).count();
                    counts.insert(g.path.clone(), n);
                }
            }
        }
        page.motion.structure.counts = counts;
        page.motion.structure.shown = None;
    }
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let st = &mut page.motion.structure;
    if st.made_of != Some(session.doc_of) {
        if let (Some(root), Some(doc)) = (st.root.as_ref(), session.doc.as_deref()) {
            st.made_of = Some(session.doc_of);
            st.made = made_layers(root, doc);
            st.shown = None;
        }
    }
    let Some(root) = st.root.as_ref() else { return };
    let key = (session.revision, st.selected.clone(), st.folded.len() + st.counts.len() * 1000 + st.made.len() * 7);
    if st.shown.as_ref() == Some(&key) {
        return;
    }
    st.shown = Some(key);
    let mut rows = tree_rows(root, &st.folded, &st.gens, &st.counts);
    // What code nodes made at load, under each (the running model's).
    let mut i = 0;
    while i < rows.len() {
        if let Some((_, made)) = st.made.iter().find(|(p, _)| *p == rows[i].path) {
            let open = !st.folded.contains(&rows[i].path);
            rows[i].open = Some(open);
            if open {
                let depth = rows[i].depth + 1;
                let extra: Vec<TreeRow> = made.iter().map(|m| TreeRow { path: m.path.clone(), depth, label: m.label.clone(), detail: format!("made at line {}", m.line), kind: NodeKind::Layer, on: true, open: None }).collect();
                let n = extra.len();
                rows.splice(i + 1..i + 1, extra);
                i += n;
            }
        }
        i += 1;
    }
    // A generator (or one of its instances) selected: its controls are what
    // fed it, or the instance's own record.
    if let Some((g, instance)) = st.selected.as_deref().and_then(|p| generator_of(&st.gens, p)) {
        let body = generator_body(&session.source, &session.code, g, instance, session.doc.as_deref());
        let unwritten = generator_offers(&session.source, &session.code, g, instance);
        let tree_selected = Some(g.path.clone());
        if let Some(mut tree) = desk_ref.widget(cx, ids!(st_tree)).borrow_mut::<StructureTree>() {
            tree.set_model(cx, rows, unwritten, tree_selected);
        }
        if st.params_body.as_ref() != Some(&body) {
            st.params_body = Some(body.clone());
            let splash = desk_ref.widget(cx, ids!(st_params)).as_splash();
            splash.set_grants(HostGrants::CLIPBOARD);
            splash.set_text(cx, &body);
        }
        show_stacks(cx, &desk_ref, None, None);
        desk_ref.widget(cx, ids!(st_tree)).redraw(cx);
        return;
    }
    let selected = st.selected.clone().filter(|p| owner_of(root, p).is_some()).or_else(|| Some(root.path.clone()));
    let (owner, item) = selected.as_deref().and_then(|p| owner_of(root, p)).unwrap_or((root, None));
    // The parameters: the selected entry's, else the node's.
    let shown = item.unwrap_or(owner);
    let unwritten: Vec<(String, String)> = structure::params(&session.source, shown).into_iter().filter(|p| p.form == Form::Unwritten).map(|p| (shown.path.clone(), p.name)).collect();
    let tree_selected = match (item, selected.as_deref()) {
        // A row of what a node's code made.
        (_, Some(p)) if p.contains('~') => Some(p.to_string()),
        (Some(i), _) => Some(i.path.clone()),
        (None, Some("Motion.post")) => Some("Motion.post".to_string()),
        _ => Some(owner.path.clone()),
    };
    let expandable = expand_target(root, tree_selected.as_deref()).is_some();
    desk_ref.widget(cx, ids!(st_expand)).set_visible(cx, expandable);
    if let Some(mut tree) = desk_ref.widget(cx, ids!(st_tree)).borrow_mut::<StructureTree>() {
        tree.set_model(cx, rows, unwritten, tree_selected.clone());
    }
    show_stacks(cx, &desk_ref, Some(owner), tree_selected);
    let body = if selected.as_deref() == Some("Motion.post") && item.is_none() { String::new() } else { params_body(&session.source, shown) };
    if st.params_body.as_ref() != Some(&body) {
        st.params_body = Some(body.clone());
        let splash = desk_ref.widget(cx, ids!(st_params)).as_splash();
        splash.set_grants(HostGrants::CLIPBOARD);
        splash.set_text(cx, &body);
    }
    desk_ref.widget(cx, ids!(st_tree)).redraw(cx);
}

/// The column's actions: a selection, a fold, a stack's edit, a parameter
/// written.
pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let tabs = desk_ref.widget(cx, ids!(m_tab_top));
    for action in actions.filter_widget_actions_cast::<super::super::controls::SegmentAction>(tabs.widget_uid()) {
        if let super::super::controls::SegmentAction::Selected(index) = action {
            show_top_tab(page, cx, &desk_ref, index == 1);
            save_layout(page);
        }
    }
    if desk_ref.button(cx, ids!(st_expand)).clicked(actions) {
        expand(page, cx);
    }
    let tree = desk_ref.widget(cx, ids!(st_tree));
    for action in actions.filter_widget_actions_cast::<StructureAction>(tree.widget_uid()) {
        match action {
            StructureAction::Select(path) => select(page, cx, &desk_ref, &path),
            StructureAction::Fold(path) => {
                let st = &mut page.motion.structure;
                if !st.folded.remove(&path) {
                    st.folded.insert(path);
                }
                st.shown = None;
            }
            StructureAction::Hover(path) => hover(page, cx, &desk_ref, path.as_deref()),
            StructureAction::AddParam { node, name } => add_param(page, cx, &node, &name),
            StructureAction::None => {}
        }
    }
    // The stacks: an entry clicked (or added) is selected, one under the
    // pointer outlined.
    for id in [ids!(st_stack0), ids!(st_stack1)] {
        let stack = desk_ref.widget(cx, id).as_panel_effects();
        if let Some(path) = stack.selected(actions) {
            select(page, cx, &desk_ref, &path);
        }
        if let Some(path) = stack.hovered(actions) {
            hover(page, cx, &desk_ref, path.as_deref());
        }
    }
}

/// The node's attached stacks in the column's effects-stack widgets (a
/// node has at most two: its `effects:` and, a Group or Scene3D, its
/// `post:`); none for a generator.
fn show_stacks(cx: &mut Cx, desk_ref: &WidgetRef, owner: Option<&Node>, selected: Option<String>) {
    let stacks: Vec<(String, &'static str)> = owner.map(|o| o.stacks.iter().map(|s| (s.path.clone(), s.field)).collect()).unwrap_or_default();
    for (k, id) in [ids!(st_stack0), ids!(st_stack1)].into_iter().enumerate() {
        let w = desk_ref.widget(cx, id);
        let Some((path, field)) = stacks.get(k).cloned() else {
            w.set_visible(cx, false);
            continue;
        };
        if let Some(mut stack) = w.borrow_mut::<makepad_stage_motion_pickers::effects::PanelEffects>() {
            if stack.path != path {
                stack.path = path;
                stack.title = if field == "post" { "Post".into() } else { "Effects".into() };
            }
        }
        w.as_panel_effects().set_selected(cx, selected.clone());
        w.set_visible(cx, true);
        w.redraw(cx);
    }
}

/// Where the right column's layout is kept (the person's layout): its
/// open tab.
fn layout_file() -> std::path::PathBuf {
    makepad_app_shell::user_state::dir(None, "layout").join("desk_right_column")
}

fn save_layout(page: &VideoCut) {
    let file = layout_file();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&file, if page.motion.structure.history_tab { "history" } else { "structure" });
}

/// The right column as the person left it, once.
fn restore_layout(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef) {
    page.motion.structure.layout_read = true;
    let history = std::fs::read_to_string(layout_file()).is_ok_and(|text| text.trim() == "history");
    show_top_tab(page, cx, desk_ref, history);
}

/// The top pane's tab: the history, or the structure.
fn show_top_tab(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, history: bool) {
    page.motion.structure.history_tab = history;
    desk_ref.widget(cx, ids!(m_history_page)).set_visible(cx, history);
    desk_ref.widget(cx, ids!(m_structure_page)).set_visible(cx, !history);
    if let Some(mut tabs) = desk_ref.widget(cx, ids!(m_tab_top)).borrow_mut::<super::super::controls::CutSegmented>() {
        tabs.set_active(cx, history as usize);
    }
    desk_ref.redraw(cx);
}

/// A node selected (in the tree, or by the picture): the column shows it,
/// the code goes to it, the picture outlines it.
pub fn select(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, path: &str) {
    page.motion.structure.selected = Some(path.to_string());
    page.motion.structure.shown = None;
    let Some(root) = page.motion.structure.root.as_ref() else { return };
    let made_line = path.split_once('~').and_then(|(_, r)| r.split(':').next()?.parse::<usize>().ok());
    let text = page.motion.desk.session.as_ref().map(|s| s.source.as_str()).unwrap_or("");
    let at = match (made_line, generator_of(&page.motion.structure.gens, path)) {
        // Where those layers were constructed.
        (Some(line), _) => text.split_inclusive('\n').take(line.saturating_sub(1)).map(str::len).sum::<usize>().into(),
        (None, Some((g, _))) => Some(g.range.start),
        (None, None) => owner_of(root, path).map(|(o, i)| i.unwrap_or(o).range.start),
    };
    if let Some(at) = at {
        if let Some(mut code) = desk_ref.widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
            code.reveal(cx, 0, at);
        }
    }
    outline(page, cx, desk_ref, Some(path));
}

/// The picture picked something: the tree selects the node that drew it
/// (the outermost code of the document's own scene record in its call
/// stack, else the layer it was drawn in).
pub fn reveal_pick(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, code: &[(makepad_motion::schema::CodeFile, usize)], layer: &str, loops: &[(LiveId, f64)]) {
    let Some(root) = page.motion.structure.root.as_ref() else { return };
    // Drawn by a generator: it (and the instance, by the loop it ran in).
    let gen_hit = page.motion.structure.gens.iter().flat_map(|(_, g)| g.iter()).find(|g| code.iter().any(|(f, b)| *f == makepad_motion::schema::CodeFile::Doc && g.range.start <= *b && *b < g.range.end));
    if let Some(g) = gen_hit {
        let index = g.loops.last().and_then(|(var, _)| loops.iter().find(|(id, _)| *id == LiveId::from_str(var)).map(|(_, v)| *v as usize));
        let path = match index {
            Some(i) if g.instance_path(i).is_some() => format!("{}[{i}]", g.path),
            _ => g.path.clone(),
        };
        let layer_path = g.path.split('#').next().unwrap_or("").to_string();
        let st = &mut page.motion.structure;
        st.folded.remove(&layer_path);
        let mut p = layer_path.as_str();
        while let Some(cut) = p.rfind(".layers[") {
            p = &p[..cut];
            st.folded.remove(p);
        }
        st.selected = Some(path);
        st.shown = None;
        desk_ref.widget(cx, ids!(st_tree)).redraw(cx);
        return;
    }
    let by_code = code.iter().rev().filter(|(f, _)| *f == makepad_motion::schema::CodeFile::Doc).find_map(|(_, byte)| structure::at_byte(root, *byte).filter(|n| n.kind != NodeKind::Scene).map(|n| n.path.clone()));
    let by_layer = || (!layer.is_empty()).then(|| structure::layer(root, layer).map(|n| n.path.clone())).flatten();
    let Some(path) = by_code.or_else(by_layer) else { return };
    // Unfold its parents.
    let st = &mut page.motion.structure;
    let mut p = path.as_str();
    while let Some(cut) = p.rfind(".layers[") {
        p = &p[..cut];
        st.folded.remove(p);
    }
    st.selected = Some(path);
    st.shown = None;
    desk_ref.widget(cx, ids!(st_tree)).redraw(cx);
}

fn layer_path(path: &str) -> Option<&str> {
    path.strip_prefix("Motion.").filter(|p| p.starts_with("layers"))
}

/// The picture outlines what a path is (its layer's box), or nothing.
fn outline(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, path: Option<&str>) {
    if let Some((g, instance)) = path.and_then(|p| generator_of(&page.motion.structure.gens, p)) {
        let var = g.loops.last().map(|(v, _)| LiveId::from_str(v));
        let quads: Vec<[[f64; 2]; 4]> = super::desk::picked_things(page).map(|picks| picks.iter().filter(|p| p.content && drawn_by(p, g) && match (instance, var) {
            (Some(i), Some(var)) => p.loops.iter().any(|(id, v)| *id == var && *v as usize == i),
            _ => true,
        }).map(|p| p.quad).take(400).collect()).unwrap_or_default();
        if let Some(mut viewer) = desk_ref.widget(cx, ids!(m_viewer)).borrow_mut::<super::viewer::MotionViewer>() {
            viewer.set_highlight(cx, quads, String::new());
        }
        return;
    }
    if let Some(m) = path.and_then(|p| page.motion.structure.made.iter().flat_map(|(_, v)| v.iter()).find(|m| m.path == p)).cloned() {
        let quads: Vec<[[f64; 2]; 4]> = super::desk::layer_boxes(page).map(|boxes| boxes.iter().filter(|(p, _)| m.layers.iter().any(|l| p == l || p.starts_with(&format!("{l}.")) || p.starts_with(&format!("{l}#")))).map(|(_, q)| *q).collect()).unwrap_or_default();
        if let Some(mut viewer) = desk_ref.widget(cx, ids!(m_viewer)).borrow_mut::<super::viewer::MotionViewer>() {
            viewer.set_highlight(cx, quads, String::new());
        }
        return;
    }
    let quads: Vec<[[f64; 2]; 4]> = match (path.and_then(layer_path), super::desk::layer_boxes(page)) {
        (Some(layer), Some(boxes)) => {
            // An entry of a layer's stack outlines its layer.
            let layer = layer.split(".effects").next().unwrap_or(layer).split(".post").next().unwrap_or(layer);
            boxes.iter().filter(|(p, _)| p == layer || p.starts_with(&format!("{layer}."))).map(|(_, q)| *q).collect()
        }
        _ => Vec::new(),
    };
    if let Some(mut viewer) = desk_ref.widget(cx, ids!(m_viewer)).borrow_mut::<super::viewer::MotionViewer>() {
        viewer.set_highlight(cx, quads, String::new());
    }
}

/// The document's post entry a selection can be expanded from: (the
/// entry's index in the list, or None for a `post` that is one call; its
/// bytes).
fn expand_target(root: &Node, selected: Option<&str>) -> Option<(Option<usize>, std::ops::Range<usize>)> {
    let post = root.stacks.iter().find(|s| s.field == "post")?;
    let sel = selected?;
    if post.generated && (sel == "Motion.post" || post.items.first().is_some_and(|i| i.path == sel)) {
        return Some((None, post.range.clone()?));
    }
    post.items.iter().enumerate().find(|(_, i)| i.path == sel && i.kind == NodeKind::Generator).map(|(k, i)| (Some(k), i.range.clone()))
}

/// The selected generator in the scene's post written out as the passes it
/// makes (one undo step), when every value it gives is written in the scene.
fn expand(page: &mut VideoCut, cx: &mut Cx) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let Some(root) = page.motion.structure.root.as_ref() else { return };
    let Some((entry, range)) = expand_target(root, page.motion.structure.selected.as_deref()) else { return };
    let Some(doc) = session.doc.as_ref() else { return };
    let entries = match doc.post_expansion(entry) {
        Ok(e) => e,
        Err(why) => {
            page.motion.desk.status = format!("Cannot expand: {why}");
            page.motion.desk.repaint = true;
            return;
        }
    };
    let text = &session.source;
    let ls = text[..range.start].rfind('\n').map_or(0, |i| i + 1);
    let indent: String = text[ls..range.start].chars().take_while(|c| c.is_whitespace()).collect();
    let written = match entry {
        Some(_) => entries.join(&format!("\n{indent}")),
        None => format!("[\n{indent}    {}\n{indent}]", entries.join(&format!("\n{indent}    "))),
    };
    page.motion.desk.author = Some("You · structure".into());
    let gesture = u64::MAX / 8 + 3 + page.motion.structure.read_rev.unwrap_or(0);
    super::desk::code_edits(page, cx, 0, vec![Edit::splice(range.start..range.end, written, gesture, true)]);
    page.motion.structure.selected = Some("Motion.post".into());
    page.motion.structure.shown = None;
}

/// The picture outlines a node (a curve channel hovered), or the
/// selection again.
pub fn outline_path(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, path: Option<&str>) {
    hover(page, cx, desk_ref, path);
}

fn hover(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, path: Option<&str>) {
    let shown = path.map(str::to_string).or_else(|| page.motion.structure.selected.clone());
    outline(page, cx, desk_ref, shown.as_deref());
}

/// What a generator (or one of its instances) can be given more of: the
/// options its function reads that the call does not write, the options of
/// the list item it runs over that this item does not write, and its
/// literals that could become parameters (↑). (key, label) each; the key
/// says what a click writes (`gen|<generator>|opt|NAME`, `…|item|NAME`,
/// `…|lit|<byte>|NAME`).
fn generator_offers(text: &str, code: &makepad_motion::schema::code::CodeOutline, g: &Generated, instance: Option<usize>) -> Vec<(String, String)> {
    let Some(def) = structure::fn_def(text, code, &g.callee) else { return Vec::new() };
    let sig = structure::signature(text, &def);
    let mut out = Vec::new();
    let call_text = &text[g.range.clone()];
    let item_param = g.loops.last().and_then(|(var, _)| g.args.iter().position(|a| a.trim() == var)).and_then(|k| def.params.get(k).map(|p| p.0.clone()));
    match instance.and_then(|i| g.instance_path(i)) {
        // An instance: the item's own values.
        Some(at) => {
            let files = [makepad_motion::paths::File { name: "", text, code }];
            for p in sig.iter().filter(|p| p.of.is_some() && p.of == item_param) {
                if makepad_motion::paths::resolve_exact(&files, &format!("{at}.{}", p.name)).is_err() {
                    out.push((format!("gen|{}|item|{}|{}", g.path, p.name, at), p.name.clone()));
                }
            }
        }
        None => {
            // The call's options record: what it does not write yet.
            let rec = g.args.iter().rev().find(|a| a.trim_start().starts_with('{'));
            if let Some(rec) = rec {
                let written = makepad_motion::paths::record_fields(rec).unwrap_or_default();
                for p in sig.iter().filter(|p| p.of.is_some() && p.of != item_param && !written.iter().any(|(k, _)| *k == p.name)) {
                    out.push((format!("gen|{}|opt|{}", g.path, p.name), p.name.clone()));
                }
            }
            let _ = call_text;
            // Its literals, as parameters to be.
            let taken: Vec<String> = sig.iter().map(|p| p.name.clone()).collect();
            for (label, value, range) in structure::fn_literals(code, &def).into_iter().take(24) {
                let mut name: String = label.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect::<String>().trim_matches('_').to_string();
                if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    name = "value".into();
                }
                let mut unique = name.clone();
                let mut k = 2;
                while taken.contains(&unique) || out.iter().any(|(key, _): &(String, String)| key.ends_with(&format!("|{unique}"))) {
                    unique = format!("{name}{k}");
                    k += 1;
                }
                out.push((format!("gen|{}|lit|{}|{unique}", g.path, range.start), format!("↑ {label} {value}")));
            }
        }
    }
    out
}

/// A generator's offer taken (see [`generator_offers`]).
fn generator_write(page: &mut VideoCut, cx: &mut Cx, key: &str) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let parts: Vec<&str> = key.split('|').collect();
    let (Some(path), Some(what)) = (parts.get(1), parts.get(2)) else { return };
    let Some((g, _)) = generator_of(&page.motion.structure.gens, path) else { return };
    let g = g.clone();
    let text = session.source.clone();
    let def = structure::fn_def(&text, &session.code, &g.callee);
    let default_of = |name: &str| def.as_ref().and_then(|d| structure::signature(&text, d).into_iter().find(|p| p.name == name)).and_then(|p| p.default).unwrap_or_else(|| "0".into());
    let next: Result<String, String> = match *what {
        "opt" => {
            let name = parts.get(3).copied().unwrap_or("");
            let node = Node { kind: NodeKind::Generator, path: g.path.clone(), ty: g.callee.clone(), label: g.callee.clone(), comment: None, range: g.range.clone(), on: true, children: Vec::new(), stacks: Vec::new() };
            structure::write_param(&text, &node, name, &default_of(name)).map(|sp| sp.apply(&text)).ok_or_else(|| "the call has no options record".into())
        }
        "item" => {
            let (name, at) = (parts.get(3).copied().unwrap_or(""), parts.get(4).copied().unwrap_or(""));
            let files = [makepad_motion::paths::File { name: "", text: &text, code: &session.code }];
            match makepad_motion::paths::resolve_exact(&files, at) {
                Ok(site) => structure::add_field(&text, site.range.start, name, &default_of(name)).map(|sp| sp.apply(&text)).ok_or_else(|| "the item is not a record".into()),
                Err(why) => Err(why),
            }
        }
        "lit" => {
            let at = parts.get(3).and_then(|b| b.parse::<usize>().ok()).unwrap_or(0);
            let name = parts.get(4).copied().unwrap_or("value");
            let lit = session.code.literals.iter().find(|l| l.range.start == at).map(|l| l.range.clone());
            match lit {
                Some(range) => structure::promote(&text, &session.code, &g, range, name),
                None => Err("that value moved; select it again".into()),
            }
        }
        _ => return,
    };
    match next {
        Ok(next) => {
            let Some(r) = super::session::changed_range(&text, &next) else { return };
            let old_end = r.end as isize + text.len() as isize - next.len() as isize;
            page.motion.desk.author = Some("You · structure".into());
            let gesture = u64::MAX / 8 + 5 + page.motion.structure.read_rev.unwrap_or(0);
            super::desk::code_edits(page, cx, 0, vec![Edit::splice(r.start..old_end as usize, next[r.clone()].to_string(), gesture, true)]);
            page.motion.structure.shown = None;
        }
        Err(why) => {
            page.motion.desk.status = format!("Structure: {why}");
            page.motion.desk.repaint = true;
        }
    }
}

fn add_param(page: &mut VideoCut, cx: &mut Cx, node_path: &str, name: &str) {
    if node_path.starts_with("gen|") {
        generator_write(page, cx, node_path);
        return;
    }
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let Some(root) = structure::read(&session.source) else { return };
    let Some((owner, item)) = owner_of(&root, node_path) else { return };
    let node = item.unwrap_or(owner);
    let Some(param) = structure::params(&session.source, node).into_iter().find(|p| p.name == name) else { return };
    let code = param.default.clone().unwrap_or_else(|| "0".into());
    if let Some(splice) = structure::write_param(&session.source, node, name, &code) {
        page.motion.desk.author = Some("You · structure".into());
        super::desk::code_edits(page, cx, 0, vec![Edit::splice(splice.range.start..splice.range.end, splice.text, u64::MAX / 8 + 1 + page.motion.structure.read_rev.unwrap_or(0), true)]);
        page.motion.structure.shown = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::test_cx::checkout_test_cx;

    fn register(vm: &mut ScriptVm) {
        let _ = <crate::StoryApp as AppMain>::script_mod(vm);
    }

    /// A scene whose nodes write every kind of parameter row: numbers, an
    /// expression (a computed row), a name defined by a `let`, a colour, a
    /// text and a choice with no choices (a plain row).
    const DOC: &str = r#"use mod.motion.*
use mod.post; let BLOOM = 0.7
Motion{
    label: "open"
    duration: 4.0
    post: [
        post.Aberration{falloff: 2 amount: fn(t) { 1.0 + t }}
        post.Bloom{strength: BLOOM threshold: 0.82}
        post.Pass{at: @hdr reads: [@color] uniforms: {flash: 0.4} pixel: fn() -> vec4 { return self.color.sample(self.uv()) }}
    ]
    layers: [
        Rect{name: "bg" size: vec2(100, 100) fill: #ff0000 opacity: keys([0, 0], [1, 1])}
        Text{name: "title" text: "Hi" size: 40}
    ]
}
"#;

    /// The parameters' program evaluates in the panel's Splash isolate as
    /// written: every name it uses (the palette's colours, the panel's
    /// controls) is one the isolate has, so no node's panel logs a script
    /// error.
    #[test]
    fn every_nodes_parameters_program_evaluates_without_errors() {
        let root = structure::read(DOC).expect("the scene reads");
        let mut cx = checkout_test_cx(register);
        let splash = cx.with_vm(|vm| {
            let value = script_eval!(vm, {
                use mod.prelude.widgets.*
                Splash{}
            });
            WidgetRef::script_from_value(vm, value)
        });
        // The isolate binds no bare `stage`: a row naming it errs, so the
        // check below sees what the panel would log.
        splash.as_splash().set_grants(HostGrants::CLIPBOARD);
        splash.as_splash().set_text(&mut cx, "Label{text: \"x\" draw_text +: {color: stage.label}}");
        assert!(!splash.as_splash().last_errors().is_empty(), "a body naming a bare `stage` reports its error");
        let mut checked = 0;
        for node in structure::flatten(&root) {
            let body = params_body(DOC, node);
            if body.is_empty() {
                continue;
            }
            splash.as_splash().set_grants(HostGrants::CLIPBOARD);
            splash.as_splash().set_text(&mut cx, &body);
            assert_eq!(splash.as_splash().last_errors(), Vec::<String>::new(), "{}'s parameters:\n{body}", node.path);
            checked += 1;
        }
        assert!(checked >= 4, "the scene's nodes have parameter programs ({checked})");
        // A layer's slider spans the range its reader keeps the field in.
        let bg = structure::flatten(&root).into_iter().find(|n| n.label == "bg").expect("the bg layer");
        assert!(params_body(DOC, bg).contains("PanelSlider{path: \"Motion.layers[0].opacity\" label: \"opacity\" min: 0 max: 1}"), "{}", params_body(DOC, bg));
    }
}

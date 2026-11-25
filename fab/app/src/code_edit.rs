//! The code panel's file tabs and the selected object's feature tree. The model worker
//! supplies committed text; edits return as previews and one undo step per typing burst.
//!
//! Typing previews at once and commits after it rests ([`REST`]); a value
//! control's gesture previews each step and commits on its last. The tree's
//! moves and deletes are program texts too: one path for every edit of the
//! code.

use fab::ui::code::{FeatureRow, SourceState, SourceTab, SETTINGS_KIND};
use fab_model::program::{part_id, Compiled, Instance};
use fab_model::{Change, Edit};
use makepad_model_script::lang::protos::{self, Group, PROTOS};
use std::sync::Arc;

/// Seconds typing rests before the text commits.
pub const REST: f64 = 0.6;

/// Update file metadata once per model answer, never in the draw loop. Preserve drafts
/// across tab switches and commits of other files.
pub fn tabs(state: &mut SourceState, files: &fab_ai::SourceFiles, compiled: Option<&Compiled>, params: &[Arc<fab::params::PartParamsView>]) {
    let mut instances = std::collections::BTreeMap::new();
    if let Some(compiled) = compiled {
        for inst in &compiled.instances {
            let shown = instances.entry(inst.script.file.as_str()).or_insert(inst);
            if inst.name == state.instance {
                *shown = inst;
            }
        }
    }
    let mut sites: std::collections::BTreeMap<_, Vec<_>> = std::collections::BTreeMap::new();
    for part in params {
        let Some((instance, _)) = part.object.split_once('/') else { continue };
        sites.entry(instance).or_default().extend(part.params.iter().filter_map(|p| p.site.clone().map(|r| (r, p.key.clone()))));
    }
    let mut old: std::collections::BTreeMap<_, _> = std::mem::take(&mut state.tabs).into_iter().map(|t| (t.file.clone(), t)).collect();
    state.tabs = files.tabs.iter().map(|(file, name)| {
        let mut tab = old.remove(file).unwrap_or_else(|| SourceTab { file: file.clone(), ..Default::default() });
        tab.name = name.clone();
        if let Some(text) = files.texts.get(file).filter(|text| text.as_str() != tab.text.as_str()) {
            tab.text = Arc::new(text.clone());
        }
        let inst = instances.get(file.as_str());
        tab.features = inst.map_or_else(|| tab.text.lines().count(), |i| i.script.program.solids.iter().filter(|s| s.span.is_some()).count());
        tab.sites = inst.and_then(|i| sites.remove(i.name.as_str())).unwrap_or_default();
        tab
    }).collect();
}

/// The instance the code panel's script `file` shows (its first placement).
pub fn instance_of<'c>(c: &'c Compiled, file: &str) -> Option<&'c Instance> {
    c.instances.iter().find(|i| i.script.file == file)
}

/// The component tree's rows, in order: the script's top solids (what it emits and the lets
/// nothing uses) in source order, each followed by its operands one level deeper (a boolean's
/// base, then what it cuts away or joins). The solids of one loop (named after its `let`) are one row with its
/// instances under it (read-only: an edit goes to the loop), the solids a function call
/// makes are one folded row; its object is the plate part of the instance shown
/// (`instance/solid`).
pub fn rows(inst: &Instance) -> Vec<FeatureRow> {
    let p = &inst.script.program;
    let src = inst.script.source();
    // what the script emits and paints (a study's regions stay free to toggle)
    let roots = p.roots.iter().filter(|r| !fab_model::study::CALLS.contains(&r.kind)).flat_map(|r| r.args.iter()).flat_map(|(_, a)| match a {
        makepad_model_script::lang::Arg::Solid(i) => vec![*i],
        makepad_model_script::lang::Arg::Solids(ids) => ids.clone(),
        _ => Vec::new(),
    }).collect();
    // the areas `m.no_trees` keeps the trees out of, and those `m.settings` sets print settings in
    let top = |i: &usize| p.solids[*i].parent.is_none();
    let no_trees = fab_model::program::no_trees_solids(p).into_iter().filter(top).collect();
    let settings = fab_model::program::settings_solids(p).into_iter().filter(top).collect();
    let tree = Tree { inst, roots, no_trees, settings, line_of: &|at: usize| src[..at.min(src.len())].bytes().filter(|b| *b == b'\n').count() };
    let start = |i: &usize| p.solids[*i].span.as_ref().map_or(usize::MAX, |r| r.start);
    let mut tops: Vec<usize> = (0..p.solids.len()).filter(|i| p.solids[*i].parent.is_none()).collect();
    tops.sort_by_key(start);
    // A function call's solids are named after its `let` (`left.body`): one folded node.
    let call_of = |i: usize| -> Option<&str> {
        let (head, _) = p.solids[i].id.split_once('.')?;
        let head = head.split('[').next().unwrap_or(head);
        (p.solid(head).is_none() && p.names.syntax.find_let(head).is_some()).then_some(head)
    };
    let mut out = Vec::new();
    let mut done = std::collections::BTreeSet::new();
    let mut k = 0;
    while k < tops.len() {
        let i = tops[k];
        match call_of(i) {
            Some(head) if done.insert(head.to_string()) => {
                let members: Vec<usize> = tops.iter().copied().filter(|j| call_of(*j) == Some(head)).collect();
                let range = p.names.syntax.find_let(head).and_then(|loc| p.names.syntax.node(&loc)).map_or(0..0, |n| n.span.clone());
                out.push(FeatureRow { id: head.to_string(), op: "call".into(), line: (tree.line_of)(range.start), range, depth: 0, editable: true, group: true, ..Default::default() });
                let at = out.len() - 1;
                tree.list(&members, 1, &mut out);
                out[at].children = out[at + 1..].iter().filter(|r| r.depth == 1).count();
                out[at].detail = format!("{} solid{}", members.len(), if members.len() == 1 { "" } else { "s" });
            }
            Some(_) => {}
            None => {
                let start = k;
                while k < tops.len() && call_of(tops[k]).is_none() { k += 1; }
                tree.list(&tops[start..k], 0, &mut out);
                continue;
            }
        }
        k += 1;
    }
    let mut parents: Vec<String> = Vec::new();
    for row in &mut out {
        parents.truncate(row.depth);
        row.parent = parents.last().cloned();
        parents.push(row.id.clone());
    }
    // An operand's role can change where its boolean is a plain union, subtract or intersect
    // (a loop's row: its instances share one parent). O(rows).
    let first_member = |out: &[FeatureRow], k: usize| -> Option<String> {
        if !out[k].group { return Some(out[k].id.clone()); }
        out[k + 1..].iter().take_while(|r| r.depth > out[k].depth).find(|r| r.depth == out[k].depth + 1).map(|r| r.id.clone())
    };
    for k in 0..out.len() {
        if out[k].role.is_empty() || !out[k].editable { continue; }
        let parent = first_member(&out, k).and_then(|id| solid_index(p, &id)).and_then(|i| p.solids[i].parent);
        out[k].can_role = parent.is_some_and(|j| family(p.solids[j].proto).is_some());
    }
    study_rows(inst, &tree, &mut out);
    out
}

fn solid_index(p: &makepad_model_script::lang::Program, id: &str) -> Option<usize> {
    p.solids.iter().position(|s| s.id == id)
}

/// The booleans a role change rewrites: the plain ones (`m.union(a, b)`, `m.subtract(a, b)`,
/// `m.intersect(a, b)`) and the smooth ones of a distance field (`m.blend([a b ..], radius)`,
/// `m.carve(a, b, radius)`; none intersects). Its family (smooth or not) and the role its
/// operands after the first take.
fn family(proto: &str) -> Option<(bool, &'static str)> {
    match proto {
        "Union" | "BrepUnion" => Some((false, "+")),
        "Difference" | "BrepDifference" => Some((false, "-")),
        "Intersect" | "BrepIntersect" => Some((false, "&")),
        "Blend" => Some((true, "+")),
        "Carve" => Some((true, "-")),
        _ => None,
    }
}

/// How a call of the family takes its operands.
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    /// `call(a, b)`
    Ab,
    /// `call(a, b, radius)`
    AbR,
    /// `call([a b ..], radius)`
    OfR,
}

/// The call a role is written with in a family, and its shape.
fn role_call(smooth: bool, role: &str) -> Option<(&'static str, Shape)> {
    match (smooth, role) {
        (false, "+") => Some(("union", Shape::Ab)),
        (false, "-") => Some(("subtract", Shape::Ab)),
        (false, "&") => Some(("intersect", Shape::Ab)),
        (true, "+") => Some(("blend", Shape::OfR)),
        (true, "-") => Some(("carve", Shape::AbR)),
        _ => None,
    }
}

/// A call of `shape` on `base` and `ops` (one, or a list), its radius `extra` when it takes one.
fn call_code(prefix: &str, call: &str, shape: Shape, base: &str, ops: &[&str], extra: &str) -> String {
    let list = if ops.len() == 1 { ops[0].to_string() } else { format!("[{}]", ops.join(" ")) };
    match shape {
        Shape::Ab => format!("{prefix}{call}({base}, {list})"),
        Shape::AbR => format!("{prefix}{call}({base}, {list}, {extra})"),
        Shape::OfR => format!("{prefix}{call}([{base} {}], {extra})", ops.join(" ")),
    }
}

/// The top-level arguments of a call's text (`m.subtract(body, [a b])` -> `body`, `[a b]`),
/// or none when the brackets do not balance.
fn call_args(text: &str, open: usize) -> Option<Vec<&str>> {
    let inner = text.get(open + 1..text.len().checked_sub(1)?)?;
    if !text.ends_with(')') { return None; }
    let (mut depth, mut start, mut args) = (0i32, 0, Vec::new());
    for (i, ch) in inner.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => { args.push(inner[start..i].trim()); start = i + 1; }
            _ => {}
        }
        if depth < 0 { return None; }
    }
    args.push(inner[start..].trim());
    (depth == 0).then_some(args)
}

/// The edit that makes operand `id` (a solid, or a loop's row) of its boolean joined (`+`),
/// cut away (`-`) or intersected (`&`), the operands applying in the same order: the boolean
/// splits there, its own call on the base and the operands before, then the new role's call
/// with this operand, then its own call on that and the operands after (`m.subtract(body,
/// [holes bores])`, holes joined: `m.subtract(m.union(body, holes), bores)`; a smooth one keeps
/// its radius on every part). One edit of the boolean's code; only a call written with its
/// operands (and radius) alone is rewritten.
pub fn role_changed(state: &SourceState, inst: &Instance, id: &str, role: &str) -> Result<Vec<Change>, String> {
    let p = &inst.script.program;
    let src = inst.script.source();
    let row = state.feature(id).ok_or("That feature is gone")?;
    // the solids the row stands for
    let members: Vec<usize> = if row.group {
        state.features.iter().filter(|f| f.parent.as_deref() == Some(id)).filter_map(|f| solid_index(p, &f.id)).collect()
    } else {
        solid_index(p, id).into_iter().collect()
    };
    let first = *members.first().ok_or("That feature has no solid")?;
    let pi = p.solids[first].parent.ok_or("It is no operand of a boolean")?;
    if members.iter().any(|m| p.solids[*m].parent != Some(pi)) {
        return Err("Its solids are operands of different booleans".into());
    }
    let parent = &p.solids[pi];
    let (smooth, own) = family(parent.proto).ok_or("Only the operands of a union, subtract, intersect, blend or carve change role")?;
    if own == role { return Ok(Vec::new()); }
    let (old, old_shape) = role_call(smooth, own).ok_or("This boolean's operands keep their roles")?;
    let (new, new_shape) = role_call(smooth, role).ok_or("A smooth boolean (blend, carve) has no intersect")?;
    let span = parent.span.clone().ok_or("The boolean is not written in the script")?;
    let text = src.get(span.clone()).ok_or("The boolean is not written in the script")?;
    let open = text.find('(').ok_or("The boolean's call is not written as a call")?;
    let prefix = text[..open].strip_suffix(old).ok_or("The boolean's call is written in a form this cannot rewrite")?;
    let args = call_args(text, open).ok_or("The boolean's call is written in a form this cannot rewrite")?;
    let want = match old_shape { Shape::Ab | Shape::OfR => 2, Shape::AbR => 3 };
    if args.len() != want {
        return Err("The boolean's call has more than its operands and radius: change it in the code".into());
    }
    let extra = if smooth { args[want - 1].to_string() } else { String::new() };
    // each operand as written: inline code its own text, a named solid (or a loop's) its name
    let code_of = |i: usize| -> String {
        let s = &p.solids[i];
        if let Some(r) = s.span.as_ref().filter(|r| r.start >= span.start && r.end <= span.end) {
            return src[r.clone()].to_string();
        }
        match s.id.split_once('[') {
            Some((head, _)) if p.names.syntax.find_let(head).is_some() => head.to_string(),
            _ => s.id.clone(),
        }
    };
    let (base, rest) = parent.children.split_first().ok_or("The boolean has no operands")?;
    if members.contains(base) { return Err("The base of a boolean has no role to change".into()); }
    // the operands after the base, a loop's instances (one name) as one
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for &c in rest {
        let code = code_of(c);
        match groups.last_mut() {
            Some((last, solids)) if *last == code => solids.push(c),
            _ => groups.push((code, vec![c])),
        }
    }
    let k = groups.iter().position(|(_, solids)| solids.contains(&first)).ok_or("It is no operand of its boolean")?;
    if members.iter().any(|m| !groups[k].1.contains(m)) || groups[k].1.len() != members.len() {
        return Err("Change the role of the whole loop, not one of its instances".into());
    }
    fn names(gs: &[(String, Vec<usize>)]) -> Vec<&str> {
        gs.iter().map(|g| g.0.as_str()).collect()
    }
    let base = code_of(*base);
    let left = if k == 0 { base } else { call_code(prefix, old, old_shape, &base, &names(&groups[..k]), &extra) };
    let mid = call_code(prefix, new, new_shape, &left, &[groups[k].0.as_str()], &extra);
    let code = if k + 1 < groups.len() { call_code(prefix, old, old_shape, &mid, &names(&groups[k + 1..]), &extra) } else { mid };
    Ok(vec![Change::Script { file: state.file.clone(), edit: Edit::Replace { solid: parent.id.clone(), code } }])
}

/// The force study's rows after the solids, in source order: one per `m.fixed` / `m.force` /
/// ... statement (a loop's once), its object the plate part whose study holds it, its detail
/// the value as written. They are no solids: no bypass, no moves.
fn study_rows(inst: &Instance, tree: &Tree, out: &mut Vec<FeatureRow>) {
    let script = &inst.script;
    let src = script.source();
    let mut seen = std::collections::HashSet::new();
    let mut loads: Vec<(&str, usize, &fab_model::study::Load)> = script
        .objects
        .iter()
        .filter_map(|o| script.study.get(&o.name).map(|s| (o, s)))
        .flat_map(|(o, s)| s.loads.iter().enumerate().map(move |(k, l)| (o.name.as_str(), k, l)))
        .filter(|(_, _, l)| seen.insert((l.span.as_ref().map(|r| (r.start, r.end)), l.loc)))
        .collect();
    loads.sort_by_key(|(_, _, l)| l.span.as_ref().map_or(usize::MAX, |r| r.start));
    let mut ids = std::collections::HashSet::new();
    for (object, index, l) in loads {
        let call = l.kind.call();
        // never a solid's id (those hold no spaces)
        let mut id = format!("{call} {}", l.on);
        let mut n = 1;
        while !ids.insert(id.clone()) {
            n += 1;
            id = format!("{call} {} #{n}", l.on);
        }
        let range = l.span.clone().unwrap_or(0..0);
        let unit = match l.kind {
            fab_model::study::Kind::Fixed => "",
            fab_model::study::Kind::Force(_) => " N",
            fab_model::study::Kind::Pressure(_) => " MPa",
            fab_model::study::Kind::Torque(_) => " N mm",
        };
        let detail = l.value_span.clone().map_or(String::new(), |r| format!("{}{unit}", &src[r]));
        out.push(FeatureRow {
            id,
            op: call.into(),
            object: part_id(&inst.name, object),
            line: (tree.line_of)(range.start),
            range,
            detail,
            study: Some(fab::ui::code::StudyRow { index, top: l.top, on: l.on.clone() }),
            ..Default::default()
        });
    }
}

/// The `let` a loop's solid is named after (`holes` of `holes[2]`).
fn loop_head<'p>(p: &'p makepad_model_script::lang::Program, i: usize) -> Option<&'p str> {
    let (head, _) = p.solids[i].id.split_once('[')?;
    p.names.syntax.find_let(head).is_some().then_some(head)
}

/// Positions in `list` from `k` sharing a loop's `let`, else the contiguous run with its span.
fn group_of(p: &makepad_model_script::lang::Program, list: &[usize], k: usize) -> Vec<usize> {
    if let Some(head) = loop_head(p, list[k]) {
        return (k..list.len()).filter(|j| loop_head(p, list[*j]) == Some(head)).collect();
    }
    let Some(span) = p.solids[list[k]].span.as_ref() else { return vec![k] };
    (k..list.len()).take_while(|j| p.solids[list[*j]].span.as_ref() == Some(span)).collect()
}

struct Tree<'a> {
    inst: &'a Instance,
    /// Emitted solids, collected once per tree rebuild.
    roots: std::collections::HashSet<usize>,
    /// The solids `m.no_trees` marks.
    no_trees: std::collections::HashSet<usize>,
    /// The solids `m.settings` sets print settings in.
    settings: std::collections::HashSet<usize>,
    line_of: &'a dyn Fn(usize) -> usize,
}

impl Tree<'_> {
    /// Sibling solids, a loop's instances as one node.
    fn list(&self, list: &[usize], depth: usize, out: &mut Vec<FeatureRow>) {
        let p = &self.inst.script.program;
        let mut taken = vec![false; list.len()];
        for k in 0..list.len() {
            if taken[k] { continue; }
            let group: Vec<_> = group_of(p, list, k).into_iter().filter(|j| !taken[*j]).collect();
            for j in &group { taken[*j] = true; }
            let n = group.len();
            if n > 1 {
                let s = &p.solids[list[k]];
                let head = loop_head(p, list[k]);
                let id = head.unwrap_or_else(|| s.id.split('[').next().unwrap_or(&s.id)).to_string();
                let range = head.and_then(|head| p.names.syntax.find_let(head)).and_then(|loc| p.names.syntax.node(&loc)).map_or(s.span.clone().unwrap_or(0..0), |n| n.span.clone());
                let op = if group.iter().all(|j| p.solids[list[*j]].proto == s.proto) { s.proto.to_lowercase() } else { "loop".into() };
                let off = group.iter().all(|j| p.solids[list[*j]].off);
                let root = group.iter().any(|j| self.roots.contains(&list[*j]));
                let object = part_id(&self.inst.name, &id);
                out.push(FeatureRow { id, object, op, line: (self.line_of)(range.start), range, depth, role: role(p, list[k]), detail: format!("×{n}"), children: n, editable: true, group: true, off, root, ..Default::default() });
                for j in group {
                    self.solid(list[j], depth + 1, false, out);
                }
            } else {
                self.solid(list[k], depth, true, out);
            }
        }
    }

    fn solid(&self, i: usize, depth: usize, editable: bool, out: &mut Vec<FeatureRow>) {
        let p = &self.inst.script.program;
        let s = &p.solids[i];
        let range = s.span.clone().unwrap_or(0..0);
        let detail = match s.proto {
            _ if self.no_trees.contains(&i) => fab::ui::code::NO_TREES.to_string(),
            _ if self.settings.contains(&i) => fab::ui::code::SETTINGS_HERE.to_string(),
            // The mesh it imports: the call's first argument, as written.
            "Import" => p.source()[range.clone()].split('"').nth(1).unwrap_or("").to_string(),
            _ => String::new(),
        };
        out.push(FeatureRow {
            id: s.id.clone(),
            // a box print settings are set in is that, whatever its shape
            op: if self.settings.contains(&i) { SETTINGS_KIND.to_string() } else { s.proto.to_lowercase() },
            object: part_id(&self.inst.name, &s.id),
            link: s.link.clone().unwrap_or_default(),
            line: (self.line_of)(range.start),
            editable: editable && s.span.is_some(),
            off: s.off,
            root: self.roots.contains(&i),
            range,
            depth,
            role: role(p, i),
            detail,
            ..Default::default()
        });
        let at = out.len() - 1;
        self.list(&s.children, depth + 1, out);
        out[at].children = out[at + 1..].iter().filter(|r| r.depth == depth + 1).count();
    }
}

/// How solid `i` takes part in the solid it is an operand of.
fn role(p: &makepad_model_script::lang::Program, i: usize) -> &'static str {
    let Some(parent) = p.solids[i].parent.map(|j| &p.solids[j]) else { return "" };
    if parent.children.first() == Some(&i) {
        return "";
    }
    match parent.proto {
        "Difference" | "Carve" | "BrepDifference" => "-",
        "Union" | "Blend" | "BrepUnion" => "+",
        "Intersect" | "BrepIntersect" => "&",
        _ => "",
    }
}

/// A new committed text of the script the panel shows and the project it builds. `own`: it
/// came from the panel's own commit (the panel keeps its text, cursor and undo); otherwise
/// the panel shows it again.
pub fn committed(state: &mut SourceState, text: Arc<String>, own: bool, compiled: Option<&Compiled>) {
    if let Some(c) = compiled {
        match c.instance(&state.instance).filter(|i| i.script.file == state.file).or_else(|| instance_of(c, &state.file)) {
            Some(inst) if inst.script.source() == text.as_str() => {
                // (a test print's row has no code: it stays)
                let tests: Vec<FeatureRow> = state.features.iter().filter(|row| row.detail == fab::ui::code::TEST_PRINT).cloned().collect();
                state.features = rows(inst);
                state.features.extend(tests);
                for row in state.features.iter().filter(|row| row.group) {
                    if state.seen.insert(row.id.clone()) && row.op == "call" {
                        state.collapsed.insert(row.id.clone());
                    }
                }
                state.instance = inst.name.clone();
            }
            Some(_) => {}
            // The script is no longer placed (an undo took it off): no rows of it stay.
            None => {
                state.features.clear();
                state.instance.clear();
            }
        }
    }
    let ids: std::collections::BTreeSet<_> = state.features.iter().map(|row| row.id.as_str()).collect();
    state.collapsed.retain(|id| ids.contains(id.as_str()));
    state.seen.retain(|id| ids.contains(id.as_str()));
    state.text = text;
    if state.selected.as_ref().is_some_and(|id| state.feature(id).is_none()) {
        state.selected = None;
    }
    if !own {
        state.rev += 1;
        state.problem = None;
        state.editing = false;
    }
}

/// The sibling before (`up`) or after row `id`: the slot an arrow button moves it to.
pub fn sibling(state: &SourceState, id: &str, up: bool) -> Option<String> {
    let row = state.feature(id)?;
    let siblings: Vec<_> = state.features.iter().filter(|f| f.parent == row.parent).collect();
    let i = siblings.iter().position(|f| f.id == id)?;
    let j = if up { i.checked_sub(1)? } else { i + 1 };
    siblings.get(j).map(|f| f.id.clone())
}

/// A drag and the arrow buttons share the structured sibling move.
pub fn reordered(state: &SourceState, id: &str, to: &str) -> Option<Vec<Change>> {
    let (row, target) = (state.feature(id)?, state.feature(to)?);
    if id == to || !row.editable || !target.editable || row.parent != target.parent {
        return None;
    }
    Some(vec![Change::Script { file: state.file.clone(), edit: Edit::Move { solid: id.into(), to: to.into() } }])
}

/// The changes that remove solid `id` from the script shown: its statement or written code and
/// every use of it, one edit ([`makepad_model_script::lang::edit::remove`]).
pub fn removed(state: &SourceState, id: &str) -> Option<Vec<Change>> {
    let row = state.feature(id)?;
    let file = state.file.clone();
    // a fixture or load: its statement, with its line when it stands alone on it
    if let Some(s) = &row.study {
        if !s.top || row.range.end > state.text.len() {
            return None;
        }
        let range = crate::study_edit::whole_line(&state.text, &row.range);
        return Some(vec![Change::Script { file, edit: Edit::Splice { range, text: String::new() } }]);
    }
    Some(vec![Change::Script { file, edit: Edit::Remove { solid: id.into() } }])
}

/// The plate part a feature row picks with it (its handles show, as a pick in the view's): its
/// object's, or that of the nearest row above it that is a plate part's, else the first plate
/// part of the shown script's (a solid no object emits, as a no-trees area).
pub fn part_of_row(state: &SourceState, parts: &[fab::api::PlatePart], row: &FeatureRow) -> Option<usize> {
    let mut at = Some(row);
    while let Some(r) = at {
        if let Some(i) = parts.iter().position(|p| p.object == r.object) {
            return Some(i);
        }
        at = r.parent.as_deref().and_then(|id| state.feature(id));
    }
    let prefix = format!("{}/", state.instance);
    parts.iter().position(|p| p.object.starts_with(&prefix))
}

/// The parameter a value under the mouse belongs to: its own key, or the list it is an
/// item of (`plate.size.1` -> `plate.size`), if the parameter model has one.
pub fn param_key(params: &fab::params::ParamState, key: &str) -> Option<String> {
    let has = |k: &str| params.parts.iter().any(|p| p.param(k).is_some());
    if has(key) {
        return Some(key.to_string());
    }
    let (list, last) = key.rsplit_once('.')?;
    (last.parse::<usize>().is_ok() && has(list)).then(|| list.to_string())
}

/// The Add menu's primitives (as plate parts) and the vocabulary by its groups.
const PRIMITIVES: [(&str, &str); 5] = [("box", "Box"), ("cylinder", "Cylinder"), ("sphere", "Sphere"), ("cone", "Cone"), ("torus", "Torus")];

const MENU_GROUPS: [(Group, &str); 10] = [(Group::Generator, "From a sketch"), (Group::Brep, "Exact (B-rep)"), (Group::Combine, "Combine"), (Group::Modify, "Modify"), (Group::Mechanical, "Mechanical"), (Group::Text, "Text"), (Group::Field, "Field"), (Group::Joint, "Joint"), (Group::Weave, "Weave"), (Group::Lighten, "Lighten")];

/// The Design workspace's Add menu: a flyout per group of the vocabulary, each type by its name.
pub fn add_menu() -> Vec<fab::ui::popover::MenuItem> {
    use fab::ui::popover::MenuItem;
    use makepad_widgets::LiveId;
    let mut menu = vec![MenuItem::new(LiveId::from_str("add_group_primitive"), "Primitives")
        .flyout(PRIMITIVES.iter().map(|(shape, label)| MenuItem::new(LiveId::from_str(&format!("add_shape_{shape}")), label)).collect())];
    for (g, label) in MENU_GROUPS {
        let items: Vec<MenuItem> = PROTOS.iter().filter(|p| p.group == g).map(|p| MenuItem::new(LiveId::from_str(&format!("add_op_{}", p.name)), p.name)).collect();
        if !items.is_empty() {
            menu.push(MenuItem::new(LiveId::from_str(&format!("add_group_{}", g.name())), label).flyout(items));
        }
    }
    menu
}

/// What an Add menu pick adds: a primitive part, or a solid of the vocabulary.
pub enum AddPick {
    Shape(String),
    Op(&'static str),
}

pub fn add_pick(id: makepad_widgets::LiveId) -> Option<AddPick> {
    use makepad_widgets::LiveId;
    if let Some((shape, _)) = PRIMITIVES.iter().find(|(s, _)| LiveId::from_str(&format!("add_shape_{s}")) == id) {
        return Some(AddPick::Shape(shape.to_string()));
    }
    PROTOS.iter().find(|p| LiveId::from_str(&format!("add_op_{}", p.name)) == id).map(|p| AddPick::Op(p.name))
}

const SHAPE_EDITS: [(&str, &str, fab::api::ShapeOp); 9] = {
    use fab::api::{ShapeOp, ShapePrim};
    [
        ("shape_subtract_box", "Subtract box", ShapeOp::Subtract(ShapePrim::Box)),
        ("shape_subtract_cylinder", "Subtract cylinder", ShapeOp::Subtract(ShapePrim::Cylinder)),
        ("shape_subtract_sphere", "Subtract sphere", ShapeOp::Subtract(ShapePrim::Sphere)),
        ("shape_add_box", "Add box to part", ShapeOp::Add(ShapePrim::Box)),
        ("shape_add_cylinder", "Add cylinder to part", ShapeOp::Add(ShapePrim::Cylinder)),
        ("shape_add_sphere", "Add sphere to part", ShapeOp::Add(ShapePrim::Sphere)),
        ("shape_cut_bottom", "Cut bottom", ShapeOp::CutBottom),
        ("shape_cut_plane", "Cut plane", ShapeOp::CutPlane),
        ("shape_split", "Keep both halves, add pins", ShapeOp::Split),
    ]
};

/// The shape edits on the picked node.
pub fn shape_menu() -> Vec<fab::ui::popover::MenuItem> {
    SHAPE_EDITS.iter().map(|(id, label, _)| fab::ui::popover::MenuItem::new(makepad_widgets::LiveId::from_str(id), label)).collect()
}

/// The operation of a Shape menu pick.
pub fn shape_pick(id: makepad_widgets::LiveId) -> Option<fab::api::ShapeOp> {
    SHAPE_EDITS.iter().find(|(key, _, _)| makepad_widgets::LiveId::from_str(key) == id).map(|(_, _, op)| *op)
}

/// The Shape menu's Drape: a mail square draped over a sphere below it.
const DRAPE_EXAMPLE: &str = "m.drape(m.mail(Rect{size: [60mm 60mm]}), m.sphere({radius: 50mm at: [0, -30mm, 0]}))";
/// The Shape menu's Form: [`DRAPE_EXAMPLE`] made a solid.
const FORM_EXAMPLE: &str = "m.form(m.drape(m.mail(Rect{size: [60mm 60mm]}), m.sphere({radius: 50mm at: [0, -30mm, 0]})))";

/// The changes that add type `name`'s example as a new solid of the instance's script, its
/// `source` the `selected` solid (`instance/solid`) for a modifier (which then is emitted
/// instead of it), and the new solid's plate part id.
pub fn with_op(inst: &Instance, name: &str, selected: Option<&str>) -> Option<(Vec<Change>, String)> {
    let p = protos::proto(name)?;
    let prog = &inst.script.program;
    let base = name.to_lowercase();
    let taken = |n: &str| prog.solids.iter().any(|s| s.id == n) || prog.names.syntax.find_let(n).is_some();
    let id = (1..).map(|i| format!("{base}{i}")).find(|n| !taken(n)).unwrap();
    let example = match name {
        "Links" | "Mail" => p.example.split_once(" = ")?.1,
        "Drape" => DRAPE_EXAMPLE,
        "Form" => FORM_EXAMPLE,
        _ => p.example,
    };
    let mut edits = vec![Edit::Add { code: format!("let {id} = {example}"), before: None }];
    let local = selected.and_then(|s| s.strip_prefix(&format!("{}/", inst.name))).filter(|_| p.field("source").is_some());
    if let Some(src) = local {
        edits.push(Edit::Field { solid: id.clone(), field: "source".into(), code: src.to_string() });
        if inst.script.object(src).is_some() {
            edits.push(Edit::Root { kind: "object".into(), solid: src.to_string(), add: false });
        }
    }
    if p.field("source").is_none() || local.is_some() {
        edits.push(Edit::Add { code: format!("object({id})"), before: None });
    }
    let file = inst.script.file.clone();
    Some((edits.into_iter().map(|edit| Change::Script { file: file.clone(), edit }).collect(), part_id(&inst.name, &id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Drape and Form examples' sphere sits below the mail: `at` is three items (the
    /// commas keep `-30mm` one of its own), and the parser has nothing to warn of.
    #[test]
    fn drape_examples_parse_to_the_items_they_show() {
        for example in [DRAPE_EXAMPLE, FORM_EXAMPLE] {
            assert!(makepad_script::parser::parse_warnings(example).is_empty(), "{example}");
            let tree = makepad_script::syntax::parse(example).unwrap();
            let mut counts = Vec::new();
            tree.walk(&mut |_, node| {
                if let makepad_script::syntax::Kind::Array { items, .. } = &node.kind {
                    counts.push(items.len());
                }
            });
            assert_eq!(counts, [2, 3], "{example}");
        }
    }

    /// The tree's delete of a cutter its carve still uses takes it out of the carve's list, and
    /// of the last cutter turns the carve into its base: each one undo step, and the plate
    /// rebuilds with the object on it, under its name.
    #[test]
    fn the_tree_deletes_a_cutter_its_carve_uses_and_the_plate_rebuilds() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let empty = fab_model::program::EMPTY;
        let source = format!("{empty}let body = m.sphere({{radius: 10mm}})\nlet mouth = m.cylinder({{radius: 3mm height: 30mm}})\nlet cut1 = m.box({{size: [8mm 8mm 8mm] at: [-9mm 0 0]}})\nlet vase = m.carve(body, [mouth cut1], 0)\nobject(vase)\n");
        let (reply, _) = session.set_source("t.cad.splash", &source);
        assert!(reply.ok, "{}", reply.text);
        let delete = |session: &mut fab_ai::Session, id: &str| {
            let c = session.compiled.clone().unwrap();
            let text = session.source("t.cad.splash").unwrap();
            let state = SourceState { file: "t.cad.splash".into(), text: Arc::new(text), features: rows(instance_of(&c, "t.cad.splash").unwrap()), ..Default::default() };
            let reply = session.commit("remove", removed(&state, id).expect("a solid's row is removable"));
            assert!(reply.ok, "deleting '{id}': {}", reply.text);
            let ev = session.last.clone().unwrap();
            assert_eq!(ev.plate.parts.iter().map(|p| p.object.as_str()).collect::<Vec<_>>(), ["t/vase"], "the vase stays on the plate");
            session.source("t.cad.splash").unwrap()
        };
        let one = delete(&mut session, "cut1");
        assert_eq!(one, format!("{empty}let body = m.sphere({{radius: 10mm}})\nlet mouth = m.cylinder({{radius: 3mm height: 30mm}})\nlet vase = m.carve(body, [mouth], 0)\nobject(vase)\n"));
        let none = delete(&mut session, "mouth");
        assert_eq!(none, format!("{empty}let vase = m.sphere({{radius: 10mm}})\nobject(vase)\n"));
        let reply = session.undo_redo(fab_model::HistoryAction::Undo);
        assert!(reply.ok, "{}", reply.text);
        assert_eq!(session.source("t.cad.splash").unwrap(), one, "one undo step brings the mouth back");
    }

    #[test]
    fn a_linked_solid_s_row_carries_its_key() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let empty = fab_model::program::EMPTY;
        let source = format!("{empty}let a = m.link(m.box({{size: [10mm 10mm 10mm]}}), \"elec:port:0:3:0\")\nlet b = m.subtract(m.box({{size: [20mm 20mm 20mm]}}), a)\nobject(b, name: \"b\")\n");
        let (reply, _) = session.set_source("t.cad.splash", &source);
        assert!(reply.ok, "{}", reply.text);
        let c = session.compiled.clone().unwrap();
        let state = SourceState { file: "t.cad.splash".into(), instance: "t".into(), features: rows(instance_of(&c, "t.cad.splash").unwrap()), ..Default::default() };
        let a = state.feature("a").expect("the linked operand has a row");
        assert_eq!(a.parent.as_deref(), Some("b"), "the linked operand is under b");
        assert_eq!(a.link, "elec:port:0:3:0", "the operand's row keeps its host key");
        assert!(state.feature("b").expect("the emitted solid has a row").link.is_empty(), "b does not inherit its operand's host key");
    }

    /// A row picks its part with it, as a pick in the view does: the object's own row, a cutter
    /// under it, and a no-trees area no object emits all pick the vase; another script's part
    /// is not theirs.
    #[test]
    fn a_feature_row_picks_its_part() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let empty = fab_model::program::EMPTY;
        let source = format!("{empty}let body = m.sphere({{radius: 10mm}})\nlet cut1 = m.box({{size: [8mm 8mm 8mm] at: [-9mm 0 0]}})\nlet vase = m.carve(body, [cut1], 0)\nobject(vase)\nlet no_trees1 = m.box({{size: [5mm 5mm 5mm] at: [20mm 2.5mm 0]}})\nm.no_trees(no_trees1)\n");
        let (reply, _) = session.set_source("t.cad.splash", &source);
        assert!(reply.ok, "{}", reply.text);
        let c = session.compiled.clone().unwrap();
        let state = SourceState { file: "t.cad.splash".into(), instance: "t".into(), features: rows(instance_of(&c, "t.cad.splash").unwrap()), ..Default::default() };
        let part = |object: &str| fab::api::PlatePart { object: object.into(), ..Default::default() };
        let parts = [part("u/cup"), part("t/vase")];
        for id in ["vase", "cut1", "no_trees1"] {
            let row = state.feature(id).unwrap_or_else(|| panic!("a row for {id}: {:?}", state.features.iter().map(|f| &f.id).collect::<Vec<_>>()));
            assert_eq!(part_of_row(&state, &parts, row), Some(1), "{id} picks the vase");
        }
        assert_eq!(part_of_row(&state, &[part("u/cup")], state.feature("cut1").unwrap()), None, "not another script's part");
    }
}

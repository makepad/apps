//! Host-built parts and the bodies that take their additions and cuts.

use super::*;

/// Builds a solid directly in the program's graph, in model coordinates.
pub type GenSolid = Arc<dyn Fn(&mut csg_graph::Graph) -> Result<csg_graph::NodeId, String> + Send + Sync>;

#[derive(Clone)]
pub struct GenPart {
    pub solid: GenSolid,
    pub adds: Vec<GenSolid>,
    pub cuts: Vec<GenSolid>,
    /// Keys of this panel's own cuts that these generated cuts take over.
    /// Only cuts linked with a claimed key are omitted; unkeyed cuts always apply.
    pub claims: Vec<String>,
}

impl std::fmt::Debug for GenPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenPart").field("adds", &self.adds.len()).field("cuts", &self.cuts.len()).field("claims", &self.claims).finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct PanelReq {
    pub name: String,
    /// Index into `Program::solids`, before the panel's claimable cuts or generated edits.
    pub base: usize,
    pub pose: V,
}

#[derive(Clone, Debug)]
pub struct GenRequest {
    pub part: String,
    pub kind: String,
    /// Canonical Splash, with the panel handle replaced by its name.
    pub params: String,
    pub panel: Option<String>,
}

impl V {
    /// Canonical Splash for a plain value. Numbers retain VM units (metres/radians),
    /// object fields are sorted, and functions and opaque handles are rejected.
    pub fn splash(&self) -> Result<String, String> {
        Ok(match self {
            V::Nil => "nil".into(),
            V::Bool(b) => b.to_string(),
            V::Num(n) if n.is_finite() => n.to_string(),
            V::Str(s) => format!("{s:?}"),
            V::Id(s) => format!("@{s}"),
            V::Color(c) => format!("#{c:08x}"),
            V::Arr(a) => format!("[{}]", a.iter().map(V::splash).collect::<Result<Vec<_>, _>>()?.join(", ")),
            V::Obj(o) if o.part.is_none() => {
                let mut fields = o.fields.iter().collect::<Vec<_>>();
                fields.sort_by(|a, b| a.0.cmp(&b.0));
                let mut values = fields.into_iter().map(|(k, v)| {
                    let key = if k.chars().next().is_some_and(|c| c == '_' || c.is_alphabetic()) && k.chars().all(|c| c == '_' || c.is_alphanumeric()) { k.clone() } else { format!("{k:?}") };
                    Ok(format!("{key}: {}", v.splash()?))
                }).collect::<Result<Vec<_>, String>>()?;
                values.extend(o.items.iter().map(V::splash).collect::<Result<Vec<_>, _>>()?);
                format!("{}{{{}}}", o.proto.as_deref().unwrap_or(""), values.join(", "))
            }
            _ => return Err(format!("expected a plain value, got {}", self.describe())),
        })
    }
}

impl Lower<'_, '_, '_> {
    fn generated_id(&self, obj: &Obj) -> String {
        match obj.part.and_then(|part| self.part_names.get(part)) {
            Some(id) if !id.is_empty() => id.clone(),
            _ => self.line_name(protos::proto(obj.proto.as_deref().unwrap_or("")).map_or("part", |p| p.call), obj.loc),
        }
    }

    fn panel_name(&self, obj: &Obj) -> Res<String> {
        let f = self.fields(obj)?;
        if f.get("name").is_some() { f.text("name") } else { Ok(self.generated_id(obj)) }
    }

    /// Collect every request before lowering any panel: its generated parts are normally
    /// declared later than the panel, but their cuts belong in its first graph node.
    /// A generated part's id says where it was made, so state stored against it stays on it
    /// while other repeats of its call come and go. A call outside any fn and loop is its plain
    /// name (`generated_base`), a line name the naming moment writes into its object call before
    /// a line above can move it. A part made in a fn or a loop cannot be named that way (its call
    /// is every repeat's), so its id carries no line numbers: its call's ordinal among the
    /// script's generated calls, each script call site it was made through as its callee and
    /// ordinal (outermost first), and its `for` loop positions (`generated#0 at posts#1[2]`: the
    /// first generated call, through the second call of `posts`, third iteration). Only adding
    /// or removing a call of the same fn, or a generated call, ahead of it shifts it, as removing
    /// an earlier loop element shifts a loop position. Parts alike in all of this (a `while`
    /// loop records no position) are numbered in creation order (`#2`).
    pub(super) fn prepare_generated(&mut self, parts: &[vm::PartCall]) -> Res<()> {
        let mut used = std::collections::HashSet::new();
        let calls = parts.iter().any(|p| p.call == "generated" && !pinned(p)).then(|| Calls::new(self.names));
        for (part, call) in parts.iter().enumerate().filter(|(_, p)| p.call == "generated") {
            let v = self.reader.part(part);
            let Some(obj) = v.obj().filter(|o| o.proto.as_deref() == Some("Generated")) else { continue };
            let f = self.fields(obj)?;
            let kind = f.text("kind")?;
            let mut panel = None;
            let params = match f.get("params") {
                None => "{}".to_string(),
                Some(V::Obj(params)) if params.part.is_none() && params.proto.is_none() => {
                    let mut fields = params.fields.clone();
                    if let Some((_, value)) = fields.iter_mut().find(|(k, _)| k == "panel") {
                        let Some(p) = value.obj().filter(|p| p.part.is_some() && p.proto.as_deref() == Some("Panel")) else {
                            return f.err("'panel' must be a panel part made by m.panel");
                        };
                        let name = self.panel_name(p)?;
                        *value = V::Str(name.clone());
                        panel = Some(name);
                    }
                    V::Obj(Arc::new(Obj { ident: params.ident, part: None, proto: None, fields, items: params.items.clone(), children: params.children.clone(), loc: params.loc })).splash().or_else(|e| f.err(e))?
                }
                Some(_) => return f.err("parameters must be a plain object"),
            };
            let natural = match &calls {
                Some(calls) if !pinned(call) => calls.made_at(self.names, self.generated_base(part, call, obj, Some(calls)), call),
                _ => self.generated_base(part, call, obj, None),
            };
            let (mut id, mut n) = (natural.clone(), 1);
            while !used.insert(id.clone()) {
                n += 1;
                id = format!("{natural}#{n}");
            }
            self.gen_ids.insert(part, id.clone());
            self.gen_requests.push(GenRequest { part: id, kind, params, panel });
        }
        Ok(())
    }

    /// A generated part's name, before where it was made: the `let` its own call is bound by;
    /// else, for a call outside fns and loops, the name its root gives it or its line name; else
    /// its call's ordinal among the script's generated calls (a call in a library: `generated`).
    fn generated_base(&self, part: usize, call: &vm::PartCall, obj: &Obj, calls: Option<&Calls>) -> String {
        if let Some(site) = self.names.site(call.loc).filter(|s| !s.binds.is_empty()) {
            return site.binds.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(".");
        }
        if let Some(calls) = calls {
            return calls.at(self.names, call.loc).map_or_else(|| call.call.to_string(), |c| c.builder.clone());
        }
        match self.part_names.get(part) {
            Some(name) if !name.is_empty() && super::part_binds(self.names, call).is_empty() => name.clone(),
            _ => self.line_name(call.call, obj.loc),
        }
    }

    pub(super) fn generated_part(&mut self, obj: &Obj, id: &str) -> Res<NodeId> {
        let f = self.fields(obj)?;
        match self.generated.get(id).cloned() {
            Some(part) => (part.solid)(&mut self.graph).or_else(|e| f.err(e)),
            None => self.push(&f, Op::Mesh { mesh: Arc::new(csg_graph::TriMesh::new()) }),
        }
    }

    pub(super) fn panel(&mut self, obj: &Obj, id: &str, index: usize) -> Res<NodeId> {
        let f = self.fields(obj)?;
        let name = self.panel_name(obj)?;
        let base = self.solid(f.need("base")?)?;
        if self.solids[index].frame.is_none() {
            self.solids[index].frame = self.solids[base].frame.clone();
        }
        let mut panel_cuts = Vec::new();
        if let Some(values) = f.get("cuts") {
            for (n, value) in flat(values).into_iter().enumerate() {
                let cut = self.solid(value)?;
                let node = self.child(value, format!("{id}.cuts.{n}"), index)?;
                panel_cuts.push((cut, node));
            }
        }
        let targets: std::collections::HashSet<NodeId> = self.gen_requests.iter().filter(|r| r.panel.as_deref() == Some(&name)).filter_map(|r| self.solids.iter().find(|s| s.id == r.part).map(|s| s.node)).collect();
        let mut todo = vec![self.solids[base].node];
        todo.extend(panel_cuts.iter().map(|(_, node)| *node));
        let mut seen = std::collections::HashSet::new();
        while let Some(node) = todo.pop() {
            if !seen.insert(node) { continue; }
            if targets.contains(&node) {
                return f.err("a generated part cannot be the base or a cut of the panel it cuts");
            }
            todo.extend(self.graph.node(node).op.inputs());
        }
        let pose = f.get("pose").cloned().unwrap_or(V::Nil);
        pose.splash().or_else(|e| f.err(e))?;
        self.panels.push(PanelReq { name: name.clone(), base, pose });
        let mut node = self.child(f.need("base")?, format!("{id}.base"), index)?;
        let parts: Vec<GenPart> = self.gen_requests.iter().filter(|r| r.panel.as_deref() == Some(&name)).filter_map(|r| self.generated.get(&r.part).cloned()).collect();
        let claims: std::collections::HashSet<&str> = parts.iter().flat_map(|p| p.claims.iter().map(String::as_str)).collect();
        let mut adds = Vec::new();
        let mut cuts: Vec<NodeId> = panel_cuts.into_iter().filter_map(|(cut, node)| {
            let solid = &self.solids[cut];
            (!solid.off && !solid.link.as_deref().is_some_and(|key| claims.contains(key))).then_some(node)
        }).collect();
        for p in parts {
            for add in p.adds { adds.push(add(&mut self.graph).or_else(|e| f.err(e))?); }
            for cut in p.cuts { cuts.push(cut(&mut self.graph).or_else(|e| f.err(e))?); }
        }
        if !adds.is_empty() { node = self.push(&f, Op::Boolean { mode: BoolMode::Union, a: node, b: adds })?; }
        if !cuts.is_empty() { node = self.push(&f, Op::Boolean { mode: BoolMode::Difference, a: node, b: cuts })?; }
        self.placed(&f, node)
    }
}

/// A generated call outside any fn and loop: its line name can be written into its object call.
fn pinned(call: &vm::PartCall) -> bool {
    call.stack.is_empty() && call.loops.is_empty()
}

/// The script's calls in source order, for ids without line numbers: each call's bytes, its
/// callee as written with its ordinal among that callee's calls (`posts#1`), and its last name
/// with its ordinal among calls of that name however they are reached (`generated#0` for
/// `m.generated(..)`). A line inserted anywhere changes none of them.
struct Calls(Vec<Call>);

struct Call {
    span: Range<usize>,
    site: String,
    builder: String,
}

impl Calls {
    fn new(names: &names::Names) -> Self {
        let source = names.syntax.source();
        let mut spans = Vec::new();
        names.syntax.walk(&mut |_, node| {
            if let makepad_script::syntax::Kind::Call { callee, .. } = &node.kind {
                spans.push((node.span.clone(), callee.span.clone()));
            }
        });
        spans.sort_by_key(|(span, _)| (span.start, std::cmp::Reverse(span.end)));
        let (mut sites, mut builders) = (HashMap::new(), HashMap::new());
        Self(spans.into_iter().map(|(span, callee)| {
            let written: String = source.get(callee).unwrap_or("").split_whitespace().collect();
            let last = written.rsplit('.').next().unwrap_or("").to_string();
            Call { span, builder: numbered(&mut builders, last), site: numbered(&mut sites, written) }
        }).collect())
    }

    /// The innermost call at a script location.
    fn at(&self, names: &names::Names, loc: Option<(u32, u32)>) -> Option<&Call> {
        let at = names.offset(loc?)?;
        self.0.iter().filter(|c| c.span.start <= at && at <= c.span.end).min_by_key(|c| c.span.len())
    }

    /// A part's id: its name, the script call sites it was made through and its loop positions.
    fn made_at(&self, names: &names::Names, base: String, call: &vm::PartCall) -> String {
        let mut id = base;
        let sites: Vec<&str> = call.stack.iter().filter_map(|loc| Some(self.at(names, Some(*loc))?.site.as_str())).collect();
        if !sites.is_empty() {
            id.push_str(" at ");
            id.push_str(&sites.join(" > "));
        }
        for position in &call.loops {
            id.push_str(&format!("[{position}]"));
        }
        id
    }
}

/// `key#n`, n counting the keys seen so far.
fn numbered(count: &mut HashMap<String, usize>, key: String) -> String {
    let n = count.entry(key.clone()).or_insert(0);
    *n += 1;
    format!("{key}#{}", *n - 1)
}

//! `fab.round`: what is picked, rounded (or bevelled) by one number. The view's Round and Chamfer
//! commands drag that number at a handle on the pick; the AI gives it. Both write the same edit:
//!
//! - an exact part's picked edges and chains (one part): the part's body becomes its fillet,
//!   `let body = m.brep.fillet(body_union, 2mm, {edges: m.edge(body_union.top, ..)})` right after
//!   what it was, now `body_union` (`Edit::After`): the part keeps its name, its placement and
//!   what is set on it; once it is there, its radius;
//! - a crease on a part drawn from its field: the blend radius of the statement joining the two
//!   edits; a hard union or subtract becomes its blended form (`m.blend`, `m.carve`) first;
//! - an edit's own edges on a part drawn from its field: its rounding (a box's `radius`), which
//!   rounds all of its edges alike.
//!
//! A plan is read from the picks and the committed program; nothing is built for it. Its
//! [`Template`] writes the change for any value, so a drag previews the very batch its release
//! commits: one `FabModel::apply`, one undo step.
use crate::out::{arr, n, obj, s};
use crate::picks::{Kind, Pick};
use crate::{inspect, Reply, Session};
use fab_model::params::{Kind as PKind, Template, Writes, AT_VALUE};
use makepad_model::json::Value;

/// The tool's name, for the AI and for the view's own rounds.
pub const TOOL: &str = "round";

/// What a pick must be for a round.
const NOTHING: &str = "pick what to round first (E): an edge or a chain of an exact part, or a crease or an edit's edges on a part drawn from its field";

/// A round on the picks: what it writes at each value, and where its handle sits.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The printed object it is on (`instance/object`).
    pub object: String,
    /// "Round" or "Chamfer".
    pub label: String,
    /// What the value does, in words for the handle ("rounds all edges of `body`").
    pub says: String,
    pub template: Template,
    /// What writes the value once a commit made it the script's own (the new statement's
    /// radius, the blend a union became); none: `template` stays.
    pub then: Option<Template>,
    /// The value now and the range it drags in (mm).
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// The handle's spot and the way it drags out of the material, in the picked solid's model frame.
    pub at: [f64; 3],
    pub dir: [f64; 3],
    /// The picks it acts on.
    pub picks: Vec<String>,
}

pub(crate) fn unit(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    (l > 1e-9).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// A length argument in mm: a number, or text such as `2mm`.
pub(crate) fn mm_arg(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) if x.is_finite() => Some(*x),
        Value::Int(i) => Some(*i as f64),
        Value::Str(t) => t.trim().trim_end_matches("mm").trim().parse().ok(),
        _ => None,
    }
}

/// What a template writes, as fab.apply takes it, the value written as `v`.
pub(crate) fn written(t: &Template, v: &str) -> String {
    writes(&t.writes, v)
}

fn writes(w: &Writes, v: &str) -> String {
    match w {
        Writes::Set { site } => format!("set({site}, {v})"),
        Writes::Param { name, .. } => format!("set({name}, {v})"),
        Writes::Field { solid, field, code } => format!("field({solid}, {field}, {})", code.replace(AT_VALUE, v)),
        Writes::Replace { solid, code } => format!("replace({solid}, {})", code.replace(AT_VALUE, v)),
        Writes::After { solid, code } => format!("after({solid})\nlet {solid} = {}", code.replace(AT_VALUE, v)),
        // each site at its own value: `base + rate * v`
        Writes::Many(sites) => sites.iter().map(|l| writes(&l.writes, &linked(l, v))).collect::<Vec<_>>().join("\n"),
        Writes::Hoisted { hoists, then } => hoists.iter().map(|(solid, to)| format!("rename({solid}, {to})")).chain(std::iter::once(writes(then, v))).collect::<Vec<_>>().join("\n"),
    }
}

/// A linked site's value in words for the value `v`: `20mm + v`, `-v / 2`.
fn linked(l: &fab_model::params::Linked, v: &str) -> String {
    let by = match l.rate {
        r if (r - 1.0).abs() < 1e-9 => v.to_string(),
        r if (r + 1.0).abs() < 1e-9 => format!("-{v}"),
        r if (r - 0.5).abs() < 1e-9 => format!("{v} / 2"),
        r if (r + 0.5).abs() < 1e-9 => format!("-{v} / 2"),
        r => format!("{r} * {v}"),
    };
    if l.base.abs() < 1e-9 { by } else { format!("{}mm + {by}", fmt_mm(l.base)) }
}

fn fmt_mm(v: f64) -> String {
    let t = format!("{v:.3}");
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The number value site `sid.path` holds as fab.params lists it: set where it is, a declared
/// parameter's name set as that parameter, anything worked out refused (none: not written).
pub(crate) fn site_or_param(params: &[fab_model::params::PartParams], inst: &fab_model::program::Instance, sid: &str, path: &str, node: Option<&makepad_script::syntax::Node>) -> Result<Option<(Writes, f64)>, String> {
    let param = |key: &str| params.iter().flat_map(|pp| pp.params.iter()).find(|q| q.key == key);
    let key = fab_model::program::part_id(&inst.name, &format!("{sid}.{path}"));
    if let Some(q) = param(&key).filter(|q| !q.derived) {
        return Ok(Some((Writes::Set { site: format!("{sid}.{path}") }, q.value.num().unwrap_or(0.0))));
    }
    let Some(node) = node else { return Ok(None) };
    // a number written as it is (`1mm`, `0`)
    let written = text(inst.script.source(), node).trim();
    if let Ok(v) = written.strip_suffix("mm").unwrap_or(written).trim().parse::<f64>() {
        return Ok(Some((Writes::Set { site: format!("{sid}.{path}") }, v)));
    }
    if let Some(name) = node.ident().filter(|n| inst.script.param(n).is_some()) {
        let key = fab_model::program::part_id(&inst.name, name);
        let value = param(&key).and_then(|q| q.value.num()).unwrap_or(0.0);
        return Ok(Some((Writes::Param { instance: inst.name.clone(), name: name.to_string() }, value)));
    }
    Err(format!("`{sid}`'s {path} is worked out (`{}`): change the values it is made of", text(inst.script.source(), node).trim()))
}

/// The text of node `n` of `src`.
pub(crate) fn text<'a>(src: &'a str, n: &makepad_script::syntax::Node) -> &'a str {
    &src[n.span.clone()]
}

/// A command at the picks, by the one number it drags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Round,
    Chamfer,
    Pull,
    Hole,
    Shell,
}

/// What a command that wrote its step goes on setting, in words (`` sets `r`'s by ``).
pub fn step_words(t: &Template) -> Option<String> {
    match &t.writes {
        Writes::Set { site } => {
            let (sid, field) = site.split_once('.')?;
            Some(format!("sets `{sid}`'s {field}"))
        }
        _ => None,
    }
}

/// The picks a tool call names (`picks`: an id or a list; none: all of them).
pub(crate) fn pick_ids(args: &Value) -> Vec<String> {
    match args.get("picks") {
        None => Vec::new(),
        Some(Value::Str(id)) => vec![id.clone()],
        Some(v) => v.as_arr().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
    }
}

impl Session {
    /// The plan of command `op` on picks `ids` (none: all of them).
    pub fn command_plan(&mut self, ids: &[String], op: Op) -> Result<Plan, String> {
        match op {
            Op::Round | Op::Chamfer => self.round_plan(ids, op == Op::Chamfer),
            Op::Pull => self.pull_plan(ids),
            Op::Hole => self.hole_plan(ids, None),
            Op::Shell => self.shell_plan(ids),
        }
    }

    /// Picks `ids` as the build now has them (none: all of them).
    pub(crate) fn chosen_picks(&mut self, ids: &[String]) -> Result<Vec<Pick>, String> {
        self.picks_follow_build();
        if ids.is_empty() {
            return Ok(self.picks.list.clone());
        }
        ids.iter().map(|id| self.picks.list.iter().find(|p| &p.id == id).cloned().ok_or_else(|| format!("no pick '{id}'"))).collect()
    }

    /// The round of picks `ids` (none: all of them); `chamfer` bevels exact edges instead.
    pub fn round_plan(&mut self, ids: &[String], chamfer: bool) -> Result<Plan, String> {
        let picks = self.chosen_picks(ids)?;
        let edges: Vec<&Pick> = picks.iter().filter(|p| matches!(p.kind, Kind::Edge | Kind::Chain)).collect();
        if !edges.is_empty() {
            return self.round_exact(&edges, chamfer);
        }
        match picks.iter().rev().find(|p| matches!(p.kind, Kind::Crease | Kind::Edges)) {
            Some(_) if chamfer => Err("a chamfer bevels an exact part's edges; on a part drawn from its field a crease or an edit's edges round".into()),
            Some(p) => self.round_field(p),
            None => Err(NOTHING.into()),
        }
    }

    /// Exact edges and chains on one step: a fillet (or chamfer) statement after it.
    fn round_exact(&mut self, edges: &[&Pick], chamfer: bool) -> Result<Plan, String> {
        let object = edges[0].object.clone();
        if edges.iter().any(|p| p.object != object) {
            return Err("the picked edges are on different parts: round one part's edges at a time".into());
        }
        let body = self.exact_body(&edges[0].on)?;
        let (inst, k) = self.build_on(edges)?;
        let program = &inst.script.program;
        let solid = &program.solids[k];
        if !solid.named || solid.call.is_some() {
            return Err(format!("`{}` has no `let` of its own at the top level: name its body to round its edges", solid.id));
        }
        // (each edge by its faces' copies' own names, where the body holds the part twice: by a
        // name the copies share it would round every copy's)
        let mut own = body.clone().map(|b| crate::hoist::Own::new(program, b));
        let selectors: Vec<String> = edges
            .iter()
            .map(|p| {
                let selector = p.selector.clone().ok_or_else(|| format!("pick {} has no name a script can write", p.id))?;
                match (&mut own, &p.named) {
                    (Some(own), Some(crate::picks::Named::Edge(pair))) => Ok(own.edge(pair, p.at.map(|(q, _)| q), p.kind == Kind::Chain)?.unwrap_or(selector)),
                    _ => Ok(selector),
                }
            })
            .collect::<Result<_, String>>()?;
        let (op, field, label) = if chamfer { ("chamfer", "size", "Chamfer") } else { ("fillet", "radius", "Round") };
        let picked = if selectors.len() == 1 { selectors[0].clone() } else { format!("[{}]", selectors.join(" ")) };
        let code = format!("m.brep.{op}({}, {AT_VALUE}, {{edges: {picked}}})", solid.id);
        let after = Writes::After { solid: solid.id.clone(), code };
        let file = inst.script.file.clone();
        let kind = if chamfer { PKind::Length } else { PKind::Radius };
        let last = edges[edges.len() - 1];
        // the edge it names (by the point pointed at, when there is one): the handle on it, out of
        // the material between its two faces' normals where they are flat
        let (mut at, normal) = last.at.unwrap_or(([0.0; 3], [0.0, 1.0, 0.0]));
        let pointed = last.at.is_some();
        let dir = body.and_then(|b| {
            let crate::picks::Named::Edge(pair) = last.named.as_ref()? else { return None };
            let near = |e: &&makepad_csg_graph::BrepEntity| e.points.iter().map(|q| (0..3).map(|k| (q[k] - at[k]).powi(2)).sum::<f64>()).fold(f64::MAX, f64::min);
            let edge = inspect::edges_between(&b, &pair[0], &pair[1]).into_iter().min_by(|x, y| near(x).total_cmp(&near(y)))?;
            if !pointed {
                at = edge.at;
            }
            let normals: Vec<[f64; 3]> = edge.faces.iter().filter_map(|f| b.faces.iter().find(|x| x.id == *f)?.normal).collect();
            match normals.as_slice() {
                [a, c] => unit([a[0] + c[0], a[1] + c[1], a[2] + c[2]]),
                _ => None,
            }
        });
        let count = edges.len();
        let says = format!("{} {count} edge{} of `{id}`: `{id}` becomes its {op}", if chamfer { "bevels" } else { "rounds" }, if count == 1 { "" } else { "s" }, id = solid.id);
        Ok(Plan {
            object,
            label: label.into(),
            says,
            template: Template { file: file.clone(), kind, writes: own.as_ref().map_or_else(|| after.clone(), |own| own.writes(after.clone())) },
            then: Some(Template { file, kind, writes: Writes::Set { site: format!("{}.{field}", solid.id) } }),
            value: 0.0,
            min: 0.1,
            max: 50.0,
            step: 0.1,
            at,
            dir: dir.or_else(|| unit(normal)).unwrap_or([0.0, 1.0, 0.0]),
            picks: edges.iter().map(|p| p.id.clone()).collect(),
        })
    }

    /// A crease or an edit's own edges on a part drawn from its field.
    fn round_field(&mut self, p: &Pick) -> Result<Plan, String> {
        let c = self.compiled.clone().ok_or("nothing is built yet")?;
        let params = self.params()?;
        let (at, normal) = self.handle_spot(p);
        let plan = |template: Template, then: Option<Template>, value: f64, says: String| Plan {
            object: p.object.clone(),
            label: "Round".into(),
            says,
            template,
            then,
            value,
            min: 0.0,
            max: 50.0,
            step: 0.1,
            at,
            dir: unit(normal).unwrap_or([0.0, 1.0, 0.0]),
            picks: vec![p.id.clone()],
        };
        let site_or_param = |inst: &fab_model::program::Instance, sid: &str, field: &str, node: Option<&makepad_script::syntax::Node>| site_or_param(&params, inst, sid, field, node);
        match p.kind {
            Kind::Crease => {
                let [a, b] = [&p.solids[0], p.solids.get(1).ok_or("a crease names two solids")?];
                let (fa, fb) = (self.facts(a)?, self.facts(b)?);
                let joint = fa.parents.iter().find(|x| fb.parents.contains(x)).cloned().ok_or_else(|| format!("`{}` and `{}` meet in the object's own statement: join them with m.blend to round where they meet", fa.short, fb.short))?;
                let (inst, i) = c.part(&joint).ok_or_else(|| format!("no solid '{joint}'"))?;
                let program = &inst.script.program;
                let solid = &program.solids[i];
                let sid = solid.id.clone();
                let proto = makepad_model_script::lang::protos::proto(solid.proto).ok_or_else(|| format!("`{sid}` has no builder"))?;
                let call = makepad_model_script::lang::edit::part_call(program, solid)?;
                let syn = &program.names.syntax;
                let fields = makepad_model_script::lang::edit::call_fields(syn, &call, proto);
                let node = |name: &str| fields.iter().find(|(f, _)| f == name).and_then(|(_, at)| syn.node(at.loc()));
                let src = inst.script.source();
                let file = inst.script.file.clone();
                let template = |writes: Writes| Template { file: file.clone(), kind: PKind::Radius, writes };
                match solid.proto {
                    "Blend" | "Carve" => {
                        let (writes, value) = site_or_param(inst, &sid, "radius", node("radius"))?.ok_or_else(|| format!("`{sid}` has no radius written"))?;
                        let what = if solid.proto == "Blend" { "blend" } else { "carve's edge" };
                        Ok(plan(template(writes), None, value, format!("rounds the crease: `{sid}`'s {what}, every crease it makes")))
                    }
                    "Union" | "Difference" => {
                        let (Some(na), Some(nb)) = (node("a"), node("b")) else { return Err(format!("`{sid}`'s operands are not written in its call")) };
                        let a_text = text(src, na).trim().to_string();
                        let b_text = match &nb.kind {
                            makepad_script::syntax::Kind::Array { items, .. } => items.iter().map(|x| text(src, x).trim()).collect::<Vec<_>>().join(" "),
                            _ => text(src, nb).trim().to_string(),
                        };
                        let (code, was, now) = if solid.proto == "Union" { (format!("m.blend([{a_text} {b_text}], {AT_VALUE})"), "union", "blend") } else { (format!("m.carve({a_text}, {}, {AT_VALUE})", text(src, nb).trim()), "subtract", "carve") };
                        let then = template(Writes::Set { site: format!("{sid}.radius") });
                        Ok(plan(template(Writes::Replace { solid: sid.clone(), code }), Some(then), 0.0, format!("rounds the crease: `{sid}` becomes a {now} (was a {was})")))
                    }
                    other => Err(format!("`{}` and `{}` are joined by `{sid}` ({}): only unions, subtracts, blends and carves round where they meet", fa.short, fb.short, other.to_lowercase())),
                }
            }
            Kind::Edges => {
                let f = self.facts(&p.solids[0])?;
                let (inst, i) = c.part(&f.id).ok_or_else(|| format!("no solid '{}'", f.id))?;
                let solid = &inst.script.program.solids[i];
                if solid.proto != "Box" {
                    return Err(format!("`{}` ({}) has no rounding of its own: round a crease where it meets another edit", f.short, f.builder));
                }
                let sid = solid.id.clone();
                let program = &inst.script.program;
                let node = makepad_model_script::lang::edit::part_call(program, solid).ok().and_then(|call| {
                    let syn = &program.names.syntax;
                    let fields = makepad_model_script::lang::edit::call_fields(syn, &call, makepad_model_script::lang::protos::proto("Box")?);
                    fields.iter().find(|(name, _)| name == "radius").and_then(|(_, at)| syn.node(at.loc()))
                });
                let template = |writes: Writes| Template { file: inst.script.file.clone(), kind: PKind::Radius, writes };
                let says = format!("rounds all edges of `{sid}`");
                match site_or_param(inst, &sid, "radius", node)? {
                    Some((writes, value)) => Ok(plan(template(writes), None, value, says)),
                    None => {
                        let then = template(Writes::Set { site: format!("{sid}.radius") });
                        Ok(plan(template(Writes::Field { solid: sid.clone(), field: "radius".into(), code: AT_VALUE.into() }), Some(then), 0.0, says))
                    }
                }
            }
            _ => Err(NOTHING.into()),
        }
    }

    /// Where a field pick's handle goes: where it was pointed at; for a pick named without a point
    /// (the AI's), on the part's surface by the crease (the nearest points of its two solids'
    /// surfaces, taken in turn, meet on it) or by the edit's corner. A few surface queries, once
    /// per plan.
    fn handle_spot(&mut self, p: &Pick) -> ([f64; 3], [f64; 3]) {
        if let Some(at) = p.at {
            return at;
        }
        let mut q = p.bounds.map_or([0.0; 3], |(lo, hi)| [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0]);
        match (p.kind, p.solids.as_slice()) {
            (Kind::Crease, [a, b]) => {
                if let (Ok(a), Ok(b)) = (self.surface_query(a), self.surface_query(b)) {
                    for _ in 0..12 {
                        let Some((x, _)) = b.nearest_model(q) else { break };
                        let Some((y, _)) = a.nearest_model(x) else { break };
                        q = y;
                    }
                }
            }
            _ => {
                if let Some((_, hi)) = p.bounds {
                    q = hi;
                }
            }
        }
        let on = self.build_on(&[p]).ok().map(|(inst, k)| fab_model::program::part_id(&inst.name, &inst.script.program.solids[k].id));
        on.and_then(|id| self.surface_query(&id).ok()).and_then(|s| s.nearest_model(q)).unwrap_or((q, [0.0, 1.0, 0.0]))
    }

    /// The plate with a command's batch made (a round at a dragged value), evaluated, not
    /// committed: only the instances it edits rebuild, their fields meshed in draft. `cancelled`
    /// stops the build at its next check.
    pub fn preview_changes(&mut self, changes: &[fab_model::Change], cancelled: Option<&dyn Fn() -> bool>) -> Result<(std::sync::Arc<fab_model::program::Compiled>, std::sync::Arc<fab_model::eval::Evaluated>, std::sync::Arc<Vec<fab_model::params::PartParams>>), String> {
        let compiled = self.compiled.clone().ok_or("nothing is built yet")?;
        // the instance the first change edits builds in draft
        let instance = changes.iter().find_map(|c| match c {
            fab_model::Change::Script { file, .. } => compiled.instances.iter().find(|i| &i.script.file == file).map(|i| i.name.clone()),
            fab_model::Change::Param { instance, .. } => Some(instance.clone()),
            _ => None,
        });
        self.eval.draft = instance.map(|i| (i, crate::PREVIEW_DRAFT));
        let built = self.model.preview(&mut self.eval, changes, cancelled);
        self.eval.draft = None;
        let (_, c, ev) = built.map_err(|e| e.to_string())?;
        let model = std::sync::Arc::new(fab_model::params::model(&c, &ev));
        let (c, ev) = self.previewed(c, ev);
        Ok((c, ev, model))
    }

    /// The AI's round: the picks (`picks`, default all), `radius` (mm; a chamfer's size with
    /// `chamfer: true`) committed as one undo step; without `radius`, what it would write.
    pub(crate) fn round_tool(&mut self, args: &Value) -> Reply {
        let ids = pick_ids(args);
        let chamfer = matches!(args.get("chamfer"), Some(Value::Bool(true)));
        let plan = match self.round_plan(&ids, chamfer) {
            Ok(plan) => plan,
            Err(e) => return Reply::err(e),
        };
        let Some(radius) = args.get("radius").and_then(mm_arg) else {
            let writes = written(&plan.template, "<value>");
            return Reply::ok(obj(vec![("label", s(&plan.label)), ("says", s(&plan.says)), ("object", s(&plan.object)), ("value", n(plan.value)), ("min", n(plan.min)), ("max", n(plan.max)), ("writes", s(writes)), ("picks", arr(plan.picks.iter().map(s)))]));
        };
        // (a round that writes something new is no round at 0)
        let low = if plan.then.is_some() { plan.min.max(0.1) } else { plan.min };
        if !(low..=plan.max).contains(&radius) {
            return Reply::err(format!("{} takes {low} to {} mm", plan.label.to_lowercase(), plan.max));
        }
        self.commit(TOOL, plan.template.changes(radius))
    }
}

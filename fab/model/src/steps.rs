//! A plate body's build history, derived from its compiled solid inputs and shown
//! in the same words and display units as the person's tools.

use crate::params::{self, Kind};
use crate::program::{Compiled, Script};
use makepad_model_script::lang::{edit, protos, Solid};
use makepad_script::syntax::{Kind as SyntaxKind, Loc};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

/// The person's word for a prototype's builder call, the tools' words. Other calls keep their
/// name with the first letter capitalised.
pub fn verb(call: &str) -> Cow<'static, str> {
    Cow::Borrowed(match call {
        "fillet" => "Round",
        "chamfer" => "Chamfer",
        "union" => "Join",
        "subtract" => "Cut",
        "intersect" => "Intersect",
        "extrude" => "Extrude",
        "revolve" => "Revolve",
        "sweep" => "Sweep",
        "loft" => "Loft",
        "shell" => "Shell",
        "pull" => "Pull",
        "hole" => "Hole",
        "mirror" => "Mirror",
        "blend" => "Blend",
        "box" => "Box",
        "cylinder" => "Cylinder",
        "sphere" => "Sphere",
        "cone" => "Cone",
        "torus" => "Torus",
        "import" => "Import",
        other => {
            let mut chars = other.chars();
            return Cow::Owned(chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default());
        }
    })
}

/// Other operands of a boolean grouped under the name the person sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// Joining (+), cutting (−) or intersecting (&).
    pub role: char,
    /// The loop binding, solid binding or inline solid id.
    pub name: String,
    /// Number of operand occurrences belonging to this branch.
    pub count: usize,
    /// The solid index of the branch's first operand: what hovering its badge lights up.
    pub first: usize,
}

impl fmt::Display for Branch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{} ×{}", self.role, self.name, self.count)
    }
}

/// One solid on a body's first-input spine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// Position in build order, starting at one.
    pub n: usize,
    /// The solid's existing script id, including for an inline call.
    pub id: String,
    /// Index in the compiled program's solids.
    pub solid: usize,
    /// Person-facing builder verb.
    pub verb: Cow<'static, str>,
    /// The builder's call name from its prototype.
    pub call: &'static str,
    /// First written numeric field, formatted for display.
    pub value: Option<String>,
    /// One-based source line (one when no location was recorded).
    pub line: u32,
    /// One-based inclusive last line of the builder's statement.
    pub end_line: u32,
    /// The solid's source bytes when it has a site in the script.
    pub range: Option<Range<usize>>,
    /// Boolean operand badges, in order of first use.
    pub uses: Vec<Branch>,
    /// Whether the script bypasses this solid when it is an operand.
    pub off: bool,
    /// The script function whose call made this solid.
    pub made_in: Option<String>,
    /// Other emitted objects whose bodies depend on this solid, in emission order.
    pub shared_with: Vec<String>,
}

/// The step containing a diagnostic line, or the first step starting after it.
pub fn step_of_line(steps: &[Step], line: u32) -> Option<usize> {
    steps.iter().position(|s| (s.line..=s.end_line).contains(&line))
        .or_else(|| steps.iter().position(|s| s.line > line))
}

/// A failed edit can shift the candidate's lines relative to the committed steps. Prefer
/// the failing solid's id, then its operand badge, before falling back to the line.
pub fn step_of_failure(steps: &[Step], line: u32, solid: Option<&str>) -> Option<usize> {
    solid.and_then(|id| {
        steps.iter().position(|s| s.id == id)
            .or_else(|| steps.iter().position(|s| s.uses.iter().any(|branch| branch.name == id)))
    }).or_else(|| step_of_line(steps, line))
}

/// A parameter as the step's diagnostic or undo label shows it. A change of a value
/// spells its unit once, after the second number.
pub fn value_text(p: &params::Param, value: &params::PValue, with_unit: bool) -> String {
    use params::PValue;
    let number = |x| params::write_like("", Kind::Number, x);
    let text = match value {
        PValue::Num(x) => number(if p.kind == Kind::Ratio { x * 100.0 } else { *x }),
        PValue::Point(coords) => coords.iter().map(|x| number(*x)).collect::<Vec<_>>().join(", "),
        PValue::Bool(on) => return if *on { "on" } else { "off" }.into(),
        PValue::Choice(i) => return p.choices.get(*i).map(|(label, _)| label.clone()).unwrap_or_default(),
        PValue::Profile(points) => return format!("{} points", points.len()),
    };
    match (with_unit, p.unit.as_str()) {
        (false, _) | (_, "") => text,
        (_, "°") => text + "°",
        (_, unit) => format!("{text} {unit}"),
    }
}

fn branches(solids: &[Solid], solid: &Solid, call: &str) -> Vec<Branch> {
    let role = match call {
        "union" | "blend" => '+',
        "subtract" | "carve" => '−',
        "intersect" => '&',
        _ => return Vec::new(),
    };
    let mut uses: Vec<Branch> = Vec::new();
    let mut indices = HashMap::new();
    for &i in solid.inputs.iter().skip(1) {
        let id = solids[i].id.as_str();
        let name = id.split_once('[').map_or(id, |(binding, _)| binding);
        let index = *indices.entry(name).or_insert_with(|| {
            uses.push(Branch { role, name: name.into(), count: 0, first: i });
            uses.len() - 1
        });
        uses[index].count += 1;
    }
    uses
}

fn headline(history: &History<'_>, proto: &protos::Proto, call: &Loc) -> Option<String> {
    let syn = &history.script.program.names.syntax;
    for (name, at) in edit::call_fields(syn, call, proto) {
        let Some(kind) = proto.field(&name).and_then(|f| Kind::parse(f.kind.name())).filter(|k| k.is_numeric()) else {
            continue;
        };
        let node = syn.node(at.loc())?;
        let value = match &node.kind {
            SyntaxKind::Number(n) => Some(*n),
            SyntaxKind::Unary { op, operand } if op == "-" => operand.number().map(|n| -n),
            SyntaxKind::Ident(name) => history.bindings.get(name.as_str()).and_then(|bindings| {
                bindings.iter().rev().find(|(scope, _)| scope.contains(&node.span.start))?.1
            }),
            _ => None,
        };
        if let Some(value) = value {
            let (display, suffix) = match kind.default_unit() {
                "mm" => (value * crate::program::UNIT, " mm"),
                "°" => (value.to_degrees(), "°"),
                "%" => (value * 100.0, " %"),
                _ => (value, ""),
            };
            return Some(params::write_like("", Kind::Number, display) + suffix);
        }
        let mut chars = node.text(syn.source()).trim().chars();
        let mut text: String = chars.by_ref().take(16).collect();
        if chars.next().is_some() {
            text.push('…');
        }
        return Some(text);
    }
    None
}

/// Temporary lookups for one history request, shared by all the requested bodies.
struct History<'a> {
    script: &'a Script,
    calls: HashMap<Range<usize>, Loc>,
    // Only names of declared params. A local binding of the same name takes over
    // inside its scope and shows as written; each param declaration has its VM value.
    bindings: HashMap<&'a str, Vec<(Range<usize>, Option<f64>)>>,
    users: HashMap<usize, Vec<&'a str>>,
}

impl<'a> History<'a> {
    fn new(script: &'a Script) -> Self {
        let names = &script.program.names;
        let mut bindings = HashMap::new();
        let mut declared = HashMap::new();
        for param in &script.program.params {
            if let (Some(name), Some(site)) = (param.name.as_deref(), names.site(param.call.loc)) {
                bindings.entry(name).or_insert_with(Vec::new);
                declared.insert(&site.span, param.call.value);
            }
        }
        let mut calls = HashMap::new();
        names.syntax.walk(&mut |loc, node| match &node.kind {
            SyntaxKind::Call { .. } => {
                calls.entry(node.span.clone()).or_insert_with(|| loc.clone());
            }
            SyntaxKind::Let { names: bound, value, .. } => {
                for name in bound {
                    if let Some(sites) = bindings.get_mut(name.as_str()) {
                        let end = loc.parent().and_then(|p| names.syntax.node(&p)).map_or(node.span.end, |scope| scope.span.end);
                        sites.push((node.span.end..end, value.as_ref().and_then(|v| declared.get(&v.span).copied())));
                    }
                }
            }
            SyntaxKind::Fn { params, body, .. } => {
                for param in params {
                    if let Some(sites) = bindings.get_mut(param.name.as_str()) {
                        sites.push((body.span.clone(), None));
                    }
                }
            }
            SyntaxKind::For { binding, body, .. } => {
                for name in script.source()[binding.clone()].split(|c: char| !c.is_alphanumeric() && c != '_') {
                    if let Some(sites) = bindings.get_mut(name) {
                        sites.push((body.span.clone(), None));
                    }
                }
            }
            _ => {}
        });
        let mut users = HashMap::<usize, Vec<&str>>::new();
        if script.objects.len() > 1 {
            for object in &script.objects {
                let mut todo = object.bodies.clone();
                let mut seen = HashSet::new();
                while let Some(i) = todo.pop() {
                    if seen.insert(i) {
                        users.entry(i).or_default().push(&object.name);
                        todo.extend(&script.program.solids[i].inputs);
                    }
                }
            }
        }
        History { script, calls, bindings, users }
    }

    fn spine(&self, body: usize, object: Option<&str>) -> Vec<Step> {
        let program = &self.script.program;
        let syn = &program.names.syntax;
        let mut result = Vec::new();
        let mut next = Some(body);
        while let Some(i) = next {
            let Some(solid) = program.solids.get(i) else { break };
            let proto = protos::proto(solid.proto).expect("a compiled solid has a prototype");
            let call = proto.call;
            let value = solid.span.as_ref().and_then(|span| self.calls.get(span)).and_then(|loc| headline(self, proto, loc));
            let made_in = solid.call.as_ref().and_then(|c| {
                let site = c.inner.last().unwrap_or(&c.span);
                let SyntaxKind::Call { callee, .. } = &syn.node(self.calls.get(site)?)?.kind else { return None };
                Some(callee.text(syn.source()).to_string())
            });
            let shared_with = self.users.get(&i).into_iter().flatten()
                .copied().filter(|name| Some(*name) != object).map(str::to_string).collect();
            let line = solid.loc.map_or(1, |(line, _)| line);
            let end_line = solid.span.as_ref().map_or(line, |span| {
                line + syn.source()[span.clone()].matches('\n').count() as u32
            });
            result.push(Step {
                n: 0,
                id: solid.id.clone(),
                solid: i,
                verb: verb(call),
                call,
                value,
                line,
                end_line,
                range: solid.span.clone(),
                uses: branches(&program.solids, solid, call),
                off: solid.off,
                made_in,
                shared_with,
            });
            next = solid.inputs.first().copied();
        }
        result.reverse();
        for (i, step) in result.iter_mut().enumerate() {
            step.n = i + 1;
        }
        result
    }
}

/// The steps of the placed part whose plate object is `part` (`instance/solid`), in the
/// words of [`body_steps`]. Empty when no instance of `c` places such a body.
pub fn part_steps(c: &Compiled, part: &str) -> Vec<Step> {
    let Some((instance, _)) = crate::program::split_id(part) else { return Vec::new() };
    let Some(inst) = c.instance(instance) else { return Vec::new() };
    let solids = &inst.script.program.solids;
    let bodies = inst.placed.iter().filter(|b| crate::program::part_id(&inst.name, &solids[b.solid].id) == part).map(|b| (b.solid, b.object.as_str()));
    body_steps(&inst.script, bodies).into_iter().next().map(|(_, steps)| steps).unwrap_or_default()
}

/// One walk of the body's spine, O(solids and operand occurrences of the part).
/// Source calls are indexed once. For sharing, each other object's input graph is
/// visited once per object. No geometry is evaluated.
pub fn steps(script: &Script, solid: usize) -> Vec<Step> {
    History::new(script).spine(solid, script.object_of(solid).map(|o| o.name.as_str()))
}

/// One build-order list per emitted body of the named object, in body order. The
/// source and sharing lookups are built once, then each body's spine is walked once.
pub fn object_steps(script: &Script, object: &str) -> Vec<(usize, Vec<Step>)> {
    let Some(object) = script.object(object) else { return Vec::new() };
    body_steps(script, object.bodies.iter().map(|&i| (i, object.name.as_str())))
}

/// Read several placed bodies of one script in one request. Each pair is the solid
/// index and its emitted object's name. Shares the indexing cost of [`steps`] across
/// the batch, then walks each requested spine once.
pub fn body_steps<'a>(script: &Script, bodies: impl IntoIterator<Item = (usize, &'a str)>) -> Vec<(usize, Vec<Step>)> {
    let mut bodies = bodies.into_iter().peekable();
    if bodies.peek().is_none() {
        return Vec::new();
    }
    let history = History::new(script);
    bodies.map(|(i, object)| (i, history.spine(i, Some(object)))).collect()
}

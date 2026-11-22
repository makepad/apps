//! System editing actions shared by the command palette, UI and AI tools.

use crate::{board_doc, sys_doc};
use fab_elec_core::{Diag, DocId, LinkId, Sel};
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::{Design, DesignOp, Link, LinkKind, PinMap, SysOp, Unit};
use makepad_script::syntax::{self, parse_value_with, write::quote, Kind, Node};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub static ACTIONS: &[ActionDef] = &[
    ActionDef {
        name: "elec.link.add",
        label: "Add link",
        group: "System",
        help: "Join two link ends with a mate or a wire, using the system document's spelling.",
        returns: "seq: int  the step it committed\nlabel: text  its name in the undo history, with the new link's id (Add wire link 3)",
        fields: &[
            FieldDef {
                name: "kind",
                kind: FieldKind::Choice(&["mate", "wire"]),
                help: "The kind of link to add.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "a",
                kind: FieldKind::Splash,
                help: "The first end, as in the system document: ledctl.mcu.dbg or ledctl.mcu.dbg.pin(\"1\").",
                required: true,
                default: "",
            },
            FieldDef {
                name: "b",
                kind: FieldKind::Splash,
                help: "The second end, as in the system document: probe.j or probe.j.pin(\"1\").",
                required: true,
                default: "",
            },
        ],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
    ActionDef {
        name: "elec.link.remove",
        label: "Remove link",
        group: "System",
        help: "Remove the link with the given id. A link used by a mount must be unmounted first.",
        returns: "seq: int  the step it committed\nlabel: text  its name in the undo history",
        fields: &[FieldDef {
            name: "link",
            kind: FieldKind::Int { min: 0, max: u32::MAX as i64 },
            help: "The link's id, as a number.",
            required: true,
            default: "",
        }],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
    ActionDef {
        name: "elec.link.replace",
        label: "Replace link",
        group: "System",
        help: "Change a link's kind or ends. Each omitted field keeps its current value.",
        returns: "seq: int  the step it committed\nlabel: text  its name in the undo history",
        fields: &[
            FieldDef {
                name: "link",
                kind: FieldKind::Int { min: 0, max: u32::MAX as i64 },
                help: "The link's id, as a number.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "kind",
                kind: FieldKind::Choice(&["mate", "wire"]),
                help: "The new kind; omitted keeps the current kind.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "a",
                kind: FieldKind::Splash,
                help: "The first end, as in the system document; omitted keeps the current end.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "b",
                kind: FieldKind::Splash,
                help: "The second end, as in the system document; omitted keeps the current end.",
                required: false,
                default: "",
            },
        ],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
    ActionDef {
        name: "elec.unit.mount",
        label: "Mount unit",
        group: "System",
        help: "Mount a unit on another unit, a mate link, or the world. A mate's pose is solved later from its connectors, so at and rot cannot be stored with link.",
        returns: "seq: int  the step it committed\nlabel: text  its name in the undo history",
        fields: &[
            FieldDef {
                name: "unit",
                kind: FieldKind::Text,
                help: "The unit's name as the document writes it, such as probe.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "on",
                kind: FieldKind::Text,
                help: "The parent unit's name; mutually exclusive with link. Neither means the world.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "link",
                kind: FieldKind::Int { min: 0, max: u32::MAX as i64 },
                help: "The mate link's id; mutually exclusive with on, at and rot.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "at",
                kind: FieldKind::Splash,
                help: "Translation as in the system document, such as [10mm, 0mm, -8mm]; omitted is zero. Refused with link.",
                required: false,
                default: "[0mm, 0mm, 0mm]",
            },
            FieldDef {
                name: "rot",
                kind: FieldKind::Splash,
                help: "Rotation as in the system document, such as [0deg, 0deg, 90deg]; omitted is zero. Refused with link.",
                required: false,
                default: "[0deg, 0deg, 0deg]",
            },
        ],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
    ActionDef {
        name: "elec.unit.unmount",
        label: "Unmount unit",
        group: "System",
        help: "Reset a unit's mount to the world frame with the identity transform.",
        returns: "seq: int  the step it committed\nlabel: text  its name in the undo history",
        fields: &[FieldDef {
            name: "unit",
            kind: FieldKind::Text,
            help: "The unit's name as the document writes it, such as probe.",
            required: true,
            default: "",
        }],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
];

pub fn register_actions() {
    fab_elec_design::action::register(ACTIONS);
}

fn error(field: &str, message: impl std::fmt::Display, sel: Option<Sel>) -> Diag {
    Diag::error(format!("{field}: {message}"), sel.into_iter().collect())
}

fn unit<'a>(design: &'a Design, field: &str, text: &str) -> Result<&'a Unit, Diag> {
    let text = text.trim();
    let name = if text.starts_with('"') {
        let n = syntax::parse_value(text).map_err(|e| error(field, e.message, None))?;
        n.string().ok_or_else(|| error(field, "expected a string literal", None))?.to_string()
    } else { text.to_string() };
    design.units.iter().find(|u| u.name == name).ok_or_else(|| error(field, format!("no unit named {name}"), None))
}

fn link<'a>(design: &'a Design, text: &str) -> Result<&'a Link, Diag> {
    let id = text.trim().replace('_', "").parse::<u32>()
        .map(LinkId).map_err(|_| error("link", "expected an integer from 0 to 4294967295", None))?;
    design.link(id).ok_or_else(|| error("link", format!("no link with id {}", id.0), Some(Sel::Link(id))))
}

/// One composed statement, with field ranges for attributing reader diagnostics. These
/// ranges also keep a field from supplying extra arguments, options or declarations.
struct Statement {
    text: String,
    fields: Vec<(&'static str, Range<usize>, Option<Sel>)>,
}

impl Statement {
    fn new(text: String) -> Statement { Statement { text, fields: vec![] } }

    fn field(&mut self, name: &'static str, text: &str, sel: Option<Sel>) {
        let start = self.text.len();
        self.text.push_str(text);
        self.fields.push((name, start..self.text.len(), sel));
    }

    fn error(&self, at: usize, message: impl std::fmt::Display) -> Diag {
        let (field, _, sel) = self.fields.iter().find(|(_, span, _)| span.start <= at && at <= span.end)
            .or_else(|| self.fields.iter().rev().find(|(_, span, _)| span.start <= at))
            .unwrap_or(&self.fields[0]);
        error(field, message, sel.clone())
    }

    fn parse(&self) -> Result<Node, Diag> {
        parse_value_with(&self.text, &board_doc::units()).map_err(|e| self.error(e.at, format!("syntax: {}", e.message)))
    }

    fn check(&self, name: &str, n: &Node) -> Result<(), Diag> {
        if self.fields.iter().any(|(field, span, _)| *field == name && span.start <= n.span.start && n.span.end <= span.end) {
            Ok(())
        } else { Err(self.error(n.span.start, "expected one field value")) }
    }

    fn diag(&self, diag: Diag) -> Diag {
        let at = diag.sel.iter().find_map(|sel| match sel { Sel::Source { start, .. } => Some(*start as usize), _ => None }).unwrap_or(0);
        self.error(at, diag.message)
    }

    fn end(&self, design: &Design, name: &str, n: &Node) -> Result<fab_elec_design::LinkEnd, Diag> {
        self.check(name, n)?;
        sys_doc::read_end(&self.text, &design.units, n).map_err(|diag| {
            let sel = sys_doc::end_path(DocId(0), &self.text, &design.units, &HashMap::new(), n).ok().map(|(id, _)| Sel::Unit(id));
            error(name, diag.message, sel)
        })
    }
}

/// Build the system ops for the host's command path, without applying them. Unit fields
/// contain document names (bare or quoted text); link fields contain numeric `LinkId`s.
/// Ends and poses use the system document's reader. Cost: one small statement parse and
/// lowering, O(units + links), plus the reader's component-path lookups and quoted text reads.
/// Nothing runs per frame.
pub fn sys_ops(design: &Design, tool: &str, args: &[(&str, &str)]) -> Result<Vec<DesignOp>, Diag> {
    let action = ACTIONS.iter().find(|a| a.name == tool).ok_or_else(|| error("tool", format!("unknown {tool}"), None))?;
    for (i, (name, _)) in args.iter().enumerate() {
        if !action.fields.iter().any(|f| f.name == *name) { return Err(error(name, "unknown field", None)); }
        if args[..i].iter().any(|(other, _)| other == name) { return Err(error(name, "duplicate field", None)); }
    }
    let get = |name| args.iter().find_map(|(key, value)| (*key == name).then_some(*value));
    for field in action.fields {
        if field.required && get(field.name).is_none() { return Err(error(field.name, "required field is missing", None)); }
    }
    let op = match tool {
        "elec.link.remove" => SysOp::RemoveLink(link(design, get("link").expect("required field checked"))?.id),
        "elec.link.add" | "elec.link.replace" => {
            let old = get("link").map(|text| link(design, text)).transpose()?;
            let kind = if let Some(text) = get("kind") {
                match text.trim().strip_prefix('@').unwrap_or(text.trim()) {
                    "mate" => LinkKind::Mate,
                    "wire" => LinkKind::Wire,
                    _ => return Err(error("kind", "expected mate|wire", old.map(|l| Sel::Link(l.id)))),
                }
            } else { old.expect("only replace omits kind").kind };
            let mut s = Statement::new(format!("{}(", sys_doc::link_kind(kind)));
            for (name, end) in [("a", old.map(|l| l.a)), ("b", old.map(|l| l.b))] {
                if name == "b" { s.text.push_str(", "); }
                let text = get(name).map(str::to_string).unwrap_or_else(|| sys_doc::link_end(design, end.expect("only replace omits an end"), &HashSet::new()));
                s.field(name, &text, None);
            }
            s.text.push(')');
            let n = s.parse()?;
            let ends = n.call(sys_doc::link_kind(kind)).ok_or_else(|| s.error(n.span.end, "expected one link call"))?;
            for (i, name) in ["a", "b"].into_iter().enumerate() {
                let end = ends.get(i).ok_or_else(|| error(name, "expected a link end", None))?;
                s.check(name, end)?;
            }
            if let Some(extra) = ends.get(2) { return Err(s.error(extra.span.start, "expected two link ends")); }
            let a = s.end(design, "a", &ends[0])?;
            let b = s.end(design, "b", &ends[1])?;
            let link = Link {
                id: old.map_or_else(|| sys_doc::next_link_id(design.links.iter().map(|l| l.id)), |l| l.id),
                kind, a, b, pinmap: old.map_or(PinMap::Derived, |l| l.pinmap.clone()),
            };
            if old.is_some() { SysOp::ReplaceLink(link) } else { SysOp::AddLink(link) }
        }
        "elec.unit.mount" | "elec.unit.unmount" => {
            let unit = unit(design, "unit", get("unit").expect("required field checked"))?;
            if tool == "elec.unit.unmount" { return Ok(vec![DesignOp::Sys(SysOp::ClearMount(unit.id))]); }
            let parent = get("on").map(|text| self::unit(design, "on", text)).transpose()?;
            if get("link").is_some() {
                if let Some(parent) = parent { return Err(error("on", "cannot be supplied with link", Some(Sel::Unit(parent.id)))); }
                for field in ["at", "rot"] {
                    if get(field).is_some() { return Err(error(field, "cannot be supplied with link; a mate's pose is solved from its connectors", Some(Sel::Unit(unit.id)))); }
                }
            }
            if parent.is_some_and(|p| p.id == unit.id) { return Err(error("on", "a unit cannot be mounted on itself", Some(Sel::Unit(unit.id)))); }
            let mate = get("link").map(|text| link(design, text)).transpose()?;
            if let Some(link) = mate.filter(|l| l.kind != LinkKind::Mate) { return Err(error("link", "expected a mate link", Some(Sel::Link(link.id)))); }
            let mut names = HashMap::new();
            let mut s = Statement::new("mount(".into());
            s.field("unit", &format!("unit({})", quote(&unit.name)), Some(Sel::Unit(unit.id)));
            s.text.push_str(", {");
            if let Some(parent) = parent {
                s.text.push_str("on: ");
                s.field("on", &format!("unit({})", quote(&parent.name)), Some(Sel::Unit(parent.id)));
            } else if let Some(link) = mate {
                names.insert("parent".into(), Some(link.id));
                s.text.push_str("on: ");
                s.field("link", "parent", Some(Sel::Link(link.id)));
            }
            for field in ["at", "rot"] {
                if let Some(text) = get(field) {
                    s.text.push_str(&format!(" {field}: "));
                    s.field(field, text, Some(Sel::Unit(unit.id)));
                }
            }
            s.text.push_str("})");
            let n = s.parse()?;
            let Some([target, opts]) = n.call("mount") else { return Err(s.error(0, "expected one mount")); };
            s.check("unit", target)?;
            let Kind::Object { ty: None, fields, .. } = &opts.kind else { return Err(s.error(opts.span.start, "expected mount options")); };
            for field in fields {
                let name = match field.name.as_deref() {
                    Some("on") if mate.is_some() => "link",
                    Some(name @ ("on" | "at" | "rot")) => name,
                    _ => return Err(s.error(field.value.span.start, "unknown mount option")),
                };
                s.check(name, &field.value)?;
            }
            if fields.len() + 1 != s.fields.len() { return Err(s.error(opts.span.start, "expected one value per mount field")); }
            SysOp::SetMount(sys_doc::read_mount(&s.text, &design.units, &design.links, &names, &n).map_err(|e| s.diag(e))?)
        }
        _ => unreachable!("the catalog lists only system actions"),
    };
    Ok(vec![DesignOp::Sys(op)])
}

//! BOM reads use the committed design and the host's offline rate snapshot. A group edit
//! resolves every target before submitting one document transaction.
use crate::edit_actions::{arg, board_unit, check_tool, error, Args};
use crate::{Docs, Plan};
use fab_elec_bom::{sheet, BoardFacts, BomSheet, Figure, Rates, Run, SheetView};
use fab_elec_circuit::{CircuitOp, Component};
use fab_elec_core::{BoardId, CompId, CompRef, Diag};
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::{By, Design, DesignOp, Resolved};
use fab_elec_lang::splice::SharedCalls;
use makepad_widgets_core::makepad_script::syntax::write::quote;
use std::{collections::{BTreeMap, BTreeSet}, fmt::Write};

const fn text(name: &'static str, required: bool, help: &'static str) -> FieldDef {
    FieldDef { name, kind: FieldKind::Text, required, default: "", help }
}
const UNIT: FieldDef = text("unit", false, "Board unit name. May be omitted for a single-board design.");
const BOM_READ: ActionDef = ActionDef {
    name: "elec.bom.read", label: "Read bill of materials", group: "BOM",
    help: "List the board's parts, sourcing fields, library inheritance and missing data. A line ID addresses exactly its member paths.",
    fields: &[UNIT, FieldDef { name: "group", kind: FieldKind::Bool, required: false, default: "true", help: "Group like parts." },
        text("filter", false, "Find text in the visible columns."), text("only", false, "One line ID, reference or path; path: forces an exact path."),
        FieldDef { name: "after", kind: FieldKind::Int { min: 0, max: i64::MAX }, required: false, default: "0", help: "Pass the previous page's next." }],
    returns: "total: int\nnext: int  cursor when another page exists\nlines: [{..}]
  line: text
  refs: [text]
  paths: [text]
  qty: int
  value: text
  footprint: text
  mpn: text
  supplier: text
  supplier_part: text
  description: text
  dnp: bool
  fields: {..}
  inherited: [text]
  alts: {..}
  checks: [text]
  lifecycle: choice active|nrnd|obsolete  the worst its parts state, when one does
  approved: bool  true when every part on the line is approved; absent otherwise",
    inputs: &[], risk: Risk::Read, keys: "",
};
pub const FIELD_SET: ActionDef = ActionDef {
    name: "elec.bom.field.set", label: "Edit BOM field", group: "BOM",
    help: "Set a component field on every listed part in one undo step. Omit text to remove the override.",
    fields: &[UNIT, text("parts", true, "One line ID, or comma-separated component paths or unique refs."),
        text("key", true, "Field name, such as mpn, supplier, supplier_part or description."),
        text("text", false, "New component override; omit to inherit.")],
    returns: "seq: int\nlabel: text", inputs: &[], risk: Risk::Edit, keys: "",
};
const PARTS: FieldDef = text("parts", true, "One line ID, or comma-separated paths or unique refs.");
pub const DNP_SET: ActionDef = ActionDef {
    name: "elec.bom.dnp.set", label: "Set do not populate", group: "BOM",
    help: "Omit or restore the selected parts in the factory BOM and placement file.",
    fields: &[UNIT, PARTS, FieldDef { name: "dnp", kind: FieldKind::Bool, required: true, default: "", help: "True omits the parts from assembly." }],
    returns: "seq: int\nlabel: text", inputs: &[], risk: Risk::Edit, keys: "",
};
pub const ALT_ADD: ActionDef = ActionDef {
    name: "elec.bom.alt.add", label: "Add alternate", group: "BOM",
    help: "Record an alternate for each part in its next free slot. Electrical equivalence is the person's call.",
    fields: &[UNIT, PARTS, text("mpn", true, "Alternate manufacturer part number."), text("supplier", false, "Supplier."),
        text("supplier_part", false, "Supplier part number."), text("note", false, "Note about this alternate.")],
    returns: "seq: int\nlabel: text", inputs: &[], risk: Risk::Edit, keys: "",
};
pub const ALT_USE: ActionDef = ActionDef {
    name: "elec.bom.alt.use", label: "Use alternate", group: "BOM",
    help: "Swap an alternate with the fitted sourcing fields. Electrical equivalence is the person's call.",
    fields: &[UNIT, PARTS, FieldDef { name: "alt", kind: FieldKind::Int { min: 1, max: i64::MAX }, required: true, default: "", help: "Alternate slot number." }],
    returns: "seq: int\nlabel: text", inputs: &[], risk: Risk::Edit, keys: "",
};
pub static ASSEMBLY_ACTIONS: &[ActionDef] = &[DNP_SET, ALT_ADD, ALT_USE];
pub const COST_READ: ActionDef = ActionDef {
    name: "elec.cost.read", label: "Estimate run cost", group: "BOM",
    help: "Estimate purchased parts, fabrication and assembly from dated offline sources. Unpriced lines leave a lower bound.",
    fields: &[UNIT, FieldDef { name: "boards", kind: FieldKind::Int { min: 1, max: i64::MAX }, required: true, default: "", help: "Boards in the run." },
        text("profile", false, "The loaded rate profile's name."), FieldDef { name: "assembly", kind: FieldKind::Bool, required: false,
            default: "true", help: "Include factory assembly and its attrition. False selects hand assembly." }],
    returns: "header: text\ncoverage: text\nselected: {..}  the run, its line purchases, charges and currency subtotals\ncomparisons: [{..}]  amounts in currency millionths with dated sources",
    inputs: &[], risk: Risk::Read, keys: "",
};
pub static ACTIONS: &[ActionDef] = &[BOM_READ, FIELD_SET, COST_READ];
const PAGE: usize = 20;

fn words<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    format!("[{}]", values.into_iter().map(quote).collect::<Vec<_>>().join(" "))
}

pub fn bom_read(design: &Design, args: &Args) -> Result<String, Diag> {
    BOM_READ.check_args(args)?;
    let (unit, line) = board_and_line(design, args, "only")?;
    let mut view = SheetView::default();
    view.cam.group_symbols = arg(args, "group").is_none_or(|v| v.trim() != "false");
    view.cam.filter_string = arg(args, "filter").unwrap_or("").into();
    if let Some(only) = arg(args, "only") {
        let paths = match line {
            Some(paths) => {
                let known: BTreeSet<_> = unit.circuit.comps.iter().map(|c| c.path.as_str()).collect();
                if let Some(path) = paths.iter().find(|p| !known.contains(p.as_str())) {
                    return Err(error("only", format!("no component at {path}")));
                }
                paths
            }
            None => vec![sheet::component(&unit.circuit, only.trim()).map_err(|e| error("only", e))?.path.clone()],
        };
        view.only = Some(paths.into_iter().collect());
    }
    let sheet = sheet(design, unit.id, &view).map_err(|e| error("unit", e))?;
    let after = arg(args, "after").unwrap_or("0").trim().replace('_', "").parse::<u64>().map_err(|e| error("after", e))?;
    let total = sheet.rows.len();
    let start = usize::try_from(after).unwrap_or(usize::MAX).min(total);
    let end = start.saturating_add(PAGE).min(total);
    let mut out = format!("{{total: {total}");
    if end < total { write!(out, " next: {end}").unwrap(); }
    out.push_str(" lines: [");
    for row in &sheet.rows[start..end] {
        write!(out, "{{line: {} refs: {} paths: {} qty: {} dnp: {}", quote(&row.key),
            words(row.refs.iter().map(String::as_str)), words(row.paths.iter().map(String::as_str)), row.qty, row.dnp).unwrap();
        for (name, column) in [("value", "Value"), ("footprint", "Footprint"), ("mpn", "MPN"),
            ("supplier", "Supplier"), ("supplier_part", "Supplier part"), ("description", "Description")] {
            let index = sheet.header.iter().position(|v| v == column).expect("default BOM column");
            write!(out, " {name}: {}", quote(&row.cells[index])).unwrap();
        }
        for (name, alternatives) in [("fields", false), ("alts", true)] {
            write!(out, " {name}: {{").unwrap();
            for (key, values) in row.fields.iter().filter(|(key, _)| !alternatives || sheet::alternate_key(key).is_some()) {
                write!(out, "{}: {} ", quote(key), words(values.iter().map(String::as_str))).unwrap();
            }
            out.push('}');
        }
        write!(out, " inherited: {} checks: {}", words(row.inherited.iter().map(String::as_str)), words(row.checks.iter().copied())).unwrap();
        if let Some(lifecycle) = row.lifecycle {
            write!(out, " lifecycle: {}", lifecycle.name()).unwrap();
        }
        if row.approved {
            out.push_str(" approved: true");
        }
        out.push_str("} ");
    }
    out.push_str("]}");
    Ok(out)
}

fn board_and_line<'a>(design: &'a Design, args: &Args, field: &str)
    -> Result<(&'a fab_elec_design::Unit, Option<Vec<String>>), Diag> {
    let line = arg(args, field).map(str::trim).filter(|s| s.starts_with("line:"))
        .map(sheet::line_members).transpose().map_err(|e| error(field, e))?;
    let named = arg(args, "unit");
    if line.as_ref().is_some_and(|(u, _)| named.is_some_and(|name| name != u)) {
        return Err(error("unit", "the line belongs to a different board"));
    }
    let unit = board_unit(design, "unit", named.or_else(|| line.as_ref().map(|(u, _)| u.as_str())))?;
    Ok((unit, line.map(|(_, paths)| paths)))
}

/// Where a shared call's instances appear in the canonical grouped BOM.
pub struct GroupedLines {
    lines: BTreeMap<CompRef, (std::sync::Arc<str>, bool)>,
}

fn part_refs(unit: BoardId, comps: &[CompId]) -> Vec<CompRef> {
    comps.iter().map(|&comp| CompRef { board: unit, comp }).collect()
}

impl GroupedLines {
    pub fn new(shared: &SharedCalls, unit: BoardId, sheet: &BomSheet) -> Self {
        let mut lines = BTreeMap::new();
        for row in &sheet.rows {
            let parts = part_refs(unit, &row.comps);
            let partial = shared.partial_groups(&parts);
            let mut groups = BTreeSet::new();
            for part in parts {
                if let Some(first) = shared.group(part).first() { groups.insert(*first); }
            }
            let editable = partial.is_empty();
            for group in partial { groups.remove(&group[0]); }
            if !groups.is_empty() {
                let label: std::sync::Arc<str> = row.refs.join(", ").into();
                for first in groups { lines.insert(first, (label.clone(), editable)); }
            }
        }
        Self { lines }
    }

    fn reason(&self, design: &Design, group: &[CompRef]) -> String {
        let names: Vec<_> = group.iter().map(|p| design.unit(p.board).unwrap().circuit.comps[p.comp.ix()].refdes.as_str()).collect();
        let (last, rest) = names.split_last().expect("a shared call has several instances");
        let way_out = match self.lines.get(&group[0]) {
            Some((line, true)) => format!("Edit them together on the {line} line."),
            Some((_, false)) => "Their line also contains part of another shared expression. Edit this field in the code, or split the expression.".into(),
            None => "They are on different lines here, so edit this field in the code, or split the expression.".into(),
        };
        format!("{} and {last} come from one expression in the code. {way_out}", rest.join(", "))
    }

    pub fn annotate(&self, design: &Design, unit: BoardId, shared: &SharedCalls, sheet: &mut BomSheet, grouped: bool) {
        let mut explained = BTreeSet::new();
        let mut reason = |comps: &[CompId]| {
            let groups = shared.partial_groups(&part_refs(unit, comps));
            if groups.is_empty() { return None; }
            let reasons: Vec<_> = groups.into_iter()
                .filter(|group| grouped || explained.insert(group[0]))
                .map(|group| self.reason(design, group)).collect();
            Some(if reasons.is_empty() { "Shared, see above.".into() } else { reasons.join(" ") })
        };
        for row in &mut sheet.rows {
            row.read_only = reason(&row.comps);
            for alt in &mut row.alternates { alt.read_only = reason(&alt.comps); }
        }
    }
}

struct Targets<'a> {
    unit: &'a fab_elec_design::Unit,
    parts: Vec<&'a Component>,
    /// A representative component for each shared call, within this unit.
    group: BTreeMap<CompId, CompId>,
}

impl Targets<'_> {
    /// If one instance needs a change, include every selected instance of its call for
    /// that key. The splicer checks both complete membership and identical intentions.
    fn edit(&self, desired: Vec<CircuitOp>, verb: &str) -> Result<Plan, Diag> {
        let mut dirty = BTreeSet::new();
        let identify = |op: &CircuitOp| match op {
            CircuitOp::SetField { comp, key, .. } => (*comp, Some(key.clone())),
            CircuitOp::SetDnp { comp, .. } => (*comp, None),
            _ => unreachable!("BOM edits change fields or DNP"),
        };
        for op in &desired {
            let (comp, key) = identify(op);
            let component = &self.unit.circuit.comps[comp.ix()];
            let changed = match op {
                CircuitOp::SetField { key, text, .. } => component.fields.get(key) != text.as_deref(),
                CircuitOp::SetDnp { dnp, .. } => component.dnp != *dnp,
                _ => unreachable!(),
            };
            if changed { dirty.insert((*self.group.get(&comp).unwrap_or(&comp), key)); }
        }
        let ops = desired.into_iter().filter(|op| {
            let (comp, key) = identify(op);
            dirty.contains(&(*self.group.get(&comp).unwrap_or(&comp), key))
        }).map(|op| DesignOp::Circuit { unit: self.unit.id, op }).collect::<Vec<_>>();
        if ops.is_empty() { return Err(error("parts", "the selected components already have that state")); }
        let refs = self.parts.iter().take(2).map(|c| c.refdes.as_str()).collect::<Vec<_>>().join(", ");
        let mut label = format!("{verb} of {refs}");
        if self.parts.len() > 2 { write!(label, " (+{})", self.parts.len() - 2).unwrap(); }
        Ok(Plan::Edit(Resolved { ops, place: vec![], label }))
    }
}

fn targets<'a>(design: &'a Design, docs: Option<&Docs>, args: &Args) -> Result<Targets<'a>, Diag> {
    let (unit, line) = board_and_line(design, args, "parts")?;
    let targets = if let Some(paths) = line {
        let by_path: BTreeMap<_, _> = unit.circuit.comps.iter().map(|c| (c.path.as_str(), c)).collect();
        paths.iter().map(|p| by_path.get(p.as_str()).copied().ok_or_else(|| error("parts", format!("no circuit component at {p}"))))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        arg(args, "parts").unwrap().split(',').map(|p| sheet::component(&unit.circuit, p.trim()).map_err(|e| error("parts", e))).collect::<Result<Vec<_>, _>>()?
    };
    let mut seen = BTreeSet::new();
    let targets: Vec<_> = targets.into_iter().filter(|c| seen.insert(c.id)).collect();
    let mut group = BTreeMap::new();
    if let Some(docs) = docs {
        let calls = fab_elec_lang::splice::SharedCalls::new(design, docs.map());
        let parts = targets.iter().map(|c| CompRef { board: unit.id, comp: c.id }).collect::<Vec<_>>();
        let partial = calls.partial_groups(&parts);
        if !partial.is_empty() {
            let grouped = sheet(design, unit.id, &SheetView::default()).map_err(|e| error("unit", e))?;
            let lines = GroupedLines::new(&calls, unit.id, &grouped);
            let selected: BTreeSet<_> = parts.iter().copied().collect();
            let mut reasons = Vec::new();
            for group in partial {
                let path = |p: &CompRef| {
                    let unit = design.unit(p.board).unwrap();
                    let c = &unit.circuit.comps[p.comp.ix()];
                    format!("{} ({}/{})", c.refdes, unit.name, c.path)
                };
                let omitted = group.iter().filter(|p| !selected.contains(p)).map(path).collect::<Vec<_>>();
                let requested = group.iter().filter(|p| selected.contains(p)).map(path).collect::<Vec<_>>();
                reasons.push(format!("{} Selected: {}. Left out: {}.", lines.reason(design, group), requested.join(", "), omitted.join(", ")));
            }
            return Err(error("parts", reasons.join(" ")));
        }
        for part in parts {
            if let Some(first) = calls.group(part).first() { group.insert(part.comp, first.comp); }
        }
    }
    Ok(Targets { unit, parts: targets, group })
}

pub fn field_set(design: &Design, docs: Option<&Docs>, tool: &str, args: &Args, by: By) -> Result<Plan, Diag> {
    check_tool(tool, &FIELD_SET, args)?;
    let key = arg(args, "key").unwrap().trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') ||
        ["value", "footprint", "refs", "reference", "qty", "quantity", "dnp"].iter().any(|s| key.eq_ignore_ascii_case(s)) {
        return Err(error("key", "choose an editable text field such as mpn, supplier or supplier_part"));
    }
    let text = arg(args, "text");
    crate::sheet_actions::check_part_field(key, text, by)?;
    let targets = targets(design, docs, args)?;
    let mut ops = Vec::new();
    for c in &targets.parts {
        // CAM reads Description before description. Override the actual inherited spelling.
        let field = if key.eq_ignore_ascii_case("description") {
            if c.fields.get("Description").or(c.part.fields.get("Description")).is_some() { "Description" } else { "description" }
        } else { key };
        ops.push(CircuitOp::SetField { comp: c.id, key: field.into(), text: text.map(str::to_owned) });
    }
    targets.edit(ops, &format!("{} {key}", if text.is_some() { "Set" } else { "Clear" }))
}

pub fn dnp_set(design: &Design, docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &DNP_SET, args)?;
    let dnp = arg(args, "dnp").unwrap().trim() == "true";
    let targets = targets(design, docs, args)?;
    let mut ops = Vec::new();
    for c in &targets.parts {
        ops.push(CircuitOp::SetDnp { comp: c.id, dnp });
    }
    targets.edit(ops, if dnp { "Set DNP" } else { "Clear DNP" })
}

pub fn alt_add(design: &Design, docs: Option<&Docs>, tool: &str, args: &Args, by: By) -> Result<Plan, Diag> {
    check_tool(tool, &ALT_ADD, args)?;
    if arg(args, "mpn").unwrap().trim().is_empty() { return Err(error("mpn", "enter a manufacturer part number")); }
    let targets = targets(design, docs, args)?;
    let author = match by {
        By::Ai { .. } => Some("AI"),
        By::Agent { .. } => Some("agent"),
        By::Person | By::Disk => None,
    };
    let mut ops = Vec::new();
    for c in &targets.parts {
        let used = c.fields.0.iter().chain(&c.part.fields.0).filter_map(|(k, _)| sheet::alternate_key(k).map(|(n, _)| n)).collect::<BTreeSet<_>>();
        let mut slot = 1;
        for n in used {
            if n != slot { break; }
            slot += 1;
        }
        for key in ["mpn", "supplier", "supplier_part", "note", "by"] {
            let text = if key == "by" { author } else { arg(args, key) };
            if let Some(value) = text {
                ops.push(CircuitOp::SetField { comp: c.id, key: format!("alt.{slot}.{key}"), text: Some(value.into()) });
            }
        }
    }
    targets.edit(ops, "Add alternate")
}

pub fn alt_use(design: &Design, docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    check_tool(tool, &ALT_USE, args)?;
    let slot: i64 = arg(args, "alt").unwrap().trim().replace('_', "").parse().map_err(|e| error("alt", e))?;
    let targets = targets(design, docs, args)?;
    let mut ops = Vec::new();
    for c in &targets.parts {
        let value = |key: &str| c.fields.get(key).or_else(|| c.part.fields.get(key));
        if value(&format!("alt.{slot}.mpn")).is_none() {
            return Err(error("alt", format!("{} has no alternate {slot}", c.path)));
        }
        // Unspecified sourcing on the new part must also mask an old library default.
        let mut set = |key: String, value: Option<&str>| {
            let text = value.or_else(|| c.part.fields.get(&key).filter(|v| !v.is_empty()).map(|_| ""));
            ops.push(CircuitOp::SetField { comp: c.id, key, text: text.map(str::to_owned) });
        };
        let legacy = c.part.fields.0.iter().chain(&c.fields.0).map(|(k, _)| k.as_str())
            .filter(|k| k.eq_ignore_ascii_case("lcsc")).min().unwrap_or("lcsc");
        for (suffix, fitted) in [("mpn","mpn"), ("supplier","supplier"), ("supplier_part","supplier_part"),
            ("lcsc",legacy), ("note","alt.fitted.note"), ("by","alt.fitted.by")] {
            let other = format!("alt.{slot}.{suffix}");
            let previous = value(fitted).or_else(|| (suffix == "mpn").then_some(""));
            set(fitted.into(), value(&other));
            set(other, previous);
        }
    }
    targets.edit(ops, &format!("Use alternate {slot}"))
}

fn figure(f: &Figure) -> String {
    format!("{{micros: {} currency: {} source: {} date: {}}}", f.micros, quote(&f.currency), quote(&f.source), quote(&f.date))
}
fn run(boards: i64, result: &Result<Run, String>) -> String {
    match result {
        Err(why) => format!("{{boards: {boards} unavailable: {}}}", quote(why)),
        Ok(r) => {
            let list = |v: &[Figure]| v.iter().map(figure).collect::<Vec<_>>().join(" ");
            let surcharges = r.surcharges.iter().map(|(n, v)| format!("{{name: {} fee: {}}}", quote(n), figure(v))).collect::<Vec<_>>().join(" ");
            format!("{{boards: {boards} setup: {} each: {} surcharges: [{surcharges}] totals: [{}] per_board: [{}]}}",
                figure(&r.setup), figure(&r.each), list(&r.totals), list(&r.per_board))
        }
    }
}

fn amount(a:&fab_elec_bom::cost::Amount) -> String {
    let source = |s:&Source| {
        let url=s.url.as_ref().map_or(String::new(),|u|format!(" url: {}",quote(u)));
        format!("source: {} date: {}{url}",quote(&s.name),quote(&s.date))
    };
    let provenance = if a.sources.len()==1 { source(a.sources.first().unwrap()) }
        else { format!("sources: [{}]",a.sources.iter().map(|s| format!("{{{}}}",source(s))).collect::<Vec<_>>().join(" ")) };
    format!("{{micros: {} currency: {} {provenance}}}",a.micros,quote(&a.currency))
}
fn purchase(p:&fab_elec_bom::cost::Purchase) -> String {
    let opt=|f:&Option<fab_elec_bom::cost::Amount>| f.as_ref().map_or("null".into(),amount);
    let count=|n:Option<i128>| n.map_or("null".into(),|v| v.to_string());
    format!("{{needed: {} spares: {} ordered: {} unit: {} bought: {} used_per_board: {} notes: {}}}",p.needed,count(p.spares),count(p.ordered),
        opt(&p.unit),opt(&p.bought),opt(&p.used_per_board),words(p.notes.iter().map(String::as_str)))
}

fn cost_run(r:&fab_elec_bom::cost::CostRun, detailed:bool) -> String {
    let amounts=|v:&[fab_elec_bom::cost::Amount]| v.iter().map(amount).collect::<Vec<_>>().join(" ");
    let mut out=format!("{{boards: {} priced: {} fitted: {} lower_bound: {} totals: [{}] per_board: [{}]",
        r.boards,r.priced,r.fitted,r.lower_bound,amounts(&r.totals),amounts(&r.per_board));
    if detailed {
        write!(out," parts: [{}] fabrication: {} dnp: {} lines: [",amounts(&r.parts),run(r.boards,&r.fabrication),words(r.dnp.iter().map(String::as_str))).unwrap();
        for l in &r.lines {
            let alts=l.alternates.iter().map(|(k,i,p)| format!("{{line: {} alt: {i} purchase: {}}}",quote(k),purchase(p))).collect::<Vec<_>>().join(" ");
            write!(out,"{{line: {} refs: {} purchase: {} alternates: [{alts}]}} ",quote(&l.key),words(l.refs.iter().map(String::as_str)),
                l.fitted.as_ref().map_or("null".into(),purchase)).unwrap();
        }
        out.push_str("] assembly: ");
        match &r.assembly {
            Err(s)=>write!(out,"{{unavailable: {}}}",quote(s)).unwrap(),
            Ok(a)=> {
                let fees=a.charges.iter().map(|(n,f)| format!("{{name: {} amount: {}}}",quote(n),amount(f))).collect::<Vec<_>>().join(" ");
                write!(out,"{{charges: [{fees}] total: {} assumptions: {}}}",amount(&a.total),words(a.assumptions.iter().map(String::as_str))).unwrap();
            }
        }
        if let Some(x) = &r.exchange {
            write!(out," exchange: {{from: {} to: {} micros_per_unit: {} source: {} date: {}}}",quote(&x.from),quote(&x.to),x.rate,quote(&x.source.name),quote(&x.source.date)).unwrap();
        }
        if let Some(extra)=&r.extra { write!(out," flat: {}",amount(extra)).unwrap(); }
    }
    out.push('}');
    out
}

pub fn cost_read(design:&Design,rates:&Rates,library:&Library,settings:&fab_elec_bom::cost::Options,args:&Args) -> Result<String,Diag> {
    COST_READ.check_args(args)?;
    let unit=board_unit(design,"unit",arg(args,"unit"))?;
    if arg(args,"profile").is_some_and(|p| p!=rates.name) { return Err(error("profile",format!("loaded profile is {}",rates.name))); }
    let boards=arg(args,"boards").unwrap().trim().replace('_',"").parse().map_err(|e| error("boards",e))?;
    let board=unit.board.as_ref().unwrap();
    let sheet=sheet(design,unit.id,&SheetView::default()).map_err(|e| error("unit",e))?;
    let mut options=settings.clone();
    if let Some(v)=arg(args,"assembly") { options.hand=v.trim()=="false"; }
    let est=fab_elec_bom::cost::estimate(&sheet,&BoardFacts::from_board(board),&assembly_placements(board,&unit.circuit),&library.prices,rates,boards,&options);
    let selected=est.selected.map_err(|e| error("boards",e))?;
    let comparisons=est.comparisons.iter().map(|(q,r)| match r { Ok(r)=>cost_run(r,false), Err(e)=>format!("{{boards: {q} unavailable: {}}}",quote(e)) }).collect::<Vec<_>>().join(" ");
    Ok(format!("{{header: {} coverage: {} selected: {} comparisons: [{comparisons}]}}",quote(est.header),quote(est.excludes),cost_run(&selected,true)))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cmd, Engine, Mailbox, Ticket};
    use fab_elec_board::Factories;
    use fab_elec_design::By;
    use std::sync::Arc;

    #[test]
    fn ledctl_read_group_commit_undo_and_cost() {
        let docs = fab_elec_fixtures::docs("ledctl").unwrap();
        let (design, docs) = Docs::from_evaluated(docs, fab_elec_fixtures::ledctl_eval());
        let args = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let unit = &design.units[0];
        let sheet = sheet(&design, unit.id, &SheetView::default()).unwrap();
        let group = sheet.rows.iter().find(|r| r.paths.iter().any(|p| p == "reg.c_in")).unwrap();
        assert!(group.paths.iter().any(|p| p == "reg.c_out"));
        let one_group = bom_read(&design, &args(&[("only", &group.key)])).unwrap();
        assert!(one_group.starts_with("{total: 1 ") && one_group.contains(&quote(&group.key)));
        let grouped = bom_read(&design, &[]).unwrap();
        assert!(grouped.contains(&quote(&group.key)) && grouped.contains("no MPN"), "{grouped}");
        let first = bom_read(&design, &args(&[("group", "false")])).unwrap();
        let second = bom_read(&design, &args(&[("group", "false"), ("after", &PAGE.to_string())])).unwrap();
        let total = sheet.rows.iter().map(|r| r.qty).sum::<usize>();
        assert!(first.contains(&format!("total: {total} next: {PAGE}")));
        assert_eq!((first.matches("{line:").count(), second.matches("{line:").count()), (PAGE, total - PAGE));
        assert!(!second.contains(" next:"));
        let mut duplicate = design.clone();
        let circuit = Arc::make_mut(&mut duplicate.units[0].circuit);
        for path in ["reg.c_in", "reg.c_out"] {
            let id = circuit.comp(path).unwrap().id;
            circuit.comps[id.ix()].refdes = "Cap.Pair".into();
        }
        let read = bom_read(&duplicate, &args(&[("only", "Cap.Pair")])).unwrap_err();
        let edit = field_set(&duplicate, None, FIELD_SET.name, &args(&[("parts", "Cap.Pair"), ("key", "mpn")]), By::Person).unwrap_err();
        for failure in [read, edit] {
            assert!(failure.message.contains("path:reg.c_in") && failure.message.contains("path:reg.c_out"), "{failure:?}");
        }
        assert!(bom_read(&duplicate, &args(&[("only", "path:reg.c_in")])).unwrap().contains("qty: 1"));
        let estimate = cost_read(&design, &Rates::example(), &Library::default(), &Default::default(), &args(&[("boards", "10")])).unwrap();
        let figures = estimate.matches("micros:").count();
        assert!(figures >= 5 && estimate.contains("Estimate, not a quote"));
        assert_eq!(estimate.matches("source: \"example, edit me\" date: \"2026-11-24\"").count(), figures);
        let circuit = unit.circuit.clone();
        let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::default(), Arc::default(), "BOM".into())
            .with_docs(docs).with_bom_tools();
        let mailbox = Mailbox::default();
        let apply = |engine: &mut Engine, cmd| {
            mailbox.push(cmd);
            let mut step = engine.step(mailbox.take(), &mut |_| {});
            loop {
                assert!(step.rejected.is_empty(), "{:?}", step.rejected);
                if step.evals.is_empty() { break; }
                for job in step.evals { mailbox.result(job.run()); }
                step = engine.step(mailbox.take(), &mut |_| {});
            }
        };
        apply(&mut engine, Cmd::Invoke { tool: FIELD_SET.name.into(), args: args(&[("parts", &group.key), ("key", "mpn"), ("text", "DEMO")]),
            by: By::Person, preview: false, ticket: Ticket(1) });
        assert_eq!(engine.log().entries.len(), 1);
        assert!(engine.log().entries[0].label.starts_with("Set mpn of "));
        for c in &group.comps {
            assert_eq!(engine.design().units[0].circuit.comps[c.ix()].fields.get("mpn"), Some("DEMO"));
        }
        apply(&mut engine, Cmd::Undo);
        assert_eq!(engine.design().units[0].circuit, circuit, "one undo restores every component override");
        let factory_has = |design: &Design, reference: &str, fitted: bool| {
            use fab_elec_cam::{profile::{FileKind, ResolvedProfile}, tables::{fill_generic_tables, PlacementRules}, ReadBack};
            let unit = &design.units[0];
            let board = unit.board.as_deref().unwrap();
            assert!(board.placed.iter().any(|p| p.refdes == reference));
            let mut profile = ResolvedProfile::standard_2_layer("ledctl", "A");
            profile.files.retain(|f| matches!(f.kind, FileKind::Bom | FileKind::Placement));
            fill_generic_tables(&mut profile, board, &unit.circuit, &PlacementRules::default());
            let export = fab_elec_cam::export(board, &unit.circuit, &[], &profile).unwrap();
            assert!(export.validation.ok(), "{:?}", export.validation.failures().collect::<Vec<_>>());
            for kind in [FileKind::Bom, FileKind::Placement] {
                let col = if kind == FileKind::Bom { 1 } else { 0 };
                let (_, _, ReadBack::Table(rows)) = export.read_back.iter().find(|(_, k, _)| *k == kind).unwrap() else { panic!("expected a table") };
                assert_eq!(rows.iter().skip(1).any(|r| r[col].split(',').any(|r| r == reference)), fitted, "{kind:?}");
            }
        };
        let invoke = |tool: &str, pairs: &[(&str, &str)], by| Cmd::Invoke {
            tool: tool.into(), args: args(pairs), by, preview: false, ticket: Ticket(2),
        };
        apply(&mut engine, invoke(DNP_SET.name, &[("parts", "C1"), ("dnp", "true")], By::Person));
        assert_eq!(engine.log().head, 1);
        assert!(engine.design().units[0].circuit.comps.iter().any(|c| c.refdes == "C1" && c.dnp));
        factory_has(engine.design(), "C1", false);
        apply(&mut engine, Cmd::Undo);
        assert_eq!(engine.design().units[0].circuit, circuit);
        factory_has(engine.design(), "C1", true);
        let leds = sheet.rows.iter().find(|r| r.refs.first().is_some_and(|r| r == "D1")).unwrap();
        assert_eq!(leds.qty, 8);
        let refusal = field_set(engine.design(), engine.docs(), FIELD_SET.name,
            &args(&[("parts", "D1,D2"), ("key", "mpn"), ("text", "fitted")]), By::Person).unwrap_err();
        assert!(refusal.message.contains("D1, D2, D3, D4, D5, D6, D7 and D8 come from one expression in the code."));
        assert!(refusal.message.contains("Edit them together on the D1, D2, D3, D4, D5, D6, D7, D8 line."));
        assert!(refusal.message.contains("Selected: D1 (ledctl/lamps[0].d)"));
        for n in 3..=8 { assert!(refusal.message.contains(&format!("D{n} (")), "{refusal:?}"); }

        let unit = &engine.design().units[0];
        let shared = SharedCalls::new(engine.design(), engine.docs().unwrap().map());
        let lines = GroupedLines::new(&shared, unit.id, &sheet);
        let mut individual = SheetView::default();
        individual.cam.group_symbols = false;
        let mut singles = fab_elec_bom::sheet(engine.design(), unit.id, &individual).unwrap();
        lines.annotate(engine.design(), unit.id, &shared, &mut singles, false);
        let reason = |name: &str| singles.rows.iter().find(|r| r.refs[0] == name).unwrap().read_only.as_deref().unwrap();
        assert!(reason("C3").starts_with("C3, C4 and C5 come from one expression in the code. They are on different lines here"));
        assert!(reason("D1").contains("Edit them together on the D1, D2, D3, D4, D5, D6, D7, D8 line."));
        for name in ["C4", "C5", "D2", "D8"] { assert_eq!(reason(name), "Shared, see above."); }

        let fitted = |key, text| invoke(FIELD_SET.name, &[("parts", &leds.key), ("key", key), ("text", text)], By::Person);
        apply(&mut engine, fitted("mpn", "original"));
        assert_eq!((engine.log().head, engine.log().entries[0].text.edits.len()), (1, 1));
        let ai = By::Ai { turn: 3, call: 7 };
        apply(&mut engine, invoke(ALT_ADD.name, &[("parts",&leds.key), ("mpn","spare"), ("supplier","source"), ("note","check voltage")], ai));
        assert_eq!(engine.log().head, 2);
        assert_eq!(engine.log().entries[1].text.edits.len(), 1);
        assert_eq!(engine.log().entries[1].by, ai);
        let alternates = fab_elec_bom::sheet(engine.design(), engine.design().units[0].id, &SheetView::default()).unwrap();
        let choice = &alternates.rows.iter().find(|r| r.key == leds.key).unwrap().alternates[0];
        assert!(choice.ai && choice.key == leds.key && choice.note == "check voltage");
        let added = engine.design().units[0].circuit.clone();
        for &comp in &leds.comps {
            let fields = &added.comps[comp.ix()].fields;
            assert_eq!(fields.get("alt.1.mpn"), Some("spare"));
            assert_eq!(fields.get("alt.1.by"), Some("AI"));
            assert_eq!(fields.get("mpn"), Some("original"));
        }
        apply(&mut engine, invoke(ALT_USE.name, &[("parts",&leds.key), ("alt","1")], By::Person));
        assert_eq!((engine.log().head, engine.log().entries[2].text.edits.len()), (3, 1));
        for &comp in &leds.comps {
            let fields = &engine.design().units[0].circuit.comps[comp.ix()].fields;
            assert_eq!(fields.get("mpn"), Some("spare"));
            assert_eq!(fields.get("supplier"), Some("source"));
            assert_eq!(fields.get("alt.1.mpn"), Some("original"));
            assert_eq!(fields.get("alt.fitted.note"), Some("check voltage"));
            assert_eq!(fields.get("alt.1.by"), None);
        }
        apply(&mut engine, Cmd::Undo);
        assert_eq!(engine.design().units[0].circuit, added);
        apply(&mut engine, Cmd::Undo);
        assert!(engine.design().units[0].circuit.comps.iter().all(|c| c.fields.get("alt.1.mpn").is_none()));
        apply(&mut engine, Cmd::Undo);
        assert_eq!(engine.design().units[0].circuit, circuit);
        apply(&mut engine, invoke(DNP_SET.name, &[("parts",&leds.key), ("dnp","true")], By::Person));
        assert_eq!((engine.log().head, engine.log().entries[0].text.edits.len()), (1, 1));
        assert!(leds.comps.iter().all(|c| engine.design().units[0].circuit.comps[c.ix()].dnp));
        apply(&mut engine, Cmd::Undo);
        assert_eq!(engine.design().units[0].circuit, circuit);
        // Same fitted line, different existing alternates: each member uses its own next slot.
        apply(&mut engine, invoke(ALT_ADD.name, &[("parts","reg.c_in"), ("mpn","first")], By::Agent { call: 8 }));
        assert_eq!(engine.design().units[0].circuit.comp("reg.c_in").unwrap().fields.get("alt.1.by"), Some("agent"),
            "the alternate must keep the outside agent's attribution");
        apply(&mut engine, invoke(ALT_ADD.name, &[("parts",&group.key), ("mpn","second")], By::Person));
        let options = fab_elec_bom::sheet(engine.design(), engine.design().units[0].id, &SheetView::default()).unwrap();
        let options = &options.rows.iter().find(|r| r.key == group.key).unwrap().alternates;
        let slots: Vec<_> = options.iter().filter(|o| o.mpn == "second").collect();
        assert_eq!(slots.iter().map(|o| o.number).collect::<Vec<_>>(), [1, 2]);
        assert!(slots.iter().all(|o| !o.ai && o.comps.len() == 1));
        apply(&mut engine, invoke(ALT_USE.name, &[("parts",&slots[0].key), ("alt","1")], By::Person));
        let changed = &engine.design().units[0].circuit;
        assert_eq!(changed.comp("reg.c_out").unwrap().fields.get("mpn"), Some("second"));
        assert_eq!(changed.comp("reg.c_in").unwrap().fields.get("mpn"), None);



    }
}

pub const PRICE_SET: ActionDef = ActionDef {
    name: "elec.price.set", label: "Save part price", group: "BOM",
    help: "Write a dated local price. Every tool write needs the person's approval and a source URL.",
    fields: &[text("mpn",false,"MPN key, or supply a supplier and supplier part."),
        text("supplier",false,"Supplier for a supplier-part key."), text("supplier_part",false,"Supplier part number."),
        text("breaks",true,"Quantity=unit price pairs, or a list of objects with min and micros."),
        text("currency",true,"Three-letter currency code."), text("moq",false,"Minimum order, default 1."),
        text("multiple",false,"Order multiple, default 1."), text("source",false,"Person's price source. Tool writes are labelled AI lookup or Agent lookup."),
        text("url",false,"Source URL, required for a tool lookup.")],
    returns: "ok: bool", inputs: &[], risk: Risk::Library, keys: "",
};

use fab_elec_bom::prices::{breaks_from_text, Entry, Library, Source, Sourcing};
use std::path::Path;

pub enum PriceRequest {
    Person(PriceWrite),
    Review(PriceProposal),
}

/// Only the app's response to the person's approval consumes a pending proposal.
pub struct PriceProposal(Entry);
impl PriceProposal {
    pub fn entry(&self) -> &Entry { &self.0 }
    pub fn accept(self) -> PriceWrite { PriceWrite(self.0) }
}
pub struct PriceWrite(Entry);

pub fn price_request(args:&Args, by:By, date:&str) -> Result<PriceRequest,String> {
    PRICE_SET.check_args(args).map_err(|d| d.message)?;
    let field = |k| arg(args,k).unwrap_or("").trim();
    let part = Sourcing { mpn:field("mpn").into(), supplier:field("supplier").into(), supplier_part:field("supplier_part").into() };
    let key = match (part.mpn_key(),part.supplier_key()) {
        (Some(k),None) if part.supplier.is_empty() && part.supplier_part.is_empty() => k,
        (None,Some(k)) => k,
        _ => return Err("choose one MPN or one supplier and supplier part".into()),
    };
    let currency = field("currency");
    if currency.len()!=3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) { return Err("enter a three-letter uppercase currency code".into()); }
    let positive = |k| arg(args,k).unwrap_or("1").trim().parse::<i64>().ok().filter(|n| *n>0).ok_or_else(|| format!("{k} must be a positive integer"));
    let source = match by {
        By::Ai { .. } => "AI lookup",
        By::Agent { .. } => "Agent lookup",
        By::Person | By::Disk if field("source").is_empty() => "User entry",
        By::Person | By::Disk => field("source"),
    };
    let url=arg(args,"url").map(str::trim).filter(|s| !s.is_empty());
    if by.is_tool() && !url.is_some_and(|u| ["https://","http://"].into_iter().filter_map(|p| u.strip_prefix(p))
        .any(|s| !s.split('/').next().unwrap_or("").is_empty() && !s.chars().any(char::is_whitespace))) {
        return Err(format!("{source} needs the source HTTP or HTTPS URL"));
    }
    let e=Entry { key,currency:currency.into(),breaks:breaks_from_text(field("breaks"))?,moq:positive("moq")?,multiple:positive("multiple")?,
        source:Source {name:source.into(),date:date.into(),url:url.map(str::to_owned)},assembly_class:None };
    Ok(if by.is_tool() { PriceRequest::Review(PriceProposal(e)) } else { PriceRequest::Person(PriceWrite(e)) })
}

pub fn read_library(dir:&Path) -> Result<Library,String> {
    match std::fs::read_to_string(dir.join("library.json")) {
        Ok(text) => Library::from_text(&text).map_err(|e| format!("price library: {e}")),
        Err(e) if e.kind()==std::io::ErrorKind::NotFound => Ok(Library::default()),
        Err(e) => Err(format!("read price library: {e}")),
    }
}

/// A single rename publishes both the prices and the confirmed import mapping. The lock
/// spans read/modify/replace, including other Fab processes sharing the same user folder.
fn update_library(dir:&Path, change:impl FnOnce(&mut Library)) -> Result<Library,String> {
    use std::io::Write;
    let io = |e:std::io::Error| format!("write price library: {e}");
    std::fs::create_dir_all(dir).map_err(io)?;
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join("write.lock")).map_err(io)?;
    lock.lock().map_err(io)?;
    let mut library=read_library(dir)?;
    change(&mut library);
    let tmp=dir.join("library.tmp");
    // Replace a previous interrupted write; never follow its temporary file.
    match std::fs::remove_file(&tmp) { Ok(()) => (),Err(e) if e.kind()==std::io::ErrorKind::NotFound => (),Err(e) => return Err(io(e)) }
    let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&tmp).map_err(io)?;
    file.write_all(library.to_text().as_bytes()).and_then(|()| file.sync_all()).map_err(io)?;
    drop(file);
    std::fs::rename(tmp,dir.join("library.json")).map_err(io)?;
    Ok(library)
}

pub fn write_price(dir:&Path, write:PriceWrite) -> Result<Library,String> {
    update_library(dir, |book| {
        let mut entry = write.0;
        if let Some(old) = book.prices.0.get(&entry.key) { entry.assembly_class.clone_from(&old.assembly_class); }
        book.prices.0.insert(entry.key.clone(), entry);
    })
}

pub fn assembly_placements(board:&fab_elec_board::Board, circuit:&fab_elec_circuit::Circuit) -> fab_elec_bom::cost::Placements {
    use fab_elec_parts::PadKind;
    board.placed.iter().filter(|p| !circuit.comps.get(p.comp.ix()).is_some_and(|c| c.dnp))
        .map(|p| (p.path.clone(),(p.side,p.footprint.pads.iter()
        .filter(|pad| matches!(pad.kind,PadKind::Smd|PadKind::Thru)).count()))).collect()
}

pub fn write_settings(dir: &Path, settings: fab_elec_bom::prices::Settings, date: &str) -> Result<Library,String> {
    // Validate first. Preserve the existing date on each unchanged setting under the write lock.
    settings.clone().dated(&Default::default(),date).options()?;
    update_library(dir, |book| book.settings = settings.dated(&book.settings,date))
}

pub fn write_import(dir:&Path, import:fab_elec_bom::import::Import) -> Result<Library,String> {
    update_library(dir,|book| {
        for entry in import.entries { book.prices.0.insert(entry.key.clone(),entry); }
        book.mappings.insert(import.header,import.mapping);
    })
}

#[cfg(test)]
mod price_tests {
    use super::*;
    #[test]
    fn each_ai_write_needs_its_own_accepted_proposal_and_keeps_its_url() {
        let dir=std::env::temp_dir().join(format!("fab-bom3-prices-{}-{}",std::process::id(),std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let args=|pairs:&[(&str,&str)]| pairs.iter().map(|(k,v)| (k.to_string(),v.to_string())).collect::<Vec<_>>();
        let mut fields=args(&[("mpn","TEST-CAP"),("breaks","1=0.80"),("currency","USD"),("source","receipt")]);
        let ai=By::Ai{turn:1,call:2};
        assert!(price_request(&fields,ai,"2026-11-25").is_err());
        let agent = By::Agent { call: 3 };
        assert!(matches!(price_request(&fields,agent,"2026-11-25"), Err(why) if why.contains("URL")),
            "an agent must supply a source URL even when it supplies a source name");
        let PriceRequest::Person(write)=price_request(&fields,By::Person,"2026-11-25").unwrap() else {panic!()};
        write_price(&dir,write).unwrap();
        fields.push(("url".into(),"https://example.invalid/part".into()));
        let PriceRequest::Review(proposal) = price_request(&fields,agent,"2026-11-25").unwrap() else {
            panic!("an agent's price must wait for the person's approval");
        };
        assert_eq!(proposal.entry().source.name, "Agent lookup", "the review must identify the outside agent's lookup");
        fields.iter_mut().find(|f| f.0=="breaks").unwrap().1="1=0.60".into();
        for accept in [false,true] {
            let PriceRequest::Review(proposal)=price_request(&fields,ai,"2026-11-26").unwrap() else {panic!()};
            assert_eq!(read_library(&dir).unwrap().prices.0.values().next().unwrap().breaks[&1],800_000);
            if accept { write_price(&dir,proposal.accept()).unwrap(); }
        }
        let library=read_library(&dir).unwrap();
        let e=library.prices.0.values().next().unwrap();
        assert_eq!((&*e.source.name,&*e.source.date,e.source.url.as_deref()),("AI lookup","2026-11-26",Some("https://example.invalid/part")));
        let mut design=fab_elec_fixtures::ledctl();
        std::sync::Arc::make_mut(&mut design.units[0].circuit).comps.iter_mut().find(|c| c.path=="reg.c_in").unwrap().fields.0.push(("mpn".into(),"TEST-CAP".into()));
        let answer=cost_read(&design,&Rates::example(),&library,&Default::default(),&args(&[("boards","10"),("assembly","false")])).unwrap();
        assert!(answer.contains("micros: 6000000 currency: \"USD\" source: \"AI lookup\" date: \"2026-11-26\" url: \"https://example.invalid/part\""));
        assert!(answer.contains("lower_bound: true") && answer.contains("priced: 1"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod field_authority_tests {
    use super::*;
    use crate::sheet_actions::part_field;

    fn args(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn ai_cannot_set_approved_through_field_actions() {
        let design = fab_elec_fixtures::ledctl();
        let part = |key: &str, text: &str, by| part_field(&design, None, "elec.part.field", &args(&[("part", "J1"), ("key", key), ("text", text)]), by);
        let bom = |key: &str, text: &str, by| field_set(&design, None, FIELD_SET.name, &args(&[("parts", "J1"), ("key", key), ("text", text)]), by);
        let refused = "approved is set by the person on the part definition";
        for by in [By::Ai { turn: 1, call: 2 }, By::Agent { call: 3 }, By::Disk] {
            for key in ["approved", "Approved"] {
                let message = part(key, "true", by).unwrap_err().message;
                assert!(message.contains(refused), "elec.part.field {key} by {by:?}: {message}");
                let message = bom(key, "true", by).unwrap_err().message;
                assert!(message.contains(refused), "elec.bom.field.set {key} by {by:?}: {message}");
            }
        }
        let guidance = "approved is set on the part definition (approved: true), not as a component field";
        for key in ["approved", "Approved"] {
            assert!(part(key, "true", By::Person).unwrap_err().message.contains(guidance), "part field must direct the person to the definition");
            assert!(bom(key, "true", By::Person).unwrap_err().message.contains(guidance), "BOM field must direct the person to the definition");
        }
        for by in [By::Ai { turn: 1, call: 2 }, By::Person] {
            let message = part("lifecycle", "eol", by).unwrap_err().message;
            assert!(message.contains("a lifecycle is active, nrnd or obsolete"), "{message}");
            let message = bom("lifecycle", "eol", by).unwrap_err().message;
            assert!(message.contains("a lifecycle is active, nrnd or obsolete"), "{message}");
            for name in ["active", "nrnd", "obsolete"] {
                assert!(part("lifecycle", name, by).is_ok(), "elec.part.field lifecycle {name} by {by:?}");
                assert!(bom("lifecycle", name, by).is_ok(), "elec.bom.field.set lifecycle {name} by {by:?}");
            }
        }
    }
}

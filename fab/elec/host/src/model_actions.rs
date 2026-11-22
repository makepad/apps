//! A part's 3D model in the action catalog (EKMODELS), as its card shows and switches it: what it
//! says about the model (`elec.part.model`), the part's body source, KiCad's model, EasyEDA's or
//! its plain body (`elec.part.body.set`, a design edit: the card's switch makes the same commit),
//! and this computer's choice of where it gets a KiCad model (`elec.part.model.from`, the card's
//! other switch, no design change). A source that is off in 3D Model Sources is refused: only the
//! person turns sources on, there.
//!
//! Cost: a part lookup by name and a few model-board lookups per call.

use crate::{Docs, Plan};
use fab_elec_core::Diag;
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::{Design, Resolved, Unit};
use fab_elec_lang::map::SourceMap;
use fab_elec_parts::model_board::{self, Choice, Family};
use fab_elec_view::explorer::body_source::{body_choices, body_plan, BodyPick};
use makepad_widgets_core::makepad_script::syntax::write::quote;
use std::fmt::Write;

const PART: FieldDef = FieldDef {
    name: "part", kind: FieldKind::Text, required: true, default: "",
    help: "The part's reference (U1) or path (mcu.u).",
};

const UNIT: FieldDef = FieldDef {
    name: "unit", kind: FieldKind::Text, required: false, default: "",
    help: "The unit's name. Needed only when two units have a part of that name.",
};

pub const MODEL_READ: ActionDef = ActionDef {
    name: "elec.part.model",
    label: "Read part 3D model",
    group: "Board",
    help: "Read where a part's 3D model comes from, as its card says: KiCad's library on this computer or its download (with the library tag, host and date), the EasyEDA part service, or why the part shows its plain body. Also which 3D model sources are on.",
    returns: "part: text
body: text  kicad, easyeda, own (a model file of the design), plain, generated or script
model: text  the project path of the model it names; empty when it names none
here: bool  whether the body drawn is that model; false: the plain body stands in
from: text  where it came from, or that it is not here
detail: text  the download's host and date, the library folder, the part number, or why it is not here
get_from: text  this computer's choice for a KiCad model: auto, local or download; empty for other bodies
choices: [text]  the get_from choices whose sources are on now
body_source: text  the part's body source: kicad, easyeda or plain (no library model: a plain or generated body); empty for a model of the design's own or a script
body_sources: [{..}]  the sources elec.part.body.set takes for its part
  source: text
  offered: bool
  why: text  why not, when it is not offered
sources: {..}  the 3D model sources on now; only the person turns one on
  kicad_local: bool
  kicad_download: bool
  easyeda: bool",
    fields: &[PART, UNIT],
    inputs: &[], risk: Risk::Read, keys: "",
};

pub const MODEL_FROM: ActionDef = ActionDef {
    name: "elec.part.model.from",
    label: "Get part model from",
    group: "Board",
    help: "Choose where this computer gets a part's KiCad 3D model, as the switch in the part's card does: auto (KiCad's library on this computer, else its download), local or download. It is this computer's choice and changes nothing in the design. A source that is off in 3D Model Sources is refused.",
    returns: "part: text
get_from: text  the choice now
model: text  the model's project path",
    fields: &[
        PART,
        FieldDef {
            name: "from", kind: FieldKind::Choice(&["auto", "local", "download"]), required: true, default: "",
            help: "auto, local (this computer's KiCad library only) or download (KiCad's library download only).",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Run, keys: "",
};

pub const BODY_SET: ActionDef = ActionDef {
    name: "elec.part.body.set",
    label: "Set part 3D body source",
    group: "Board",
    help: "Switch a part's 3D body between KiCad's library model, the EasyEDA part service's model and its plain body, as the switch in its card does. It is a design edit of the part (every component of that part draws it), one undo step, written as the part's e.body. A source that is off in 3D Model Sources, or a model the part has none of, is refused with why; elec.part.model lists what is offered.",
    returns: "seq: int  the step it committed
label: text  its name in the undo history",
    fields: &[
        PART,
        FieldDef {
            name: "source", kind: FieldKind::Choice(&["kicad", "easyeda", "plain"]), required: true, default: "",
            help: "kicad (the part's own KiCad library model), easyeda (the EasyEDA model of its LCSC number) or plain (no library model: the part's own body when it has none, else a plain box).",
        },
        UNIT,
    ],
    inputs: &[], risk: Risk::Edit, keys: "",
};

pub static ACTIONS: &[ActionDef] = &[MODEL_READ, MODEL_FROM, BODY_SET];

fn error(field: &str, reason: impl std::fmt::Display) -> Diag {
    Diag::error(format!("{field}: {reason}"), vec![])
}

fn arg<'a>(args: &'a [(String, String)], name: &str) -> Option<&'a str> {
    args.iter().find(|(key, _)| key == name).map(|(_, text)| text.as_str())
}

/// The named part and its body, by reference or path, in the named unit or the only one that has
/// it.
fn part_body<'a>(design: &'a Design, args: &[(String, String)]) -> Result<(&'a Unit, &'a fab_elec_circuit::Component, Option<&'a fab_elec_parts::Body3d>), Diag> {
    let text = arg(args, "part").expect("required field checked");
    let units: Vec<_> = match arg(args, "unit") {
        Some(name) => vec![design.unit_by_name(name).ok_or_else(|| error("unit", format_args!("no unit named {name}")))?],
        None => design.units.iter().collect(),
    };
    let mut found = units.iter().filter_map(|unit| unit.circuit.comps.iter().find(|c| c.refdes == text || c.path == text).map(|c| (unit, c)));
    let Some((unit, comp)) = found.next() else {
        return Err(error("part", format_args!("no part {text}; elec.design.read lists them")));
    };
    if let Some((other, _)) = found.next() {
        return Err(error("part", format_args!("{text} is in {} and {}; pass unit", unit.name, other.name)));
    }
    Ok((unit, comp, comp.part.body.as_ref()))
}

/// `elec.part.model`: what the part's card says about its 3D model; `map` says where its part's
/// `e.body` is written.
pub fn model_read(design: &Design, map: Option<&SourceMap>, args: &[(String, String)]) -> Result<String, Diag> {
    MODEL_READ.check_args(args)?;
    let (unit, comp, body) = part_body(design, args)?;
    let on = model_board::sources();
    let mut out = format!("{{part: {} ", quote(&comp.refdes));
    let Some(body) = body else {
        write!(out, "body: \"plain\" model: \"\" here: false from: \"no body\" detail: {} get_from: \"\" choices: [] body_source: \"\" body_sources: [] ",
            quote("the part has no footprint body")).unwrap();
        return Ok(sources(out, on));
    };
    let model = model_board::of_body(body);
    let kind = match (model.family, &body.source, &model.path) {
        (Some(Family::Kicad), ..) => "kicad",
        (Some(Family::EasyEda), ..) => "easyeda",
        (None, fab_elec_parts::BodySource::Mesh(_), _) | (None, _, Some(_)) => "own",
        (None, fab_elec_parts::BodySource::Package(_), _) => "generated",
        (None, fab_elec_parts::BodySource::Script(_), _) => "script",
        (None, fab_elec_parts::BodySource::Extrude, _) => "plain",
    };
    let path = model.path.as_deref().unwrap_or("");
    let (get_from, choices) = match model.family {
        Some(Family::Kicad) => (model_board::choice(path).key(),
            Choice::ALL.iter().filter(|c| model_board::available(Family::Kicad, **c)).map(|c| quote(c.key())).collect::<Vec<_>>()),
        _ => ("", Vec::new()),
    };
    write!(out, "body: {} model: {} here: {} from: {} detail: {} get_from: {} choices: [{}] ",
        quote(kind), quote(path), model.here, quote(&model.line), quote(&model.detail), quote(get_from), choices.join(" ")).unwrap();
    let picks = body_choices(comp, map.and_then(|map| map.body_site(unit.id, &comp.part.name)));
    write!(out, "body_source: {} body_sources: [", quote(picks.current.map_or("", BodyPick::key))).unwrap();
    for (pick, offered) in BodyPick::ALL.iter().zip(&picks.picks) {
        write!(out, "{{source: {} offered: {} why: {}}} ", quote(pick.key()), offered.is_ok(), quote(offered.as_ref().err().copied().unwrap_or(""))).unwrap();
    }
    out.push_str("] ");
    Ok(sources(out, on))
}

fn sources(mut out: String, on: u8) -> String {
    write!(out, "sources: {{kicad_local: {} kicad_download: {} easyeda: {}}}}}",
        on & model_board::SOURCE_LOCAL != 0, on & model_board::SOURCE_DOWNLOAD != 0, on & model_board::SOURCE_EASYEDA != 0).unwrap();
    out
}

/// `elec.part.model.from`: this computer's choice for the part's KiCad model, as its card sets it.
/// The app keeps the choice and asks for the model again by it.
pub fn model_from(design: &Design, args: &[(String, String)]) -> Result<String, Diag> {
    MODEL_FROM.check_args(args)?;
    let (_, comp, body) = part_body(design, args)?;
    let choice = Choice::of_key(arg(args, "from").expect("required field checked")).expect("choice checked");
    let model = body.map(model_board::of_body);
    let path = match &model {
        Some(model) if model.family == Some(Family::Kicad) => model.path.clone().expect("a KiCad model has its path"),
        Some(model) => return Err(error("part", format_args!("{}'s body is no KiCad library model ({}: {})", comp.refdes, model.line, model.detail))),
        None => return Err(error("part", format_args!("{} has no footprint body", comp.refdes))),
    };
    model_board::choose(&path, choice).map_err(|refused| error("from", refused))?;
    Ok(format!("{{part: {} get_from: {} model: {}}}", quote(&comp.refdes), quote(choice.key()), quote(&path)))
}


/// `elec.part.body.set`: the commit the card's body switch makes for the same part and source.
pub fn body_set(design: &Design, docs: Option<&Docs>, tool: &str, args: &[(String, String)]) -> Result<Plan, Diag> {
    if tool != BODY_SET.name {
        return Err(error("tool", format_args!("unknown {tool}")));
    }
    BODY_SET.check_args(args)?;
    let (unit, comp, _) = part_body(design, args)?;
    let pick = BodyPick::of_key(arg(args, "source").expect("required field checked")).expect("choice checked");
    let site = docs.and_then(|docs| docs.map().body_site(unit.id, &comp.part.name));
    let name = fab_elec_lang::models_doc_name(&unit.name);
    let models_doc = docs.is_some_and(|docs| docs.sources().find(&name).and_then(|id| docs.sources().doc(id)).is_some_and(|doc| doc.present));
    let (label, ops) = body_plan(unit.id, &unit.name, comp, site, models_doc, pick).map_err(|why| error("source", why))?;
    Ok(Plan::Edit(Resolved { label, ops, place: vec![] }))
}

//! A part's 3D body source (EKMODELS): KiCad's library model, the EasyEDA part service's model, or
//! plain: no library model (the part's own body when that is none, a plain or generated one or a
//! model of the design's own; else a plain box). It is the design's choice, an `e.body` override of the part (every component of
//! that part draws it), written as one undo step: the same commit from the part card's switch and
//! from `elec.part.body.set`. Which choices a part has, what each writes, and in a few words why
//! one is not offered: no KiCad model, no LCSC number, EasyEDA off. Turning a source on is the
//! person's, in 3D Model Sources.
//!
//! Cost: a few model-board lookups and a scan of the part's fields per call.

use fab_elec_circuit::{CircuitOp, Component};
use fab_elec_core::BoardId;
use fab_elec_design::{DesignOp, FileOp};
use fab_elec_lang::map::BodySite;
use fab_elec_lang::models_doc_name;
use fab_elec_parts::model_board::{self, Family, SOURCE_EASYEDA};
use fab_elec_parts::{Body3d, BodySource};
use makepad_widgets_core::makepad_script::syntax::write::quote;

/// The body sources a part can be switched to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BodyPick {
    Kicad,
    EasyEda,
    Plain,
}

impl BodyPick {
    pub const ALL: [BodyPick; 3] = [BodyPick::Kicad, BodyPick::EasyEda, BodyPick::Plain];

    /// The word `elec.part.body.set` and `elec.part.model` use.
    pub fn key(self) -> &'static str {
        match self {
            BodyPick::Kicad => "kicad",
            BodyPick::EasyEda => "easyeda",
            BodyPick::Plain => "plain",
        }
    }

    pub fn of_key(key: &str) -> Option<BodyPick> {
        BodyPick::ALL.into_iter().find(|pick| pick.key() == key.trim())
    }

    /// The word the card's switch shows.
    pub fn word(self) -> &'static str {
        match self {
            BodyPick::Kicad => "KiCad",
            BodyPick::EasyEda => "EasyEDA",
            BodyPick::Plain => "plain",
        }
    }
}

/// What a pick writes: the override goes (the part's own body), or this override is written.
#[derive(Clone, PartialEq, Debug)]
pub enum Write {
    Own,
    Body(String),
}

/// A part's body choices: the one it draws now (None: a model of the design's own or an object
/// script), and per [`BodyPick::ALL`] what picking it writes or why it cannot.
#[derive(Clone, PartialEq, Debug)]
pub struct BodyChoices {
    pub current: Option<BodyPick>,
    pub picks: [Result<Write, &'static str>; 3],
}

/// The text a new models document starts with.
const MODELS_DOC: &str = "// The 3D bodies chosen for this design's parts: each e.body names the body a part draws
// (a part card's 3D body switch, or elec.part.body.set).
let e = mod.elec
";

/// The pick a body is: a library model's family, or plain for a plain or generated body.
fn pick_of(body: &Body3d) -> Option<BodyPick> {
    let model = model_board::of_body(body);
    match model.family {
        Some(Family::Kicad) => Some(BodyPick::Kicad),
        Some(Family::EasyEda) => Some(BodyPick::EasyEda),
        None if model.path.is_none() && matches!(body.source, BodySource::Extrude | BodySource::Package(_)) => Some(BodyPick::Plain),
        None => None,
    }
}

/// The LCSC number of an EasyEDA model's project path (`parts/lcsc/C2040.glb`).
fn lcsc_of(path: &str) -> Option<&str> {
    path.strip_prefix("parts/lcsc/")?.strip_suffix(".glb")
}

fn lcsc_number(text: &str) -> bool {
    text.strip_prefix('C').is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// The choices of `comp`'s part; `site` is where its `e.body` override is written, if it has one
/// (the part's own body is the site's, else the one it draws).
pub fn body_choices(comp: &Component, site: Option<&BodySite>) -> BodyChoices {
    let drawn = comp.part.body.as_ref();
    let own = match site {
        Some(site) => site.own.as_ref(),
        None => drawn,
    };
    let own_path = own.and_then(|body| model_board::of_body(body).path);
    let own_pick = own.and_then(pick_of);
    let number = [drawn.and_then(|body| model_board::of_body(body).path), own_path.clone()].iter().flatten()
        .find_map(|path| lcsc_of(path).map(str::to_string))
        .or_else(|| comp.fields.0.iter().chain(comp.part.fields.0.iter())
            .find(|(key, value)| key.eq_ignore_ascii_case("lcsc") && lcsc_number(value.trim()))
            .map(|(_, value)| value.trim().to_string()));
    let kicad = if own_pick == Some(BodyPick::Kicad) { Ok(Write::Own) } else { Err("no KiCad model") };
    let easyeda = match number {
        None => Err("no LCSC number"),
        Some(_) if model_board::sources() & SOURCE_EASYEDA == 0 => Err("EasyEDA off"),
        Some(number) => {
            let path = format!("parts/lcsc/{number}.glb");
            if own_path.as_deref() == Some(path.as_str()) { Ok(Write::Own) } else { Ok(Write::Body(format!("{{mesh: {}}}", quote(&path)))) }
        }
    };
    // plain is no library model: the part's own body when it is none, else a plain box
    let plain = match own_pick {
        Some(BodyPick::Kicad | BodyPick::EasyEda) => Ok(Write::Body("{}".into())),
        _ => Ok(Write::Own),
    };
    BodyChoices { current: drawn.and_then(pick_of), picks: [kicad, easyeda, plain] }
}

/// The commit that switches `comp`'s part to `pick`: its label and ops (the unit's models
/// document added first when a new override needs it and `models_doc` says it is not there), or
/// why not.
pub fn body_plan(unit: BoardId, unit_name: &str, comp: &Component, site: Option<&BodySite>, models_doc: bool, pick: BodyPick)
    -> Result<(String, Vec<DesignOp>), String>
{
    if comp.part.body.is_none() {
        return Err(format!("{} has no 3D body", comp.refdes));
    }
    let choices = body_choices(comp, site);
    if choices.current == Some(pick) {
        return Err(format!("{} draws its {} body already", comp.refdes, pick.word()));
    }
    let index = BodyPick::ALL.iter().position(|p| *p == pick).expect("every pick is listed");
    let write = choices.picks[index].clone().map_err(|why| format!("{}: {} is not offered: {why}", comp.refdes, pick.word()))?;
    let part = comp.part.name.clone();
    let body = match write {
        Write::Own if site.is_none() => return Err(format!("{} draws its own body already", comp.refdes)),
        Write::Own => None,
        Write::Body(text) => Some(text),
    };
    let mut ops = Vec::new();
    if body.is_some() && site.is_none() && !models_doc {
        ops.push(DesignOp::File(FileOp::AddDoc { name: models_doc_name(unit_name), text: MODELS_DOC.into() }));
    }
    let label = format!("{part} 3D body: {}", pick.word());
    ops.push(DesignOp::Circuit { unit, op: CircuitOp::SetBody { part, body } });
    Ok((label, ops))
}

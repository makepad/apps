//! The panel-part generators behind one entry per kind, for the model's generated parts.
//!
//! Everything is in the part's own frame: millimetres, z up, the origin at the anchor footprint's
//! origin on the board surface and +z out of the board on the anchor's side. The host resolves the
//! anchor, casts the panel ray, places the bodies and cuts, and publishes the board items; a
//! generate call is the kind's build, a few milliseconds, run off the UI thread.

use crate::csg::{Csg, Prim};
use crate::params::{unit_of, Kind, ParamTable, PartParams};
use fab_elec_board::Item;
use fab_materials::{Colour, FinishId, MaterialId};

/// A parameter value as the script gives it. Numbers are in the script's units (metres,
/// radians) and are converted by the kind's own table; text is enum names, material and finish
/// ids, colour names and paths.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamValue<'a> {
    Num(f64),
    Bool(bool),
    Text(&'a str),
    /// `0xRRGGBBAA`.
    Colour(u32),
}

/// The panel along the part's +z axis from its origin, in the part's frame (mm): the distance to
/// its first face, its thickness there and that face's normal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelAt {
    pub back: f64,
    pub thickness: f64,
    pub normal: [f64; 3],
}

pub struct ElecGenInput<'a> {
    /// The script's name for the part; its board items are `FeatureRef::Generated(part)`.
    pub part: &'a str,
    /// The kind's parameters by key, without the anchor. Missing keys keep their defaults; an
    /// unknown key or a value of the wrong kind is an error naming it.
    pub params: &'a [(&'a str, ParamValue<'a>)],
    /// When set, it overrides the kind's panel distance and thickness parameters.
    pub panel: Option<PanelAt>,
}

/// One body of the part.
pub struct GenBody {
    pub name: String,
    pub csg: Csg,
    pub material: MaterialId,
    pub finish: FinishId,
    pub colour: Colour,
}

pub struct GenOutput {
    /// The part's bodies, the main one first.
    pub bodies: Vec<GenBody>,
    /// What the panel loses (holes, openings, seats) and gains (bosses, snap seats).
    pub panel_cuts: Vec<Csg>,
    pub panel_adds: Vec<Csg>,
    /// Board features in the anchor footprint's frame, for `derived::place` at the anchor's
    /// placement and `derived::publish` under the part's name.
    pub board_items: Vec<Item>,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
}

pub type ElecGenerator = fn(&ElecGenInput) -> Result<GenOutput, String>;

/// Every kind with its entry.
pub fn generators() -> &'static [(&'static str, ElecGenerator)] {
    &[
        ("light_pipe", light_pipe),
        ("jack_ring", crate::jack_ring::generate::generate),
        ("knob", crate::knob::generate::generate),
        ("display_window", crate::display_window::generate::generate),
    ]
}

/// The entry of a kind.
pub fn generator(kind: &str) -> Option<ElecGenerator> {
    generators().iter().find(|(k, _)| *k == kind).map(|(_, g)| *g)
}

/// A script number in a descriptor's display unit.
fn from_script(v: f64, unit: &str) -> f64 {
    match unit {
        "mm" => v * 1e3,
        "um" => v * 1e6,
        "deg" => v.to_degrees(),
        _ => v,
    }
}

/// Set `params` on `p` through the kind's table, as an inspector edit would (its rules apply).
pub(crate) fn apply<P: PartParams>(kind: &str, table: &ParamTable<P>, p: &mut P, params: &[(&str, ParamValue)]) -> Result<(), String> {
    for &(key, value) in params {
        if key == "name" {
            let ParamValue::Text(name) = value else { return Err(format!("{kind}: name takes text, got {value:?}")) };
            p.set_name(name.to_owned());
            continue;
        }
        let Some(d) = table.find(key) else {
            let keys: Vec<_> = table.descs.iter().map(|d| d.key).collect();
            return Err(format!("{kind}: no parameter {key:?}; it has {}", keys.join(", ")));
        };
        let understood = match (value, &d.kind) {
            (ParamValue::Num(v), Kind::Float { fmt, .. }) => {
                table.set(p, d, from_script(v, unit_of(fmt)));
                true
            }
            (ParamValue::Num(v), Kind::Int { .. }) if v.fract() == 0.0 => {
                table.set(p, d, v);
                true
            }
            (ParamValue::Bool(b), Kind::Bool { .. }) => table.set_text(p, d, if b { "true" } else { "false" }),
            (ParamValue::Colour(c), Kind::Colour { .. }) => table.set_text(p, d, &format!("#{:06x}", c >> 8)),
            (ParamValue::Text(t), Kind::Enum { .. } | Kind::Colour { .. } | Kind::Material { .. } | Kind::Finish { .. } | Kind::Path { .. }) => table.set_text(p, d, t),
            _ => false,
        };
        if !understood {
            return Err(format!("{kind}: {key} cannot take {value:?}"));
        }
    }
    Ok(())
}

/// Set a float parameter in millimetres by key, through the table's rules.
pub(crate) fn set_mm<P: PartParams>(table: &ParamTable<P>, p: &mut P, key: &str, v: f64) {
    let d = table.find(key).unwrap_or_else(|| panic!("the table has {key}"));
    table.set(p, d, v);
}

/// A light pipe over its LED: the pipe, the panel hole it passes, and its board features.
fn light_pipe(input: &ElecGenInput) -> Result<GenOutput, String> {
    use crate::light_pipe::{build, derived::board_items, param_table, PipeParams};
    let table = param_table();
    let mut p = PipeParams::default();
    p.set_name(input.part.to_owned());
    apply("light_pipe", table, &mut p, input.params)?;
    if let Some(panel) = input.panel {
        set_mm(table, &mut p, "led_to_panel", panel.back);
        set_mm(table, &mut p, "panel_thickness", panel.thickness);
    }
    let b = build(&p);
    // The hole through the panel on the pipe's axis, a little past both faces.
    let over = 0.05;
    let mut hole = Csg { segments: b.csg.segments, ..Csg::default() };
    hole.add(Prim::cylinder("panel hole", [0.0, 0.0, p.led_to_panel - over], p.hole_diameter * 0.5, p.panel_thickness + 2.0 * over, [0.0, 0.0, 1.0]));
    let body = GenBody {
        name: p.name().to_owned(),
        material: table.material(&p),
        finish: table.finish(&p, "finish"),
        colour: p.colour(),
        csg: b.csg.clone(),
    };
    Ok(GenOutput {
        board_items: board_items(&p, &b),
        bodies: vec![body],
        panel_cuts: vec![hole],
        panel_adds: Vec::new(),
        warnings: b.warnings,
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg::Shape;
    use crate::light_pipe::{build, derived::board_items, param_table, PipeParams};

    #[test]
    fn light_pipe_entry_maps_script_units_and_the_panel() {
        let pipe = generator("light_pipe").expect("the light pipe has an entry");
        let params = [("hole_diameter", ParamValue::Num(0.0032)), ("colour", ParamValue::Colour(0x3366ccff))];
        let panel = PanelAt { back: 8.5, thickness: 2.0, normal: [0.0, 0.0, -1.0] };
        let out = pipe(&ElecGenInput { part: "pipe", params: &params[..1], panel: Some(panel) }).expect("the pipe generates");
        // The same edits through the table, in millimetres.
        let table = param_table();
        let mut p = PipeParams::default();
        p.set_name("pipe".into());
        for (key, v) in [("hole_diameter", 3.2), ("led_to_panel", 8.5), ("panel_thickness", 2.0)] {
            set_mm(table, &mut p, key, v);
        }
        let b = build(&p);
        assert_eq!(out.bodies.len(), 1, "a light pipe is one body");
        assert_eq!(out.bodies[0].name, "pipe", "the body takes the part's name");
        assert_eq!(out.bodies[0].csg, b.csg, "metres become millimetres and the panel sets distance and thickness");
        assert_eq!(out.board_items, board_items(&p, &b), "the board items are the pipe's own, in the LED's frame");
        let Shape::Cylinder { center, r, length, .. } = &out.panel_cuts[0].steps[0].prim.shape else { panic!("the panel cut is a cylinder") };
        assert!((r - 1.6).abs() < 1e-6 && (center[2] - 8.45).abs() < 1e-6 && (length - 2.1).abs() < 1e-6, "the hole spans the panel at the pipe's axis: r {r} from {} over {length}", center[2]);
        assert_eq!(out.warnings, b.warnings, "the build's warnings pass through");

        let err = |params: &[(&str, ParamValue)]| pipe(&ElecGenInput { part: "pipe", params, panel: None }).err().unwrap_or_default();
        assert!(err(&params[1..]).contains("no parameter \"colour\""), "an unknown key is named: {}", err(&params[1..]));
        assert!(err(&[("hole_diameter", ParamValue::Bool(true))]).contains("hole_diameter cannot take"), "a value of the wrong kind is named");
    }

    #[test]
    fn every_kind_generates_its_defaults_with_and_without_a_panel() {
        let panel = PanelAt { back: 8.0, thickness: 2.0, normal: [0.0, 0.0, -1.0] };
        for &(kind, entry) in generators() {
            for panel in [None, Some(panel)] {
                let out = entry(&ElecGenInput { part: kind, params: &[], panel }).unwrap_or_else(|e| panic!("{kind} with {panel:?}: {e}"));
                assert!(!out.bodies.is_empty() && out.bodies[0].name == kind, "{kind}: the main body first, named after the part");
                assert!(out.bodies.iter().all(|b| !b.csg.steps.is_empty()), "{kind}: no empty body");
                assert!(panel.is_some() || ["light_pipe", "jack_ring"].contains(&kind) || out.panel_cuts.is_empty(), "{kind}: no panel parameters, no panel, no cut");
            }
        }
    }
}

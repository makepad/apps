//! A knob on the panel front: its body, then its separate cap and pointer.

use super::{build, param_table, KnobParams};
use crate::generate::{apply, ElecGenInput, GenBody, GenOutput};
use crate::params::PartParams;

/// The knob's bottom rests on the panel front, `panel.back + panel.thickness` up the anchor's
/// axis, or on the board without a panel. It cuts nothing and needs nothing of the board: its
/// shaft is the pot's, and the pot is the board's part.
pub fn generate(input: &ElecGenInput) -> Result<GenOutput, String> {
    let table = param_table();
    let mut p = KnobParams::default();
    p.set_name(input.part.to_owned());
    apply("knob", table, &mut p, input.params)?;
    let mut b = build(&p);
    let front = input.panel.map_or(0.0, |panel| panel.back + panel.thickness);
    let (material, finish) = (table.material(&p), table.finish(&p, "finish"));
    b.csg.lift(front);
    let mut bodies = vec![GenBody { name: p.name().to_owned(), csg: b.csg, material, finish, colour: p.colour() }];
    for mut body in b.bodies {
        body.csg.lift(front);
        bodies.push(GenBody { name: body.name, csg: body.csg, material, finish, colour: body.colour });
    }
    Ok(GenOutput { bodies, panel_cuts: Vec::new(), panel_adds: Vec::new(), board_items: Vec::new(), warnings: b.warnings, notes: b.notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{set_mm, PanelAt, ParamValue};

    #[test]
    fn entry_rests_every_body_on_the_panel_front_in_its_own_colour() {
        let params = [
            ("diameter", ParamValue::Num(0.024)),
            ("segments", ParamValue::Num(64.0)),
            ("top_style", ParamValue::Text("Cap insert")),
            ("indicator", ParamValue::Text("Pointer line")),
        ];
        let panel = PanelAt { back: 8.0, thickness: 2.5, normal: [0.0, 0.0, -1.0] };
        let out = generate(&ElecGenInput { part: "control", params: &params, panel: Some(panel) }).expect("the knob generates");
        // The same edits through the table, in millimetres.
        let table = param_table();
        let mut p = KnobParams::default();
        p.set_name("control".into());
        set_mm(table, &mut p, "diameter", 24.0);
        set_mm(table, &mut p, "segments", 64.0);
        for (key, v) in [("top_style", "Cap insert"), ("indicator", "Pointer line")] {
            assert!(table.set_text(&mut p, table.find(key).expect("the table has the key"), v), "the table takes {key} {v}");
        }
        let mut b = build(&p);
        b.csg.lift(10.5);
        let names: Vec<_> = out.bodies.iter().map(|body| body.name.as_str()).collect();
        assert_eq!(names, ["control", "cap", "line"], "the main body first, then the cap and the pointer");
        assert_eq!(out.bodies[0].csg, b.csg, "metres become millimetres and the knob sits on the panel front, 8 + 2.5 mm up");
        assert_eq!(out.bodies[0].colour, p.colour(), "the main body takes the table's colour");
        for (body, own) in out.bodies[1..].iter().zip(&b.bodies) {
            let mut csg = own.csg.clone();
            csg.lift(10.5);
            assert_eq!(body.csg, csg, "{} sits on the panel front with the body", body.name);
            assert_eq!(body.colour, own.colour, "{} keeps its own colour", body.name);
        }
        assert_ne!(out.bodies[1].colour, out.bodies[0].colour, "the fixture's cap is not the body's colour");
        assert!(out.panel_cuts.is_empty() && out.panel_adds.is_empty() && out.board_items.is_empty(), "a knob cuts nothing and asks nothing of the board");
        assert_eq!((out.warnings, out.notes), (b.warnings, b.notes), "the build's warnings and notes pass through");

        let err = |params: &[(&str, ParamValue)]| generate(&ElecGenInput { part: "knob", params, panel: None }).err().unwrap_or_default();
        assert!(err(&[("unknown", ParamValue::Num(1.0))]).contains("no parameter \"unknown\""), "an unknown key is named");
        assert!(err(&[("diameter", ParamValue::Bool(true))]).contains("diameter cannot take"), "a value of the wrong kind is named");
    }
}

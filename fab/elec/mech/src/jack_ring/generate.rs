//! A jack ring on the panel front and the panel hole its jack passes.

use super::{build, param_table, RingParams};
use crate::csg::{Csg, Prim};
use crate::generate::{apply, set_mm, ElecGenInput, GenBody, GenOutput};
use crate::params::PartParams;

/// The ring's bottom rests on the panel front, `panel.back + panel.thickness` up the anchor's
/// axis; without a panel, the table's thickness above the board. The ring needs nothing of the
/// board: its jack's bushing passes the panel, and the jack is the board's part.
pub fn generate(input: &ElecGenInput) -> Result<GenOutput, String> {
    let table = param_table();
    let mut p = RingParams::default();
    p.set_name(input.part.to_owned());
    apply("jack_ring", table, &mut p, input.params)?;
    if let Some(panel) = input.panel {
        set_mm(table, &mut p, "panel_thickness", panel.thickness);
    }
    let mut b = build(&p);
    let front = input.panel.map_or(0.0, |panel| panel.back) + p.panel_thickness;
    // The bushing's hole, 0.5 mm over the thread, a little past both panel faces.
    let over = 0.05;
    let mut hole = Csg { segments: b.csg.segments, ..Csg::default() };
    hole.add(Prim::cylinder("panel hole", [0.0, 0.0, -p.panel_thickness - over], p.thread_d * 0.5 + 0.25, p.panel_thickness + 2.0 * over, [0.0, 0.0, 1.0]));
    hole.lift(front);
    b.csg.lift(front);
    Ok(GenOutput {
        bodies: vec![GenBody { name: p.name().to_owned(), material: table.material(&p), finish: table.finish(&p, "finish"), colour: p.colour(), csg: b.csg }],
        panel_cuts: vec![hole],
        panel_adds: Vec::new(),
        board_items: Vec::new(),
        warnings: b.warnings,
        notes: b.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg::Shape;
    use crate::generate::{PanelAt, ParamValue};

    #[test]
    fn entry_maps_units_and_rests_the_ring_on_the_panel_front() {
        let params = [
            ("diameter", ParamValue::Num(0.018)),
            ("panel_thickness", ParamValue::Num(0.001)),
            ("segments", ParamValue::Num(48.0)),
            ("thread_mode", ParamValue::Text("Cosmetic (plain minor bore)")),
        ];
        let panel = PanelAt { back: 9.0, thickness: 2.5, normal: [0.0, 0.0, -1.0] };
        let out = generate(&ElecGenInput { part: "jack nut", params: &params, panel: Some(panel) }).expect("the ring generates");
        // The same edits through the table, in millimetres; the panel's thickness wins.
        let table = param_table();
        let mut p = RingParams::default();
        p.set_name("jack nut".into());
        for (key, v) in [("diameter", 18.0), ("segments", 48.0), ("panel_thickness", 2.5)] {
            set_mm(table, &mut p, key, v);
        }
        assert!(table.set_text(&mut p, table.find("thread_mode").expect("the table has thread_mode"), "Cosmetic (plain minor bore)"));
        let mut b = build(&p);
        b.csg.lift(11.5);
        assert_eq!(out.bodies.len(), 1, "a jack ring is one body");
        let body = &out.bodies[0];
        assert_eq!(body.name, "jack nut", "the body takes the part's name");
        assert_eq!(body.csg, b.csg, "metres become millimetres and the ring sits on the panel front, 9 + 2.5 mm up");
        assert_eq!((body.material, body.finish, body.colour), (table.material(&p), table.finish(&p, "finish"), p.colour()), "the look comes from the table");
        let Shape::Cylinder { center, r, length, .. } = &out.panel_cuts[0].steps[0].prim.shape else { panic!("the panel cut is a cylinder") };
        assert!((r - (p.thread_d * 0.5 + 0.25)).abs() < 1e-6, "the hole clears the thread by 0.5 mm: r {r}");
        assert!((center[2] - 8.95).abs() < 1e-5 && (center[2] + length - 11.55).abs() < 1e-5, "the hole spans the panel: from {} over {length}", center[2]);
        assert!(out.panel_adds.is_empty() && out.board_items.is_empty(), "the ring adds no panel solid and asks nothing of the board");
        assert_eq!((out.warnings, out.notes), (b.warnings, b.notes), "the build's warnings and notes pass through");

        let err = |params: &[(&str, ParamValue)]| generate(&ElecGenInput { part: "ring", params, panel: None }).err().unwrap_or_default();
        assert!(err(&[("unknown", ParamValue::Num(1.0))]).contains("no parameter \"unknown\""), "an unknown key is named");
        assert!(err(&[("diameter", ParamValue::Bool(true))]).contains("diameter cannot take"), "a value of the wrong kind is named");
    }
}

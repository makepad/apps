//! A display window and its flush mounting cuts in the display footprint's frame.

use super::{build, layout, outline_poly, param_table, screen_data, HoleType, Outline, Viewport, WindowParams};
use crate::csg::{Csg, Prim};
use crate::generate::{apply, ElecGenInput, GenBody, GenOutput};
use crate::params::PartParams;
use fab_materials::{Colour, MaterialId};

/// The window's front is flush with the panel front, `panel.back + panel.thickness` up the
/// anchor's axis, in a pocket that takes the sheet and the layers under it; behind the pocket
/// the panel opens over the display. Without a panel the stack's bottom rests on the board and
/// nothing is cut. The display is the board's part and owns its keepout.
pub fn generate(input: &ElecGenInput) -> Result<GenOutput, String> {
    let table = param_table();
    let mut p = WindowParams::default();
    p.set_name(input.part.to_owned());
    apply("display_window", table, &mut p, input.params)?;
    let b = build(&p);
    let dz = input.panel.map_or(-b.report.adhesive_bottom, |panel| (panel.back + panel.thickness) - p.thickness);
    let mut panel_cuts = Vec::new();
    if let Some(panel) = input.panel {
        let l = layout(&p);
        // Through cuts reach a little past both panel faces.
        let over = 0.05;
        let (z, height) = (panel.back - dz - over, panel.thickness + 2.0 * over);
        let cut = |name: &str, poly, z, height| {
            let mut csg = Csg { segments: b.window.segments, ..Csg::default() };
            csg.add(Prim::extrude(name, z, height, vec![poly]));
            csg.lift(dz);
            csg
        };
        let d = screen_data(p.screen());
        let opening = if d.glass_w > 0.0 || p.viewport() != Viewport::None {
            // Over the display's glass, or the viewport where it is larger.
            let mut q = p.clone();
            let round = if d.glass_w > 0.0 { d.round } else { p.viewport() == Viewport::Round };
            q.outline = if round { Outline::Round } else { Outline::Rect }.index();
            q.diameter = d.glass_w.max(b.report.view_w);
            q.width = d.glass_w.max(b.report.view_w);
            q.height = d.glass_h.max(b.report.view_h);
            q.corner = if d.glass_w > 0.0 { 0.3 } else { p.view_corner };
            outline_poly(&q, 0.0).into_iter().map(|v| [v[0] + p.view_x, v[1] + p.view_y]).collect()
        } else {
            // A ring or a custom printed window: the margin inside the adhesive carries the seat.
            l.adhesive_inner.clone()
        };
        panel_cuts.push(cut("display opening", opening, z, height));
        // The pocket, 0.1 mm over the outline, from the adhesive's bottom through the front.
        panel_cuts.push(cut("window pocket", outline_poly(&p, -0.1), b.report.adhesive_bottom, p.thickness - b.report.adhesive_bottom + over));
        for (i, hole) in l.holes.iter().enumerate() {
            let name = if i == 0 && p.hole() != HoleType::None { "encoder hole" } else { "window mounting hole" };
            panel_cuts.push(cut(name, hole.poly.iter().map(|v| [v[0] + hole.cx, v[1] + hole.cy]).collect(), z, height));
        }
    }
    // The sheet takes the table's look; the layers under it the reference app's.
    let ink = MaterialId::lookup("silk_epoxy").expect("the epoxy ink is in the material table");
    let tape = MaterialId::lookup("unspecified_body").expect("the unspecified body is in the material table");
    let layers = [
        (p.name().to_owned(), b.window, table.material(&p), table.finish(&p, "finish"), p.colour()),
        ("print".into(), b.print, ink, ink.get().default_finish(), p.print_colour),
        ("ink".into(), b.ink, ink, ink.get().default_finish(), Colour::srgb([0.95, 0.95, 0.93]).with_translucency(0.55)),
        ("adhesive".into(), b.adhesive, tape, tape.get().default_finish(), Colour::srgb([0.93, 0.87, 0.70]).with_translucency(0.35)),
    ];
    let mut bodies = Vec::new();
    for (name, mut csg, material, finish, colour) in layers {
        if !csg.steps.is_empty() {
            csg.lift(dz);
            bodies.push(GenBody { name, csg, material, finish, colour });
        }
    }
    Ok(GenOutput { bodies, panel_cuts, panel_adds: Vec::new(), board_items: Vec::new(), warnings: b.warnings, notes: b.notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg::Shape;
    use crate::generate::{set_mm, PanelAt, ParamValue};

    #[test]
    fn entry_places_the_window_stack_and_all_panel_cuts() {
        let params = [("screen", ParamValue::Text("0.96\" OLED 128x64")),
            ("thickness", ParamValue::Num(0.0012)), ("mount_holes", ParamValue::Text("Four")),
            ("hole", ParamValue::Text("D flat")), ("seg_count", ParamValue::Num(4.0)),
            ("seg_ink", ParamValue::Text("Diffuser ink")), ("segments", ParamValue::Num(48.0))];
        let panel = PanelAt { back: 8.0, thickness: 3.0, normal: [0.0, 0.0, -1.0] };
        let out = generate(&ElecGenInput { part: "display cover", params: &params, panel: Some(panel) }).expect("window entry succeeds");
        let table = param_table();
        let mut p = WindowParams::default();
        p.set_name("display cover".into());
        for (key, value) in [("screen", "0.96\" OLED 128x64"), ("mount_holes", "Four"), ("hole", "D flat"), ("seg_ink", "Diffuser ink")] {
            assert!(table.set_text(&mut p, table.find(key).expect("descriptor exists"), value), "table accepts {key}");
        }
        set_mm(table, &mut p, "thickness", 1.2);
        table.set(&mut p, table.find("seg_count").expect("segment count descriptor exists"), 4.0);
        table.set(&mut p, table.find("segments").expect("segments descriptor exists"), 48.0);
        let b = build(&p);
        let dz = 11.0 - p.thickness;
        let Shape::Loft { rings: before, .. } = &b.window.steps[0].prim.shape else { panic!("window starts with its sheet loft") };
        let Shape::Loft { rings: after, .. } = &out.bodies[0].csg.steps[0].prim.shape else { panic!("translated window retains its sheet loft") };
        assert_eq!(after[0].pts, before[0].pts, "translation preserves the sheet outline");
        assert!((after[0].z - before[0].z - dz).abs() < 1e-6, "sheet back is panel front minus script thickness");
        assert_eq!(out.bodies.len(), 4, "window, print, diffuser ink and adhesive are emitted in order");
        for (body, expected) in out.bodies.iter().zip([&b.window, &b.print, &b.ink, &b.adhesive]) {
            let mut moved = expected.clone();
            moved.lift(dz);
            assert_eq!(body.csg, moved, "every {} primitive moves by the same offset", body.name);
        }
        assert_eq!((out.bodies[0].material, out.bodies[0].finish, out.bodies[0].colour), (table.material(&p), table.finish(&p, "finish"), p.colour()), "the sheet takes the table's look");
        assert_eq!(out.bodies[1].colour, p.print_colour, "the print takes its colour");
        assert!(out.bodies[2].colour.translucency > 0.0, "the diffuser ink is translucent");
        assert_eq!(out.bodies[0].name, "display cover", "main body takes the part name");
        assert_eq!(out.panel_cuts.len(), 7, "opening, pocket, encoder hole and four mounting holes are emitted");
        let Shape::Extrude { contours, .. } = &out.panel_cuts[0].steps[0].prim.shape else { panic!("display opening is an outline extrusion") };
        let ymin = contours[0].iter().map(|q| q[1]).fold(f64::INFINITY, f64::min);
        let ymax = contours[0].iter().map(|q| q[1]).fold(f64::NEG_INFINITY, f64::max);
        assert!(((ymin + ymax) * 0.5 - p.view_y).abs() < 1e-5, "display opening follows the active area's offset");
        assert!(ymax - ymin >= screen_data(p.screen()).glass_h - 1e-5, "display opening covers the glass height");
        for (i, cut) in out.panel_cuts.iter().enumerate() {
            let Shape::Extrude { z, height, .. } = &cut.steps[0].prim.shape else { panic!("panel features are extruded outlines") };
            if i == 1 {
                assert!((*z - (dz + b.report.adhesive_bottom)).abs() < 1e-6, "seat reaches the adhesive bottom including the modelled print");
            } else {
                assert!(*z <= panel.back && *z + *height >= (panel.back + panel.thickness), "{} spans both panel faces", cut.steps[0].prim.label);
            }
        }
        let l = layout(&p);
        for (cut, hole) in out.panel_cuts[2..].iter().zip(&l.holes) {
            let Shape::Extrude { contours, .. } = &cut.steps[0].prim.shape else { unreachable!() };
            assert_eq!(contours[0], hole.poly.iter().map(|q| [q[0] + hole.cx, q[1] + hole.cy]).collect::<Vec<_>>(), "panel holes match the fitted window holes including keying");
        }
        assert!(out.panel_adds.is_empty() && out.board_items.is_empty(), "window adds no bosses or display-owned board features");
        assert_eq!(out.warnings, b.warnings, "window warnings pass through");
        assert_eq!(out.notes, b.notes, "window notes pass through");
        let nominal = generate(&ElecGenInput { part: "window", params: &[("segments", ParamValue::Num(48.0))], panel: None }).expect("nominal display stack generates");
        let Shape::Loft { rings, .. } = &nominal.bodies[0].csg.steps[0].prim.shape else { panic!("nominal window retains its sheet loft") };
        let mut defaults = WindowParams::default();
        table.set(&mut defaults, table.find("segments").expect("segments descriptor exists"), 48.0);
        let Shape::Loft { rings: own, .. } = &build(&defaults).window.steps[0].prim.shape else { unreachable!() };
        assert!((rings[0].z - own[0].z + build(&defaults).report.adhesive_bottom).abs() < 1e-6, "without a panel the stack's bottom rests on the board");
        assert!(nominal.panel_cuts.is_empty(), "without a panel no mounting cuts are emitted");
        for (key, value) in [("unknown_window_key", ParamValue::Num(1.0)), ("thickness", ParamValue::Bool(true))] {
            let err = generate(&ElecGenInput { part: "window", params: &[(key, value)], panel: None }).err().expect("invalid parameter fails");
            assert!(err.contains(key), "invalid parameter error names {key}: {err}");
        }
    }
}

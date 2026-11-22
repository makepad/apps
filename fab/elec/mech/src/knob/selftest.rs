use super::*;
use crate::csg::Shape;
use makepad_csg_mesh::{
    mesh::TriMesh,
    validate::{pieces, validate_mesh},
    volume::mesh_volume,
};
use std::sync::Arc;

struct Case {
    name: &'static str,
    p: KnobParams,
    undercut: Option<bool>,
}

fn settings(p: &mut KnobParams, text: &str, name: &str) {
    param_table().from_text(&text.replace(';', "\n"), p, None)
        .unwrap_or_else(|e| panic!("{name}: parameters parse: {e}"));
}
fn preset(name: &'static str, text: &str) -> Case {
    let mut p = KnobParams { segments: 48, ..KnobParams::default() };
    settings(&mut p, text, name);
    p.name = name.into();
    let undercut = (p.grip_style() == GripStyle::Scallops).then_some(false);
    Case { name, p, undercut }
}
fn collet() -> KnobParams {
    preset("collet_dark", "height=16; lobes=0; taper=0.91; grip_style=Scallops; ridges=10;
        ridge_depth=0.8; grip_from=0.06; grip_to=0.84; scallop_width=3.2; scallop_phase=18;
        scallop_skip=true; edge_style=Chamfer; top_edge=0.6; bottom_edge=0.3; top_style=Cap insert;
        rim_width=2; rim_chamfer=0.4; cap_thickness=1.6; cap_recess=0.25; cap_edge=0.25;
        indicator=Pointer line; ind_start=0.12; line_width_start=0.35; line_width_end=1;
        line_depth=0.5; line_color=#F4F4F0; color=#141414").p
}
fn cases() -> Vec<Case> {
    let mut cases = vec![
        preset("large_scalloped_lobes", "diameter=32; height=18; lobes=7; lobe_style=Scallop;
            lobe_depth=2.5; lobe_shape=0.45; top_style=Dome; dome_height=1.5; indicator=Dot;
            ind_width=2; ribs=6; color=#F2CC26; roughness=0.55"),
        preset("solid_dome", "diameter=22; height=13; lobes=0; top_edge=2.5; top_style=Dome;
            dome_height=2.5; indicator=None; construction=Solid (cast); color=#E6E3DB; roughness=0.75"),
        preset("five_lobes", "color=#1A1A1C; roughness=0.55"),
        preset("fluted_encoder_skirt", "diameter=17; lobes=0; grip_style=Flutes; ridges=28;
            ridge_depth=0.5; grip_from=0; grip_to=0.85; top_style=Flat; skirt=true;
            skirt_diameter=24; indicator=None; color=#1A1A1C; roughness=0.55"),
        preset("knurled_encoder", "diameter=18; height=14; lobes=0; taper=0.97;
            grip_style=Diamond knurl; ridges=40; ridge_depth=0.3; grip_from=0.08;
            grip_to=0.75; indicator=None; color=#434549; roughness=0.55"),
        preset("cored_dome", "diameter=22; height=13; lobes=0; top_edge=2.5; top_style=Dome;
            dome_height=2.5; indicator=None; color=#E6E3DB; roughness=0.75"),
        preset("fluted_skirt_pointer", "diameter=19; height=16; lobes=0; grip_style=Flutes;
            ridges=24; ridge_depth=0.6; grip_to=0.9; top_style=Flat; skirt=true;
            skirt_diameter=28; indicator=Groove + notch; color=#C71F1F; roughness=0.55"),
        Case { name: "collet_dark", p: collet(), undercut: Some(false) },
        preset("engraved_scalloped_skirt", "diameter=30; height=18; lobes=0; taper=0.93;
            grip_style=Scallops; ridges=14; ridge_depth=1.2; grip_from=0.04; grip_to=0.8;
            scallop_width=4.2; scallop_phase=12.857; scallop_skip=true; top_edge=1.6;
            dish_diameter=0.75; dish_depth=0.7; skirt=true; skirt_diameter=36; skirt_height=2.5;
            indicator=Pointer line; ind_start=0.3; line_mode=Engraved; line_width_start=0.6;
            line_width_end=1.4; ribs=6; color=#3A3D40; line_color=#F2F2ED"),
        preset("solid_triangle", "diameter=24; height=14; lobes=3; lobe_style=Polygon;
            lobe_shape=0.15; taper=1; edge_style=Chamfer; top_edge=0.4; bottom_edge=0.2;
            top_style=Flat; indicator=None; construction=Solid (cast); color=#FFFFFF;
            roughness=0.05; material=flint; translucency=0.999"),
        preset("hex_trim", "diameter=12; height=10; lobes=6; lobe_style=Polygon; lobe_shape=0.25;
            top_edge=0.8; top_style=Flat; ind_width=0.8; bore_depth=8; ribs=0;
            color=#1A5CBF; roughness=0.55"),
        preset("twisted_star", "diameter=24; height=16; lobes=6; lobe_style=Star; lobe_depth=2;
            lobe_shape=0.3; twist=40; indicator=Raised line; color=#E6007E; roughness=0.55"),
    ];
    let mut red = collet();
    settings(&mut red, "diameter=18; height=14; scallop_width=2.8; scallop_phase=0; scallop_skip=false;
        rim_width=1.9; cap_recess=0.3; cap_crown=0.25; cap_color=#D02F2A; indicator=None", "collet_colour");
    cases.push(Case { name: "collet_colour", p: red, undercut: Some(false) });
    for (name, text, undercut) in [
        ("line_raised_dome", "top_style=Dome; dome_height=1.2; line_mode=Raised; line_height=0.3; edge_style=Fillet; top_edge=1.2", false),
        ("line_inlay_flat_skirt", "top_style=Flat; skirt=true; skirt_diameter=26; skirt_height=2; diameter=20", false),
        ("line_inlay_dish", "top_style=Dish; dish_diameter=0.7; dish_depth=0.6; edge_style=Fillet; top_edge=1.0", false),
        ("line_raised_flat", "line_mode=Raised; top_style=Flat", false),
        ("line_inlay_short", "line_bottom=6; line_draft=1", false),
        ("cap_dished_inlay", "cap_crown=-0.4; cap_recess=0", false),
        ("cap_domed_engraved", "cap_crown=0.4; line_mode=Engraved", false),
        ("scallop_square_twist", "scallop_bottom=Square; twist=25; indicator=None; ridges=12; scallop_width=2.4", true),
        ("scallop_lobed", "lobes=5; lobe_depth=1.0; indicator=None; ridges=15; scallop_width=2.0; ridge_depth=0.6", false),
        ("scallop_ball_top", "scallop_top=Ball (undercut)", true),
        ("scallop_ramp_top", "scallop_top=Ramp (undercut)", true),
        ("solid_collet", "construction=Solid (cast)", false),
    ] {
        let mut p = collet();
        settings(&mut p, text, name);
        p.name = name.into();
        cases.push(Case { name, p, undercut: Some(undercut) });
    }
    // Further mechanisms absent from the preset matrix share the same proofs.
    for (name, text, undercut) in [
        ("flute_open", "grip_style=Flutes; ridges=8; ridge_depth=0.8; flute_top=Open", Some(false)),
        ("flute_fade", "grip_style=Flutes; ridges=8; ridge_depth=0.8; flute_top=Fade (undercut)", Some(true)),
        ("rings_double_d", "grip_style=Rings; ridges=4; bore_type=Double D; shaft_flat=4", None),
        ("spline_crush", "bore_type=Knurled spline; spline_teeth=18; crush_ribs=3; bore_draft=0.5", None),
        ("split_shaft", "bore_type=Split knurled; spline_teeth=18", None),
        ("hex_shaft", "bore_type=Hex; shaft_flat=6.35", None),
        ("square_shaft", "bore_type=Square; shaft_flat=4", None),
        ("round_set_screw", "bore_type=Round; set_screw=true", None),
        ("crenel_faceted_skirt", "lobes=6; lobe_style=Crenel; skirt=true; skirt_diameter=28;
            skirt_facets=6; skirt_taper=2; skirt_edge_style=Chamfer; skirt_chamfer_angle=30", None),
    ] {
        let mut c = preset(name, "lobes=0; top_style=Flat; indicator=None");
        settings(&mut c.p, text, name);
        c.undercut = undercut;
        cases.push(c);
    }
    cases
}

// Keep all feature dimensions and the generator's algorithms, but use fewer
// angular samples and loft bands for the kernel proofs. Public builds retain
// the full adaptive scallop resolution and every ring.
fn coarse_build(p: &KnobParams) -> KnobBuild {
    let mut g = Gen::new(p);
    let pitch = if g.scallops() { p.ridges * 12 }
        else if matches!(p.grip_style(), GripStyle::Flutes | GripStyle::Knurl) { p.ridges * 4 }
        else { 48 };
    g.n = pitch.max(48) as usize;
    g.th = (0..g.n).map(|i| TAU * i as f64 / g.n as f64).collect();
    let mut b = build_gen(p, g);
    fn bands(c: &mut Csg) {
        c.segments = c.segments.min(24);
        for st in &mut c.steps {
            match &mut st.prim.shape {
                Shape::Group(child) => bands(Arc::make_mut(child)),
                Shape::Loft { rings, crease, .. } if matches!(st.prim.label.as_str(), "body" | "body inset" | "body outset" | "cavity") => {
                    let last = rings.len() - 1;
                    let keep: Vec<_> = (0..rings.len()).filter(|&i| i == 0 || i == last || crease[i] || i % 4 == 0).collect();
                    *rings = keep.iter().map(|&i| rings[i].clone()).collect();
                    *crease = keep.iter().map(|&i| crease[i]).collect();
                }
                _ => {}
            }
        }
    }
    bands(&mut b.csg);
    for body in &mut b.bodies { bands(&mut body.csg); }
    b
}

// Down-facing outer area in the pull band, excluding the underside and cavity.
fn undercut_area(p: &KnobParams, mesh: &TriMesh, tol_deg: f64) -> f64 {
    let r = p.diameter * 0.5;
    let h = p.height;
    let sin_tol = (tol_deg * std::f64::consts::PI / 180.0).sin();
    let z_lo = (if p.skirt { p.skirt_height.min(p.height * 0.6) + p.skirt_blend } else { p.bottom_edge }) + 0.05;
    let mut area = 0.0;
    for i in 0..mesh.triangles.len() {
        let (a, b, c) = mesh.triangle_vertices(i);
        let u = b - a;
        let v = c - a;
        let nx = u.y * v.z - u.z * v.y;
        let ny = u.z * v.x - u.x * v.z;
        let nz = u.x * v.y - u.y * v.x;
        let len = (nx * nx + ny * ny + nz * nz).sqrt();
        if len < 1e-12 { continue; }
        let cx = (a.x + b.x + c.x) / 3.0;
        let cy = (a.y + b.y + c.y) / 3.0;
        let cz = (a.z + b.z + c.z) / 3.0;
        if cz < z_lo || cz > h - 0.01 { continue; }
        let zf = cz / h;
        let r_out = r * (1.0 + (p.taper - 1.0) * zf) + p.bulge;
        if cx.hypot(cy) < r_out - p.ridge_depth - 0.35 { continue; }
        if nz / len < -sin_tol { area += 0.5 * len; }
    }
    area
}

fn checked(csg: &Csg, case: &str, body: &str) -> Arc<TriMesh> {
    let m = crate::lower::mesh(csg).unwrap_or_else(|e| panic!("{case}/{body}: mesh evaluates: {e}"));
    let r = validate_mesh(&m);
    assert!(!m.triangles.is_empty(), "{case}/{body}: body is nonempty");
    assert!(r.is_closed, "{case}/{body}: body is closed: {r:?}");
    assert!(r.is_manifold, "{case}/{body}: body is manifold: {r:?}");
    assert!(r.is_consistently_oriented, "{case}/{body}: consistent orientation: {r:?}");
    assert_eq!((r.near_duplicate_pairs, r.coincident_pairs), (0, 0),
        "{case}/{body}: no vertex pairs within the near-duplicate radius: {r:?}");
    assert_eq!(pieces(&m), 1, "{case}/{body}: body is one piece");
    assert!(mesh_volume(&m) > 0.0, "{case}/{body}: outward orientation has positive volume");
    m
}

/// The body pairs whose intersection keeps vertex pairs within the near-duplicate radius, with the
/// count each keeps (near-duplicate, coincident); every other pair keeps none, and a count may only
/// fall. Both are the raised line's zero-volume contact sheet on the knob's top: the finish refuses
/// to weld those copies (one refused weld per pair) and leaves them joined by edges 1e-16 to 1e-13
/// long.
const CLOSE_PAIRS: [(&str, &str, (usize, usize)); 2] = [
    ("line_raised_dome", "body/line", (7, 0)),
    ("line_raised_flat", "body/line", (6, 0)),
];

fn close_pairs_at_most(overlap: &TriMesh, case: &str, pair: &str) {
    let r = validate_mesh(overlap);
    let (near, coincident) = CLOSE_PAIRS.iter()
        .find(|row| row.0 == case && row.1 == pair)
        .map_or((0, 0), |row| row.2);
    assert!(r.near_duplicate_pairs <= near && r.coincident_pairs <= coincident,
        "{case}/{pair}: intersection keeps {} near-duplicate and {} coincident vertex pairs, at most ({near}, {coincident})",
        r.near_duplicate_pairs, r.coincident_pairs);
}

#[test]
fn presets_and_features_mesh_release_and_do_not_overlap() {
    let mut cored = 0.0;
    let mut solid = 0.0;
    for c in cases() {
        let built = coarse_build(&c.p);
        let main = checked(&built.csg, c.name, "body");
        if c.name == "collet_dark" { cored = mesh_volume(&main); }
        if c.name == "solid_collet" { solid = mesh_volume(&main); }
        let mut meshes = vec![("body", main.clone())];
        for body in &built.bodies {
            let m = checked(&body.csg, c.name, &body.name);
            meshes.push((&body.name, m));
            let (colour, roughness) = if body.name == "cap" { (c.p.cap_colour, 0.5) } else { (c.p.line_colour, 0.45) };
            assert_eq!(body.colour, colour, "{}/{}: body colour", c.name, body.name);
            assert_eq!(body.roughness, roughness, "{}/{}: body roughness", c.name, body.name);
        }
        for i in 0..meshes.len() {
            for j in i + 1..meshes.len() {
                let (an, a) = &meshes[i];
                let (bn, b) = &meshes[j];
                let overlap = makepad_csg_cad::intersection(a, b);
                close_pairs_at_most(&overlap, c.name, &format!("{an}/{bn}"));
                let volume = mesh_volume(&overlap).abs();
                let sum = mesh_volume(a) + mesh_volume(b);
                assert!(volume < 1e-4 * sum + 0.01,
                    "{}/{an}/{bn}: intersection volume {volume} below {} mm3", c.name, 1e-4 * sum + 0.01);
            }
        }
        if let Some(expect_undercut) = c.undercut {
            let area = undercut_area(&c.p, &main, 0.25);
            if expect_undercut {
                assert!(area > 0.5, "{}: closed/helical feature detected, undercut area {area} > 0.5 mm2", c.name);
            } else {
                assert!(area < 0.02, "{}: +z pull releases, undercut area {area} < 0.02 mm2", c.name);
            }
        }
        if c.p.grip_style() == GripStyle::Scallops {
            let count = match c.name {
                "collet_colour" => 610,
                "engraved_scalloped_skirt" => 686,
                "scallop_square_twist" => 792,
                "scallop_lobed" => 945,
                _ => 590,
            };
            let expected = format!("segments raised to {count} for the scallops");
            assert!(built.notes.contains(&expected), "{}: adaptive sampling note", c.name);
        }
        for (applies, expected) in [
            (c.p.grip_style() == GripStyle::Knurl, "a diamond knurl has down-facing facets (an undercut for a straight pull: side actions or a printed part)"),
            (c.p.grip_style() == GripStyle::Rings, "ring grooves run around the knob: their upper flanks face down (an undercut for a straight pull)"),
            (c.p.grip_style() == GripStyle::Flutes && c.p.flute_top == 1, "faded flute tops close the groove going up (an undercut): use Flute top = Open"),
            (c.p.grip_style() == GripStyle::Scallops && c.p.scallop_top() != ScallopTop::Open, "closed scallop tops overhang the groove: an undercut for the cavity pulled along +z (use Scallop top = Open)"),
            (c.p.grip_style() == GripStyle::Scallops && c.p.twist.abs() > 0.5, "twisted scallops are helical: one flank faces down (an unscrewing core or a printed part)"),
            (c.p.skirt && c.p.skirt_taper < 0.5, "skirt side has no draft: give it a taper of 1 deg or more"),
        ] {
            assert_eq!(built.warnings.iter().any(|s| s == expected), applies, "{}: warning {expected}", c.name);
        }
    }
    assert!(solid > cored * 1.3, "solid_collet: solid body exceeds cored volume by 30% ({solid} vs {cored})");
    let mut old = collet();
    settings(&mut old, "hollow=false", "legacy_solid");
    assert!(!old.hollow(), "legacy_solid: hollow=false selects Solid");
}

#[test]
fn limits_report_their_warnings_and_notes() {
    for (name, text, expected) in [
        ("small_skirt", "skirt=true; skirt_diameter=18", "skirt is smaller than the body"),
        ("thick_cap", "height=6; top_edge=0.4; top_style=Cap insert; cap_thickness=5", "cap too thick for the knob: thickness limited"),
        ("wide_rim", "top_style=Cap insert; rim_width=8", "rim width limited to 60% of the top radius"),
        ("wide_rim_chamfer", "top_style=Cap insert; rim_width=0.5; rim_chamfer=1", "rim chamfer limited to the rim width"),
        ("tiny_recess", "diameter=8; top_edge=1; top_style=Cap insert; rim_width=2", "cap recess very small: narrow the rim"),
        ("rim_indicator", "top_style=Cap insert", "indicator kept on the rim around the cap (Pointer line crosses the cap)"),
        ("dome_ridge", "top_style=Dome; indicator=Raised line", "raised line on a dome top sits at the dome's base height"),
        ("deep_line", "top_style=Cap insert; indicator=Pointer line; line_depth=2", "line depth limited to the cap thickness minus 0.3 mm"),
        ("thin_line_wall", "indicator=Pointer line; line_depth=1.5", "line groove deeper than the wall minus 0.4 mm"),
        ("cap_edge_line", "top_style=Cap insert; indicator=Pointer line; cap_edge=0.8", "cap edge fillet deeper than the line: the line thins at the cap edge"),
        ("raised_line_end", "indicator=Pointer line; line_mode=Raised; line_bottom=5", "raised line ends above the bottom: its bottom end faces down (an undercut); use line_bottom = 0"),
        ("outside_flat", "shaft_flat=6", "D flat is outside the shaft"),
        ("deep_bore", "bore_depth=25", "bore deeper than the knob; clamped"),
        ("thick_top", "height=10; top_thickness=6; dish_depth=4", "top too thick or knob too short to core out"),
        ("wide_boss", "boss_wall=6", "boss is as wide as the knob"),
        ("thin_skin", "bore_depth=14", "bore reaches into the top: thin skin above the shaft"),
        ("thin_wall", "wall=0.8", "walls under 1 mm are hard to fill in ABS"),
        ("thick_ribs", "rib_thickness=2", "ribs thicker than ~60% of the wall show sink marks"),
        ("screw_in_skin", "set_screw=true; screw_height=12", "set screw hole reaches the top skin"),
        ("screw_misses", "set_screw=true; screw_height=1", "set screw hole misses the bore"),
    ] {
        let c = preset(name, text);
        let b = build(&c.p);
        assert!(b.warnings.iter().any(|w| w == expected), "{name}: expected warning {expected}; got {:?}", b.warnings);
    }
    let c = preset("scallop_limits", "lobes=0; grip_style=Scallops; ridges=16; ridge_depth=3; scallop_width=4;
        top_style=Cap insert; rim_width=1; indicator=Pointer line; scallop_phase=11.25; twist=10");
    let b = coarse_build(&c.p);
    for note in [
        "scallop depth limited to half the width (a deeper cut would undercut its own edges)",
        "scallops wider than their pitch: they merge",
    ] {
        assert!(b.notes.iter().any(|s| s == note), "scallop_limits: expected note {note}");
    }
    for warning in [
        "open scallops leave under 0.5 mm of rim beside the cap recess: widen the rim",
        "the pointer line runs through a scallop: set Scallop phase or Skip under indicator",
        "twisted scallops cross the straight pointer line",
    ] {
        assert!(b.warnings.iter().any(|s| s == warning), "scallop_limits: expected warning {warning}");
    }
    let c = preset("narrow_line", "lobes=0; top_style=Flat; indicator=Pointer line; line_width_start=0.1;
        line_width_end=0.2; line_draft=3");
    let b = build(&c.p);
    assert!(b.warnings.iter().any(|s| s == "line draft limited to 0.25 deg (the line is too narrow for more)"),
        "narrow_line: reports the limited draft in degrees: {:?}", b.warnings);
    let c = preset("tight_cavity", "diameter=12; lobes=0; top_style=Flat; indicator=None");
    let b = build(&c.p);
    assert!(b.warnings.iter().any(|s| s == "little room between boss and wall: consider Construction: Solid (cast) or a smaller boss"),
        "tight_cavity: reports boss-to-wall clearance");
    assert_eq!(b.report.diameter, 12.0, "tight_cavity: report diameter");
    assert_eq!(b.report.height, 15.0, "tight_cavity: report height");
    assert_eq!(b.report.bore, 6.16, "tight_cavity: report bore includes diametral clearance");
}

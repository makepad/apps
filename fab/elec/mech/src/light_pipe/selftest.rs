use super::{build, param_table, presets, LegStyle, PipeParams};
use crate::lower;
use makepad_csg_mesh::{
    mesh::TriMesh,
    validate::{pieces, validate_mesh},
};
use std::collections::HashSet;

// These values satisfy the generator's warning conditions, including the material's
// snap-strain limit and the selected finish's side-draft requirement.
const EXPECTED_WARNINGS: &[(&str, &[&str])] = &[
    ("defaults", &[]),
    ("panel-back-press", &[]),
    ("panel-front-countersunk", &[]),
    ("panel-front-flange", &[]),
    ("panel-hole-3mm-0603", &[]),
    ("panel-hole-5mm-3535", &[]),
    ("pcb-heat-stake", &[]),
    ("pcb-hook-and-peg", &[]),
    ("pcb-locating-captive", &[]),
    ("pcb-press-pegs", &[]),
    ("rgb-1010-6mm", &[]),
    ("rgb-2020-10mm", &[]),
    ("rgb-3228-reverse-5mm", &[]),
    ("rgb-5050-12mm", &[]),
    ("split-pin-pmma", &[]),
    ("square-pipe-rgb-2020", &[]),
];

fn parse(name: &str, text: &str) -> PipeParams {
    let mut params = PipeParams::default();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A single unknown key must fail; in a whole file it could be silently skipped.
        param_table()
            .from_text(line, &mut params, None)
            .unwrap_or_else(|error| panic!("{name}: parameter line {line:?} parses: {error}"));
    }
    params
}

fn check_mesh(name: &str, mut params: PipeParams) {
    params.segments = 48;
    let built = build(&params);
    let expected = EXPECTED_WARNINGS
        .iter()
        .find_map(|&(preset, warnings)| (preset == name).then_some(warnings))
        .unwrap_or_else(|| panic!("{name}: expected warnings are listed"));
    let actual: Vec<_> = built.warnings.iter().map(String::as_str).collect();
    assert_eq!(actual, expected, "{name}: exactly the expected warnings");
    let mesh = lower::mesh(&built.csg)
        .unwrap_or_else(|error| panic!("{name}: solid builds and meshes: {error}"));
    let report = validate_mesh(&mesh);
    assert!(report.is_closed, "{name}: mesh is closed: {report:?}");
    assert!(report.is_manifold, "{name}: mesh is manifold: {report:?}");
    assert!(
        report.is_consistently_oriented,
        "{name}: mesh is consistently oriented: {report:?}"
    );
    assert_eq!(
        report.degenerate_triangles, 0,
        "{name}: mesh has no degenerate triangles: {report:?}"
    );
    assert_eq!(
        (report.near_duplicate_pairs, report.coincident_pairs), (0, 0),
        "{name}: no vertex pairs within the near-duplicate radius: {report:?}"
    );
    assert_eq!(pieces(&mesh), 1, "{name}: mesh is one piece");
    // Frosted triangle counts need the kernel tag operation and its lowering.
}

#[test]
fn presets_and_defaults_are_closed_single_pieces() {
    let defaults = PipeParams::default();
    let text = param_table().to_text(&defaults, None);
    let parsed = parse("defaults", &text);
    assert_eq!(
        param_table().to_text(&parsed, None), text,
        "defaults: parameter text round trips"
    );
    check_mesh("defaults", defaults);

    let mut names = HashSet::new();
    for entry in presets::presets() {
        assert!(names.insert(entry.name), "{}: preset name is unique", entry.name);
        let params = parse(entry.name, entry.text);
        assert_eq!(params.name, entry.name, "{}: text keeps its preset name", entry.name);
        assert_eq!(
            presets::preset(entry.name), Some(params.clone()),
            "{}: named lookup loads every field over defaults", entry.name
        );
        check_mesh(entry.name, params);
    }
    assert_eq!(names.len(), 15, "presets: all fifteen parameter sets are present");
    assert_eq!(
        names.len() + 1, EXPECTED_WARNINGS.len(),
        "presets: warning expectations cover every preset and the defaults"
    );
    assert!(presets::preset("missing-preset").is_none(), "missing-preset: lookup returns None");
}

fn slice_at(mesh: &TriMesh, z: f64) -> Vec<[f64; 2]> {
    let mut points = Vec::new();
    for triangle in &mesh.triangles {
        for edge in 0..3 {
            let a = mesh.vertices[triangle[edge] as usize];
            let b = mesh.vertices[triangle[(edge + 1) % 3] as usize];
            if (a.z - z) * (b.z - z) < 0.0 {
                let u = (z - a.z) / (b.z - a.z);
                points.push([a.x + u * (b.x - a.x), a.y + u * (b.y - a.y)]);
            }
        }
    }
    points
}

fn fits_at(points: &[[f64; 2]], cx: f64, radius: f64, dx: f64) -> bool {
    for q in points {
        if (q[0] + dx - cx).hypot(q[1]) > radius + 1e-3 {
            return false;
        }
    }
    true
}

#[test]
fn hole_fit_fuzz() {
    let mut seed = 12345_u32;
    let mut rnd = |lo: f64, hi: f64| {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        lo + (hi - lo) * ((seed >> 8) as f64 / 16777216.0)
    };
    let mut failures = Vec::new();
    for case in 0..40 {
        let mut p = PipeParams { segments: 48, ..PipeParams::default() };
        p.pcb_hole = rnd(0.8, 4.0);
        p.leg_width = rnd(0.4, 3.0);
        p.leg_thickness = rnd(0.4, 2.5);
        p.hook_overhang = rnd(0.1, 2.0);
        p.hole_clearance = rnd(0.0, 0.3);
        let styles = [LegStyle::HookOut, LegStyle::HookIn, LegStyle::SplitPin, LegStyle::HookAndPeg];
        p.leg_style = styles[rnd(0.0, 3.999) as usize].index();
        p.hole_spacing = rnd(6.0, 16.0);
        p.fit_to_led = rnd(0.0, 1.0) > 0.5;
        let pcbs = [0.8, 1.0, 1.6, 2.0];
        p.pcb_thickness = pcbs[rnd(0.0, 3.999) as usize];
        p.lead_in = rnd(20.0, 45.0);
        p.catch_angle = if rnd(0.0, 1.0) > 0.3 { 90.0 } else { 45.0 };
        p.slot_width = rnd(0.3, 1.5);

        let built = build(&p);
        let mesh = match lower::mesh(&built.csg) {
            Ok(mesh) => mesh,
            Err(error) => {
                failures.push(format!("fuzz {case}: meshes: {error}\nparameters: {p:#?}"));
                continue;
            }
        };
        let radius = p.pcb_hole * 0.5;
        let spacing = built.report.hole_spacing;
        let zc = built.report.catch_z;
        let rest0 = slice_at(&mesh, -0.02);
        let rest1 = slice_at(&mesh, -p.pcb_thickness + 0.02);
        let mut barb = Vec::new();
        let mut z = zc - 0.02;
        while z > built.report.leg_bottom + 0.01 {
            barb.extend(slice_at(&mesh, z));
            z -= 0.04;
        }
        let neck = slice_at(&mesh, zc + 0.02);
        let mut why = Vec::new();
        for sx in [-1.0_f64, 1.0] {
            // The mixed fixing's peg is exempt from the snap-leg checks.
            if p.leg_style() == LegStyle::HookAndPeg && sx < 0.0 {
                continue;
            }
            let cx = sx * spacing * 0.5;
            let side = |points: &[[f64; 2]], prong: i32| -> Vec<[f64; 2]> {
                points.iter().copied().filter(|q| {
                    q[0] * sx > 0.0
                        && (prong == 0 || (q[0] - cx) * f64::from(prong) > 0.0)
                }).collect()
            };
            let too_small = built.warnings.iter().any(|w| w.contains("too small for a snap leg"));
            if too_small {
                continue;
            }
            if !fits_at(&side(&rest0, 0), cx, radius, 0.0)
                || !fits_at(&side(&rest1, 0), cx, radius, 0.0)
            {
                why.push(format!("side {sx}: leg-at-rest"));
            }
            let prongs: &[i32] = if p.leg_style() == LegStyle::SplitPin { &[-1, 1] } else { &[0] };
            for &prong in prongs {
                let mut pass = side(&barb, prong);
                pass.extend(side(&neck, prong));
                let mut ok = false;
                let mut dx = -3.0_f64;
                while dx <= 3.0 && !ok {
                    ok = fits_at(&pass, cx, radius, dx);
                    dx += 0.002;
                }
                if !ok {
                    why.push(format!("side {sx}, prong {prong}: barb-pass"));
                }
            }
            let mut reach = 0.0_f64;
            for q in side(&barb, 0) {
                reach = reach.max((q[0] - cx).hypot(q[1]));
            }
            let warned = built.warnings.iter().any(|w| {
                w.contains("overlaps the PCB") || w.contains("cannot bend")
            });
            if reach < radius + 0.03 && !warned {
                why.push(format!("side {sx}: no-catch"));
            }
        }
        if !why.is_empty() {
            failures.push(format!(
                "fuzz {case}: {}\nparameters: {p:#?}\nfit notes: {:?}\nwarnings: {:?}",
                why.join(", "), built.report.fit_notes, built.warnings
            ));
        }
    }
    assert!(failures.is_empty(), "hole-fit fuzz failures:\n{}", failures.join("\n\n"));
}

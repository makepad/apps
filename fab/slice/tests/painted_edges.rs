//! A fill's edge on a curved face prints where the paint is, layer for layer: along each outer
//! wall the black run's ends sit on the analytic edge at the layer's own height, and from one
//! layer to the next they move only as far as the edge's slope there says, with no layer's
//! run sticking out past its neighbours'.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use makepad_fab_toolpath::role::Role;

const HEAD: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet white = Material{albedo: [0.95 0.95 0.95] name: \"White\"}\nlet black = Material{albedo: [0.02 0.02 0.02] name: \"Black\"}\n";

/// The shortest run of a colour on the surface (the outer wall's bead, mm): a fill narrower than
/// this on a layer prints none there.
const BEAD: f64 = 0.45;

/// The pupil's middle (model frame, y up, its face to +z) and the fill's radii.
const AT: [f64; 3] = [0.0, 15.0, 14.4];
const FILL: [f64; 3] = [4.0, 7.5, 3.3];

/// A white ball with a black fill of an ellipse through its front: a cylinder along z, whose
/// edge on the face is the ellipse x²/4² + (y-15)²/7.5² = 1 seen from the front.
fn ellipse() -> String {
    format!("{HEAD}let ball = m.sphere({{radius: 15mm at: [0 15mm 0]}})\nobject(ball)\nm.fill(ball, white)\nm.fill(m.ellipsoid({{radii: [4mm 7.5mm 300mm] at: [0 15mm 0]}}), black)\n")
}

/// A white ball with a pupil carved into its front (blended), and a black fill of an ellipsoid a
/// little larger than the pupil about the same middle: the edge is where the ball's face leaves
/// that ellipsoid.
fn pupil() -> String {
    format!("{HEAD}let ball = m.sphere({{radius: 15mm at: [0 15mm 0]}})\nlet pupil = m.ellipsoid({{radii: [3.7mm 7.2mm 3mm] at: [0 15mm 14.4mm]}})\nlet eye = m.carve(ball, pupil, 0.25mm)\nobject(eye)\nm.fill(eye, white)\nm.fill(m.ellipsoid({{radii: [4mm 7.5mm 3.3mm] at: [0 15mm 14.4mm]}}), black)\n")
}

fn build(source: &str) -> Evaluated {
    let m = FabModel::with_script("eye", "eye.cad.splash", source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let mut ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    // black prints in tool 2, the rest in tool 1
    let colors = ev.part_colors.clone();
    ev.plate.map_colors(&colors, &|name| Some(if name == "Black" { (2, 1) } else { (1, 0) }));
    ev
}

fn setup(layer: f64) -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", &layer.to_string());
    JobSetup {
        machine: Preset::default(),
        process,
        filaments: vec![Preset::default(), Preset::default()],
        filament_map: vec![1, 1],
        nozzle_volume_type: vec!["Standard".into()],
        bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None, None],
    }
}

/// One layer's black run along the front of its outer wall: the model x of its ends as printed
/// and as the analytic fill has them, at the layer's model height.
#[derive(Debug, Clone, Copy)]
struct Row {
    y: f64,
    printed: (f64, f64),
    sampled: (f64, f64),
    analytic: (f64, f64),
}

/// The black runs of the front of each layer's outer wall, `inside(model point)` the fill.
fn rows(source: &str, layer: f64, inside: &dyn Fn([f64; 3]) -> bool) -> Vec<Row> {
    let ev = build(source);
    let fr = &ev.frames[0];
    let field = ev.fields[0].as_ref().map(|pf| {
        let mut to = pf.to_printer;
        for (r, o) in fr.offset.iter().enumerate() {
            to[r][3] -= o;
        }
        PartField::new(pf.shape.clone(), to)
    });
    let frames = vec![Frame { local: fr.local.clone(), offset: fr.offset, field }];
    let res = slice_frames(&setup(layer), &ev.plate, &frames, &[], None).expect("slices");
    let paint = ev.paint[0].as_ref().expect("painted");
    let m = paint.from_printer;
    let model = |p: [f64; 3]| -> [f64; 3] { std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]) };
    let black = 1;
    let mut out = Vec::new();
    // the first layer prints in the part's own filament whatever its paint
    for l in res.layers().iter().skip(1) {
        let z = l.print_z - 0.5 * l.height;
        let (mut printed, mut sampled, mut analytic) = ((f64::INFINITY, f64::NEG_INFINITY), (f64::INFINITY, f64::NEG_INFINITY), (f64::INFINITY, f64::NEG_INFINITY));
        let sp = ev.plate.parts[0].paint.as_ref().expect("the slicer's paint");
        let mut y = 0.0;
        for ol in l.objects.iter().filter(|o| !o.support_only) {
            for p in ol.islands.iter().flat_map(|i| &i.walls).filter(|p| p.role == Role::OuterWall) {
                let mut pts: Vec<[f64; 3]> = p.pts.iter().map(|q| [q.x_mm(), q.y_mm(), z]).collect();
                if p.closed {
                    pts.push(pts[0]);
                }
                for w in pts.windows(2) {
                    let len = ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt();
                    let k = (len / 0.01).ceil().max(1.0) as usize;
                    for j in 0..k {
                        let t = (j as f64 + 0.5) / k as f64;
                        let at: [f64; 3] = std::array::from_fn(|d| w[0][d] + (w[1][d] - w[0][d]) * t);
                        let q = model(at);
                        // the front half, about the pupil
                        if q[2] < 5.0 {
                            continue;
                        }
                        y = q[1];
                        if ol.filament == black {
                            printed = (printed.0.min(q[0]), printed.1.max(q[0]));
                        }
                        if (sp.sample_at)(at).slot != 0 {
                            sampled = (sampled.0.min(q[0]), sampled.1.max(q[0]));
                        }
                        if inside(q) {
                            analytic = (analytic.0.min(q[0]), analytic.1.max(q[0]));
                        }
                    }
                }
            }
        }
        if printed.0.is_finite() || analytic.0.is_finite() {
            out.push(Row { y, printed, sampled, analytic });
        }
    }
    out
}

/// The worst any layer's run end sits off the analytic edge, and the worst any end moves from
/// one layer to the next past what the edge itself moves (mm, along x). Rows where only one of
/// the two has a run (the fill's very top and bottom) count by the other's whole width.
fn report(name: &str, rows: &[Row]) -> (f64, f64) {
    let mut off = 0.0f64;
    for r in rows {
        let e = if r.printed.0.is_finite() && r.analytic.0.is_finite() {
            (r.printed.0 - r.analytic.0).abs().max((r.printed.1 - r.analytic.1).abs())
        } else if r.printed.0.is_finite() {
            r.printed.1 - r.printed.0
        } else if r.analytic.1 - r.analytic.0 < BEAD {
            // narrower than a bead: no run of its own in the print
            0.0
        } else {
            r.analytic.1 - r.analytic.0
        };
        if e > 0.1 {
            println!("{name}: y {:.2}: printed {:.3}..{:.3}, paint {:.3}..{:.3}, analytic {:.3}..{:.3}", r.y, r.printed.0, r.printed.1, r.sampled.0, r.sampled.1, r.analytic.0, r.analytic.1);
        }
        off = off.max(e);
    }
    let mut jump = 0.0f64;
    for w in rows.windows(2) {
        let both = |r: &Row| r.printed.0.is_finite() && r.analytic.0.is_finite();
        if both(&w[0]) && both(&w[1]) {
            let d = |f: fn(&Row) -> (f64, f64)| {
                let (a, b) = (f(&w[0]), f(&w[1]));
                (b.0 - a.0, b.1 - a.1)
            };
            let (p, a) = (d(|r| r.printed), d(|r| r.analytic));
            jump = jump.max((p.0 - a.0).abs()).max((p.1 - a.1).abs());
        }
    }
    println!("{name}: {} layers through the fill; run ends at most {off:.3} mm off the edge, moving at most {jump:.3} mm more than it between layers", rows.len());
    (off, jump)
}

/// A black ellipse on a white ball, sliced: every layer's black run ends on the ellipse.
#[test]
fn an_ellipse_fill_prints_on_its_edge_layer_for_layer() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let inside = |q: [f64; 3]| (q[0] / 4.0).powi(2) + ((q[1] - 15.0) / 7.5).powi(2) < 1.0;
    let (off, jump) = report("ellipse", &rows(&ellipse(), 0.2, &inside));
    assert!(off < 0.05 && jump < 0.05, "the ellipse's edge prints {off:.3} mm off, jumping {jump:.3} mm between layers");
}

/// A carved pupil on a white ball filled black a little past its rim, sliced: every layer's
/// black run ends where the ball's face leaves the fill.
#[test]
fn a_carved_pupil_s_fill_prints_on_its_edge_layer_for_layer() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let inside = |q: [f64; 3]| (0..3).map(|k| ((q[k] - AT[k]) / FILL[k]).powi(2)).sum::<f64>() < 1.0;
    let (off, jump) = report("pupil", &rows(&pupil(), 0.2, &inside));
    assert!(off < 0.05 && jump < 0.05, "the pupil's edge prints {off:.3} mm off, jumping {jump:.3} mm between layers");
}

//! A part painted by position (a script's brush stroke, smooth-region flood or noise field on
//! the demo vase) changes colour in every layer where the paint's boundary crosses it: along
//! each outer wall, at each layer's own height, the printed filament is off the paint at that
//! point for at most a bead width, and the noise field changes filament inside the wall too, not
//! only on its surface. (How fast paint slices is a benchmark's question, not a test's:
//! `fab-slicebench`.)

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use makepad_fab_toolpath::role::Role;

const VASE: &str = include_str!("../../app/resources/vase.cad.splash");
const NOISE: &str = include_str!("../../app/resources/noise_paint.splash");
const MATERIALS: &str = "let base = Material{albedo: [0.05 0.2 0.85] name: \"Blue\"}\nlet red = Material{albedo: [0.8 0.05 0.05] name: \"Red\"}\nm.fill(vase, base)\n";

fn build(source: &str) -> Evaluated {
    let m = FabModel::with_script("vase", "vase.cad.splash", source);
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    let mut ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
    // The job maps blue (and the part's own colour) to tool 1, red and yellow to tool 2.
    let colors = ev.part_colors.clone();
    ev.plate.map_colors(&colors, &|name| Some(if matches!(name, "Red" | "Yellow") { (2, 1) } else { (1, 0) }));
    ev
}

/// Thick layers: the paint's boundary is crossed by every layer alike. `inside_exact`: the
/// paint prints as it is all through the part (`fab_paint_inside_exact`), as most of these tests
/// read it; else deeper than its skin a fill takes it in large cells.
fn setup(inside_exact: bool) -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.6");
    process.values.set_str("fab_paint_inside_exact", if inside_exact { "1" } else { "0" });
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

/// The surface point of the belly at model height `y` and angle `a` (model frame, y up).
fn belly(y: f64, a: f64) -> [f64; 3] {
    let v = (y - 23.8) / 25.2;
    let r = 21.0 * (1.0 - v * v).sqrt();
    [r * a.cos(), y, r * a.sin()]
}

fn mm3(p: [f64; 3]) -> String {
    format!("[{:.3}mm {:.3}mm {:.3}mm]", p[0], p[1], p[2])
}

fn cases() -> Vec<(&'static str, String)> {
    let stroke = format!("{VASE}\n{MATERIALS}m.stroke([{} {}], 3mm, red)\n", mm3(belly(18.0, 0.1)), mm3(belly(32.0, 0.75)));
    let flood = format!("{VASE}\n{MATERIALS}m.fill(Flood{{seed: {} angle: 12deg}}, red)\n", mm3(belly(23.8, 0.0)));
    vec![("stroke", stroke), ("flood", flood), ("noise", format!("{VASE}\n{NOISE}"))]
}

struct Sliced {
    ev: Evaluated,
    res: makepad_fab_slice::SliceResult,
}

fn slice(source: &str, inside_exact: bool) -> Sliced {
    let ev = build(source);
    let fr = &ev.frames[0];
    // a field part slices from its field, any other from its mesh
    let field = ev.fields[0].as_ref().map(|pf| {
        let mut to = pf.to_printer;
        for (r, o) in fr.offset.iter().enumerate() {
            to[r][3] -= o;
        }
        PartField::new(pf.shape.clone(), to)
    });
    let frames = vec![Frame { local: fr.local.clone(), offset: fr.offset, field }];
    let res = slice_frames(&setup(inside_exact), &ev.plate, &frames, &[], None).expect("slices");
    Sliced { ev, res }
}

/// The shortest run of a colour on the surface: the rule the slicer holds its colours to.
const BEAD: f64 = 0.4;

/// How the printed colours sit against the paint, each layer at its own height.
#[derive(Debug, Default)]
struct Fit {
    /// Every stretch of outer wall (mm along it) printed in another filament than the paint
    /// there: how far each colour boundary sits off the painted one, or the length of a run too
    /// short to print.
    off: Vec<f64>,
    wall: f64,
    /// Outer wall length per filament.
    wall_by: [f64; 2],
    /// Extrusion length inside the part (every path but outer walls) printed in another
    /// filament than the paint at that point, beyond `tol` along the path.
    inside_off: f64,
    inside: f64,
    /// Extrusion length inside the part per filament.
    inside_by: [f64; 2],
}

impl Fit {
    fn worst(&self) -> f64 {
        self.off.iter().copied().fold(0.0, f64::max)
    }

    fn typical(&self) -> f64 {
        let mut off = self.off.clone();
        off.sort_by(f64::total_cmp);
        off.get(off.len() / 2).copied().unwrap_or(0.0)
    }
}

/// Calls `visit` at points about `step` apart along a path, with its direction and the length
/// the point stands for.
fn walk(pts: &[(f64, f64)], step: f64, visit: &mut dyn FnMut(f64, f64, (f64, f64), f64)) {
    for w in pts.windows(2) {
        let ((ax, ay), (bx, by)) = (w[0], w[1]);
        let len = (bx - ax).hypot(by - ay);
        if len < 1e-9 {
            continue;
        }
        let k = (len / step).ceil() as usize;
        for j in 0..k {
            let t = (j as f64 + 0.5) / k as f64;
            visit(ax + (bx - ax) * t, ay + (by - ay) * t, ((bx - ax) / len, (by - ay) / len), len / k as f64);
        }
    }
}

fn fit(s: &Sliced, tol: f64) -> Fit {
    let mut f = Fit::default();
    let part = &s.ev.plate.parts[0];
    let Some(sp) = part.paint.as_ref() else { return f };
    let own = part.filament as usize - 1;
    let states = sp.states(part.filament);
    let fil_at = |x: f64, y: f64, z: f64| -> usize {
        let st = states.get((sp.sample_at)([x, y, z]).slot as usize).copied().unwrap_or(0);
        if st == 0 { own } else { st as usize - 1 }
    };
    // whether the paint within `tol` along a direction either way holds `fil`
    let near = |x: f64, y: f64, z: f64, d: (f64, f64), fil: usize| (0..=4).any(|j| {
        let t = tol * j as f64 / 4.0;
        fil_at(x + d.0 * t, y + d.1 * t, z) == fil || fil_at(x - d.0 * t, y - d.1 * t, z) == fil
    });
    // The first layer prints in the part's own filament whatever its paint.
    for layer in s.res.layers().iter().skip(1) {
        let z = layer.print_z - 0.5 * layer.height;
        let paths = || layer.objects.iter().filter(|o| !o.support_only).flat_map(|ol| ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).map(|p| (ol.filament, p)));
        for (fil, p) in paths() {
            let mut pts: Vec<(f64, f64)> = p.pts.iter().map(|q| (q.x_mm(), q.y_mm())).collect();
            if p.role == Role::OuterWall {
                if p.closed {
                    pts.push(pts[0]);
                }
                let mut stretch = 0.0;
                walk(&pts, 0.02, &mut |x, y, _, len| {
                    f.wall += len;
                    f.wall_by[fil.min(1)] += len;
                    let paint = fil_at(x, y, z);
                    if paint == fil {
                        if stretch > 0.0 {
                            f.off.push(std::mem::take(&mut stretch));
                        }
                    } else {
                        stretch += len;
                    }
                });
                if stretch > 0.0 {
                    f.off.push(stretch);
                }
            } else {
                walk(&pts, 0.4, &mut |x, y, d, len| {
                    f.inside += len;
                    f.inside_by[fil.min(1)] += len;
                    if fil_at(x, y, z) != fil && !near(x, y, z, d, fil) {
                        f.inside_off += len;
                    }
                });
            }
        }
    }
    f
}

/// Tool changes per layer from the G-code (`T<n>` lines after each layer change).
fn tool_changes(g: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for l in g.lines() {
        if l.starts_with("; CHANGE_LAYER") {
            out.push(0);
        } else if let Some(last) = out.last_mut() {
            let t = l.trim();
            if t.len() >= 2 && t.starts_with('T') && t[1..].chars().all(|c| c.is_ascii_digit()) {
                *last += 1;
            }
        }
    }
    out
}

#[test]
fn painted_vases_change_colour_where_the_paint_is() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    for (name, src) in cases() {
        let s = slice(&src, true);
        let fit = fit(&s, 0.5);
        let tc = tool_changes(&s.res.gcode);
        let changing = tc.iter().filter(|&&n| n > 0).count();
        println!("{name}: boundary error worst {:.3} typical {:.3} mm, {} stretches over {:.0} mm of outer wall ({:.0} mm painted)", fit.worst(), fit.typical(), fit.off.len(), fit.wall, fit.wall_by[1]);
        // Both colours print on the outer walls, each boundary within a bead of the paint's at
        // the layer's own height.
        assert!(fit.wall_by[0] > 0.0 && fit.wall_by[1] > 0.0, "{name}: no colour boundary on the outer walls: {:?} mm by filament", fit.wall_by);
        assert!(fit.worst() <= BEAD + 0.02, "{name}: a colour sits {:.3} mm off the paint", fit.worst());
        assert!(fit.inside_off < 0.01 * fit.inside, "{name}: {:.1} of {:.0} mm inside off the paint", fit.inside_off, fit.inside);
        // A filament change at most once per layer and filament.
        assert!(tc.iter().all(|&n| n <= 1), "{name}: tool changes per layer {tc:?}");
        if name == "noise" {
            // The noise changes filament through the wall, layer after layer: both filaments
            // print inside the part (inner walls), and nearly every layer changes.
            assert!(fit.inside_by[0] > 0.2 * fit.inside && fit.inside_by[1] > 0.2 * fit.inside, "noise inside by filament {:?} of {:.0} mm", fit.inside_by, fit.inside);
            assert!(changing * 10 >= 9 * (tc.len() - 1), "noise: {changing} of {} layers change filament", tc.len());
        }
    }
}

/// A 20 mm block whose material is a field of its script (`m.fill(cube, Mix{..})`).
fn block(fill: &str) -> String {
    format!(
        "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\
         let cube = m.box({{size: [20mm, 20mm, 20mm] at: [0, 10mm, 0]}})\n\
         let blue = Material{{albedo: [0.05 0.2 0.85] name: \"Blue\"}}\n\
         let yellow = Material{{albedo: [0.9 0.75 0.05] name: \"Yellow\"}}\n\
         m.fill(cube, blue)\nm.fill(cube, {fill})\nobject(cube)\n"
    )
}

const VORONOI: &str = "Mix{materials: [blue yellow] field: fn(p) { noise.cell_id(p / 6mm, 3) } sharp: 1}";
const GRADIENT: &str = "Mix{materials: [blue yellow] field: fn(p) { p.y / 20mm }}";

/// A Voronoi of two filaments through the block (each cell blue or yellow by its id) prints
/// each cell in its filament: on the outer walls each colour boundary within a bead of the
/// field's, inside the part (inner walls, infill, top and bottom) the filament the field gives
/// there, both filaments in good share, and one head swap a layer at most.
#[test]
fn a_voronoi_block_prints_a_filament_per_cell() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let s = slice(&block(VORONOI), true);
    let fit = fit(&s, 0.5);
    let tc = tool_changes(&s.res.gcode);
    // the outer walls' pieces between colour boundaries
    let pieces = s.res.layers().iter().flat_map(|l| &l.objects).flat_map(|ol| ol.islands.iter().flat_map(|i| &i.walls)).filter(|p| p.role == Role::OuterWall && !p.closed).count();
    println!("voronoi: {pieces} outer wall pieces, boundary error worst {:.3} typical {:.3} mm over {:.0} mm of outer wall; inside off {:.1} of {:.0} mm, by filament {:?}; swaps a layer {tc:?}, {:.0} s", fit.worst(), fit.typical(), fit.wall, fit.inside_off, fit.inside, fit.inside_by, s.res.stats.total_time);
    assert!(pieces > 100, "voronoi: only {pieces} pieces of outer wall between colours");
    assert!(fit.worst() <= BEAD + 0.02, "voronoi: a colour sits {:.3} mm off the field", fit.worst());
    assert!(fit.inside_off < 0.01 * fit.inside, "voronoi: {:.1} of {:.0} mm inside off the field", fit.inside_off, fit.inside);
    assert!(fit.inside_by[0] > 0.2 * fit.inside && fit.inside_by[1] > 0.2 * fit.inside, "voronoi inside by filament {:?} of {:.0} mm", fit.inside_by, fit.inside);
    assert!(tc.iter().all(|&n| n <= 1), "voronoi: tool changes per layer {tc:?}");
}

/// A blend from blue at the foot to yellow at the top, dithered: every 1.2 mm slab prints
/// yellow in the blend's share there, on the outer walls and inside, and one head swap a layer
/// at most. Where the walls' blend is too thin to be worth a swap each layer (a twentieth of a
/// colour, here the foot's slab) its specks gather on fewer layers, a slab's share coarser.
#[test]
fn a_dithered_gradient_prints_its_share_per_slab() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let s = slice(&block(GRADIENT), true);
    let part = &s.ev.plate.parts[0];
    let sp = part.paint.as_ref().expect("the block's paint");
    let own = part.filament as usize - 1;
    let states = sp.states(part.filament);
    let fil = |slot: u8| match states.get(slot as usize).copied().unwrap_or(0) { 0 => own, st => st as usize - 1 };
    // the blend's share of yellow (filament 1) at a point
    let share = |x: f64, y: f64, z: f64| {
        let h = (sp.sample_at)([x, y, z]);
        let here = (fil(h.slot) == 1) as u8 as f64;
        match h.mix {
            None => here,
            Some((other, t)) => here * (1.0 - t as f64) + (fil(other) == 1) as u8 as f64 * t as f64,
        }
    };
    // per layer, outer wall then inside: [yellow printed, yellow in the blend, length]
    let mut per_layer = Vec::new();
    for layer in s.res.layers().iter().skip(1) {
        let z = layer.print_z - 0.5 * layer.height;
        let mut acc = [[0.0f64; 3]; 2];
        for ol in layer.objects.iter().filter(|o| !o.support_only) {
            for p in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
                let mut pts: Vec<(f64, f64)> = p.pts.iter().map(|q| (q.x_mm(), q.y_mm())).collect();
                if p.closed {
                    pts.push(pts[0]);
                }
                let a = &mut acc[(p.role != Role::OuterWall) as usize];
                walk(&pts, 0.1, &mut |x, y, _, len| {
                    a[0] += len * (ol.filament == 1) as u8 as f64;
                    a[1] += len * share(x, y, z);
                    a[2] += len;
                });
            }
        }
        per_layer.push((z, acc));
    }
    let tc = tool_changes(&s.res.gcode);
    // the worst slab off its blend: on the outer wall, inside, and on a wall too thin a blend
    let mut worst = [0.0f64; 3];
    for slab in per_layer.chunks(2) {
        let mut sum = [[0.0f64; 3]; 2];
        for (_, acc) in slab {
            for k in 0..2 {
                for j in 0..3 {
                    sum[k][j] += acc[k][j];
                }
            }
        }
        let off: Vec<f64> = sum.iter().map(|a| if a[2] > 0.0 { (a[0] - a[1]) / a[2] } else { 0.0 }).collect();
        println!("slab at {:.1} mm: wall yellow {:.3} of blend {:.3}, inside {:.3} of {:.3}", slab[0].0, sum[0][0] / sum[0][2].max(1e-9), sum[0][1] / sum[0][2].max(1e-9), sum[1][0] / sum[1][2].max(1e-9), sum[1][1] / sum[1][2].max(1e-9));
        let field = sum[0][1] / sum[0][2].max(1e-9);
        let thin = field.min(1.0 - field) < 0.06;
        for (k, at) in [(0, if thin { 2 } else { 0 }), (1, 1)] {
            worst[at] = worst[at].max(off[k].abs());
        }
        // the blend is the script's: yellow's share rises with the height, 0 at the foot
        let z = slab.iter().map(|(z, _)| z).sum::<f64>() / slab.len() as f64;
        assert!((field - z / 20.0).abs() < 0.01, "the blend at {z:.1} mm is {field:.3}, not the field's {:.3}", z / 20.0);
    }
    let plain = slice(&block("blue"), true);
    println!("gradient: worst slab off wall {:.3} (thin {:.3}) inside {:.3}; swaps a layer {tc:?}; print {:.0} s, in blue alone {:.0} s", worst[0], worst[2], worst[1], s.res.stats.total_time, plain.res.stats.total_time);
    assert!(worst[0] < 0.01, "a slab's outer wall prints {:.3} off the blend", worst[0]);
    assert!(worst[2] < 0.025, "a thin blend's slab of outer wall prints {:.3} off the blend", worst[2]);
    assert!(worst[1] < 0.04, "a slab's inside prints {:.3} off the blend", worst[1]);
    assert!(tc.iter().all(|&n| n <= 1), "gradient: tool changes per layer {tc:?}");
}

/// A blend too thin to be worth a head swap on every layer (a fifth of a percent of yellow)
/// prints its yellow on few layers, each with a swap's worth, and keeps its share over the
/// block: a swap to yellow and back for each of those layers, a third of a swap a layer in
/// all, where yellow specks on every layer would swap on each.
#[test]
fn a_thin_blend_swaps_heads_on_few_layers() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let s = slice(&block("Mix{materials: [blue yellow] field: fn(p) { 0.002 }}"), true);
    let tc = tool_changes(&s.res.gcode);
    let swaps: usize = tc.iter().sum();
    let (mut yellow, mut all) = (0.0, 0.0);
    for layer in s.res.layers().iter().skip(1) {
        for ol in layer.objects.iter().filter(|o| !o.support_only) {
            for p in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
                let len = p.length_mm();
                all += len;
                if ol.filament == 1 {
                    yellow += len;
                }
            }
        }
    }
    let share = yellow / all;
    println!("thin blend: {swaps} head swaps over {} layers {tc:?}, yellow {share:.4} of the extrusion; {:.0} s", tc.len(), s.res.stats.total_time);
    assert!(swaps * 2 < tc.len(), "{swaps} swaps over {} layers: {tc:?}", tc.len());
    assert!(share > 0.0012 && share < 0.0028, "the blend prints {share:.4} yellow");
}

/// Deeper than the paint's skin the block's fills take its paint in large cells of one material:
/// the walls (outer and inner) and the surfaces that show print as they do with an exact inside,
/// piece for piece in each filament, and the hidden fills of a layer deeper than the skin in
/// tens of pieces, in both filaments: a dithered blend's hundreds cut fourfold, a Voronoi's
/// cells (already in tens) as many.
#[test]
fn a_painted_inside_prints_in_large_cells() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    // per layer, sorted: each wall and showing fill (filament, role, points); per layer the
    // hidden fills' pieces, and their length per filament
    type Skin = Vec<Vec<(usize, String, Vec<(i64, i64)>)>>;
    let read = |s: &Sliced| -> (Skin, Vec<usize>, [f64; 2]) {
        let (mut skin, mut pieces, mut by) = (Vec::new(), Vec::new(), [0.0f64; 2]);
        for layer in s.res.layers() {
            let (mut here, mut hidden) = (Vec::new(), 0);
            for ol in layer.objects.iter().filter(|o| !o.support_only) {
                for island in &ol.islands {
                    for p in island.walls.iter().chain(island.fills.iter().filter(|f| f.role.shows())) {
                        here.push((ol.filament, format!("{:?}", p.role), p.pts.iter().map(|q| (q.x, q.y)).collect()));
                    }
                    for p in island.fills.iter().filter(|f| !f.role.shows()) {
                        hidden += 1;
                        by[ol.filament.min(1)] += p.length_mm();
                    }
                }
            }
            here.sort();
            skin.push(here);
            pieces.push(hidden);
        }
        (skin, pieces, by)
    };
    // the layer in the middle of the block, deep in its inside
    let mid = |pieces: &[usize]| pieces[pieces.len() / 2];
    for (name, fill, dithered) in [("gradient", GRADIENT, true), ("voronoi", VORONOI, false)] {
        let (exact, coarse) = (slice(&block(fill), true), slice(&block(fill), false));
        let ((skin_exact, hidden_exact, _), (skin_coarse, hidden_coarse, by)) = (read(&exact), read(&coarse));
        println!(
            "{name}: hidden fill pieces a layer exact {hidden_exact:?}, in cells {hidden_coarse:?} (by filament {by:.0?} mm); print {:.0} s exact, {:.0} s in cells",
            exact.res.stats.total_time,
            coarse.res.stats.total_time
        );
        assert_eq!(skin_exact.len(), skin_coarse.len(), "{name}: the layers");
        for (k, (a, b)) in skin_exact.iter().zip(&skin_coarse).enumerate() {
            assert!(a == b, "{name}: layer {k}'s walls and surfaces print differently in cells");
        }
        let (a, b) = (mid(&hidden_exact), mid(&hidden_coarse));
        assert!(b < 50 && (!dithered || b * 4 < a), "{name}: the middle layer's hidden fills in {b} pieces in cells, {a} exact");
        assert!(by[0] > 0.2 * (by[0] + by[1]) && by[1] > 0.2 * (by[0] + by[1]), "{name}: the inside prints by filament {by:?}");
    }
}

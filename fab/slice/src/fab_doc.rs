//! A Fab document sliced as Fab builds it (`fab-slice fab`): a project, or an object script
//! placed once with the files beside it, evaluated on the machine preset's bed and sliced with
//! its print colours on the job's filaments.

use crate::live::{slice_frames, Frame, PartField};
use crate::SliceResult;
use fab_model::eval::{Evaluated, Evaluator};
use fab_model::program::{Compiled, ScriptEnv};
use fab_model::{colour_sets, project, FabModel};
use fab_plate::machine::{Clearance, Machine, Nozzle, Rect, Technology};
use makepad_fab_settings::profiles::JobSetup;
use std::path::Path;
use std::sync::Arc;

/// The document at `doc`: a project, or an object script with its paint, strokes and pictures
/// read from beside it.
pub fn load(doc: &Path) -> Result<FabModel, String> {
    if !project::is_script(doc) {
        return project::load(doc, &ScriptEnv::default());
    }
    let text = std::fs::read_to_string(doc).map_err(|e| format!("{}: {e}", doc.display()))?;
    let dir = doc.parent().unwrap_or(Path::new("."));
    let files = fab_model::program::paint_files(&text).into_iter().filter_map(|f| std::fs::read_to_string(dir.join(&f)).ok().map(|t| (f, t))).collect();
    let mut strokes = std::collections::BTreeMap::new();
    project::load_strokes(dir, &text, &mut strokes)?;
    let file = doc.file_name().and_then(|f| f.to_str()).unwrap_or("part.cad.splash").to_string();
    let mut model = FabModel::with_script_files(&project::stem(&file), &file, &text, files, strokes);
    let mut pictures = fab_model::picture::Pictures::new();
    fab_model::picture::load(dir, &text, &mut pictures)?;
    model.pictures = Arc::new(pictures);
    Ok(model)
}

/// The machine the parts are laid out on: the machine preset's printable area and height, a
/// nozzle per extruder reaching all of it.
pub fn machine(setup: &JobSetup) -> Result<Machine, String> {
    let values = &setup.machine.values;
    let area = values.points("printable_area");
    if area.is_empty() {
        return Err("the machine preset has no printable_area".into());
    }
    let (lo, hi) = area.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), p| ([lo[0].min(p.0), lo[1].min(p.1)], [hi[0].max(p.0), hi[1].max(p.1)]));
    let bed = Rect::new(lo[0], lo[1], hi[0], hi[1]);
    let height = values.str("printable_height").parse::<f64>().unwrap_or(250.0);
    let nozzles = (0..values.vec_len("nozzle_diameter").max(1)).map(|e| Nozzle { name: format!("Tool {}", e + 1), reach: bed, max_height: height }).collect();
    Ok(Machine {
        name: setup.machine.name.clone(),
        technology: Technology::Filament,
        bed,
        max_height: height,
        clearance: Clearance { max_radius: 0.0, height_to_rod: 0.0, height_to_lid: 0.0, dist_to_rod: 0.0 },
        nozzles,
        keep_out: Vec::new(),
        slots: Vec::new(),
    })
}

/// `model` built on `setup`'s machine, its pictures printing with the `calibrated` sets as Fab
/// prints them: compiled, evaluated, and the nozzle (mm) it was built for.
pub fn evaluate(setup: &JobSetup, model: &FabModel, calibrated: &Arc<colour_sets::Sets>) -> Result<(Compiled, Evaluated, f64), String> {
    let mut e = Evaluator::new(machine(setup)?);
    e.calibrated = calibrated.clone();
    if let Some(n) = setup.machine.values.get("nozzle_diameter").and_then(|v| v.as_vec().first().and_then(|s| s.trim().parse::<f64>().ok())) {
        e.profile.nozzle = n;
    }
    let (compiled, ev) = model.evaluate(&mut e, None).map_err(|f| f.to_string())?;
    Ok((compiled, ev, e.profile.nozzle))
}

/// The evaluated parts sliced: their print colours in `colors` order print with filaments 1,
/// 2, .. (each on the nozzle the job's filament map gives it), every other with filament 1;
/// each filament's colour is its print colour's as the parts show it.
pub fn slice(setup: &mut JobSetup, ev: &mut Evaluated, colors: &[String]) -> Result<SliceResult, String> {
    let map = setup.filament_map.clone();
    let part_colors = ev.part_colors.clone();
    ev.plate.map_colors(&part_colors, &|name| colors.iter().position(|c| c == name).map(|k| (k as u32 + 1, map.get(k).map_or(0, |n| n.max(&1) - 1) as usize)));
    for (k, name) in colors.iter().enumerate().take(setup.filaments.len()) {
        let shown = ev.plate.parts.iter().filter_map(|p| p.paint.as_ref()).flat_map(|sp| sp.colors.iter()).find(|c| c.color.as_deref() == Some(name.as_str()));
        if let Some(c) = shown {
            setup.filament_colours[k] = Some(format!("#{:06X}", fab_model::program::color_srgb(c.rgb) >> 8));
        }
    }
    let frames: Vec<Frame> = ev
        .frames
        .iter()
        .zip(&ev.fields)
        .map(|(fr, field)| {
            let field = field.as_ref().and_then(|pf| {
                let mut to = pf.to_printer;
                for (r, o) in fr.offset.iter().enumerate() {
                    to[r][3] -= o;
                }
                PartField::placed(pf.shape.clone(), to)
            });
            Frame { local: fr.local.clone(), offset: fr.offset, field }
        })
        .collect();
    slice_frames(setup, &ev.plate, &frames, &[], None)
}

/// The head swaps in `gcode`: each tool selected after another.
pub fn head_swaps(gcode: &str) -> usize {
    let (mut tool, mut swaps) = (None, 0usize);
    for l in gcode.lines() {
        let t = l.trim();
        if let Some(n) = t.strip_prefix('T').filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
            if tool.is_some_and(|p: &str| p != n) {
                swaps += 1;
            }
            tool = Some(n);
        }
    }
    swaps
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_colour::{oklab, Image};
    use fab_model::picture::{self, Picture, Pictures};
    use makepad_fab_gcode::record::{MoveKind, Role};
    use std::sync::Arc;

    /// A picture going round the hues left to right and from dark to light top to bottom.
    fn wheel() -> Arc<Picture> {
        let (w, h) = (64, 64);
        let pixels = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 / (w - 1) as f32, (i / w) as f32 / (h - 1) as f32);
                let a = std::f32::consts::TAU * x;
                oklab::linear([0.3 + 0.6 * y, 0.12 * a.cos(), 0.12 * a.sin()]).map(|v| v.clamp(0.0, 1.0))
            })
            .collect();
        Arc::new(Picture { bytes: Arc::new(b"wheel".to_vec()), image: Arc::new(Image { width: w, height: h, pixels }) })
    }

    fn script(filaments: &[(&str, &str)]) -> String {
        let mut s = String::from("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n");
        for (i, (name, hex)) in filaments.iter().enumerate() {
            s.push_str(&format!("let f{i} = Material{{albedo: {hex} name: \"{name}\"}}\n"));
        }
        let list: Vec<String> = (0..filaments.len()).map(|i| format!("f{i}")).collect();
        s.push_str(&format!("let tile = m.picture({{image: \"wheel.png\" width: 16mm filaments: [{}]}})\nobject(tile)\n", list.join(" ")));
        s
    }

    /// A picture tile sliced prints every cell of every layer in the filament its dither gave
    /// it, with a white backing and without (where the backing repeats the dithered layers):
    /// each extrusion of the tile, sampled every 0.05 mm away from the cells' edges and inside
    /// the wall's ring of cells, is the cell's filament on its layer, every such cell is printed
    /// on every layer, and the heads change at most once per filament a layer.
    #[test]
    fn a_sliced_picture_prints_each_cell_in_its_dithers_filament() {
        let cmyk = [("Cyan", "#00a3e0"), ("Magenta", "#d6007e"), ("Yellow", "#ffd400"), ("Black", "#1a1a1a")];
        let cmyw = [("Cyan", "#00a3e0"), ("Magenta", "#d6007e"), ("Yellow", "#ffd400"), ("White", "#f2f2f0")];
        for filaments in [&cmyk, &cmyw] {
            let label = filaments.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("+");
            let mut model = FabModel::with_script("tile", "tile.cad.splash", &script(filaments));
            model.pictures = Arc::new(Pictures::from([("wheel.png".to_string(), wheel())]));
            let mut setup = crate::tests::bare_setup(4, &[]);
            let (compiled, mut ev, nozzle) = evaluate(&setup, &model, &Default::default()).unwrap_or_else(|e| panic!("{e}"));
            assert!(ev.warnings.is_empty(), "{:?}", ev.warnings);
            let spec = picture::specs(&compiled.instances[0].script)[0];
            let set = picture::set_of(spec, &[], &Default::default()).unwrap_or_else(|e| panic!("{e}"));
            let (_, tile) = picture::grid(spec, &set, nozzle);
            let colors: Vec<String> = filaments.iter().map(|(n, _)| n.to_string()).collect();
            let res = slice(&mut setup, &mut ev, &colors).unwrap_or_else(|e| panic!("{e}"));
            let ([nx, ny], cell, layers) = (tile.size, tile.cell, tile.layers.len());
            // the part's own lines (not the skirt, the brim, the tower or the purges)
            let part = |r: Role| matches!(r, Role::OuterWall | Role::InnerWall | Role::InternalSolidInfill | Role::TopSurface | Role::BottomSurface | Role::GapFill | Role::SparseInfill);
            let lines: Vec<_> = res.timeline.parts.iter().flatten().filter(|r| r.kind == MoveKind::Extrude && r.e > 0.0 && part(r.role)).collect();
            assert!(!lines.is_empty(), "{label}: no lines of the tile");
            // the tile's middle: its extrusions' on the first layer (the wall round it is even)
            let first = lines.iter().map(|r| r.z_layer).fold(f32::MAX, f32::min);
            let (lo, hi) = lines.iter().filter(|r| r.z_layer == first).flat_map(|r| [r.p0, r.p1]).fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), p| ([lo[0].min(p[0] as f64), lo[1].min(p[1] as f64)], [hi[0].max(p[0] as f64), hi[1].max(p[1] as f64)]));
            let origin = [0.5 * (lo[0] + hi[0]) - 0.5 * nx as f64 * cell, 0.5 * (lo[1] + hi[1]) - 0.5 * ny as f64 * cell];
            let height = spec.arg.layer;
            let (mut wrong, mut samples) = (vec![0usize; layers], vec![0usize; layers]);
            let mut printed = vec![vec![false; nx * ny]; layers];
            let mut first_wrong: Vec<Option<String>> = vec![None; layers];
            for r in &lines {
                let layer = (r.z_layer as f64 / height).round() as usize - 1;
                assert!(layer < layers, "an extrusion at z {} above the tile", r.z_layer);
                let (a, b) = ([r.p0[0] as f64, r.p0[1] as f64], [r.p1[0] as f64, r.p1[1] as f64]);
                let n = ((b[0] - a[0]).hypot(b[1] - a[1]) / 0.05).ceil().max(1.0) as usize;
                for s in 0..=n {
                    let t = s as f64 / n as f64;
                    let u = [(a[0] + t * (b[0] - a[0]) - origin[0]) / cell, (a[1] + t * (b[1] - a[1]) - origin[1]) / cell];
                    let (x, y) = (u[0].floor(), u[1].floor());
                    let near_edge = [u[0] - x, u[1] - y].iter().any(|f| f * cell < 0.05 || (1.0 - f) * cell < 0.05);
                    if near_edge || x < 2.0 || y < 2.0 || x >= nx as f64 - 2.0 || y >= ny as f64 - 2.0 {
                        continue;
                    }
                    let i = y as usize * nx + x as usize;
                    let want = tile.layers[layer][i] + 1;
                    samples[layer] += 1;
                    printed[layer][i] |= r.filament == want;
                    if r.filament != want {
                        wrong[layer] += 1;
                        first_wrong[layer].get_or_insert_with(|| format!("cell {x},{y} at {:.2},{:.2} prints filament {} for {want} ({:?})", u[0], u[1], r.filament, r.role));
                    }
                }
            }
            for l in 0..layers {
                let inside = (2..ny - 2).flat_map(|y| (2..nx - 2).map(move |x| y * nx + x));
                let missed = inside.filter(|&i| !printed[l][i]).count();
                assert!(wrong[l] == 0, "{label} layer {}: {} of {} samples print the wrong filament, first {}", l + 1, wrong[l], samples[l], first_wrong[l].as_deref().unwrap_or(""));
                assert!(missed == 0, "{label} layer {}: {missed} cells print no line of their filament", l + 1);
            }
            // a layer changes heads once per filament at most, however many its cells' runs
            let swaps = head_swaps(&res.gcode);
            assert!(swaps <= layers * filaments.len(), "{label}: {swaps} head swaps over {layers} layers");
        }
    }
}

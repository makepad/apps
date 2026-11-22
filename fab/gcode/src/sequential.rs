//! Order and clearances for printing whole objects.

use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::geom::*;
use makepad_fab_toolpath::{flow, par, Layer, Role, Toolpaths};

pub fn active(tp: &Toolpaths, job: &Job) -> bool {
    tp.objects.len() > 1 && job.s("print_sequence") == "by object"
}

pub fn order(tp: &Toolpaths) -> Vec<usize> {
    let mut out: Vec<usize> = (0..tp.objects.len()).collect();
    out.sort_by(|&a, &b| tp.objects[a].max[2].total_cmp(&tp.objects[b].max[2]).then(a.cmp(&b)));
    out
}

/// Original layer, object, and layer within that object.
pub fn layer_order(tp: &Toolpaths) -> Vec<(usize, usize, usize)> {
    let mut layers = vec![Vec::new(); tp.objects.len()];
    for (li, layer) in tp.layers.iter().enumerate() {
        for ol in &layer.objects { layers[ol.object].push(li); }
    }
    for b in &tp.np_blocks { layers[b.object].push(b.layer); }
    let mut out = Vec::new();
    for object in order(tp) {
        let list = &mut layers[object];
        list.sort_by(|&a, &b| tp.layers[a].print_z.total_cmp(&tp.layers[b].print_z).then(a.cmp(&b)));
        list.dedup();
        out.extend(list.iter().enumerate().map(|(j, &li)| (li, object, j)));
    }
    out
}

pub(crate) fn reordered(tp: &Toolpaths, sequence: &[(usize, usize, usize)]) -> Toolpaths {
    let mut out = Toolpaths { objects: tp.objects.clone(), ..Toolpaths::default() };
    let mut z = 0.0;
    let mut indices = std::collections::BTreeMap::new();
    for &(li, object, j) in sequence {
        let layer = &tp.layers[li];
        let index = out.layers.len();
        if j == 0 { z = 0.0; }
        out.layers.push(Layer { index, print_z: layer.print_z, height: layer.print_z - z,
            objects: layer.objects.iter().filter(|o| o.object == object).cloned().collect() });
        indices.insert((li, object), index);
        z = layer.print_z;
    }
    out.np_blocks = tp.np_blocks.iter().cloned().map(|mut b| {
        b.layer = indices[&(b.layer, b.object)];
        b
    }).collect();
    out.np_blocks.sort_by_key(|b| b.layer);
    out
}

pub(crate) fn skirt_loops(job: &Job) -> usize {
    let n = job.f("skirt_loops").round().max(0.0) as usize;
    if job.b("skirt_per_object") && (n > 0 || !matches!(job.s("draft_shield"), "" | "disabled")) { n.max(1) } else { 0 }
}

pub(crate) fn skirt_width(job: &Job, f: usize) -> f64 {
    let nozzle = job.nozzle_diameter(job.ext_of(f)).max(0.1);
    ["initial_layer_line_width", "line_width"].iter().map(|k| job.cfg.abs_or_pct(k, 0, nozzle)).find(|&v| v > 0.0).unwrap_or(nozzle)
}

pub(crate) fn has_brim(layer: &Layer, object: usize) -> bool {
    layer.objects.iter().filter(|o| o.object == object).flat_map(|o| &o.islands)
        .flat_map(|i| i.walls.iter().chain(&i.fills)).any(|p| p.role == Role::Brim && !p.pts.is_empty())
}

fn hull_distance(a: &[Pt], b: &[Pt]) -> f64 {
    let contains = |h: &[Pt], p| h.len() >= 3 && (0..h.len()).all(|i| orient(h[i], h[(i + 1) % h.len()], p) >= 0);
    if contains(a, b[0]) || contains(b, a[0]) { return 0.0; }
    let mut d = f64::INFINITY;
    for i in 0..a.len() {
        let (p, q) = (a[i], a[(i + 1) % a.len()]);
        for j in 0..b.len() {
            let (r, s) = (b[j], b[(j + 1) % b.len()]);
            let crosses = |u: i128, v: i128| u == 0 || v == 0 || (u < 0) != (v < 0);
            if BBox::of(&[p, q]).overlaps(&BBox::of(&[r, s]))
                && crosses(orient(p, q, r), orient(p, q, s)) && crosses(orient(r, s, p), orient(r, s, q)) { return 0.0; }
            d = d.min(dist_point_segment(p, r, s)).min(dist_point_segment(q, r, s))
                .min(dist_point_segment(r, p, q)).min(dist_point_segment(s, p, q));
        }
    }
    d / SCALE
}

pub fn check(tp: &Toolpaths, job: &Job) -> Result<(), String> {
    let positive = |key: &str| {
        let v = job.f(key);
        if v.is_finite() && v > 0.0 { Ok(v) } else { Err(format!("printing by object needs the printer's clearance ({key} must be positive)")) }
    };
    let radius = positive("extruder_clearance_max_radius")?;
    let rod = positive("extruder_clearance_height_to_rod")?;
    let lid = positive("extruder_clearance_height_to_lid")?;
    let build = positive("printable_height")?;
    let rod_dist = job.f("extruder_clearance_dist_to_rod");
    if !rod_dist.is_finite() || rod_dist < 0.0 || !job.cfg.has("extruder_clearance_dist_to_rod") {
        return Err("printing by object needs the printer's clearance (extruder_clearance_dist_to_rod must be non-negative)".into());
    }
    let mut blocks = vec![Vec::new(); tp.layers.len()];
    for b in &tp.np_blocks {
        let Some(layer) = blocks.get_mut(b.layer) else { return Err("printing by object needs a layer for every non-planar block".into()) };
        layer.push(b);
    }
    let hulls = par::map(tp.layers.len(), |li| {
        let layer = &tp.layers[li];
        let mut points = std::collections::BTreeMap::<usize, (Vec<Pt>, f64)>::new();
        for ol in &layer.objects {
            let (pts, z) = points.entry(ol.object).or_default();
            *z = z.max(layer.print_z);
            for p in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
                pts.extend_from_slice(&p.pts);
                for dz in &p.z { *z = z.max(layer.print_z + dz); }
            }
        }
        for b in &blocks[li] {
            let (pts, z) = points.entry(b.object).or_default();
            *z = z.max(b.z);
            for p in &b.paths {
                pts.extend_from_slice(&p.pts);
                for dz in &p.z { *z = z.max(b.z + dz); }
            }
        }
        points.into_iter().map(|(o, (mut pts, z))| (o, convex_hull(&mut pts), z)).collect::<Vec<_>>()
    });
    let mut hull = vec![Vec::new(); tp.objects.len()];
    let mut height = vec![0.0f64; tp.objects.len()];
    for (o, pts, z) in hulls.into_iter().flatten() {
        if o >= hull.len() { return Err("printing by object has an unknown object in its paths".into()); }
        hull[o].extend(pts);
        height[o] = height[o].max(z);
    }
    for (o, pts) in hull.iter_mut().enumerate() {
        *pts = convex_hull(pts);
        if pts.is_empty() { return Err(format!("printing by object needs extrusion paths for {}", tp.objects[o].name)); }
    }
    let nozzle = job.f("nozzle_height");
    let clearance = if nozzle.is_finite() && height.iter().all(|&h| h < nozzle) { nozzle } else { radius };
    let mut r = 0.5 * clearance - 0.1;
    let mut used: Vec<usize> = tp.layers.iter().flat_map(|l| &l.objects).filter(|o| o.has_paths()).map(|o| o.filament)
        .chain(tp.np_blocks.iter().filter(|b| !b.paths.is_empty()).map(|b| tp.objects[b.object].filament)).collect();
    used.sort_unstable();
    used.dedup();
    let mut hop = 0.0f64;
    for f in used {
        let h = job.eff("z_hop", f);
        if !h.is_finite() || h < 0.0 { return Err("printing by object needs a non-negative z_hop".into()); }
        hop = hop.max(h);
    }
    let margin = 2.0 * hop / (std::f64::consts::TAU * 3.0f64.to_radians().atan());
    let n = skirt_loops(job);
    if n > 0 {
        let distance = job.f("skirt_distance");
        if !distance.is_finite() || distance < 0.0 { return Err("printing by object needs a non-negative skirt_distance".into()); }
        let mut reach = 0.0f64;
        for (li, object, j) in layer_order(tp) {
            if j != 0 || has_brim(&tp.layers[li], object) { continue; }
            for f in tp.layers[li].objects.iter().filter(|o| o.object == object).map(|o| o.filament).chain(std::iter::once(tp.objects[object].filament)) {
                let width = skirt_width(job, f);
                let spacing = flow::spacing(width, tp.layers[li].print_z);
                reach = reach.max(distance + (n as f64 - 0.5) * spacing + 0.5 * width);
            }
        }
        r = (r + 0.5 * reach).max(reach);
    }
    let need = 2.0 * r + margin;
    for a in 0..hull.len() {
        for b in a + 1..hull.len() {
            let d = hull_distance(&hull[a], &hull[b]);
            if d < need {
                return Err(format!("{} and {} are {d:.2} mm apart: printing by object needs {need:.2} mm between objects (the toolhead's clearance radius {radius:.2} mm)", tp.objects[a].name, tp.objects[b].name));
            }
        }
    }
    let boxes: Vec<BBox> = hull.iter().map(|h| BBox::of(h)).collect();
    let sequence = order(tp);
    for (i, &object) in sequence.iter().enumerate() {
        let later = &sequence[i + 1..];
        let (mut limit, mut reason, mut next) = if let Some(&next) = later.first() { (lid.min(build), if lid <= build { "the lid" } else { "the build height" }, next) }
            else { (build, "the build height", object) };
        if let Some(&o) = later.iter().find(|&&o| boxes[object].min.y_mm() - 0.5 * rod_dist <= boxes[o].max.y_mm() + 0.5 * rod_dist
            && boxes[o].min.y_mm() - 0.5 * rod_dist <= boxes[object].max.y_mm() + 0.5 * rod_dist) {
            if rod <= limit { limit = rod; reason = "the gantry rod"; next = o; }
        }
        if height[object] > limit {
            let next = if later.is_empty() { "the end of the print" } else { &tp.objects[next].name };
            return Err(format!("{} is {:.2} mm tall: printing it before {next} needs it below {limit:.2} mm ({reason})", tp.objects[object].name, height[object]));
        }
    }
    Ok(())
}

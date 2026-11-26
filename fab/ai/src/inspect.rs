//! The AI's measuring tools: sections, measurements, mesh checks (shells,
//! slivers, thin features, weak necks), edges and the vocabulary. Every answer
//! is a Splash value, answer first, in the model frame (millimetres, Y up)
//! unless it says otherwise; lengths carry their unit.

use crate::out::{arr, deg, flag, int, mm, mm2, mm3, n, name, obj, s, S};
use fab_model::eval::{Evaluated, Evaluator};
use fab_model::program::Compiled;
use makepad_csg_cad::measure;
use makepad_csg_mesh::bvh::Bvh;
use makepad_csg_graph::TriMesh;
use makepad_csg_math::{dvec3, Vec3d};
use makepad_model::json::Value;

fn r(x: f64) -> S {
    mm(x)
}

fn v3(p: Vec3d) -> S {
    mm3([p.x, p.y, p.z])
}

fn dir3(p: Vec3d) -> S {
    arr([n(p.x), n(p.y), n(p.z)])
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) if f.is_finite() => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn p3(v: Option<&Value>) -> Option<Vec3d> {
    let a = v?.as_arr()?;
    if a.len() != 3 {
        return None;
    }
    Some(dvec3(num(&a[0])?, num(&a[1])?, num(&a[2])?))
}

fn axis_of(v: &Value) -> Result<usize, String> {
    match v.get("axis").and_then(Value::as_str).unwrap_or("y") {
        "x" => Ok(0),
        "y" => Ok(1),
        "z" => Ok(2),
        other => Err(format!("unknown axis '{other}' (x, y, z)")),
    }
}

/// The printed objects of the last evaluation (by name).
pub fn printed_objects(ev: &Evaluated) -> Vec<String> {
    ev.plate.parts.iter().map(|p| p.object.clone()).collect()
}

pub(crate) fn objects_arg(args: &Value, ev: &Evaluated) -> Vec<String> {
    match args.get("object") {
        Some(Value::Str(s)) => vec![s.clone()],
        Some(Value::Arr(list)) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => printed_objects(ev),
    }
}

/// The section of objects at a plane: area, outlines, the narrowest place
/// and a picture of it.
pub fn section(args: &Value, ev: &Evaluated, eval: &mut Evaluator) -> Result<(S, Vec<(String, Vec<u8>)>), String> {
    let axis = axis_of(args)?;
    let at = args.get("at").and_then(num).ok_or("'at' is missing: where the plane cuts, in mm along the axis")?;
    let names = objects_arg(args, ev);
    let (u, v) = match axis {
        0 => ("y", "z"),
        1 => ("x", "z"),
        _ => ("x", "y"),
    };
    let mut all = Vec::new();
    let mut rows = Vec::new();
    for name in &names {
        let mesh = eval.object_mesh(name)?;
        let regions = measure::section(&mesh, axis, at);
        if regions.is_empty() {
            continue;
        }
        let area: f64 = regions.iter().map(|g| g.area()).sum();
        let holes: usize = regions.iter().map(|g| g.holes.len()).sum();
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for g in &regions {
            let (a, b) = g.bounds();
            for k in 0..2 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        }
        let mut fields = vec![("solid", s(name)), ("area_mm2", n(area)), ("pieces", int(regions.len())), ("holes", int(holes)), ("from", mm2(lo)), ("to", mm2(hi))];
        if let Some((w, p)) = measure::section_min_width(&regions, 0.25) {
            fields.push(("min_width", r(w)));
            fields.push(("min_width_at", mm2(p)));
        }
        rows.push(obj(fields));
        all.extend(regions);
    }
    if rows.is_empty() {
        return Err(format!("the plane {}={at} cuts none of {}", ["x", "y", "z"][axis], names.join(", ")));
    }
    let (w, h, px, mm, origin) = measure::raster(&all, 768);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for g in px {
        rgba.extend_from_slice(&[g, g, g, 255]);
    }
    let image = makepad_model::RgbaImage { width: w as u32, height: h as u32, pixels: rgba };
    let png = image.to_png(&makepad_model::Limits::default()).map_err(|e| format!("{e:?}"))?;
    let label = format!(
        "Section at {}={at} mm: material dark, 10 mm grid; image x = model {u} from {:.1}, image y = model {v} from {:.1} downward, {:.3} mm per pixel",
        ["x", "y", "z"][axis], origin[0], origin[1], mm
    );
    let answer = obj(vec![("plane", s(format!("{}={at}mm", ["x", "y", "z"][axis]))), ("axes", s(format!("u={u}, v={v}"))), ("sections", arr(rows))]);
    Ok((answer, vec![(label, png)]))
}

/// Measurements: size, distance, clearance, wall thickness, angle.
/// One thread a solid is built with.
#[derive(Clone, Copy)]
struct ThreadUse {
    diameter: f64,
    pitch: Option<f64>,
    clearance: f64,
    left: bool,
    /// Cut out of the solid (a threaded hole), not standing on it.
    hole: bool,
}

/// The threads under a node: a thread on the cut side of a subtract, or a tapped hole, is a hole's.
fn threads_under(graph: &makepad_csg_graph::Graph, node: makepad_csg_graph::NodeId, cut: bool, out: &mut Vec<ThreadUse>) {
    use makepad_csg_graph::{BoolMode, HoleKind, Op};
    match &graph.node(node).op {
        Op::Thread { diameter, pitch, clearance, left, internal, .. } => out.push(ThreadUse { diameter: *diameter, pitch: *pitch, clearance: *clearance, left: *left, hole: cut || *internal }),
        Op::Boolean { mode: BoolMode::Difference, a, b } => {
            threads_under(graph, *a, cut, out);
            for tool in b {
                threads_under(graph, *tool, !cut, out);
            }
        }
        Op::Hole { source, kind, diameter, .. } => {
            if let HoleKind::Tapped { pitch, clearance } = kind {
                out.push(ThreadUse { diameter: *diameter, pitch: *pitch, clearance: *clearance, left: false, hole: !cut });
            }
            threads_under(graph, *source, cut, out);
        }
        op => {
            for input in op.inputs() {
                threads_under(graph, input, cut, out);
            }
        }
    }
}

/// Whether a threaded rod (on solid a or b) goes into a threaded hole (on the other) on this
/// printer: the radial clearance between them against what its layers and walls need. The
/// numbers are a rule until a coupon has measured the printer.
fn thread_fit(args: &Value, c: &Compiled, eval: &Evaluator) -> Result<S, String> {
    let mut threads = Vec::new();
    for key in ["a", "b"] {
        let id = args.get(key).and_then(Value::as_str).ok_or_else(|| format!("'{key}' is missing: a threaded solid's id"))?;
        let (inst, solid) = c.part(id).ok_or_else(|| format!("no solid '{id}'"))?;
        let prog = &inst.script.program;
        threads_under(&prog.graph, prog.solids[solid].node, false, &mut threads);
    }
    let rod = threads.iter().find(|t| !t.hole).copied().ok_or("no threaded rod on a or b: is it built with m.thread?")?;
    let hole = threads.iter().find(|t| t.hole).copied().ok_or("no threaded hole on a or b: subtract an m.thread, or drill a tapped m.hole")?;
    let pitch_of = |t: &ThreadUse| t.pitch.or_else(|| makepad_csg_cad::iso_coarse_pitch(t.diameter));
    let (Some(rod_pitch), Some(hole_pitch)) = (pitch_of(&rod), pitch_of(&hole)) else { return Err("a thread has no pitch and no ISO coarse pitch".into()) };
    let radial = (hole.diameter - rod.diameter) / 2.0 + hole.clearance - rod.clearance;
    let depth = 0.625 * 0.866_025_403_784 * rod_pitch;
    let profile = &eval.profile;
    // Printed apart, the gap needs less than print-in-place; the flanks' layer stairs add to it.
    let stairs = profile.layer * (30f64.to_radians().tan()) / 2.0;
    let calibrated = profile.measured.c_xy.is_some();
    let free = if calibrated { profile.c_xy() } else { 0.6 * profile.c_xy() } + stairs;
    let verdict = if (rod_pitch - hole_pitch).abs() > 1e-6 || rod.left != hole.left {
        "mismatch"
    } else if radial < free * 0.5 {
        "binds"
    } else if radial < free {
        "tight"
    } else if radial > depth * 0.5 {
        "loose"
    } else {
        "fits"
    };
    let note = match verdict {
        "mismatch" => "the pitch or the hand differs: these do not mate".to_string(),
        "binds" => format!("under half the {free:.2} mm this printer needs: expect it to jam; raise the clearance"),
        "tight" => format!("under the {free:.2} mm this printer runs free at: it may turn only with force or after chasing; raise the clearance, or print a short coupon to find out"),
        "loose" => format!("over half the {depth:.2} mm thread depth is gone: it turns freely but holds little; lower the clearance"),
        _ => "should turn by hand".to_string(),
    };
    let mut note = note;
    if rod_pitch < 2.0 * profile.nozzle {
        note.push_str(&format!("; a {rod_pitch} mm pitch is under two {} mm nozzle widths, so the flanks barely resolve", profile.nozzle));
    }
    // the check: whether it turns by hand and holds; `verdict` says how it fails
    Ok(obj(vec![
        crate::checks::Check::of(verdict == "fits").fields().remove(0),
        ("verdict", s(verdict)),
        ("radial_clearance", r(radial)),
        ("needs", r(free)),
        ("engagement", n(((1.0 - radial / depth).clamp(0.0, 1.0) * 100.0).round() / 100.0)),
        ("pitch", r(rod_pitch)),
        ("basis", s(if calibrated { "measured on this printer" } else { "the FDM rule, untested on this printer" })),
        ("note", s(note)),
    ]))
}

fn fitted_parts(args: &Value, eval: &mut Evaluator) -> Result<S, String> {
    let solid = |key| args.get(key).and_then(Value::as_str).ok_or_else(|| format!("'{key}' needs a solid id"));
    let pick = |key| p3(args.get(key)).ok_or_else(|| format!("'{key}' needs a point on the seating face, [x, y, z] in model mm"));
    let (a, b) = (solid("a")?, solid("b")?);
    if a == b { return Err("fit needs two different parts; a and b are the same part".into()); }
    let (a_at, b_at) = (pick("a_at")?, pick("b_at")?);
    let turn = args.get("turn").map(|v| num(v).ok_or("'turn' needs a finite angle in degrees")).transpose()?.unwrap_or(0.0);
    let fit = crate::fit::measure(&eval.object_mesh(a)?, &eval.object_mesh(b)?, a_at, b_at, turn)?;

    // Keep small gaps readable down to the contact tolerance; mm() rounds to hundredths.
    let length = |x: f64| S::Verbatim(format!("{}mm", (x * 1e9).round() / 1e9 + 0.0));
    let point = |p: Vec3d| arr([p.x, p.y, p.z].map(length));
    let direction = |p: Vec3d| arr([p.x, p.y, p.z].map(S::Num));
    let sides = fit.gaps.iter().enumerate().filter_map(|(i, gap)| {
        let g = gap.as_ref()?;
        let dir = fit.directions[i];
        let axis = dir.dominant_axis();
        let model_axis = [Vec3d::X, Vec3d::Y, Vec3d::Z][axis];
        let label = if dir.cross(model_axis).length() < 1e-12 {
            ["x", "y", "z"][axis]
        } else if i < 2 { "u" } else { "v" };
        let sign = if i % 2 == 0 { "+" } else { "-" };
        Some(obj(vec![("side", s(format!("{sign}{label}"))), ("direction", direction(dir)),
            ("gap", length(g.distance)), ("from", point(g.a)), ("to", point(g.b))]))
    });
    let mut fields = vec![
        ("seat", flag(fit.seat)),
        ("overlap_mm3", S::Num(fit.overlap_mm3)),
        ("side_gap", fit.side_gap.map_or(S::Nil, length)),
        ("sides", arr(sides)),
        ("pose", obj(vec![("axis", direction(fit.axis)), ("angle", S::Num(fit.degrees)), ("move", point(fit.translation))])),
    ];
    if let Some(bb) = fit.overlap_bounds {
        fields.push(("overlap_bounds", obj(vec![("min", point(bb.min)), ("max", point(bb.max))])));
    }
    Ok(obj(fields))
}

pub fn measure_tool(args: &Value, c: &Compiled, ev: &Evaluated, eval: &mut Evaluator) -> Result<S, String> {
    let what = args.get("what").and_then(Value::as_str).ok_or("'what' is missing: size, clearance, distance, wall, angle, edge, faces, fit, thread_fit or mass")?;
    match what {
        "fit" => fitted_parts(args, eval),
        "thread_fit" => thread_fit(args, c, eval),
        "size" => {
            let mut rows = Vec::new();
            for name in objects_arg(args, ev) {
                let m = eval.object_mesh(&name)?;
                let bb = m.bounding_box();
                let vol = makepad_csg_cad::signed_volume(&m);
                let exact = eval.object_exact(&name).ok().flatten();
                rows.push(obj(vec![("exact_volume_mm3", exact.map_or(S::Nil, |b| n(b.measure.volume))), ("solid", s(&name)), ("min", v3(bb.min)), ("max", v3(bb.max)), ("size", v3(bb.max - bb.min)), ("volume_mm3", n(vol))]));
            }
            Ok(obj(vec![("sizes", arr(rows))]))
        }
        "clearance" | "distance" => {
            // Points or objects on either side.
            let mut side = |k: &str| -> Result<Result<Vec3d, TriMesh>, String> {
                match args.get(k) {
                    Some(Value::Str(name)) => eval.object_mesh(name).map(Err),
                    Some(p) => p3(Some(p)).map(Ok).ok_or_else(|| format!("'{k}' is a solid id or [x, y, z] mm")),
                    None => Err(format!("'{k}' is missing")),
                }
            };
            let (a, b) = (side("a")?, side("b")?);
            let (d, pa, pb) = match (&a, &b) {
                (Ok(p), Ok(q)) => (p.distance(*q), *p, *q),
                (Ok(p), Err(m)) | (Err(m), Ok(p)) => {
                    let bvh = Bvh::build(m);
                    let (d, q, _) = bvh.closest(*p).ok_or("the object is empty")?;
                    if matches!(a, Ok(_)) { (d, *p, q) } else { (d, q, *p) }
                }
                (Err(ma), Err(mb)) => {
                    let (d, pa, pb) = measure::clearance(ma, mb).ok_or("an object is empty")?;
                    // Overlapping solids report no clearance.
                    let overlap = makepad_csg_cad::intersection(ma, mb);
                    if !overlap.triangles.is_empty() && makepad_csg_cad::signed_volume(&overlap) > 1e-6 {
                        return Ok(obj(vec![("clearance", r(0.0)), ("overlap_mm3", n(makepad_csg_cad::signed_volume(&overlap))), ("note", s("the solids overlap"))]));
                    }
                    (d, pa, pb)
                }
            };
            let key = if what == "clearance" { "clearance" } else { "distance" };
            Ok(obj(vec![(key, r(d)), ("from", v3(pa)), ("to", v3(pb)), ("delta", v3(pb - pa))]))
        }
        "wall" => {
            let name = args.get("object").and_then(Value::as_str).ok_or("'object' is missing")?;
            let at = p3(args.get("at")).ok_or("'at' is missing: a point on or near the surface, [x, y, z]")?;
            let m = eval.object_mesh(name)?;
            let bvh = Bvh::build(&m);
            let (d, q, tri) = bvh.closest(at).ok_or("the object is empty")?;
            let nrm = match p3(args.get("dir")) {
                Some(dir) if dir.length() > 1e-9 => dir.normalize() * -1.0,
                _ => m.triangle_normal(tri as usize),
            };
            let t = measure::thickness_at(&bvh, q, nrm, Some(tri)).ok_or("the ray found no far side (is the solid closed?)")?;
            Ok(obj(vec![("wall", r(t)), ("from", v3(q)), ("to", v3(q - nrm * t)), ("off_surface", r(d))]))
        }
        "angle" => {
            let name = args.get("object").and_then(Value::as_str).ok_or("'object' is missing")?;
            let (a, b) = (p3(args.get("a")).ok_or("'a' is a point on one face")?, p3(args.get("b")).ok_or("'b' is a point on the other face")?);
            let m = eval.object_mesh(name)?;
            let bvh = Bvh::build(&m);
            let (_, _, ta) = bvh.closest(a).ok_or("the object is empty")?;
            let (_, _, tb) = bvh.closest(b).ok_or("the object is empty")?;
            let (na, nb) = (m.triangle_normal(ta as usize), m.triangle_normal(tb as usize));
            let between = na.dot(nb).clamp(-1.0, 1.0).acos().to_degrees();
            Ok(obj(vec![("normals_angle", deg(between)), ("faces_angle", deg(180.0 - between)), ("normal_a", dir3(na)), ("normal_b", dir3(nb))]))
        }
        "edge" => {
            let name = args.get("object").and_then(Value::as_str).ok_or("'object' is missing")?;
            let at = p3(args.get("at")).ok_or("'at' is a point near an edge, [x, y, z]")?;
            let mesh = eval.object_mesh(name)?;
            let positions: Vec<[f64; 3]> = mesh.vertices.iter().map(|p| [p.x, p.y, p.z]).collect();
            let indices: Vec<u32> = mesh.triangles.iter().flatten().copied().collect();
            let points = measure::feature_edge_chain(&positions, &indices, [at.x, at.y, at.z]).ok_or("the object has no feature edges")?;
            let mut fields = vec![("solid", s(name)), ("length", r(measure::edge_length(&points)))];
            if let Some(circle) = measure::fit_circle3(&points).filter(|c| c.rms < 1e-3 * c.radius) {
                fields.push(("radius", r(circle.radius)));
                fields.push(("center", v3(dvec3(circle.center[0], circle.center[1], circle.center[2]))));
            }
            Ok(obj(fields))
        }
        "faces" => {
            let name = args.get("object").and_then(Value::as_str).ok_or("'object' is missing")?;
            let a = p3(args.get("a")).ok_or("'a' is a point on one face")?;
            let b = p3(args.get("b")).ok_or("'b' is a point on the other face")?;
            let mesh = eval.object_mesh(name)?;
            let bvh = Bvh::build(&mesh);
            let (_, pa, ta) = bvh.closest(a).ok_or("the object is empty")?;
            let (_, pb, tb) = bvh.closest(b).ok_or("the object is empty")?;
            let (na, nb) = (mesh.triangle_normal(ta as usize), mesh.triangle_normal(tb as usize));
            let gap = measure::plane_distance([pa.x, pa.y, pa.z], [na.x, na.y, na.z], [pb.x, pb.y, pb.z], [nb.x, nb.y, nb.z]);
            Ok(obj(vec![("distance", r(gap.distance)), ("angle", deg(gap.angle_deg)), ("parallel", flag(gap.parallel)), ("from", v3(pa)), ("to", v3(pb))]))
        }
        other => Err(format!("unknown measurement '{other}' (size, clearance, distance, wall, angle, edge, faces, fit, thread_fit, mass)")),
    }
}

/// One finding of the mesh check.
fn finding(kind: &str, severity: &str, object: &str, at: Option<Vec3d>, message: String, hint: &str) -> S {
    let mut f = vec![("kind", name(kind)), ("severity", name(severity)), ("solid", s(object))];
    if let Some(p) = at {
        f.push(("at", v3(p)));
    }
    f.push(("message", s(message)));
    f.push(("hint", s(hint)));
    obj(f)
}

/// Mesh checks of printed objects: closed/manifold, separate shells and
/// specks, slivers, thin features (thinner than two line widths) and weak
/// necks (a cross-section much smaller than above and below it).
pub fn check_mesh(args: &Value, ev: &Evaluated, eval: &mut Evaluator, line_width: f64) -> Result<S, String> {
    let (findings, rows) = mesh_findings(args, ev, eval, line_width, true)?;
    Ok(obj(vec![crate::checks::Check::of(!crate::checks::has_error(&findings)).fields().remove(0), ("frame", s("model (mm, Y up)")), ("line_width", r(line_width)), ("findings", arr(findings)), ("solids", arr(rows))]))
}

/// The mesh check's findings and its per-solid rows.
/// `fields`: the parts drawn from a distance field too (they are meshed for it); without, a
/// plate check leaves them out (their field is checked by the slicer, never meshed).
pub fn mesh_findings(args: &Value, ev: &Evaluated, eval: &mut Evaluator, line_width: f64, fields: bool) -> Result<(Vec<S>, Vec<S>), String> {
    let mut rows = Vec::new();
    let mut findings = Vec::new();
    let field_part = |name: &str| ev.plate.parts.iter().position(|p| p.object == name).is_some_and(|i| ev.fields.get(i).is_some_and(Option::is_some));
    for name in objects_arg(args, ev).into_iter().filter(|n| fields || !field_part(n)) {
        let m = eval.object_mesh(&name)?;
        let report = makepad_csg_mesh::validate::validate_mesh(&m);
        let shells = measure::shells(&m);
        let total: f64 = shells.iter().map(|s| s.volume.abs()).sum::<f64>().max(1e-9);
        let specks: Vec<&measure::Shell> = shells.iter().filter(|s| s.volume.abs() < 1.0 || s.volume.abs() < total * 1e-3).collect();
        let slivers = measure::sliver_count(&m, 1e-4);
        let samples = measure::thickness_samples(&m, 6000);
        let min_ok = 2.0 * line_width;
        let mut thin: Vec<&measure::ThicknessSample> = samples.iter().filter(|s| s.wall && s.thickness < min_ok).collect();
        thin.sort_by(|a, b| a.thickness.total_cmp(&b.thickness));
        let min_t = samples.iter().map(|s| s.thickness).fold(f64::INFINITY, f64::min);
        let mut row = vec![("solid", s(&name)), ("closed", flag(report.is_closed)), ("manifold", flag(report.is_manifold)), ("shells", int(shells.len())), ("triangles", int(m.triangles.len())), ("slivers", int(slivers))];
        if min_t.is_finite() {
            row.push(("min_wall", r(min_t)));
        }
        rows.push(obj(row));
        if !report.is_closed {
            findings.push(finding("open", "error", &name, None, format!("{} open edges: the solid is not closed", report.boundary_edges), "Overlap touching parts by a little, or rebuild the feature that opened it (fab.inspect names the last ones)."));
        }
        for s in specks.iter().take(5) {
            findings.push(finding("speck", "warning", &name, Some((s.lo + s.hi) * 0.5), format!("a separate piece of {:.3} mm³ ({} triangles), from {:.2?} to {:.2?}", s.volume.abs(), s.triangles, [s.lo.x, s.lo.y, s.lo.z], [s.hi.x, s.hi.y, s.hi.z]), "Usually a boolean sliver: grow the cutter past the face it trims, or join it to the body."));
        }
        if shells.len() - specks.len() > 1 {
            findings.push(finding("pieces", "info", &name, None, format!("{} separate solids in one part", shells.len() - specks.len()), "Fine if intended (they print together); otherwise join them with a boolean union."));
        }
        // Thin features: report the thinnest few places, at least 2 mm apart.
        let mut shown: Vec<Vec3d> = Vec::new();
        for s in thin {
            if shown.iter().any(|p| p.distance(s.at) < 2.0) {
                continue;
            }
            shown.push(s.at);
            let severity = if s.thickness < line_width { "error" } else { "warning" };
            findings.push(finding("thin_feature", severity, &name, Some(s.at), format!("{:.2} mm thick here, under two line widths ({:.2} mm){}", s.thickness, min_ok, if s.thickness < line_width { ": thinner than one line, it will not print" } else { ": it prints as a single weak line" }), "Thicken it to two line widths or more, or use a smaller nozzle."));
            if shown.len() >= 5 {
                break;
            }
        }
        // Weak necks along the print's up axis (model Y): a cross-section far
        // smaller than the ones a few millimetres above and below.
        let bb = m.bounding_box();
        let step = 0.2;
        let n = ((bb.max.y - bb.min.y) / step).floor() as usize;
        let areas: Vec<(f64, f64)> = (1..n).map(|k| {
            let y = bb.min.y + k as f64 * step;
            (y, measure::section(&m, 1, y).iter().map(|g| g.area()).sum())
        }).collect();
        let reach = (5.0 / step) as usize;
        let mut last_neck = f64::NEG_INFINITY;
        for i in 0..areas.len() {
            let (y, a) = areas[i];
            if a <= 0.0 {
                continue;
            }
            let below = areas[i.saturating_sub(reach)..i].iter().map(|x| x.1).fold(0.0, f64::max);
            let above = areas[i + 1..(i + 1 + reach).min(areas.len())].iter().map(|x| x.1).fold(0.0, f64::max);
            if below > 0.0 && above > 0.0 && a < 0.15 * below.min(above) && y - last_neck > 2.0 {
                let regions = measure::section(&m, 1, y);
                let width = measure::section_min_width(&regions, 0.05).map(|w| w.0).unwrap_or(0.0);
                if width < min_ok * 1.5 {
                    last_neck = y;
                    let (lo, hi) = regions.iter().fold(([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]), |(lo, hi), g| {
                        let (a, b) = g.bounds();
                        ([lo[0].min(a[0]), lo[1].min(a[1])], [hi[0].max(b[0]), hi[1].max(b[1])])
                    });
                    let at = dvec3((lo[0] + hi[0]) / 2.0, y, (lo[1] + hi[1]) / 2.0);
                    findings.push(finding("weak_neck", if width < line_width { "error" } else { "warning" }, &name, Some(at), format!("the part narrows to {:.2} mm² ({:.2} mm wide) at Y {:.1}, against {:.1} mm² below and {:.1} mm² above: what stands on it can snap off or print in the air", a, width, y, below, above), "Thicken the neck to at least two line widths, or add a fillet where it meets the body."));
                }
            }
        }
    }
    Ok((findings, rows))
}

/// A solid's sharp edges, for the AI to choose fillet and chamfer selectors.
pub fn edges_value(mesh: &TriMesh) -> S {
    let chains = makepad_csg_cad::feature_edges(mesh, 30.0);
    arr(chains.iter().take(64).map(|c| {
        let m = c.midpoint();
        let mut fields = vec![("kind", name(c.kind_name())), ("convex", flag(!c.concave)), ("at", v3(m)), ("length", r(c.length())), ("bend", deg(c.bend_deg))];
        if let Some(d) = c.direction() {
            fields.push(("dir", dir3(d)));
        }
        if let makepad_csg_cad::EdgeShape::Circle { radius, .. } = c.shape {
            fields.push(("radius", r(radius)));
        }
        obj(fields)
    }))
}

/// A face name as the script writes it: `b.top`, `e.side(section.base)` or `e.side(2)`, `s.inner()`, `s.inner(b.front)`,
/// `r.face(m.edge(b.top, b.front))`, `r.corner(b.top, b.front, b.right)` (`b` the part it
/// comes from).
pub fn written(
    n: &makepad_csg_graph::edges::FaceName,
    parts: &std::collections::HashMap<makepad_csg_graph::Key, String>,
    sides: &std::collections::HashMap<makepad_csg_graph::Key, Vec<Option<String>>>,
) -> String {
    use makepad_csg_graph::edges::Role;
    let of = parts.get(&n.origin).map_or("?", String::as_str);
    let list = |l: &[&makepad_csg_graph::edges::FaceName]| l.iter().map(|n| written(n, parts, sides)).collect::<Vec<_>>().join(", ");
    match &n.role {
        Role::Named(f) => format!("{of}.{}", f.name()),
        Role::Side(i) => match sides.get(&n.origin).and_then(|names| names.get(*i as usize)).and_then(Option::as_deref) {
            Some(segment) => format!("{of}.side({segment})"),
            None => format!("{of}.side({i})"),
        },
        Role::Inner(None) => format!("{of}.inner()"),
        Role::Inner(Some(outer)) => format!("{of}.inner({})", written(outer, parts, sides)),
        Role::Blend(a, b) => format!("{of}.face(m.edge({}))", list(&[a, b])),
        Role::Corner(_) => format!("{of}.corner({})", list(&corner_faces(n, parts))),
    }
}

/// The names a corner is written with: those of its faces the script can write (each face holds
/// its part's name and its builder's), else all.
fn corner_faces<'n>(n: &'n makepad_csg_graph::edges::FaceName, parts: &std::collections::HashMap<makepad_csg_graph::Key, String>) -> Vec<&'n makepad_csg_graph::edges::FaceName> {
    let makepad_csg_graph::edges::Role::Corner(round) = &n.role else { return Vec::new() };
    let some: Vec<_> = round.iter().filter(|x| writable(x, parts)).collect();
    if some.is_empty() { round.iter().collect() } else { some }
}

/// Whether the script can write name `n`: every part it comes from is a part the script names
/// (`leg2`; not a builder under its part's placement, nor an anonymous call inside another).
pub(crate) fn writable(n: &makepad_csg_graph::edges::FaceName, parts: &std::collections::HashMap<makepad_csg_graph::Key, String>) -> bool {
    use makepad_csg_graph::edges::Role;
    let named = parts.get(&n.origin).is_some_and(|id| !id.is_empty() && !id.starts_with(|c: char| c.is_ascii_digit()) && id.chars().all(|c| c.is_alphanumeric() || c == '_'));
    named && match &n.role {
        Role::Inner(Some(outer)) => writable(outer, parts),
        Role::Blend(a, c) => writable(a, parts) && writable(c, parts),
        Role::Corner(round) => round.iter().any(|x| writable(x, parts)),
        _ => true,
    }
}

/// The steps name `n` comes from as written: its own and its faces' (a corner's written ones).
fn origins(n: &makepad_csg_graph::edges::FaceName, parts: &std::collections::HashMap<makepad_csg_graph::Key, String>, out: &mut Vec<makepad_csg_graph::Key>) {
    use makepad_csg_graph::edges::Role;
    out.push(n.origin);
    match &n.role {
        Role::Inner(Some(outer)) => origins(outer, parts, out),
        Role::Blend(a, c) => {
            origins(a, parts, out);
            origins(c, parts, out);
        }
        Role::Corner(_) => corner_faces(n, parts).into_iter().for_each(|x| origins(x, parts, out)),
        _ => {}
    }
}

/// Face `face`'s names the script can write, in the order a pick takes them: from steps the body
/// holds once first (a step it holds twice names every copy), then the outermost part's first.
/// Never by the names' own order: they are sorted by key.
pub(crate) fn face_names(b: &fab_model::eval::ExactBody, face: u32) -> Vec<&makepad_csg_graph::edges::FaceName> {
    let Some(f) = b.faces.iter().find(|f| f.id == face) else { return Vec::new() };
    let mut names: Vec<_> = f.names.iter().filter(|n| writable(n, &b.parts)).collect();
    names.sort_by_cached_key(|n| {
        let mut keys = Vec::new();
        origins(n, &b.parts, &mut keys);
        (keys.iter().any(|k| b.repeated.contains(k)), b.order.iter().position(|k| *k == n.origin))
    });
    names
}

/// The name that picks a face: its first in [`face_names`]' order. One every copy shares only
/// when it has no other (the edits then refuse it, [`shared_name`]).
pub(crate) fn face_pick(b: &fab_model::eval::ExactBody, face: u32) -> Option<String> {
    face_names(b, face).first().map(|n| written(n, &b.parts, &b.sides))
}

/// Whether face `face` holds the name a script writes as `name`.
pub(crate) fn face_holds(b: &fab_model::eval::ExactBody, face: u32, name: &str) -> bool {
    face_named(b, face, name).is_some()
}

/// The names of an edge's two faces that pick it, in order (`m.edge(a, b)`).
pub(crate) fn edge_pair(b: &fab_model::eval::ExactBody, e: &makepad_csg_graph::BrepEntity) -> Option<[String; 2]> {
    let [x, y] = e.faces[..] else { return None };
    let mut pair = [face_pick(b, x)?, face_pick(b, y)?];
    pair.sort();
    Some(pair)
}

/// The edges where faces named `a` and `c` meet: the pieces `m.edge(a, c)` picks.
pub(crate) fn edges_between<'a>(b: &'a fab_model::eval::ExactBody, a: &str, c: &str) -> Vec<&'a makepad_csg_graph::BrepEntity> {
    b.edges.iter().filter(|e| matches!(e.faces[..], [x, y] if (face_holds(b, x, a) && face_holds(b, y, c)) || (face_holds(b, x, c) && face_holds(b, y, a)))).collect()
}

/// The selector of edge `e` picked between `a` and `c`: `m.edge(a, c)`, with `near:` (its middle)
/// when the two faces meet in several pieces, and `chain: true` for its tangent chain.
pub(crate) fn edge_selector(b: &fab_model::eval::ExactBody, [a, c]: &[String; 2], e: &makepad_csg_graph::BrepEntity, chain: bool) -> String {
    edge_text(a, c, edge_near(b, [a.as_str(), c.as_str()], e), chain)
}

/// Where an edge `m.edge(a, c)` names is: its point, when the two faces meet in several pieces.
pub(crate) fn edge_near(b: &fab_model::eval::ExactBody, [a, c]: [&str; 2], e: &makepad_csg_graph::BrepEntity) -> Option<[f64; 3]> {
    (edges_between(b, a, c).len() > 1).then_some(e.at)
}

/// `m.edge(a, c)` with its options: the point `near` it, the chain.
pub(crate) fn edge_text(a: &str, c: &str, near: Option<[f64; 3]>, chain: bool) -> String {
    let near = near.map(|at| format!("near: {}", makepad_script::syntax::write::list(at.map(|v| format!("{v:.2}mm")))));
    let options: Vec<String> = near.into_iter().chain(chain.then(|| "chain: true".to_string())).collect();
    if options.is_empty() { format!("m.edge({a}, {c})") } else { format!("m.edge({a}, {c}, {{{}}})", options.join(" ")) }
}

/// Why an edit by the name `name` face `face` holds must not be made: the name comes from a part
/// the body holds more than once (a part and its anonymous mirror, two equal unplaced boxes), so
/// it names every copy's face and an edit by it could land on another copy. None: the name is
/// this face's own.
pub(crate) fn shared_name(b: &fab_model::eval::ExactBody, face: u32, name: &str) -> Option<String> {
    let n = face_named(b, face, name)?;
    let mut keys = Vec::new();
    origins(n, &b.parts, &mut keys);
    let key = keys.into_iter().find(|k| b.repeated.contains(k))?;
    let part = b.parts.get(&key).map_or("?", String::as_str);
    Some(format!("two faces share the name `{name}`: this body holds the same `{part}` more than once, and an edit by that name could land on the other copy, so nothing was changed (give each copy a `let` of its own, placed where it goes)"))
}

/// The name face `face` holds that the script writes as `text`.
pub(crate) fn face_named<'b>(b: &'b fab_model::eval::ExactBody, face: u32, text: &str) -> Option<&'b makepad_csg_graph::edges::FaceName> {
    let numeric = std::collections::HashMap::new();
    b.faces.iter().find(|f| f.id == face)?.names.iter().find(|n| {
        written(n, &b.parts, &b.sides) == text || (!b.sides.is_empty() && written(n, &b.parts, &numeric) == text)
    })
}

/// An exact body's sharp edges, each with its two faces and the selector that picks it
/// (`m.edge(a, b)`, with `near:` where the two faces meet in several pieces).
pub fn exact_edges(b: &fab_model::eval::ExactBody) -> S {
    let sharp: Vec<_> = b.edges.iter().filter(|e| !e.smooth).collect();
    let pairs: Vec<_> = sharp.iter().map(|e| edge_pair(b, e)).collect();
    arr(sharp.iter().zip(&pairs).take(64).map(|(e, pair)| {
        let mut f = vec![("id", int(e.id)), ("kind", name(e.kind)), ("convex", flag(e.convex)), ("at", v3(dvec3(e.at[0], e.at[1], e.at[2]))), ("length", r(e.size))];
        if let Some(radius) = e.radius {
            f.push(("radius", r(radius)));
        }
        if !e.faces.is_empty() {
            f.push(("faces", arr(e.faces.iter().map(|&i| int(i)))));
        }
        if let Some([a, c]) = pair {
            let shared = pairs.iter().filter(|p| *p == pair).nth(1).is_some();
            f.push(("edge", s(edge_text(a, c, shared.then_some(e.at), false))));
        }
        obj(f)
    }))
}

/// An exact body in a tool's answer: exact volume, area and centroid, then its edges and faces,
/// to pick m.brep.fillet / chamfer / shell selectors from: a face's names as the script writes
/// them, an edge's selector by its two faces.
pub fn exact_value(name_of: &str, b: &fab_model::eval::ExactBody) -> S {
    let at = |p: [f64; 3]| v3(dvec3(p[0], p[1], p[2]));
    let faces = b.faces.iter().take(64).map(|e| {
        let mut f = vec![("id", int(e.id)), ("kind", name(e.kind)), ("at", at(e.at)), ("area_mm2", n(e.size))];
        if let Some(normal) = e.normal {
            f.push(("normal", at(normal)));
        }
        // the names the script can write, the one a pick takes first
        let mut names: Vec<String> = Vec::new();
        for n in face_names(b, e.id) {
            let n = written(n, &b.parts, &b.sides);
            if !names.contains(&n) {
                names.push(n);
            }
        }
        if !names.is_empty() {
            f.push(("names", arr(names.iter().map(|n| s(n)))));
        }
        obj(f)
    });
    obj(vec![
        ("solid", s(name_of)),
        ("volume_mm3", n(b.measure.volume)),
        ("area_mm2", n(b.measure.area)),
        ("centroid", at(b.measure.centroid)),
        ("min", at(b.measure.min)),
        ("max", at(b.measure.max)),
        ("edges", exact_edges(b)),
        ("faces", arr(faces)),
    ])
}

/// The model language's vocabulary, from its prototype table: every type with its fields
/// and an example, for the guide.
pub fn vocabulary(group: &str) -> Result<String, String> {
    use makepad_model_script::lang::protos::{Group, PROTOS};
    let groups = [Group::Primitive, Group::Generator, Group::Mechanical, Group::Text, Group::Modify, Group::Combine, Group::Field, Group::Brep, Group::Joint, Group::Weave, Group::Lighten, Group::Sketch, Group::Shape, Group::Segment, Group::Look, Group::Study, Group::Handle];
    let names = |prefix: &str| groups.iter().map(|g| match g.aliases() {
        [] => g.name().to_string(),
        aliases => format!("{} ({prefix}{})", g.name(), aliases.join(" or ")),
    }).collect::<Vec<_>>().join(", ");
    let group = group.trim();
    if group.is_empty() {
        return Ok(format!("Ask for `vocabulary <group>`: {}. A builder's fields are required first, then optional (?).", names("")));
    }
    let Some(g) = groups.iter().copied().find(|g| g.name().eq_ignore_ascii_case(group) || g.aliases().iter().any(|alias| alias.eq_ignore_ascii_case(group))) else {
        return Err(format!("no vocabulary group '{}'; the groups are: {}", group.escape_debug(), names("also ")));
    };
    let mut out = String::new();
    for p in PROTOS.iter().filter(|p| p.group == g) {
        let mut fields: Vec<String> = p.fields.iter().filter(|f| f.required).map(|f| format!("{} ({})", f.key, f.kind.name())).collect();
        fields.extend(p.fields.iter().filter(|f| !f.required).map(|f| format!("{}? ({})", f.key, f.kind.name())));
        let head = if p.call.is_empty() { p.name.to_string() } else if p.args.is_empty() { format!("m.{}({{..}})", p.path()) } else { format!("m.{}({}, {{..}})", p.path(), p.args.join(", ")) };
        out.push_str(&format!("- `{head}`: {} Fields: {}. Example: `{}`\n", p.help, fields.join(", "), p.example));
    }
    Ok(out)
}

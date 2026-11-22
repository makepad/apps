//! The bodies of one object glued by the slicer, layer by layer: a field head with two exact
//! locks sunk into it and out of its sides, listed as one object (`object([head, lock, other])`).
//! Glued, each layer prints the union of the outlines, so no wall runs and nothing prints twice
//! where they overlap, the volume is counted once, and the supports are planned for one body:
//! none climbs through the head to a lock's sunk underside. As separate bodies, each of those
//! fails. Locks of another filament own all of themselves, the head's paint by position holds on
//! the rest, and no facet inside the head hints an overhang. A leg segment of the flex beetle
//! bonded to its TPU cord is no one body: each prints apart, and the space they share is the
//! cord's, which runs on unbroken through the segment.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use makepad_fab_slice::SliceResult;
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::{bbox_of, total_area_mm2, ExPolygon, Pt, SCALE};
use makepad_fab_slicer::offset::offset_mm;
use makepad_fab_toolpath::Role;
use std::collections::{BTreeMap, BTreeSet};

/// A ball of a head (a field) and two locks of hair (exact sweeps) from inside it out of its
/// sides, `extra` (paint) before the object.
fn script(extra: &str) -> String {
    format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\
let head = m.sdf({{field: fn(p) {{ return sdf_sphere(p - vec3(0, 12mm, 0), 12mm) }} bounds: [[-12.5mm, -0.5mm, -12.5mm], [12.5mm, 24.5mm, 12.5mm]] cell: 0.5mm}})\n\
let lock = m.brep.sweep({{sketch: Circle{{radius: 2.5mm}} path: [[0 14mm 0] [7mm 15mm 0] [14mm 16mm 0] [22mm 13mm 0]]}})\n\
let other = m.brep.sweep({{sketch: Circle{{radius: 2.5mm}} path: [[0 14mm 0] [-7mm 15mm 0] [-14mm 16mm 0] [-22mm 13mm 0]]}})\n\
{extra}object([head, lock, other], name: \"bust\")\n")
}

/// The locks in brown.
const BROWN: &str = "let brown = Material{albedo: [0.4 0.25 0.1] name: \"Brown\"}\nm.fill(lock, brown)\nm.fill(other, brown)\n";

/// A red stroke on the head's front, low and away from the locks.
const STROKE: &str = "let red = Material{albedo: [0.8 0.05 0.05] name: \"Red\"}\nm.stroke([[0 6mm 10.392mm] [4mm 6mm 9.592mm]], 3mm, red)\n";

const PLATE: &str = "use mod.fab.*\nscript(\"hair.cad.splash\")\nplace(\"hair\", object: \"bust\", at: [175mm 160mm])\n";

/// The plate of `source`, its bodies one object when `glued` (as evaluated), else each its own,
/// as the slicer's frames (each where the evaluator placed it). The colours `tools` names print
/// in their filaments (1-based), the rest in filament 1.
fn plate(source: &str, glued: bool, tools: &[(&str, u32)]) -> (Evaluated, Vec<Frame>) {
    let scripts = BTreeMap::from([("hair.cad.splash".to_string(), source.to_string())]);
    let model = FabModel::load("hair", PLATE, scripts).expect("the plate loads");
    let mut ev = model.evaluate(&mut Evaluator::new(Machine::dual_nozzle_350()), None).unwrap_or_else(|f| panic!("{f}")).1;
    assert_eq!(ev.plate.parts.len(), 3, "a head and two locks");
    assert!(ev.fields[0].is_some() && ev.plate.parts[1..].iter().all(|p| p.print_mesh.is_some()), "the head a field, the locks exact");
    assert_eq!(ev.plate.groups(), vec![vec![0, 1, 2]], "the object's bodies are one group, in its order");
    let colors = ev.part_colors.clone();
    ev.plate.map_colors(&colors, &|name| Some((tools.iter().find(|t| t.0 == name).map_or(1, |t| t.1), 0)));
    if !glued {
        for p in &mut ev.plate.parts {
            p.body_of = None;
        }
    }
    let frames = frames_of(&ev);
    (ev, frames)
}

/// The slicer's frames of `ev`'s parts, each where the evaluator placed it.
fn frames_of(ev: &Evaluated) -> Vec<Frame> {
    ev.frames.iter().zip(&ev.fields).map(|(fr, pf)| {
        let field = pf.as_ref().map(|pf| {
            let mut to = pf.to_printer;
            for (r, o) in fr.offset.iter().enumerate() {
                to[r][3] -= o;
            }
            PartField::new(pf.shape.clone(), to)
        });
        Frame { local: fr.local.clone(), offset: fr.offset, field }
    }).collect()
}

/// The job: `n` filaments on one nozzle, each of which prints (no tool is dropped, so a path's
/// filament is the plate's).
fn setup(ev: &Evaluated, n: usize) -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.2");
    process.values.set_str("enable_support", "1");
    process.values.set_str("support_type", "normal(auto)");
    let mut machine = Preset::default();
    machine.values.set_vec("printable_area", ev.machine.bed.corners());
    JobSetup { machine, process, filaments: vec![Preset::default(); n], filament_map: vec![1; n], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None; n] }
}

fn slice(source: &str, glued: bool, tools: &[(&str, u32)]) -> (Evaluated, SliceResult) {
    let (ev, frames) = plate(source, glued, tools);
    let res = slice_frames(&setup(&ev, 1 + tools.len()), &ev.plate, &frames, &[], None).expect("slices");
    (ev, res)
}

/// Layer key: its print z in µm.
fn key(z: f64) -> i64 {
    (z * 1000.0).round() as i64
}

/// Object `object`'s model regions per layer, and each layer's height.
fn regions(res: &SliceResult, object: usize) -> BTreeMap<i64, (Vec<ExPolygon>, f64)> {
    let mut out = BTreeMap::new();
    for l in &res.toolpaths.layers {
        let r: Vec<ExPolygon> = l.objects.iter().filter(|o| o.object == object && !o.support_only).flat_map(|o| o.slices.iter().cloned()).collect();
        if !r.is_empty() {
            out.insert(key(l.print_z), (r, l.height));
        }
    }
    out
}

/// Per layer, the points of every path whose role `keep` takes, with its object and filament;
/// with `step` (mm) a point that far apart all along each path too: a long line's own points
/// may all lie outside a region it crosses.
fn points(res: &SliceResult, keep: impl Fn(Role) -> bool, step: Option<f64>) -> BTreeMap<i64, Vec<(usize, usize, Pt)>> {
    let mut out: BTreeMap<i64, Vec<(usize, usize, Pt)>> = BTreeMap::new();
    for l in &res.toolpaths.layers {
        for o in &l.objects {
            for p in o.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).filter(|p| keep(p.role)) {
                let at = out.entry(key(l.print_z)).or_default();
                at.extend(p.pts.first().map(|&q| (o.object, o.filament, q)));
                for w in p.pts.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    let n = step.map_or(1, |s| ((b.x - a.x) as f64).hypot((b.y - a.y) as f64).div_euclid(s * SCALE) as i64 + 1);
                    at.extend((1..=n).map(|k| (o.object, o.filament, Pt { x: a.x + (b.x - a.x) * k / n, y: a.y + (b.y - a.y) * k / n })));
                }
            }
        }
    }
    out
}

fn inside(set: &[ExPolygon], q: Pt) -> bool {
    set.iter().any(|e| e.contains(q))
}

fn model(r: Role) -> bool {
    !matches!(r, Role::Support | Role::SupportInterface | Role::SupportTransition | Role::Brim | Role::Skirt)
}

/// The bodies' own outlines per layer (sliced apart): where two or more overlap, less a band
/// along the union's edge, where the union's own walls run.
struct Overlap {
    /// Per layer: the union of the bodies, and its height.
    union: BTreeMap<i64, (Vec<ExPolygon>, f64)>,
    cores: BTreeMap<i64, Vec<ExPolygon>>,
    /// The overlap's volume and the bodies' (mm³).
    shared: f64,
    sum: f64,
}

fn overlap(apart: &SliceResult) -> Overlap {
    let bodies: Vec<BTreeMap<i64, (Vec<ExPolygon>, f64)>> = (0..3).map(|o| regions(apart, o)).collect();
    let (mut union, mut cores) = (BTreeMap::new(), BTreeMap::new());
    let (mut shared, mut sum) = (0.0, 0.0);
    let zs: BTreeSet<i64> = bodies.iter().flat_map(|b| b.keys().copied()).collect();
    for z in zs {
        let each: Vec<&Vec<ExPolygon>> = bodies.iter().filter_map(|b| b.get(&z).map(|(r, _)| r)).collect();
        let height = bodies.iter().filter_map(|b| b.get(&z).map(|(_, h)| *h)).fold(0.0, f64::max);
        let all = union_ex(&each.iter().flat_map(|r| r.iter().cloned()).collect::<Vec<_>>());
        let mut twice: Vec<ExPolygon> = Vec::new();
        for a in 0..each.len() {
            for b in a + 1..each.len() {
                twice.extend(intersection_ex(each[a], each[b]));
            }
        }
        let twice = union_ex(&twice);
        let s: f64 = each.iter().map(|r| total_area_mm2(r)).sum();
        sum += s * height;
        shared += (s - total_area_mm2(&all)) * height;
        let core = intersection_ex(&twice, &offset_mm(&all, -2.0));
        if !core.is_empty() {
            cores.insert(z, core);
        }
        union.insert(z, (all, height));
    }
    Overlap { union, cores, shared, sum }
}

/// Per overlap core layer, the (object, filament)s printing model paths across it, half a
/// millimetre in from its edges (where a path that stops at a body's outline ends).
fn printers(res: &SliceResult, cores: &BTreeMap<i64, Vec<ExPolygon>>) -> Vec<BTreeSet<(usize, usize)>> {
    let p = points(res, model, Some(0.5));
    cores.iter().map(|(z, c)| {
        let c = offset_mm(c, -0.5);
        p.get(z).map_or(BTreeSet::new(), |v| v.iter().filter(|(_, _, q)| inside(&c, *q)).map(|&(o, f, _)| (o, f)).collect())
    }).collect()
}

#[test]
fn a_head_and_its_sunk_locks_listed_as_one_object_print_as_one_body() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let source = script("");
    let (_, apart) = slice(&source, false, &[]);
    let (_, glued) = slice(&source, true, &[]);
    let o = overlap(&apart);
    let g = regions(&glued, 0);
    assert!(regions(&glued, 1).is_empty() && regions(&glued, 2).is_empty(), "the locks have no layers of their own when glued");
    let mut union = 0.0;
    for (z, (all, height)) in &o.union {
        let u = total_area_mm2(all);
        let (gr, _) = g.get(z).unwrap_or_else(|| panic!("the glued body has the layer at {z} µm"));
        assert!((total_area_mm2(gr) - u).abs() <= 0.01 * u + 0.05, "at {z} µm the glued layer is the union: {:.3} mm², the union {u:.3}", total_area_mm2(gr));
        union += u * height;
    }
    println!("[glue] overlap {:.1} mm³, union {union:.1}, the bodies {:.1}; {} layers with an overlap core", o.shared, o.sum, o.cores.len());
    assert!(o.shared > 100.0 && o.cores.len() > 10, "the locks are sunk into the head: {:.1} mm³, {} layers", o.shared, o.cores.len());
    // The volume: the union's, once.
    let (once, twice) = (makepad_fab_slice::glue::volumes(&glued.toolpaths), makepad_fab_slice::glue::volumes(&apart.toolpaths));
    println!("[glue] volume glued {once:.1?} mm³, as three bodies {twice:.1?}");
    assert!((once[0] - union).abs() <= 0.01 * union && once[1] == 0.0 && once[2] == 0.0, "the glued volume {once:.1?} mm³ is the union's {union:.1}");
    assert!((twice.iter().sum::<f64>() - o.sum).abs() <= 0.01 * o.sum, "as three bodies the overlap counts more than once: {twice:.1?} against {:.1}", o.sum);
    // No wall, and nothing printed twice, in the overlap's core.
    let walls = |r: Role| matches!(r, Role::OuterWall | Role::InnerWall | Role::OverhangWall);
    let walled = |res: &SliceResult| -> usize {
        let p = points(res, walls, None);
        o.cores.iter().map(|(z, c)| p.get(z).map_or(0, |v| v.iter().filter(|(_, _, q)| inside(c, *q)).count())).sum()
    };
    let doubled = |res: &SliceResult| printers(res, &o.cores).iter().filter(|by| by.iter().map(|&(obj, _)| obj).collect::<BTreeSet<_>>().len() > 1).count();
    let (walled_glued, walled_apart, doubled_glued, doubled_apart) = (walled(&glued), walled(&apart), doubled(&glued), doubled(&apart));
    println!("[glue] wall points in the overlap: glued {walled_glued}, apart {walled_apart}; layers printed twice there: glued {doubled_glued}, apart {doubled_apart}");
    assert!(walled_apart > 0 && doubled_apart > 0, "as three bodies the overlap has walls and prints twice");
    assert_eq!(walled_glued, 0, "glued, no wall runs inside the overlap");
    assert_eq!(doubled_glued, 0, "glued, the overlap prints once");
    // The supports: none inside the head, glued; as three bodies the locks' climb through it.
    let head = regions(&apart, 0);
    let supports = |r: Role| matches!(r, Role::Support | Role::SupportInterface | Role::SupportTransition);
    let through = |res: &SliceResult| -> (usize, usize) {
        let p = points(res, supports, None);
        let inner: usize = head.iter().map(|(z, (hr, _))| {
            let deep = offset_mm(hr, -0.5);
            p.get(z).map_or(0, |v| v.iter().filter(|(_, _, q)| inside(&deep, *q)).count())
        }).sum();
        (inner, p.values().map(Vec::len).sum())
    };
    let ((inner_glued, all_glued), (inner_apart, all_apart)) = (through(&glued), through(&apart));
    println!("[glue] support points inside the head: glued {inner_glued} of {all_glued}, apart {inner_apart} of {all_apart}");
    assert!(inner_apart > 0, "as three bodies the locks' supports climb through the head");
    assert_eq!(inner_glued, 0, "glued, no support stands inside the head");
    assert!(all_glued > 0, "glued, the locks' free undersides are still supported");
}

/// Brown locks on a head of filament 1: glued, the locks own all of themselves. Their surface
/// prints brown, and the overlap prints once, as the glued body: by regions (no paint by
/// position) all brown, and by position, when the head carries a red stroke, which still prints,
/// as the paint's inside prints (it shows nowhere). As three bodies both filaments print there.
#[test]
fn locks_of_another_filament_own_all_of_themselves() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let brown = script(BROWN);
    let by_regions: &[(&str, u32)] = &[("Brown", 2)];
    let (ev, apart) = slice(&brown, false, by_regions);
    assert_eq!(ev.plate.parts.iter().map(|p| p.filament).collect::<Vec<_>>(), vec![1, 2, 2], "the head in filament 1, the locks in 2");
    let o = overlap(&apart);
    assert!(o.cores.len() > 10, "{} layers with an overlap core", o.cores.len());
    let by_apart = printers(&apart, &o.cores);
    assert!(by_apart.iter().any(|by| by.iter().any(|&(_, f)| f == 0) && by.iter().any(|&(_, f)| f == 1)), "as three bodies the head's filament and brown both print in the overlap");
    // the locks out of the head, half a millimetre clear of it: their surface shows
    let head = regions(&apart, 0);
    let mut out_of_head: BTreeMap<i64, Vec<ExPolygon>> = BTreeMap::new();
    for lock in 1..3 {
        for (z, (r, _)) in regions(&apart, lock) {
            let clear = head.get(&z).map_or(r.clone(), |(h, _)| difference_ex(&r, &offset_mm(h, 0.5)));
            out_of_head.entry(z).or_default().extend(clear);
        }
    }
    let painted = script(&format!("{BROWN}{STROKE}"));
    let by_position: &[(&str, u32)] = &[("Red", 2), ("Brown", 3)];
    let (ev, _) = plate(&painted, true, by_position);
    assert!(ev.paint[0].is_some(), "the head is painted by position");
    // (how, the script, its tools, brown's filament (0-based), every filament that prints)
    let cases: [(&str, &String, &[(&str, u32)], usize, BTreeSet<usize>); 2] = [("by regions", &brown, by_regions, 1, [0, 1].into()), ("by position", &painted, by_position, 2, [0, 1, 2].into())];
    for (how, source, tools, brown, want) in cases {
        let (_, glued) = slice(source, true, tools);
        let by = printers(&glued, &o.cores);
        let fils: BTreeSet<usize> = by.iter().flatten().map(|&(_, f)| f).collect();
        let surface = points(&glued, |r| matches!(r, Role::OuterWall | Role::OverhangWall), Some(0.5));
        let shown: Vec<usize> = out_of_head.iter().flat_map(|(z, r)| surface.get(z).into_iter().flatten().filter(|(_, _, q)| inside(r, *q)).map(|&(_, f, _)| f)).collect();
        let shown_fils: BTreeSet<usize> = shown.iter().copied().collect();
        println!("[glue] {how}: filaments printing in the overlap {fils:?} over {} layers; on the locks' surface {shown_fils:?} at {} points", by.len(), shown.len());
        assert!(shown.len() > 100 && shown_fils == [brown].into(), "{how}: the locks' surface prints brown: {shown_fils:?} at {} points", shown.len());
        assert!(by.iter().all(|by| by.iter().all(|&(obj, _)| obj == 0)), "{how}: the overlap prints once, as the glued body: {by:?}");
        assert!(by.iter().filter(|by| !by.is_empty()).count() > 10, "{how}: the glued body fills the overlap");
        if how == "by regions" {
            assert_eq!(fils, [brown].into(), "{how}: only brown prints in the overlap");
        }
        let printed: BTreeSet<usize> = points(&glued, model, None).values().flatten().map(|&(_, f, _)| f).collect();
        assert_eq!(printed, want, "{how}: the head's filament, its paint and brown each print");
    }
}

/// The locks' undersides inside the head are no overhang of the glued body: no facet there
/// hints one, while as three bodies they do.
#[test]
fn no_facet_inside_the_head_hints_an_overhang() {
    let source = script("");
    let materials = fab_printability::Materials::default();
    let hints = |glued: bool| {
        let (ev, _) = plate(&source, glued, &[]);
        let report = ev.analyse(&materials);
        let head = ev.fields[0].as_ref().and_then(|f| f.on_plate()).expect("the head's field on the plate");
        let mut deep = 0;
        let mut area = 0.0;
        for lock in 1..3 {
            let m = &ev.plate.parts[lock].mesh;
            for (t, &d) in m.triangles.iter().zip(&report.overhang[lock]) {
                if d == 0 {
                    continue;
                }
                let c: [f64; 3] = std::array::from_fn(|k| t.iter().map(|&v| m.positions[v as usize][k]).sum::<f64>() / 3.0);
                deep += usize::from(head(c[0], c[1], c[2]) < -0.5);
            }
            area += report.issues.iter().filter(|i| i.kind == "overhang" && i.object.as_deref() == Some(ev.plate.parts[lock].object.as_str())).map(|i| i.area_mm2).sum::<f64>();
        }
        (deep, area)
    };
    let ((deep_glued, area_glued), (deep_apart, area_apart)) = (hints(true), hints(false));
    println!("[glue] overhang facets of the locks inside the head: glued {deep_glued}, apart {deep_apart}; the locks' overhang hints {area_glued:.1} mm², apart {area_apart:.1}");
    assert!(deep_apart > 0, "as three bodies the locks' undersides inside the head are overhangs");
    assert_eq!(deep_glued, 0, "glued, no facet inside the head hints an overhang");
    assert!(area_glued < area_apart, "glued, the locks hint less overhang");
}

/// A leg segment of the flex beetle, a pod 13 mm long domed like a pebble, and its TPU cord of the
/// beetle's house section flat on the bed along the segment's axis, from 4 mm past one end to 4 mm
/// past the other; `tail` places what it names.
fn leg(tail: &str) -> String {
    format!("use mod.model.*\nuse mod.fab.*\nuse mod.math.*\nlet m = mod.model\n\
let pebble = [[0, 1] [0.35, 0.98] [0.6, 0.92] [0.8, 0.8] [0.93, 0.6] [1, 0.36]]\n\
fn pod(f) {{\nPolygon{{points: for i in 0..26 {{\nlet t = if i < 13 {{ -1.5708 + i * 0.2618 }} else {{ 1.5708 + (i - 13) * 0.2618 }}\n\
[(if i < 13 {{ 3.7mm }} else {{ -3.7mm }}) + 2.8mm * f * cos(t), 2.8mm * f * sin(t)]\n}}}}\n}}\n\
let segment = m.loft({{sections: for q in pebble {{ Section{{sketch: pod(q[1]) at: 4.2mm * q[0]}} }}}})\n\
let cord = m.sweep({{sketch: Polygon{{points: [[0, -1.2mm] [1.008mm, -1.2mm] [2.448mm, -0.3mm] [2.448mm, 0.3mm] [1.008mm, 1.2mm] [0, 1.2mm]]}} path: [[-10.5mm, 0, 0] [10.5mm, 0, 0]]}})\n\
m.fill(cord, Material{{albedo: [0.75 0.42 0.2] name: \"Copper\"}})\n{tail}")
}

const LEG_PLATE: &str = "use mod.fab.*\nscript(\"leg.cad.splash\")\nplace(\"leg\", object: \"leg\", at: [175mm 160mm])\n";

/// The model regions per layer of the segment and of the cord, of those the leg's `tail` places.
fn leg_layers(tail: &str) -> [BTreeMap<i64, Vec<ExPolygon>>; 2] {
    let scripts = BTreeMap::from([("leg.cad.splash".to_string(), leg(tail))]);
    let model = FabModel::load("leg", LEG_PLATE, scripts).expect("the plate loads");
    let mut ev = model.evaluate(&mut Evaluator::new(Machine::dual_nozzle_350()), None).unwrap_or_else(|f| panic!("{f}")).1;
    let colors = ev.part_colors.clone();
    ev.plate.map_colors(&colors, &|name| Some((if name == "Copper" { 2 } else { 1 }, 0)));
    let res = slice_frames(&setup(&ev, 2), &ev.plate, &frames_of(&ev), &[], None).expect("slices");
    // the segment prints in filament 1, the copper cord in 2
    [1, 2].map(|f| ev.plate.parts.iter().position(|p| p.filament == f).map_or(BTreeMap::new(), |o| regions(&res, o).into_iter().map(|(z, (r, _))| (z, r)).collect()))
}

/// The space a leg segment of the flex beetle shares with the TPU cord bonded through it is the
/// cord's (the bond cuts the cord out of the segment): on every layer the cord prints all of
/// itself, one strip unbroken through the segment and out of both its ends, and the segment's
/// layers are its own less exactly the cord's cross-section, so the cord is anchored inside it and
/// nothing prints twice. Each alone, both fill that space.
#[test]
fn a_bonded_cord_runs_unbroken_through_its_segment() {
    let bonded = leg_layers("let leg = m.bond(cord, [segment])\nobject(segment, name: \"leg\")\n");
    // each alone, as it is
    let loose = [leg_layers("object(segment, name: \"leg\")\n")[0].clone(), leg_layers("object(cord, name: \"leg\")\n")[1].clone()];
    assert!(bonded.iter().chain(&loose).all(|l| !l.is_empty()), "the segment and the cord print, bonded and each alone");
    let area = |r: &[ExPolygon]| total_area_mm2(r);
    // what of `want` more than 10 µm in `got` misses, and what of `got` lies more than 10 µm out of
    // `want` (mm²): the cut and the cord are tessellated apart, a few µm off each other on a slope
    let within = |want: &[ExPolygon], got: &[ExPolygon]| (area(&difference_ex(&offset_mm(want, -0.01), got)), area(&difference_ex(got, &offset_mm(want, 0.01))));
    let mut through = 0;
    for (z, cord) in &loose[1] {
        let got = bonded[1].get(z).unwrap_or_else(|| panic!("the bonded cord prints at {z} µm"));
        let (lost, gained) = (area(&difference_ex(cord, got)), area(&difference_ex(got, cord)));
        assert!(lost < 1e-3 && gained < 1e-3, "the bonded cord prints all of itself at {z} µm: {lost:.4} mm² lost, {gained:.4} mm² more");
        let Some(body) = loose[0].get(z) else { continue };
        let shared = area(&intersection_ex(body, cord));
        if shared > 1.0 {
            through += 1;
        }
        // one strip, out of both the segment's ends
        let (b, c) = (bbox_of(body), bbox_of(got));
        assert!(got.len() == 1 && c.min.x < b.min.x && c.max.x > b.max.x, "at {z} µm the cord runs unbroken through the segment: {} pieces, x {:.2}..{:.2} past {:.2}..{:.2}", got.len(), c.min.x_mm(), c.max.x_mm(), b.min.x_mm(), b.max.x_mm());
        let want = difference_ex(body, cord);
        let left = bonded[0].get(z).map_or(&[][..], |r| &r[..]);
        let (missing, extra) = within(&want, left);
        assert!(missing < 1e-4 && extra < 1e-4 && area(&intersection_ex(left, got)) < 1e-3, "at {z} µm the bonded segment leaves out the cord's cross-section ({shared:.2} mm²), no more: {missing:.4} mm² missing, {extra:.4} mm² more");
    }
    assert!(through >= 8, "each as it is, both fill the space they share on {through} layers");
    // above the cord the segment is its own
    for (z, body) in loose[0].range(loose[1].keys().last().expect("the cord's layers") + 1..) {
        let got = bonded[0].get(z).unwrap_or_else(|| panic!("the bonded segment prints at {z} µm"));
        let (missing, extra) = within(body, got);
        assert!(missing < 1e-4 && extra < 1e-4, "above the cord, at {z} µm, the bonded segment is its own: {missing:.4} mm² missing, {extra:.4} mm² more");
    }
}

//! Package bodies generated from the footprint: each family turns a part's footprint data and
//! height ([`PackageBody`]) into its moulded body, leads and marks. Everything is in the
//! footprint's frame in millimetres, z up from the board surface, so the 3D view places the
//! result like any other body.
//!
//! Dimensions come only from the footprint and the part. What neither states (stand-off, mark
//! size) is a named constant per family, a typical value of the family's packages.

use crate::csg::{Csg, Prim};
use crate::generate::GenBody;
use crate::mesh::{BodyMesh, BODY_VERTEX_FLOATS};
use crate::scene::{PackageBody, PackagePad};
use fab_elec_core::{mm, to_mm, BBox, P};
use fab_elec_parts::PackageKind;
use fab_materials::part_look::PartClass;
use fab_materials::{Colour, MaterialId, Surface};

/// QFN: the moulded body's stand-off above the board, the terminals' A1 (0 to 0.05 mm).
pub const QFN_STANDOFF_MM: f64 = 0.02;
/// QFN: the terminals' length in from the outline (L, typically 0.3 to 0.5 mm).
pub const QFN_TERMINAL_LEN_MM: f64 = 0.40;
/// QFN: the terminals' thickness up from the board, a typical lead frame's 0.203 mm.
pub const QFN_TERMINAL_T_MM: f64 = 0.20;
/// QFN: the pin-1 dimple's radius as a share of the body's shorter side, at most
/// [`QFN_PIN1_MAX_R_MM`].
pub const QFN_PIN1_R_SHARE: f64 = 0.04;
pub const QFN_PIN1_MAX_R_MM: f64 = 0.25;
/// QFN: the dimple's centre sits this many radii in from both edges of the pin-1 corner.
pub const QFN_PIN1_INSET_RADII: f64 = 2.5;
/// QFN: how deep the dimple's concave cap sinks into the top at its centre.
pub const QFN_PIN1_DEPTH_MM: f64 = 0.05;

/// SOP: the moulded body's stand-off above the board (A1, typically 0.10 mm).
pub const SOP_STANDOFF_MM: f64 = 0.10;
/// SOP: the length of shoulder buried in the moulding (typically 0.10 mm).
pub const SOP_LEAD_IN_MM: f64 = 0.10;
/// SOP: the shoulder's reach beyond the outline (typically 0.20 mm).
pub const SOP_SHOULDER_MM: f64 = 0.20;
/// SOP: the lead frame's thickness (typically 0.15 mm).
pub const SOP_LEAD_T_MM: f64 = 0.15;
/// SOP: the lead's width along the side (typically 65% of the pad width).
pub const SOP_LEAD_WIDTH_SHARE: f64 = 0.65;
/// SOP: the shoulder's exit height above the stand-off (typically 45% of the moulding height).
pub const SOP_EXIT_SHARE: f64 = 0.45;
/// SOP: the copper left beyond the toe of the foot (typically 0.10 mm).
pub const SOP_TOE_MM: f64 = 0.10;
/// SOP: the foot's length along the pad (L, typically 0.60 mm).
pub const SOP_FOOT_MM: f64 = 0.60;

/// Chip: the end caps' length as a share of the body length, at most; the pads' overlap with
/// the body when shorter (0.25 mm on an 0402, 0.4 mm on an 0805).
pub const CHIP_CAP_SHARE: f64 = 0.25;
/// Chip: how far the end caps' plating stands proud of the block (typically 0.02 mm).
pub const CHIP_INSET_MM: f64 = 0.02;

/// Display: the metal back plate's thickness (typically 0.20 mm).
pub const DISPLAY_BACK_MM: f64 = 0.20;
/// Display: the bezel width on the sides away from the flex (typically 1.0 mm).
pub const DISPLAY_BEZEL_MM: f64 = 1.0;
/// Display: the bezel width on the flex side for the driver and fan-out (typically 3.0 mm).
pub const DISPLAY_CHIN_MM: f64 = 3.0;
/// Display: the active glass's recess below the top of the bezel (typically 0.01 mm).
pub const DISPLAY_ACTIVE_RECESS_MM: f64 = 0.01;
/// Display: the flex's thickness (typically 0.10 mm).
pub const DISPLAY_FLEX_T_MM: f64 = 0.10;
/// Display: the flex's margin beside and beyond the outermost contacts (typically 0.50 mm).
pub const DISPLAY_FLEX_MARGIN_MM: f64 = 0.50;
/// Display: the black bezel ink's sRGB colour (typically `[0.02, 0.02, 0.02]`).
pub const DISPLAY_BEZEL_SRGB: [f32; 3] = [0.02, 0.02, 0.02];
/// Display: the unlit active area's sRGB colour (typically `[0.05, 0.06, 0.08]`).
pub const DISPLAY_ACTIVE_SRGB: [f32; 3] = [0.05, 0.06, 0.08];

/// The package's bodies, the moulded body first.
pub fn package_bodies(p: &PackageBody) -> Result<Vec<GenBody>, String> {
    match p.kind {
        PackageKind::Qfn => qfn(p),
        PackageKind::Sop => sop(p),
        PackageKind::Chip => chip(p),
        PackageKind::Display => display(p),
    }
}

/// The package's bodies meshed for the 3D view (flat-shaded, hard edges), each with its surface.
/// Run off the UI thread, once per distinct package.
pub fn package_meshes(p: &PackageBody) -> Result<Vec<(BodyMesh, Surface)>, String> {
    package_bodies(p)?.into_iter().map(|body| {
        let tri = crate::lower::mesh(&body.csg).map_err(|e| format!("{}: {e}", body.name))?;
        let mut mesh = BodyMesh::default();
        mesh.vertices.reserve(tri.triangles.len() * 3 * BODY_VERTEX_FLOATS);
        for (t, tri_ix) in tri.triangles.iter().enumerate() {
            let [a, b, c] = tri_ix.map(|i| tri.vertices[i as usize]);
            let (u, v) = ([b.x - a.x, b.y - a.y, b.z - a.z], [c.x - a.x, c.y - a.y, c.z - a.z]);
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len == 0.0 { continue; }
            let has_corners = tri.corner_normals.len() == tri.triangles.len() * 3;
            for (k, q) in [a, b, c].into_iter().enumerate() {
                let n = if has_corners {
                    let m = tri.corner_normals[t * 3 + k];
                    [m.x, m.y, m.z]
                } else {
                    n.map(|x| x / len)
                };
                mesh.indices.push((mesh.vertices.len() / BODY_VERTEX_FLOATS) as u32);
                mesh.vertices.extend_from_slice(&[q.x as f32, q.y as f32, q.z as f32, 0.0, n[0] as f32, n[1] as f32, n[2] as f32, 0.0]);
            }
        }
        let mut surface = Surface::new(body.material);
        surface.finish = body.finish;
        surface.colour = body.colour;
        Ok((mesh, surface))
    }).collect()
}

fn mm2(p: P) -> [f64; 2] {
    [to_mm(p.x), to_mm(p.y)]
}

/// A body with its material's default finish and natural colour.
fn material_body(name: &str, csg: Csg, material: MaterialId) -> GenBody {
    let surface = Surface::new(material);
    GenBody { name: name.into(), csg, material, finish: surface.finish, colour: surface.colour }
}

/// The body corner nearest pad 1 as signs of x and y from the body's centre.
fn pin1_corner(centre: [f64; 2], pad: &PackagePad) -> [f64; 2] {
    let at = mm2(pad.at);
    [if at[0] < centre[0] { -1.0 } else { 1.0 }, if at[1] < centre[1] { -1.0 } else { 1.0 }]
}

/// The pin-1 dimple cut into a `w` x `d` moulding's top at `top`, in the corner nearest pad 1 (QFN's
/// size rules, shared by every moulded family).
fn pin1_dimple(csg: &mut Csg, p: &PackageBody, centre: [f64; 2], w: f64, d: f64, top: f64) {
    if let Some(pad) = p.pin1() {
        let r = (w.min(d) * QFN_PIN1_R_SHARE).min(QFN_PIN1_MAX_R_MM);
        let [sx, sy] = pin1_corner(centre, pad);
        let inset = r * QFN_PIN1_INSET_RADII;
        // A concave spherical cap, `r` across at the top and the depth deep at its centre: its
        // sloped floor shades apart from the flat top, as a moulded dimple does.
        let depth = QFN_PIN1_DEPTH_MM;
        let sphere_r = (r * r + depth * depth) / (2.0 * depth);
        let at = [centre[0] + sx * (w / 2.0 - inset), centre[1] + sy * (d / 2.0 - inset), top - depth + sphere_r];
        csg.sub(Prim::sphere("pin-1 mark", at, sphere_r));
    }
}

/// A QFN (or DFN) body: the moulded box over the body outline from the stand-off to the part's
/// height, with the pin-1 dimple in the corner nearest pad 1. Each edge pad has a metal
/// terminal set flush into the nearest side of the moulding.
fn qfn(p: &PackageBody) -> Result<Vec<GenBody>, String> {
    let ([x0, y0], [x1, y1]) = (mm2(p.body[0]), mm2(p.body[1]));
    let (w, d, top) = (x1 - x0, y1 - y0, to_mm(p.height));
    if w <= 0.0 || d <= 0.0 {
        return Err("qfn: the footprint has no body outline and no pads".into());
    }
    if top <= QFN_STANDOFF_MM + QFN_PIN1_DEPTH_MM {
        return Err(format!("qfn: a height of {top} mm leaves no body above the {QFN_STANDOFF_MM} mm stand-off"));
    }
    let centre = [(x0 + x1) / 2.0, (y0 + y1) / 2.0];
    let mut csg = Csg::default();
    csg.add(Prim::cuboid("moulded body", [centre[0], centre[1], (QFN_STANDOFF_MM + top) / 2.0], [w, d, top - QFN_STANDOFF_MM]));
    pin1_dimple(&mut csg, p, centre, w, d, top);
    let sides = [(0, x0, -1.0), (0, x1, 1.0), (1, y0, -1.0), (1, y1, 1.0)];
    let mut terminals = Csg::default();
    for pad in &p.pads {
        let (pad_min, pad_max) = (mm2(pad.min), mm2(pad.max));
        if (0..2).all(|i| pad_min[i] <= centre[i] && centre[i] <= pad_max[i]) {
            continue;
        }
        let at = mm2(pad.at);
        let (axis, edge, outward) = sides.into_iter().min_by(|&(a, x, _), &(b, y, _)| {
            (at[a] - x).abs().total_cmp(&(at[b] - y).abs())
        }).expect("the body has four sides");
        let along = 1 - axis;
        let start = pad_min[along].max([x0, y0][along]);
        let end = pad_max[along].min([x1, y1][along]);
        if start >= end {
            continue;
        }
        let terminal_box = |label: &str, overrun: f64| {
            let inner = edge - outward * QFN_TERMINAL_LEN_MM;
            let outer = edge + outward * overrun;
            let mut at = [0.0, 0.0, (QFN_TERMINAL_T_MM - overrun) / 2.0];
            let mut size = [0.0, 0.0, QFN_TERMINAL_T_MM + overrun];
            at[axis] = (outer + inner) / 2.0;
            size[axis] = (outer - inner).abs();
            at[along] = (start + end) / 2.0;
            size[along] = end - start;
            Prim::cuboid(label, at, size)
        };
        terminals.add(terminal_box("terminal", 0.0));
        // Extend the cut past the outline and below the board, keeping the inner and top faces.
        csg.sub(terminal_box("terminal cut", 0.01));
    }
    let epoxy = MaterialId::lookup("epoxy_mold").expect("the moulding compound is in the material table");
    let body = material_body("moulded body", csg, epoxy);
    if terminals.steps.is_empty() {
        return Ok(vec![body]);
    }
    let tin_lead = MaterialId::lookup("tin_lead").expect("the terminal metal is in the material table");
    Ok(vec![body, material_body("terminals", terminals, tin_lead)])
}

/// The nearest finite outline edge as its normal axis, coordinate and outward sign.
fn sop_side(at: [f64; 2], min: [f64; 2], max: [f64; 2]) -> (usize, f64, f64) {
    let sides = [(0, min[0], -1.0), (0, max[0], 1.0), (1, min[1], -1.0), (1, max[1], 1.0)];
    let distance_squared = |&(axis, edge, _): &(usize, f64, f64)| {
        let along = 1 - axis;
        let normal = at[axis] - edge;
        let tangent = at[along] - at[along].clamp(min[along], max[along]);
        normal * normal + tangent * tangent
    };
    // min_by keeps the first side on a tie: left, right, bottom, top.
    sides.into_iter().min_by(|a, b| distance_squared(a).total_cmp(&distance_squared(b)))
        .expect("the body has four sides")
}

/// A moulded small-outline body with a gull-wing lead from its side to each projecting pad.
fn sop(p: &PackageBody) -> Result<Vec<GenBody>, String> {
    let [min, max] = p.body.map(mm2);
    let (w, d, top) = (max[0] - min[0], max[1] - min[1], to_mm(p.height));
    if w <= 0.0 || d <= 0.0 {
        return Err("sop: the footprint has no body outline and no pads".into());
    }
    if top <= SOP_STANDOFF_MM + QFN_PIN1_DEPTH_MM {
        return Err(format!("sop: a height of {top} mm leaves no body above the {SOP_STANDOFF_MM} mm stand-off"));
    }
    let centre = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
    let mut csg = Csg::default();
    csg.add(Prim::cuboid("moulded body", [centre[0], centre[1], (SOP_STANDOFF_MM + top) / 2.0], [w, d, top - SOP_STANDOFF_MM]));
    pin1_dimple(&mut csg, p, centre, w, d, top);

    let t = SOP_LEAD_T_MM;
    let z_exit = SOP_STANDOFF_MM + (top - SOP_STANDOFF_MM) * SOP_EXIT_SHARE;
    let mut leads = Csg::default();
    for pad in &p.pads {
        let (pad_min, pad_max) = (mm2(pad.min), mm2(pad.max));
        if (0..2).all(|i| pad_min[i] <= centre[i] && centre[i] <= pad_max[i]) {
            continue;
        }
        let at = mm2(pad.at);
        let (axis, edge, outward) = sop_side(at, min, max);
        let outer_end = if outward > 0.0 { pad_max[axis] } else { pad_min[axis] };
        let out = outward * (outer_end - edge);
        if out <= SOP_SHOULDER_MM + t {
            continue;
        }
        let along = 1 - axis;
        let width = (pad_max[along] - pad_min[along]) * SOP_LEAD_WIDTH_SHARE;
        let point = |u: f64, across: f64, z: f64| {
            let mut q = [0.0, 0.0, z];
            q[axis] = edge + outward * u;
            q[along] = at[along] + across;
            q
        };
        let lead_box = |label: &str, u0: f64, u1: f64, z: f64| {
            let mut size = [0.0, 0.0, t];
            size[axis] = u1 - u0;
            size[along] = width;
            Prim::cuboid(label, point((u0 + u1) / 2.0, 0.0, z), size)
        };
        let f1 = out - SOP_TOE_MM;
        let f0 = SOP_SHOULDER_MM.max(f1 - SOP_FOOT_MM);
        leads.add(lead_box("shoulder", -SOP_LEAD_IN_MM, SOP_SHOULDER_MM, z_exit));
        leads.add(lead_box("foot", f0, f1, t / 2.0));
        // At either side of the lead, the inner and outer faces bend down together.
        let mut leg = Vec::with_capacity(8);
        for across in [-width / 2.0, width / 2.0] {
            leg.push(point(SOP_SHOULDER_MM - t, across, z_exit - t / 2.0));
            leg.push(point(SOP_SHOULDER_MM, across, z_exit + t / 2.0));
            leg.push(point(f0, across, 0.0));
            leg.push(point(f0 + t, across, t));
        }
        leads.add(Prim::hull("leg", leg));
    }
    let epoxy = MaterialId::lookup("epoxy_mold").expect("the moulding compound is in the material table");
    let body = material_body("moulded body", csg, epoxy);
    if leads.steps.is_empty() {
        return Ok(vec![body]);
    }
    let tin_lead = MaterialId::lookup("tin_lead").expect("the terminal metal is in the material table");
    Ok(vec![body, material_body("leads", leads, tin_lead)])
}

/// A two-terminal chip with an inset block and metal caps over the ends nearest its pads.
fn chip(p: &PackageBody) -> Result<Vec<GenBody>, String> {
    let [first, second] = p.pads.as_slice() else {
        let n = p.pads.len();
        return Err(format!("chip: a chip has two pads, found {n}"));
    };
    let a = if first.at.x.abs_diff(second.at.x) > first.at.y.abs_diff(second.at.y) { 0 } else { 1 };
    let b = 1 - a;
    let [min, max] = p.body.map(mm2);
    let length = max[a] - min[a];
    let top = to_mm(p.height);
    let inset = CHIP_INSET_MM;
    let pad_overlap = |pad: &PackagePad| mm2(pad.max)[a].min(max[a]) - mm2(pad.min)[a].max(min[a]);
    let overlap = pad_overlap(first).min(pad_overlap(second));
    let max_cap = CHIP_CAP_SHARE * length;
    let t = if overlap <= 0.0 { max_cap } else { overlap.min(max_cap) };
    if top <= 2.0 * inset {
        return Err(format!("chip: a height of {top} mm leaves no block after the {inset} mm inset"));
    }
    if length <= 2.0 * t {
        return Err(format!("chip: a length of {length} mm leaves no block between two {t} mm end caps"));
    }

    let cuboid = |label: &str, along: [f64; 2], across: [f64; 2], z: [f64; 2]| {
        let spans = if a == 0 { [along, across, z] } else { [across, along, z] };
        Prim::cuboid(label, spans.map(|[lo, hi]| (lo + hi) / 2.0), spans.map(|[lo, hi]| hi - lo))
    };
    let mut caps = Csg::default();
    for end in [[min[a], min[a] + t], [max[a] - t, max[a]]] {
        caps.add(cuboid("end cap", end, [min[b], max[b]], [0.0, top]));
    }
    let tin_lead = MaterialId::lookup("tin_lead").expect("the end cap metal is in the material table");
    let caps = material_body("end caps", caps, tin_lead);

    let mut block = Csg::default();
    block.add(cuboid("block", [min[a] + inset, max[a] - inset], [min[b] + inset, max[b] - inset], [inset, top - inset]));
    let material = match p.class {
        PartClass::Capacitor => MaterialId::lookup("ceramic_mlcc").expect("the capacitor ceramic is in the material table"),
        PartClass::Inductor => MaterialId::lookup("ferrite").expect("ferrite is in the material table"),
        _ => MaterialId::lookup("alumina").expect("alumina is in the material table"),
    };
    let block = material_body("block", block, material);

    if matches!(p.class, PartClass::Resistor | PartClass::Fuse) {
        let mut film = Csg::default();
        film.add(cuboid("film",
            [min[a] + t - inset, max[a] - t + inset],
            [min[b] + 2.0 * inset, max[b] - 2.0 * inset],
            [top - 2.0 * inset, top - inset / 2.0],
        ));
        let coating = MaterialId::lookup("thick_film_top").expect("the thick film is in the material table");
        Ok(vec![block, material_body("film", film, coating), caps])
    } else {
        Ok(vec![block, caps])
    }
}

struct DisplayLayout {
    window: ([f64; 2], [f64; 2]),
    flex: Option<([f64; 2], [f64; 2])>,
}

fn display_layout(p: &PackageBody) -> Result<DisplayLayout, &'static str> {
    let outline = BBox { min: p.body[0], max: p.body[1] };
    if outline.width() == 0 || outline.height() == 0 {
        return Err("display: the module box is empty");
    }
    let mut contacts = BBox::EMPTY;
    let mut sum = [0.0; 2];
    let mut count = 0usize;
    for pad in &p.pads {
        let bounds = BBox { min: pad.min, max: pad.max };
        if bounds.is_empty() || outline.intersects(&bounds) {
            continue;
        }
        contacts = contacts.union(bounds);
        let [x, y] = mm2(pad.at);
        sum[0] += x;
        sum[1] += y;
        count += 1;
    }

    let inset = outline.inflate(-mm(DISPLAY_BEZEL_MM));
    let [mut window_min, mut window_max] = [inset.min, inset.max].map(|q| [q.x, q.y]);
    let flex = if count == 0 {
        None
    } else {
        let centre = sum.map(|s| s / count as f64);
        let (axis, edge, outward) = sop_side(centre, mm2(outline.min), mm2(outline.max));
        let mut flex_min = mm2(contacts.min).map(|v| v - DISPLAY_FLEX_MARGIN_MM);
        let mut flex_max = mm2(contacts.max).map(|v| v + DISPLAY_FLEX_MARGIN_MM);
        let extra_chin = mm(DISPLAY_CHIN_MM - DISPLAY_BEZEL_MM);
        // The flex extends 1 mm under the module on the same side as the wider bezel.
        if outward < 0.0 {
            window_min[axis] = window_min[axis].saturating_add(extra_chin);
            flex_max[axis] = edge + 1.0;
        } else {
            window_max[axis] = window_max[axis].saturating_sub(extra_chin);
            flex_min[axis] = edge - 1.0;
        }
        Some((flex_min, flex_max))
    };
    if (0..2).any(|i| window_min[i] >= window_max[i]) {
        return Err("display: the bezel leaves no active area");
    }
    Ok(DisplayLayout { window: (window_min.map(to_mm), window_max.map(to_mm)), flex })
}

/// The active area of a display module (`PackageKind::Display`) in footprint millimetres,
/// min and max: the live picture draws here.
pub fn display_window(p: &PackageBody) -> Option<([f64; 2], [f64; 2])> {
    if p.kind != PackageKind::Display {
        return None;
    }
    display_layout(p).ok().map(|layout| layout.window)
}

fn display(p: &PackageBody) -> Result<Vec<GenBody>, String> {
    let layout = display_layout(p)?;
    let top = to_mm(p.height);
    if top <= DISPLAY_BACK_MM + DISPLAY_ACTIVE_RECESS_MM + 0.1 {
        return Err(format!("display: a height of {top} mm leaves too little glass above the back"));
    }
    let outline = (mm2(p.body[0]), mm2(p.body[1]));
    let cuboid = |name: &str, (min, max): ([f64; 2], [f64; 2]), z0: f64, z1: f64| {
        Prim::cuboid(name,
            [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0, (z0 + z1) / 2.0],
            [max[0] - min[0], max[1] - min[1], z1 - z0])
    };
    let glass = MaterialId::lookup("glass").expect("glass is in the material table");
    let mut cover = Csg::default();
    cover.add(cuboid("cover", outline, DISPLAY_BACK_MM, top));
    cover.sub(cuboid("window", layout.window, 0.0, top + DISPLAY_ACTIVE_RECESS_MM));
    let mut cover = material_body("cover", cover, glass);
    cover.colour = Colour::srgb(DISPLAY_BEZEL_SRGB);

    let mut active = Csg::default();
    active.add(cuboid("active area", layout.window, DISPLAY_BACK_MM, top - DISPLAY_ACTIVE_RECESS_MM));
    let mut active = material_body("active area", active, glass);
    active.colour = Colour::srgb(DISPLAY_ACTIVE_SRGB);

    let mut back = Csg::default();
    back.add(cuboid("back", outline, 0.0, DISPLAY_BACK_MM));
    let steel = MaterialId::lookup("steel_zinc").expect("the back plate metal is in the material table");
    let mut bodies = vec![cover, active, material_body("back", back, steel)];
    if let Some(bounds) = layout.flex {
        let mut flex = Csg::default();
        flex.add(cuboid("flex", bounds, 0.0, DISPLAY_FLEX_T_MM));
        let polyimide = MaterialId::lookup("polyimide").expect("polyimide is in the material table");
        bodies.push(material_body("flex", flex, polyimide));
    }
    Ok(bodies)
}

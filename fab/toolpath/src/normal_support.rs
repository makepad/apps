//! Normal supports: raster contacts, vertical columns and a separate Z stack.
//! R1 §5 / R5 §2, §5. Geometry stays on the slicer's integer grid.
//!
//! Inside the part: to a column the part is its outline with what that encloses (the tree
//! planner's rule), so none stands in the part's hollows, pores or lattice cells, and the overhang
//! over them is reported as inside. Not under an island with nothing below it: that one would
//! print on air.

use crate::{fill, flow, ExtrusionPath, Island, Params, Role};
use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex, union2_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, Join};

use crate::skeleton::{GivenUp, Skeleton};
use crate::support_paint::Masks;

/// How far past the side gap (mm) an enforced face's narrow strip reaches: room for a column.
const COLUMN_ROOM: f64 = 0.05;
/// Below this area (mm²) what a column lost holds no bead.
const SPECK: f64 = 0.05;

#[derive(Clone, Debug)]
pub struct NormalParams {
    pub threshold: f64,
    pub xy_gap: f64,
    pub first_xy_gap: f64,
    pub top_gap: f64,
    pub bottom_gap: f64,
    pub top_layers: usize,
    pub bottom_layers: usize,
    pub width: f64,
    pub expansion: f64,
    pub angle: f64,
    pub base_pattern: String,
    pub interface_pattern: String,
    pub bottom_spacing: f64,
    pub independent: bool,
    pub max_height: f64,
    pub grid: bool,
    pub bed_only: bool,
    pub first_expansion: f64,
    pub first_density: f64,
    /// Columns may stand inside the part's hollows and pores (by default none does).
    pub stand_inside: bool,
}

impl NormalParams {
    pub fn from_job(job: &Job, filament: usize, width: f64) -> Self {
        let top_layers = job.cfg.i64_at("support_interface_top_layers", 0).max(0) as usize;
        let bottom = job.cfg.i64_at("support_interface_bottom_layers", 0);
        let e = job.ext_of(filament);
        // An enabled tower setting does not create a tower for one material.
        let tower = job.b("enable_prime_tower") && job.filaments() > 1;
        Self {
            threshold: job.f("support_threshold_angle"),
            xy_gap: {
                let xy = job.f("support_object_xy_distance").max(0.0);
                if job.b("top_z_overrides_xy_distance") { xy.min(job.f("support_top_z_distance").max(0.2)) } else { xy }
            },
            first_xy_gap: job.f("support_object_first_layer_gap").max(0.0),
            top_gap: job.f("support_top_z_distance").max(0.0),
            bottom_gap: { let v = job.f("support_bottom_z_distance"); if v > 0.0 { v } else { job.f("support_top_z_distance").max(0.0) } },
            top_layers,
            bottom_layers: if bottom < 0 { top_layers } else { bottom as usize },
            width,
            expansion: job.f("support_expansion"),
            angle: job.f("support_angle").to_radians(),
            base_pattern: job.s("support_base_pattern").into(),
            interface_pattern: job.s("support_interface_pattern").into(),
            bottom_spacing: job.f("support_bottom_interface_spacing").max(0.0),
            independent: job.b("independent_support_layer_height") && !tower,
            max_height: { let h = job.ext("max_layer_height", e); if h > 0.0 { h } else { job.nozzle_diameter(e) * 0.75 } },
            grid: job.s("support_style") != "snug",
            bed_only: job.b("support_on_build_plate_only"),
            // The automatic foot in the reference extends about 2 mm
            // beyond the normal grid; an explicit expansion overrides it.
            first_expansion: { let v = job.f("raft_first_layer_expansion"); if v >= 0.0 { v } else { 2.0 } },
            first_density: (job.f("raft_first_layer_density") / 100.0).clamp(0.0, 1.0),
            stand_inside: job.b("fab_support_inside"),
        }
    }
}

#[derive(Default)]
pub struct NormalLayer {
    pub z: f64,
    pub height: f64,
    pub base: Vec<ExPolygon>,
    pub interface: Vec<ExPolygon>,
    pub bottom: Vec<ExPolygon>,
    pub transition: Vec<ExPolygon>,
}

struct Contact {
    z: f64,
    region: Vec<ExPolygon>,
    /// The model layer it holds up or stands on.
    model: usize,
}

fn rect(x0: i64, y0: i64, x1: i64, y1: i64) -> ExPolygon {
    ExPolygon::new(Polygon::new(vec![Pt::new(x0, y0), Pt::new(x1, y0), Pt::new(x1, y1), Pt::new(x0, y1)]))
}

/// Occupied cells, coalesced into row runs before the polygon union. Cost
/// follows the contact's bounding cells, not the bed or the object volume.
fn raster(region: &[ExPolygon], step: f64, angle: f64, center: Pt) -> Vec<ExPolygon> {
    if region.is_empty() { return Vec::new(); }
    let (sin, cos) = angle.sin_cos();
    let transform = |region: &[ExPolygon], inverse: bool| -> Vec<ExPolygon> {
        region.iter().map(|e| {
            let ring = |r: &Polygon| Polygon::new(r.pts.iter().map(|p| {
                if inverse { p.rotated(cos, sin).add(center) } else { p.sub(center).rotated(cos, -sin) }
            }).collect());
            ExPolygon { contour: ring(&e.contour), holes: e.holes.iter().map(ring).collect() }
        }).collect()
    };
    let local = transform(region, false);
    let s = scaled(step).max(1);
    let bb = bbox_of(&local);
    let mut rows = Vec::new();
    for y in bb.min.y.div_euclid(s)..=bb.max.y.div_euclid(s) {
        let band = intersection_ex(&local, &[rect(bb.min.x - 1, y * s, bb.max.x + 1, (y + 1) * s)]);
        if band.is_empty() { continue; }
        let b = bbox_of(&band);
        let mut start = None;
        let end = b.max.x.div_euclid(s);
        for x in b.min.x.div_euclid(s)..=end + 1 {
            let hit = x <= end && !intersection_ex(&band, &[rect(x * s, y * s, (x + 1) * s, (y + 1) * s)]).is_empty();
            if hit && start.is_none() { start = Some(x); }
            if !hit {
                if let Some(a) = start.take() { rows.push(rect(a * s, y * s, x * s, (y + 1) * s)); }
            }
        }
    }
    transform(&union_ex(&rows), true)
}

pub fn generate(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &Params, sp: &NormalParams, center: Pt, paint: &Masks, skeleton: &mut Skeleton) -> Result<Vec<NormalLayer>, crate::par::Cancelled> {
    let n = slices.len();
    if ctl.cancelled() { return Err(crate::par::Cancelled); }
    if n < 2 { return Ok(Vec::new()); }
    // The reference's normal grid uses gap + nominal bead spacing, also
    // when independent layers get thicker; default base lines keep one angle.
    let spacing = flow::spacing(sp.width, p.layer_height);
    let cell = p.support_base_spacing.max(0.0) + spacing;
    let project = |area: &[ExPolygon]| {
        let area = offset_ex(area, sp.expansion * SCALE, Join::Miter(3.0));
        if sp.grid { raster(&area, cell, sp.angle, center) } else { area }
    };
    let mut tops = Vec::new();
    for i in 1..n {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        let grow = if sp.threshold <= 0.0 { p.w_outer / 2.0 } else { heights[i].1 / sp.threshold.min(89.0).to_radians().tan() };
        // Open the printable slice, not the overhang strip: opening the
        // difference would erase a sustained slope's sub-width demand.
        let printable = offset_ex(&offset_ex(&slices[i], -sp.width / 2.0 * SCALE, Join::Miter(3.0)), sp.width / 2.0 * SCALE, Join::Miter(3.0));
        let mut overhang = difference_ex(&printable, &offset_ex(&slices[i - 1], grow * SCALE, Join::Miter(3.0)));
        let floating = crate::support::floating(&slices[i], &slices[i - 1]);
        if !floating.is_empty() {
            overhang.extend(floating);
            overhang = union_ex(&overhang);
        }
        if !paint.is_empty() {
            let asked = paint.layer(i, slices, overhang, sp.width, sp.xy_gap + spacing / 2.0 + COLUMN_ROOM);
            skeleton.blocked.extend(asked.blocked);
            overhang = if asked.thin.is_empty() { asked.demand } else { union_ex(&[asked.demand, asked.thin].concat()) };
        }
        if overhang.is_empty() { continue; }
        let z = heights[i].0 - heights[i].1 - sp.top_gap;
        if z < heights[0].0 - 1e-6 { continue; }
        let region = project(&overhang);
        if !region.is_empty() { tops.push(Contact { z, region, model: i }); }
    }
    if tops.is_empty() { return Ok(Vec::new()); }
    // Where what holds an overhang would ring the part, walls cut it into arcs that come off: the
    // columns keep off them like off the part. They reach past the foot's widening on the bed.
    let mut roofs: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
    for c in &tops {
        roofs[c.model].extend(c.region.iter().cloned());
    }
    let near = crate::par::try_map(ctl, n, |l| if roofs.get(l + 1).is_some_and(|r| !r.is_empty()) { offset_ex(&slices[l], sp.xy_gap * SCALE, Join::Miter(3.0)) } else { Vec::new() })?;
    let walls = crate::support::ring_walls(ctl, &roofs, &near, slices, 0, crate::support::WALL_WIDTHS * sp.width, sp.first_expansion + sp.width, 0.0)?;
    // The columns keep off what the part encloses as off the part, and stop on it: none stands in
    // its hollows or pores, unless it asks. Under what floats in there a column's room stays open.
    let enclosing = if sp.stand_inside { Vec::new() } else { crate::support::with_inside(ctl, slices, sp.expansion.max(0.0) + cell + sp.xy_gap + spacing)? };
    let solid: &[Vec<ExPolygon>] = if sp.stand_inside { slices } else { &enclosing };
    let encloses = solid.iter().zip(slices).any(|(a, b)| a != b);
    let last_z = tops.last().unwrap().z;
    let demand = union_ex(&tops.iter().flat_map(|c| c.region.iter().cloned()).collect::<Vec<_>>());
    let mut floors = Vec::new();
    if !sp.bed_only {
        for i in 0..n - 1 {
            if ctl.cancelled() { return Err(crate::par::Cancelled); }
            let z = heights[i].0 + sp.bottom_gap + heights[i].1;
            if z > last_z + 1e-6 { break; }
            let surface = difference_ex(&slices[i], &slices[i + 1]);
            let surface = intersection_ex(&surface, &demand);
            if !surface.is_empty() { floors.push(Contact { z, region: offset_ex(&surface, (sp.width - spacing / 2.0) * SCALE, Join::Square), model: i }); }
        }
    }
    let mut zs = vec![heights[0].0];
    if sp.independent {
        let mut fixed: Vec<f64> = tops.iter().chain(floors.iter()).map(|c| c.z).filter(|z| *z >= heights[0].0).collect();
        fixed.sort_by(|a, b| a.partial_cmp(b).unwrap());
        fixed.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        for z in fixed {
            if ctl.cancelled() { return Err(crate::par::Cancelled); }
            let prev = *zs.last().unwrap();
            let steps = ((z - prev) / sp.max_height - 1e-6).ceil().max(1.0) as usize;
            for k in 1..=steps { if z > prev + 1e-6 { zs.push(prev + (z - prev) * k as f64 / steps as f64); } }
        }
    } else {
        zs = heights.iter().map(|h| h.0).take_while(|z| *z <= last_z + 1e-6).collect();
    }
    let floor = if p.raft_layers > 0 { heights[0].0 - heights[0].1 } else { 0.0 };
    let mut layers: Vec<NormalLayer> = zs.iter().enumerate().map(|(i, &z)| NormalLayer { z, height: z - if i == 0 { floor } else { zs[i - 1] }, ..Default::default() }).collect();
    for contact in &tops {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        let end = zs.partition_point(|z| *z <= contact.z + 1e-6);
        for k in 0..sp.top_layers {
            if end > k { layers[end - 1 - k].interface.extend(contact.region.iter().cloned()); }
        }
        // Output evidence: a dense base-to-roof transition precedes the
        // requested top interface layers, with the same pitch and angle.
        if sp.top_layers > 0 && end > sp.top_layers {
            layers[end - 1 - sp.top_layers].transition.extend(contact.region.iter().cloned());
        }
    }
    for contact in &floors {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        let start = zs.partition_point(|z| *z < contact.z - 1e-6);
        // The bottom contact plus the requested floor layers carries the
        // sparse base above a model landing.
        if sp.bottom_layers > 0 {
            for layer in layers.iter_mut().skip(start).take(sp.bottom_layers + 1) {
                layer.bottom.extend(contact.region.iter().cloned());
            }
        }
    }
    let mut active = Vec::new();
    let mut ci = tops.len();
    let mut mi = n;
    // Per layer, the roof only what the part encloses took.
    let mut inside: Vec<Vec<ExPolygon>> = vec![Vec::new(); if encloses { layers.len() } else { 0 }];
    for i in (0..layers.len()).rev() {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        let layer = &mut layers[i];
        while mi > 0 && heights[mi - 1].0 > layer.z + 1e-6 {
            if ctl.cancelled() { return Err(crate::par::Cancelled); }
            mi -= 1;
            // Stop a column at a model landing, while retaining demand
            // introduced below it on subsequent descending layers.
            active = difference_ex(&active, &solid[mi]);
            // Nor does it go on through what is blocked.
            if !paint.block(mi).is_empty() {
                active = difference_ex(&active, paint.block(mi));
            }
        }
        while ci > 0 && tops[ci - 1].z >= layer.z - 1e-6 {
            if ctl.cancelled() { return Err(crate::par::Cancelled); }
            ci -= 1;
            active = union2_ex(&active, &tops[ci].region);
        }
        let mut obstacles = Vec::new();
        // Without what it encloses, where there is roof to tell.
        let mut part = Vec::new();
        let low = layer.z - layer.height - sp.bottom_gap;
        let high = layer.z + sp.top_gap;
        let begin = heights.partition_point(|h| h.0 < low + 1e-6);
        let mut end = begin;
        for j in begin..n {
            if heights[j].0 - heights[j].1 >= high - 1e-6 { break; }
            obstacles.extend(solid[j].iter().cloned());
            obstacles.extend(walls[j].iter().cloned());
            if encloses && !layer.interface.is_empty() {
                part.extend(slices[j].iter().cloned());
                part.extend(walls[j].iter().cloned());
            }
            end = j + 1;
        }
        if paint.blocks() {
            // A blocked surface is touched from just past the gaps, on either side.
            for j in begin.saturating_sub(1)..=end.min(n - 1) {
                obstacles.extend(paint.block(j).iter().cloned());
                if !part.is_empty() {
                    part.extend(paint.block(j).iter().cloned());
                }
            }
        }
        let first = p.first(i);
        let gap = if first { sp.first_xy_gap } else { sp.xy_gap };
        // Reference contact paths chamfer the clearance's convex corners
        // (the straight corner cuts are the square-offset join).
        let reach = (gap + if first { p.w_first / 2.0 } else { spacing / 2.0 }) * SCALE;
        let clearance = offset_ex(&union_ex(&obstacles), reach, Join::Square);
        if !part.is_empty() {
            let roof = intersection_ex(&union_ex(&layer.interface), &active);
            inside[i] = difference_ex(&intersection_ex(&roof, &clearance), &offset_ex(&union_ex(&part), reach, Join::Square));
        }
        let body = difference_ex(&active, &clearance);
        layer.interface = intersection_ex(&union_ex(&layer.interface), &body);
        layer.bottom = difference_ex(&intersection_ex(&union_ex(&layer.bottom), &body), &layer.interface);
        let interfaces = union2_ex(&layer.interface, &layer.bottom);
        layer.transition = difference_ex(&intersection_ex(&union_ex(&layer.transition), &body), &interfaces);
        let base_body = if first {
            // Grow the bed foot before trimming, so its expansion cannot
            // cross the model clearance or overlap a contact interface.
            difference_ex(&offset_ex(&active, sp.first_expansion * SCALE, Join::Square), &clearance)
        } else { body };
        layer.base = difference_ex(&difference_ex(&base_body, &interfaces), &layer.transition);
    }
    // What the part encloses took, told per contact where its roof would have been.
    if encloses {
        for contact in &tops {
            let end = zs.partition_point(|z| *z <= contact.z + 1e-6);
            let Some(took) = end.checked_sub(1).map(|k| &inside[k]).filter(|g| !g.is_empty()) else { continue };
            let left: Vec<ExPolygon> = intersection_ex(&contact.region, took).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
            skeleton.inside.extend(GivenUp::of(contact.model, &left));
        }
    }
    // What stands: from the bed only, every column reaches the bed. Under what is blocked, or on
    // what the part encloses, a column may have stopped in the air: it reaches the bed or the
    // model, or it goes.
    if sp.bed_only || paint.blocks() || encloses {
        // Only what the part encloses stops a column short: what goes is that rule's to tell.
        let enclosed_only = encloses && !sp.bed_only && !paint.blocks();
        let mut reachable = Vec::new();
        // Per layer, what went: said per contact below, under a paint that may have asked for it,
        // or as inside.
        let mut lost: Vec<Vec<ExPolygon>> = vec![Vec::new(); if paint.is_empty() && !enclosed_only { 0 } else { layers.len() }];
        for i in 0..layers.len() {
            if ctl.cancelled() { return Err(crate::par::Cancelled); }
            let layer = &mut layers[i];
            let body = union_ex(&[layer.base.clone(), layer.interface.clone(), layer.transition.clone(), layer.bottom.clone()].concat());
            if reachable.is_empty() && layer.z == heights[0].0 { reachable = body.clone(); }
            if !sp.bed_only {
                // The model right under it holds it up, and a face there the band over the gap at its
                // edge. A wall beside it holds nothing.
                let low = layer.z - layer.height - sp.bottom_gap;
                let under = heights.partition_point(|h| h.0 < low + 1e-6);
                if under > 0 {
                    let face = slices.get(under).map_or_else(|| slices[under - 1].clone(), |over| difference_ex(&slices[under - 1], over));
                    let band = offset_ex(&face, (sp.xy_gap + sp.width) * SCALE, Join::Miter(3.0));
                    reachable = union_ex(&[reachable, slices[under - 1].clone(), band].concat());
                }
            }
            layer.base = intersection_ex(&layer.base, &reachable);
            layer.interface = intersection_ex(&layer.interface, &reachable);
            layer.transition = intersection_ex(&layer.transition, &reachable);
            layer.bottom = intersection_ex(&layer.bottom, &reachable);
            if let Some(l) = lost.get_mut(i) {
                *l = difference_ex(&body, &reachable);
            }
            reachable = intersection_ex(&body, &reachable);
        }
        if !lost.is_empty() {
            for contact in &tops {
                let end = zs.partition_point(|z| *z <= contact.z + 1e-6);
                let Some(gone) = end.checked_sub(1).map(|k| &lost[k]).filter(|g| !g.is_empty()) else { continue };
                let left: Vec<ExPolygon> = intersection_ex(&contact.region, gone).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
                if enclosed_only {
                    skeleton.inside.extend(GivenUp::of(contact.model, &left));
                } else {
                    skeleton.given_up.extend(GivenUp::of(contact.model, &left));
                }
            }
        }
    }
    Ok(layers)
}

pub fn paths(p: &Params, sp: &NormalParams, layer: &NormalLayer, li: usize, center: Pt) -> Island {
    let first = p.first(li);
    let h = layer.height;
    let nominal = flow::spacing(sp.width, p.layer_height);
    let mut island = Island::default();
    for (role, original, gap) in [
        (Role::Support, &layer.base, p.support_base_spacing),
        (Role::SupportTransition, &layer.transition, p.support_interface_spacing),
        (Role::SupportInterface, &layer.interface, p.support_interface_spacing),
        (Role::SupportInterface, &layer.bottom, sp.bottom_spacing),
    ] {
        if original.is_empty() { continue; }
        let base = role == Role::Support;
        let width = if first && base { p.w_first } else { sp.width };
        let area = flow::cross_section(width, h);
        let mut speed = if first { p.speeds.first_layer } else if base { p.support_speed } else { p.support_interface_speed };
        if p.speeds.max_volumetric > 0.0 { speed = speed.min(p.speeds.max_volumetric / area); }
        let mk = |pts: Vec<Pt>, closed: bool| ExtrusionPath { role, loop_pts: if closed { pts.clone() } else { Vec::new() }, pts, closed, width, height: h, area, speed, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 };
        let mut region = original.clone();
        let mut spacing = nominal + gap.max(0.0);
        let mut angle = sp.angle + if base { 0.0 } else { std::f64::consts::FRAC_PI_2 };
        if base && first {
            for e in &region {
                for ring in e.rings() {
                    let mut pts = ring.pts.clone();
                    pts.push(pts[0]);
                    island.fills.push(mk(pts, true));
                }
            }
            region = offset_ex(&region, -flow::spacing(width, h) * SCALE, Join::Miter(3.0));
            if sp.first_density <= 0.0 { continue; }
            spacing = flow::spacing(width, h) / sp.first_density;
            angle += std::f64::consts::FRAC_PI_2;
        }
        // A floor follows the model landing, without raster expansion.
        // Narrow floor strips in the reference use a boundary loop when
        // the regular interface lattice misses the strip.
        if std::ptr::eq(original, &layer.bottom) {
            let mut wide = Vec::new();
            for e in region {
                let (lo, hi) = fill::extent(std::slice::from_ref(&e), angle);
                if unscaled(hi - lo) < spacing && fill::family(std::slice::from_ref(&e), angle, spacing, center, fill::Align::Grid).is_empty() {
                    for r in e.rings() {
                        let mut pts = r.pts.clone();
                        pts.push(pts[0]);
                        island.fills.push(mk(pts, true));
                    }
                } else { wide.push(e); }
            }
            region = wide;
        }
        if !base && sp.interface_pattern == "concentric" {
            let mut rings = region;
            while !rings.is_empty() {
                for e in &rings { for r in e.rings() { let mut pts = r.pts.clone(); pts.push(pts[0]); island.fills.push(mk(pts, true)); } }
                rings = offset_ex(&rings, -spacing * SCALE, Join::Miter(3.0));
            }
            continue;
        }
        if !base && sp.interface_pattern == "rectilinear_interlaced" && li % 2 == 1 { angle += std::f64::consts::FRAC_PI_2; }
        if base && !first && sp.base_pattern == "hollow" {
            for e in &region {
                for r in e.rings() {
                    let mut pts = r.pts.clone();
                    pts.push(pts[0]);
                    island.fills.push(mk(pts, true));
                }
            }
            continue;
        }
        let angles = if base && !first && sp.base_pattern == "rectilinear-grid" || !base && sp.interface_pattern == "grid" { vec![angle, angle + std::f64::consts::FRAC_PI_2] } else { vec![angle] };
        for angle in angles {
            // A tiny outward offset includes scanlines on a raster border.
            let region = offset_ex(&region, 10.0, Join::Miter(3.0));
            let mut segs = fill::family(&region, angle, spacing, center, fill::Align::Grid);
            if role == Role::SupportTransition {
                let (sin, cos) = angle.sin_cos();
                let origin = -sin * center.x_mm() + cos * center.y_mm();
                for e in &region {
                    let (lo, hi) = fill::extent(std::slice::from_ref(e), angle);
                    let (lo, hi) = (unscaled(lo), unscaled(hi));
                    let first = origin + ((lo - origin) / spacing).ceil() * spacing;
                    let last = origin + ((hi - origin) / spacing).floor() * spacing;
                    for (at, gap) in [(lo + 0.00001, first - lo), (hi - 0.00001, hi - last)] {
                        if gap > spacing / 2.0 {
                            let pivot = Pt::from_mm(center.x_mm() - sin * (at - origin), center.y_mm() + cos * (at - origin));
                            segs.extend(fill::family(&region, angle, hi - lo + 2.0 * spacing, pivot, fill::Align::Grid).into_iter().filter(|s| e.contains(lerp(s.a, s.b, 0.5))));
                        }
                    }
                }
            }
            segs.sort_by_key(|s| (s.a, s.b));
            segs.dedup_by_key(|s| (s.a, s.b));
            let anchor = if base && !first || role == Role::SupportTransition { spacing - nominal / 2.0 } else { 0.0 };
            // Dense interfaces also join split lines around a model edge;
            // a spacing-sized connection limit would leave that edge bare.
            let connect = if role == Role::SupportInterface { f64::INFINITY } else { spacing * 2.5 };
            for mut pl in fill::chain(&region, &segs, connect, anchor) {
                if pl.pts.len() < 2 { continue; }
                if role == Role::SupportInterface {
                    // The reference's free interface ends run on by 0.45
                    // nominal spacings, including on thicker support layers.
                    let extend = |a: Pt, b: Pt| {
                        let d = a.dist(b);
                        if d > 0.0 { lerp(b, a, 1.0 + 0.45 * nominal * SCALE / d) } else { a }
                    };
                    let last = pl.pts.len() - 1;
                    pl.pts[0] = extend(pl.pts[0], pl.pts[1]);
                    pl.pts[last] = extend(pl.pts[last], pl.pts[last - 1]);
                }
                island.fills.push(mk(pl.pts, false));
            }
        }
    }
    island
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support_paint::box_region;
    use makepad_fab_slicer::source::SupportPaint;

    const H: f64 = 0.2;
    const LAYERS: usize = 70;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    }

    fn heights(n: usize) -> Vec<(f64, f64)> {
        (0..n).map(|l| ((l + 1) as f64 * H, H)).collect()
    }

    /// A stem carrying a wide cap: columns all round under the cap.
    fn cap() -> Vec<Vec<ExPolygon>> {
        (0..LAYERS + 10).map(|l| vec![if l < LAYERS { rect(47.0, 47.0, 53.0, 53.0) } else { rect(37.0, 37.0, 63.0, 63.0) }]).collect()
    }

    /// A face 30 degrees off vertical: the threshold leaves it alone.
    fn slope(l: usize) -> ExPolygon {
        rect(40.0, 40.0, 60.0 + 0.115 * l as f64, 60.0)
    }

    fn plan(slices: &[Vec<ExPolygon>], block: Option<[[f64; 3]; 2]>, enforce: Option<[[f64; 3]; 2]>, bed_only: bool) -> (Vec<NormalLayer>, Skeleton) {
        plan_with(slices, block, enforce, bed_only, false)
    }

    fn plan_with(slices: &[Vec<ExPolygon>], block: Option<[[f64; 3]; 2]>, enforce: Option<[[f64; 3]; 2]>, bed_only: bool, stand_inside: bool) -> (Vec<NormalLayer>, Skeleton) {
        let heights = heights(slices.len());
        let mut p = Params::from_job(&crate::plate_tests::job("classic"), 0);
        p.support_base_spacing = 2.5;
        let sp = NormalParams { threshold: 30.0, xy_gap: 0.35, first_xy_gap: 0.2, top_gap: 0.2, bottom_gap: 0.2, top_layers: 2, bottom_layers: 2, width: 0.4, expansion: 0.0, angle: 0.0, base_pattern: "rectilinear".into(), interface_pattern: "rectilinear".into(), bottom_spacing: 0.5, independent: false, max_height: 0.3, grid: true, bed_only, first_expansion: 2.0, first_density: 0.9, stand_inside };
        let zs: Vec<f64> = heights.iter().map(|&(z, h)| z - h / 2.0).collect();
        let region = |b: Option<[[f64; 3]; 2]>| b.map(|b| box_region(b[0], b[1]));
        let paint = Some(SupportPaint { block: region(block), enforce: region(enforce), no_trees: None });
        let masks = Masks::of(crate::par::Ctl::default(), paint, &zs, ([0.0; 3], [150.0, 150.0, 40.0]), crate::support_paint::round_the_part(([0.0; 3], [150.0, 150.0, 40.0]))).unwrap();
        let mut skeleton = Skeleton::default();
        let layers = generate(crate::par::Ctl::default(), slices, &heights, &p, &sp, Pt::from_mm(50.0, 50.0), &masks, &mut skeleton).unwrap();
        (layers, skeleton)
    }

    fn inside(layer: &NormalLayer, r: &ExPolygon) -> f64 {
        total_area_mm2(&intersection_ex(&[layer.base.clone(), layer.interface.clone(), layer.bottom.clone(), layer.transition.clone()].concat(), std::slice::from_ref(r)))
    }

    /// A block at the ceiling over half of a cap: that half's contact is gone and nothing touches
    /// the block, the other half keeps its columns, and what it took is told. Without paint
    /// nothing is told.
    #[test]
    fn a_block_takes_exactly_that_support_and_says_so() {
        let slices = cap();
        let (free, plain) = plan(&slices, None, None, false);
        assert!(plain.blocked.is_empty() && plain.given_up.is_empty());
        let (blocked, kept) = (rect(50.0 - 0.3, 30.0, 70.0, 70.0), rect(30.0, 30.0, 47.0, 70.0));
        let contact = LAYERS - 2;
        assert!(inside(&free[contact], &blocked) > 100.0, "the block's half has contact without it: {}", inside(&free[contact], &blocked));
        let (layers, skeleton) = plan(&slices, Some([[50.0, 30.0, 13.6], [70.0, 70.0, 14.4]]), None, false);
        for l in LAYERS - 4..layers.len() {
            assert_eq!(inside(&layers[l], &blocked), 0.0, "layer {l} touches the block");
        }
        assert!((inside(&layers[contact], &kept) - inside(&free[contact], &kept)).abs() < 0.02 * inside(&free[contact], &kept), "the other half: {} against {}", inside(&layers[contact], &kept), inside(&free[contact], &kept));
        let took: f64 = skeleton.blocked.iter().map(|g| g.area).sum();
        assert!((300.0..330.0).contains(&took), "the half of the cap's overhang is told: {took} mm2");
    }

    /// Columns standing on the part stay when a block elsewhere switches on the check of what stands.
    #[test]
    fn a_block_elsewhere_leaves_columns_on_the_part() {
        let slices: Vec<Vec<ExPolygon>> = (0..LAYERS + 10).map(|l| vec![if l < 20 || l >= LAYERS { rect(37.0, 37.0, 63.0, 63.0) } else { rect(47.0, 47.0, 53.0, 53.0) }]).collect();
        let round = rect(30.0, 30.0, 70.0, 70.0);
        let (free, _) = plan(&slices, None, None, false);
        assert!(inside(&free[40], &round) > 50.0, "without it columns stand on the plinth: {}", inside(&free[40], &round));
        let (layers, skeleton) = plan(&slices, Some([[130.0, 130.0, 0.0], [140.0, 140.0, 20.0]]), None, false);
        assert!(skeleton.blocked.is_empty() && skeleton.given_up.is_empty(), "{:?}", skeleton.given_up);
        for l in 0..layers.len() {
            assert!((inside(&layers[l], &round) - inside(&free[l], &round)).abs() < 1e-6, "layer {l} moved");
        }
    }

    /// A slab across the columns' way down: nothing stands in it, below it nothing starts, and
    /// what stood above it on nothing is gone and told.
    #[test]
    fn a_column_cut_by_a_block_goes_and_is_told() {
        let slices = cap();
        let (layers, skeleton) = plan(&slices, Some([[50.0, 30.0, 5.0], [70.0, 70.0, 6.0]]), None, false);
        let side = rect(52.0, 30.0, 70.0, 70.0);
        for l in [5, 25, 28, 40, 60, LAYERS - 2] {
            assert_eq!(inside(&layers[l], &side), 0.0, "layer {l}: the column cut by the block");
        }
        assert!(inside(&layers[40], &rect(30.0, 30.0, 47.0, 70.0)) > 100.0, "the other side stands");
        let lost: f64 = skeleton.given_up.iter().map(|g| g.area).sum();
        assert!(lost > 200.0, "what it cut off is told: {lost} mm2");
    }

    /// A face too steep for the angle asks for nothing. Enforced, it gets columns there, down to
    /// the bed, and nowhere else.
    #[test]
    fn enforce_adds_columns_under_a_face_the_angle_leaves_alone() {
        let slices: Vec<Vec<ExPolygon>> = (0..120).map(|l| vec![slope(l)]).collect();
        assert!(plan(&slices, None, None, false).0.is_empty(), "the angle leaves this face alone");
        let (layers, skeleton) = plan(&slices, None, Some([[50.0, 46.0, 8.0], [90.0, 54.0, 30.0]]), false);
        assert!(skeleton.given_up.is_empty());
        let under = rect(58.0, 41.0, 90.0, 59.0);
        assert!(inside(&layers[20], &under) > 10.0 && !layers[0].base.is_empty(), "columns to the bed: {}", inside(&layers[20], &under));
        let all = total_area_mm2(&layers[20].base) + total_area_mm2(&layers[20].transition);
        assert!(all - inside(&layers[20], &under) < 1e-6, "{all} mm2 of which only {} under the face", inside(&layers[20], &under));
        // The contact comes up to the face, at each height where it is enforced.
        for l in [50, 80, 110] {
            let at = rect(60.0 + 0.115 * l as f64, 46.0, 90.0, 54.0);
            assert!(inside(&layers[l - 2], &at) > 0.1, "layer {l}: nothing right under the face");
        }
    }

    /// From the bed only, over a plinth: no column reaches the bed, and an enforced face says so.
    /// Allowed on the part, the columns stand on the plinth.
    #[test]
    fn enforce_out_of_reach_is_told() {
        let slices: Vec<Vec<ExPolygon>> = (0..80).map(|l| vec![if l < 10 { rect(-100.0, -100.0, 200.0, 200.0) } else { slope(l) }]).collect();
        let enforce = Some([[50.0, 46.0, 8.0], [90.0, 54.0, 30.0]]);
        let (layers, skeleton) = plan(&slices, None, enforce, true);
        assert!(layers.iter().all(|l| l.base.is_empty() && l.interface.is_empty()), "nothing reaches the bed");
        assert!(skeleton.given_up.iter().map(|g| g.area).sum::<f64>() > 1.0, "and it is told");
        let (layers, skeleton) = plan(&slices, None, enforce, false);
        assert!(skeleton.given_up.is_empty() && inside(&layers[30], &rect(58.0, 41.0, 90.0, 59.0)) > 10.0, "on the plinth it stands");
    }

    /// Paint that holds where nothing is asked moves no column.
    #[test]
    fn paint_that_misses_moves_nothing() {
        let slices = cap();
        let (free, _) = plan(&slices, None, None, false);
        let away = Some([[130.0, 130.0, 0.0], [140.0, 140.0, 20.0]]);
        let (layers, skeleton) = plan(&slices, away, away, false);
        assert!(skeleton.blocked.is_empty() && skeleton.given_up.is_empty());
        assert_eq!(layers.len(), free.len());
        for (l, (a, b)) in layers.iter().zip(&free).enumerate() {
            assert!(a.base == b.base && a.interface == b.interface && a.bottom == b.bottom && a.transition == b.transition, "layer {l} moved");
        }
    }
    /// A spool: a cap on a stem off a wider foot. What holds the cap stands on the foot and would
    /// ring the stem; it stands in arcs instead, on every layer, and the cap stays held.
    #[test]
    fn columns_round_a_stem_stand_in_arcs() {
        let slices: Vec<Vec<ExPolygon>> = (0..60).map(|l| vec![if l < 20 { rect(38.0, 38.0, 62.0, 62.0) } else if l < 50 { rect(47.0, 47.0, 53.0, 53.0) } else { rect(36.0, 36.0, 64.0, 64.0) }]).collect();
        let (layers, _) = plan(&slices, None, None, false);
        let stem = Pt::from_mm(50.0, 50.0);
        let mut roofed = 0.0f64;
        for (l, layer) in layers.iter().enumerate() {
            let body = union_ex(&[layer.base.clone(), layer.interface.clone(), layer.bottom.clone(), layer.transition.clone()].concat());
            assert!(body.iter().all(|e| !ExPolygon::new(e.contour.clone()).contains(stem)), "layer {l} rings the stem");
            let roof = union_ex(&layer.interface);
            if !roof.is_empty() && layer.z > 9.0 {
                assert!(roof.len() >= 3, "layer {l}: the roof is in arcs, {} of them", roof.len());
                roofed = roofed.max(total_area_mm2(&roof));
            }
        }
        assert!(roofed > 500.0, "the cap stays held: {roofed} mm2 of roof");
    }

    /// How far round `c` (degrees, to 5) `e` reaches, sampled from 4 to 12 mm out.
    fn wraps(e: &ExPolygon, c: Pt) -> f64 {
        let hit = (0..72).filter(|&k| {
            let a = (k as f64 * 5.0).to_radians();
            (8..=24).any(|r| e.contains(Pt::from_mm(c.x_mm() + r as f64 * 0.5 * a.cos(), c.y_mm() + r as f64 * 0.5 * a.sin())))
        }).count();
        hit as f64 * 5.0
    }

    /// The spool again, its stem joined by a rib out past the cap's edge: what holds the cap would
    /// close round the stem but for the rib's gap, and still can't come off it. It stands in arcs.
    #[test]
    fn columns_round_a_stem_with_a_rib_stand_in_arcs() {
        let slices: Vec<Vec<ExPolygon>> = (0..60).map(|l| if l < 20 { vec![rect(38.0, 38.0, 62.0, 62.0)] } else if l < 50 { union_ex(&[rect(47.0, 47.0, 53.0, 53.0), rect(53.0, 49.0, 70.0, 51.0)]) } else { vec![rect(36.0, 36.0, 64.0, 64.0)] }).collect();
        let (layers, _) = plan(&slices, None, None, false);
        let stem = Pt::from_mm(50.0, 50.0);
        let mut roofed = 0.0f64;
        for (l, layer) in layers.iter().enumerate().filter(|(_, layer)| layer.z > 4.2 && layer.z < 9.8) {
            let body = union_ex(&[layer.base.clone(), layer.interface.clone(), layer.bottom.clone(), layer.transition.clone()].concat());
            for e in &body {
                assert!(wraps(e, stem) <= 200.0, "layer {l}: a piece goes {} degrees round the stem", wraps(e, stem));
            }
            roofed = roofed.max(total_area_mm2(&layer.interface));
        }
        assert!(roofed > 400.0, "the cap stays held: {roofed} mm2 of roof");
    }

    /// What stands in `region` (mm³), every layer.
    fn stands_in(layers: &[NormalLayer], region: &[ExPolygon]) -> f64 {
        layers.iter().map(|l| total_area_mm2(&intersection_ex(&[l.base.clone(), l.interface.clone(), l.bottom.clone(), l.transition.clone()].concat(), region)) * l.height).sum()
    }

    fn given_inside(skeleton: &Skeleton) -> f64 {
        skeleton.inside.iter().map(|g| g.area).sum()
    }

    /// Every column stands: each island of a layer rests on the layer below's, or on the part
    /// under its bottom gap, or on the bed.
    fn nothing_hangs(layers: &[NormalLayer], slices: &[Vec<ExPolygon>]) {
        let body = |l: &NormalLayer| union_ex(&[l.base.clone(), l.interface.clone(), l.bottom.clone(), l.transition.clone()].concat());
        for i in 1..layers.len() {
            let below = body(&layers[i - 1]);
            let under = ((layers[i].z - layers[i].height - 0.2) / H).round() as usize;
            let part = offset_ex(&slices[under.saturating_sub(1)], 0.8 * SCALE, Join::Miter(3.0));
            for e in body(&layers[i]) {
                let held = total_area_mm2(&intersection_ex(std::slice::from_ref(&e), &below)) + total_area_mm2(&intersection_ex(std::slice::from_ref(&e), &part));
                assert!(held > 1e-6, "layer {i}: a column of {} mm2 stands on air", e.area_mm2());
            }
        }
    }

    /// A cup with a shelf inside it and a lid past its walls: the shelf's and the lid's overhang
    /// over the hollow get no columns, the lid's brim past the walls does.
    #[test]
    fn no_column_stands_in_a_cup() {
        let (lo, hi) = (30.0, 70.0);
        let wall = |extra: Vec<ExPolygon>| {
            let ring = difference_ex(&[rect(lo, lo, hi, hi)], &[rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)]);
            union_ex(&[ring, extra].concat())
        };
        let slices: Vec<Vec<ExPolygon>> = (0..90).map(|l| {
            if l < 6 {
                vec![rect(lo, lo, hi, hi)]
            } else if l < 50 {
                wall(Vec::new())
            } else if l < 60 {
                wall(vec![rect(lo + 2.0, lo + 2.0, lo + 18.0, hi - 2.0)])
            } else if l < 75 {
                wall(Vec::new())
            } else {
                wall(vec![rect(lo - 12.0, lo - 12.0, hi + 12.0, hi + 12.0)])
            }
        }).collect();
        let hollow = [rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)];
        let (free, _) = plan_with(&slices, None, None, false, true);
        assert!(stands_in(&free, &hollow) > 100.0, "asked to, it stands in the hollow: {} mm3", stands_in(&free, &hollow));
        let (layers, skeleton) = plan(&slices, None, None, false);
        assert!(stands_in(&layers, &hollow) < 1e-6, "{} mm3 stands in the hollow", stands_in(&layers, &hollow));
        let brim = difference_ex(&[rect(lo - 12.0, lo - 12.0, hi + 12.0, hi + 12.0)], &[rect(lo - 0.5, lo - 0.5, hi + 0.5, hi + 0.5)]);
        let outside: f64 = layers.iter().map(|l| total_area_mm2(&intersection_ex(&l.base, &brim)) * l.height).sum();
        assert!(outside > 100.0, "the brim keeps its columns: {outside} mm3");
        let given = given_inside(&skeleton);
        assert!(given > 500.0, "what's over the hollow is reported: {given} mm2");
        assert!(skeleton.inside.iter().any(|g| (g.at[0] - (lo + 10.0)).abs() < 8.0 && (g.at[1] - 50.0).abs() < 18.0), "it says where: {:?}", skeleton.inside);
        nothing_hangs(&layers, &slices);
    }

    /// Under an arch wider than the gap columns stand; under a narrower one it is inside, past
    /// its mouths by a quarter of the gap.
    #[test]
    fn an_open_arch_is_outside_a_narrow_one_inside() {
        let arch = |span: f64| -> Vec<Vec<ExPolygon>> {
            (0..60).map(|l| if l < 40 { vec![rect(40.0, 40.0, 46.0, 70.0), rect(46.0 + span, 40.0, 52.0 + span, 70.0)] } else { vec![rect(40.0, 40.0, 52.0 + span, 70.0)] }).collect()
        };
        let slices = arch(30.0);
        let (layers, skeleton) = plan(&slices, None, None, false);
        let under = [rect(47.0, 41.0, 75.0, 69.0)];
        assert!(stands_in(&layers, &under) > 100.0, "the open arch keeps its columns: {} mm3", stands_in(&layers, &under));
        assert_eq!(given_inside(&skeleton), 0.0, "nothing of it counts as inside");
        nothing_hangs(&layers, &slices);
        let slices = arch(8.0);
        let mouth = crate::support::INSIDE_GAP / 4.0;
        let under = [rect(46.5, 40.0 + mouth, 53.5, 70.0 - mouth)];
        let (free, _) = plan_with(&slices, None, None, false, true);
        assert!(stands_in(&free, &under) > 20.0, "asked to, it stands under the narrow one: {} mm3", stands_in(&free, &under));
        let (layers, skeleton) = plan(&slices, None, None, false);
        assert!(stands_in(&layers, &under) < 1e-6, "{} mm3 stands under the narrow arch", stands_in(&layers, &under));
        assert!(given_inside(&skeleton) > 100.0, "what's over it is reported: {} mm2", given_inside(&skeleton));
        nothing_hangs(&layers, &slices);
    }

    /// A closed box with a peg hanging from its lid, its end on air or on a hair-thin post: the
    /// peg's end gets its column inside, and only it; the lid's underside is reported.
    #[test]
    fn an_island_on_air_inside_keeps_its_column() {
        let (lo, hi) = (30.0, 70.0);
        for hair in [false, true] {
            let slices: Vec<Vec<ExPolygon>> = (0..90).map(|l| {
                let ring = difference_ex(&[rect(lo, lo, hi, hi)], &[rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)]);
                if l < 6 || l >= 80 {
                    vec![rect(lo, lo, hi, hi)]
                } else if l >= 40 {
                    union_ex(&[ring, vec![rect(48.0, 48.0, 52.0, 52.0)]].concat())
                } else if hair {
                    union_ex(&[ring, vec![rect(51.9, 51.9, 52.0, 52.0)]].concat())
                } else {
                    ring
                }
            }).collect();
            let (layers, skeleton) = plan(&slices, None, None, false);
            let under_peg = rect(47.0, 47.0, 53.0, 53.0);
            for l in [20, 38] {
                assert!(inside(&layers[l], &under_peg) > 0.5, "hair {hair}, layer {l}: the peg's end has a column under it: {} mm2", inside(&layers[l], &under_peg));
            }
            let elsewhere = difference_ex(&[rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)], &[rect(40.0, 40.0, 60.0, 60.0)]);
            assert!(stands_in(&layers, &elsewhere) < 1e-6, "hair {hair}: {} mm3 stands in the box away from the peg", stands_in(&layers, &elsewhere));
            assert!(given_inside(&skeleton) > 500.0, "hair {hair}: the lid's underside is reported: {} mm2", given_inside(&skeleton));
            nothing_hangs(&layers, &slices);
        }
    }

    /// Bars crossing in stacked bands, pores open on every face, and a wing past one face: nothing
    /// stands in the pores past their mouths, and the wing keeps its columns.
    #[test]
    fn no_column_stands_in_a_lattice() {
        let (lo, hi) = (30.0, 62.0);
        let slices: Vec<Vec<ExPolygon>> = (0..100).map(|l| {
            let along_x = (l / 10) % 2 == 0;
            let mut bars: Vec<ExPolygon> = (0..5).map(|k| {
                let a = lo + 8.0 * k as f64 - if k == 4 { 1.5 } else { 0.0 };
                if along_x { rect(lo, a, hi, a + 1.5) } else { rect(a, lo, a + 1.5, hi) }
            }).collect();
            if l >= 90 {
                bars.push(rect(hi, lo, hi + 16.0, hi));
            }
            union_ex(&bars)
        }).collect();
        let mouth = crate::support::INSIDE_GAP / 4.0;
        let pores = [rect(lo + mouth, lo + mouth, hi - mouth, hi - mouth)];
        let (free, _) = plan_with(&slices, None, None, false, true);
        assert!(stands_in(&free, &pores) > 100.0, "asked to, it stands in the pores: {} mm3", stands_in(&free, &pores));
        let (layers, skeleton) = plan(&slices, None, None, false);
        assert!(stands_in(&layers, &pores) < 1e-6, "{} mm3 stands in the pores", stands_in(&layers, &pores));
        let wing = [rect(hi + 0.5, lo, hi + 16.0, hi)];
        assert!(stands_in(&layers, &wing) > 100.0, "the wing keeps its columns: {} mm3", stands_in(&layers, &wing));
        assert!(given_inside(&skeleton) > 100.0, "what the bars hold over the pores is reported: {} mm2", given_inside(&skeleton));
        nothing_hangs(&layers, &slices);
    }
}

//! Raft layers beneath one object's model and supports.

use crate::{fill, flow, ExtrusionPath, Island, ObjectLayer, Params, Role};
use makepad_fab_slicer::clip::union_ex;
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, offset_polyline, Join};

pub(crate) fn layers(p: &Params, filament: usize, outline: &[ExPolygon], model: &[(f64, f64, ObjectLayer)], supports: &[(usize, f64, f64, ObjectLayer)]) -> Vec<(f64, f64, ObjectLayer)> {
    let mut footprint = outline.to_vec();
    if let Some(z) = supports.iter().filter(|s| s.3.has_paths()).map(|s| s.1).min_by(f64::total_cmp) {
        for (_, _, _, layer) in supports.iter().filter(|s| (s.1 - z).abs() < 1e-6) {
            if layer.support_only {
                footprint.extend(layer.slices.iter().cloned());
            } else {
                for island in &layer.islands {
                    let mut pts = island.walls.iter().chain(&island.fills).flat_map(|path| path.pts.iter().copied()).collect();
                    let hull = convex_hull(&mut pts);
                    if hull.len() >= 3 { footprint.push(ExPolygon::new(Polygon::new(hull))); }
                    else if hull.len() == 2 { footprint.extend(offset_polyline(&hull, p.support_width / 2.0 * SCALE)); }
                }
            }
        }
    }
    if footprint.is_empty() { return Vec::new(); }
    let join = Join::Round(0.01 * SCALE);
    let area = offset_ex(&union_ex(&footprint), p.raft_expansion * SCALE, join);
    if area.is_empty() { return Vec::new(); }
    let object = model.first().map(|l| l.2.object).or_else(|| supports.first().map(|s| s.3.object)).unwrap_or(0);
    let center = bbox_of(&area).center();
    p.raft_heights.iter().enumerate().map(|(li, &(z, h))| {
        let first = li == 0;
        let base = li < p.raft_base_layers;
        let region = if first { offset_ex(&area, p.raft_first_expansion * SCALE, join) } else { area.clone() };
        let width = if first { p.w_first } else { p.support_width };
        let line_spacing = flow::spacing(width, h);
        let flow_area = flow::cross_section(width, h);
        let role = if base { Role::Support } else { Role::SupportInterface };
        let mut speed = if first { p.speeds.first_layer } else if base { p.support_speed } else { p.support_interface_speed };
        if p.speeds.max_volumetric > 0.0 { speed = speed.min(p.speeds.max_volumetric / flow_area); }
        let mk = |pts: Vec<Pt>, closed: bool| ExtrusionPath { role, loop_pts: if closed { pts.clone() } else { Vec::new() }, pts, closed, width, height: h, area: flow_area, speed, ..Default::default() };
        let mut island = Island::default();
        if base {
            for e in offset_ex(&region, -width / 2.0 * SCALE, join) {
                for ring in e.rings() {
                    if ring.pts.len() < 3 { continue; }
                    let mut pts = ring.pts.clone();
                    pts.push(pts[0]);
                    island.fills.push(mk(pts, true));
                }
            }
        }
        if !first || !base || p.raft_first_density > 0.0 {
            let spacing = if first && base { line_spacing / p.raft_first_density } else { line_spacing + (if base { p.support_base_spacing } else { p.support_interface_spacing }).max(0.0) };
            let angle = if base {
                if first { std::f64::consts::FRAC_PI_2 } else { 0.0 }
            } else if (li - p.raft_base_layers) % 2 == 0 { std::f64::consts::FRAC_PI_4 } else { -std::f64::consts::FRAC_PI_4 };
            let inner = offset_ex(&region, -(width / 2.0 + if base { line_spacing / 2.0 } else { 0.0 }) * SCALE, join);
            for pl in fill::lines(&inner, angle, spacing, center, fill::Align::Grid, fill::Connect::Boundary { max: spacing * 3.0, anchor: 0.0 }) {
                if pl.pts.len() >= 2 { island.fills.push(mk(pl.pts, false)); }
            }
        }
        (z, h, ObjectLayer { object, filament, islands: vec![std::sync::Arc::new(island)], slices: region.into(), support_only: true })
    }).collect()
}

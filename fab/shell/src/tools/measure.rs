//! The measurement tool — distance, area, angle.
//!
//! The state machine is tiny; the accuracy is the point. Every point placed is
//! a [`crate::tools::snap`] result, so a distance between two corners is the
//! model's own dimension, not a click position: the gate below measures a box
//! part through the whole pipeline (camera → projection → ray → BVH → snap)
//! and asserts it reproduces the part's bounds to within one millimetre.
//!
//! Results go through the model host as document changes. `AppState::measurements`
//! is the derived view of the project's rulers; the drawing lives in `overlay.rs`.

use crate::api::*;
use crate::tools::session;
use makepad_csg_cad::measure as cad;
use makepad_widgets_core::*;

/// How many points a measurement of this kind needs before it commits.
/// `Area` is open-ended: it commits when the user closes the loop.
pub fn needed(kind: MeasureKind) -> usize {
    match kind {
        MeasureKind::Distance => 2,
        MeasureKind::Angle => 3,
        MeasureKind::Area => usize::MAX,
        MeasureKind::EdgeLength | MeasureKind::Radius => 1,
        MeasureKind::FaceDistance => 2,
    }
}

/// Meters / square meters / degrees for a finished point set.
pub fn value_of(kind: MeasureKind, points: &[Vec3f], normals: &[Vec3f]) -> f64 {
    use crate::model::query;
    match kind {
        MeasureKind::Distance => {
            if points.len() < 2 {
                0.0
            } else {
                query::distance(points[0], points[1]) as f64
            }
        }
        MeasureKind::Angle => {
            if points.len() < 3 {
                0.0
            } else {
                // Clicked A, then the corner, then B: the angle is at the
                // middle point.
                query::angle_deg(points[0], points[1], points[2]) as f64
            }
        }
        MeasureKind::Area => query::polygon_area(points) as f64,
        MeasureKind::EdgeLength => cad::edge_length(&positions(points)),
        MeasureKind::Radius => cad::fit_circle3(&positions(points)).map_or(f64::NAN, |c| c.radius),
        MeasureKind::FaceDistance => face_gap(points, normals).map_or(f64::NAN, |g| g.distance),
    }
}

/// Format a value in the user's display unit (`session::ToolSession::units`).
pub fn format(kind: MeasureKind, value: f64, units: &Units) -> String {
    match kind {
        MeasureKind::Distance => units.format_length(value),
        MeasureKind::Area => units.format_area(value),
        MeasureKind::Angle => units.format_angle(value),
        MeasureKind::EdgeLength | MeasureKind::FaceDistance => units.format_length(value),
        MeasureKind::Radius => format!("R {}", Units { precision: units.precision.max(2), ..*units }.format_length(value)),
    }
}

pub fn kind_label(kind: MeasureKind) -> &'static str {
    match kind {
        MeasureKind::Distance => "Distance",
        MeasureKind::Area => "Area",
        MeasureKind::Angle => "Angle",
        MeasureKind::EdgeLength => "Edge",
        MeasureKind::Radius => "Radius",
        MeasureKind::FaceDistance => "Faces",
    }
}

/// How far the loop strays from its own best-fit plane, in meters.
///
/// `query::polygon_area` is Newell's method: for a non-planar loop it reports
/// the area of the *projection* onto the best-fit plane without saying so. We
/// measure the deviation and say so (`~` on the label) rather than quoting a
/// number that is not the area of anything.
pub fn planarity(points: &[Vec3f]) -> f32 {
    if points.len() < 4 {
        return 0.0;
    }
    let mut n = Vec3f::default();
    let mut c = Vec3f::default();
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        n += Vec3f::cross(a, b);
        c += a;
    }
    let len = n.length();
    if len < 1e-9 {
        return 0.0;
    }
    let n = n / len;
    let c = c / points.len() as f32;
    points
        .iter()
        .map(|p| (*p - c).dot(n).abs())
        .fold(0.0f32, f32::max)
}

/// Loops flatter than this count as planar (1 mm — the measurement gate).
pub const PLANAR_TOLERANCE: f32 = 0.001;

/// Turn a finished draft into a `Measurement`. Returns `None` when there are
/// not enough points.
pub fn commit(kind: MeasureKind, points: Vec<Vec3f>, units: &Units) -> Option<Measurement> {
    commit_with_normals(kind, points, Vec::new(), units)
}

pub fn commit_with_normals(kind: MeasureKind, points: Vec<Vec3f>, normals: Vec<Vec3f>, units: &Units) -> Option<Measurement> {
    let min = match kind {
        MeasureKind::Distance => 2,
        MeasureKind::Angle => 3,
        MeasureKind::Area => 3,
        MeasureKind::EdgeLength | MeasureKind::FaceDistance => 2,
        MeasureKind::Radius => 3,
    };
    if points.len() < min {
        return None;
    }
    let circle = if kind == MeasureKind::Radius { Some(cad::fit_circle3(&positions(&points))?) } else { None };
    if circle.is_some_and(|c| c.rms >= 1e-3 * c.radius) { return None; }
    let gap = if kind == MeasureKind::FaceDistance { Some(face_gap(&points, &normals)?) } else { None };
    let value = match (circle, gap) {
        (Some(circle), _) => circle.radius,
        (_, Some(gap)) => gap.distance,
        _ => value_of(kind, &points, &normals),
    };
    if !value.is_finite() { return None; }
    let mut label = format(kind, value, units);
    if let Some(gap) = gap {
        if !gap.parallel { label = units.format_angle(gap.angle_deg); }
    }
    if kind == MeasureKind::Area && planarity(&points) > PLANAR_TOLERANCE {
        // Say that this is the projected area of a non-planar loop.
        label = format!("~{label}");
    }
    Some(Measurement {
        name: None,
        kind,
        points,
        value,
        label,
        circle,
        gap,
        normals,
    })
}

pub fn position(p: Vec3f) -> [f64; 3] {
    [p.x as f64, p.y as f64, p.z as f64]
}

pub fn world(p: [f64; 3]) -> Vec3f {
    vec3(p[0] as f32, p[1] as f32, p[2] as f32)
}

fn positions(points: &[Vec3f]) -> Vec<[f64; 3]> {
    points.iter().map(|p| position(*p)).collect()
}

pub fn face_gap(points: &[Vec3f], normals: &[Vec3f]) -> Option<cad::PlaneGap> {
    if points.len() < 2 || normals.len() < 2 || normals[..2].iter().any(|n| n.length() < 1e-9) { return None; }
    Some(cad::plane_distance(position(points[0]), position(normals[0]), position(points[1]), position(normals[1])))
}

pub fn measurement_label(m: &Measurement, units: &Units) -> String {
    if let Some(gap) = m.gap {
        if !gap.parallel { return units.format_angle(gap.angle_deg); }
    }
    if !m.value.is_finite() { return m.label.clone(); }
    let label = format(m.kind, m.value, units);
    if m.kind == MeasureKind::Area && m.label.starts_with('~') {
        format!("~{label}")
    } else {
        label
    }
}

pub fn kind_name(kind: MeasureKind) -> &'static str {
    match kind {
        MeasureKind::Distance => "distance",
        MeasureKind::Area => "area",
        MeasureKind::Angle => "angle",
        MeasureKind::EdgeLength => "edge_length",
        MeasureKind::Radius => "radius",
        MeasureKind::FaceDistance => "face_distance",
    }
}

pub fn parse_kind(kind: &str) -> Option<MeasureKind> {
    Some(match kind {
        "distance" => MeasureKind::Distance,
        "area" => MeasureKind::Area,
        "angle" => MeasureKind::Angle,
        "edge_length" | "edge" => MeasureKind::EdgeLength,
        "radius" => MeasureKind::Radius,
        "face_distance" | "faces" => MeasureKind::FaceDistance,
        _ => return None,
    })
}

/// Selecting a ruler always releases the part selection, including its gizmo.
pub fn select(state: &mut AppState, index: usize) -> bool {
    if index >= state.measurements.len() { return false; }
    state.scene_state.clear_selection();
    state.selected_measure = Some(index);
    session::with(|s| s.measure_drag = None);
    true
}

/// Install the worker's derived view without letting an erased index select its neighbour.
pub fn install(state: &mut AppState, mut measurements: Vec<Measurement>) {
    session::with(|s| s.measure_drag = None);
    if measurements.len() < state.measurements.len()
        || state.selected_measure.is_some_and(|i| i >= measurements.len()) {
        state.selected_measure = None;
    }
    for m in &mut measurements { m.label = measurement_label(m, &state.units); }
    state.measurements = measurements;
}

/// The feature's dimension line (the circle and face witness line are drawn separately).
fn dimension(m: &Measurement) -> Option<[Vec3f; 2]> {
    let first = *m.points.first()?;
    if let Some(circle) = m.circle {
        let center = world(circle.center);
        let normal = world(circle.normal);
        let radial = first - center;
        let u = (radial - normal * radial.dot(normal)).normalize();
        Some([center, center + u * circle.radius as f32])
    } else if m.gap.is_some_and(|g| g.parallel) && m.points.len() >= 2 && !m.normals.is_empty() {
        let n = m.normals[0].normalize();
        Some([first, first + n * (m.points[1] - first).dot(n)])
    } else {
        None
    }
}

/// Shared by drawing and picking so a label has the same anchor in both paths.
pub(super) fn label_position(m: &Measurement, proj: &ViewProjector) -> Option<DVec2> {
    let anchor = if let Some([a, b]) = dimension(m) {
        proj.project((a + b) * 0.5)?
    } else if matches!(m.kind, MeasureKind::EdgeLength | MeasureKind::Radius | MeasureKind::FaceDistance) {
        if m.points.len() == 2 {
            proj.project((m.points[0] + m.points[1]) * 0.5)?
        } else {
            proj.project(*m.points.get(m.points.len() / 2)?)?
        }
    } else if m.kind == MeasureKind::Angle {
        proj.project(*m.points.get(1)?)?
    } else {
        if m.points.is_empty() { return None; }
        let mut center = dvec2(0.0, 0.0);
        for p in &m.points { center += proj.project(*p)?; }
        center / m.points.len() as f64
    };
    Some(anchor + dvec2(7.0, -13.0))
}

fn segment_distance(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let ap = p - a;
    let t = ((ap.x * ab.x + ap.y * ab.y) / (ab.x * ab.x + ab.y * ab.y).max(1e-9)).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Screen-space pick, with the same feature geometry and label anchor as the overlay.
fn hit_test(m: &Measurement, units: &Units, proj: &ViewProjector, pos: DVec2) -> bool {
    let near = |a, b| match (proj.project(a), proj.project(b)) {
        (Some(a), Some(b)) => segment_distance(pos, a, b) <= 7.0,
        _ => false,
    };
    if let Some([a, b]) = dimension(m) {
        if near(a, b) { return true; }
        if let Some(circle) = m.circle {
            let center = world(circle.center);
            let u = (b - a).normalize();
            let v = Vec3f::cross(world(circle.normal), u);
            let mut last = b;
            for i in 1..=96 {
                let angle = std::f32::consts::TAU * i as f32 / 96.0;
                let next = center + (u * angle.cos() + v * angle.sin()) * circle.radius as f32;
                if near(last, next) { return true; }
                last = next;
            }
        } else if m.points.len() >= 2 && near(b, m.points[1]) { return true; }
    } else if m.points.windows(2).any(|p| near(p[0], p[1])) {
        return true;
    }
    if m.kind == MeasureKind::Area && m.points.len() > 2 && near(m.points[0], *m.points.last().unwrap()) {
        return true;
    }
    label_position(m, proj).is_some_and(|p| {
        let width = measurement_label(m, units).chars().count() as f64 * 7.0;
        pos.x >= p.x - 5.0 && pos.x <= p.x + width + 5.0 && pos.y >= p.y - 5.0 && pos.y <= p.y + 19.0
    })
}

pub(super) fn pick(state: &AppState, proj: &ViewProjector, pos: DVec2) -> Option<usize> {
    if !state.view.overlays.measurements { return None; }
    state.measurements.iter().enumerate().rev()
        .find(|(_, m)| hit_test(m, &state.units, proj, pos)).map(|(i, _)| i)
}

pub(super) fn point_at(state: &AppState, proj: &ViewProjector, pos: DVec2) -> Option<(usize, usize)> {
    if !state.view.overlays.measurements { return None; }
    let index = state.selected_measure?;
    state.measurements.get(index)?.points.iter().enumerate()
        .filter_map(|(i, p)| proj.project(*p).map(|p| (i, (p - pos).length())))
        .filter(|(_, distance)| *distance <= 10.0)
        .min_by(|a, b| a.1.total_cmp(&b.1)).map(|(point, _)| (index, point))
}

pub(super) fn start_drag(state: &AppState, proj: &ViewProjector, pos: DVec2, index: usize, point: usize) {
    let original = state.measurements[index].clone();
    let anchor = original.points[point];
    let normal = proj.ray(proj.rect.pos + proj.rect.size * 0.5).dir;
    session::with(|s| s.measure_drag = Some(session::MeasureDrag {
        index, point, start: pos, preview: original.clone(), original,
        plane: Plane::from_point_normal(anchor, normal), moved: false,
    }));
}

pub(super) fn drag_to(state: &AppState, proj: &ViewProjector, pos: DVec2) {
    // Snap once per pointer event; drawing never scans the scene's geometry.
    let options = state.snap;
    let hit = (options.vertex || options.edge_midpoint || options.edge || options.face)
        .then(|| super::snap::snap(&state.scene, proj, pos, &options, &|id| state.is_visible(id) && !state.plate.is_context(id)))
        .flatten().filter(|h| match h.kind {
            SnapKind::Vertex => options.vertex,
            SnapKind::EdgeMidpoint => options.edge_midpoint,
            SnapKind::Edge => options.edge,
            SnapKind::Face => options.face,
            SnapKind::Ground => false,
        });
    session::with(|s| {
        let Some(drag) = &mut s.measure_drag else { return };
        if !drag.moved && (pos - drag.start).length() < super::DRAG_SLOP { return; }
        let ray = proj.ray(pos);
        let Some(point) = hit.map(|h| h.point).or_else(|| ray.intersect_plane(&drag.plane).map(|t| ray.at(t))) else { return };
        let mut points = drag.original.points.clone();
        points[drag.point] = point;
        let mut normals = drag.original.normals.clone();
        if drag.original.kind == MeasureKind::FaceDistance {
            if let (Some(normal), Some(to)) = (hit.and_then(|h| h.normal), normals.get_mut(drag.point)) { *to = normal; }
        }
        // Invalid circle fits/degenerate faces still preview the point being dragged.
        let mut preview = commit_with_normals(drag.original.kind, points.clone(), normals.clone(), &state.units)
            .unwrap_or_else(|| Measurement {
                name: None, kind: drag.original.kind, points, normals, value: f64::NAN,
                label: "Invalid measurement".into(), circle: None, gap: None,
            });
        preview.name = drag.original.name.clone();
        drag.preview = preview;
        drag.moved = true;
        s.snap_cursor = hit;
    });
}

pub(super) fn finish_drag(cx: &mut Cx) {
    session::with(|s| {
        s.snap_cursor = None;
        if let Some(drag) = s.measure_drag.take() {
            if drag.moved && drag.preview != drag.original {
                cx.action(ShellAction::SetMeasurement { index: drag.index, measurement: drag.preview });
            }
        }
    });
}

/// Collect only this element's triangles, as snapping does. Work is performed
/// once on a click, never by the overlay. Convert to mm for the edge weld.
pub fn edge_chain(scene: &Scene, hit: &SnapHit) -> Result<Vec<Vec3f>, &'static str> {
    let el = hit.element.and_then(|id| scene.element(id)).ok_or("Pick a part's feature edge")?;
    let count: usize = el.ranges.iter().map(|r| r.2 as usize).sum();
    let mut positions = Vec::with_capacity(count);
    for &(bi, first, count) in &el.ranges {
        let Some(batch) = scene.batches.get(bi as usize) else { continue };
        let end = (first as usize + count as usize).min(batch.indices.len());
        for tri in batch.indices[first as usize..end].chunks_exact(3) {
            for &i in tri {
                positions.push(position(batch.position(i)).map(|v| v * 1000.0));
            }
        }
    }
    let indices: Vec<u32> = (0..positions.len() as u32).collect();
    let at = position(hit.point).map(|v| v * 1000.0);
    cad::feature_edge_chain(&positions, &indices, at)
        .map(|chain| chain.into_iter().map(|p| world(p.map(|v| v * 0.001))).collect())
        .ok_or("No feature edge here")
}

pub fn edge_measurement(scene: &Scene, proj: &ViewProjector, hit: &SnapHit, kind: MeasureKind, radius_px: f32, units: &Units) -> Result<Measurement, &'static str> {
    let points = edge_chain(scene, hit)?;
    let nearest = cad::closest_on_chain(&positions(&points), position(hit.point)).ok_or("No feature edge here")?;
    let (Some(a), Some(b)) = (proj.project(hit.point), proj.project(world(nearest))) else { return Err("Pick a visible feature edge") };
    if (a - b).length() > radius_px as f64 { return Err("Pick closer to a feature edge"); }
    commit(kind, points, units).ok_or("This edge is not circular")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::snap::snap;
    use crate::model::ElementId;

    const MM: f64 = 0.001;

    #[test]
    fn rulers_and_parts_have_mutually_exclusive_selection() {
        let mut state = AppState::default();
        state.set_scene(std::sync::Arc::new(small_box_scene()));
        let id = state.scene.elements[0].id;
        let m = commit(MeasureKind::Distance, vec![vec3(0.0, 0.0, 0.0), vec3(0.04, 0.0, 0.0)], &state.units).unwrap();
        install(&mut state, vec![m.clone()]);
        for action in [ShellAction::SelectOnly(id), ShellAction::SelectToggle(id), ShellAction::SelectAdd(id), ShellAction::SelectSet(vec![id])] {
            assert!(select(&mut state, 0));
            assert!(state.scene_state.selection.is_empty());
            state.apply_core(&action);
            assert_eq!(state.selected_measure, None);
            assert!(state.scene_state.selection.contains(id));
        }
        assert!(!select(&mut state, 1));
        assert!(state.scene_state.selection.contains(id));
        assert!(select(&mut state, 0));
        state.update_scene(state.scene.clone(), false);
        assert_eq!(state.selected_measure, Some(0));
        assert_eq!(state.measurements.len(), 1);
        state.apply_core(&ShellAction::ClearSelection);
        assert_eq!(state.selected_measure, None);
        select(&mut state, 0);
        install(&mut state, Vec::new());
        assert_eq!(state.selected_measure, None);
        install(&mut state, vec![m]);
        select(&mut state, 0);
        state.set_scene(state.scene.clone());
        assert_eq!(state.selected_measure, None);
        assert!(state.measurements.is_empty());
    }

    #[test]
    fn picks_ruler_lines_labels_and_handles_in_screen_space() {
        let pane = crate::tools::ToolSet::default().pane;
        let mut state = AppState::default();
        let scene = small_box_scene();
        state.view.camera_mut(pane).frame_bounds(&scene.bounds, 1.0);
        let proj = ViewProjector::new(*state.view.camera(pane), Rect { pos: dvec2(30.0, 40.0), size: dvec2(800.0, 800.0) });
        let m = commit(MeasureKind::Distance, vec![scene.bounds.min, scene.bounds.max], &state.units).unwrap();
        let midpoint = (proj.project(m.points[0]).unwrap() + proj.project(m.points[1]).unwrap()) * 0.5;
        let label = label_position(&m, &proj).unwrap();
        state.measurements.push(m);
        assert_eq!(super::pick(&state, &proj, midpoint), Some(0));
        assert_eq!(super::pick(&state, &proj, label + dvec2(20.0, 5.0)), Some(0));
        assert_eq!(point_at(&state, &proj, proj.project(state.measurements[0].points[0]).unwrap()), None);
        select(&mut state, 0);
        assert_eq!(point_at(&state, &proj, proj.project(state.measurements[0].points[0]).unwrap()), Some((0, 0)));
        state.view.overlays.measurements = false;
        assert_eq!(super::pick(&state, &proj, midpoint), None);
    }

    fn small_box_scene() -> Scene {
        let mut model = crate::model::fixture::parts();
        model.elements.truncate(1);
        model.meshes.truncate(1);
        for p in &mut model.meshes[0].positions {
            p[0] = (p[0] + 3.0) * 0.005;
            p[1] = (p[1] + 2.0) * 0.005;
            p[2] *= 0.01 / 0.6;
        }
        Scene::from_model(model, &mut |_| {})
    }

    fn cylinder_scene() -> Scene {
        use crate::model::{ElementData, MeshData, MeshId, MeshRef, ModelData};
        let mut mesh = MeshData::default();
        mesh.positions.extend_from_slice(&[[0.0, 0.0, 0.0], [0.0, 0.0, 0.02]]);
        let segments = 96u32;
        for i in 0..segments {
            let t = std::f32::consts::TAU * i as f32 / segments as f32;
            for z in [0.0, 0.02] {
                mesh.positions.push([0.0075 * t.cos(), 0.0075 * t.sin(), z]);
            }
        }
        for i in 0..segments {
            let a = 2 + 2 * i;
            let b = a + 1;
            let c = 2 + 2 * ((i + 1) % segments);
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, c, b, b, c, d, 0, c, a, 1, b, d]);
        }
        let model = ModelData {
            meshes: vec![mesh],
            elements: vec![ElementData {
                id: ElementId::from_index(0),
                name: "Cylinder".into(),
                meshes: vec![MeshRef::identity(MeshId::from_index(0))],
                ..Default::default()
            }],
            ..Default::default()
        };
        Scene::from_model(model, &mut |_| {})
    }

    fn pick(scene: &Scene, at: Vec3f, view: Vec3f, face: bool) -> (ViewProjector, SnapHit) {
        let center = aabb_center(&scene.bounds);
        let cam = Camera {
            target: center,
            eye: center + view.normalize() * 0.15,
            ortho: true,
            ortho_height: 0.06,
            near: 0.001,
            far: 1.0,
            ..Default::default()
        };
        let proj = ViewProjector::new(cam, Rect { pos: dvec2(0.0, 0.0), size: dvec2(1600.0, 1200.0) });
        let mut screen = proj.project(at).unwrap();
        if !face {
            // A pixel inside the silhouette guarantees a surface ray hit.
            screen += (proj.project(center).unwrap() - screen).normalize();
        }
        let opts = SnapOptions { vertex: false, edge_midpoint: false, edge: !face, ..Default::default() };
        let hit = snap(scene, &proj, screen, &opts, &|_| true).expect("ray hits the part");
        assert!(hit.element.is_some(), "{hit:?}");
        (proj, hit)
    }

    #[test]
    fn feature_edge_pick_measures_30_mm_box_edge() {
        let scene = small_box_scene();
        let (proj, hit) = pick(&scene, vec3(0.015, 0.0, 0.01), vec3(0.0, -1.0, 1.0), false);
        let m = edge_measurement(&scene, &proj, &hit, MeasureKind::EdgeLength, 12.0, &Units::millimeters()).unwrap();
        assert!((m.value - 0.03).abs() < 0.05 * MM, "{m:?}");
        assert_eq!(needed(MeasureKind::EdgeLength), 1);
        assert!(commit(MeasureKind::Radius, m.points, &Units::millimeters()).is_none());
    }

    #[test]
    fn face_picks_measure_opposite_box_faces() {
        let scene = small_box_scene();
        let (_, a) = pick(&scene, vec3(0.009, 0.0, 0.004), vec3(0.0, -1.0, 0.2), true);
        let (_, b) = pick(&scene, vec3(0.021, 0.02, 0.006), vec3(0.0, 1.0, 0.2), true);
        assert_eq!(a.kind, SnapKind::Face);
        assert_eq!(b.kind, SnapKind::Face);
        let m = commit_with_normals(MeasureKind::FaceDistance, vec![a.point, b.point], vec![a.normal.unwrap(), b.normal.unwrap()], &Units::millimeters()).unwrap();
        assert!((m.value - 0.02).abs() < 0.05 * MM, "{m:?}");
        assert!(m.gap.unwrap().parallel);
        assert_eq!(needed(MeasureKind::FaceDistance), 2);
    }

    #[test]
    fn rim_pick_measures_cylinder_radius() {
        let scene = cylinder_scene();
        // Off the facet seams: a ray exactly along a shared edge is a
        // precision case of the BVH, not of the measurement.
        let t = -1.5f32;
        let (proj, hit) = pick(&scene, vec3(0.0075 * t.cos(), 0.0075 * t.sin(), 0.02), vec3(0.0, -1.0, 1.0), false);
        let m = edge_measurement(&scene, &proj, &hit, MeasureKind::Radius, 12.0, &Units::millimeters()).unwrap();
        assert!((m.value - 0.0075).abs() < 0.05 * MM, "{m:?}");
        assert_eq!(m.label, "R 7.50 mm");
        assert_eq!(m.points.first(), m.points.last());
        let length = value_of(MeasureKind::EdgeLength, &m.points, &[]);
        assert!((length - std::f64::consts::TAU * 0.0075).abs() < 0.05 * MM);
        assert_eq!(needed(MeasureKind::Radius), 1);
    }

    fn corners(b: &Aabb) -> [Vec3f; 8] {
        [
            vec3(b.min.x, b.min.y, b.min.z),
            vec3(b.max.x, b.min.y, b.min.z),
            vec3(b.max.x, b.max.y, b.min.z),
            vec3(b.min.x, b.max.y, b.min.z),
            vec3(b.min.x, b.min.y, b.max.z),
            vec3(b.max.x, b.min.y, b.max.z),
            vec3(b.max.x, b.max.y, b.max.z),
            vec3(b.min.x, b.max.y, b.max.z),
        ]
    }

    /// An element whose eight bounds corners are all real vertices — a box
    /// part. Its bounds are then a *known* dimension we can measure
    /// against.
    fn box_element(scene: &Scene) -> Option<(ElementId, Aabb)> {
        let mut best: Option<(ElementId, Aabb, f32)> = None;
        for el in &scene.elements {
            if !el.has_geometry() || el.triangle_count > 4096 {
                continue;
            }
            let ext = aabb_extent(&el.bounds);
            let size = ext.x.max(ext.y);
            if size < 1.0 || ext.z < 0.5 {
                continue;
            }
            if best.as_ref().map_or(false, |b| b.2 >= size) {
                continue;
            }
            // every corner must exist as a vertex
            let cs = corners(&el.bounds);
            let mut found = [false; 8];
            for (bi, first, count) in el.ranges.iter().copied() {
                let Some(batch) = scene.batches.get(bi as usize) else {
                    continue;
                };
                let end = (first + count) as usize;
                for i in first as usize..end.min(batch.indices.len()) {
                    let p = batch.position(batch.indices[i]);
                    for (k, c) in cs.iter().enumerate() {
                        if (p - *c).length() < 1e-4 {
                            found[k] = true;
                        }
                    }
                }
            }
            if found.iter().all(|f| *f) {
                best = Some((el.id, el.bounds, size));
            }
        }
        best.map(|(id, b, _)| (id, b))
    }

    /// Measure between two world points the way the user does: put the camera
    /// where both are visible, aim a few pixels inside each corner, snap.
    fn measure_between(scene: &Scene, element: ElementId, a: Vec3f, b: Vec3f) -> (Vec3f, Vec3f) {
        let bounds = scene.element(element).unwrap().bounds;
        let center = aabb_center(&bounds);
        let mut cam = Camera::default();
        // Look from a direction that sees both corners obliquely.
        cam.target = center;
        cam.eye = center + vec3(1.0, -1.3, 0.85).normalize() * (aabb_radius(&bounds) * 3.0);
        cam.frame_bounds(&bounds, 1.5);
        cam.fit_clip_planes(&bounds, 1.5);
        let rect = Rect {
            pos: dvec2(0.0, 0.0),
            size: dvec2(1500.0, 1000.0),
        };
        let proj = ViewProjector::new(cam, rect);
        let sc = proj.project(center).unwrap();
        let opts = SnapOptions::default();
        // Only this element is "visible": the measure tool with the part
        // isolated, which is how you measure one part on a full plate.
        let visible = |id: ElementId| id == element;
        let mut out = [a, b];
        for (i, p) in [a, b].iter().enumerate() {
            let s = proj.project(*p).unwrap();
            let inward = (sc - s).normalize();
            let mut got = None;
            for push in [4.0, 6.0, 9.0] {
                if let Some(h) = snap(scene, &proj, s + inward * push, &opts, &visible) {
                    if h.kind == SnapKind::Vertex {
                        got = Some(h.point);
                        break;
                    }
                    got = got.or(Some(h.point));
                }
            }
            out[i] = got.unwrap_or(*p);
        }
        (out[0], out[1])
    }

    fn assert_element_measures(scene: &Scene) {
        let Some((id, bounds)) = box_element(scene) else {
            panic!("no box-shaped element in this scene to measure");
        };
        let ext = aabb_extent(&bounds);
        let axis = if ext.x >= ext.y { 0 } else { 1 };
        let (a, b) = if axis == 0 {
            (
                vec3(bounds.min.x, bounds.min.y, bounds.min.z),
                vec3(bounds.max.x, bounds.min.y, bounds.min.z),
            )
        } else {
            (
                vec3(bounds.min.x, bounds.min.y, bounds.min.z),
                vec3(bounds.min.x, bounds.max.y, bounds.min.z),
            )
        };
        let expect_len = if axis == 0 { ext.x } else { ext.y } as f64;
        let (sa, sb) = measure_between(scene, id, a, b);
        let got = value_of(MeasureKind::Distance, &[sa, sb], &[]);
        assert!(
            (got - expect_len).abs() < MM,
            "length: measured {got} m, bounds say {expect_len} m (element {:?})",
            scene.element(id).map(|e| e.name.clone())
        );

        // and the height, bottom corner to top corner
        let a = vec3(bounds.min.x, bounds.min.y, bounds.min.z);
        let b = vec3(bounds.min.x, bounds.min.y, bounds.max.z);
        let (sa, sb) = measure_between(scene, id, a, b);
        let got = value_of(MeasureKind::Distance, &[sa, sb], &[]);
        assert!(
            (got - ext.z as f64).abs() < MM,
            "height: measured {got} m, bounds say {} m",
            ext.z
        );
    }

    #[test]
    fn measures_a_part_to_the_millimetre() {
        let scene = Scene::from_model(crate::model::fixture::parts(), &mut |_| {});
        assert_element_measures(&scene);
    }

    #[test]
    fn area_and_angle_math() {
        let square = [
            vec3(0.0, 0.0, 0.0),
            vec3(3.0, 0.0, 0.0),
            vec3(3.0, 4.0, 0.0),
            vec3(0.0, 4.0, 0.0),
        ];
        assert!((value_of(MeasureKind::Area, &square, &[]) - 12.0).abs() < 1e-6);
        let angle = [vec3(1.0, 0.0, 0.0), vec3(0.0, 0.0, 0.0), vec3(0.0, 1.0, 0.0)];
        assert!((value_of(MeasureKind::Angle, &angle, &[]) - 90.0).abs() < 1e-4);
        let u = Units {
            source_to_meters: 1.0,
            display: crate::model::units::LengthUnit::Millimeter,
            precision: 0,
        };
        assert_eq!(format(MeasureKind::Distance, 3.25, &u), "3250 mm");
    }
}

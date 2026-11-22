//! Fit the panel's active window to a face in the body's mesh frame.

use super::board_geom::{BodyMesh, BODY_VERTEX_FLOATS};
use super::{aabb_center, aabb_is_empty, vec3, Aabb, Cx, Vec3f};
use fab_elec_core::{to_mm, CompRef, Sel};
use fab_elec_mech::scene::ElecScene;
use fab_elec_parts::{body::port_axes, BodyPortKind};
use fab_elec_view::{live::sim::LiveScreen, ElecLive};
use makepad_widgets_core::log;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// A display panel's luminance at full white and full brightness, in cd/m²: the glass emits a
/// texel's linear value times this, times the panel's brightness.
pub(super) const SCREEN_WHITE_NITS: f32 = 500.0;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Panel {
    size: (u32, u32),
    glass: [u32; 4],
}

impl From<&LiveScreen> for Panel {
    fn from(screen: &LiveScreen) -> Self {
        Self { size: screen.size, glass: screen.glass }
    }
}

/// Only the fields that place the glass. Keeping the live Arc here would make the host copy
/// its panel vector on the next pixel publication.
#[derive(Default)]
pub struct ScreenLayout {
    panels: HashMap<CompRef, Panel>,
}

impl ScreenLayout {
    fn live(cx: &Cx) -> &[LiveScreen] {
        cx.get_global_ref::<ElecLive>().map_or(&[], |live| live.sim.screens.as_slice())
    }

    pub(super) fn changed(&self, cx: &Cx) -> bool {
        let live = Self::live(cx);
        live.len() != self.panels.len() || live.iter().any(|s| self.panels.get(&s.comp) != Some(&Panel::from(s)))
    }

    pub(super) fn update(&mut self, cx: &Cx) -> bool {
        if !self.changed(cx) { return false; }
        self.panels.clear();
        self.panels.extend(Self::live(cx).iter().map(|s| (s.comp, Panel::from(s))));
        true
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScreenFace {
    pub comp: CompRef,
    pub board: usize,
    pub body: usize,
    /// Body millimetres, before the glass offset.
    pub centre: Vec3f,
    pub dir: Vec3f,
    pub up: Vec3f,
    pub right: Vec3f,
    pub half_width: f32,
    pub half_height: f32,
    /// Whole frame size and active window (x, y, width, height) in memory texels.
    pub size: (u32, u32),
    pub glass: [u32; 4],
}

/// Fit each panel to its first light port or to the top of its drawn mesh. The bounds are the
/// union of that body's mesh primitives in body mm, indexed by scene board and body.
pub fn screen_faces(scene: &ElecScene, layout: &ScreenLayout, mesh_bounds: &[Vec<Aabb>]) -> Vec<ScreenFace> {
    let mut faces = Vec::with_capacity(layout.panels.len());
    if layout.panels.is_empty() { return faces; }
    for (board, unit) in scene.boards.iter().enumerate() {
        for (body, part) in unit.bodies.iter().enumerate() {
            let Sel::Comp(comp) = &part.sel else { continue };
            let Some(&Panel { size, glass }) = layout.panels.get(comp) else { continue };
            let light = part.ports.iter().find(|p| p.kind == BodyPortKind::Light);
            let (centre, dir, up, right, width, height) = if let Some(port) = light {
                // The profile lies on the port's axes (`port_axes`): u across, v up.
                let open = port.profile.bbox();
                let (u, v) = port_axes(port.dir);
                let axis = |a: [f64; 3]| vec3(a[0] as f32, a[1] as f32, a[2] as f32);
                let mid = open.center();
                let centre = axis(port.at.map(to_mm)) + axis(u) * to_mm(mid.x) as f32 + axis(v) * to_mm(mid.y) as f32;
                (centre, axis(port.dir).normalize(), axis(v), axis(u), open.width() as f64 * 1e-6, open.height() as f64 * 1e-6)
            } else {
                let window = match &part.mesh {
                    fab_elec_mech::scene::MeshRef::Package(p) => fab_elec_mech::package::display_window(p)
                        .map(|(min, max)| (min, max, to_mm(p.height))),
                    _ => None,
                };
                let (centre, width, height) = if let Some((min, max, top)) = window {
                    let mid_x = (min[0] + max[0]) / 2.0;
                    let mid_y = (min[1] + max[1]) / 2.0;
                    (vec3(mid_x as f32, mid_y as f32, top as f32), max[0] - min[0], max[1] - min[1])
                } else {
                    // The bounds are the uploaded scene's: a body they do not cover yet waits for its build.
                    let Some(bounds) = mesh_bounds.get(board).and_then(|bodies| bodies.get(body)) else { continue };
                    if aabb_is_empty(bounds) { continue; }
                    let middle = aabb_center(bounds);
                    let span = bounds.max - bounds.min;
                    (vec3(middle.x, middle.y, bounds.max.z), f64::from(span.x), f64::from(span.y))
                };
                (centre, vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), width, height)
            };
            if width <= 0.0 || height <= 0.0 || glass[2] == 0 || glass[3] == 0 { continue; }
            let aspect = f64::from(glass[2]) / f64::from(glass[3]);
            let face_aspect = width / height;
            if face_aspect < aspect * 0.99 || face_aspect > aspect * 1.01 {
                static REPORTED: OnceLock<Mutex<HashSet<CompRef>>> = OnceLock::new();
                if REPORTED.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).insert(*comp) {
                    log!("screen {comp:?}: face aspect {face_aspect:.5}, glass aspect {aspect:.5}");
                }
            }
            faces.push(ScreenFace {
                comp: *comp, board, body, centre, dir, up, right, size, glass,
                half_width: (0.5 * width.min(height * aspect)) as f32,
                half_height: (0.5 * height.min(width / aspect)) as f32,
            });
        }
    }
    faces
}

impl ScreenFace {
    pub(super) fn glass_centre(&self) -> Vec3f {
        self.centre + self.dir * 0.02
    }

    /// Full frame-memory texels, including positions beyond the active window during a drag.
    pub(super) fn texel(&self, p_body_mm: Vec3f) -> (f32, f32) {
        let offset = p_body_mm - self.centre;
        let s = offset.dot(self.right) / self.half_width;
        let t = offset.dot(self.up) / self.half_height;
        let [x, y, w, h] = self.glass;
        (x as f32 + (s + 1.0) * 0.5 * w as f32, y as f32 + (1.0 - t) * 0.5 * h as f32)
    }

    /// Counterclockwise from the bottom left, with row zero on the up edge.
    pub(super) fn quad(&self) -> BodyMesh {
        let mut mesh = BodyMesh { vertices: Vec::with_capacity(4 * BODY_VERTEX_FLOATS), indices: vec![0, 1, 2, 0, 2, 3] };
        let centre = self.glass_centre();
        let right = self.right * self.half_width;
        let up = self.up * self.half_height;
        for (p, u, v) in [
            (centre - right - up, 0.0, 1.0), (centre + right - up, 1.0, 1.0),
            (centre + right + up, 1.0, 0.0), (centre - right + up, 0.0, 0.0),
        ] {
            mesh.vertices.extend_from_slice(&[p.x, p.y, p.z, u, self.dir.x, self.dir.y, self.dir.z, v]);
        }
        mesh
    }
}

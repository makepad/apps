//! The shell's shared state and the actions that change it.
//!
//! * **State flows one way.** Widgets read [`AppState`] through
//!   `scope.data.get_mut::<AppState>()` during draw/handle and change it
//!   only through the hot-path writes listed on [`AppState`] or by emitting a
//!   [`ShellAction`] with `cx.action(...)`. The app applies actions in
//!   `main.rs::App::dispatch` → [`AppState::apply_core`] plus each module's
//!   `apply` hook.
//! * **GPU handles stay on the UI thread.** The app owns the shared scene,
//!   and each viewport owns its camera passes.
//! * **Coordinates:** world is right-handed, Z up, meters. Screen/window
//!   coordinates are layout points, y down (`Rect`, `DVec2`). NDC is x right,
//!   y up, both −1..1.

use makepad_widgets_core::makepad_micro_serde::*;
use makepad_widgets_core::*;
pub use crate::ui::editors::Family;
pub use fab_elec_view::canvas::pane::SavedView2d;
pub use fab_elec_view::instruments::logic_view::ScopeView;
use crate::ui::editors::Editor;
use crate::ui::areas::WorkspaceOp;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

pub use crate::model::{
    aabb_center, aabb_empty, aabb_extent, aabb_is_empty, aabb_radius, aabb_union, Bvh,
    ElementId, Frustum, LoadError, LoadProgress, MaterialId, MeasureKind, Ray, RayHit,
    PoseSet, RenderBatch, Scene, SceneState, SectionPlane, SectionState, Selection, SnapHit, SnapKind,
    SnapOptions, Units, Vertex, VERTEX_STRIDE,
};
pub use crate::model::LengthUnit;
pub use crate::model::makepad_math::{Aabb, Plane};
pub use makepad_render::sky::{noaa_solar_position, SkyDate};

// ===========================================================================
// Camera
// ===========================================================================

/// How far along an orthographic view the floor is held at most (m), seen along the bed: as far
/// as the perspective view's far plane holds it.
const ORTHO_FLOOR_REACH: f32 = 50.0;

fn corners(b: &Aabb) -> [Vec3f; 8] {
    std::array::from_fn(|i| vec3(
        if i & 1 == 0 { b.min.x } else { b.max.x },
        if i & 2 == 0 { b.min.y } else { b.max.y },
        if i & 4 == 0 { b.min.z } else { b.max.z },
    ))
}

/// A look-at camera. `target` is the orbit pivot; `eye - target` is the
/// boom. Perspective uses `fov_y_deg`; orthographic uses `ortho_height`
/// (world meters visible vertically). The navigator changes it; the viewport,
/// the tools and the overlays read it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub eye: Vec3f,
    pub target: Vec3f,
    pub up: Vec3f,
    pub fov_y_deg: f32,
    pub ortho: bool,
    pub ortho_height: f32,
    pub near: f32,
    pub far: f32,
    /// Turned over: the world's up is -Z, so the bed shows on top (a resin printer's plate,
    /// its stack hanging from it). Orbiting keeps it, as it keeps +Z otherwise.
    pub hung: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            eye: vec3(0.45, -0.5, 0.35),
            target: vec3(0.0, 0.0, 0.02),
            up: vec3(0.0, 0.0, 1.0),
            fov_y_deg: 40.0,
            ortho: false,
            ortho_height: 0.5,
            near: 0.02,
            far: 4000.0,
            hung: false,
        }
    }
}

impl Camera {
    /// The world's up the camera keeps level: +Z, or -Z while it is hung.
    pub fn world_up(&self) -> Vec3f {
        vec3(0.0, 0.0, if self.hung { -1.0 } else { 1.0 })
    }

    /// The camera sees the bed plane (z = 0) from beneath: its eye is under it, or, orthographic
    /// (every ray parallel), it looks up. The view then leaves the floor out
    /// ([`AppState::floor_shown`]) and shows the print's underside.
    pub fn under_bed(&self) -> bool {
        if self.ortho {
            self.forward().z > 0.0
        } else {
            self.eye.z < 0.0
        }
    }

    pub fn forward(&self) -> Vec3f {
        (self.target - self.eye).normalize()
    }

    pub fn right(&self) -> Vec3f {
        Vec3f::cross(self.forward(), self.up).normalize()
    }

    /// Camera-space up (orthogonal to forward), not the world up hint.
    pub fn true_up(&self) -> Vec3f {
        Vec3f::cross(self.right(), self.forward()).normalize()
    }

    pub fn distance(&self) -> f32 {
        (self.target - self.eye).length()
    }

    pub fn view(&self) -> Mat4f {
        Mat4f::look_at(self.eye, self.target, self.up)
    }

    /// Clip z runs 0..w at the GPU on every backend. `Mat4f::perspective` reaches 0 at about twice
    /// `near` (it is -w at `near`), so that is the plane its picture is cut at; the orthographic
    /// depth runs 0..1 from `near` to `far`.
    pub fn projection(&self, aspect: f32) -> Mat4f {
        let aspect = aspect.max(1e-4);
        if self.ortho {
            let h = self.ortho_height.max(1e-3) * 0.5;
            let w = h * aspect;
            let (n, f) = (self.near, self.far);
            Mat4f {
                v: [
                    1.0 / w,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    1.0 / h,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    1.0 / (n - f),
                    0.0,
                    0.0,
                    0.0,
                    n / (n - f),
                    1.0,
                ],
            }
        } else {
            Mat4f::perspective(self.fov_y_deg, aspect, self.near, self.far)
        }
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4f {
        Mat4f::mul(&self.projection(aspect), &self.view())
    }

    /// Pick near/far from the scene bounds so depth precision follows the
    /// model. Call before `projection()` when the scene or eye changed. Orthographic, every ray
    /// is parallel and where the eye stands on the view's axis changes no pixel: it stands back
    /// behind all a view of `aspect` can show (the bounds, and the floor seen from above), so
    /// each ray starts behind what it can meet and near and far hold all of it, however close
    /// the view is zoomed in.
    pub fn fit_clip_planes(&mut self, bounds: &Aabb, aspect: f32) {
        if aabb_is_empty(bounds) {
            return;
        }
        if self.ortho {
            self.fit_ortho(bounds, aspect);
            return;
        }
        let r = aabb_radius(bounds).max(1.0);
        let d = (aabb_center(bounds) - self.eye).length();
        self.far = (d + r * 2.0).max(50.0);
        // Far off, the depth precision a far near plane gives; close to the scene, a plane close
        // enough never to cut what the camera is looking at from up to `MIN_DISTANCE` away.
        self.near = ((d - aabb_radius(bounds)) * 0.125).clamp(0.0005, (self.far * 1e-5).max(0.02).min(0.5));
    }

    /// [`Camera::fit_clip_planes`] orthographic: the depths along the view, from the look point,
    /// of the bounds' corners and of the floor (z = 0) where the view's corners meet it, the eye
    /// back far enough to see all of them in front of it. An eye already back far enough (and not
    /// absurdly far) stays where it is, and the planes are read from the camera as it then
    /// stands, so a view at rest keeps one camera frame after frame.
    fn fit_ortho(&mut self, bounds: &Aabb, aspect: f32) {
        let Some((lo, _, pad)) = self.ortho_reach(bounds, aspect) else { return };
        let need = (pad - lo).max(pad);
        let back = self.distance();
        if !(back >= need && back <= need * 2.0) {
            let eye = self.target - self.forward() * (need * 1.5);
            if eye.is_finite() {
                self.eye = eye;
            }
        }
        let Some((lo, hi, pad)) = self.ortho_reach(bounds, aspect) else { return };
        let back = self.distance();
        self.near = back + lo - pad * 0.5;
        self.far = back + hi + pad;
    }

    /// What an orthographic view of `aspect` holds along its axis, from the look point: the
    /// nearest and farthest depths of the bounds and the floor it shows, and a margin.
    fn ortho_reach(&self, bounds: &Aabb, aspect: f32) -> Option<(f32, f32, f32)> {
        let (f, r, u) = (self.forward(), self.right(), self.true_up());
        if !(f.is_finite() && r.is_finite() && u.is_finite()) {
            return None;
        }
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for bits in 0..8 {
            let c = vec3(
                if bits & 1 == 0 { bounds.min.x } else { bounds.max.x },
                if bits & 2 == 0 { bounds.min.y } else { bounds.max.y },
                if bits & 4 == 0 { bounds.min.z } else { bounds.max.z },
            );
            let d = (c - self.target).dot(f);
            (lo, hi) = (lo.min(d), hi.max(d));
        }
        // Seen from above, the floor runs past the bounds to the view's edges: toward the eye at
        // the bottom, away at the top, the farther the flatter the view (along the bed, as far
        // as perspective's far plane holds it).
        if f.z < -1e-3 {
            let h = self.ortho_height.max(1e-3) * 0.5;
            let w = h * aspect.max(1e-4);
            for (x, y) in [(-w, -h), (w, -h), (w, h), (-w, h)] {
                let o = self.target + r * x + u * y;
                let t = (-o.z / f.z).clamp(-ORTHO_FLOOR_REACH, ORTHO_FLOOR_REACH);
                (lo, hi) = (lo.min(t), hi.max(t));
            }
        }
        Some((lo, hi, ((hi - lo) * 0.05).max(self.ortho_height * 0.1).max(1e-3)))
    }

    /// Frame a bounding box: keeps the view direction, moves the eye so the
    /// bounding sphere fits the vertical field of view.
    pub fn frame_bounds(&mut self, bounds: &Aabb, aspect: f32) {
        if aabb_is_empty(bounds) {
            return;
        }
        let center = aabb_center(bounds);
        // The scene is in metres: a centimetre, the closest the near plane allows.
        let radius = aabb_radius(bounds).max(0.01);
        let dir = self.forward();
        let half_fov = (self.fov_y_deg.to_radians() * 0.5).max(0.01);
        let fit = if aspect < 1.0 {
            (half_fov.tan() * aspect).atan()
        } else {
            half_fov
        };
        let distance = radius / fit.sin() * 1.05;
        self.target = center;
        self.eye = center - dir * distance;
        self.ortho_height = radius * 2.1 / aspect.min(1.0);
        self.fit_clip_planes(bounds, aspect);
    }

    /// World-space ray through an NDC point, from the near plane: every ray the view's tools,
    /// picks and tiles cast comes from here (or the same matrices), so what a pixel shows is
    /// what a ray through it meets. Orthographic, the rays are parallel and the near plane lies
    /// behind all the view holds ([`Camera::fit_clip_planes`]).
    pub fn ray_at_ndc(&self, ndc: Vec2f, aspect: f32) -> Ray {
        let inv = self.view_projection(aspect).invert();
        let unproject = |z: f32| {
            let v = inv.transform_vec4(vec4(ndc.x, ndc.y, z, 1.0));
            let w = if v.w.abs() < 1e-12 { 1e-12 } else { v.w };
            vec3(v.x / w, v.y / w, v.z / w)
        };
        let a = unproject(if self.ortho { 0.0 } else { -1.0 });
        let b = unproject(1.0);
        Ray::new(a, b - a)
    }

    /// Project a world point to NDC. `None` when the point is not on screen
    /// depth-wise: behind the camera (perspective w test) or outside the
    /// near/far clip range (NDC z test — the only test that catches points
    /// behind an *orthographic* eye, where w stays 1; review minor).
    pub fn project(&self, p: Vec3f, aspect: f32) -> Option<Vec3f> {
        self.clip_to_ndc(self.view_projection(aspect).transform_vec4(vec4(p.x, p.y, p.z, 1.0)))
    }

    /// Whether the box's centre or a corner projects inside the viewport at positive depth.
    pub fn sees(&self, bounds: Aabb, aspect: f32) -> bool {
        if aabb_is_empty(&bounds) {
            return false;
        }
        let vp = self.view_projection(aspect);
        let visible = |p: Vec3f| {
            let c = vp.transform_vec4(vec4(p.x, p.y, p.z, 1.0));
            c.z > 0.0 && c.z <= c.w && c.x.abs() <= c.w && c.y.abs() <= c.w
        };
        visible(aabb_center(&bounds)) || corners(&bounds).into_iter().any(visible)
    }

    /// A clip-space point's NDC, None when it is not on screen depth-wise ([`Camera::project`]).
    fn clip_to_ndc(&self, v: Vec4f) -> Option<Vec3f> {
        if v.w <= 1e-6 {
            return None;
        }
        let ndc = vec3(v.x / v.w, v.y / v.w, v.z / v.w);
        if ndc.z < if self.ortho { 0.0 } else { -1.0 } || ndc.z > 1.0 {
            return None;
        }
        Some(ndc)
    }
}

/// Projection helper for anything drawn in the 2D pass over a viewport
/// (measurement labels, section handles, gizmos). Built by the viewport once
/// per frame from its camera and rect.
#[derive(Clone, Copy, Debug)]
pub struct ViewProjector {
    pub camera: Camera,
    pub rect: Rect,
    pub aspect: f32,
    pub view_proj: Mat4f,
}

impl ViewProjector {
    pub fn new(camera: Camera, rect: Rect) -> Self {
        let aspect = (rect.size.x / rect.size.y.max(1.0)) as f32;
        ViewProjector {
            camera,
            rect,
            aspect,
            view_proj: camera.view_projection(aspect),
        }
    }

    /// Window-space point (layout points) for a world point, or `None` when
    /// behind the camera or outside the clip depth range (same depth rules
    /// as [`Camera::project`] — ortho behind-the-eye included).
    pub fn project(&self, p: Vec3f) -> Option<DVec2> {
        let ndc = self.camera.clip_to_ndc(self.view_proj.transform_vec4(vec4(p.x, p.y, p.z, 1.0)))?;
        let (nx, ny) = (ndc.x as f64, ndc.y as f64);
        Some(dvec2(
            self.rect.pos.x + (nx * 0.5 + 0.5) * self.rect.size.x,
            self.rect.pos.y + (0.5 - ny * 0.5) * self.rect.size.y,
        ))
    }

    pub fn ndc(&self, screen: DVec2) -> Vec2f {
        let x = ((screen.x - self.rect.pos.x) / self.rect.size.x.max(1.0)) * 2.0 - 1.0;
        let y = 1.0 - ((screen.y - self.rect.pos.y) / self.rect.size.y.max(1.0)) * 2.0;
        vec2(x as f32, y as f32)
    }

    pub fn ray(&self, screen: DVec2) -> Ray {
        self.camera.ray_at_ndc(self.ndc(screen), self.aspect)
    }

    /// Screen-space size in points of one world meter at `p`.
    pub fn points_per_meter_at(&self, p: Vec3f) -> f64 {
        let r = self.camera.right();
        match (self.project(p), self.project(p + r)) {
            (Some(a), Some(b)) => (b - a).length(),
            _ => 0.0,
        }
    }
}

// ===========================================================================
// View modes
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Shading {
    /// Clay: one neutral material under a studio light, cavity on creases.
    Solid,
    /// The PBR view drawn by `makepad-render`: the parts' materials, sun,
    /// sky and shadows.
    #[default]
    Realtime,
}

impl Shading {
    pub const ALL: [Shading; 2] = [Shading::Solid, Shading::Realtime];

    pub fn label(self) -> &'static str {
        match self {
            Shading::Solid => "Solid",
            Shading::Realtime => "Realtime",
        }
    }

    /// As the view's preferences write it.
    pub fn key(self) -> &'static str {
        match self {
            Shading::Solid => "solid",
            Shading::Realtime => "realtime",
        }
    }

    pub fn from_key(key: &str) -> Option<Shading> {
        Shading::ALL.into_iter().find(|s| s.key() == key.trim())
    }
}

/// How parts with a distance field are drawn (View › Shading): by the SDF
/// engine from the field (the default), or as their evaluated mesh (the
/// mesh that export and the slicer use: a debug and export check).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PartLook {
    #[default]
    Field,
    Mesh,
}

impl PartLook {
    pub fn label(self) -> &'static str {
        match self {
            PartLook::Field => "Field",
            PartLook::Mesh => "Mesh",
        }
    }
}

/// How the desktop's Sliced view draws the beads a pixel or less across (View › Sliced Detail):
/// Fast draws each as one quad whose colour is the display's mean over the bead's front, within
/// a few levels of the bead drawn whole; Exact draws every bead whole, slower on a large print.
/// The headset draws its tiers either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SlicedDetail {
    #[default]
    Fast,
    Exact,
}

/// Exact correctly rounds every traced sample and is slower; Fast is the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TracedDetail {
    #[default]
    Fast,
    Exact,
}

/// A printed plastic's surface finish (the GGX roughness of a filament): satin, with a
/// highlight that reads under the studio key. Meshes and fields alike.
pub const FILAMENT_ROUGHNESS: f32 = 0.38;

/// What lights Realtime shading (View › Shading): either studio, or the dated sun and sky.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Lighting {
    /// A dark studio set whose gaps between the lights give metal its bright shapes and
    /// dark breaks, with a soft key that follows the camera.
    #[default]
    Studio,
    /// The same lights on a dim grey set with a broad source behind the camera that the
    /// board's coat mirrors into vertical metal faces, such as pins and shields.
    BrightStudio,
    /// The sun and sky of a place, date and time (`AppState::sun`).
    SunAndSky,
}

impl Lighting {
    pub fn label(self) -> &'static str {
        match self {
            Lighting::Studio => "Studio Lighting",
            Lighting::BrightStudio => "Bright Studio",
            Lighting::SunAndSky => "Sun and Sky",
        }
    }

    /// The name kept in the view preferences.
    pub fn key(self) -> &'static str {
        match self {
            Lighting::Studio => "studio",
            Lighting::BrightStudio => "bright",
            Lighting::SunAndSky => "sun",
        }
    }

    pub fn from_key(key: &str) -> Option<Lighting> {
        [Lighting::Studio, Lighting::BrightStudio, Lighting::SunAndSky].into_iter().find(|l| l.key() == key.trim())
    }

    pub fn is_studio(self) -> bool {
        matches!(self, Lighting::Studio | Lighting::BrightStudio)
    }
}

/// Viewport overlay toggles (View › Overlays in the ⋯ menu).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlays {
    pub grid: bool,
    pub axes: bool,
    pub outlines: bool,
    pub cavity: bool,
    pub section_planes: bool,
    pub measurements: bool,
    pub nav_gizmo: bool,
}

impl Default for Overlays {
    fn default() -> Self {
        Overlays {
            grid: true,
            axes: true,
            outlines: true,
            cavity: true,
            section_planes: true,
            measurements: true,
            nav_gizmo: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OrbitStyle {
    #[default]
    Turntable,
    Trackball,
}

/// Numpad-style preset views. `Isometric` is our extra.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerRon, DeRon)]
pub enum PresetView {
    Front,
    Back,
    Right,
    Left,
    Top,
    Bottom,
    Isometric,
}

impl PresetView {
    pub fn label(self) -> &'static str {
        match self {
            PresetView::Front => "Front",
            PresetView::Back => "Back",
            PresetView::Right => "Right",
            PresetView::Left => "Left",
            PresetView::Top => "Top",
            PresetView::Bottom => "Bottom",
            PresetView::Isometric => "Isometric",
        }
    }

    /// Direction the camera looks *along* (eye = target − dir·d) and the up
    /// hint, Z up world. Front looks along +Y (from −Y), as in Fab.
    pub fn look_dir_and_up(self) -> (Vec3f, Vec3f) {
        match self {
            PresetView::Front => (vec3(0.0, 1.0, 0.0), vec3(0.0, 0.0, 1.0)),
            PresetView::Back => (vec3(0.0, -1.0, 0.0), vec3(0.0, 0.0, 1.0)),
            PresetView::Right => (vec3(-1.0, 0.0, 0.0), vec3(0.0, 0.0, 1.0)),
            PresetView::Left => (vec3(1.0, 0.0, 0.0), vec3(0.0, 0.0, 1.0)),
            PresetView::Top => (vec3(0.0, 0.0, -1.0), vec3(0.0, 1.0, 0.0)),
            PresetView::Bottom => (vec3(0.0, 0.0, 1.0), vec3(0.0, -1.0, 0.0)),
            PresetView::Isometric => (vec3(-1.0, 1.0, -0.82).normalize(), vec3(0.0, 0.0, 1.0)),
        }
    }
}

/// The view tools: select (with the part gizmo), measure and section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Measure(MeasureKind),
    Section,
    /// The Sliced view's bed tool: a drag on the bed draws an area no tree may stand or grow
    /// in (a rectangle; with Alt a disc), `m.no_trees` in the nearest object's script.
    NoTrees,
    /// The Sliced view's layer tool: a drag on the shown layer draws a volume that prints with
    /// settings of its own (a box as tall as its shorter side; with Alt a sphere), `m.settings`
    /// in the nearest object's script.
    SettingsHere,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Measure(MeasureKind::Distance) => "Measure Distance",
            Tool::Measure(MeasureKind::Area) => "Measure Area",
            Tool::Measure(MeasureKind::Angle) => "Measure Angle",
            Tool::Measure(MeasureKind::EdgeLength) => "Measure Edge",
            Tool::Measure(MeasureKind::Radius) => "Measure Radius",
            Tool::Measure(MeasureKind::FaceDistance) => "Measure Faces",
            Tool::Section => "Section",
            Tool::NoTrees => "No trees here",
            Tool::SettingsHere => "Print settings here",
        }
    }
}

/// The plate view's state.
#[derive(Clone, Debug)]
pub struct ViewportState {
    /// Stable slots. Plate occupies 0; later Plate views and editors lease the lowest free slot.
    pub panes: Vec<Option<PaneState>>,
    released_panes: Rc<RefCell<Vec<PaneId>>>,
    /// Leases parked where no `AppState` reaches ([`PaneLease::park`]), drained with the released.
    parked_panes: Rc<RefCell<Vec<PaneId>>>,
    /// Mirror plane `n.xyz, d`, with nonzero `n`: `dot(n, p) + d = 0`, in Z-up fab metres.
    /// The plate writes the subject board's mid-plane, else the scene bounds' mid-plane,
    /// each frame before cameras derive. Defaults to the bed plane.
    pub mirror_plane: [f32; 4],
    pub orbit_style: OrbitStyle,
    /// How parts are lit and drawn: the person's choice, saved on this machine.
    pub shading: Shading,
    /// The parts' look: the person's choice, kept for the next session.
    pub look: PartLook,
    /// View > Drag Detail: how fine each version under the hand is, for every part.
    pub drag_detail: fab_sdf_tiles::trace::DragDetail,
    /// View > Sliced Detail: the person's choice, saved on this machine.
    pub sliced_detail: SlicedDetail,
    /// View > Traced Detail: the person's choice, kept for the next session.
    pub traced_detail: TracedDetail,
    /// View > Soldered: assembled joints in the realtime and traced electronics views.
    pub soldered: bool,
    /// What lights Realtime outside Electronics: the person's choice, saved on this machine.
    pub lighting: Lighting,
    /// Electronics' own lighting choice, saved independently of the other workspaces.
    pub board_lighting: Lighting,
    /// View > Overlays: the person's choice, saved on this machine.
    pub overlays: Overlays,
    pub xray: bool,
    /// View › See-Through.
    pub see: SeeThrough,
    /// View › Translucent materials: draw translucent part surfaces as glass.
    pub glass: bool,
    /// What is under the pointer right now (hot path: written by the viewport
    /// widget directly, read by everyone).
    pub hover: Option<RayHit>,
    /// The Plate pane last pressed ([`Self::active_plate`]).
    pub pressed_plate: PaneId,
    /// The panes of the Plate views on screen, by instance number: the first is the lead
    /// ([`Self::lead_plate`]). The shell publishes it with each change of what the dock shows.
    pub shown_plates: Vec<PaneId>,
    /// Each Plate view past the first: its instance number and the pane its stage leased.
    pub plate_instances: Vec<(u32, PaneId)>,
    /// The cameras the plate's view file gave Plate views past the first, by instance number:
    /// a view bound later starts at its own ([`PlateView::plates`]).
    pub saved_plates: Vec<(u32, Camera, Option<PresetView>)>,
}

impl Default for ViewportState {
    fn default() -> Self {
        ViewportState {
            panes: vec![Some(PaneState::default())],
            released_panes: Rc::default(),
            parked_panes: Rc::default(),
            mirror_plane: [0.0, 0.0, 1.0, 0.0],
            orbit_style: OrbitStyle::Turntable,
            shading: Shading::Realtime,
            look: crate::view_prefs::default_look(),
            drag_detail: crate::view_prefs::load_drag_detail(),
            sliced_detail: crate::view_prefs::load_sliced_detail(),
            traced_detail: crate::view_prefs::load_traced_detail(),
            soldered: crate::view_prefs::load_soldered(),
            lighting: Lighting::default(),
            board_lighting: Lighting::BrightStudio,
            overlays: Overlays::default(),
            xray: false,
            see: SeeThrough::default(),
            glass: true,
            hover: None,
            pressed_plate: PaneId::PLATE_ZERO,
            shown_plates: Vec::new(),
            plate_instances: Vec::new(),
            saved_plates: Vec::new(),
        }
    }
}

/// View › See-Through: each field part's paint as a hologram through its box, each material in
/// its filament colour, the part's surface a thin shell in it, so the pattern the shape cuts
/// through shows, inside the part and around it (`viewport::see_through`).
#[derive(Clone, Debug, PartialEq)]
pub struct SeeThrough {
    pub on: bool,
    /// How much the part's shell and a millimetre of its inside hide what is behind them: 0 a
    /// ghost, 1 solid.
    pub opacity: f32,
    /// How brightly the paint glows outside the part: 0 not at all.
    pub glow: f32,
    /// The parts drawn so now (their grids are ready): the viewport writes it, and the plate
    /// view's other passes leave them out.
    pub drawn: Vec<ElementId>,
}

impl Default for SeeThrough {
    fn default() -> Self {
        SeeThrough { on: false, opacity: 0.35, glow: 0.35, drawn: Vec::new() }
    }
}

impl ViewportState {
    /// The lighting the window's 3D views draw in. One sky and one studio serve every pane, so
    /// one lighting draws at a time: the plate's, or the electronics' own choice for their 3D
    /// view while it shows without a Plate ([`UiState::board_lit`]). O(1).
    pub fn lighting_for(&self, ui: &UiState) -> Lighting {
        if ui.board_lit() { self.board_lighting } else { self.lighting }
    }

    /// A camera moved away from its preset view.
    pub fn mark_camera_changed(&mut self, pane: PaneId) {
        self.pane_mut(pane).preset = None;
    }

    /// The camera of `pane`. Every reader goes through here; an invalid id panics.
    pub fn camera(&self, pane: PaneId) -> &Camera {
        &self.pane(pane).camera
    }

    /// The camera of `pane`, to move it; an invalid id panics.
    pub fn camera_mut(&mut self, pane: PaneId) -> &mut Camera {
        &mut self.pane_mut(pane).camera
    }
}

/// A pane of the plate view. Pane 0 is Plate 0's for good. Code names the pane it means: its
/// own (`self.pane`), the active Plate ([`ViewportState::active_plate`]: commands without a
/// pointer), the lead Plate ([`ViewportState::lead_plate`]: one-per-window work and automatic
/// cameras) or a role ([`PaneRole`]); `PRIMARY` is for tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct PaneId(pub u16);

impl PaneId {
    #[cfg(test)]
    pub const PRIMARY: PaneId = PaneId(0);
    /// Plate 0's pane, held for good: never released. Not "the plate view": name the own,
    /// active or lead pane, or a role.
    pub(crate) const PLATE_ZERO: PaneId = PaneId(0);
}

/// What a pane is for, which decides the features it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PaneRole {
    /// A Plate view: tools, markers, branches and the force tip.
    #[default]
    Plate,
    /// The Device editor's view.
    Device,
}

/// Held by an editor for its lifetime, including while its viewport is parked.
#[derive(Debug)]
pub struct PaneLease {
    id: PaneId,
    role: PaneRole,
    released: Rc<RefCell<Vec<PaneId>>>,
    parked: Rc<RefCell<Vec<PaneId>>>,
}

impl PaneLease {
    pub fn id(&self) -> PaneId {
        self.id
    }

    pub fn role(&self) -> PaneRole {
        self.role
    }

    /// Park the pane where no `AppState` reaches (a layout switch's hook): `AppState` parks it
    /// ([`AppState::park_pane`]) before the next event, unless it draws first.
    pub fn park(&self) {
        self.parked.borrow_mut().push(self.id);
    }
}

impl Drop for PaneLease {
    fn drop(&mut self) {
        self.released.borrow_mut().push(self.id);
    }
}

/// A pose set the app keeps in [`AppState::pose_sets`]: its index there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoseSetId(pub u16);

impl PoseSetId {
    /// The device's: assembled, as a kit, or a frame of the animation between
    /// ([`AppState::device`]). Every 3D editor of Electronics shows it.
    pub const DEVICE: PoseSetId = PoseSetId(0);
}

/// The device the Electronics 3D editors show.
#[derive(Clone, Debug, Default)]
pub struct DeviceView {
    /// The Device editor's leased viewport.
    pub pane: Option<PaneId>,
    /// The Device camera has been placed by the person using it.
    pub placed: bool,
    /// The opening waiting for the Device editor to draw.
    pub restore: Option<DeviceRestore>,
    /// As a kit: every part where it prints. Else assembled; the change animates between.
    pub kit: bool,
    /// Stopped at a build step (the build instructions): the plan's steps up to and including it
    /// put on. None shows the whole kit or the whole device ([`DeviceView::kit`]).
    pub step: Option<usize>,
    /// The plan's steps in order, by name (`Base`, `Board`, a shell's role): what the step
    /// controls show. Written by the app's device assembly; empty without a case.
    pub steps: Arc<[String]>,
    /// Where the device stands, from 0 (the kit) to 1 (assembled), between while it moves. Written
    /// by the app's device assembly; the Device view's own framing follows it.
    pub position: f64,
    /// The case's own parts (its shells, keycaps, knobs and light pipes), which a pane's Case
    /// toggle hides ([`PaneState::case_hidden`]). Written by the app's device assembly.
    pub case: Arc<std::collections::HashSet<ElementId>>,
}

#[derive(Clone, Copy, Debug)]
pub enum DeviceRestore {
    Frame,
    Camera(Camera),
}

/// The poses a pane shows its parts at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanePoses {
    /// The stage's ([`SceneState::pose`]): Mechanics' moves, the placement preview, a resin lift.
    #[default]
    Stage,
    /// A set the app owns, such as the device assembled or as a kit; parts it leaves out rest.
    Set(PoseSetId),
}

/// How to derive a camera from its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkHow {
    /// Copies the source camera.
    Synced,
    /// Reflects the source camera through the mirror plane to see the underside from below.
    Flipped,
    /// Uses Flipped's camera, with its picture mirrored left-right when shown by a later blit.
    Through,
}

impl LinkHow {
    fn transform(self, camera: Camera, plane: [f32; 4]) -> Camera {
        match self {
            Self::Synced => camera,
            Self::Flipped | Self::Through => reflect_camera(camera, plane),
        }
    }
}

/// A pane's relationship to its camera source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaneLink {
    /// Has its own camera.
    #[default]
    Free,
    /// Follows the camera of a Free Plate pane.
    Linked { how: LinkHow, to: PaneId },
}

impl PaneLink {
    /// How this pane follows its source, if linked.
    pub fn how(&self) -> Option<LinkHow> {
        match *self {
            Self::Free => None,
            Self::Linked { how, .. } => Some(how),
        }
    }

    /// The Plate whose camera this pane follows.
    pub fn source(&self) -> Option<PaneId> {
        match *self {
            Self::Free => None,
            Self::Linked { to, .. } => Some(to),
        }
    }
}

/// How a plate pane makes its picture, independently of its camera link.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaneMode {
    #[default]
    Raster,
    Traced,
}

/// Throttled, allocation-free publication from the pane to its chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceProgress {
    pub samples: u32,
    pub target: u32,
    pub stale: bool,
    pub fields_not_traced: bool,
    pub bodies_not_traced: bool,
    pub unavailable: bool,
}

impl Default for TraceProgress {
    fn default() -> Self {
        Self {
            samples: 0,
            target: 256,
            stale: false,
            fields_not_traced: false,
            bodies_not_traced: false,
            unavailable: false,
        }
    }
}

/// One pane's camera, link and display settings.
#[derive(Clone, Debug, Default)]
pub struct PaneState {
    pub camera: Camera,
    /// Preset the camera is aligned to, cleared by an orbit.
    pub preset: Option<PresetView>,
    /// This Plate's camera and preset in the other mode. Monitor starts at the Plate's current view.
    pub hidden_view: Option<(Camera, Option<PresetView>)>,
    pub role: PaneRole,
    pub link: PaneLink,
    pub mode: PaneMode,
    pub trace: TraceProgress,
    /// What its parts show at.
    pub poses: PanePoses,
    /// Leaves out the device's case parts ([`DeviceView::case`]): its Case toggle off.
    pub case_hidden: bool,
    /// Parked ([`AppState::park_pane`]) and not drawn since; an editor's pane starts so.
    pub(crate) parked: bool,
    /// Published by the navigator/viewport; every tracer yields to every pane.
    pub(crate) interacting: bool,
    /// Includes presses captured by a tool overlay above the viewport widget.
    pub(crate) pointer_held: bool,
}

impl PaneState {
    /// A saved camera and preset back on this pane: Design's, kept aside while Monitor shows its
    /// own (`monitor`), or the one it shows.
    pub fn restore_camera(&mut self, camera: Camera, preset: Option<PresetView>, monitor: bool) {
        match &mut self.hidden_view {
            Some(design) if monitor => *design = (camera, preset),
            _ => {
                self.camera = camera;
                self.preset = preset;
            }
        }
    }
}

impl ViewportState {
    pub(crate) fn pane(&self, id: PaneId) -> &PaneState {
        self.panes[id.0 as usize].as_ref().expect("vacant pane slot")
    }

    pub(crate) fn pane_mut(&mut self, id: PaneId) -> &mut PaneState {
        self.panes[id.0 as usize].as_mut().expect("vacant pane slot")
    }

    pub fn pane_count(&self) -> usize {
        self.panes.iter().flatten().count()
    }

    /// The Plate that commands without a pointer act on (presets, Frame All, the View menu):
    /// the Plate last pressed while it is on screen, else the lead. O(Plates on screen).
    pub fn active_plate(&self) -> PaneId {
        if self.shown_plates.contains(&self.pressed_plate) { self.pressed_plate } else { self.lead_plate() }
    }

    /// The Plate that does one-per-window work, shows the one-per-window chrome (the slice bars,
    /// the job card, the loading card) and drives the automatic cameras (the follow camera, the
    /// saved picture, scan): the lowest Plate on screen, Plate 0 while none is. Only a change of
    /// what the dock shows changes it. O(1).
    pub fn lead_plate(&self) -> PaneId {
        self.shown_plates.first().copied().unwrap_or(PaneId::PLATE_ZERO)
    }

    /// The pane that shows Plate view `n` (Plate 0's is pane 0), while it is bound. O(Plates).
    pub fn plate_pane(&self, n: u32) -> Option<PaneId> {
        if n == 0 { return Some(PaneId::PLATE_ZERO); }
        self.plate_instances.iter().find(|(m, _)| *m == n).map(|(_, pane)| *pane)
    }

    /// The instance number of the Plate view `pane` shows, while it is bound. O(Plates).
    pub fn plate_number(&self, pane: PaneId) -> Option<u32> {
        if pane == PaneId::PLATE_ZERO { return Some(0); }
        self.plate_instances.iter().find(|(_, p)| *p == pane).map(|(n, _)| *n)
    }

    /// The pane whose camera `pane` shows: the one it follows, else itself (every link names
    /// its root). O(1).
    pub fn camera_root(&self, pane: PaneId) -> PaneId {
        self.panes.get(pane.0 as usize).and_then(Option::as_ref).and_then(|p| p.link.source()).unwrap_or(pane)
    }

    /// `a` and `b` are two views of one camera: one follows the other or both follow one. O(1).
    pub fn same_camera(&self, a: PaneId, b: PaneId) -> bool {
        a != b && self.camera_root(a) == self.camera_root(b)
    }

    pub(crate) fn has_followers(&self, pane: PaneId) -> bool {
        self.panes.iter().flatten().any(|p| p.link.source() == Some(pane))
    }

    /// Every Plate view's pane: what a new document or a load resets.
    pub fn plate_panes(&self) -> Vec<PaneId> {
        self.panes.iter().enumerate()
            .filter(|(_, p)| p.as_ref().is_some_and(|p| p.role == PaneRole::Plate))
            .map(|(i, _)| PaneId(i as u16)).collect()
    }

    fn add_pane(&mut self, link: PaneLink) -> PaneId {
        let index = self.panes[1..].iter().position(Option::is_none)
            .map_or(self.panes.len(), |offset| 1 + offset);
        self.insert_pane(index, link)
    }

    fn insert_pane(&mut self, index: usize, link: PaneLink) -> PaneId {
        let id = PaneId(u16::try_from(index).expect("pane count exceeds PaneId range"));
        let link = self.resolve_link(id, link).unwrap_or(PaneLink::Free);
        let camera = *self.camera(link.source().unwrap_or_else(|| self.active_plate()));
        let pane = Some(PaneState { camera, link, ..Default::default() });
        if index == self.panes.len() {
            self.panes.push(pane);
        } else {
            self.panes[index] = pane;
        }
        id
    }

    /// A link goes directly to a Free Plate, including when the requested Plate follows one.
    fn resolve_link(&self, pane: PaneId, link: PaneLink) -> Option<PaneLink> {
        let PaneLink::Linked { how, to } = link else { return Some(PaneLink::Free) };
        let target = self.panes.get(to.0 as usize)?.as_ref()?;
        if target.role != PaneRole::Plate || to == pane {
            return None;
        }
        let source = target.link.source().unwrap_or(to);
        (source != pane).then_some(PaneLink::Linked { how, to: source })
    }

    /// Every link names its root, so one pass works in any slot order, even with a parked root.
    pub fn settle_cameras(&mut self) {
        for index in 0..self.panes.len() {
            if let Some(PaneLink::Linked { how, to }) = self.panes[index].as_ref().map(|p| p.link) {
                let camera = how.transform(*self.camera(to), self.mirror_plane);
                *self.camera_mut(PaneId(index as u16)) = camera;
            }
        }
    }

    /// Carry navigation back to the source. Each link's transform is its own inverse.
    pub fn write_back(&mut self, pane: PaneId) {
        let pane = self.pane(pane);
        if let PaneLink::Linked { how, to } = pane.link {
            let camera = how.transform(pane.camera, self.mirror_plane);
            *self.camera_mut(to) = camera;
        }
    }
}

fn reflect_point(point: Vec3f, plane: [f32; 4]) -> Vec3f {
    let n = vec3(plane[0], plane[1], plane[2]);
    point - n * (2.0 * (n.dot(point) + plane[3]) / n.dot(n))
}

fn reflect_direction(direction: Vec3f, plane: [f32; 4]) -> Vec3f {
    let n = vec3(plane[0], plane[1], plane[2]);
    direction - n * (2.0 * n.dot(direction) / n.dot(n))
}

fn reflect_camera(camera: Camera, plane: [f32; 4]) -> Camera {
    Camera {
        eye: reflect_point(camera.eye, plane),
        target: reflect_point(camera.target, plane),
        up: reflect_direction(camera.up, plane),
        ..camera
    }
}

// ===========================================================================
// Sky, load, ui
// ===========================================================================

/// The sun and sky that light the realtime view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyState {
    pub date: SkyDate,
    pub time_local: f32,
    /// Effective civil offset in hours east of UTC, including DST.
    pub tz_offset: f32,
    pub latitude: f32,
    pub longitude: f32,
    /// North clockwise from model +Y, in degrees.
    pub north_deg: f32,
    pub turbidity: f32,
    /// Distance-haze amount, 0 = off and 1 = the renderer's full outdoor
    /// fog density.
    pub haze: f32,
    /// User compensation applied by the engine sky model.
    pub exposure_ev: f32,
}

impl Default for SkyState {
    fn default() -> Self {
        Self {
            date: SkyDate::default(),
            time_local: 14.0,
            tz_offset: 2.0,
            latitude: 52.37,
            longitude: 4.9,
            north_deg: 0.0,
            turbidity: 2.5,
            haze: 0.35,
            exposure_ev: 0.0,
        }
    }
}

impl SkyState {
    /// How much brighter the sun is than the sky it hangs in, at full
    /// daylight — see [`makepad_scene::SunConfig::daylight_balance`].
    ///
    /// The engine's stock split is about 2.6:1, a soft key that suits a
    /// stylised world and reads as a bright overcast day: shadows fill in and
    /// a part's faces flatten into one grey. Measured clear daylight is
    /// nothing like that (diffuse horizontal illuminance is roughly a tenth
    /// of the global), and the plate reads best under that clear sky: a
    /// sunlit top face clearly brighter than the side in its shadow.
    pub const DAYLIGHT_BALANCE: f32 = 9.0;

    pub fn solar_position(&self) -> (f32, f32) {
        noaa_solar_position(
            self.date,
            self.time_local,
            self.tz_offset,
            self.latitude,
            self.longitude,
        )
    }

    /// Unit vector toward the sun in Fab's Z-up coordinates.
    pub fn direction(&self) -> Vec3f {
        let (elevation, azimuth) = self.solar_position();
        let elevation = elevation.to_radians();
        let azimuth = (azimuth + self.north_deg).to_radians();
        let horizontal = elevation.cos();
        vec3(
            horizontal * azimuth.sin(),
            horizontal * azimuth.cos(),
            elevation.sin(),
        )
        .normalize()
    }

    pub fn elevation_deg(&self) -> f32 {
        self.solar_position().0
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum LoadStatus {
    #[default]
    Idle,
    Loading {
        path: PathBuf,
        progress: LoadProgress,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
    Loaded {
        path: Option<PathBuf>,
    },
}

/// A page of the Model sources editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourcesPage {
    /// The sources and their switches.
    #[default]
    Settings,
    /// EasyEDA's warning, shown before it turns on.
    EasyEdaWarning,
    /// Every external resource Fab uses: what, from where, under which licence.
    Licences,
}

/// What the plate is being worked in (Blender's object modes): which sim runs, whose camera the
/// plate view looks through, whether the slice shows and which tools edit. It acts while a Plate
/// editor is on screen ([`UiState::mode`]); a workspace without one keeps it for its return.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash, SerRon, DeRon)]
pub enum PlateMode {
    /// Parts, features, the build, the plate and its print settings; the slice is a render mode.
    #[default]
    Design,
    /// The assembled piece live in a physics sim; parts are grabbed, not edited.
    Mechanics,
    /// The printer's own camera and job: the plate shows what the printer prints.
    Monitor,
}

/// A workspace is one saved arrangement of the editors a stage of the job needs. Switching it
/// never touches the document; each remembers the user's own dock changes. One may pin a plate
/// mode ([`Workspace::pinned_mode`]): choosing it puts the plate in that mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash, SerRon, DeRon)]
pub enum Workspace {
    /// Everything up to the print: parts, parametric features, the AI build, the plate, its
    /// print settings. The slice is a render mode of this view ([`RenderMode::Slice`]).
    #[default]
    Design,
    /// The assembled piece live: joints move in a physics sim, parts can be grabbed and thrown.
    Mechanics,
    /// The printer: card, trays, camera, job progress.
    Monitor,
    /// Arm cells (experimental): metal built and trimmed by arms in a virtual cell, the loop
    /// that corrects them, and the physics checks.
    Arm,
    /// A headset scan as it happens: the capture's state, each keyframe where it was taken from,
    /// the surface growing under them.
    Scan,
    /// Electronics (experimental): circuits, boards and their checks, live on the plate.
    Electronics,
}

impl Workspace {
    pub const ALL: [Workspace; 6] = [Workspace::Design, Workspace::Mechanics, Workspace::Monitor, Workspace::Arm, Workspace::Scan, Workspace::Electronics];

    pub fn label(self) -> &'static str {
        match self {
            Workspace::Design => "Design",
            Workspace::Monitor => "Monitor",
            Workspace::Mechanics => "Mechanics",
            Workspace::Arm => "Arm",
            Workspace::Scan => "Scan",
            Workspace::Electronics => "Electronics",
        }
    }

    pub fn index(self) -> usize {
        Workspace::ALL.iter().position(|w| *w == self).unwrap_or(0)
    }

    /// Shown only when View › Experimental turns it on ([`AppState::workspace_shown`]).
    pub fn experimental(self) -> bool {
        matches!(self, Workspace::Arm | Workspace::Scan)
    }

    /// The family a workspace of this kind holds: the plate's for the modes' kinds.
    pub fn family(self) -> Family {
        match self {
            Workspace::Design | Workspace::Mechanics | Workspace::Monitor => Family::Plate,
            Workspace::Arm => Family::Arm,
            Workspace::Scan => Family::Scan,
            Workspace::Electronics => Family::Electronics,
        }
    }

    /// The plate mode choosing this workspace puts the plate in: Design, Mechanics and Monitor
    /// pin their own; Arm, Scan and Electronics pin none (the plate keeps its mode).
    pub fn pinned_mode(self) -> Option<PlateMode> {
        match self {
            Workspace::Design => Some(PlateMode::Design),
            Workspace::Mechanics => Some(PlateMode::Mechanics),
            Workspace::Monitor => Some(PlateMode::Monitor),
            Workspace::Arm | Workspace::Scan | Workspace::Electronics => None,
        }
    }

    /// Whether this workspace's layout holds an editor of `family`. Each workspace's layout
    /// fixes its families: the Electronics areas hold only electronics editors and its 3D view,
    /// under the recorder that always shows. O(1).
    pub fn shows(self, family: Family) -> bool {
        match family {
            Family::Plate => matches!(self, Workspace::Design | Workspace::Mechanics | Workspace::Monitor),
            Family::Arm => self == Workspace::Arm,
            Family::Scan => self == Workspace::Scan,
            Family::Electronics => self == Workspace::Electronics,
        }
    }

    /// The plate mode in force once this workspace is chosen while the plate is in `mode`: its
    /// pinned mode, else `mode`; none without a Plate editor on screen. O(1).
    pub fn mode_from(self, mode: PlateMode) -> Option<PlateMode> {
        self.shows(Family::Plate).then(|| self.pinned_mode().unwrap_or(mode))
    }
}

/// How the plate view draws the plate: one control (View › Shading and the view's own switch).
/// The state behind it stays where its readers are: `view.look`, `print.finish`,
/// `print.play.active` and `force.active`; [`AppState::render_mode`] reads them as one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    Field,
    Mesh,
    /// The sliced print over the plate: contours and toolpaths, layer and move sliders.
    Slice,
    /// The studied part's stress over the plate (Design only): the real part in the study's
    /// colours, its fixtures and loads in place, every edit studied again.
    Force,
}

impl RenderMode {
    pub const ALL: [RenderMode; 4] = [RenderMode::Field, RenderMode::Mesh, RenderMode::Slice, RenderMode::Force];

    pub fn label(self) -> &'static str {
        match self {
            RenderMode::Field => "Field",
            RenderMode::Mesh => "Mesh",
            RenderMode::Slice => "Slice",
            RenderMode::Force => "Force",
        }
    }
}

/// The editor that shows a notice. Design and Slice share the plate view, Circuit is the
/// schematic, and Device is the 3D view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoticeView {
    Plate,
    Board,
    Circuit,
    Device,
    Scope,
}

/// A refusal, a problem, or a step for the person to take in a view.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub view: NoticeView,
    pub text: String,
    /// A standing problem stays until its source replaces or clears it.
    pub standing: bool,
    /// Wall-clock time when the view said this. A press only clears an earlier passing notice.
    pub at: f64,
}

/// The notices the views show now, one per view at most.
#[derive(Clone, Debug, Default)]
pub struct Notices {
    list: Vec<Notice>,
    /// Changes whenever a notice is added, replaced or cleared.
    pub revision: u64,
}

impl Notices {
    pub fn say(&mut self, view: NoticeView, text: String) {
        self.set(view, text, false);
    }

    pub fn stand(&mut self, view: NoticeView, text: String) {
        self.set(view, text, true);
    }

    fn set(&mut self, view: NoticeView, text: String, standing: bool) {
        if text.is_empty() {
            self.clear(view);
            return;
        }
        let notice = Notice { view, text, standing, at: Cx::time_now() };
        if let Some(old) = self.list.iter_mut().find(|n| n.view == view) {
            if *old == notice || (standing && old.standing && old.text == notice.text) {
                return;
            }
            *old = notice;
        } else {
            self.list.push(notice);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn clear(&mut self, view: NoticeView) {
        if let Some(index) = self.list.iter().position(|n| n.view == view) {
            self.list.remove(index);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn clear_passing(&mut self, view: NoticeView, before: f64) {
        if self.get(view).is_some_and(|n| !n.standing && n.at < before) {
            self.clear(view);
        }
    }

    pub fn get(&self, view: NoticeView) -> Option<&Notice> {
        self.list.iter().find(|n| n.view == view)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Notice> {
        self.list.iter()
    }
}

/// UI chrome state. Everything a panel needs to draw itself.
#[derive(Clone, Debug)]
pub struct UiState {
    /// The caption's workspaces and the one shown ([`Self::kind`] is what it is).
    /// Edit > Undo the AI's Last Turn: the presses of Undo that take the AI's last turn back,
    /// while its steps are the newest (None: not offered).
    pub undo_turn: Option<usize>,
    pub workspaces: crate::workspaces::Workspaces,
    /// The plate's mode, kept while no Plate editor shows ([`Self::mode`] is the one in force).
    pub plate_mode: PlateMode,
    /// The editors the dock shows now, a bit per [`Editor`] (`1 << editor as u64`): the shell
    /// writes it when a layout loads or a tab comes forward. Every bit until it does, so a panel
    /// alone (in a test) is on screen.
    pub screen: u64,
    /// The slice render mode to come back to from Mechanics (whose view is the sim's).
    pub slice_after_mechanics: bool,
    /// The override row last clicked (`crate::ui::overrides::Override::id`): a setting's row is
    /// lit, in the Overrides editor and the Parts list, while its object stays selected.
    pub picked_override: Option<String>,
    /// The Strength editor's state.
    pub strength: crate::ui::strength::StrengthView,
    /// A test print's box being dragged or typed, not committed yet: the views show it there.
    pub test_drag: Option<TestDrag>,
    /// Files from outside dragged over the window: the chip riding the pointer says what their
    /// drop does (the shell draws it while the drag lasts).
    pub drop_chip: Option<DropChip>,
    /// The experimental Arm workspace is offered: its tab, its shortcut and its row on the
    /// wrist (View › Experimental › Arm, kept on this machine; off by default).
    pub arm_shown: bool,
    /// The experimental Scan workspaces are offered: their tabs, shortcuts and rows on the
    /// wrist (View › Experimental › Scan, kept on this machine; off by default).
    pub scan_shown: bool,
    /// Follow AI (View menu): an edit from outside the app that no editor on screen shows brings
    /// the first workspace that shows it forward. Off at each start.
    pub follow_ai: bool,
    /// The Scan view draws the scan in the cameras' colours (the Scan panel's Colour; kept on this machine; on by default).
    pub scan_colour: bool,
    /// The Scan view marks where the keyframes were taken from (the Scan panel's Cameras; kept on this machine; on by default).
    pub scan_cameras: bool,
    /// Where Delete deletes parts: each Plate view's area (its viewport's or its overlay's, by
    /// pane) and the parts list's, when they took the keys. Owners refresh these references
    /// after every redraw.
    pub plate_keys: Vec<Area>,
    pub parts_key: Area,
    /// The Plate views the dock holds but does not show, by tab: their stages hear only what
    /// keeps their viewports whole. Published with `screen`.
    pub plates_hidden: Vec<LiveId>,
    /// What the editors currently tell the person.
    pub notices: Notices,
    pub command_palette_open: bool,
    pub keymap_help_open: bool,
    pub about_open: bool,
    /// The page the Model sources editor shows.
    pub sources_page: SourcesPage,
    /// An editor shown alone over the workspace (Edit › 3D Model Sources…), until Back or
    /// another workspace; the workspace's areas are not changed by it.
    pub page: Option<Editor>,
    /// The open document's name and a short line about it (parts, build).
    pub document_name: String,
    pub document_info: String,
    /// The nozzle-zone overlay on the plate (each nozzle's reach, keep-outs,
    /// ceilings): off by default, on while its chip is toggled.
    pub show_zones: bool,
    /// The printer camera in the view's corner.
    pub camera_pip: bool,
    pub gizmo: GizmoMode,
    pub paint_radius: f64,
    pub paint_color: [f32; 3],
    /// The print colour the brush paints in (`None`: a new colour of `paint_color`).
    pub paint_name: Option<String>,
    /// What a press of the paint tool does.
    pub paint_mode: PaintMode,
    /// The smooth fill's largest angle between neighbouring normals (degrees).
    pub paint_angle: f64,
    /// The brush and fills paint the part's own colour back (Erase). Holding Alt flips it while held.
    pub paint_erase: bool,
    /// The marker is on (the chat's Annotate): a drag on the view draws a mark for the AI.
    pub marker: bool,
    /// What the paint tool paints: the colour, or where supports never or always go.
    pub paint_target: PaintTarget,
    /// The pattern a Pattern fill writes.
    pub paint_pattern: PaintPattern,
    /// Lay flat is armed: the next click on a part turns that surface down onto the bed.
    pub lay_flat: bool,
    pub sculpt_op: fab_paint_field::StrokeOp,
    pub sculpt_radius: f64,
    pub sculpt_strength: f64,
    /// The Part panel's Scale keeps its proportions: an edit of one axis scales all three by the same factor.
    pub scale_locked: bool,
    /// What a click of the Force tool adds.
    pub force_kind: ForceKind,
    /// The size a new load of each kind takes (by `ForceKind as usize`; a fixture has none):
    /// the Force strip's dial sets it.
    pub force_size: [f64; 4],
}

impl UiState {
    /// The plate mode in force: the plate's while a Plate editor is on screen, else none (Arm,
    /// Scan and Electronics show no plate: no mode's sim runs, and their 3D views show the model
    /// as it is). O(1).
    pub fn mode(&self) -> Option<PlateMode> {
        self.shows(Family::Plate).then_some(self.plate_mode)
    }

    /// The kind of the workspace shown (what a seek of it would find).
    pub fn kind(&self) -> Workspace {
        self.workspaces.current().kind()
    }

    /// The shown workspace holds the Program (the code toggle is on). O(tabs).
    pub fn code_shown(&self) -> bool {
        self.workspaces.current().tree.any_instance(&|i| i.editor == Editor::Code)
    }

    /// The workspace is offered: one holding an experimental family only while View ›
    /// Experimental has that family on. O(areas).
    pub fn offered(&self, def: &crate::workspaces::WorkspaceDef) -> bool {
        !(def.shows(Family::Arm) && !self.arm_shown) && !(def.shows(Family::Scan) && !self.scan_shown)
    }

    /// The offered workspaces' ids in the caption's order. O(workspaces x areas).
    pub fn offered_ids(&self) -> Vec<crate::workspaces::WorkspaceId> {
        self.workspaces.list.iter().filter(|d| self.offered(d)).map(|d| d.id).collect()
    }

    /// The offered workspace `by` places along the caption from the shown one, round the ends
    /// (the temporary layout's tab last, as the row has it).
    pub fn step_workspace(&self, by: isize) -> Option<crate::workspaces::WorkspaceId> {
        let mut ids = self.offered_ids();
        ids.extend(self.workspaces.temp_id());
        let at = ids.iter().position(|id| *id == self.workspaces.current)?;
        Some(ids[(at as isize + by).rem_euclid(ids.len() as isize) as usize])
    }

    /// The first offered workspace that answers `kind` ([`crate::workspaces::Workspaces::seek`]).
    pub fn seek(&self, kind: Workspace) -> Option<crate::workspaces::WorkspaceId> {
        self.workspaces.seek(kind, |d| self.offered(d))
    }

    /// The dock shows `editor` now ([`Self::screen`]): an editor off screen skips the work of
    /// its own controls, which nobody can press. O(1).
    pub fn on_screen(&self, editor: Editor) -> bool {
        self.screen & (1 << editor as u64) != 0
    }

    /// The plate mode in force is `mode` ([`Self::mode`]).
    pub fn in_mode(&self, mode: PlateMode) -> bool {
        self.mode() == Some(mode)
    }

    /// An editor of `family` is on screen. O(1).
    pub fn shows(&self, family: Family) -> bool {
        self.workspaces.current().shows(family)
    }

    /// The 3D views on screen draw in the electronics' lighting: theirs show and no Plate does
    /// ([`ViewportState::lighting_for`]). O(1).
    pub fn board_lit(&self) -> bool {
        self.shows(Family::Electronics) && !self.shows(Family::Plate)
    }

    /// Show the first workspace that answers `kind` and put the plate in the mode it pins, nothing
    /// else ([`ShellAction::SetWorkspace`] also moves what follows the mode). None answering: a
    /// mode's kind sets the mode on the plate on screen, if one is; otherwise, when no workspace
    /// at all answers it (the person deleted them), its default is added at the end of the row and
    /// shown.
    pub fn enter(&mut self, kind: Workspace) {
        if let Some(id) = self.seek(kind) {
            self.enter_id(id);
        } else if let (Some(mode), true) = (kind.pinned_mode(), self.shows(Family::Plate)) {
            self.plate_mode = mode;
        } else if self.workspaces.seek(kind, |_| true).is_none() {
            let (_, added) = self.workspaces.apply(&crate::workspaces::ListOp::Add(crate::workspaces::Origin::of_kind(kind)));
            if let Some(id) = added {
                self.enter_id(id);
            }
        }
    }

    /// Show workspace `id` and put the plate in the mode it pins, nothing else.
    pub fn enter_id(&mut self, id: crate::workspaces::WorkspaceId) {
        self.workspaces.switch(id);
        // a page shown over the workspace stays with it
        self.page = None;
        if let Some(mode) = self.workspaces.current().mode {
            self.plate_mode = mode;
        }
    }

    /// Mirror Cx's focus-area migration without taking focus from an input or overlay.
    pub fn refresh_plate_keys(&mut self, old: Area, new: Area) {
        if old.is_empty() { return; }
        for area in self.plate_keys.iter_mut().chain(std::iter::once(&mut self.parts_key)) {
            if *area == old { *area = new; }
        }
    }

    /// The Plate view on `pane` took the keys at `area`.
    pub fn set_plate_key(&mut self, pane: PaneId, area: Area) {
        let at = pane.0 as usize;
        if self.plate_keys.len() <= at {
            self.plate_keys.resize(at + 1, Area::Empty);
        }
        self.plate_keys[at] = area;
    }

    pub fn parts_have_focus(&self, focus: Area) -> bool {
        !focus.is_empty() && (self.parts_key == focus || self.plate_keys.contains(&focus))
    }

    /// The dock shows this Plate (or has not said yet).
    pub fn plate_shown(&self, instance: crate::ui::areas::Instance) -> bool {
        !self.plates_hidden.contains(&instance.tab())
    }
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            undo_turn: None,
            workspaces: crate::workspaces::Workspaces::default(),
            plate_mode: PlateMode::Design,
            screen: u64::MAX,
            slice_after_mechanics: false,
            picked_override: None,
            strength: Default::default(),
            test_drag: None,
            drop_chip: None,
            arm_shown: false,
            scan_shown: false,
            follow_ai: false,
            scan_colour: true,
            scan_cameras: true,
            plate_keys: Vec::new(),
            parts_key: Area::Empty,
            plates_hidden: Vec::new(),
            notices: Notices::default(),
            command_palette_open: false,
            keymap_help_open: false,
            about_open: false,
            sources_page: SourcesPage::Settings,
            page: None,
            document_name: String::new(),
            document_info: String::new(),
            show_zones: false,
            camera_pip: false,
            gizmo: GizmoMode::Off,
            paint_radius: 3.0,
            paint_color: [0.8, 0.05, 0.05],
            paint_name: None,
            paint_mode: PaintMode::Brush,
            paint_angle: 30.0,
            paint_erase: false,
            marker: false,
            paint_target: PaintTarget::Color,
            paint_pattern: PaintPattern::default(),
            lay_flat: false,
            sculpt_op: fab_paint_field::StrokeOp::Raise,
            sculpt_radius: 3.0,
            sculpt_strength: 0.5,
            scale_locked: true,
            force_kind: ForceKind::Fixed,
            force_size: [0.0, 10.0, 0.1, 1000.0],
        }
    }
}

/// What the paint tool paints: a colour, or the support channels (`m.support_block`,
/// `m.support_enforce`), which the views show in a colour of their own while the tool paints them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PaintTarget {
    #[default]
    Color,
    SupportBlock,
    SupportEnforce,
}

impl PaintTarget {
    pub fn channel(self) -> fab_paint_field::Channel {
        match self {
            PaintTarget::Color => fab_paint_field::Channel::Color,
            PaintTarget::SupportBlock => fab_paint_field::Channel::SupportBlock,
            PaintTarget::SupportEnforce => fab_paint_field::Channel::SupportEnforce,
        }
    }

    /// The colour a support channel shows in (linear); `None` for the colour itself.
    pub fn tint(self) -> Option<[f32; 3]> {
        match self {
            PaintTarget::Color => None,
            PaintTarget::SupportBlock => Some([0.85, 0.08, 0.35]),
            PaintTarget::SupportEnforce => Some([0.1, 0.65, 0.2]),
        }
    }
}

/// The paint tool's press: brush a stroke, fill the feature under the pointer, fill the part,
/// flood the smooth surface region round the pointer (up to edges sharper than `paint_angle`),
/// or fill the part with a pattern of its own colour and the paint colour (`paint_pattern`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PaintMode {
    #[default]
    Brush,
    Feature,
    Part,
    Flood,
    Pattern,
}

/// What a Pattern fill writes: `m.fill(part, Mix{..})`, the part's own colour and the paint
/// colour through its volume by a field of the point (see `fab/ai/GUIDE.md`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintPattern {
    pub kind: PatternKind,
    /// The pattern's feature size (mm): a cell, a swirl, a band, a spot or stripe to the next.
    pub scale: f64,
    /// Another seed is another pattern of the same kind.
    pub seed: u32,
    /// How sharply each colour turns into the other, 0..1 (1: no blend between them).
    pub sharp: f64,
    /// Moves the whole pattern toward the paint colour (over 0) or the part's own, -1..1.
    pub balance: f64,
}

impl Default for PaintPattern {
    fn default() -> Self {
        PaintPattern { kind: PatternKind::Cells, scale: 6.0, seed: 1, sharp: PatternKind::Cells.sharp(), balance: 0.0 }
    }
}

impl PaintPattern {
    pub const SCALE: (f64, f64) = (0.5, 200.0);
    pub const SEEDS: u32 = 999;

    /// Held to the ranges the strip's dials span.
    pub fn held(self) -> Self {
        PaintPattern {
            scale: self.scale.clamp(Self::SCALE.0, Self::SCALE.1),
            seed: self.seed.clamp(1, Self::SEEDS),
            sharp: self.sharp.clamp(0.0, 1.0),
            balance: self.balance.clamp(-1.0, 1.0),
            ..self
        }
    }
}

/// The field a Pattern fill mixes its two colours by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PatternKind {
    /// Voronoi cells, each one colour or the other.
    #[default]
    Cells,
    /// Smooth 3D noise.
    Noise,
    /// Veins swirled through the part.
    Marble,
    /// Wavy layers up the part.
    Bands,
    /// One colour at the foot turning into the other at the top.
    Ramp,
    /// Spots grown on the part's skin (a Turing pattern: `Turing{preset: @spots}`).
    Spots,
    /// Stripes grown on the part's skin (`Turing{preset: @stripes}`).
    Stripes,
}

impl PatternKind {
    pub const ALL: [PatternKind; 7] = [PatternKind::Cells, PatternKind::Noise, PatternKind::Marble, PatternKind::Bands, PatternKind::Ramp, PatternKind::Spots, PatternKind::Stripes];

    pub fn label(self) -> &'static str {
        match self {
            PatternKind::Cells => "Cells",
            PatternKind::Noise => "Noise",
            PatternKind::Marble => "Marble",
            PatternKind::Bands => "Bands",
            PatternKind::Ramp => "Ramp",
            PatternKind::Spots => "Spots",
            PatternKind::Stripes => "Stripes",
        }
    }

    pub fn next(self) -> PatternKind {
        let i = Self::ALL.iter().position(|&k| k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The sharpness a kind starts at: cells and bands read as regions, the rest as blends.
    pub fn sharp(self) -> f64 {
        match self {
            PatternKind::Cells | PatternKind::Spots | PatternKind::Stripes => 1.0,
            PatternKind::Bands => 0.6,
            PatternKind::Marble => 0.3,
            PatternKind::Noise | PatternKind::Ramp => 0.0,
        }
    }

    /// Whether its scale and seed change it (a ramp spans the part).
    pub fn scaled(self) -> bool {
        self != PatternKind::Ramp
    }
}

/// A named feature of a part (`holes`, `bores`): its own region in the model frame.
#[derive(Clone, Debug)]
pub struct PaintFeature {
    pub name: String,
    pub region: fab_paint_field::Region,
    /// Its box (model frame): the smallest one under the pointer is the one picked.
    pub volume: f64,
}

impl PartialEq for PaintFeature {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.volume == other.volume
    }
}

/// What the part gizmo on the selected part does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GizmoMode {
    /// Select only.
    Off,
    #[default]
    Move,
    Rotate,
    Scale,
    /// The part's shape handles (its parameters, see `params`).
    Shape,
    Paint,
    Sculpt,
    /// Fixtures and loads on the part the Force render mode studies (the Force tool and its
    /// strip: a click adds the kind `UiState::force_kind` names).
    Force,
}

/// Scene objects named with this prefix are the printer's (bed, plate,
/// build volume): never listed, picked or selected.
pub const CONTEXT_PREFIX: &str = "fab:";
/// The nozzle-zone overlay's objects (shown on request).
pub const ZONE_PREFIX: &str = "fab:zone:";
/// The highest a printer element's top may lie over the bed plane and still be its floor
/// (world metres): the zone strips' lift.
pub const FLOOR_TOP: f32 = 0.0001;

/// A part's paint as the views bake it: its program (model frame, mm), the colour per slot
/// (linear; slot 0 unused), the printer-to-model frame, and the key of what its volume is made
/// of (equal keys bake the same volume: the colours and the frame are drawn, not baked).
#[derive(Clone)]
pub struct PaintView {
    pub program: Arc<fab_paint_field::Program>,
    pub colors: Vec<[f32; 3]>,
    pub from_printer: [[f64; 4]; 3],
    pub key: u64,
}

impl std::fmt::Debug for PaintView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PaintView({} layers, key {:016x})", self.program.layers.len(), self.key)
    }
}

impl PaintView {
    /// What the views show of it for `target`: its colour, or a support channel in that
    /// channel's tint. `None` when it paints nothing there.
    pub fn shown(&self, target: PaintTarget) -> Option<PaintView> {
        match target.tint() {
            None => self.program.has(fab_paint_field::Channel::Color).then(|| self.clone()),
            Some(tint) => self.program.shown(target.channel()).map(|program| PaintView {
                program: Arc::new(program),
                colors: vec![[0.0; 3], tint],
                from_printer: self.from_printer,
                key: self.shown_key(target),
            }),
        }
    }

    /// Whether it paints anything `target` shows.
    pub fn shows(&self, target: PaintTarget) -> bool {
        self.program.has(target.channel())
    }

    /// The key of [`PaintView::shown`].
    pub fn shown_key(&self, target: PaintTarget) -> u64 {
        match target {
            PaintTarget::Color => self.key,
            t => self.key ^ (t.channel() as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15),
        }
    }
}

impl PartialEq for PaintView {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.from_printer == other.from_printer && self.colors == other.colors
    }
}

/// One placed part of the plate, as the parts list and the part panel
/// show it. Positions are printer millimetres (Z up, bed origin).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlatePart {
    /// Included in slicing/printing; excluded parts remain editable on the bed.
    pub printed: bool,
    /// Its `place(...)` says `hidden: true`: kept on the plate, out of the views (a board turned
    /// off; [`AppState::sync_plate_elements`] hides its element).
    pub hidden: bool,
    /// Rides in a printed part; drawn solid in place, in the Sliced view too.
    pub insert: bool,
    /// An insert printed into its part (`insert(.., embed: true)`): the print pauses for it. The
    /// Sliced view draws it from the layer the print pauses after ([`AppState::embed_waits`]).
    pub embed: bool,
    /// A heat-set insert melted into its part after the print (`insert(.., melt: ..)`): its size
    /// (`M3`); empty for any other part.
    pub melt: String,
    pub insert_into: String,
    /// The model object (stable across rebuilds).
    pub object: String,
    /// The script instance shared by this placement and its linked clones.
    pub script: String,
    /// Original (0), or a linked placement.
    pub link: u32,
    /// The script source file.
    pub file: String,
    pub name: String,
    /// The print colour it prints in (its name: the identity, never the colour value).
    pub print_color: String,
    /// What it shows (linear): its print colour's.
    pub color: [f32; 4],
    pub paint: Option<Arc<fab_plate::paint::PartPaint>>,
    /// Its paint by position for the views (its script's program).
    pub paint_view: Option<PaintView>,
    /// Printer mm to the part's model frame (mm), and its named features there (the fill tool).
    pub model_from_printer: [[f64; 4]; 3],
    pub features: Arc<Vec<PaintFeature>>,
    /// This part's print-mode overrides, sorted by key.
    pub print: Vec<(String, String)>,
    pub min_mm: [f64; 3],
    pub max_mm: [f64; 3],
    /// Where its model origin sits: what its `place` puts at `at` and turns and scales about
    /// (the part's Position, its gizmo's pivot).
    pub origin: [f64; 3],
    /// The `place` turn about Z (degrees).
    pub rotate: f64,
    /// Its whole `place` turn: degrees about the printer's X, then Y, then Z (`rotate` is the
    /// last).
    pub turn: [f64; 3],
    /// The `place` scale: factors along the printer's X, Y, Z of the unturned part, before its
    /// turn (all equal: uniform).
    pub scale: [f64; 3],
    pub dropped: bool,
    /// A part of files dragged over the window, where their drop would place it: a ghost until
    /// they are dropped.
    pub arriving: bool,
    pub triangles: usize,
    /// Its source imports a mesh asset that can be repaired.
    pub imported_mesh: bool,
    /// Off the volume or out of every nozzle's reach.
    pub blocking: bool,
    pub issues: Vec<String>,
    /// Its viewport element (selection), when the scene has it.
    pub element: Option<ElementId>,
    /// A part whose own geometry a layer of the view draws at its place (a case's board volume,
    /// linked `board:<name>`, is where the board layer draws that board).
    pub linked: Option<LinkedPlace>,
    /// Its distance field, traced into viewport tiles; its mesh serves export and slicing.
    pub sdf: Option<PartSdf>,
    /// The object it is a body of when that object prints its bodies as one
    /// (`object([head, hair], name: "bust")`): the object's id and name. The parts list shows
    /// the object with its bodies under it.
    pub body_of: Option<(String, String)>,
}

/// Where a layer of the view draws its own geometry for a linked plate part.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkedPlace {
    /// The host key the part's solid was linked with (`m.link`): `"board:ledctl"`.
    pub key: String,
    /// That geometry's frame (its millimetres) to printer mm, row-major: the part's
    /// `model_to_plate` after its frame of the same name.
    pub plate_from_frame: [[f64; 4]; 3],
}

/// A part's solid as a distance field (`fab_model::eval::PartField::distance`): `to_printer`
/// turns, moves and scales it evenly, so the shape's value stays a distance on the plate (a part
/// scaled by axis has its stretch in its shape).
#[derive(Clone, Debug)]
pub struct PartSdf {
    pub shape: std::sync::Arc<fab_sdf_engine::makepad_csg_sdf::SdfShape>,
    /// Row-major, `p_printer_mm = to_printer * p_shape`.
    pub to_printer: [[f64; 4]; 3],
    /// The shape's key: equal keys, equal fields.
    pub key: [u8; 32],
}

impl PartSdf {
    /// A model's field `shape` (key `key`) under its whole placement `to_printer`, as a distance:
    /// a stretch along its own axes goes into the shape ([`makepad_csg_sdf::stretched`]). None
    /// for a placement that shears or flattens it.
    pub fn placed(shape: std::sync::Arc<fab_sdf_engine::makepad_csg_sdf::SdfShape>, to_printer: &[[f64; 4]; 3], key: [u8; 32]) -> Option<PartSdf> {
        Some(match fab_sdf_engine::makepad_csg_sdf::stretched(&shape, to_printer)? {
            (Some(s), rest) => PartSdf { key: s.key(), shape: std::sync::Arc::new(s), to_printer: rest },
            (None, _) => PartSdf { shape, to_printer: *to_printer, key },
        })
    }
}

/// A part's distance field at a live (dragged) value, ahead of its plate.
#[derive(Clone, Debug)]
pub struct LiveSdf {
    pub object: String,
    pub sdf: PartSdf,
    /// When the value was asked for, and how long the program took to compile.
    pub asked: std::time::Instant,
    pub compile_ms: f64,
}

impl PartialEq for PartSdf {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key && self.to_printer == o.to_printer
    }
}

impl PlatePart {
    /// A board's stand-in (`board_solid`, linked `board:<name>`): the board layer draws the real
    /// board in its place; it never prints, and its eye shows or hides it.
    pub fn is_board(&self) -> bool {
        self.linked.as_ref().is_some_and(|l| l.key.starts_with("board:"))
    }
    pub fn size(&self) -> [f64; 3] {
        [self.max_mm[0] - self.min_mm[0], self.max_mm[1] - self.min_mm[1], self.max_mm[2] - self.min_mm[2]]
    }
    pub fn center(&self) -> [f64; 3] {
        [(self.max_mm[0] + self.min_mm[0]) * 0.5, (self.max_mm[1] + self.min_mm[1]) * 0.5, (self.max_mm[2] + self.min_mm[2]) * 0.5]
    }
}

/// The plate the app evaluated, and the printer it is for.
#[derive(Clone, Debug, Default)]
pub struct PlateInfo {
    /// Printed parts first (slicer indices), followed by excluded placements.
    pub parts: Vec<PlatePart>,
    pub nozzles: Vec<String>,
    /// The print colours (the project's, then implicit ones); where each prints is the
    /// printer mapping's (`PrintState::colors`).
    pub colors: Vec<fab_plate::PrintColor>,
    pub machine: String,
    /// The bed (printer mm) and the volume's height.
    pub bed_min: [f64; 2],
    pub bed_max: [f64; 2],
    pub max_height: f64,
    /// Scene elements that are the printer's (bed, zones, frame): never
    /// listed, picked or selected.
    pub context_elements: Vec<ElementId>,
    /// The subset of those that are the nozzle-zone overlay.
    pub zone_elements: Vec<ElementId>,
    /// The subset of those that are the floor, lying in the bed plane (the heated bed, the
    /// build plate and its face, the zone strips on it): left out while the camera is under
    /// the bed ([`AppState::floor_shown`]).
    pub floor_elements: Vec<ElementId>,
    /// Bumped on every new plate.
    pub revision: u64,
    /// The project's global slicer settings (key, value), on top of the process profile.
    pub settings: Vec<(String, String)>,
    /// A drag's preview of the plate, not yet committed: the paint bakes nothing for it, what
    /// was baked last staying on show until the committed plate's bake replaces it.
    pub preview: bool,
    /// The Test prints view shows: `parts` are the ticked pieces, where they print, and these are
    /// the plate's own parts, what its lists and overrides read. Nothing under it edits.
    pub whole: Option<Arc<Vec<PlatePart>>>,
}

/// Plate-to-scene links used by glass classification. Rebuilt with `sync_plate_elements`,
/// never by an element predicate: hosts are a small set, and each mesh part remembers one
/// transparent batch (if any), so material eligibility needs no search through the plate.
#[derive(Default)]
pub(crate) struct GlassParts {
    pub hosts: std::collections::HashSet<ElementId>,
    pub elements: std::collections::HashMap<ElementId, (usize, Option<u32>)>,
}

impl PlateInfo {
    /// The plate view's box: the bed and what stands on it, not the whole build volume. Empty
    /// without a printer.
    pub fn view_box(&self) -> Aabb {
        if self.bed_max[0] <= self.bed_min[0] {
            return aabb_empty();
        }
        let mut min = [self.bed_min[0], self.bed_min[1], 0.0];
        let mut max = [self.bed_max[0], self.bed_max[1], 30.0];
        // Declared placements include fields before their display tiles or meshes arrive,
        // and parts outside the printable volume still belong in the opening view.
        for part in &self.parts {
            for axis in 0..3 {
                min[axis] = min[axis].min(part.min_mm[axis]);
                max[axis] = max[axis].max(part.max_mm[axis]);
            }
        }
        let at = crate::tools::transform::mm_to_world;
        Aabb { min: at(min), max: at(max) }
    }

    /// The part an element belongs to.
    pub fn part_of(&self, element: ElementId) -> Option<usize> {
        self.parts.iter().position(|p| p.element == Some(element))
    }
    /// Whether this element is an insert riding in a printed part.
    pub fn is_insert(&self, id: ElementId) -> bool {
        self.part_of(id).is_some_and(|i| self.parts[i].insert)
    }
    pub fn is_context(&self, element: ElementId) -> bool {
        self.context_elements.contains(&element)
    }
    pub fn is_floor(&self, element: ElementId) -> bool {
        self.floor_elements.contains(&element)
    }

    /// Where `ray` meets the plate's face (z = 0, inside the bed), from above or from under it,
    /// drawn or not: what a press on the plate turns the view about. `None` without a printer.
    pub fn bed_point(&self, ray: &Ray) -> Option<Vec3f> {
        if self.bed_max[0] <= self.bed_min[0] || ray.dir.z.abs() < 1e-9 {
            return None;
        }
        let t = -ray.origin.z / ray.dir.z;
        let p = ray.at(t);
        let mm = |v: f64| v as f32 * 0.001;
        let inside = (mm(self.bed_min[0])..=mm(self.bed_max[0])).contains(&p.x) && (mm(self.bed_min[1])..=mm(self.bed_max[1])).contains(&p.y);
        (t > 0.0 && inside && p.is_finite()).then_some(vec3(p.x, p.y, 0.0))
    }
}

/// A direct edit of the plate's parts. The app turns each into one undoable
/// model transaction (the same `fab.apply` the AI uses).
#[derive(Clone, Debug, PartialEq)]
pub enum PartAction {
    MakePrintable { object: String },
    /// These inserts print into their parts (`embed: true`: the print pauses to place them) or
    /// only ride in them, one undo step.
    SetEmbed { objects: Vec<String>, embed: bool },
    /// Select (or, with `toggle`, add/remove) a part by index.
    Select { index: usize, toggle: bool },
    Rename { object: String, name: String },
    /// These parts print in this print colour (one undo step).
    SetColor { objects: Vec<String>, color: String },
    /// A painted material of this part (its colour value) prints in this print colour.
    SetPaintColor { object: String, rgb: [f32; 3], color: String },
    /// Where a print colour prints on a printer: a nozzle side (`left`, `right`) and tray
    /// (`A2`, `HT1`, `Ext`); `None` forgets it there. Saved with the project.
    MapColor { color: String, printer: String, at: Option<(String, String)> },
    /// A print colour's design colour apart from its material's (`None`: its material's again).
    SetColorRgb { color: String, rgb: Option<[f32; 3]> },
    /// These parts print from this tray (one undo step): in `color`, the print colour mapped
    /// there (its place is saved), or in a new colour saved there. `paint`: only those painted
    /// materials (their colour values) of the one part; empty: the parts themselves.
    AssignTray { objects: Vec<String>, color: Option<String>, printer: String, side: String, tray: String, paint: Vec<[f32; 3]> },
    /// One press, sampled in printer millimetres; committed as one undo step (one record of the
    /// part's strokes file). `color`: the print colour it paints in (`None`: a colour of its own
    /// made of `rgb`); `erase`: back to the part's own colour.
    /// `target`: a support channel's stroke is one record of the part's file for it instead.
    PaintStroke { object: String, rgb: [f32; 3], color: Option<String>, erase: bool, radius: f64, points: Vec<[f64; 3]>, target: PaintTarget },
    /// One sculpt press, one undo step: a shape record (raise, lower, crease..) of the part's
    /// strokes file, which sculpts its distance field. Points and radius in the part's model
    /// frame (mm), as the view drew the stroke live: the record is what was shown.
    SculptStroke { object: String, op: fab_paint_field::StrokeOp, radius: f64, strength: f64, points: Vec<[f64; 3]> },
    /// Fill features of a part (`None`: the whole part) in a colour, one undo step.
    PaintFill { object: String, features: Vec<Option<String>>, rgb: [f32; 3], color: Option<String>, target: PaintTarget },
    /// Fill a part with `pattern`: its own colour (`own`, `own_color`) and `rgb` (`color`) mixed
    /// through its volume, one undo step.
    PaintPattern { object: String, own: [f32; 3], own_color: Option<String>, rgb: [f32; 3], color: Option<String>, pattern: PaintPattern },
    /// Flood the smooth surface region round `seed` (printer mm) up to edges sharper than
    /// `angle` (degrees), one undo step.
    PaintFlood { object: String, seed: [f64; 3], angle: f64, rgb: [f32; 3], color: Option<String>, target: PaintTarget },
    /// A change to the part's force study, the `m.fixed` / `m.force` / ... statements of its
    /// script, one undo step.
    Study { object: String, edit: StudyEdit },
    /// These objects' override of a slicer setting; `None` puts them back on the global one.
    SetPrintSetting { objects: Vec<String>, key: String, value: Option<String> },
    /// These objects follow the global settings again.
    ClearPrintSettings { objects: Vec<String> },
    /// The others take this object's overrides.
    CopyPrintSettings { from: String, to: Vec<String> },
    /// The plate's global slicer setting; `None` puts it back on the profile.
    SetPlateSetting { key: String, value: Option<String> },
    /// The plate's global set back to the profile.
    ClearPlateSettings,
    /// Its origin to `at` on the bed (mm), turned about it to `rotate` about Z (degrees).
    Place { object: String, at: [f64; 2], rotate: f64 },
    /// Scaled to `scale` about its origin: its `place` scale itself, factors along its own axes
    /// (the printer's X, Y, Z before its turn), one undo step.
    Scale { object: String, scale: [f64; 3] },
    /// Sit it on the plate.
    Drop { object: String },
    /// Spread every part over the plate.
    Arrange,
    DuplicateLinked { object: String },
    DuplicateCopy { object: String },
    Delete { object: String },
    /// All selected objects removed in one transaction (including assembly bodies).
    DeleteParts { objects: Vec<String> },
    DeleteScripts { objects: Vec<String> },
    DeleteScript { object: String },
    /// Include/exclude placements in one undoable plate edit.
    SetPrint { objects: Vec<String>, print: bool },
    /// View visibility, independent of whether the part prints.
    SetVisible { objects: Vec<String>, visible: bool },
    /// Keep placements on the plate but out of the views (`hidden: true`), or show them again, in
    /// one undoable plate edit: a board's eye, and View › Show Board.
    SetHidden { objects: Vec<String>, hidden: bool },
    /// A new primitive part ("box", "cylinder", "sphere", "cone", "torus").
    Add { shape: String },
    /// Live feedback while a gizmo drags (no edit yet): offset in mm and
    /// turn in degrees; `None` ends it.
    Preview(Option<(String, [f64; 3], f64)>),
    /// A shape edit of the object's script on the feature picked in the tree (none picked:
    /// the object itself), one undo step: the new solid is then picked.
    Shape { object: String, op: ShapeOp },
    /// Turn the object so the surface whose outward `normal` (printer frame) was clicked faces
    /// the bed, and sit it there.
    LayFlat { object: String, normal: [f64; 3] },
    /// A tree of the object's supports deleted in the Sliced view, by its tips (mm, the part's
    /// slice frame): a mark on its placement, one undo step; its trees plan without it.
    DeleteTree { object: String, tips: Vec<[f64; 3]> },
    /// An area drawn on the bed in the Sliced view where no tree may stand or grow: a solid of
    /// the nearest object's script marked by `m.no_trees`, one undo step; it is then picked.
    NoTrees { area: BedArea },
    /// A volume drawn on the shown layer (at plate height `z`, mm) in the Sliced view that
    /// prints with settings of its own ([`settings_span`] high): a solid of the nearest object's
    /// script with `m.settings` (a solid infill to start), one undo step; it is then picked.
    SettingsHere { area: BedArea, z: f64 },
    /// A print settings volume from a menu, through `fab.settings_volume`: over the whole of
    /// `object`, or on `feature` (`instance/solid`) in `shape` (`follows`, `box`, `sphere`,
    /// `cylinder`), with a solid infill to start, one undo step; it is then picked.
    SettingsVolume { object: Option<String>, feature: Option<String>, shape: &'static str },
    /// An area a script marks (`instance/solid`: no trees, print settings) clicked in the
    /// Sliced view: its script shows and its row is picked, as the tree picks it.
    PickArea { object: String },
    /// A marked area (`instance/solid`) deleted from the view: its solid and its statement
    /// (`m.no_trees`, `m.settings`) go from the script, one undo step.
    DeleteArea { object: String },
    /// Take a print settings box's setting (its row's `key`, `inst/solid#settings.wall_loops`)
    /// out of its statement: the box prints with its object's again; one undo step.
    ResetAreaSetting { key: String },
}

/// A test print as the views show it: where its box stands on the plate (mm; degrees about the
/// plate's X, then Y, then Z).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TestPrintView {
    pub name: String,
    /// The part it lives on.
    pub object: String,
    pub center: [f64; 3],
    pub size: [f64; 3],
    pub turn: [f64; 3],
    /// How the piece lies on the bed (degrees about the model's X, then Y, then Z); none: as printed.
    pub pose: Option<[f64; 3]>,
    /// That pose as the turn on the plate from as printed.
    pub lay: Option<[[f64; 3]; 3]>,
    /// Ticked: it shows, slices and prints when the test prints are chosen.
    pub on: bool,
    /// A brim of its own round its piece's first layer, as wide as the profile's brim.
    pub brim: bool,
}

impl TestPrintView {
    /// Its own axes in the plate's frame: the columns.
    pub fn axes(&self) -> [[f64; 3]; 3] {
        crate::model::state::turn_matrix(self.turn)
    }

    /// The corners of its box: the foot's four, then the top's.
    pub fn corners(&self) -> [[f64; 3]; 8] {
        let r = self.axes();
        let half = self.size.map(|v| 0.5 * v);
        std::array::from_fn(|k| {
            let x = if matches!(k % 4, 1 | 2) { half[0] } else { -half[0] };
            let y = if k % 4 >= 2 { half[1] } else { -half[1] };
            let z = if k >= 4 { half[2] } else { -half[2] };
            std::array::from_fn(|i| self.center[i] + r[i][0] * x + r[i][1] * y + r[i][2] * z)
        })
    }

    /// Whether its box reaches into the box `lo`..`hi` (plate mm), as the slice takes it: boxes
    /// apart along an axis of either are apart.
    pub fn reaches(&self, lo: [f64; 3], hi: [f64; 3]) -> bool {
        let corners = self.corners();
        for k in 0..3 {
            let (a, b) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p[k]), b.max(p[k])));
            if b <= lo[k] || a >= hi[k] {
                return false;
            }
        }
        let r = self.axes();
        for (k, half) in self.size.map(|v| 0.5 * v).into_iter().enumerate() {
            let along = |x: f64, y: f64, z: f64| (x - self.center[0]) * r[0][k] + (y - self.center[1]) * r[1][k] + (z - self.center[2]) * r[2][k];
            let mut range = (f64::MAX, f64::MIN);
            for x in [lo[0], hi[0]] {
                for y in [lo[1], hi[1]] {
                    for z in [lo[2], hi[2]] {
                        let d = along(x, y, z);
                        range = (range.0.min(d), range.1.max(d));
                    }
                }
            }
            if range.1 <= -half || range.0 >= half {
                return false;
            }
        }
        true
    }
}

/// What files dragged over the window would do when dropped, as the chip at the pointer says it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DropChip {
    /// Their name (or how many) and what they add, or why a drop would not go as wanted.
    pub text: String,
    /// A drop would not go as wanted (no room for it, a script that does not build).
    pub warn: bool,
}

/// A test print's placement as a drag or a field shows it until its edit lands: its box and how
/// its piece lies. The panel's next field and the gizmo's next drag start from it.
#[derive(Clone, Debug, PartialEq)]
pub struct TestDrag {
    pub name: String,
    pub center: [f64; 3],
    pub size: [f64; 3],
    pub turn: [f64; 3],
    pub pose: Option<[f64; 3]>,
}

/// What the test print list, its fields and its gizmo ask of the app. Each edit is one undo step,
/// on the path `fab.test_print` takes.
#[derive(Clone, Debug, PartialEq)]
pub enum TestPrintAction {
    /// A true cube in the middle of a part; it is then picked.
    Add { object: String },
    /// Pick it, the one pick there is: its part selected, its row picked in the tree.
    Pick { name: String },
    /// Its placement: its box on the plate, and how its piece lies (degrees about the model's X,
    /// then Y, then Z; none: as printed).
    Place { name: String, center: [f64; 3], size: [f64; 3], turn: [f64; 3], pose: Option<[f64; 3]> },
    Rename { name: String, to: String },
    Tick { name: String, on: bool },
    Delete { name: String },
    /// The face a click meets on a piece goes face down: the plate ray `from + t dir` (mm) on
    /// test print `name`'s piece in place or, with none, on the ticked pieces where they print.
    LayFlat { name: Option<String>, from: [f64; 3], dir: [f64; 3] },
    /// A brim of its piece's own, or the profile's.
    Brim { name: String, on: bool },
}

/// How high (mm) a no-trees area drawn on the bed reaches: the trees lean over it from there.
pub const NO_TREES_HEIGHT: f64 = 5.0;

/// The heights (plate mm, bottom and top) of a print settings volume drawn on the layer at
/// height `z`: a box as tall as its shorter side, a sphere as its disc, about the layer; one
/// that would reach under the bed stands on it.
pub fn settings_span(area: BedArea, z: f64) -> (f64, f64) {
    let h = match area {
        BedArea::Rect { min, max } => (max[0] - min[0]).min(max[1] - min[1]),
        BedArea::Disc { radius, .. } => 2.0 * radius,
    };
    let lo = (z - 0.5 * h).max(0.0);
    (lo, lo + h)
}

/// An area on the bed (plate mm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BedArea {
    Rect { min: [f64; 2], max: [f64; 2] },
    Disc { centre: [f64; 2], radius: f64 },
}

/// A primitive the shape edits add or cut away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapePrim {
    Box,
    Cylinder,
    Sphere,
}

impl ShapePrim {
    pub fn call(self) -> &'static str {
        match self {
            ShapePrim::Box => "box",
            ShapePrim::Cylinder => "cylinder",
            ShapePrim::Sphere => "sphere",
        }
    }
}

/// What a shape edit writes into the program (Edit shape strip, the tree's Add menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeOp {
    /// Cut a primitive away from the feature.
    Subtract(ShapePrim),
    /// Join a primitive to the feature.
    Add(ShapePrim),
    /// Cut away everything below a height over the bed (a slab under the part).
    CutBottom,
    /// Cut away everything on one side of a plane (a slab to turn and move).
    CutPlane,
    /// Keep both sides of the cut plane (none: level through the middle), with alignment pins:
    /// each half an object of its own, on the plate flat side down.
    Split,
}

/// A finished measurement.
#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    /// Optional user name, separate from the computed dimension label.
    pub name: Option<String>,
    pub kind: MeasureKind,
    pub points: Vec<Vec3f>,
    /// Meters, square meters or degrees depending on `kind`.
    pub value: f64,
    pub label: String,
    pub circle: Option<makepad_csg_cad::measure::Circle3>,
    pub gap: Option<makepad_csg_cad::measure::PlaneGap>,
    pub normals: Vec<Vec3f>,
}

/// Measurement rows and viewport picks share these document actions.
#[derive(Clone, Debug, PartialEq)]
pub enum MeasureAction {
    Select(usize),
    Delete(usize),
    Rename(usize, String),
}

// ===========================================================================
// AppState
// ===========================================================================

/// The single source of truth, owned by `App`, passed to every widget via
/// `Scope::with_data`. Plain data.
///
/// Hot-path direct writes allowed (everything else goes through actions):
/// * viewport: `view.hover`
/// * navigator: its pane's camera and preset
/// * tools: `scene_state.section` via `set_section` (measurement drafts stay in the tool session)
pub struct AppState {
    /// Never `None`: `Scene::empty()` until a load completes.
    pub scene: Arc<Scene>,
    /// The electronics host's live design (the preview while one is live), delivered by
    /// `ElecHost::take_scene` in the app.
    pub elec_scene: Option<Arc<fab_elec_mech::scene::ElecScene>>,
    /// A drag's newest body poses by component, its moved parts only (`ElecHost::take_poses`),
    /// newer than `elec_scene` while its scenes are on their way: the 3D views pose those bodies by
    /// them until a scene has them. Empty: no drag poses any more (a load).
    pub elec_poses: Option<Arc<[(fab_elec_core::Sel, fab_elec_core::Xform3)]>>,
    /// `elec_scene` was built from the committed design (no preview in it): once the 3D views
    /// have prepared it, it replaces a finished drag's `elec_poses`, matching or not.
    pub elec_scene_committed: bool,
    /// An electronics edit's live preview is asked for or showing (a drag on the board). The 3D
    /// views count it as motion: no still-view samples until it ends, so every frame of a drag
    /// is of one quality and the picture converges after the drop.
    pub elec_previewing: bool,
    /// A scene the views will show is on its way: the plate is being built (a plate or demo
    /// loading, a case change rebuilding it) or the electronics owes the views a scene. The 3D
    /// views report themselves unsettled until it has come (their description's `settled`).
    pub scene_coming: bool,
    /// The parts the Electronics views have selected (a part, one of its pins, pads or ports, or
    /// its placement on a board), as the host's view state holds them (`ElecHost::selected_parts`):
    /// the 3D views ring their bodies, or the printed keycaps and knobs bound to them, as the
    /// active selection. Empty: none.
    pub elec_selected: Arc<[fab_elec_core::CompRef]>,
    /// Where the boards' outline union is centred on the bed (plate mm): a free spot from the
    /// plate's placement path, kept while it stays free. None until a plate is evaluated: the
    /// bed's centre.
    pub elec_spot: Option<[f64; 2]>,
    /// Plate parts a layer of the view draws in their place as its own geometry (a cased board):
    /// the plate's passes leave their meshes out, and they stay for picking and the checks. The
    /// viewport writes it when its layers' links change.
    pub drawn_by_link: Vec<ElementId>,
    /// The elements their placements keep out of the views (`hidden: true`) at the last
    /// [`Self::sync_plate_elements`]: one shown again is unhidden there.
    pub place_hidden: Vec<ElementId>,
    /// Plate parts that glow with a live part's light (a light pipe with the LED it carries).
    /// Empty unless a mode sets it ([`ShellAction::SetPartGlows`]).
    pub part_glows: Arc<[PartGlow]>,
    /// Plate parts a press on presses a live input (a keycap on its switch): the viewport
    /// presses `comp` as it presses the board's own part. Empty unless a mode sets it.
    pub part_presses: Arc<[PartPress]>,
    /// Plate parts a vertical drag on turns a live knob (a printed knob on its pot's shaft): the
    /// viewport turns `comp` through its travel. Empty unless a mode sets it.
    pub part_turns: Arc<[PartTurn]>,
    /// The shapes whose field stopped an open-ended loop at its cap under the view.
    pub field_loop_stops: std::collections::HashSet<[u8; 32]>,
    /// Moves when a part comes to show a stopped loop: the parts list redraws its rows.
    pub field_loop_marks: u64,
    pub scene_state: SceneState,
    /// The plate view.
    pub view: ViewportState,
    pub sun: SkyState,
    pub load: LoadStatus,
    /// The job the 3D view shows a card for, over the plate still on the bed (another plate
    /// on its way there).
    pub progress_card: Option<ProgressCard>,
    /// The plates and the demos (File > Open Recent shows the newest plates).
    pub plates: PlateList,
    pub ui: UiState,
    pub tool: Tool,
    pub snap: SnapOptions,
    pub measurements: Vec<Measurement>,
    /// The plate's test prints, as its project has them.
    pub test_prints: Vec<TestPrintView>,
    pub selected_measure: Option<usize>,
    /// Bumped by `set_scene`; widgets holding per-scene caches diff it.
    pub scene_revision: u64,
    /// How lengths and areas are shown (Fab: millimetres). Every length/area
    /// label formats through `units.display` / `precision`.
    pub units: Units,
    /// The slice job, its status and the print preview (`print`).
    pub print: crate::print::PrintState,
    /// The plate as the panels see it: the user's parts (never the printer's
    /// helper geometry) and the printer's bed.
    pub plate: PlateInfo,
    pub(crate) glass_parts: GlassParts,
    /// The parts' parameters for the generated controls and the shape handles.
    pub params: crate::params::ParamState,
    /// The program as text and its features (the Design workspace's code and tree).
    pub source: crate::ui::code::SourceState,
    /// The Mechanics workspace: the live sim's state as the panel shows it.
    pub mech: MechView,
    /// The Arm workspace (experimental): the arm cell's run as the panel and the cell view show it.
    pub arm: ArmView,
    /// The Scan workspace: a headset scan as the panel and the scan view show it.
    pub scan: ScanView,
    /// The Force render mode: the study of the selected part as the panel and the plate view show it.
    pub force: ForceView,
    /// The viewport's field parts and tile tracers, shared with the headset view.
    pub sdf: Option<Rc<RefCell<crate::viewport::sdf::SdfLayer>>>,
    /// Shared scene uploads and renderer for all viewports.
    pub scene_gpu: Rc<RefCell<crate::viewport::scene_gpu::SceneGpu>>,
    /// The pose sets panes can show ([`PanePoses::Set`]), indexed by [`PoseSetId`]; the device's
    /// is always there ([`PoseSetId::DEVICE`]).
    pub pose_sets: Vec<PoseSet>,
    pub device: DeviceView,
    /// Where Fab may get electronics parts' 3D models (this machine's choice).
    pub model_sources: crate::model_sources::ModelSources,
    /// Which projects' twins may reach real servers (this machine's choice).
    pub firmware_net: crate::firmware_net::FirmwareNet,
    /// The saved firmware network tokens' names and hosts, or why they cannot be read.
    pub firmware_tokens: Result<Vec<crate::firmware_net::TokenEntry>, String>,
    /// What the person picked for the AI in the Edit shape mode ([`crate::tools::picks`]).
    pub picks: crate::tools::picks::PickState,
}

/// A plate part that glows with a live part's light: a light pipe and the LED it carries.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PartGlow {
    pub element: ElementId,
    pub comp: fab_elec_core::CompRef,
    /// The pipe's pose on its LED in the assembled case, in Fab metres.
    pub seat: crate::model::state::Pose,
}

/// A plate part a press on presses a live input: a keycap and the switch it sits on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PartPress {
    pub element: ElementId,
    pub comp: fab_elec_core::CompRef,
}

/// A plate part a drag on turns a live knob: a printed knob and the pot it sits on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PartTurn {
    pub element: ElementId,
    pub comp: fab_elec_core::CompRef,
}

/// The scene the Mechanics sim runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MechScene {
    #[default]
    Drop,
    Drape,
    Hang,
    /// The plate's planned supports as loose pieces around the parts: shake them, pull them out.
    Supports,
    /// A board's case closed with the board in it, every printed part where it sits: its keys
    /// press the running board's switches and its light pipes carry its LEDs' light.
    Closed,
}

/// How a part's distance is found for the sim's contacts and the supports' checks: the person's
/// choice, kept on this machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MechContact {
    /// Built in a moment; off by up to a few cells at sharp edges and in narrow gaps.
    #[default]
    Fast,
    /// Within a quarter layer of the true distance at edges and in narrow gaps, and closer on
    /// faces; takes longer to build.
    Exact,
}

/// What a press on a part does in Mechanics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MechTool {
    /// Hold a part by a firm spring and move it with the cursor; letting go leaves it as the sim has it.
    Grab,
    /// As Grab; letting go gives the part the cursor's velocity of the last few frames.
    #[default]
    Throw,
    /// Click a point on a part to pin it in space there; click the pin again to unpin.
    Pin,
    /// Hang the part from the cursor on a soft spring (a rope) so it swings.
    Dangle,
    /// Click to give the part an impulse away from the camera at that point.
    Poke,
}

/// The Mechanics sim as the panel shows it (the app fills it each frame it runs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MechView {
    /// The tool a press uses.
    pub tool: MechTool,
    /// The element being held (highlighted), and the elements that are pinned.
    pub grabbed_element: Option<ElementId>,
    pub pinned: Vec<ElementId>,
    pub scene: MechScene,
    /// Which of the plate's chain parts the sim is of.
    pub piece: usize,
    /// The ball a cloth drapes over in this scene (printer mm: centre, radius).
    pub ball: Option<([f64; 3], f64)>,
    /// What it is doing ("Building the joints...", an error, empty when running fine).
    pub status: String,
    /// A sim is up.
    pub ready: bool,
    pub running: bool,
    pub shaking: bool,
    /// Sim seconds per wall second.
    pub speed: f64,
    /// The contact setting in force.
    pub contact: MechContact,
    pub time: f64,
    pub joints: Vec<MechJointView>,
    /// Each string's tension (N), as the assembly lists them.
    pub strings: Vec<f64>,
    /// What is known of the joints, in a line ("2 joints · not checked", "7 of 7 joints free · worst fuse risk medium").
    pub verdict: String,
    /// The part being dragged (its plate part index).
    pub grabbed: Option<usize>,
    /// How many bodies the action menu applies to now: the selected parts', or all when none is selected.
    pub targets: usize,
    /// Some targeted body is pinned; every targeted body is frozen.
    pub targets_pinned: bool,
    pub targets_frozen: bool,
    /// The support pieces in a [`MechScene::Supports`] sim, and what the check says of each so far.
    pub pieces: Vec<MechPieceView>,
    /// What a hand carries rather than pulls: the parts of a [`MechScene::Supports`] sim.
    pub carried: Vec<ElementId>,
    /// The closed box's board output is heard on the speakers.
    pub listening: bool,
    /// While it is heard: what the output plays, in a line ("Rendering 3.1 of 8.0 s · plays in
    /// 12 s"), and how far the stretch it renders has come (0 to 1), None while it plays live.
    pub listen_line: String,
    pub listen_progress: Option<f32>,
    /// The joint check, run when asked: its progress, its answers and its marks.
    pub check: MechCheckView,
    /// The plate objects shown only where the sim has them (a piece it hangs on its pin, turned from
    /// where it printed): until its pose places one, the views leave it out and it has no grab target.
    pub posed_only: Vec<String>,
    /// The pieces of field parts that print apart, each drawn by its own field where the sim moves it.
    pub fields: Vec<MechField>,
    /// A printed board's bench leads the sim's clock (it reads the motion the sim gives the board's
    /// sensors): the pair goes at the bench's pace and the speed setting waits for the lead to end.
    pub lead: Option<MechLead>,
}

/// The bench leading the Mechanics sim's clock, as the bar shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MechLead {
    /// The pace the bench's run reaches, simulated seconds per wall second; None until measured.
    pub rate: Option<f64>,
    /// The bench's run waits for the motion this sim writes (its status's limit is the streamed
    /// input): the sim is the pair's slower side.
    pub waits_for_motion: bool,
}

/// A piece of a field part that prints apart, as Mechanics shows it: its scene object (the sim's own
/// scene has it, an element with the field's box), its field (the part's within the piece's mask),
/// and its part's colour.
#[derive(Clone, Debug, PartialEq)]
pub struct MechField {
    pub object: String,
    pub sdf: PartSdf,
    pub color: [f32; 4],
}

/// The joint check as the panel and the view show it (the app keeps it across sims of the same
/// piece).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MechCheckView {
    /// The jointed piece's joints (none: no check to run).
    pub joints: usize,
    /// The running check's job: the panel's busy line shows its stage, bar, time and Cancel.
    pub job: Option<makepad_widgets_core::jobs::JobId>,
    /// What the last check said in a line ("1 of 1 joints free · worst fuse risk low"); empty:
    /// not checked.
    pub line: String,
    /// The last check was of an earlier shape or other print settings.
    pub stale: bool,
    /// Per joint, what the check says of it so far.
    pub rows: Vec<String>,
    /// What the view marks (printer frame, metres, where the parts are now): lines (a joint's
    /// region, a turn's range) and points with a label.
    pub lines: Vec<(Vec<[f32; 3]>, MechMarkKind)>,
    pub marks: Vec<MechMark>,
    /// The moving parts shown turned to the angles the sweep tests and stops at: their elements
    /// (drawn see-through) and colours.
    pub ghosts: Vec<(ElementId, [f32; 4])>,
}

/// What a mark of the joint check shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MechMarkKind {
    /// Where the check is looking: a joint's region.
    #[default]
    Region,
    /// The closest the parts come.
    Gap,
    /// A layer where a bead could tack the parts together.
    Tack,
    /// Where a turn stops, and the range it measured.
    Stop,
    /// What keeps a joint from moving: fused, tacked, too close.
    Bind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MechMark {
    pub at: [f32; 3],
    pub label: String,
    pub kind: MechMarkKind,
}

/// What the support removal check says of a piece.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MechLight {
    /// Not checked yet.
    #[default]
    Waiting,
    Green,
    Amber,
    Red,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MechPieceView {
    /// Its element's name in the scene.
    pub name: String,
    /// As people know it: its part's name.
    pub label: String,
    pub light: MechLight,
    /// In words: "comes out whole", "no way out found"...
    pub words: String,
    /// A point on it at rest (printer frame, metres): where a press takes it.
    pub grip: [f32; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MechJointView {
    pub name: String,
    pub kind: String,
    /// Radians: the angle now (a ball: its tilt), and its range; a slider's in millimetres.
    pub angle: f64,
    pub lower: f64,
    pub upper: f64,
    pub ok: bool,
    /// The force through it (N), when the sim measures it.
    pub load: Option<f64>,
    /// Its motor, when the script gives it one: the panel's row for it sets it.
    pub motor: Option<MechMotorView>,
}

/// A joint's motor as the Mechanics panel shows and sets it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MechMotorView {
    /// A speed drive; else a position servo.
    pub speed: bool,
    /// What it is set to: a servo's angle in radians (a slider's place in millimetres), a drive's
    /// speed in radians a second (a slider's in millimetres a second).
    pub target: f64,
    /// What it pushes with now and at most: N mm (a slider's N).
    pub effort: f64,
    pub limit: f64,
}

/// What the action menu does to the selected parts (all parts when none is selected).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MechOp {
    /// Let go of the part and let it fall onto the plate.
    Drop,
    /// Pin it at its top and let it dangle.
    Hang,
    Pin,
    Unpin,
    /// Throw it away from the camera and up.
    Throw,
    /// Back to the start pose.
    Home,
    /// Stop it where it is (true), or let it move again (false).
    Freeze(bool),
}

/// What the Mechanics workspace asks the app's sim to do.
#[derive(Clone, Debug, PartialEq)]
pub enum MechAction {
    /// An action-menu action on the selected parts.
    Do(MechOp),
    /// One step while paused.
    Step,
    Play,
    Pause,
    /// Restart the chosen scene.
    Reset,
    /// Check the joints (gap, as-printed layers, range sweep); the busy line's Cancel stops it.
    CheckJoints,
    /// Choose a scene and reset the sim.
    Scene(MechScene),
    /// Choose which chain part the sim is of.
    Piece(usize),
    /// Sim seconds per wall second.
    Speed(f64),
    /// How parts' distances are found; the parts that use them are built again.
    Contact(MechContact),
    /// Shake the held parts (on / off).
    Shake(bool),
    /// The closed box: hear the board's output on the speakers (on / off).
    Listen(bool),
    /// Set the motor of [`MechView::joints`]`[joint]` to `target` ([`MechMotorView::target`]'s units).
    Motor { joint: usize, target: f64 },
    /// Grab what an element shows (a part, or one link of a chain) at a point on it (printer frame,
    /// metres). `hand`: a hand's hold in the headset (firm, the part keeps up with the hand), else
    /// the pointer's (a soft pull).
    Grab { element: ElementId, point: [f64; 3], hand: bool },
    /// Move the grab to a point (printer frame, metres).
    Drag { point: [f64; 3] },
    /// Let go; a hand's throw gives the held point its velocity (printer frame, m/s).
    Release { velocity: Option<[f64; 3]> },
    /// A hand takes what [`MechView::carried`] names where the sim has it, and the sim plays: the
    /// view shows the sim a step or two late, and the hand's first [`MechAction::Carry`] after this
    /// is where it has them, not a pose to go to.
    Take,
    /// A hand carries what [`MechView::carried`] names to this pose: rest -> now, a turn and a move
    /// (printer frame, metres). After a [`MechAction::Take`] they move from where the take found
    /// them as the hand has moved since. They stay there when the hand lets go.
    Carry { pose: [[f64; 4]; 3] },
    /// Choose the tool (the app keeps `MechView::tool`).
    Tool(MechTool),
    /// Pin the element at a point on it (printer frame, metres), or unpin it if it is pinned.
    Pin { element: ElementId, point: [f64; 3] },
    /// Hold by a soft spring (a rope) at a point; moved with `Drag`, ended with `Release`.
    Dangle { element: ElementId, point: [f64; 3] },
    /// An impulse (printer frame, kg*m/s per kg of the part: a velocity change, m/s) at a point.
    Poke { element: ElementId, point: [f64; 3], dv: [f64; 3] },
}

// ===========================================================================
// Arm (experimental)
// ===========================================================================

/// How much of the arm loop runs (the planner's loop modes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ArmLoop {
    /// A rigid slicer's: the path through IK, nothing observed.
    Rigid,
    /// The dynamic model's feed-forward, still blind.
    Feedforward,
    /// Feed-forward and every observation: tracker, melt pool, scans.
    #[default]
    Live,
}

impl ArmLoop {
    pub const ALL: [ArmLoop; 3] = [ArmLoop::Rigid, ArmLoop::Feedforward, ArmLoop::Live];
    pub fn label(self) -> &'static str {
        match self {
            ArmLoop::Rigid => "Rigid",
            ArmLoop::Feedforward => "Feed-forward",
            ArmLoop::Live => "Live",
        }
    }
}

/// How the virtual cell differs from the models the planner plans with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ArmErrors {
    /// The cell is its models.
    Exact,
    /// Mis-modelled as real cells are: softer joints, unmodelled tool mass, fuller beads.
    #[default]
    Typical,
}

impl ArmErrors {
    pub const ALL: [ArmErrors; 2] = [ArmErrors::Exact, ArmErrors::Typical];
    pub fn label(self) -> &'static str {
        match self {
            ArmErrors::Exact => "Exact",
            ArmErrors::Typical => "Typical",
        }
    }
}

/// What the planner drives: the virtual cell, a dry run against real devices with the
/// process off, or the real cell. Only the virtual cell exists yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ArmTarget {
    #[default]
    Sim,
    DryRun,
    Real,
}

impl ArmTarget {
    pub const ALL: [ArmTarget; 3] = [ArmTarget::Sim, ArmTarget::DryRun, ArmTarget::Real];
    pub fn label(self) -> &'static str {
        match self {
            ArmTarget::Sim => "Sim",
            ArmTarget::DryRun => "Dry run",
            ArmTarget::Real => "Real",
        }
    }
    /// It can run here.
    pub fn available(self) -> bool {
        self == ArmTarget::Sim
    }
}

/// Which field the part view colours by its distance to the design.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ArmField {
    /// What the virtual cell truly built.
    #[default]
    Built,
    /// What the planner knows of it from its observations.
    Seen,
}

impl ArmField {
    pub const ALL: [ArmField; 2] = [ArmField::Built, ArmField::Seen];
    pub fn label(self) -> &'static str {
        match self {
            ArmField::Built => "As built",
            ArmField::Seen => "As seen",
        }
    }
}

/// One arm at one moment (cell frame, mm, Z up).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArmShape {
    pub name: String,
    /// The links as capsules (ends and radius): where the arm truly is, and the rigid pose of
    /// the setpoint it was sent.
    pub links: Vec<([f32; 3], [f32; 3], f32)>,
    pub sent: Vec<([f32; 3], [f32; 3], f32)>,
    /// Each link's centre of mass where it truly is and in the setpoint's rigid pose, and its
    /// mass as truly carried (kg).
    pub masses: Vec<([f32; 3], [f32; 3], f32)>,
    /// Where the tracker saw the TCP over the last seconds.
    pub trail: Vec<[f32; 3]>,
    /// What the arm is doing ("deposit", "travel", ...), and whether its process is on.
    pub doing: String,
    pub on: bool,
    /// Its TCP and tool axis (pointing into the work), and where the TCP was sent to be.
    pub tcp: [f32; 3],
    pub axis: [f32; 3],
    pub target: [f32; 3],
}

/// One moment of the arm cell for the cell view (cell frame, mm, Z up).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArmScene {
    pub time: f64,
    pub arms: Vec<ArmShape>,
    /// What the arms reach (the cell view frames it), and the part's box (the close-up's).
    pub cell_lo: [f32; 3],
    pub cell_hi: [f32; 3],
    pub part_lo: [f32; 3],
    pub part_hi: [f32; 3],
    /// What the part stands on, and the design, as outline loops.
    pub stand: Arc<Vec<Vec<[f32; 3]>>>,
    pub design: Arc<Vec<Vec<[f32; 3]>>>,
    /// The part's surface, sampled: each point and its distance off the design (mm; positive
    /// is excess, negative missing). Shared by the moments between two samplings.
    pub surface: Arc<Vec<([f32; 3], f32)>>,
    /// The working arm's path over the next half second (the planner's horizon).
    pub horizon: Vec<[f32; 3]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArmCheckState {
    #[default]
    Waiting,
    Running,
    Pass,
    Fail,
}

/// One physics or cell check as the panel lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArmCheckRow {
    pub name: String,
    pub state: ArmCheckState,
    pub figures: String,
    /// How long it took (ms).
    pub ms: f64,
}

/// The Arm workspace as the panel and the cell view show it (the app's arm lab fills it).
#[derive(Clone, Debug, PartialEq)]
pub struct ArmView {
    /// Plastic (one arm printing, with an extruder) or metal (the welding and trimming pair).
    pub plastic: bool,
    /// The cells and jobs offered, and the ones chosen.
    pub cells: Vec<String>,
    pub cell: usize,
    pub jobs: Vec<String>,
    pub job: usize,
    /// Plastic: the extruder heads offered and the one chosen, its nozzle (mm) and the
    /// nozzles it takes.
    pub heads: Vec<String>,
    pub head: usize,
    pub nozzle: f64,
    pub nozzles: (f64, f64),
    pub mode: ArmLoop,
    pub errors: ArmErrors,
    pub target: ArmTarget,
    pub field: ArmField,
    pub running: bool,
    /// The job ran to its end (or stopped).
    pub ended: bool,
    /// Cell seconds per wall second; 0 runs as fast as it can.
    pub speed: f64,
    /// How much the cell view enlarges each arm's deflection from its setpoint's rigid pose.
    pub flex: f64,
    /// What it is doing, in a line; the run's figures as (label, value) rows; its events,
    /// newest last.
    pub status: String,
    pub stats: Vec<(String, String)>,
    /// The cell loop's real-time path: compiled ahead or what is left, and the lazy compiles
    /// real-time threads met (empty until the workspace first shows).
    pub realtime: String,
    pub events: Vec<String>,
    pub scene: Option<Arc<ArmScene>>,
    pub checks: Vec<ArmCheckRow>,
    /// The comparison tab: the parts it offers and the one chosen, the table it shows (made
    /// for its key), the key last asked for, what it is doing, the row whose sheet shows.
    pub parts: Vec<String>,
    pub compare_part: usize,
    pub compare: Option<Arc<ArmCompare>>,
    pub compare_asked: Option<ArmCompareKey>,
    pub compare_status: String,
    pub compare_row: usize,
}

/// What a comparison is made for: the part, the plastic head and its nozzle (tenths of a mm).
pub type ArmCompareKey = (usize, usize, u8);

impl ArmView {
    /// The comparison the panel's choices ask for now.
    pub fn compare_key(&self) -> ArmCompareKey {
        (self.compare_part, self.head, (self.nozzle * 10.0).round() as u8)
    }
}

/// Each option's time for a part against the H2D's, as the comparison tab shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArmCompare {
    pub key: ArmCompareKey,
    /// What it compares: the part, the plastic head.
    pub title: String,
    pub rows: Vec<ArmCompareRow>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArmCompareRow {
    /// The group a row starts ("Plastic", "Metal"; empty: as the row above).
    pub group: String,
    pub name: String,
    pub time: String,
    pub ratio: String,
    /// Faster than the H2D.
    pub faster: bool,
    pub mode: String,
    pub limit: String,
    /// Its hover hint: every limit weighed, and what else to know.
    pub hint: String,
    /// Where its figures come from: (parameter, value, P/M/E).
    pub sheet: Vec<(String, String, String)>,
}

impl Default for ArmView {
    fn default() -> Self {
        ArmView {
            plastic: false,
            cells: Vec::new(),
            cell: 0,
            jobs: Vec::new(),
            job: 0,
            heads: Vec::new(),
            head: 0,
            nozzle: 1.0,
            nozzles: (1.0, 1.0),
            mode: ArmLoop::default(),
            errors: ArmErrors::default(),
            target: ArmTarget::default(),
            field: ArmField::default(),
            running: false,
            ended: false,
            speed: 1.0,
            flex: 20.0,
            status: String::new(),
            stats: Vec::new(),
            realtime: String::new(),
            events: Vec::new(),
            scene: None,
            checks: Vec::new(),
            parts: Vec::new(),
            compare_part: 2,
            compare: None,
            compare_asked: None,
            compare_status: String::new(),
            compare_row: 0,
        }
    }
}

/// What a scan is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanPhase {
    #[default]
    Idle,
    /// Asked for: no headset linked yet, or the headset is opening its cameras (the wearer's
    /// permissions).
    Waiting,
    Capturing,
    /// A kept scan read back, keyframe by keyframe.
    Showing,
    /// The headset cannot capture: the status says why.
    Failed,
}

impl ScanPhase {
    /// A scan is under way: Stop ends it.
    pub fn busy(self) -> bool {
        matches!(self, ScanPhase::Waiting | ScanPhase::Capturing | ScanPhase::Showing)
    }
}

/// A count of keyframes in words: "1 keyframe", "14 keyframes".
pub fn keyframes_in_words(count: usize) -> String {
    if count == 1 { "1 keyframe".to_string() } else { format!("{count} keyframes") }
}

/// A point of a scan's surface (the headset's tracking space: y up, metres).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanPoint {
    pub point: [f32; 3],
    /// Unit, out of the surface.
    pub normal: [f32; 3],
    /// The keyframe that saw it first.
    pub first: u32,
    /// The width of the square it stands for, metres.
    pub size: f32,
    /// The colour the keyframes' pictures show there, 0..1 (mid grey where none did).
    pub colour: [f32; 3],
}

/// Where a keyframe was taken from.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanKeyframe {
    pub head: [f32; 3],
    /// Each picture's camera, row-major: x right, y down, z forward, its centre.
    pub cameras: Vec<[[f32; 4]; 3]>,
    /// The headset's depth came with it.
    pub depth: bool,
    /// Seconds into the scan.
    pub at: f64,
}

/// A scan kept on this computer.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanKept {
    pub name: String,
    pub keyframes: usize,
    pub megabytes: f64,
}

/// The Scan workspace as the panel and the scan view show it (the app's scan fills it).
#[derive(Clone, Debug, Default)]
pub struct ScanView {
    pub phase: ScanPhase,
    /// What the scan is doing, in a line; and what the headset last said of its cameras.
    pub status: String,
    pub headset_says: String,
    pub keyframes: Vec<ScanKeyframe>,
    /// The base surface so far, from the headset's depth.
    pub surface: Arc<Vec<ScanPoint>>,
    /// The detail the keyframes' pictures gave so far, and the base's 10 mm cells it has a point
    /// in (a cell's whole numbers: the point over 0.01, floored). It stands in for the base
    /// there and in the cells next to them.
    pub detail: Arc<Vec<ScanPoint>>,
    pub covered: Arc<std::collections::HashSet<[i32; 3]>>,
    /// Seconds since the scan began.
    pub seconds: f64,
    /// The panel's figures: name, value.
    pub stats: Vec<(String, String)>,
    /// The scans kept, newest first.
    pub kept: Vec<ScanKept>,
}

/// What the Scan workspace asks the app's scan to do.
#[derive(Clone, Debug, PartialEq)]
pub enum ScanAction {
    /// Ask the linked headset for its cameras and keep what comes.
    Start,
    Stop,
    /// Empty the view (a kept scan stays kept).
    Clear,
    /// Read the kept scan at this row back into the view.
    Show(usize),
    /// The view draws the scan in the cameras' colours (on) or in its plain tint (off); kept for the next start.
    Colour(bool),
    /// The view marks where the keyframes were taken from (on) or shows the scan alone (off); kept for the next start.
    Cameras(bool),
}

/// What the Arm workspace asks the app's arm lab to do.
#[derive(Clone, Debug, PartialEq)]
pub enum ArmAction {
    /// Plastic or metal; a cell or a job; a plastic cell's head and nozzle (mm) (the run
    /// starts over).
    Plastic(bool),
    Cell(usize),
    Job(usize),
    Head(usize),
    Nozzle(f64),
    Mode(ArmLoop),
    Errors(ArmErrors),
    Target(ArmTarget),
    Field(ArmField),
    Play,
    Pause,
    /// Back to the job's start.
    Reset,
    Speed(f64),
    Flex(f64),
    /// Run every physics and cell check.
    RunChecks,
    /// The comparison tab shows a table that is not the one its choices ask for: make it.
    Compare,
    /// The comparison's part; the row whose sheet shows.
    ComparePart(usize),
    CompareRow(usize),
}

// ===========================================================================
// Force
// ===========================================================================

/// What a click on the part becomes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ForceKind {
    /// Held in place: a clamped face, a bolted hole.
    #[default]
    Fixed,
    /// A force (N) spread over the region.
    Force,
    /// A pressure (MPa) pushing into the surface.
    Pressure,
    /// A torque (N mm) about the region's axis (a hole's, else its normal).
    Torque,
}

impl ForceKind {
    pub fn label(self) -> &'static str {
        match self {
            ForceKind::Fixed => "Fixed",
            ForceKind::Force => "Force",
            ForceKind::Pressure => "Pressure",
            ForceKind::Torque => "Torque",
        }
    }
    /// The unit its value is in.
    pub fn unit(self) -> &'static str {
        match self {
            ForceKind::Fixed => "",
            ForceKind::Force => "N",
            ForceKind::Pressure => "MPa",
            ForceKind::Torque => "N mm",
        }
    }

    /// The sizes the Force strip's dial spans (none for a fixture).
    pub fn sizes(self) -> Option<(f64, f64)> {
        match self {
            ForceKind::Fixed => None,
            ForceKind::Force => Some((0.1, 100_000.0)),
            ForceKind::Pressure => Some((0.001, 1000.0)),
            ForceKind::Torque => Some((1.0, 10_000_000.0)),
        }
    }

    /// A new load of this kind and `size` on a region of outward `normal`: a force pushing
    /// into it, a pressure pushing into it, a torque turning about its axis.
    pub fn value(self, size: f64, normal: [f64; 3]) -> [f64; 3] {
        match self {
            ForceKind::Fixed => [0.0; 3],
            ForceKind::Force => normal.map(|v| (-size * v * 100.0).round() / 100.0),
            ForceKind::Pressure | ForceKind::Torque => [size, 0.0, 0.0],
        }
    }
}

/// How a click grew its region: a face (normals that turn slowly) or a hole's wall (Fab reads
/// which from the surface; Alt takes the other).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ForcePick {
    #[default]
    Face,
    Hole,
}

/// What the part is coloured by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ForceField {
    #[default]
    VonMises,
    /// The largest principal stress: tension red, compression blue.
    Principal,
    /// How much of its strength the printed material uses (1 / the safety factor).
    Safety,
    Displacement,
}

impl ForceField {
    pub const ALL: [ForceField; 4] = [ForceField::VonMises, ForceField::Principal, ForceField::Safety, ForceField::Displacement];
    pub fn label(self) -> &'static str {
        match self {
            ForceField::VonMises => "Von Mises",
            ForceField::Principal => "Principal",
            ForceField::Safety => "Safety factor",
            ForceField::Displacement => "Displacement",
        }
    }
    /// Its place in [`fab_force::display::Display::values`].
    pub fn index(self) -> usize {
        match self {
            ForceField::VonMises => fab_force::display::VON_MISES,
            ForceField::Principal => fab_force::display::PRINCIPAL,
            ForceField::Safety => fab_force::display::UTILISATION,
            ForceField::Displacement => fab_force::display::DISPLACEMENT,
        }
    }
}

/// A change to a part's force study (its script's `m.fixed`, `m.force`, `m.pressure`,
/// `m.torque` statements); a load is known by its index in the part's study.
#[derive(Clone, Debug, PartialEq)]
pub enum StudyEdit {
    /// A new fixture or load on the face, or the hole's wall, through `at` (printer mm), with
    /// its value (see [`ForceLoad::value`]).
    Add { kind: ForceKind, pick: ForcePick, at: [f64; 3], value: [f64; 3] },
    /// A load's value (see [`ForceLoad::value`]).
    Set { index: usize, value: [f64; 3] },
    Remove { index: usize },
    /// Every fixture and load the Force tab can edit.
    Clear,
}

/// One fixture or load as the panel lists it: a statement of the part's script.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForceLoad {
    pub kind: ForceKind,
    /// Where, as the script writes it (`holes[1]`, `Face{at: [..]}`).
    pub on: String,
    /// A statement of its own at the script's top: the panel edits and removes it (one in a
    /// loop or a function is edited in the code).
    pub top: bool,
    /// Force: the vector (N, printer frame); Pressure: [MPa, 0, 0]; Torque: [N mm, 0, 0].
    pub value: [f64; 3],
    /// Its region on the surface shown: area (mm^2), centre, mean outward normal, and a
    /// hole's axis.
    pub area: f64,
    pub centroid: [f64; 3],
    pub normal: [f64; 3],
    pub axis: Option<[f64; 3]>,
}

impl ForceLoad {
    /// How big it is: a force's length, a pressure's or a torque's size (its sign apart).
    pub fn size(&self) -> f64 {
        match self.kind {
            ForceKind::Fixed => 0.0,
            ForceKind::Force => self.value.iter().map(|v| v * v).sum::<f64>().sqrt(),
            ForceKind::Pressure | ForceKind::Torque => self.value[0].abs(),
        }
    }

    /// Its value at `size`: the same direction (a force's), the same sign (a pull, a turn the
    /// other way).
    pub fn sized(&self, size: f64) -> [f64; 3] {
        let now = self.size();
        match self.kind {
            ForceKind::Fixed => self.value,
            ForceKind::Force if now > 0.0 => self.value.map(|v| (v * size / now * 100.0).round() / 100.0),
            ForceKind::Force => ForceKind::Force.value(size, self.normal),
            ForceKind::Pressure | ForceKind::Torque => [if self.value[0] < 0.0 { -size } else { size }, 0.0, 0.0],
        }
    }
}

/// A solve's answer as the Force view shows it: its fields over the part, and the deformed
/// mesh's surface (where the exaggerated deformation shows, and what its colour scale is of).
#[derive(Clone, Debug)]
pub struct ForceAnswer {
    pub values: Arc<fab_force::display::Values>,
    pub display: Arc<fab_force::display::Display>,
}

/// What the Force view shows over the studied part (the plate view draws the part itself, as
/// it always does).
#[derive(Clone, Debug)]
pub struct ForceScene {
    /// Changes whenever the answer or the marks do.
    pub revision: u64,
    /// The solve shown, none before the first.
    pub answer: Option<ForceAnswer>,
    /// The loads' regions and what a click of the Force tool would take: 1 fixed, 2 force, 3
    /// pressure, 4 torque, 4 more for the focused load's.
    pub marks: Arc<fab_force::display::Marks>,
    /// The part's box (printer mm).
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    /// Where the safety factor is lowest away from the fixtures, and that factor.
    pub weakest: Option<([f32; 3], f64)>,
    /// Where the von Mises stress is largest (its cell's centre), the point `fab.force` gives.
    pub peak: Option<[f32; 3]>,
}

impl PartialEq for ForceScene {
    fn eq(&self, o: &Self) -> bool {
        self.revision == o.revision && Arc::ptr_eq(&self.marks, &o.marks)
    }
}

/// Design's Force render mode as its panel and the plate view show it (the app's study fills
/// it).
#[derive(Clone, Debug, PartialEq)]
pub struct ForceView {
    /// The Force render mode is on (Design shows it: [`AppState::force_shown`]).
    pub active: bool,
    /// The part studied ("" none) and its index among the plate's parts; `note` says why there
    /// is nothing to study.
    pub part: String,
    pub index: Option<usize>,
    pub note: String,
    /// The filament families the study has strength data for (what "Study as" offers).
    pub filaments: Vec<String>,
    /// What the part prints in, as the study reads it, and where that is set (the Job tab's
    /// COLOURS), in a line: "PETG · the spool in L · A2 (blue)".
    pub material: String,
    /// The part's filament type when the study has no strength data for it: the panel offers
    /// "Study as", and nothing is solved until one is picked.
    pub strengthless: Option<String>,
    /// What a filament without strength data is studied as (`filaments`' order).
    pub study_as: Option<usize>,
    /// The material as the part will print (walls, skins, infill, weak layers), or as solid
    /// plastic.
    pub as_printed: bool,
    /// The print settings the material follows, in a line.
    pub print: String,
    /// What a click of the Force tool would take where the pointer is ("Hole wall, 3.0 mm
    /// across: click to fix it"), for the plate view's floating tip; empty off the part.
    pub hover: String,
    /// The load whose typed editor is open (a double-click on it with the Force tool).
    pub typing: Option<usize>,
    /// A handle drag's value for the load it holds, drawn until it is let go (the script gets
    /// it then).
    pub held: Option<(usize, [f64; 3])>,
    /// The pointer is on a handle of the picked load, or drags one: the view's tip stays down
    /// (a click there takes the handle).
    pub grip: bool,
    /// The part's fixtures and loads, as its script writes them.
    pub loads: Vec<ForceLoad>,
    /// The load the feature tree picked (its region shows strongest).
    pub focus: Option<usize>,
    pub field: ForceField,
    /// How much the deformed shape shows its displacement: 0 not at all, 1 the largest
    /// displacement at [`FORCE_DEFORM_SHARE`] of the part's size.
    pub deform: f64,
    pub status: String,
    /// The job on the board the panel's busy line follows (the infill floor's while it runs,
    /// else the study's), and whether the study was stopped by its Cancel (Refine goes on).
    pub busy: Option<makepad_widgets_core::jobs::JobId>,
    pub stopped: bool,
    /// What the busy line's Refine offers when the study is done with its safety factor not
    /// converged: one cell size finer, with about how long it takes ("Refine (~25 s)").
    pub refine: Option<String>,
    /// The study waits for the slicer, which goes first (its job has not started while a
    /// slice runs).
    pub waiting: bool,
    /// The AI hub the study solves on: none answering, the busy line offers to start it.
    pub hub: ForceHub,
    pub solving: bool,
    /// The study's results as (label, value) rows, and how the one shown was solved (what it
    /// sampled, its cells, the solve and its times: the busy line's tip).
    pub stats: Vec<(String, String)>,
    pub how: Vec<(String, String)>,
    pub scene: Option<Arc<ForceScene>>,
    /// The infill floor, found on request ([`ForceAction::FindFloor`]).
    pub floor: ForceFloor,
}

/// The AI hub Force solves on, as the panel tells it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ForceHub {
    /// One answers (or none was needed yet).
    #[default]
    Answering,
    /// None answers: the study waits for one, and the busy line offers to start it
    /// ([`ForceAction::StartHub`]).
    Missing,
    /// Started from the panel, not answering yet.
    Starting,
}

/// The infill floor of the part studied: the lowest sparse infill density at which its lowest
/// safety factor stays at 1 (under it the part breaks) and at 2, and the safety factor at other
/// wall loop counts, at the Medium cells.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForceFloor {
    /// Its state in a line ("Infill floor (Medium, 1.06 mm cells): SF 1 at 6 %, SF 2 at 24 %",
    /// what it is trying, or why it cannot run); empty before the first request.
    pub status: String,
    pub running: bool,
    pub rows: Vec<(String, String)>,
}

impl ForceView {
    /// The study's state in a line, with its wait for the slicer.
    pub fn status_line(&self) -> String {
        match self.waiting {
            true => format!("{} (waiting for the slicer, which goes first)", self.status.trim_end_matches('.')),
            false => self.status.clone(),
        }
    }

    /// The study's state as the plate view heads it: the status up to its first colon (what is
    /// missing or what went wrong; the Force panel says the rest), with its wait for the slicer.
    pub fn headline(&self) -> String {
        let head = self.status.split_once(": ").map_or(self.status.as_str(), |(h, _)| h).trim_end_matches('.');
        match self.waiting {
            true => format!("{head} (waiting for the slicer, which goes first)"),
            false => head.to_string(),
        }
    }
}

/// The share of the part's size the largest displacement shows as at full deformation.
pub const FORCE_DEFORM_SHARE: f64 = 0.1;

impl Default for ForceView {
    fn default() -> Self {
        ForceView {
            active: false,
            part: String::new(),
            index: None,
            note: String::new(),
            filaments: Vec::new(),
            material: String::new(),
            strengthless: None,
            study_as: None,
            as_printed: true,
            print: String::new(),
            hover: String::new(),
            typing: None,
            held: None,
            grip: false,
            loads: Vec::new(),
            focus: None,
            field: ForceField::default(),
            deform: 0.0,
            status: String::new(),
            busy: None,
            stopped: false,
            refine: None,
            waiting: false,
            hub: ForceHub::Answering,
            solving: false,
            stats: Vec::new(),
            how: Vec::new(),
            scene: None,
            floor: ForceFloor::default(),
        }
    }
}

/// What the Force panel and the plate view ask the app's study to do.
#[derive(Clone, Debug, PartialEq)]
pub enum ForceAction {
    /// Study a filament without strength data as this family (`ForceView::filaments`' order).
    StudyAs(usize),
    AsPrinted(bool),
    /// A click of the Force tool on the part: the point under it (printer mm, undeformed);
    /// `flip` (Alt held) takes the other reading of face or hole. On a load it picks that
    /// load, and `edit` (a double-click) opens its typed editor ([`ForceView::typing`]);
    /// elsewhere it adds one (not on a double-click).
    Place { point: [f64; 3], flip: bool, edit: bool },
    /// The typed editor open on load `index`, or closed.
    Type(Option<usize>),
    /// The Force tool's pointer over the part (the point under it, and whether Alt flips the
    /// reading), or off it: what a click would take shows.
    Hover { at: Option<([f64; 3], bool)> },
    Remove(usize),
    /// A load's value (see [`ForceLoad::value`]).
    Value(usize, [f64; 3]),
    /// The feature tree picked load `index` of plate part `object`'s study (none: a solid).
    Focus { object: String, index: Option<usize> },
    Field(ForceField),
    Deform(f64),
    /// Every fixture and load off.
    Clear,
    /// Find the infill floor of the study as it stands ([`ForceFloor`]).
    FindFloor,
    /// Go on with a study its Cancel stopped.
    Refine,
    /// Start this machine's AI Hub app, which the study waits for ([`ForceHub::Missing`]).
    StartHub,
}

impl Default for AppState {
    fn default() -> Self {
        let scene = crate::viewport::scene_gpu::SceneGpu::default();
        AppState {
            field_loop_stops: Default::default(),
            field_loop_marks: 0,
            scene: Arc::new(Scene::empty()),
            elec_scene: None,
            elec_poses: None,
            elec_scene_committed: true,
            elec_previewing: false,
            scene_coming: false,
            elec_selected: Arc::new([]),
            elec_spot: None,
            drawn_by_link: Vec::new(),
            place_hidden: Vec::new(),
            part_glows: Arc::new([]),
            part_presses: Arc::new([]),
            part_turns: Arc::new([]),
            scene_state: SceneState::default(),
            view: ViewportState::default(),
            sun: SkyState::default(),
            load: LoadStatus::Idle,
            progress_card: None,
            plates: PlateList::default(),
            ui: UiState::default(),
            tool: Tool::Select,
            snap: SnapOptions::default(),
            measurements: Vec::new(),
            test_prints: Vec::new(),
            selected_measure: None,
            scene_revision: 0,
            units: Units {
                source_to_meters: 1.0,
                display: LengthUnit::Millimeter,
                precision: 1,
            },
            print: Default::default(),
            plate: PlateInfo::default(),
            glass_parts: GlassParts::default(),
            params: Default::default(),
            source: Default::default(),
            mech: Default::default(),
            arm: ArmView::default(),
            scan: ScanView::default(),
            force: ForceView::default(),
            sdf: Some(scene.sdf.clone()),
            scene_gpu: Rc::new(RefCell::new(scene)),
            pose_sets: vec![PoseSet::default()],
            device: DeviceView::default(),
            model_sources: crate::model_sources::load(),
            firmware_net: crate::firmware_net::load(),
            firmware_tokens: crate::firmware_net::tokens(),
            picks: Default::default(),
        }
    }
}

/// The box a camera should frame. `Scene::bounds` stays the truth for culling and clip planes,
/// but a few stray vertices (an imported mesh with a decode artefact far from the rest) must not
/// push the camera away from what is really there. Per axis of a vertex sample, up to 4 % of the
/// vertices at either end go, and only when they are further from the rest than the rest is
/// wide: a low-poly part beside a dense one is a small share of the vertices and stays. Parts
/// drawn from a distance field have no vertices: their boxes count whole.
pub fn framing_bounds(scene: &Scene) -> Aabb {
    let stride = crate::model::VERTEX_STRIDE;
    let total: usize = scene.batches.iter().map(|b| b.vertices.len() / stride).sum();
    if total < 64 {
        return scene.bounds;
    }
    let step = (total / 200_000).max(1);
    let mut axes: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for b in &scene.batches {
        let v = &b.vertices;
        for i in (0..v.len() / stride).step_by(step) {
            for (k, axis) in axes.iter_mut().enumerate() {
                let x = v[i * stride + k];
                if x.is_finite() {
                    axis.push(x);
                }
            }
        }
    }
    let mut lo = [0.0f32; 3];
    let mut hi = [0.0f32; 3];
    for (k, a) in axes.iter_mut().enumerate() {
        let n = a.len();
        if n < 64 {
            return scene.bounds;
        }
        // Only the two ends need their order.
        let tail = ((n - 1) as f32 * 0.04) as usize;
        let upper = n - 1 - tail;
        a.select_nth_unstable_by(tail, f32::total_cmp);
        a[tail..].select_nth_unstable_by(upper - tail, f32::total_cmp);
        a[..tail].sort_unstable_by(f32::total_cmp);
        a[upper + 1..].sort_unstable_by(f32::total_cmp);
        // Each end's widest gap (below `i + 1`) is where a stray cluster would stop.
        let widest = |from: usize, to: usize| (from..to).max_by(|&i, &j| (a[i + 1] - a[i]).total_cmp(&(a[j + 1] - a[j]))).unwrap_or(from);
        let first = widest(0, tail) + 1;
        let last = widest(upper, n - 1);
        let rest = a[last] - a[first];
        lo[k] = if a[first] - a[first - 1] > rest { a[first] } else { a[0] };
        hi[k] = if a[last + 1] - a[last] > rest { a[last] } else { a[n - 1] };
    }
    let meshes = Aabb {
        min: vec3(lo[0], lo[1], lo[2]),
        max: vec3(hi[0], hi[1], hi[2]),
    };
    scene.elements.iter().filter(|e| !e.has_geometry()).fold(meshes, |b, e| aabb_union(&b, &e.bounds))
}

impl AppState {
    /// Give an editor the lowest available slot after the two stage panes, for `role`.
    pub fn lease_pane(&mut self, role: PaneRole) -> PaneLease {
        self.drain_released_panes();
        let id = self.view.add_pane(PaneLink::Free);
        let pane = self.view.pane_mut(id);
        pane.parked = true;
        pane.role = role;
        PaneLease { id, role, released: self.view.released_panes.clone(), parked: self.view.parked_panes.clone() }
    }

    /// Retire dropped editor leases before dispatch or reusing a slot in the same event.
    pub fn drain_released_panes(&mut self) {
        let parked = std::mem::take(&mut *self.view.parked_panes.borrow_mut());
        for pane in parked {
            self.park_pane(pane);
        }
        let released = std::mem::take(&mut *self.view.released_panes.borrow_mut());
        for pane in released {
            self.release_pane(pane);
        }
    }

    pub(crate) fn park_pane(&mut self, id: PaneId) {
        let Some(pane) = self.view.panes.get_mut(id.0 as usize).and_then(Option::as_mut) else { return };
        pane.parked = true;
        pane.interacting = false;
        pane.pointer_held = false;
        self.let_go_of_host(id);
        let mut scene = self.scene_gpu.borrow_mut();
        scene.sdf.borrow_mut().drop_pane(id);
        if scene.hovered_pane == Some(id) {
            scene.hovered_pane = None;
            self.view.hover = None;
        }
        if scene.navigation_pane == Some(id) {
            scene.navigation_pane = None;
            scene.camera_moving = false;
        }
    }

    /// The scene host is the pane whose draw runs the work all panes share (the studio and
    /// environment, SSAO, the element lookup): a pane that stops drawing lets go of it, and the
    /// next pane to draw takes it ([`crate::viewport::scene_gpu::SceneGpu::take_host`]). O(1).
    pub(crate) fn let_go_of_host(&mut self, id: PaneId) {
        let mut scene = self.scene_gpu.borrow_mut();
        if scene.host == Some(id) {
            scene.renderer.set_ssao(None);
            scene.renderer.set_element_lookup(None, 0);
            scene.host = None;
        }
    }

    // Editors reach here only from their dropped lease, so a live handle cannot name a reused slot.
    fn release_pane(&mut self, id: PaneId) {
        if id == PaneId::PLATE_ZERO { return; }
        if self.device.pane == Some(id) {
            self.device.pane = None;
            if self.device.placed {
                // The slot can be reused before the Device editor leases another pane.
                self.device.restore.get_or_insert(DeviceRestore::Camera(*self.view.camera(id)));
                self.device.placed = false;
            }
        }
        self.park_pane(id);
        for pane in self.view.panes.iter_mut().flatten() {
            if pane.link.source() == Some(id) {
                pane.link = PaneLink::Free;
            }
        }
        self.view.panes[id.0 as usize] = None;
        if !self.view.panes.iter().flatten().any(|pane| !pane.parked && pane.mode == PaneMode::Traced) {
            self.scene_gpu.borrow_mut().product_scene = None;
        }
    }

    /// A later Plate view's pane changed its link or mode (its chip, See through): its instance
    /// keeps them, by Plate view numbers, for the next bind and the file. O(Plates).
    fn keep_plate_view(&mut self, pane: PaneId) {
        let Some(n) = self.view.plate_number(pane).filter(|n| *n > 0) else { return };
        let Some(p) = self.view.panes.get(pane.0 as usize).and_then(Option::as_ref) else { return };
        let link = p.link.how().zip(p.link.source().and_then(|s| self.view.plate_number(s)));
        let traced = p.mode == PaneMode::Traced;
        let instance = crate::ui::areas::Instance { editor: crate::ui::editors::Editor::Viewport, n };
        self.ui.workspaces.set_plate_view(instance, link, traced);
    }

    /// A later Plate view's pane as its instance keeps them, at its bind: its link (to its
    /// source's pane, else free) and mode. Written straight to the pane: nothing is kept back.
    pub fn show_plate_view(&mut self, pane: PaneId, link: PaneLink, traced: bool) {
        let link = self.view.resolve_link(pane, link).unwrap_or(PaneLink::Free);
        let mode = if traced { PaneMode::Traced } else { PaneMode::Raster };
        let Some(p) = self.view.panes.get_mut(pane.0 as usize).and_then(Option::as_mut) else { return };
        p.link = link;
        if p.mode != mode {
            p.mode = mode;
            p.trace = TraceProgress::default();
            if !self.view.panes.iter().flatten().any(|p| !p.parked && p.mode == PaneMode::Traced) {
                self.scene_gpu.borrow_mut().product_scene = None;
            }
        }
    }

    /// A new pose set, every part at rest, for panes to show.
    pub fn add_pose_set(&mut self) -> PoseSetId {
        self.pose_sets.push(PoseSet::default());
        PoseSetId(u16::try_from(self.pose_sets.len() - 1).expect("pose set count exceeds PoseSetId range"))
    }

    pub fn pose_set(&self, id: PoseSetId) -> &PoseSet {
        &self.pose_sets[id.0 as usize]
    }

    pub fn pose_set_mut(&mut self, id: PoseSetId) -> &mut PoseSet {
        &mut self.pose_sets[id.0 as usize]
    }

    /// Show `pane`'s parts at `poses`. Returns whether the pane changed its pose source.
    pub fn set_pane_poses(&mut self, pane: PaneId, poses: PanePoses) -> bool {
        let Some(p) = self.view.panes.get_mut(pane.0 as usize).and_then(Option::as_mut) else { return false };
        let changed = p.poses != poses;
        p.poses = poses;
        changed
    }

    /// What `pane`'s parts show at (the stage's for a vacant slot).
    pub fn pane_poses(&self, pane: PaneId) -> PanePoses {
        self.view.panes.get(pane.0 as usize).and_then(Option::as_ref).map_or(PanePoses::Stage, |p| p.poses)
    }

    /// Where `element` shows under `poses`, when not at rest. O(1).
    pub fn pose_under(&self, poses: PanePoses, element: ElementId) -> Option<crate::model::state::Pose> {
        match poses {
            PanePoses::Stage => self.scene_state.pose(element),
            PanePoses::Set(id) => self.pose_set(id).get(element),
        }
    }

    /// Where `element` shows in `pane`, when not at rest. O(1).
    pub fn pose_in(&self, pane: PaneId, element: ElementId) -> Option<crate::model::state::Pose> {
        self.pose_under(self.pane_poses(pane), element)
    }

    /// Every element `pane` shows away from rest, each once. O(posed).
    pub fn posed_in(&self, pane: PaneId) -> Vec<(ElementId, crate::model::state::Pose)> {
        match self.pane_poses(pane) {
            PanePoses::Stage => self.scene_state.posed(),
            PanePoses::Set(id) => self.pose_set(id).iter().collect(),
        }
    }

    /// What a key of `poses` holds beyond the scene state's revision: the set and its revision.
    pub fn poses_key(&self, poses: PanePoses) -> Option<(PoseSetId, u64)> {
        match poses {
            PanePoses::Stage => None,
            PanePoses::Set(id) => Some((id, self.pose_set(id).revision())),
        }
    }

    /// Whether any unparked pane shows the device assembly.
    pub fn device_shown(&self) -> bool {
        self.view.panes.iter().flatten().any(|p| !p.parked && p.poses == PanePoses::Set(PoseSetId::DEVICE))
    }

    /// The pose source shared by fields, See-Through, the pipe list's surface test, the traced
    /// scene and the headset. Uses the first set shown by an unparked pane, else the stage's
    /// poses. O(panes).
    pub fn shared_poses(&self) -> PanePoses {
        self.view.panes.iter().flatten().filter(|p| !p.parked).find_map(|p| matches!(p.poses, PanePoses::Set(_)).then_some(p.poses)).unwrap_or_default()
    }

    /// Where `element` shows in the shared layers ([`Self::shared_poses`]), when not at rest.
    pub fn shared_pose(&self, element: ElementId) -> Option<crate::model::state::Pose> {
        self.pose_under(self.shared_poses(), element)
    }

    /// One of the device's case parts that `pane`'s Case toggle leaves out. O(1).
    pub fn case_hidden_in(&self, pane: PaneId, id: ElementId) -> bool {
        self.view.panes.get(pane.0 as usize).and_then(Option::as_ref).is_some_and(|p| p.case_hidden && self.device.case.contains(&id))
    }

    /// The view as this machine's preferences have it (the look chosen last).
    pub fn apply_view_prefs(&mut self) {
        if let Some(look) = crate::view_prefs::load_look() {
            self.view.look = look;
        }
        if let Some(lighting) = crate::view_prefs::load_lighting() {
            self.view.lighting = lighting;
        }
        if let Some(lighting) = crate::view_prefs::load_board_lighting() {
            self.view.board_lighting = lighting;
        }
        if let Some(shading) = crate::view_prefs::load_shading() {
            self.view.shading = shading;
        }
        if let Some(overlays) = crate::view_prefs::load_overlays() {
            self.view.overlays = overlays;
        }
        self.print.play.active = crate::view_prefs::load_slice();
        self.print.finish = crate::view_prefs::load_surface();
        self.ui.arm_shown = crate::view_prefs::load_arm();
        self.ui.scan_shown = crate::view_prefs::load_scan();
        self.ui.workspaces = crate::workspaces::Workspaces::load();
        // a workspace that is not offered now (its family switched off) does not come back
        if !self.ui.offered(self.ui.workspaces.current()) {
            self.ui.enter(Workspace::Design);
        }
        if let Some(mode) = self.ui.workspaces.current().mode {
            self.ui.plate_mode = mode;
        }
        self.ui.scan_colour = crate::view_prefs::load_scan_colour();
        self.ui.scan_cameras = crate::view_prefs::load_scan_cameras();
        let ortho = crate::view_prefs::load_ortho();
        for pane in self.view.plate_panes() {
            crate::nav::orbit::set_ortho(self.view.camera_mut(pane), ortho);
        }
    }

    /// Whether the workspace is offered now: an experimental one only while View › Experimental
    /// has it on.
    pub fn workspace_shown(&self, w: Workspace) -> bool {
        !w.experimental() || (w == Workspace::Arm && self.ui.arm_shown) || (w == Workspace::Scan && self.ui.scan_shown)
    }

    /// The render mode the view's Normal shows: the parts' look (View › Shading).
    pub fn normal_mode(&self) -> RenderMode {
        match self.view.look {
            PartLook::Field => RenderMode::Field,
            PartLook::Mesh => RenderMode::Mesh,
        }
    }

    /// The plate view's render mode.
    pub fn render_mode(&self) -> RenderMode {
        if self.force_shown() {
            RenderMode::Force
        } else if self.print.play.active {
            RenderMode::Slice
        } else {
            match self.view.look {
                PartLook::Field => RenderMode::Field,
                PartLook::Mesh => RenderMode::Mesh,
            }
        }
    }

    /// The slice view shows the print and parts cannot be edited under it: only the printer's
    /// Monitor. In Design the plate stays editable under the slice.
    pub fn slice_blocks_edits(&self) -> bool {
        (self.print.play.active && !self.ui.in_mode(PlateMode::Design)) || self.print.monitor_holds() || self.plate.whole.is_some()
    }

    /// The Program beside the plate or gone (the code toggle): a split of the area holding the
    /// plate at the width last kept, or its tab closed, as ops on the shown workspace; the toggle
    /// and the width are kept on this machine for a new Design. False when nothing changed (no
    /// plate on screen, or it is so already). O(tabs), on a toggle.
    fn show_code(&mut self, on: bool) -> bool {
        let tree = &self.ui.workspaces.current().tree;
        let (_, width) = crate::view_prefs::load_code();
        let op = match (on, tree.showing(Editor::Code)) {
            (true, None) => match tree.showing(Editor::Viewport) {
                Some(area) => WorkspaceOp::Split {
                    area,
                    side: crate::ui::areas::Side::Right,
                    editor: Editor::Code,
                    align: SplitterAlign::FromB(width.unwrap_or(crate::workspaces::CODE_START_W)),
                },
                None => return false,
            },
            (false, Some(area)) => {
                let Some(instance) = tree.stack_of(area).and_then(|(tabs, _)| tabs.into_iter().find(|i| i.editor == Editor::Code)) else { return false };
                // the width it was left at, for the next time it comes
                let kept = match tree.parent_split(area) {
                    Some((_, SplitterAxis::Horizontal, SplitterAlign::FromB(w), true)) => Some(w),
                    _ => width,
                };
                crate::view_prefs::save_code(false, kept);
                WorkspaceOp::CloseTab { area, instance }
            }
            _ => return false,
        };
        if on {
            crate::view_prefs::save_code(true, None);
        }
        let done = self.ui.workspaces.edit(op).is_ok();
        if done {
            self.ui.workspaces.save();
        }
        done
    }

    /// What follows the plate mode in force, once it moved from `before` to [`UiState::mode`]
    /// (a workspace chosen; the same mode again still settles the gizmo and Monitor's copy).
    /// A workspace change `f` makes, and what follows the mode it moves; a tool's stroke in
    /// progress ends first. True (the caption and the views redraw).
    fn switch_workspace(&mut self, f: impl FnOnce(&mut UiState)) -> bool {
        crate::tools::paint::cancel();
        crate::tools::sculpt::cancel();
        let before = self.ui.mode();
        f(&mut self.ui);
        self.mode_moved(before);
        true
    }

    fn mode_moved(&mut self, before: Option<PlateMode>) {
        use PlateMode::*;
        let after = self.ui.mode();
        if before == Some(Monitor) {
            self.swap_monitor_view();
        }
        // The sim's view has no slice; it comes back with the mode after it.
        if after == Some(Mechanics) && before != Some(Mechanics) {
            self.ui.slice_after_mechanics = self.print.play.active;
            self.print.play.active = false;
            self.print.play.playing = false;
        } else if before == Some(Mechanics) && after != Some(Mechanics) {
            self.print.play.active = self.ui.slice_after_mechanics;
        }
        if after == Some(Monitor) {
            self.swap_monitor_view();
        }
        // Move, turn and scale stay chosen across modes; Shape, Paint, Sculpt and Force are Design's.
        self.ui.gizmo = match (after, self.ui.gizmo) {
            (Some(Monitor), GizmoMode::Paint | GizmoMode::Sculpt | GizmoMode::Force) => GizmoMode::Off,
            // Mechanics grabs parts with the pointer: no gizmo.
            (Some(Mechanics), _) => GizmoMode::Off,
            (_, g) => g,
        };
        self.print.follow.monitor = after == Some(Monitor);
        // No job: Monitor looks at what the other modes look at, from its own copy.
        if after == Some(Monitor) && !self.print.monitor_holds() {
            self.print.play = self.print.follow.hidden_play.clone();
        }
    }

    /// Monitor's view is its own: its playback and camera swap with the other modes'.
    fn swap_monitor_view(&mut self) {
        std::mem::swap(&mut self.print.play, &mut self.print.follow.hidden_play);
        for id in self.view.plate_panes() {
            let pane = self.view.pane_mut(id);
            let hidden = pane.hidden_view.get_or_insert((pane.camera, pane.preset));
            std::mem::swap(&mut pane.camera, &mut hidden.0);
            std::mem::swap(&mut pane.preset, &mut hidden.1);
        }
    }

    /// Whether the plate's views draw the print preview: in Design mode its Sliced render mode
    /// and in Monitor mode the job do. Mechanics shows the model as it is, whatever render mode
    /// Design was left in, and so do the 3D views of workspaces without a Plate (Electronics,
    /// Scan, Arm). O(1).
    pub fn slice_workspace(&self) -> bool {
        matches!(self.ui.mode(), Some(PlateMode::Design | PlateMode::Monitor))
    }

    /// The print preview this workspace's views draw ([`Self::slice_workspace`]).
    pub fn slice_shown(&self) -> Option<&Arc<crate::print::PrintPreview>> {
        self.print.showing().filter(|_| self.slice_workspace())
    }

    /// The resin layers this workspace's views draw ([`Self::slice_workspace`]).
    pub fn resin_shown(&self) -> Option<&crate::print::resin::ResinShow> {
        self.print.resin_shown().filter(|_| self.slice_workspace())
    }

    /// Design's slice view shows the toolpaths in the parts' place: the parts stay selectable
    /// and editable, and only inserts are drawn (unless "Show parts" is on).
    pub fn parts_under_slice(&self) -> bool {
        self.ui.in_mode(PlateMode::Design) && self.print.showing().is_some() && !self.print.play.show_model
    }

    /// The plate's boards ([`PlatePart::is_board`]): None without one, else whether any shows.
    /// O(parts).
    pub fn board_shown(&self) -> Option<bool> {
        let mut boards = self.plate.parts.iter().filter(|p| p.is_board()).peekable();
        boards.peek()?;
        Some(boards.any(|p| !p.hidden))
    }

    /// The Force render mode shows: Design's view with the studied part's stress.
    pub fn force_shown(&self) -> bool {
        self.force.active && self.ui.in_mode(PlateMode::Design)
    }

    /// The Force tool is the tool: a click on the studied part adds `ui.force_kind`.
    pub fn force_tool(&self) -> bool {
        self.ui.gizmo == GizmoMode::Force && self.tool == Tool::Select && self.force_shown()
    }

    /// The plate part the Force view shows a study over, if any.
    pub fn heatmap_part(&self) -> Option<usize> {
        self.force.index.filter(|_| self.force_shown() && self.force.scene.is_some())
    }

    /// The exaggerated deformation shows: the one view of a study that needs a mesh.
    pub fn force_deforms(&self) -> bool {
        self.force.deform > 0.0 && self.heatmap_part().is_some() && self.force.scene.as_ref().is_some_and(|s| s.answer.is_some())
    }

    /// The deformed mesh stands in for this element: the plate view draws it, not the part.
    pub fn under_deformed(&self, id: ElementId) -> bool {
        self.force_deforms() && self.heatmap_part().is_some_and(|i| self.plate.part_of(id) == Some(i))
    }

    /// Whether the shared layers draw this element opaque, with the primary pane's Case and floor settings.
    pub fn is_drawn(&self, id: ElementId) -> bool {
        self.is_drawn_under(id, self.view.lead_plate(), self.shared_poses())
    }

    /// An item printed into its part that the Sliced view's top layer has not reached the pause
    /// for: it is not in the print yet, so it is not drawn. At the layer the print pauses after,
    /// it stands in place.
    pub fn embed_waits(&self, id: ElementId) -> bool {
        let Some(i) = self.plate.part_of(id).filter(|&i| self.plate.parts[i].embed) else { return false };
        let Some(pv) = self.print.showing().filter(|_| self.parts_under_slice()) else { return false };
        let top = crate::print::timeline::shown_top_layer(&self.print, pv);
        let name = &self.plate.parts[i].name;
        pv.pauses.iter().find(|p| p.items.contains(name)).is_some_and(|p| top + 1 < p.layer)
    }

    /// Whether `pane` draws the element opaque at its poses, with its Case and floor settings.
    pub fn is_drawn_in(&self, id: ElementId, pane: PaneId) -> bool {
        self.is_drawn_under(id, pane, self.pane_poses(pane))
    }

    fn is_drawn_under(&self, id: ElementId, pane: PaneId, poses: PanePoses) -> bool {
        !self.case_hidden_in(pane, id)
            && self.is_visible(id) && !self.embed_waits(id) && (self.plate.is_context(id) || !self.parts_under_slice() || self.plate.is_insert(id)) && !self.is_ghost_under(poses, id) && !self.is_glass_under(poses, id) && !self.under_deformed(id) && (self.floor_shown_in(pane) || !self.plate.is_floor(id)) && !self.view.see.drawn.contains(&id) && !self.drawn_by_link.contains(&id) && (self.mech.posed_only.is_empty() || self.plate.part_of(id).is_none_or(|i| !self.part_unposed(i)))
    }

    /// Whether this element draws as glass under `poses`.
    pub fn is_glass_under(&self, poses: PanePoses, id: ElementId) -> bool {
        // The ghost test is the only glass condition that depends on the pose source.
        self.glass_candidate(id) && !self.is_ghost_under(poses, id)
    }

    /// A mesh-backed insert host or transparent part eligible for glass before choosing a pose source.
    pub fn glass_candidate(&self, id: ElementId) -> bool {
        if !self.view.glass || !self.is_visible(id) || self.parts_under_slice() || self.drawn_by_link.contains(&id) {
            return false;
        }
        let Some(&(part, batch)) = self.glass_parts.elements.get(&id) else { return false };
        let transparent = batch.and_then(|b| self.scene.batches.get(b as usize))
            .and_then(|b| self.scene.materials.get(b.material.index())).is_some_and(|m| m.transparent);
        if !self.glass_parts.hosts.contains(&id) && !transparent {
            return false;
        }
        !self.plate.parts[part].arriving
            && !self.mech.check.ghosts.iter().any(|(e, _)| *e == id)
            && !(self.force_deforms() && self.heatmap_part() == Some(part))
            && !self.view.see.drawn.contains(&id)
            && !self.part_unposed(part)
    }

    /// A plate part Mechanics shows only where the sim has it ([`MechView::posed_only`]) that the
    /// sim's pose has not placed yet: never shown first where it printed.
    pub fn part_unposed(&self, part: usize) -> bool {
        let p = &self.plate.parts[part];
        self.mech.posed_only.contains(&p.object) && p.element.is_none_or(|e| self.scene_state.moves.as_ref().is_none_or(|m| !m.contains_key(&e)))
    }

    /// Whether View › See-Through draws this part under `poses` when its grid is ready.
    pub fn see_through_shows_under(&self, poses: PanePoses, id: ElementId) -> bool {
        self.view.see.on && self.is_visible(id) && !self.parts_under_slice() && !self.is_ghost_under(poses, id) && !self.under_deformed(id) && !self.plate.is_context(id) && !self.drawn_by_link.contains(&id)
    }

    /// An arriving part, a joint check's ghost, or a part with its eye off that is neither
    /// an insert nor assembled under `poses`. The sliced view leaves ghosts out.
    pub fn is_ghost_under(&self, poses: PanePoses, id: ElementId) -> bool {
        let part = self.glass_parts.elements.get(&id).map(|&(part, _)| part).or_else(|| self.plate.part_of(id));
        part.is_some_and(|i| self.plate.parts[i].arriving || (!self.plate.parts[i].printed && !self.plate.parts[i].insert && !self.assembled_under(poses, id)))
            || self.mech.check.ghosts.iter().any(|(e, _)| *e == id)
    }

    /// Whether `poses` includes this part in an assembly, even if it prints on another plate.
    pub fn assembled_under(&self, poses: PanePoses, id: ElementId) -> bool {
        match poses {
            PanePoses::Set(set) => self.pose_set(set).contains(id),
            PanePoses::Stage => false,
        }
    }

    /// The colour a ghost is drawn in: its part's, or a joint check's ghost's own.
    pub fn ghost_color(&self, id: ElementId) -> Option<[f32; 4]> {
        match self.plate.part_of(id) {
            Some(i) => Some(self.plate.parts[i].color),
            None => self.mech.check.ghosts.iter().find(|(e, _)| *e == id).map(|(_, c)| *c),
        }
    }

    /// Whether the plate view needs its ghost pass under `poses`.
    pub fn ghosts_drawn_under(&self, poses: PanePoses) -> bool {
        self.test_cut_shown() || self.plate.parts.iter().any(|p| p.element.is_some_and(|e| self.is_ghost_drawn_under(poses, e))) || self.mech.check.ghosts.iter().any(|(e, _)| self.is_ghost_drawn_under(poses, *e))
    }

    /// Whether this element draws in the plate view's ghost pass under `poses`.
    pub fn is_ghost_drawn_under(&self, poses: PanePoses, id: ElementId) -> bool {
        self.is_visible(id) && !self.parts_under_slice() && !self.drawn_by_link.contains(&id) && (self.is_ghost_under(poses, id) || (self.test_cut_shown() && self.plate.part_of(id).is_some()))
    }

    /// The view shows a test print's piece, picked: cut out in place, the rest of the plate a ghost.
    pub fn test_cut_shown(&self) -> bool {
        self.ui.in_mode(PlateMode::Design) && !self.parts_under_slice() && self.plate.whole.is_none() && crate::ui::test_prints::picked(self).is_some()
    }

    /// How much of a part with its eye off shows in the plate view.
    pub const GHOST_ALPHA: f32 = 0.25;

    /// How much of a part on its way in (a file dragged over the window) shows in the plate
    /// view: enough to read as that part, its edges drawn over it.
    pub const ARRIVING_ALPHA: f32 = 0.5;

    /// Install a freshly loaded scene: resets selection, visibility and
    /// measurements, frames the view on the new bounds.
    pub fn set_scene(&mut self, scene: Arc<Scene>) {
        let framing = framing_bounds(&scene);
        self.scene_state = SceneState::default();
        self.measurements.clear();
        self.selected_measure = None;
        let v = &mut self.view;
        for pane in v.plate_panes() {
            let camera = v.camera_mut(pane);
            *camera = Camera::default();
            if !aabb_is_empty(&framing) {
                camera.frame_bounds(&framing, 16.0 / 9.0);
            }
            v.mark_camera_changed(pane);
        }
        self.load = LoadStatus::Loaded {
            path: scene.source_path.clone(),
        };
        // Keep the user's display unit across loads; take the source scale
        // (informational) from the new scene.
        self.units.source_to_meters = scene.units.source_to_meters;
        self.scene = scene;
        self.scene_revision = self.scene_revision.wrapping_add(1);
        self.ui.notices.clear(NoticeView::Plate);
        self.sync_plate_elements();
    }

    /// How the plate on the bed is looked at now: what a switch back to it shows again.
    pub fn plate_view(&self) -> PlateView {
        let saved = |pane: PaneId| {
            let pane = self.view.pane(pane);
            match pane.hidden_view {
                Some(design) if self.print.monitor_preview().is_some() => design,
                _ => (pane.camera, pane.preset),
            }
        };
        let (camera, preset) = saved(PaneId::PLATE_ZERO);
        let name = |e: ElementId| self.scene.element(e).map(|e| e.name.clone());
        PlateView {
            camera,
            preset,
            plates: self.view.plate_instances.iter().map(|&(n, pane)| {
                let (camera, preset) = saved(pane);
                (n, camera, preset)
            }).collect(),
            device: match self.device.restore {
                Some(DeviceRestore::Camera(camera)) => Some(camera),
                Some(DeviceRestore::Frame) => None,
                None if self.device.placed => self.device.pane.map(|pane| *self.view.camera(pane)),
                None => None,
            },
            sheet: None,
            board: None,
            scopes: Vec::new(),
            selected: self.scene_state.selection.set.iter().filter_map(|e| name(*e)).collect(),
            active: self.scene_state.selection.active.and_then(name),
            viewed: self.source.viewed.clone(),
        }
    }

    /// Show the plate on the bed (its scene installed) as [`Self::plate_view`] took it: its
    /// parts still there stay selected.
    fn restore_view(&mut self, view: &PlateView, camera: bool) {
        let find = |name: &str| self.scene.elements.iter().find(|e| e.name == name).map(|e| e.id);
        let keep: Vec<ElementId> = view.selected.iter().filter_map(|n| find(n)).collect();
        let active = view.active.as_deref().and_then(find);
        self.scene_state.select_set(keep);
        self.scene_state.selection.active = active.filter(|a| self.scene_state.selection.set.contains(a));
        if !camera { return; }
        // Each Plate view at its own saved camera; one the file has none for (a file of one Plate
        // view, a Plate view opened since) at the first's. One not bound keeps its for its bind.
        self.view.saved_plates = view.plates.clone();
        let monitor = self.print.monitor_preview().is_some();
        let own = |n: u32| view.plates.iter().find(|(m, ..)| *m == n).map_or((view.camera, view.preset), |&(_, camera, preset)| (camera, preset));
        let shown: Vec<(PaneId, (Camera, Option<PresetView>))> = std::iter::once((PaneId::PLATE_ZERO, (view.camera, view.preset)))
            .chain(self.view.plate_instances.iter().map(|&(n, pane)| (pane, own(n))))
            .collect();
        for (pane, (camera, preset)) in shown {
            self.view.pane_mut(pane).restore_camera(camera, preset, monitor);
        }
    }

    /// What a Plate view put back at a saved camera, or kept at the last plate's, frames instead
    /// when at its own aspect that camera sees nothing of the scene (the plate's scripts changed
    /// since, or another plate is on the bed): the plate, as an opening frames it. None while it
    /// sees the scene, and in Monitor, which frames its own job. O(1).
    pub(crate) fn restored_frame(&self, pane: PaneId, aspect: f32) -> Option<Aabb> {
        if self.print.monitor_preview().is_some() || self.view.camera(pane).sees(self.scene.bounds, aspect) {
            return None;
        }
        Some(self.plate.view_box())
    }

    /// Install a rebuilt scene of the same model (see
    /// [`ShellAction::SceneUpdated`]). Element ids are not stable across
    /// rebuilds, so part selection is remapped and hiding resets; the camera
    /// stays unless `frame` asks to fit it to the new bounds.
    pub fn update_scene(&mut self, scene: Arc<Scene>, frame: bool) {
        // The same parts keep their selection across a rebuild (by name: the
        // scene names each part by its model object).
        let selected: Vec<String> = self.scene_state.selection.set.iter().filter_map(|e| self.scene.element(*e)).map(|e| e.name.clone()).collect();
        let active = self.scene_state.selection.active.and_then(|e| self.scene.element(e)).map(|e| e.name.clone());
        // and a resin slice's parts stay lifted (it sets the lift again, but not before this draws)
        let lift = self.scene_state.lift.take().map(|(parts, up)| (parts.iter().filter_map(|e| self.scene.element(*e)).map(|e| e.name.clone()).collect::<Vec<_>>(), up));
        self.scene_state = SceneState::default();
        let find = |name: &str| scene.elements.iter().find(|e| e.name == name).map(|e| e.id);
        if let Some((names, up)) = lift {
            self.scene_state.lift = Some((Arc::new(names.iter().filter_map(|n| find(n)).collect()), up));
        }
        let keep: Vec<ElementId> = selected.iter().filter_map(|n| find(n)).collect();
        if !keep.is_empty() {
            self.scene_state.select_set(keep);
            if let Some(a) = active.as_deref().and_then(find) {
                self.scene_state.selection.active = Some(a);
            }
        }
        if frame {
            let bounds = framing_bounds(&scene);
            let active = self.view.active_plate();
            if !aabb_is_empty(&bounds) {
                let pane = self.view.pane_mut(active);
                // Monitor's view of its job is its own: this is the view Design comes back to.
                match &mut pane.hidden_view {
                    Some((design, _)) if self.print.monitor_preview().is_some() => design.frame_bounds(&bounds, 16.0 / 9.0),
                    _ => pane.camera.frame_bounds(&bounds, 16.0 / 9.0),
                }
            }
            self.view.mark_camera_changed(active);
        }
        self.scene = scene;
        self.scene_revision = self.scene_revision.wrapping_add(1);
        self.sync_plate_elements();
    }

    /// What the plate's report cannot know: a field's open-ended loop met its stop, under the view or
    /// in a slice or a proof. True when a part has newly come to say so.
    pub fn mark_field_loop_stops(&mut self) -> bool {
        let issue = fab_sdf_tiles::field::LOOP_STOP_ISSUE;
        let mut marked = false;
        for p in &mut self.plate.parts {
            let stopped = p.sdf.as_ref().is_some_and(|s| self.field_loop_stops.contains(&s.key) || s.shape.root.loop_stopped());
            if stopped && !p.issues.iter().any(|i| i == issue) {
                p.issues.push(issue.into());
                marked = true;
            }
        }
        self.field_loop_marks += marked as u64;
        marked
    }

    /// Tie the plate's parts and the printer's helper geometry to the
    /// installed scene's elements, and show or hide the zone overlay.
    pub fn sync_plate_elements(&mut self) {
        let mut context = Vec::new();
        let mut zones = Vec::new();
        let mut floor = Vec::new();
        for e in &self.scene.elements {
            if e.name.starts_with(ZONE_PREFIX) {
                zones.push(e.id);
            }
            if e.name.starts_with(CONTEXT_PREFIX) {
                context.push(e.id);
                if e.has_geometry() && e.bounds.max.z <= FLOOR_TOP {
                    floor.push(e.id);
                }
            }
        }
        for p in &mut self.plate.parts {
            p.element = self.scene.elements.iter().find(|e| e.name == p.object).map(|e| e.id);
        }
        // `hidden: true` keeps a placement out of the views; one shown again comes back
        let hidden: Vec<ElementId> = self.plate.parts.iter().filter(|p| p.hidden).filter_map(|p| p.element).collect();
        for e in std::mem::take(&mut self.place_hidden) {
            if !hidden.contains(&e) {
                self.scene_state.set_hidden(e, false);
            }
        }
        for &e in &hidden {
            self.scene_state.set_hidden(e, true);
        }
        self.place_hidden = hidden;
        self.mark_field_loop_stops();
        // Selection never holds the printer's geometry.
        let stray: Vec<ElementId> = self.scene_state.selection.set.iter().copied().filter(|e| context.contains(e)).collect();
        if !stray.is_empty() {
            let keep: Vec<ElementId> = self.scene_state.selection.set.iter().copied().filter(|e| !context.contains(e)).collect();
            self.scene_state.select_set(keep);
        }
        self.plate.context_elements = context;
        self.plate.zone_elements = zones;
        self.plate.floor_elements = floor;
        self.glass_parts.hosts.clear();
        self.glass_parts.elements.clear();
        let hosts: std::collections::HashSet<&str> = self.plate.parts.iter()
            .filter(|p| p.insert && !p.insert_into.is_empty() && p.insert_into != p.object)
            .map(|p| p.insert_into.as_str()).collect();
        for (part, p) in self.plate.parts.iter().enumerate() {
            let Some(id) = p.element else { continue };
            if self.plate.is_context(id) || self.plate.is_floor(id) { continue; }
            let e = &self.scene.elements[id.index()];
            // The glass lane only reuses aux meshes. A field with no ranges stays opaque.
            if e.ranges.is_empty() || !e.has_geometry() { continue; }
            if hosts.contains(p.object.as_str()) {
                self.glass_parts.hosts.insert(id);
            }
            let transparent = e.ranges.iter().find_map(|&(batch, _, count)| {
                let b = self.scene.batches.get(batch as usize)?;
                (count > 0 && self.scene.materials.get(b.material.index()).is_some_and(|m| m.transparent)).then_some(batch)
            });
            self.glass_parts.elements.insert(id, (part, transparent));
        }
    }

    /// Whether the view draws the floor: the bed's elements ([`PlateInfo::floor_elements`]),
    /// the ground grid and the shadows laid on the ground. Not while the camera is under the
    /// bed plane, where it would hide the print's underside; back the frame it rises over it.
    pub fn floor_shown(&self) -> bool {
        self.floor_shown_in(self.view.lead_plate())
    }

    /// [`Self::floor_shown`] for one pane's camera.
    pub fn floor_shown_in(&self, pane: PaneId) -> bool {
        !self.view.camera(pane).under_bed()
    }

    /// What Frame All looks at: the plate and anything off it, or the scene where no printer's
    /// plate shows (Mechanics).
    pub fn frame_all_bounds(&self) -> Aabb {
        if !self.ui.in_mode(PlateMode::Mechanics) {
            let plate = self.plate.view_box();
            if !aabb_is_empty(&plate) {
                return plate;
            }
        }
        framing_bounds(&self.scene)
    }

    /// Bounds of the current selection, if any element in it has geometry.
    pub fn selection_bounds(&self) -> Option<Aabb> {
        self.scene
            .bounds_of(self.scene_state.selection.set.iter().copied())
    }

    /// Visibility predicate for picking/culling (closure-friendly).
    pub fn is_visible(&self, id: ElementId) -> bool {
        self.scene_state.is_visible(id)
    }

    /// Apply the actions that only touch plain state. Returns `true` when
    /// something changed and the UI should redraw. Module-specific actions are
    /// handled by each module's `apply` in `main.rs::App::dispatch`.
    pub fn apply_core(&mut self, action: &ShellAction) -> bool {
        use ShellAction::*;
        match action {
            // The loader reports progress on the caption's job strip.
            LoadStarted(path) => {
                self.load = LoadStatus::Loading {
                    path: path.clone(),
                    progress: crate::model::LoadProgress::Opening,
                };
                true
            }
            LoadProgress(p) => {
                if let LoadStatus::Loading { progress, .. } = &mut self.load {
                    *progress = p.clone();
                }
                true
            }
            Loaded(scene) => {
                self.set_scene(scene.clone());
                true
            }
            SceneUpdated { scene, frame } => {
                self.update_scene(scene.clone(), *frame);
                true
            }
            RestoreView { view, camera } => {
                self.restore_view(view, *camera);
                true
            }
            LoadFailed { path, error } => {
                self.load = LoadStatus::Failed {
                    path: path.clone(),
                    error: error.clone(),
                };
                self.ui.notices.stand(NoticeView::Plate, format!("Failed: {error}"));
                true
            }
            SelectOnly(id) => {
                self.selected_measure = None;
                self.ui.picked_override = None;
                self.scene_state.select_only(*id);
                true
            }
            SelectToggle(id) => {
                self.selected_measure = None;
                self.ui.picked_override = None;
                self.scene_state.select_toggle(*id);
                true
            }
            SelectAdd(id) => {
                self.selected_measure = None;
                self.ui.picked_override = None;
                self.scene_state.select_add(*id);
                true
            }
            SelectSet(ids) => {
                self.selected_measure = None;
                self.ui.picked_override = None;
                self.scene_state.select_set(ids.iter().copied());
                // Vec order is meaningful at the UI boundary: the first id
                // is the active one, so the part panel shows a deterministic
                // representative instead of whichever id a HashSet yields.
                self.scene_state.selection.active = ids.first().copied();
                true
            }
            ClearSelection => {
                self.selected_measure = None;
                self.ui.picked_override = None;
                self.scene_state.clear_selection();
                true
            }
            HideSelected => {
                self.scene_state.hide_selected();
                true
            }
            UnhideAll => {
                self.scene_state.unhide_all();
                true
            }
            IsolateSelected => {
                self.scene_state.isolate_selected();
                true
            }
            SetSection(section) => {
                self.scene_state.set_section(section.clone());
                true
            }
            SetDeviceKit(kit) => {
                let changed = self.device.kit != *kit || self.device.step.is_some();
                self.device.kit = *kit;
                self.device.step = None;
                changed
            }
            SetDeviceStep(step) => {
                let changed = self.device.step != *step;
                self.device.step = *step;
                changed
            }
            SetPaneCase { pane, shown } => {
                let Some(p) = self.view.panes.get_mut(pane.0 as usize).and_then(Option::as_mut) else { return false };
                let changed = p.case_hidden == *shown;
                p.case_hidden = !*shown;
                changed
            }
            SetPanePoses { pane, poses } => self.set_pane_poses(*pane, *poses),
            SetPaneLink { pane, link } => {
                let Some(link) = self.view.resolve_link(*pane, *link) else { return false };
                let Some(p) = self.view.panes.get_mut(pane.0 as usize).and_then(Option::as_mut) else { return false };
                if p.link == link { return false; }
                p.link = link;
                if let Some(source) = link.source() {
                    for follower in self.view.panes.iter_mut().flatten() {
                        if let PaneLink::Linked { to, .. } = &mut follower.link {
                            if *to == *pane { *to = source; }
                        }
                    }
                }
                self.keep_plate_view(*pane);
                true
            }
            SetPaneMode { pane, mode } => {
                let id = *pane;
                let Some(pane) = self.view.panes.get_mut(pane.0 as usize).and_then(Option::as_mut) else { return false };
                if pane.mode == *mode { return false; }
                pane.mode = *mode;
                pane.trace = TraceProgress::default();
                if !self.view.panes.iter().flatten().any(|p| !p.parked && p.mode == PaneMode::Traced) {
                    self.scene_gpu.borrow_mut().product_scene = None;
                }
                self.keep_plate_view(id);
                true
            }
            SetShading(shading) => {
                self.view.shading = *shading;
                crate::view_prefs::save_shading(*shading);
                true
            }
            SetDragDetail(detail) => {
                self.view.drag_detail = *detail;
                crate::view_prefs::save_drag_detail(*detail);
                true
            }
            SetSlicedDetail(detail) => {
                self.view.sliced_detail = *detail;
                crate::view_prefs::save_sliced_detail(*detail);
                true
            }
            SetTracedDetail(detail) => {
                self.view.traced_detail = *detail;
                crate::view_prefs::save_traced_detail(*detail);
                true
            }
            SetSoldered(on) => {
                self.view.soldered = *on;
                crate::view_prefs::save_soldered(*on);
                true
            }
            SetPartLook(look) => {
                self.view.look = *look;
                crate::view_prefs::save_look(*look);
                true
            }
            SetLighting(lighting) => {
                if self.ui.board_lit() {
                    self.view.board_lighting = *lighting;
                    crate::view_prefs::save_board_lighting(*lighting);
                } else {
                    self.view.lighting = *lighting;
                    crate::view_prefs::save_lighting(*lighting);
                }
                true
            }
            ShowCode(on) => self.show_code(*on),
            // Turning an experimental workspace on also opens it: whoever switches it on wants
            // to see it. Turning it off while it shows goes back to Design.
            ShowArm(on) | ShowScan(on) => {
                let kind = if matches!(action, ShowArm(_)) { Workspace::Arm } else { Workspace::Scan };
                if kind == Workspace::Arm {
                    self.ui.arm_shown = *on;
                    crate::view_prefs::save_arm(*on);
                } else {
                    self.ui.scan_shown = *on;
                    crate::view_prefs::save_scan(*on);
                }
                self.ui.workspaces.rev += 1;
                if *on {
                    self.apply_core(&SetWorkspace(kind));
                } else if !self.ui.offered(self.ui.workspaces.current()) {
                    self.apply_core(&SetWorkspace(Workspace::Design));
                }
                true
            }
            FollowAi(on) => std::mem::replace(&mut self.ui.follow_ai, *on) != *on,
            // A job Monitor holds has no other look, and the render mode is the other workspaces'.
            SetRenderMode(_) if self.print.monitor_holds() => false,
            SetRenderMode(mode) => {
                let look = match mode {
                    RenderMode::Field => Some(PartLook::Field),
                    RenderMode::Mesh => Some(PartLook::Mesh),
                    RenderMode::Slice | RenderMode::Force => None,
                };
                if let Some(look) = look.filter(|l| *l != self.view.look) {
                    self.view.look = look;
                    crate::view_prefs::save_look(look);
                }
                let slice = *mode == RenderMode::Slice;
                self.force.active = *mode == RenderMode::Force;
                // the Force tool works on the Force mode's study: another mode ends it
                if !self.force.active && self.ui.gizmo == GizmoMode::Force {
                    self.ui.gizmo = GizmoMode::Off;
                }
                // no trees draws on the Sliced view's bed: another mode ends it; print settings
                // draw in the Normal view too, not on the Force mode's study
                if (!slice && self.tool == Tool::NoTrees) || (self.force.active && self.tool == Tool::SettingsHere) {
                    self.tool = Tool::Select;
                }
                self.print.play.active = slice;
                self.print.play.playing &= slice;
                if slice {
                    self.print.overhangs = false;
                }
                true
            }
            FieldUnavailable { why } => {
                self.ui.notices.stand(NoticeView::Plate, format!("{why}: View › Shading › Mesh shows the parts as meshes."));
                true
            }
            FieldLoopStopped { key } => {
                self.field_loop_stops.insert(*key);
                self.mark_field_loop_stops();
                true
            }
            SetOverlays(overlays) => {
                self.view.overlays = *overlays;
                crate::view_prefs::save_overlays(*overlays);
                true
            }
            ToggleXray => {
                self.view.xray = !self.view.xray;
                true
            }
            ToggleSeeThrough => {
                self.view.see.on = !self.view.see.on;
                if !self.view.see.on {
                    self.view.see.drawn.clear();
                }
                true
            }
            ToggleGlass => {
                self.view.glass = !self.view.glass;
                true
            }
            SetSeeOpacity(v) => {
                self.view.see.opacity = v.clamp(0.0, 1.0);
                true
            }
            SetSeeGlow(v) => {
                self.view.see.glow = v.clamp(0.0, 1.0);
                true
            }
            SetTool(tool) => {
                crate::tools::paint::cancel();
                crate::tools::sculpt::cancel();
                self.tool = *tool;
                true
            }
            // (after its object's selection: a selection unpicks any override)
            PickOverride(id) => std::mem::replace(&mut self.ui.picked_override, id.clone()) != *id,
            // a hidden workspace is not offered (its shortcut, the wrist, a stale menu row)
            SetWorkspace(w) if !self.workspace_shown(*w) => false,
            SetWorkspace(w) => self.switch_workspace(|ui| ui.enter(*w)),
            SwitchWorkspace(id) if self.ui.workspaces.get(*id).is_none_or(|d| !self.ui.offered(d)) => false,
            SwitchWorkspace(id) => self.switch_workspace(|ui| ui.enter_id(*id)),
            LayoutGroup(open) => {
                if *open {
                    self.ui.workspaces.begin_group();
                } else {
                    self.ui.workspaces.end_group();
                }
                false
            }
            EditWorkspaces(op) => {
                use crate::workspaces::ListOp;
                // the temporary layout is not in the file: opening, changing or dropping it
                // leaves the file as it is
                let temp = self.ui.workspaces.temp_id();
                let row = match op {
                    ListOp::Temporary { .. } | ListOp::Dismiss => false,
                    ListOp::Pair { .. } => !self.ui.workspaces.temp_shown(),
                    ListOp::Reset(id) | ListOp::UndoLayout(id) | ListOp::Delete(id) => Some(*id) != temp,
                    _ => true,
                };
                let (changed, show) = self.ui.workspaces.apply(op);
                if let Some(id) = show {
                    self.switch_workspace(|ui| ui.enter_id(id));
                } else if changed && matches!(op, crate::workspaces::ListOp::Reset(id) if *id == self.ui.workspaces.current) {
                    // the reset one shows: its pinned mode again
                    self.switch_workspace(|ui| if let Some(mode) = ui.workspaces.current().mode { ui.plate_mode = mode });
                }
                if changed && row {
                    self.ui.workspaces.save();
                }
                changed
            }
            EditAreas(op) => {
                let done = self.ui.workspaces.edit(*op).is_ok();
                if done && !matches!(op, WorkspaceOp::Resize { .. }) && !self.ui.workspaces.temp_shown() {
                    self.ui.workspaces.save();
                }
                done
            }
            Notice { view, text } => {
                let revision = self.ui.notices.revision;
                self.ui.notices.say(*view, text.clone());
                self.ui.notices.revision != revision
            }
            ClearPassingNotice { view, before } => {
                let revision = self.ui.notices.revision;
                self.ui.notices.clear_passing(*view, *before);
                self.ui.notices.revision != revision
            }
            // the shell's: it closes the Performance editor or brings it on screen
            TogglePerf => false,
            ToggleCommandPalette => {
                self.ui.command_palette_open = !self.ui.command_palette_open;
                true
            }
            // the shell brings the Plates panel forward
            ShowPlates => false,
            ShowKeymapHelp(open) => {
                self.ui.keymap_help_open = *open;
                true
            }
            ShowAbout(open) => {
                self.ui.about_open = *open;
                true
            }
            ShowModelSources(page) => {
                match page {
                    Some(page) => {
                        self.ui.sources_page = *page;
                        self.ui.page = Some(Editor::ModelSources);
                    }
                    None => self.ui.page = None,
                }
                true
            }
            ModelSourcesPage(page) => {
                self.ui.sources_page = *page;
                true
            }
            SetPlateMode(mode) => {
                if !self.ui.shows(Family::Plate) || self.ui.plate_mode == *mode {
                    return false;
                }
                let mode = *mode;
                self.switch_workspace(|ui| ui.plate_mode = mode)
            }
            SetModelSource(source, on) => {
                match self.model_sources.set(*source, *on) {
                    Ok(()) => crate::model_sources::save(&self.model_sources),
                    // EasyEDA turns on through its warning: show it (over the workspace unless an
                    // area shows the editor), change nothing yet.
                    Err(crate::model_sources::Refused::NeedsAcceptance) => {
                        self.ui.sources_page = SourcesPage::EasyEdaWarning;
                        if !self.ui.on_screen(Editor::ModelSources) {
                            self.ui.page = Some(Editor::ModelSources);
                        }
                    }
                }
                true
            }
            AcceptEasyEda => {
                self.model_sources.accept_easyeda(makepad_civil_time::now_secs());
                crate::model_sources::save(&self.model_sources);
                self.ui.sources_page = SourcesPage::Settings;
                true
            }
            SetPartGlows(glows) => {
                self.part_glows = glows.clone();
                true
            }
            _ => false,
        }
    }
}

// ===========================================================================
// Viewport ↔ navigation / tools
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
}

/// Input as the viewport widget delivers it to the navigator and the tools.
/// Positions are window points; `hit` is the pick under the pointer for
/// `PointerDown`/`PointerMove` when the viewport computed one.
#[derive(Clone, Copy, Debug)]
pub enum ViewportInputKind {
    PointerDown {
        button: PointerButton,
        pos: DVec2,
        mods: KeyModifiers,
        tap_count: u32,
    },
    PointerMove {
        pos: DVec2,
        /// Delta since the previous move, in points.
        delta: DVec2,
        mods: KeyModifiers,
        buttons: u8,
    },
    PointerUp {
        button: PointerButton,
        pos: DVec2,
        mods: KeyModifiers,
    },
    HoverOut,
    Scroll {
        delta: DVec2,
        pos: DVec2,
        mods: KeyModifiers,
    },
    KeyDown {
        key: KeyCode,
        mods: KeyModifiers,
        repeat: bool,
    },
    KeyUp {
        key: KeyCode,
        mods: KeyModifiers,
    },
    /// Once per drawn frame while the viewport is alive. `dt` in seconds.
    Frame {
        dt: f32,
        time: f64,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ViewportInput {
    pub rect: Rect,
    pub kind: ViewportInputKind,
    pub hit: Option<RayHit>,
}

/// What a controller wants the viewport to do after handling input.
#[derive(Clone, Copy, Debug, Default)]
pub struct InputResponse {
    /// The input was used; do not pass it on.
    pub consumed: bool,
    pub redraw: bool,
    pub cursor: Option<MouseCursor>,
    /// Keep receiving `Frame` inputs (inertia, camera glides).
    pub wants_frames: bool,
}

impl InputResponse {
    pub fn consumed() -> Self {
        InputResponse {
            consumed: true,
            redraw: true,
            ..Default::default()
        }
    }
}

/// The camera controller behind the viewport (holds inertia/animation state).
pub trait NavController {
    fn handle(&mut self, cx: &mut Cx, input: &ViewportInput, state: &mut AppState) -> InputResponse;

    /// Animate the camera to frame `bounds` (Home / period key).
    fn frame(&mut self, cx: &mut Cx, state: &mut AppState, bounds: Aabb, animate: bool);

    /// Snap/animate to a preset (numpad views). Keeps the distance.
    fn preset(&mut self, cx: &mut Cx, state: &mut AppState, preset: PresetView, animate: bool);

    /// Animate the camera to frame `bounds` looking along `look`, `up` up the view.
    fn frame_from(&mut self, cx: &mut Cx, state: &mut AppState, bounds: Aabb, look: Vec3f, up: Vec3f);

    /// Toggle or set orthographic while preserving apparent size.
    fn set_ortho(&mut self, cx: &mut Cx, state: &mut AppState, ortho: bool);

    /// Orbit by a screen-space delta (the nav gizmo drags call this).
    fn orbit_by(&mut self, cx: &mut Cx, state: &mut AppState, dx: f32, dy: f32);

    /// True while a glide, a coast or a drag is in flight: the camera is moving (a drag whose
    /// pointer rests too, though it asks for no frames).
    fn is_animating(&self) -> bool;
}

/// The tools' input handler; persistent results live in `AppState`
/// (`measurements`, `scene_state.section`). Dimension lines and section
/// handles are drawn by the tools' overlay widget (`FabToolOverlay`) over the
/// viewport, reading `AppState` and building a [`ViewProjector`] from the
/// camera.
pub trait ToolController {
    fn handle(&mut self, cx: &mut Cx, input: &ViewportInput, state: &mut AppState) -> InputResponse;
}

// ===========================================================================
// Actions
// ===========================================================================

/// Everything that can happen. Emitted with `cx.action(ShellAction::X)` from
/// widgets or `Cx::post_action(ShellAction::X)` from the loader thread.
/// The longest side of the project's picture, in pixels: the viewport shrinks its settled frame
/// to it on the GPU (the plate list's pictures, `fab_ai::plates::THUMB_SIDE`).
pub const PICTURE_SIDE: u32 = 320;

/// The viewport's settled frame shrunk to [`PICTURE_SIDE`]: `rgba` top row first, `width` x
/// `height` pixels.
#[derive(Clone)]
pub struct ViewportFrame {
    /// The plate's revision (`PlateInfo::revision`) the frame was taken at: a frame of an older
    /// plate is dropped.
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: HostBuf,
}

impl std::fmt::Debug for ViewportFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ViewportFrame({}x{})", self.width, self.height)
    }
}

/// One row of the plate list: a plate, or a demo.
#[derive(Clone, Debug, PartialEq)]
pub struct PlateCard {
    /// The plate's id in the store, or the demo's key.
    pub id: String,
    pub demo: bool,
    /// A demo's group in the Demos list ("Studio"); empty: ungrouped, listed before the groups.
    pub group: String,
    pub name: String,
    /// Its edits are not in its file yet (a plate never saved, once it has parts): `*`.
    pub modified: bool,
    pub printer: String,
    /// When it was last edited ("07:29", "Yesterday", "31 Oct"); empty for a demo.
    pub when: String,
    pub file: Option<PathBuf>,
    /// Where it came from ("made from a picture", "copy of Noise vase"), or what is not as it
    /// should be ("files kept aside", "changed outside Fab", "this Fab cannot read its file"),
    /// else empty.
    pub note: String,
    /// Why the note says what it does, in full (the row's tooltip): what was kept aside, where
    /// and why; each file taken in though Fab did not write it so; what a file this Fab cannot
    /// read has that it does not know. Else empty.
    pub why: String,
    /// Its last settled frame (a PNG), when it has one.
    pub picture: Option<Arc<[u8]>>,
}

impl PlateCard {
    /// The name with a `*` while its edits are not in its file.
    pub fn title(&self) -> String {
        if self.modified { format!("{} *", self.name) } else { self.name.clone() }
    }
}

/// How a plate was looked at when another went on the bed: a switch back shows it so again.
#[derive(Clone, Debug, PartialEq)]
pub struct PlateView {
    /// The first Plate view's camera (Design's, while Monitor shows its own) and the preset it
    /// was at.
    pub camera: Camera,
    pub preset: Option<PresetView>,
    /// The other Plate views' cameras, by instance number, as `camera` is the first's.
    pub plates: Vec<(u32, Camera, Option<PresetView>)>,
    /// The Device camera when manually placed. Otherwise the editor frames the device on opening.
    pub device: Option<Camera>,
    /// The Schematic and Board cameras when manually placed, supplied by their dock bodies.
    pub sheet: Option<SavedView2d>,
    pub board: Option<SavedView2d>,
    /// The Scope instances with lane choices or a timebase of their own, by instance number.
    pub scopes: Vec<(u32, ScopeView)>,
    /// The selected parts by their objects' names, and the active one.
    pub selected: Vec<String>,
    pub active: Option<String>,
    /// The script the code panel showed.
    pub viewed: Option<String>,
}

#[derive(SerRon, DeRon)]
struct PlateViewFile {
    cameras: Vec<PlateCameraFile>,
    device: Option<PlateCameraFile>,
    sheet: Option<SavedView2d>,
    board: Option<SavedView2d>,
    scopes: Option<Vec<(u32, ScopeView)>>,
    selected: Vec<String>,
    active: Option<String>,
    viewed: Option<String>,
}

#[derive(SerRon, DeRon)]
struct PlateCameraFile {
    n: u32,
    eye: [f32; 3],
    target: [f32; 3],
    up: [f32; 3],
    fov_y_deg: f32,
    ortho: bool,
    ortho_height: f32,
    near: f32,
    far: f32,
    hung: Option<bool>,
    preset: Option<PresetView>,
}

impl PlateCameraFile {
    fn from_camera(c: Camera, preset: Option<PresetView>) -> Self {
        let xyz = |v: Vec3f| [v.x, v.y, v.z];
        Self {
            n: 0, eye: xyz(c.eye), target: xyz(c.target), up: xyz(c.up),
            fov_y_deg: c.fov_y_deg, ortho: c.ortho, ortho_height: c.ortho_height,
            near: c.near, far: c.far, hung: c.hung.then_some(true), preset,
        }
    }

    fn camera(&self) -> Camera {
        let xyz = |[x, y, z]: [f32; 3]| vec3(x, y, z);
        Camera {
            eye: xyz(self.eye), target: xyz(self.target), up: xyz(self.up),
            fov_y_deg: self.fov_y_deg, ortho: self.ortho, ortho_height: self.ortho_height,
            near: self.near, far: self.far, hung: self.hung.unwrap_or(false),
        }
    }
}

impl PlateView {
    pub fn to_file_text(&self) -> String {
        PlateViewFile {
            cameras: std::iter::once(PlateCameraFile::from_camera(self.camera, self.preset))
                .chain(self.plates.iter().map(|&(n, c, preset)| PlateCameraFile { n, ..PlateCameraFile::from_camera(c, preset) }))
                .collect(),
            device: self.device.map(|c| PlateCameraFile::from_camera(c, None)),
            sheet: self.sheet,
            board: self.board,
            scopes: (!self.scopes.is_empty()).then(|| self.scopes.clone()),
            selected: self.selected.clone(),
            active: self.active.clone(),
            viewed: self.viewed.clone(),
        }.serialize_ron()
    }

    pub fn from_file_text(text: &str) -> Option<Self> {
        let mut state = DeRonState::default();
        let mut chars = text.chars();
        state.next(&mut chars);
        state.next_tok(&mut chars).ok()?;
        let file = PlateViewFile::de_ron(&mut state, &mut chars).ok()?;
        if state.tok != DeRonTok::Eof {
            return None;
        }
        let c = file.cameras.iter().find(|c| c.n == 0)?;
        Some(Self {
            camera: c.camera(),
            plates: file.cameras.iter().filter(|c| c.n != 0).map(|c| (c.n, c.camera(), c.preset)).collect(),
            device: file.device.map(|d| d.camera()),
            sheet: file.sheet,
            board: file.board,
            scopes: file.scopes.unwrap_or_default(),
            preset: c.preset, selected: file.selected, active: file.active, viewed: file.viewed,
        })
    }
}

/// A job the person waits on, as the 3D view's card shows it ([`crate::ui::progress_card`]):
/// a title, the job's phase (its stage) and how far it has got (its fraction, a bar that never
/// runs back), and a Cancel when the job can be cancelled. The plate on the bed stays usable
/// under it. Another plate loading is one; anything with a job on the board can be.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressCard {
    /// What is happening: "Loading Rally car".
    pub title: String,
    /// What a Cancel leaves, said while the job stops: "the plate on the bed stays as it is".
    pub cancel_keeps: String,
    /// The title once the job has failed (the card then says why, until it is closed): "Rally
    /// car did not load".
    pub failed: String,
    /// The job: on the board, its stage, fraction and whether it can be cancelled.
    pub job: makepad_widgets_core::jobs::JobId,
}

/// The plate list as the model worker last sent it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlateList {
    /// The plates, the most recently edited first.
    pub plates: Vec<PlateCard>,
    /// Every demo plate, in the app's order.
    pub demos: Vec<PlateCard>,
    /// On the bed: a plate's id, or a demo's key (`on_demo`).
    pub on_bed: Option<String>,
    pub on_demo: bool,
}

impl PlateList {
    /// The card of what is on the bed.
    pub fn bed(&self) -> Option<&PlateCard> {
        let id = self.on_bed.as_ref()?;
        if self.on_demo { self.demos.iter().find(|d| &d.id == id) } else { self.plates.iter().find(|p| &p.id == id) }
    }
}

#[derive(Debug)]
pub enum ShellAction {
    // ---- files / loading ----
    OpenFile(PathBuf),
    /// Add mesh objects to the current plate. Other file kinds open normally.
    ImportFile(PathBuf),
    /// Files from outside the app came over the window (sent again when they are other files):
    /// the plate previews where their drop puts them.
    DragFiles(Vec<PathBuf>),
    /// Files from outside the app were dropped on the window: object scripts go on the plate
    /// together (one undo step), every other file as [`ShellAction::ImportFile`].
    DropFiles(Vec<PathBuf>),
    /// The drag of files over the window ended (dropped, or it left the window).
    DragFilesEnd,
    /// A demo plate, by key ([`PlateList::demos`]).
    OpenDemo(String),
    /// Put a plate on the bed as it was left, by id ([`PlateList::plates`]); the plate on the
    /// bed keeps every edit.
    OpenPlate(String),
    /// A copy of a plate on the list, by id.
    DuplicatePlate(String),
    /// Name a plate: its id and the new name.
    RenamePlate(String, String),
    /// Take a plate off the list; its files stay.
    RemovePlate(String),
    /// Move a plate's files to the trash (the person confirmed it).
    DeletePlate(String),
    /// The progress card of a job that failed was closed.
    CloseProgressCard,
    LoadStarted(PathBuf),
    LoadProgress(LoadProgress),
    Loaded(Arc<Scene>),
    LoadFailed { path: PathBuf, error: String },
    /// A live rebuild of the open model (a modeling batch, undo, redo):
    /// swaps the geometry in without the load resets: the camera stays where
    /// it is unless `frame`, the load status and history are untouched.
    SceneUpdated { scene: Arc<Scene>, frame: bool },

    // ---- selection / visibility ----
    SelectOnly(ElementId),
    SelectToggle(ElementId),
    SelectAdd(ElementId),
    SelectSet(Vec<ElementId>),
    ClearSelection,
    HideSelected,
    UnhideAll,
    IsolateSelected,

    // ---- tools ----
    SetTool(Tool),
    SetSection(SectionState),
    AddMeasurement(Measurement),
    /// A finished endpoint drag: one document transaction on release.
    SetMeasurement { index: usize, measurement: Measurement },

    // ---- the view ----
    /// The device as a kit (true) or assembled; it leaves a build step.
    SetDeviceKit(bool),
    /// Stop the device at a build step ([`DeviceView::step`]), or show it whole again.
    SetDeviceStep(Option<usize>),
    /// A pane's Case toggle: its device's case parts shown or left out.
    SetPaneCase { pane: PaneId, shown: bool },
    /// Show a pane's parts at the stage's poses or a pose set's.
    SetPanePoses { pane: PaneId, poses: PanePoses },
    SetPaneLink { pane: PaneId, link: PaneLink },
    SetPaneMode { pane: PaneId, mode: PaneMode },
    SetShading(Shading),
    SetPartLook(PartLook),
    SetDragDetail(fab_sdf_tiles::trace::DragDetail),
    SetSlicedDetail(SlicedDetail),
    SetTracedDetail(TracedDetail),
    SetSoldered(bool),
    /// View › Show Board: every board placement shown or kept out of the views, one undoable plate
    /// edit (`hidden: true`), remembered with the project. The app makes the edit.
    ShowBoard(bool),
    /// Either studio or the sun and sky, kept for this workspace's next start.
    SetLighting(Lighting),
    SetRenderMode(RenderMode),
    /// An override's row clicked (its id), lit where the overrides are listed.
    PickOverride(Option<String>),
    /// Design's code split beside the view: shown or hidden.
    ShowCode(bool),
    /// View › Experimental › Arm: the Arm workspace offered or hidden.
    ShowArm(bool),
    /// View › Experimental › Scan: the Scan workspaces offered or hidden.
    ShowScan(bool),
    /// View › Follow AI on or off ([`UiState::follow_ai`]).
    FollowAi(bool),
    /// The caption's tab (or its key, or a menu row) picked: that workspace shows.
    SwitchWorkspace(crate::workspaces::WorkspaceId),
    /// Open or close a group of layout edits undone together in each workspace.
    LayoutGroup(bool),
    /// A change to the caption's row: a rename, a copy, a reset, a delete, a move, a default added.
    EditWorkspaces(crate::workspaces::ListOp),
    /// A change to the shown workspace's areas (an area's menu, a sash, a Reveal).
    EditAreas(WorkspaceOp),
    /// A part's field cannot be drawn (`why`): the note says so; the look stays the person's.
    FieldUnavailable { why: String },
    /// An open-ended loop in the field of shape `key` stopped at its cap: the part shows an issue.
    FieldLoopStopped { key: [u8; 32] },
    /// A drag's live distance fields arrived (`ParamState::sdf_live`): the view draws them.
    SdfLive,
    SetOverlays(Overlays),
    ToggleXray,
    /// View › See-Through on or off, its opacity (0 a ghost, 1 solid) and its glow outside the
    /// part (0 none).
    ToggleSeeThrough,
    SetSeeOpacity(f32),
    SetSeeGlow(f32),
    /// View › Translucent materials: glass surfaces or the opaque body look.
    ToggleGlass,
    /// Handled by the viewport widget (via its navigator).
    FrameAll,
    FrameSelected,
    /// Turn to the selection (else the plate) and orbit about it from where the camera is.
    OrbitAroundSelection,
    /// Frame this box (metres): the plate, a fresh print preview, an issue.
    FrameBox(Aabb),
    /// Open a plate from the default direction at its first laid-out viewport size.
    FrameOpening(Aabb),
    /// Frame this box (metres) looking along `look` with `up` up the view: a face-down picture
    /// from below, the right way up.
    FrameFrom { bounds: Aabb, look: Vec3f, up: Vec3f },
    /// Restore the selection when a plate opens. If its saved camera sees the plate, also
    /// restore the camera and preset and stop the viewport's motion.
    RestoreView { view: PlateView, camera: bool },
    PresetView(PresetView),
    ToggleOrtho,
    /// Screen-space orbit request (nav gizmo drag).
    OrbitBy(f32, f32),

    // ---- ui chrome ----
    SetWorkspace(Workspace),
    Notice { view: NoticeView, text: String },
    ClearPassingNotice { view: NoticeView, before: f64 },
    TogglePerf,
    ToggleCommandPalette,
    /// Bring the Plates panel forward (File > Browse Plates…).
    ShowPlates,
    /// The system's open dialog (File > Open…), with Fab's file filters; its answer is `OpenFile`.
    OpenDialog,
    ImportDialog,
    ImportProfilesDialog,
    ImageToPartDialog,
    /// The open dialog for a photo to print in full colour (Parts > Add > Colour picture…).
    ColourPictureDialog,
    /// The viewport's settled frame, to be saved as the project's picture.
    ViewportFrame(ViewportFrame),
    ShowKeymapHelp(bool),
    ShowAbout(bool),
    /// Show the Model sources editor at a page alone over the workspace, or go back to the
    /// workspace (None).
    ShowModelSources(Option<SourcesPage>),
    /// The page the Model sources editor shows, where it is.
    ModelSourcesPage(SourcesPage),
    /// Turn a 3D model source on or off. EasyEDA on shows its warning instead; it turns on only
    /// with [`ShellAction::AcceptEasyEda`], the person's press on the warning.
    SetModelSource(crate::model_sources::ModelSource, bool),
    /// The plate's mode, set where it is (the plate's own row): the workspace stays.
    SetPlateMode(PlateMode),
    /// The person accepted EasyEDA's warning: EasyEDA's models are on, and the time is kept.
    AcceptEasyEda,
    /// Run a named command from the F3 palette (`"view.frame_all"` etc.).
    Command(String),
    /// The plate parts that glow with a live part's light: replaces the whole list (empty: none).
    SetPartGlows(Arc<[PartGlow]>),
    Quit,
}

/// Convenience for widgets: find every `ShellAction` in an actions list.
pub fn shell_actions(actions: &Actions) -> impl Iterator<Item = &ShellAction> {
    actions
        .iter()
        .filter_map(|a| a.downcast_ref::<ShellAction>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_links_sync_cameras_and_preserve_free_cameras() {
        let mut state = AppState::default();
        let second = state.view.add_pane(PaneLink::Free);
        state.park_pane(second);
        let linked = |how, to| PaneLink::Linked { how, to };
        for source in [PaneId::PRIMARY, second] {
            let view = &mut state.view;
            let synced = view.add_pane(linked(LinkHow::Synced, source));
            let flipped = view.add_pane(linked(LinkHow::Flipped, synced));
            let free = if source == second { PaneId::PRIMARY } else { view.add_pane(PaneLink::Free) };
            assert_eq!(view.pane(flipped).link, linked(LinkHow::Flipped, source), "creation resolves a linked Plate");
            let camera = Camera {
                eye: vec3(1.0, -2.0, 3.0),
                target: vec3(0.25, 0.5, 0.75),
                up: vec3(0.0, 1.0, 0.0),
                ortho: true,
                ortho_height: 2.0,
                fov_y_deg: 55.0,
                near: 0.125,
                far: 200.0,
                hung: true,
            };
            let independent = Camera { eye: vec3(-4.0, 5.0, 6.0), ..Camera::default() };
            *view.camera_mut(source) = camera;
            *view.camera_mut(free) = independent;
            view.settle_cameras();
            assert_eq!(*view.camera(source), camera, "settling must preserve the source camera");
            assert_eq!(*view.camera(synced), camera, "Synced must copy every source camera field");
            assert_eq!(*view.camera(free), independent, "Free must keep its independent camera");

            let moved = Camera { eye: vec3(2.0, -3.0, 4.0), ..camera };
            *view.camera_mut(synced) = moved;
            view.write_back(synced);
            assert_eq!(*view.camera(source), moved, "Synced navigation must write back every camera field");
            view.write_back(free);
            assert_eq!(*view.camera(source), moved, "Free navigation must preserve the source camera");
            assert_eq!(*view.camera(free), independent, "Free write_back must preserve its camera");

            let follower = view.add_pane(linked(LinkHow::Through, free));
            let vacant = PaneId(view.panes.len() as u16);
            let set_link = |pane, to| ShellAction::SetPaneLink { pane, link: linked(LinkHow::Synced, to) };
            view.pane_mut(follower).role = PaneRole::Device;
            assert!(!state.apply_core(&set_link(flipped, vacant)), "a vacant slot cannot be a source");
            assert!(!state.apply_core(&set_link(flipped, follower)), "a Device view cannot be a source even when it follows a Plate");
            assert!(!state.apply_core(&set_link(flipped, flipped)), "a linked pane cannot follow itself");
            assert_eq!(state.view.pane(flipped).link, linked(LinkHow::Flipped, source));
            assert!(!state.apply_core(&set_link(source, synced)), "resolving a follower cannot create a cycle");
            assert_eq!(state.view.pane(source).link, PaneLink::Free);
            assert!(state.apply_core(&set_link(free, flipped)));
            assert_eq!(state.view.pane(free).link, linked(LinkHow::Synced, source), "relinking resolves to the root");
            assert_eq!(state.view.pane(follower).link, linked(LinkHow::Through, source), "followers keep their link mode at the new root");

            if source == second {
                state.view.settle_cameras();
                assert_eq!(*state.view.camera(free), moved, "pane zero can derive from a later, parked Plate");
                let shown = [synced, flipped, free, follower].map(|id| (id, *state.view.camera(id)));
                *state.view.camera_mut(source) = independent;
                state.release_pane(source);
                assert_eq!(state.view.add_pane(PaneLink::Free), source, "the released source slot is reusable");
                state.view.settle_cameras();
                for (id, camera) in shown {
                    assert_eq!(state.view.pane(id).link, PaneLink::Free, "release must detach every follower");
                    assert_eq!(*state.view.camera(id), camera, "release must preserve the camera on screen");
                }
            }
        }
    }

    #[test]
    fn reflected_pane_cameras_derive_idempotently_and_write_back() {
        fn check(view: &mut ViewportState, source: PaneId, ortho: bool) {
            let source_camera = Camera {
                eye: vec3(0.5, -0.75, 1.25),
                target: vec3(0.125, 0.25, 0.375),
                up: vec3(0.0, 0.6, 0.8),
                ortho,
                ortho_height: 2.0,
                fov_y_deg: 55.0,
                near: 0.125,
                far: 200.0,
                hung: true,
            };
            *view.camera_mut(source) = source_camera;
            let flipped = view.add_pane(PaneLink::Linked { how: LinkHow::Flipped, to: source });
            let through = view.add_pane(PaneLink::Linked { how: LinkHow::Through, to: source });
            let reflected = Camera {
                eye: vec3(0.5, -0.75, -1.25),
                target: vec3(0.125, 0.25, -0.375),
                up: vec3(0.0, 0.6, -0.8),
                ..source_camera
            };
            view.settle_cameras();
            assert_eq!(*view.camera(source), source_camera, "reflection must preserve the source, ortho={ortho}");
            for pane in [flipped, through] {
                let camera = view.camera(pane);
                assert_eq!(camera.eye, reflected.eye, "{pane:?} must negate eye z over the bed, ortho={ortho}");
                assert_eq!(camera.target, reflected.target, "{pane:?} must negate target z over the bed, ortho={ortho}");
                assert_eq!(camera.up, reflected.up, "{pane:?} must reflect up as a direction, ortho={ortho}");
                assert_eq!(camera.ortho, source_camera.ortho, "{pane:?} must preserve the projection flag, ortho={ortho}");
                assert_eq!(*camera, reflected, "{pane:?} must preserve every other camera field, ortho={ortho}");
            }
            view.settle_cameras();
            for pane in [flipped, through] {
                assert_eq!(*view.camera(pane), reflected, "{pane:?} derive must be idempotent, ortho={ortho}");
            }

            let moved = Camera {
                eye: vec3(2.0, -3.0, -4.0),
                target: vec3(0.25, 0.5, -0.75),
                up: vec3(0.6, 0.0, -0.8),
                ortho_height: 1.5,
                ..reflected
            };
            for pane in [flipped, through] {
                *view.camera_mut(source) = source_camera;
                *view.camera_mut(pane) = moved;
                view.write_back(pane);
                let written = Camera {
                    eye: vec3(2.0, -3.0, 4.0),
                    target: vec3(0.25, 0.5, 0.75),
                    up: vec3(0.6, 0.0, 0.8),
                    ..moved
                };
                assert_eq!(*view.camera(source), written, "{pane:?} navigation must reflect back to the source, ortho={ortho}");
                view.settle_cameras();
                assert_eq!(*view.camera(pane), moved, "{pane:?} derive must retain the written-back navigation, ortho={ortho}");
            }

            // x + z = 2: a tilted, translated plane with a non-unit normal.
            *view.camera_mut(source) = source_camera;
            view.mirror_plane = [2.0, 0.0, 2.0, -4.0];
            view.settle_cameras();
            let tilted = Camera {
                eye: vec3(0.75, -0.75, 1.5),
                target: vec3(1.625, 0.25, 1.875),
                up: vec3(-0.8, 0.6, 0.0),
                ..source_camera
            };
            for pane in [flipped, through] {
                assert_eq!(*view.camera(pane), tilted, "{pane:?} must use the full plane equation without translating up, ortho={ortho}");
                *view.camera_mut(source) = Camera::default();
                view.write_back(pane);
                assert_eq!(*view.camera(source), source_camera, "{pane:?} must write back through the tilted plane, ortho={ortho}");
            }
        }

        for ortho in [false, true] {
            check(&mut ViewportState::default(), PaneId::PRIMARY, ortho);
            let mut view = ViewportState::default();
            let second = view.add_pane(PaneLink::Free);
            check(&mut view, second, ortho);
        }
    }

    #[test]
    fn pane_ids_stay_stable_on_removal_and_primary_cannot_be_removed() {
        let mut state = AppState::default();
        assert_eq!(state.view.pane_count(), 1, "default view must contain exactly pane 0");
        assert_eq!(*state.view.camera(PaneId::PRIMARY), Camera::default(), "pane 0 must keep today's default camera");
        assert_eq!(state.view.pane(PaneId::PRIMARY).link, PaneLink::Free, "pane 0 must default to Free");
        assert_eq!(state.view.mirror_plane, [0.0, 0.0, 1.0, 0.0], "default mirror must be the bed plane");

        let primary = Camera { eye: vec3(1.0, -2.0, 3.0), ..Camera::default() };
        *state.view.camera_mut(PaneId::PRIMARY) = primary;
        let synced = PaneLink::Linked { how: LinkHow::Synced, to: PaneId::PRIMARY };
        let through = PaneLink::Linked { how: LinkHow::Through, to: PaneId::PRIMARY };
        let first = state.view.add_pane(synced);
        let last = state.view.add_pane(through);
        assert_eq!((first, last), (PaneId(1), PaneId(2)), "new panes must receive consecutive ids");
        assert_eq!(state.view.pane_count(), 3, "adding two panes must grow the count by two");
        assert_eq!(*state.view.camera(first), primary, "Synced must start with pane 0's current camera");
        assert_eq!(*state.view.camera(last), primary, "Through must start with pane 0's camera before deriving");
        assert_eq!(state.view.pane(first).link, synced, "add_pane must store the requested link");
        let last_camera = Camera { eye: vec3(-4.0, 5.0, 6.0), ..primary };
        *state.view.camera_mut(last) = last_camera;

        state.release_pane(PaneId::PRIMARY);
        assert_eq!(state.view.pane_count(), 3, "removing pane 0 must leave every pane in place");
        assert_eq!(*state.view.camera(PaneId::PRIMARY), primary, "removing pane 0 must preserve its camera");
        state.release_pane(first);
        assert_eq!(state.view.pane_count(), 2, "removing a secondary pane must shrink the count by one");
        assert_eq!(state.view.pane(last).link, through, "a later pane must keep its id and link on removal");
        assert_eq!(*state.view.camera(last), last_camera, "a surviving pane must retain its camera");
        state.release_pane(last);
        state.release_pane(PaneId::PRIMARY);
        assert_eq!(state.view.pane_count(), 1, "removing the last secondary pane and pane 0 must never empty the view");
        assert_eq!(*state.view.camera(PaneId::PRIMARY), primary, "removals must preserve the primary camera");
        assert_eq!(state.view.add_pane(PaneLink::Free), PaneId(1), "the next view reuses the lowest free slot");
    }

    /// A pane shows its own poses (the stage's or a set's), its Case toggle leaves the device's
    /// case parts out of it alone, and the shared layers follow a set only while a pane showing
    /// it is not parked.
    #[test]
    fn a_pane_shows_its_own_poses_and_the_shared_layers_follow_a_drawn_set() {
        let mut state = AppState::default();
        let (part, other) = (ElementId(4), ElementId(5));
        state.plate.parts.push(PlatePart { element: Some(part), printed: false, insert: false, ..Default::default() });
        let lease = state.lease_pane(PaneRole::Device);
        let pane = lease.id();
        state.set_pane_poses(pane, PanePoses::Set(PoseSetId::DEVICE));
        let up: crate::model::state::Pose = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.02]];
        let before = state.pose_set(PoseSetId::DEVICE).revision();
        state.pose_set_mut(PoseSetId::DEVICE).set(part, up);
        let after = state.pose_set(PoseSetId::DEVICE).revision();
        assert_ne!(before, after, "a new pose moves the set's revision");
        state.pose_set_mut(PoseSetId::DEVICE).set(part, up);
        assert_eq!(state.pose_set(PoseSetId::DEVICE).revision(), after, "the same pose again leaves the revision");

        assert_eq!(state.pose_in(pane, part), Some(up), "the device pane shows its set's pose");
        assert_eq!(state.pose_in(pane, other), None, "a part the set leaves out rests in the device pane");
        assert_eq!(state.pose_in(PaneId::PRIMARY, part), None, "the plate pane keeps the stage's poses");
        assert_eq!(state.posed_in(pane), vec![(part, up)], "the device pane's posed parts are its set's");
        assert_eq!(state.shared_poses(), PanePoses::Stage, "a leased pane not drawn yet leaves the shared layers on the stage");
        assert!(!state.assembled_under(state.shared_poses(), part), "a set no drawn pane shows assembles nothing");
        let device = PanePoses::Set(PoseSetId::DEVICE);
        assert!(state.assembled_under(device, part), "the device set assembles its unprinted part");
        assert!(!state.is_ghost_under(device, part), "the assembled part stays out of the device's ghosts");
        assert!(state.is_ghost_under(PanePoses::Stage, part), "the same unprinted part is a ghost on the stage");

        state.view.pane_mut(pane).parked = false;
        assert_eq!(state.shared_poses(), PanePoses::Set(PoseSetId::DEVICE), "a drawn device pane puts the shared layers on its set");
        assert_eq!(state.shared_pose(part), Some(up), "the shared layers draw the set's pose");
        let poses = state.shared_poses();
        assert!(state.assembled_under(poses, part) && !state.assembled_under(poses, other), "the set's parts are assembled, the others not");

        state.device.case = Arc::new([part].into_iter().collect());
        state.apply_core(&ShellAction::SetPaneCase { pane, shown: false });
        assert!(state.case_hidden_in(pane, part), "Case off leaves a case part out of its pane");
        assert!(!state.case_hidden_in(pane, other), "Case off keeps the parts not in the case");
        assert!(!state.case_hidden_in(PaneId::PRIMARY, part), "Case off in one pane keeps the case in the others");
        assert!(state.apply_core(&ShellAction::SetDeviceKit(true)) && state.device.kit, "Kit is the device's mode");
        assert!(state.apply_core(&ShellAction::SetDeviceStep(Some(2))) && state.device.step == Some(2), "the device stops at a build step");
        assert!(state.apply_core(&ShellAction::SetDeviceKit(true)) && state.device.step.is_none(), "Kit leaves the build step");

        lease.park();
        state.drain_released_panes();
        assert_eq!(state.shared_poses(), PanePoses::Stage, "a lease parked without the app state hands the shared layers back");
        assert_eq!(state.pose_in(pane, part), Some(up), "parking keeps the pane on its set");
    }

    #[test]
    fn an_unprinted_part_is_a_ghost_on_the_plate_and_solid_in_the_device_pane() {
        let mut state = AppState::default();
        let part = ElementId(4);
        state.plate.parts.push(PlatePart { element: Some(part), printed: false, insert: false, ..Default::default() });
        let mut pose = crate::model::state::POSE_IDENTITY;
        pose[2][3] = 0.02;
        state.pose_set_mut(PoseSetId::DEVICE).set(part, pose);

        let lease = state.lease_pane(PaneRole::Device);
        let pane = lease.id();
        let action = ShellAction::SetPanePoses { pane, poses: PanePoses::Set(PoseSetId::DEVICE) };
        assert!(state.apply_core(&action), "switching the leased pane to the device set requests a redraw");
        state.view.pane_mut(pane).parked = false;

        assert!(!state.is_drawn_in(part, PaneId::PRIMARY), "the plate pane leaves the unprinted part to its ghost pass");
        assert!(state.is_drawn_in(part, pane), "the device pane draws the same part opaque in its assembly");
        assert!(state.is_drawn(part), "the shared layers see the opaque part in the unparked device pane's set");
    }

    #[test]
    fn editor_leases_reuse_slots_and_the_renderer_moves_only_on_park_or_release() {
        let mut state = AppState::default();
        let first = state.lease_pane(PaneRole::Device);
        let second = state.lease_pane(PaneRole::Device);
        let (a, b) = (first.id(), second.id());
        assert_eq!((a, b), (PaneId(1), PaneId(2)), "editor slots start after Plate's");
        // an Underside view of Plate beside them
        let underside = state.view.add_pane(PaneLink::Linked { how: LinkHow::Flipped, to: PaneId::PRIMARY });
        let underside_camera = *state.view.camera(underside);

        let scene = state.scene_gpu.clone();
        assert!(scene.borrow_mut().take_host(b), "the first pane to draw takes the renderer");
        assert!(!scene.borrow_mut().take_host(a), "another draw cannot evict an idle host");
        assert!(!scene.borrow_mut().take_host(a), "a second draw still cannot reclaim the absent host");
        assert_eq!(scene.borrow().host, Some(b), "the host stays with the pane that skipped a draw");
        let camera = Camera { fov_y_deg: 52.0, ..*state.view.camera(b) };
        *state.view.camera_mut(b) = camera;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut viewport = cx.with_vm(crate::viewport::FabViewport::script_new);
        viewport.set_pane(&second);
        viewport.park(&mut cx, &mut state);
        assert_eq!(*state.view.camera(b), camera, "parking preserves the editor slot and camera");
        assert_eq!(scene.borrow().host, None, "parking the host returns its renderer lease");
        assert!(scene.borrow_mut().take_host(a), "the next drawing pane takes the parked renderer");
        scene.borrow_mut().hovered_pane = Some(a);
        scene.borrow_mut().navigation_pane = Some(a);
        state.view.hover = Some(RayHit { element: ElementId(0), batch: 0, triangle: 0, t: 1.0,
            point: vec3(0.0, 0.0, 0.0), normal: vec3(0.0, 0.0, 1.0), bary: [1.0, 0.0] });

        drop(first);
        state.drain_released_panes();
        let replacement = state.lease_pane(PaneRole::Device);
        assert_eq!(replacement.id(), a, "the lowest free editor slot is reused");
        assert_eq!(scene.borrow().hovered_pane, None, "release clears the hovered pane");
        assert_eq!(state.view.hover, None, "release clears the hit published by that pane");
        assert_eq!(scene.borrow().navigation_pane, None, "release clears the navigation owner");
        assert_eq!(scene.borrow().host, None, "a reused slot cannot inherit the renderer lease");
        assert_eq!(*state.view.camera(b), camera, "reusing a slot preserves the other editor camera");
        assert_eq!(state.view.pane(underside).link, PaneLink::Linked { how: LinkHow::Flipped, to: PaneId::PRIMARY }, "release preserves the underside link");
        assert_eq!(*state.view.camera(underside), underside_camera, "release preserves the underside camera");
    }

    #[test]
    fn insert_hosts_draw_as_glass_unless_disabled_or_ghosted() {
        let mut state = AppState::default();
        state.scene = Arc::new(Scene::from_model(crate::model::fixture::parts(), &mut |_| {}));
        state.plate.parts = vec![
            PlatePart { object: "base".into(), printed: true, ..Default::default() },
            PlatePart { object: "post".into(), insert: true, insert_into: "base".into(), ..Default::default() },
        ];
        state.sync_plate_elements();
        let host = state.plate.parts[0].element.expect("case host has mesh geometry");
        let insert = state.plate.parts[1].element.expect("insert has mesh geometry");

        let poses = state.shared_poses();
        assert!(state.is_glass_under(poses, host), "a case shows its insert through its walls");
        assert!(!state.is_drawn(host), "glass leaves every opaque pass");
        assert!(state.is_drawn(insert) && !state.is_glass_under(poses, insert), "the insert stays opaque");

        state.view.glass = false;
        assert!(state.is_drawn(host) && !state.is_glass_under(poses, host), "the view toggle restores the opaque case");

        state.view.glass = true;
        state.plate.parts[0].printed = false;
        assert!(state.is_ghost_under(poses, host), "an excluded case is a ghost");
        assert!(!state.is_glass_under(poses, host), "ghosts never enter the glass lane");
        assert!(!state.is_drawn(host), "ghosts still leave the opaque pass");
    }

    #[test]
    fn unprinted_parts_draw_as_ghosts_unless_hidden() {
        let mut state = AppState::default();
        let first = ElementId::from_index(0);
        let second = ElementId::from_index(1);
        state.plate.parts = vec![
            PlatePart {
                element: Some(first),
                printed: true,
                ..Default::default()
            },
            PlatePart {
                element: Some(second),
                printed: false,
                ..Default::default()
            },
        ];

        let poses = state.shared_poses();
        assert!(!state.is_ghost_under(poses, first));
        assert!(state.is_ghost_under(poses, second));
        assert!(!state.is_drawn(second));
        assert!(state.is_ghost_drawn_under(poses, second));
        assert!(state.is_drawn(first));

        state.scene_state.set_hidden(second, true);
        assert!(!state.is_ghost_drawn_under(poses, second));
        assert!(!state.ghosts_drawn_under(poses), "no ghost left to draw: the ghost pass stays off");
        // A printed part on its way in (a file dragged over the window) is a ghost too.
        state.plate.parts[0].arriving = true;
        assert!(state.is_ghost_drawn_under(poses, first) && state.ghosts_drawn_under(poses), "an arriving part turns the ghost pass on");
    }

    #[test]
    fn plate_key_focus_follows_redraws_but_never_accepts_a_rename_input() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let list = DrawList::new(&mut cx);
        let area = |rect_id, redraw_id| Area::Rect(makepad_platform::RectArea { draw_list_id: list.id(), rect_id, redraw_id });
        let mut ui = UiState::default();
        assert!(!ui.parts_have_focus(Area::Empty));
        // a Plate view's (pane 0's, another Plate's on pane 2) and the parts list's
        for slot in 0..3 {
            let old = area(slot, 1);
            let new = area(slot, 2);
            match slot {
                0 => ui.set_plate_key(PaneId(0), old),
                1 => ui.parts_key = old,
                _ => ui.set_plate_key(PaneId(2), old),
            }
            assert!(ui.parts_have_focus(old));
            ui.refresh_plate_keys(old, new);
            assert!(ui.parts_have_focus(new));
            assert!(!ui.parts_have_focus(old));
            let rename = area(3, 2);
            assert!(!ui.parts_have_focus(rename));
            ui.refresh_plate_keys(rename, area(3, 3));
            assert!(!ui.parts_have_focus(area(3, 3)));
        }
        // Drawing an unfocused overlay cannot register an arbitrary new key owner.
        ui.refresh_plate_keys(Area::Empty, area(4, 1));
        assert!(!ui.parts_have_focus(area(4, 1)));
    }

    /// Nothing done to Monitor's view reaches the other workspaces', and back.
    #[test]
    fn monitor_has_its_own_playback_and_camera() {
        let mut state = AppState::default();
        state.print.play.active = true;
        state.print.play.layer_hi = 5;
        let design = *state.view.camera(PaneId::PRIMARY);
        assert!(state.apply_core(&ShellAction::SetWorkspace(Workspace::Monitor)));
        assert_eq!((state.print.play.layer_hi, *state.view.camera(PaneId::PRIMARY)), (5, design), "Monitor starts where the view is");
        state.print.play.layer_hi = 2;
        state.print.play.color = crate::print::ColorBy::Speed;
        state.view.camera_mut(PaneId::PRIMARY).fov_y_deg = 10.0;
        let monitor = *state.view.camera(PaneId::PRIMARY);
        // Through Mechanics too, which keeps the other workspaces' slice view.
        for w in [Workspace::Design, Workspace::Mechanics, Workspace::Design] {
            assert!(state.apply_core(&ShellAction::SetWorkspace(w)));
            assert_eq!(*state.view.camera(PaneId::PRIMARY), design);
        }
        assert_eq!((state.print.play.layer_hi, state.print.play.color, state.print.play.active), (5, crate::print::ColorBy::Feature, true));
        state.view.camera_mut(PaneId::PRIMARY).fov_y_deg = 20.0;
        let design = *state.view.camera(PaneId::PRIMARY);
        state.apply_core(&ShellAction::SetWorkspace(Workspace::Mechanics));
        state.apply_core(&ShellAction::SetWorkspace(Workspace::Monitor));
        assert_eq!(*state.view.camera(PaneId::PRIMARY), monitor);
        state.apply_core(&ShellAction::SetWorkspace(Workspace::Monitor));
        assert_eq!(*state.view.camera(PaneId::PRIMARY), monitor, "again in Monitor: nothing swaps");
        state.apply_core(&ShellAction::SetWorkspace(Workspace::Mechanics));
        assert_eq!(*state.view.camera(PaneId::PRIMARY), design);
        assert!(!state.print.play.active, "the sim's view has no slice");
        state.apply_core(&ShellAction::SetWorkspace(Workspace::Design));
        assert!(state.print.play.active, "and Design's comes back");
    }

    #[test]
    fn design_can_delete_while_slice_is_running_or_shown_monitor_is_read_only() {
        let mut state = AppState::default();
        state.print.play.active = true;
        state.print.status = crate::print::SliceStatus::Running { progress: 0.5, stage: "Slicing".into() };
        assert!(!state.slice_blocks_edits());
        state.ui.enter(Workspace::Monitor);
        assert!(state.slice_blocks_edits());
        state.ui.enter(Workspace::Design);
        assert!(!state.slice_blocks_edits());
    }


    /// Framing ignores a stray decode-artifact mesh: a few hundred vertices
    /// far below the plate must not widen the box the camera frames.
    #[test]
    fn framing_bounds_ignore_an_outlier_mesh() {
        let mut scene = Scene::from_model(crate::model::fixture::synthetic_model(20_000, 40), &mut |_| {});
        let honest = framing_bounds(&scene);
        assert!(!aabb_is_empty(&honest));
        assert!(honest.min.z >= scene.bounds.min.z - 1.0 && honest.max.z <= scene.bounds.max.z + 1.0);
        // 3 % of the vertices mirrored to z = -134.
        let stride = crate::model::VERTEX_STRIDE;
        let total: usize = scene.batches.iter().map(|b| b.vertices.len() / stride).sum();
        let n_out = (total * 3 / 100).max(8);
        let mut extra = Vec::with_capacity(n_out * stride);
        for i in 0..n_out {
            let mut v = vec![0.0f32; stride];
            v[0] = (i % 7) as f32;
            v[1] = (i % 5) as f32;
            v[2] = -134.0;
            extra.extend_from_slice(&v);
        }
        scene.batches[0].vertices.extend_from_slice(&extra);
        let framed = framing_bounds(&scene);
        assert!(framed.min.z > -20.0, "outlier leaked into the framing box: {:?}", framed);
        assert!((framed.min.z - honest.min.z).abs() < 2.0 && (framed.max.z - honest.max.z).abs() < 2.0);
    }

    /// A scene of `synthetic_model`'s spheres scaled down to a plate's size in metres, as Fab's
    /// scenes are.
    fn metres_scene() -> Scene {
        let mut scene = Scene::from_model(crate::model::fixture::synthetic_model(20_000, 40), &mut |_| {});
        let stride = crate::model::VERTEX_STRIDE;
        for b in &mut scene.batches {
            for v in b.vertices.chunks_exact_mut(stride) {
                v[0] = 0.15 + v[0] * 0.004;
                v[1] = 0.15 + v[1] * 0.004;
                v[2] *= 0.004;
            }
        }
        scene
    }

    #[test]
    fn a_close_camera_keeps_what_it_looks_at() {
        let plate = Aabb { min: vec3(0.0, 0.0, 0.0), max: vec3(0.35, 0.32, 0.03) };
        // What every backend clips: a point stays while its clip z is within 0..w.
        let kept = |cam: &Camera, p: Vec3f| {
            let c = cam.view_projection(1.0).transform_vec4(vec4(p.x, p.y, p.z, 1.0));
            c.z >= 0.0 && c.z <= c.w
        };
        let wall = vec3(0.128, 0.110, 0.015);
        let mut cam = Camera::default();
        cam.target = wall;
        // A wall at the closest a dolly goes, and the ridge standing out of it towards the eye.
        cam.eye = wall - vec3(0.0, crate::nav::orbit::MIN_DISTANCE, 0.0);
        cam.fit_clip_planes(&plate, 1.0);
        assert!(kept(&cam, wall), "near {}", cam.near);
        assert!(kept(&cam, wall - vec3(0.0, 0.015, 0.0)), "near {}", cam.near);
        // Far off the near plane stays where the depth buffer wants it.
        cam.eye = vec3(0.45, -0.5, 0.35);
        cam.fit_clip_planes(&plate, 1.0);
        assert_eq!(cam.near, 0.02);
    }

    /// Orthographic, zoomed in close on a wall of the plate from inside the plate's box: the eye
    /// stands back behind the box, every corner of it is drawn (its clip z within 0..w), nothing
    /// behind the eye projects, and each pixel's ray starts behind the box, so the wall is the
    /// first thing it meets. The same at rest: the next frame's fit keeps the camera.
    #[test]
    fn an_orthographic_view_holds_the_scene_however_close_it_is() {
        let plate = Aabb { min: vec3(0.0, 0.0, 0.0), max: vec3(0.35, 0.32, 0.03) };
        let kept = |cam: &Camera, p: Vec3f| {
            let c = cam.view_projection(1.5).transform_vec4(vec4(p.x, p.y, p.z, 1.0));
            c.z >= 0.0 && c.z <= c.w
        };
        let wall = vec3(0.128, 0.110, 0.015);
        for (look, what) in [(vec3(0.0, 1.0, 0.0), "along the bed"), (vec3(0.3, 1.0, -0.4), "down onto it"), (vec3(0.0, 0.0, -1.0), "straight down")] {
            let mut cam = Camera { target: wall, up: if look.z == -1.0 { vec3(0.0, 1.0, 0.0) } else { vec3(0.0, 0.0, 1.0) }, ..Camera::default() };
            cam.eye = wall - look.normalize() * crate::nav::orbit::MIN_DISTANCE;
            crate::nav::orbit::set_ortho(&mut cam, true);
            cam.fit_clip_planes(&plate, 1.5);
            for c in corners(&plate) {
                assert!(kept(&cam, c), "{what}: the corner {c:?} clipped: near {}, far {}", cam.near, cam.far);
            }
            assert!(cam.project(cam.eye + (cam.eye - cam.target), 1.5).is_none(), "{what}");
            for ndc in [vec2(0.0, 0.0), vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(1.0, -1.0)] {
                let ray = cam.ray_at_ndc(ndc, 1.5);
                assert!(ray.dir.dot(cam.forward()) > 0.99999, "{what}: the rays are parallel");
                assert!(corners(&plate).iter().all(|c| (*c - ray.origin).dot(ray.dir) > 0.0), "{what}: the ray at {ndc:?} starts inside the plate's box at {:?}", ray.origin);
            }
            let at_rest = cam;
            cam.fit_clip_planes(&plate, 1.5);
            assert_eq!(cam, at_rest, "{what}: the next frame moves the camera");
        }
    }

    /// Orthographic over the bed seen from above, the floor runs past the scene to the view's
    /// edges: toward the eye at the bottom of the view, away at the top. The eye stands back
    /// behind all of the floor the view shows, and the planes hold it, so a view low over the
    /// bed draws its floor to its edges.
    #[test]
    fn an_orthographic_view_holds_the_floor_it_shows() {
        let plate = Aabb { min: vec3(0.0, 0.0, 0.0), max: vec3(0.35, 0.32, 0.03) };
        let aspect = 1.6;
        let mut cam = Camera { target: vec3(0.17, 0.16, 0.0), eye: vec3(0.17, -0.3, 0.12), ..Camera::default() };
        crate::nav::orbit::set_ortho(&mut cam, true);
        cam.ortho_height = 2.0;
        cam.fit_clip_planes(&plate, aspect);
        for ndc in [vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0)] {
            let ray = cam.ray_at_ndc(ndc, aspect);
            let t = -ray.origin.z / ray.dir.z;
            assert!(t > 0.0, "the floor at {ndc:?} is behind the ray's start");
            let floor = ray.origin + ray.dir * t;
            let c = cam.view_projection(aspect).transform_vec4(vec4(floor.x, floor.y, floor.z, 1.0));
            assert!(c.z >= 0.0 && c.z <= c.w, "the floor at {ndc:?} clipped: near {}, far {}", cam.near, cam.far);
        }
    }

    /// The framing box follows what is there, whatever the scale: no padding in scene units.
    #[test]
    fn framing_bounds_have_no_absolute_pad() {
        let scene = metres_scene();
        let framed = framing_bounds(&scene);
        let extent = aabb_radius(&framed) * 2.0;
        assert!(extent < 0.15, "a box under 15 cm across stays that: {framed:?}");
    }

    /// A low-poly part beside a dense one is few vertices, not a stray: it stays in the box.
    #[test]
    fn framing_bounds_keep_a_low_poly_part() {
        let mut scene = metres_scene();
        let dense = framing_bounds(&scene);
        let stride = crate::model::VERTEX_STRIDE;
        let x = dense.max.x + (dense.max.x - dense.min.x) * 0.5;
        let mut part = vec![0.0f32; stride * 24];
        for v in part.chunks_exact_mut(stride) {
            (v[0], v[1], v[2]) = (x, dense.min.y, dense.min.z);
        }
        scene.batches[0].vertices.extend_from_slice(&part);
        assert_eq!(framing_bounds(&scene).max.x, x);
    }

    /// A part drawn from its field has no vertices, and counts whole.
    #[test]
    fn framing_bounds_keep_a_field_part() {
        let mut scene = metres_scene();
        let field = Aabb { min: vec3(0.5, 0.5, 0.0), max: vec3(0.52, 0.52, 0.3) };
        let id = ElementId(scene.elements.len() as u32);
        scene.elements.push(crate::model::Element { id, name: "field".into(), bounds: aabb_empty(), ranges: Vec::new(), triangle_count: 0 });
        scene.set_field_bounds(id, field);
        assert_eq!(framing_bounds(&scene).max, field.max);
    }

    fn plate_state() -> AppState {
        let mut state = AppState::default();
        state.plate.bed_min = [0.0, 0.0];
        state.plate.bed_max = [350.0, 320.0];
        state.plate.max_height = 325.0;
        state.scene = Arc::new(metres_scene());
        state
    }

    /// Frame All makes the plate large whatever stands on it: its bounding sphere fits the view,
    /// so the whole width of a tall view and about half of a wide one.
    #[test]
    fn frame_all_fills_the_view_with_the_plate() {
        let state = plate_state();
        let bounds = state.frame_all_bounds();
        for aspect in [581.0 / 938.0, 1.0, 16.0 / 9.0] {
            let mut cam = Camera::default();
            cam.frame_bounds(&bounds, aspect);
            let corners = [[0.0, 0.0], [0.35, 0.0], [0.35, 0.32], [0.0, 0.32]].map(|[x, y]: [f32; 2]| cam.project(vec3(x, y, 0.0), aspect).unwrap());
            let span = |f: fn(&Vec3f) -> f32| corners.iter().map(f).fold(f32::MIN, f32::max) - corners.iter().map(f).fold(f32::MAX, f32::min);
            let (wide, tall) = (span(|c| c.x), span(|c| c.y));
            assert!(corners.iter().all(|c| c.x.abs() <= 1.0 && c.y.abs() <= 1.0), "aspect {aspect}: the plate is in view: {corners:?}");
            assert!(wide.max(tall) > 0.9, "aspect {aspect}: the plate takes about half the view or more: {wide} wide, {tall} tall of 2");
        }
    }

    /// Mechanics shows no printer: Frame All looks at the scene alone.
    #[test]
    fn frame_all_in_mechanics_ignores_the_plate() {
        let mut state = plate_state();
        state.ui.enter(Workspace::Mechanics);
        assert_eq!(state.frame_all_bounds().min, framing_bounds(&state.scene).min);
    }

    #[test]
    fn camera_ray_hits_target() {
        let cam = Camera::default();
        let ray = cam.ray_at_ndc(vec2(0.0, 0.0), 1.5);
        let to_target = (cam.target - ray.origin).normalize();
        assert!(ray.dir.dot(to_target) > 0.999, "{:?} vs {:?}", ray.dir, to_target);
    }

    #[test]
    fn project_center_is_origin() {
        let cam = Camera::default();
        let ndc = cam.project(cam.target, 1.5).unwrap();
        assert!(ndc.x.abs() < 1e-4 && ndc.y.abs() < 1e-4);
        let proj = ViewProjector::new(cam, Rect { pos: dvec2(10.0, 20.0), size: dvec2(300.0, 200.0) });
        let p = proj.project(cam.target).unwrap();
        assert!((p.x - 160.0).abs() < 0.01 && (p.y - 120.0).abs() < 0.01);
    }

    #[test]
    fn frame_bounds_contains_sphere() {
        use fab_elec_view::instruments::{logic::LaneKind, logic_view::LaneSize};

        let mut cam = Camera::default();
        let b = Aabb { min: vec3(-5.0, -5.0, 0.0), max: vec3(5.0, 5.0, 6.0) };
        cam.frame_bounds(&b, 1.6);
        for c in corners(&b) {
            let ndc = cam.project(c, 1.6).unwrap();
            assert!(ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0, "{ndc:?}");
        }
        for ortho in [false, true] {
            let camera = Camera { ortho, hung: true, ..cam };
            let view = PlateView {
                camera, preset: Some(PresetView::Isometric),
                // a second Plate view's own camera, by its instance number
                plates: vec![(1, Camera { eye: vec3(0.5, -0.25, 1.5), ortho: !ortho, ..cam }, Some(PresetView::Top))],
                device: Some(Camera { eye: vec3(-1.0, 2.0, 3.0), ortho: !ortho, ..cam }),
                sheet: Some(SavedView2d { center: [19.25, -3.5], px_per_unit: 8.5 }),
                board: Some(SavedView2d { center: [-0.25, 24.0], px_per_unit: 17.75 }),
                // two Scopes, the second written first: each keeps its number
                scopes: vec![
                    (1, ScopeView { order: vec!["clock".into(), "data".into()], take: Some([0.068, 0.122, 0.2]), ..Default::default() }),
                    (0, ScopeView {
                        per_div: Some(0.002),
                        order: vec!["data".into(), "clock".into()],
                        sizes: vec![("data".into(), LaneSize::Large)],
                        kinds: vec![("clock".into(), LaneKind::Digital), ("data".into(), LaneKind::Analog)],
                        take: None,
                    }),
                ],
                selected: vec!["lid".into(), "base".into()], active: Some("lid".into()),
                viewed: Some("lid.cad.splash".into()),
            };
            let text = view.to_file_text();
            assert!(text.contains("Large") && text.contains("Digital") && text.contains("Analog"),
                "lane sizes and kinds are written by name");
            let restored = PlateView::from_file_text(&text).expect("a saved plate view reads back");
            assert_eq!(restored, view);
            assert!(restored.camera.sees(b, 1.6), "the saved camera frames the box");
            let away = Camera { target: camera.eye + (camera.eye - camera.target), ..camera };
            assert!(!away.sees(b, 1.6), "the box is behind the camera");
            assert!(PlateView::from_file_text(&(text + " garbage")).is_none());
        }
        assert!(PlateView::from_file_text("garbage").is_none());
        let legacy = "(cameras: [(n: 0, eye: (1, -2, 3), target: (0, 0, 0), up: (0, 0, 1),
            fov_y_deg: 40, ortho: false, ortho_height: 0.5, near: 0.02, far: 4000)], selected: [])";
        let legacy = PlateView::from_file_text(legacy).expect("a view file without optional cameras reads");
        assert_eq!((legacy.device, legacy.sheet, legacy.board), (None, None, None));
        assert!(legacy.scopes.is_empty(), "a file without scopes opens with fresh Scope views");
        assert!(legacy.plates.is_empty(), "a file of one Plate view opens the others at their own start");
        {
            // restored: the first Plate's camera on pane 0, the second's on the pane its stage
            // leased, a third's kept for its bind; saved again, each by its number
            let mut state = AppState::default();
            let lease = state.lease_pane(PaneRole::Plate);
            let fourth = state.lease_pane(PaneRole::Plate);
            state.view.plate_instances = vec![(1, lease.id()), (3, fourth.id())];
            let second = Camera { eye: vec3(0.5, -0.25, 1.5), ..cam };
            let view = PlateView { plates: vec![(1, second, Some(PresetView::Top)), (2, cam, None)], ..legacy.clone() };
            state.restore_view(&view, true);
            assert_eq!(*state.view.camera(PaneId::PLATE_ZERO), legacy.camera, "the first Plate's camera is pane 0's");
            assert_eq!((*state.view.camera(lease.id()), state.view.pane(lease.id()).preset), (second, Some(PresetView::Top)), "the second Plate's camera is its own pane's");
            assert_eq!(*state.view.camera(fourth.id()), legacy.camera, "a Plate view the file has no camera for opens at the first's");
            assert_eq!(state.view.saved_plates, view.plates, "a Plate view not bound yet keeps its camera for its bind");
            assert_eq!(state.plate_view().plates, vec![(1, second, Some(PresetView::Top)), (3, legacy.camera, None)], "each bound Plate view is saved by its number");
            // a later Plate view's saved camera that sees nothing of the scene at its own aspect
            // (the plate's scripts changed since) frames the plate; one that sees it is kept
            let mut scene = Scene::empty();
            scene.bounds = b;
            state.scene = Arc::new(scene);
            let away = Camera { target: cam.eye + (cam.eye - cam.target), ..cam };
            state.restore_view(&PlateView { camera: cam, plates: vec![(1, away, None), (3, cam, None)], ..legacy.clone() }, true);
            assert_eq!(*state.view.camera(lease.id()), away, "the saved camera goes back as it was");
            assert!(state.restored_frame(lease.id(), 1.6).is_some(), "a later Plate view that sees nothing of the scene frames the plate");
            assert!(state.restored_frame(fourth.id(), 1.6).is_none(), "a later Plate view that sees the scene keeps its camera");
            assert!(state.restored_frame(PaneId::PLATE_ZERO, 1.6).is_none(), "the first Plate view sees the scene");
        }

        let mut state = AppState::default();
        let lease = state.lease_pane(PaneRole::Device);
        state.device.pane = Some(lease.id());
        *state.view.camera_mut(lease.id()) = cam;
        assert_eq!(state.plate_view().device, None, "automatic framing is omitted");
        state.device.placed = true;
        assert_eq!(state.plate_view().device, Some(cam), "a placed camera comes from the Device pane");
        let saved = Camera { ortho: true, ..cam };
        state.device.restore = Some(DeviceRestore::Camera(saved));
        assert_eq!(state.plate_view().device, Some(saved), "a pending restore belongs to the opening plate");
        state.device.restore = Some(DeviceRestore::Frame);
        assert_eq!(state.plate_view().device, None, "a fresh opening supersedes the previous camera");
        state.device.restore = None;
        drop(lease);
        state.drain_released_panes();
        assert_eq!(state.device.pane, None, "the Device view forgets its released slot");
        assert_eq!(state.plate_view().device, Some(cam), "a placed camera survives release of its pane");
    }

    #[test]
    fn sun_is_up_at_noon_in_summer() {
        let s = SkyState {
            time_local: 12.0,
            longitude: 0.0,
            tz_offset: 0.0,
            north_deg: 0.0,
            ..Default::default()
        };
        assert!(s.elevation_deg() > 50.0, "{}", s.elevation_deg());
        let d = s.direction();
        // Northern hemisphere: the sun is to the south (−Y) at noon.
        assert!(d.y < 0.0);
        let night = SkyState {
            time_local: 1.0,
            ..s
        };
        assert!(night.elevation_deg() < 0.0);
    }

    fn angular_error(a: f32, b: f32) -> f32 {
        ((a - b + 180.0).rem_euclid(360.0) - 180.0).abs()
    }

    fn assert_noaa(case: &str, got: (f32, f32), expected: (f32, f32)) {
        assert!(
            (got.0 - expected.0).abs() <= 0.5,
            "{case}: elevation {got:?} expected {expected:?}"
        );
        assert!(
            angular_error(got.1, expected.1) <= 0.5,
            "{case}: azimuth {got:?} expected {expected:?}"
        );
    }

    #[test]
    fn noaa_reference_golden_colorado() {
        assert_noaa(
            "Golden 2003-10-17 12:30:30 MST",
            noaa_solar_position(
                SkyDate {
                    year: 2003,
                    month: 10,
                    day: 17,
                },
                12.0 + 30.5 / 60.0,
                -7.0,
                39.742476,
                -105.1786,
            ),
            (39.8884, 194.3402),
        );
    }

    #[test]
    fn noaa_reference_greenwich_equinox() {
        assert_noaa(
            "Greenwich 2024-03-20 12:00 UTC",
            noaa_solar_position(
                SkyDate {
                    year: 2024,
                    month: 3,
                    day: 20,
                },
                12.0,
                0.0,
                51.4779,
                0.0,
            ),
            (38.61, 177.65),
        );
    }

    #[test]
    fn ordered_selection_uses_the_clicked_rows_first_element_as_active() {
        let mut state = AppState::default();
        let first = ElementId::from_index(7);
        let second = ElementId::from_index(2);

        assert!(state.apply_core(&ShellAction::SelectSet(vec![first, second])));
        assert_eq!(state.scene_state.selection.active, Some(first));
        assert!(state.scene_state.selection.contains(first));
        assert!(state.scene_state.selection.contains(second));
    }

    /// The Arm and Scan workspaces are experimental (user, 11-20: "keep scan/arms in
    /// experimental"): hidden, nothing switches to them (a shortcut, the wrist, a stale menu row);
    /// offered, each opens independently; turned on, it opens; hidden again while it shows, Design
    /// comes back. The electronics workspaces are always offered.
    #[test]
    fn the_experimental_arm_workspace_opens_only_while_offered() {
        let mut state = AppState::default();
        assert!(!state.ui.arm_shown && !state.workspace_shown(Workspace::Arm) && state.workspace_shown(Workspace::Monitor));
        assert!(!state.ui.scan_shown && !state.workspace_shown(Workspace::Scan));
        assert!(state.workspace_shown(Workspace::Electronics), "the electronics workspaces need no switch");
        assert!(!state.apply_core(&ShellAction::SetWorkspace(Workspace::Arm)));
        assert!(!state.apply_core(&ShellAction::SetWorkspace(Workspace::Scan)));
        assert_eq!(state.ui.kind(), Workspace::Design);
        state.ui.arm_shown = true;
        assert!(state.workspace_shown(Workspace::Arm));
        assert!(!state.workspace_shown(Workspace::Scan));
        assert!(!state.apply_core(&ShellAction::SetWorkspace(Workspace::Scan)));
        assert!(state.apply_core(&ShellAction::SetWorkspace(Workspace::Arm)));
        assert_eq!(state.ui.kind(), Workspace::Arm);
        assert!(state.apply_core(&ShellAction::ShowArm(false)));
        assert_eq!(state.ui.kind(), Workspace::Design);
        state.ui.scan_shown = true;
        assert!(state.workspace_shown(Workspace::Scan));
        assert!(!state.workspace_shown(Workspace::Arm));
        assert!(!state.apply_core(&ShellAction::SetWorkspace(Workspace::Arm)));
        assert!(state.apply_core(&ShellAction::SetWorkspace(Workspace::Scan)));
        assert_eq!(state.ui.kind(), Workspace::Scan);
        assert!(state.apply_core(&ShellAction::ShowScan(false)));
        assert_eq!(state.ui.kind(), Workspace::Design);
        assert!(!state.ui.scan_shown && !state.workspace_shown(Workspace::Scan));
        assert!(!state.apply_core(&ShellAction::SetWorkspace(Workspace::Scan)));
        // Turning one on offers it and opens it at once.
        assert!(state.apply_core(&ShellAction::ShowArm(true)));
        assert!(state.workspace_shown(Workspace::Arm), "turned on, Arm is offered");
        assert_eq!(state.ui.kind(), Workspace::Arm, "turning Arm on opens it");
        assert!(state.apply_core(&ShellAction::ShowScan(true)));
        assert!(state.workspace_shown(Workspace::Scan), "turned on, Scan is offered");
        assert_eq!(state.ui.kind(), Workspace::Scan, "turning Scan on opens it");
        assert!(state.apply_core(&ShellAction::ShowArm(false)));
        assert_eq!(state.ui.kind(), Workspace::Scan, "turning off a workspace that is not showing keeps the one that is");
        assert!(state.apply_core(&ShellAction::ShowScan(false)));
        assert_eq!(state.ui.kind(), Workspace::Design, "turning off the showing workspace brings Design back");
        assert!(state.apply_core(&ShellAction::SetWorkspace(Workspace::Electronics)));
        assert_eq!(state.ui.kind(), Workspace::Electronics, "an electronics workspace opens without a switch");
    }
}

impl PlateInfo {
    /// The plate's own parts, whatever the view shows.
    pub fn whole_parts(&self) -> &[PlatePart] {
        self.whole.as_ref().map_or(&self.parts, |w| w.as_slice())
    }
}

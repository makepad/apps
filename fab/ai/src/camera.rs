//! `fab.render`'s cameras, as plain math: the views it knows, how each frames what it shows, and
//! where a view's pixel looks. Nothing here draws: the review renderer draws through these, and a
//! tool that reads a render (a pixel to a point on the model, a picture's points to mm) takes the
//! same cameras from here.
//!
//! Frames: the plate in printer mm (Z up); the glTF frame the render's meshes are written in
//! (metres, Y up, [`gltf_point`]); the scene the renderer draws, where what the views frame is a
//! box 2 across about the origin ([`Placed`]).

use crate::Focus;
use makepad_math::{vec3f, Mat4f, Vec3f};

/// A view's side (pixels) when three or more are asked.
pub const SIDE: usize = 512;

/// A view's side (pixels) for a render of `views` views: one or two views take the picture
/// budget four would share, so they render larger (and come back as large as it lets them).
pub fn view_side(views: usize) -> usize {
    match views {
        1 => 3 * SIDE,
        2 => 2 * SIDE,
        _ => SIDE,
    }
}

/// The standard views' count.
pub const VIEWS: usize = 8;
/// A render's field of view, degrees, unless one is asked.
pub const FOV: f32 = 42.;
/// A picture aimed at a place: narrow, so it reads nearly square-on and one bar's mm hold across it.
pub const LOOK_FOV: f32 = 20.;
/// The furthest a view's pitch goes either way: past it the camera's up would turn over.
pub const PITCH_LIMIT: f32 = 1.45;
/// How much of a view's half side what it frames fills: 6% margin on each side.
const FILL: f32 = 0.88;
// The pitch is clamped to +/-1.45, so label the near-top honestly.
const REST_ANGLES: [(f32, f32); VIEWS] = [
    (std::f32::consts::FRAC_PI_4, -std::f32::consts::FRAC_PI_4),
    (std::f32::consts::PI + std::f32::consts::FRAC_PI_4, -std::f32::consts::FRAC_PI_4),
    (std::f32::consts::FRAC_PI_4, -0.35),
    (-std::f32::consts::FRAC_PI_4, -0.35),
    (0., 0.),
    (std::f32::consts::FRAC_PI_2, 0.),
    (0., -PITCH_LIMIT),
    (std::f32::consts::FRAC_PI_4, 0.6),
];

/// One camera of a render: (yaw, pitch) radians and what to call it.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub name: String,
    pub yaw: f32,
    pub pitch: f32,
}

/// A view by name (see [`crate::VIEW_NAMES`]) or "yaw,pitch" in degrees.
/// Yaw 0 looks at the front (from +Z), positive pitch from below.
pub fn named_view(name: &str) -> Option<View> {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    let (yaw, pitch) = match name {
        "front" => (0., 0.),
        "back" => (PI, 0.),
        "left" => (-FRAC_PI_2, 0.),
        "right" => (FRAC_PI_2, 0.),
        "top" => (0., -PITCH_LIMIT),
        "bottom" | "underside" => (0., PITCH_LIMIT),
        "iso" => (FRAC_PI_4, -FRAC_PI_4),
        "iso_back" => (PI + FRAC_PI_4, -FRAC_PI_4),
        "iso_left" => (-FRAC_PI_4, -FRAC_PI_4),
        "iso_right" => (FRAC_PI_2 + FRAC_PI_4, -FRAC_PI_4),
        "low" => (FRAC_PI_4, -0.35),
        custom => {
            let (a, b) = custom.split_once(',')?;
            (a.trim().parse::<f32>().ok()?.to_radians(), b.trim().parse::<f32>().ok()?.to_radians())
        }
    };
    Some(View { name: name.to_string(), yaw, pitch })
}

/// The standard eight views: four volume views, then four silhouettes.
pub fn default_views() -> Vec<View> {
    let names = ["iso", "iso_back", "low", "low_left", "front", "side", "near_top", "underside"];
    REST_ANGLES.iter().zip(names).map(|(&(yaw, pitch), name)| View { name: name.into(), yaw, pitch }).collect()
}

/// Printer mm in the glTF frame.
pub fn gltf_point(p: [f32; 3]) -> [f32; 3] {
    [p[0] * 0.001, p[2] * 0.001, -p[1] * 0.001]
}

/// A glTF point in printer mm ([`gltf_point`]'s inverse).
pub fn plate_point(p: [f32; 3]) -> [f64; 3] {
    [p[0] as f64 * 1000.0, -p[2] as f64 * 1000.0, p[1] as f64 * 1000.0]
}

/// A plate box (printer mm, corners `lo` and `hi`) in the glTF frame, its corners low and high.
pub fn gltf_box(lo: [f64; 3], hi: [f64; 3]) -> [[f32; 3]; 2] {
    [gltf_point([lo[0] as f32, hi[1] as f32, lo[2] as f32]), gltf_point([hi[0] as f32, lo[1] as f32, hi[2] as f32])]
}

/// What a render's views frame (glTF frame), from what it shows and its focus: the focus's box,
/// or the box of its ball (true: the views frame the ball, not the box).
pub fn focus_bounds(focus: Option<Focus>, shown: [[f32; 3]; 2]) -> ([[f32; 3]; 2], bool) {
    match focus {
        None => (shown, false),
        Some(Focus::Box(lo, hi)) => (gltf_box(lo, hi), false),
        Some(Focus::Ball(at, r)) => (gltf_box(at.map(|c| c - r), at.map(|c| c + r)), true),
    }
}

/// Where the renderer puts what a render frames: in a box 2 across its longest side, at the
/// origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// Scene units per glTF metre.
    pub scale: f32,
    /// The frame's middle, glTF frame.
    pub center: [f32; 3],
    /// The frame's half sides, scene units.
    pub half: Vec3f,
}

impl Placed {
    pub fn of([min, max]: [[f32; 3]; 2]) -> Placed {
        let scale = 2. / (max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2]).max(0.000_001);
        Placed { scale, center: std::array::from_fn(|k| (min[k] + max[k]) * 0.5), half: vec3f((max[0] - min[0]) * 0.5 * scale, (max[1] - min[1]) * 0.5 * scale, (max[2] - min[2]) * 0.5 * scale) }
    }

    /// A glTF point in the scene.
    pub fn at(&self, p: [f32; 3]) -> Vec3f {
        vec3f((p[0] - self.center[0]) * self.scale, (p[1] - self.center[1]) * self.scale, (p[2] - self.center[2]) * self.scale)
    }

    /// A scene point in the glTF frame ([`Placed::at`]'s inverse).
    pub fn back(&self, p: Vec3f) -> [f32; 3] {
        [p.x / self.scale + self.center[0], p.y / self.scale + self.center[1], p.z / self.scale + self.center[2]]
    }

    /// The glTF frame in the scene: scaled, then moved so the frame's middle is the origin.
    pub fn transform(&self) -> Mat4f {
        let mut transform = Mat4f::scale(self.scale);
        transform.v[12] = -self.center[0] * self.scale;
        transform.v[13] = -self.center[1] * self.scale;
        transform.v[14] = -self.center[2] * self.scale;
        transform
    }
}

/// The camera's axes: forward, right, up.
pub fn camera_axes(yaw: f32, pitch: f32) -> (Vec3f, Vec3f, Vec3f) {
    (vec3f(yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos()), vec3f(yaw.cos(), 0., yaw.sin()), vec3f(-yaw.sin() * pitch.sin(), pitch.cos(), yaw.cos() * pitch.sin()))
}

/// A view's camera in the scene: it looks at `target` from `distance` away, along
/// [`camera_axes`]'s forward, `fov` degrees across; or, orthographic, every ray parallel, the view
/// `2 * ortho` across.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub target: Vec3f,
    pub distance: f32,
    pub fov: f32,
    pub yaw: f32,
    pub pitch: f32,
    /// Orthographic: half the view's side, scene units; None: perspective.
    pub ortho: Option<f32>,
}

/// An orthographic projection `2 * half_w` by `2 * half_h` across, its depth running 0..1 from
/// `near` to `far` (clip z runs 0..w at the GPU on every backend).
pub fn ortho_projection(half_w: f32, half_h: f32, near: f32, far: f32) -> Mat4f {
    Mat4f { v: [1.0 / half_w, 0.0, 0.0, 0.0, 0.0, 1.0 / half_h, 0.0, 0.0, 0.0, 0.0, 1.0 / (near - far), 0.0, 0.0, 0.0, near / (near - far), 1.0] }
}

impl Look {
    /// Where the camera stands.
    pub fn eye(&self) -> Vec3f {
        let (forward, _, _) = camera_axes(self.yaw, self.pitch);
        self.target - forward * self.distance
    }

    /// The scene seen from the camera.
    pub fn view_matrix(&self) -> Mat4f {
        Mat4f::look_at(self.eye(), self.target, vec3f(0., 1., 0.))
    }

    /// The view's projection, square: the near plane in front of all it frames; orthographic,
    /// its depth from the camera through twice its distance (all it draws: [`Frame::orthographic`]).
    pub fn projection(&self) -> Mat4f {
        match self.ortho {
            Some(half) => ortho_projection(half, half, 0.01, 2.0 * self.distance),
            None => Mat4f::perspective(self.fov, 1.0, (0.1 * self.distance).min(0.15), 500.0),
        }
    }

    /// Where a scene point shows in a view `side` pixels square: its pixel position ((0, 0) the
    /// top left corner), neither rounded nor kept to the view, and its depth (scene units in
    /// front of the camera, along its middle ray). None: it is not in front of the camera.
    pub fn view_point(&self, p: Vec3f, side: usize) -> Option<(f32, f32, f32)> {
        let (forward, right, up) = camera_axes(self.yaw, self.pitch);
        let depth = p.dot(forward) + self.distance;
        let (x, y) = match self.ortho {
            Some(half) => (p.dot(right) / half, p.dot(up) / half),
            None => {
                let tan = (self.fov.to_radians() * 0.5).tan();
                (p.dot(right) / (tan * depth), p.dot(up) / (tan * depth))
            }
        };
        (depth > 0.0).then(|| ((0.5 + 0.5 * x) * side as f32, (0.5 - 0.5 * y) * side as f32, depth))
    }

    /// The ray through a point of a view `side` pixels square ((0, 0) its top left corner, a
    /// pixel's middle half a pixel in): from the eye (orthographic, from the eye's plane), a unit
    /// direction, in the scene.
    pub fn ray(&self, side: usize, x: f32, y: f32) -> (Vec3f, Vec3f) {
        let (forward, right, up) = camera_axes(self.yaw, self.pitch);
        let (u, v) = (x / side as f32 * 2.0 - 1.0, 1.0 - y / side as f32 * 2.0);
        match self.ortho {
            Some(half) => (self.eye() + right * (u * half) + up * (v * half), forward),
            None => {
                let tan = (self.fov.to_radians() * 0.5).tan();
                (self.eye(), (forward + right * (u * tan) + up * (v * tan)).normalize())
            }
        }
    }
}

/// A point of the scene in the pixels of a view `side` square, if it is in the view.
pub fn project(look: &Look, p: Vec3f, side: usize) -> Option<(i32, i32)> {
    let (x, y, _) = look.view_point(p, side)?;
    let s = side as f32;
    (x > 0.0 && x < s && y > 0.0 && y < s).then(|| (x as i32, y as i32))
}

/// Fit a box of half sides `half` about the origin, leaving 6% frame margin per side.
pub fn fitted_look(half: Vec3f, yaw: f32, pitch: f32, fov: f32) -> Look {
    let pitch = pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
    let (forward, right, up) = camera_axes(yaw, pitch);
    let tan = (fov.to_radians() * 0.5).tan() * FILL;
    let mut distance = 0.5f32;
    for x in [-half.x, half.x] {
        for y in [-half.y, half.y] {
            for z in [-half.z, half.z] {
                let point = vec3f(x, y, z);
                let depth = point.dot(forward);
                distance = distance.max(point.dot(right).abs().max(point.dot(up).abs()) / tan - depth);
                distance = distance.max(0.2 - depth);
            }
        }
    }
    Look { target: vec3f(0., 0., 0.), distance, fov, yaw, pitch, ortho: None }
}

/// Fit a ball of `radius` about the origin, leaving 6% frame margin per side.
pub fn fitted_ball(radius: f32, yaw: f32, pitch: f32, fov: f32) -> Look {
    let reach = ((fov.to_radians() * 0.5).tan() * FILL).atan();
    Look { target: vec3f(0., 0., 0.), distance: radius / reach.sin(), fov, yaw, pitch: pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT), ortho: None }
}

/// Half the side an orthographic view from (yaw, pitch) needs to hold a box of half sides `half`
/// about the origin, with the perspective views' margin.
fn ortho_half(half: Vec3f, yaw: f32, pitch: f32) -> f32 {
    let (_, right, up) = camera_axes(yaw, pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT));
    let mut most = 0.0f32;
    for x in [-half.x, half.x] {
        for y in [-half.y, half.y] {
            for z in [-half.z, half.z] {
                let point = vec3f(x, y, z);
                most = most.max(point.dot(right).abs()).max(point.dot(up).abs());
            }
        }
    }
    most / FILL
}

/// An orthographic render's scale: every view takes one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ortho {
    /// Half a view's side, scene units.
    pub half: f32,
    /// From the frame's middle to the farthest of all that is drawn, scene units: the camera
    /// stands past it, so nothing drawn is behind it.
    pub reach: f32,
}

/// What a render's views frame, placed in the scene, and how its cameras take it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub placed: Placed,
    /// The views frame the ball the frame's box holds, not the box.
    pub ball: bool,
    /// From the camera to the frame's middle, scene units; None: what fits.
    pub distance: Option<f32>,
    /// Degrees across a view.
    pub fov: f32,
    /// Parallel projection, one scale for every view; None: perspective.
    pub ortho: Option<Ortho>,
}

impl Frame {
    /// `bounds`: what the views frame (glTF frame); `distance`: mm.
    pub fn new(bounds: [[f32; 3]; 2], ball: bool, distance: Option<f32>, fov: f32) -> Frame {
        let placed = Placed::of(bounds);
        Frame { placed, ball, distance: distance.map(|mm| mm * 0.001 * placed.scale), fov, ortho: None }
    }

    /// The frame drawn orthographic: one scale for all `views`, the largest any needs to hold
    /// the frame (or its ball), its camera past all of `shown` (glTF frame), what is drawn.
    pub fn orthographic(self, views: &[View], shown: [[f32; 3]; 2]) -> Frame {
        let half = if self.ball { 1.0 / FILL } else { views.iter().map(|v| ortho_half(self.placed.half, v.yaw, v.pitch)).fold(0.0, f32::max) };
        let corners = (0..8).map(|k: usize| self.placed.at(std::array::from_fn(|a| shown[(k >> a) & 1][a])).length());
        let reach = corners.fold(self.placed.half.length(), f32::max);
        Frame { ortho: Some(Ortho { half, reach }), ..self }
    }

    /// A render's frame: what it shows (glTF frame), its views and the camera it was asked for.
    pub fn of_render(shown: [[f32; 3]; 2], views: &[View], camera: &crate::Camera) -> Frame {
        let (bounds, ball) = focus_bounds(camera.focus, shown);
        let frame = Frame::new(bounds, ball, camera.distance.map(|d| d as f32), camera.fov.map_or(FOV, |f| f as f32));
        if camera.ortho { frame.orthographic(views, shown) } else { frame }
    }

    /// mm in one scene unit.
    pub fn mm(&self) -> f32 {
        1.0 / (self.placed.scale * 0.001)
    }

    /// A view's camera.
    pub fn look(&self, view: &View) -> Look {
        let (yaw, pitch) = (view.yaw, view.pitch);
        if let Some(ortho) = self.ortho {
            return Look { target: vec3f(0., 0., 0.), distance: ortho.reach + 0.5, fov: self.fov, yaw, pitch: pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT), ortho: Some(ortho.half) };
        }
        match self.distance {
            Some(distance) => Look { target: vec3f(0., 0., 0.), distance, fov: self.fov, yaw, pitch: pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT), ortho: None },
            // the ball the frame's box holds: its box is 2 across, so its radius is 1
            None if self.ball => fitted_ball(1.0, yaw, pitch, self.fov),
            None => fitted_look(self.placed.half, yaw, pitch, self.fov),
        }
    }

    /// A view's camera, `side` pixels square, in plate mm.
    pub fn camera(&self, view: &View, side: usize) -> ViewCamera {
        ViewCamera { look: self.look(view), placed: self.placed, side }
    }
}

/// How a render's answer says to read its cameras ([`ViewCamera::value`]).
pub const RAYS: &str = "A pixel (x y) of a view px square looks from eye along forward + (right (2x/px - 1) + up (1 - 2y/px)) tan(fov/2); orthographic, along forward from eye + (right (2x/px - 1) + up (1 - 2y/px)) across/2. Plate mm.";

/// One view of a render, as a tool that reads its picture takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewCamera {
    pub look: Look,
    pub placed: Placed,
    /// The view's side, pixels, as rendered ([`view_side`]).
    pub side: usize,
}

impl ViewCamera {
    /// The ray through a point of the view ((0, 0) its top left corner, in the pixels it was
    /// rendered at): from the eye, a unit direction, plate mm.
    pub fn ray_mm(&self, x: f32, y: f32) -> ([f64; 3], [f64; 3]) {
        let (eye, dir) = self.look.ray(self.side, x, y);
        let d = [dir.x as f64, -dir.z as f64, dir.y as f64];
        (plate_point(self.placed.back(eye)), d)
    }

    /// Where a plate point (mm) shows in the view, in its pixels, if it is in the view.
    pub fn pixel_of(&self, p: [f64; 3]) -> Option<(i32, i32)> {
        project(&self.look, self.placed.at(gltf_point(p.map(|c| c as f32))), self.side)
    }

    /// The camera in plate mm: its eye, and its forward, right and up.
    pub fn plate_axes(&self) -> [[f64; 3]; 4] {
        let (forward, right, up) = camera_axes(self.look.yaw, self.look.pitch);
        let dir = |d: Vec3f| [d.x as f64, -d.z as f64, d.y as f64];
        [plate_point(self.placed.back(self.look.eye())), dir(forward), dir(right), dir(up)]
    }

    /// How a render's answer tells this view's camera, the view `px` pixels square in its sheet
    /// (`sheet`, `cell` its column and row there): its eye and axes in plate mm, and its field of
    /// view in degrees or, orthographic, how many mm it spans; read as [`RAYS`] says.
    pub fn value(&self, name: &str, sheet: usize, cell: [usize; 2], px: u32) -> crate::out::S {
        use crate::out::{arr, int, n, obj, s, S};
        // an axis to five places: a hundred thousandth of a radian, well inside a pixel
        let axis = |v: [f64; 3]| arr(v.iter().map(|c| S::Num((c * 1e5).round() / 1e5 + 0.0)));
        let [eye, forward, right, up] = self.plate_axes();
        let mut fields = vec![("view", s(name)), ("sheet", int(sheet as i64)), ("cell", arr(cell.iter().map(|c| int(*c as i64)))), ("px", int(px as i64)), ("eye", arr(eye.iter().map(|c| n(*c)))), ("forward", axis(forward)), ("right", axis(right)), ("up", axis(up))];
        fields.push(match self.look.ortho {
            Some(half) => ("across", n((2.0 * half / (self.placed.scale * 0.001)) as f64)),
            None => ("fov", n(self.look.fov as f64)),
        });
        obj(fields)
    }

    /// Where a plate point (mm) shows in the view: [x, y] in its pixels, neither rounded nor kept
    /// to the view, and how far it is in front of the camera (mm along the view's middle ray).
    /// None: it is not in front of the camera.
    pub fn view_point(&self, p: [f64; 3]) -> Option<[f32; 3]> {
        let (x, y, depth) = self.look.view_point(self.placed.at(gltf_point(p.map(|c| c as f32))), self.side)?;
        Some([x, y, depth / self.placed.scale * 1000.0])
    }

    /// The plate point (mm) that shows at a point of the view (its pixels) `depth` mm in front of
    /// the camera ([`ViewCamera::view_point`]'s inverse).
    pub fn point_at(&self, x: f32, y: f32, depth: f64) -> [f64; 3] {
        let (eye, dir) = self.ray_mm(x, y);
        let forward = self.plate_axes()[1];
        let t = depth / (dir[0] * forward[0] + dir[1] * forward[1] + dir[2] * forward[2]);
        std::array::from_fn(|k| eye[k] + dir[k] * t)
    }

    /// How far the middle of what the view frames is in front of the camera, mm.
    pub fn middle_depth(&self) -> f64 {
        self.look.distance as f64 / self.placed.scale as f64 * 1000.0
    }

    /// The mm a pixel of the view spans `depth` mm in front of the camera (orthographic: at
    /// every depth).
    pub fn mm_per_pixel(&self, depth: f64) -> f64 {
        match self.look.ortho {
            Some(half) => 2.0 * half as f64 / self.placed.scale as f64 * 1000.0 / self.side as f64,
            None => 2.0 * ((self.look.fov as f64).to_radians() * 0.5).tan() * depth / self.side as f64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pixel's ray passes through the plate point that shows there, in every standard view.
    #[test]
    fn a_pixels_ray_passes_through_what_shows_there() {
        let shown = gltf_box([100.0, 120.0, 0.0], [160.0, 150.0, 40.0]);
        let point = [130.0, 135.0, 25.0];
        let views = default_views();
        for (frame, view) in [false, true].into_iter().flat_map(|ortho| {
            let frame = Frame::of_render(shown, &views, &crate::Camera { ortho, ..Default::default() });
            views.iter().map(move |v| (frame, v.clone()))
        }) {
            let camera = frame.camera(&view, SIDE);
            let (x, y) = camera.pixel_of(point).unwrap_or_else(|| panic!("{}: the point is out of the view", view.name));
            let (eye, dir) = camera.ray_mm(x as f32 + 0.5, y as f32 + 0.5);
            // the point's distance from the ray, against the mm a pixel spans there
            let to = [point[0] - eye[0], point[1] - eye[1], point[2] - eye[2]];
            let along = to[0] * dir[0] + to[1] * dir[1] + to[2] * dir[2];
            let off = (to.iter().map(|v| v * v).sum::<f64>() - along * along).max(0.0).sqrt();
            let pixel = match camera.look.ortho {
                Some(half) => 2.0 * (half / (camera.placed.scale * 0.001)) as f64 / SIDE as f64,
                None => 2.0 * ((camera.look.fov as f64).to_radians() * 0.5).tan() * along / SIDE as f64,
            };
            assert!(along > 0.0 && off <= pixel, "{} ({}): {off:.4} mm off the ray, a pixel {pixel:.4} mm", view.name, if frame.ortho.is_some() { "orthographic" } else { "perspective" });
        }
    }

    /// The camera a render's answer tells, read as its words say, finds the plate point that shows
    /// at a pixel of the picture as it came back (smaller than rendered), perspective or
    /// orthographic.
    #[test]
    fn the_answers_camera_finds_what_a_pixel_shows() {
        let shown = gltf_box([100.0, 120.0, 0.0], [160.0, 150.0, 40.0]);
        let point = [130.0, 135.0, 25.0];
        let views = default_views();
        let px = 300u32;
        for ortho in [false, true] {
            let frame = Frame::of_render(shown, &views, &crate::Camera { ortho, ..Default::default() });
            for (i, view) in views.iter().enumerate() {
                let camera = frame.camera(view, SIDE);
                let told = makepad_script::syntax::parse_value(&crate::out::text(&camera.value(&view.name, i / 4, [i % 2, i % 4 / 2], px))).expect("the camera parses");
                let num = |k: &str| told.field(k).and_then(|f| f.value.number()).unwrap_or_else(|| panic!("no '{k}'"));
                let v3 = |k: &str| -> [f64; 3] {
                    let items = told.field(k).and_then(|f| f.value.items()).unwrap_or_else(|| panic!("no '{k}'"));
                    std::array::from_fn(|a| items[a].number().unwrap())
                };
                assert_eq!(told.field("px").and_then(|f| f.value.number()), Some(px as f64));
                let (eye, forward, right, up) = (v3("eye"), v3("forward"), v3("right"), v3("up"));
                // where the point shows, in the picture's pixels
                let (x, y) = camera.pixel_of(point).unwrap_or_else(|| panic!("{}: the point is out of the view", view.name));
                let scale = px as f64 / SIDE as f64;
                let (u, v) = (2.0 * (x as f64 + 0.5) * scale / px as f64 - 1.0, 1.0 - 2.0 * (y as f64 + 0.5) * scale / px as f64);
                let (from, dir, pixel) = if ortho {
                    let half = num("across") / 2.0;
                    (std::array::from_fn(|a| eye[a] + (right[a] * u + up[a] * v) * half), forward, 2.0 * half / SIDE as f64)
                } else {
                    let t = (num("fov").to_radians() * 0.5).tan();
                    let d: [f64; 3] = std::array::from_fn(|a| forward[a] + (right[a] * u + up[a] * v) * t);
                    let l = d.iter().map(|c| c * c).sum::<f64>().sqrt();
                    let along = (0..3).map(|a| (point[a] - eye[a]) * forward[a]).sum::<f64>();
                    (eye, d.map(|c| c / l), 2.0 * t * along / SIDE as f64)
                };
                let to: [f64; 3] = std::array::from_fn(|a| point[a] - from[a]);
                let along = (0..3).map(|a| to[a] * dir[a]).sum::<f64>();
                let off = (to.iter().map(|c| c * c).sum::<f64>() - along * along).max(0.0).sqrt();
                assert!(along > 0.0 && off <= pixel, "{} (orthographic {ortho}): {off:.4} mm off the told ray, a rendered pixel {pixel:.4} mm", view.name);
            }
        }
    }
}

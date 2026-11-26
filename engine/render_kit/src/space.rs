//! 2.5D: layers as planes in 3D, seen through a perspective camera.
//!
//! World space is the comp's pixel grid extended into depth: x right, y
//! down, z away from the viewer, the frame at z = 0. The default camera
//! sits on the frame's centre at the distance where the z = 0 plane is 1:1
//! with the frame (`f = (h / 2) / tan(fov / 2)`), so a layer without a 3D
//! transform renders unchanged.
//!
//! A layer plane maps to the screen by a homography (a 3×3 projective
//! matrix); the shader inverts it per pixel, so perspective is exact, and
//! the same inverse yields each pixel's depth for depth of field.

use crate::catalog;
use crate::stack::ResolvedEffect;
use crate::anim::{Anim, Value};

pub type V3 = [f64; 3];
pub type M3 = [[f64; 3]; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn normalize(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l > 1e-12 { scale(a, 1.0 / l) } else { [0.0, 0.0, 1.0] }
}
pub fn mul(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}
fn matmul(a: &M3, b: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

pub fn invert(m: &M3) -> Option<M3> {
    let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let det = m[0][0] * c(1, 2, 1, 2) - m[0][1] * c(1, 2, 0, 2) + m[0][2] * c(1, 2, 0, 1);
    if !det.is_finite() || det.abs() < 1e-18 {
        return None;
    }
    let inv = [
        [c(1, 2, 1, 2), -c(0, 2, 1, 2), c(0, 1, 1, 2)],
        [-c(1, 2, 0, 2), c(0, 2, 0, 2), -c(0, 1, 0, 2)],
        [c(1, 2, 0, 1), -c(0, 2, 0, 1), c(0, 1, 0, 1)],
    ];
    Some(inv.map(|row| row.map(|v| v / det)))
}

/// Rotation about x, then y, then z (degrees): tilt, turn, roll.
pub fn rotation(deg: V3) -> M3 {
    let [x, y, z] = deg.map(f64::to_radians);
    let rx = [[1.0, 0.0, 0.0], [0.0, x.cos(), -x.sin()], [0.0, x.sin(), x.cos()]];
    let ry = [[y.cos(), 0.0, y.sin()], [0.0, 1.0, 0.0], [-y.sin(), 0.0, y.cos()]];
    let rz = [[z.cos(), -z.sin(), 0.0], [z.sin(), z.cos(), 0.0], [0.0, 0.0, 1.0]];
    matmul(&rz, &matmul(&ry, &rx))
}

/// The comp camera at one time.
#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    /// Frame size in pixels.
    pub size: [f64; 2],
    /// Focal length in pixels.
    pub focal: f64,
    pub position: V3,
    /// Rows: right, down, forward (world → camera).
    pub basis: M3,
    /// Focus distance from the camera, pixels.
    pub focus: f64,
    /// Blur diameter in pixels at infinity; 0 is no depth of field.
    pub aperture: f64,
}

/// A camera spec: [`catalog::CAMERA`]'s parameters, keyable.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraSpec {
    pub params: Vec<Anim>,
}

impl Default for CameraSpec {
    fn default() -> Self {
        Self { params: catalog::CAMERA.iter().map(|p| Anim::Const(p.kind.default_value())).collect() }
    }
}

impl CameraSpec {
    /// Named parameters, checked like an effect's.
    pub fn build<S: AsRef<str>>(params: impl IntoIterator<Item = (S, Anim)>) -> Result<(Self, Vec<String>), Vec<String>> {
        let mut spec = Self::default();
        let (mut errors, mut warnings) = (Vec::new(), Vec::new());
        for (name, anim) in params {
            let name = name.as_ref();
            match catalog::CAMERA.iter().position(|p| p.name == name) {
                Some(i) => match catalog::coerce_anim(&catalog::CAMERA[i], anim, &mut warnings) {
                    Ok(anim) => spec.params[i] = anim,
                    Err(e) => errors.push(format!("camera: {e}")),
                },
                None => {
                    let hint = catalog::closest(name, catalog::CAMERA.iter().map(|p| p.name)).map(|c| format!(" Did you mean `{c}`?")).unwrap_or_default();
                    errors.push(format!(
                        "the camera has no parameter `{name}`.{hint} Its parameters: {}",
                        catalog::CAMERA.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
        if errors.is_empty() { Ok((spec, warnings)) } else { Err(errors) }
    }

    /// The camera at `t` for a frame of `size` pixels.
    pub fn at(&self, t: f64, size: [f64; 2]) -> Camera {
        let get = |name: &str| {
            let i = catalog::CAMERA.iter().position(|p| p.name == name).unwrap();
            self.params[i].at(t)
        };
        let num = |name: &str| match get(name) { Value::Num(v) => v, _ => 0.0 };
        let v3 = |name: &str| match get(name) { Value::Vec3(v) => v, Value::Vec2(v) => [v[0], v[1], 0.0], _ => [0.0; 3] };
        let v2 = |name: &str| match get(name) { Value::Vec2(v) => v, _ => [0.0; 2] };
        let fov = num("fov").clamp(1.0, 170.0);
        let target = v3("target");
        let orbit = v2("orbit");
        Camera::orbit(size, fov, [target[0] * size[0], target[1] * size[1], target[2]], orbit, num("distance"), num("roll"), v3("offset"), num("focus"), num("aperture"))
    }
}

impl Camera {
    /// The default camera: the z = 0 plane is the frame, 1:1.
    pub fn default_for(size: [f64; 2]) -> Self {
        Self::orbit(size, 39.6, [size[0] * 0.5, size[1] * 0.5, 0.0], [0.0, 0.0], 0.0, 0.0, [0.0; 3], 0.0, 0.0)
    }

    /// A camera orbiting `target` (pixels) by yaw/pitch degrees at
    /// `distance` (0: the 1:1 distance), rolled, moved by `offset`.
    #[allow(clippy::too_many_arguments)]
    pub fn orbit(size: [f64; 2], fov_deg: f64, target: V3, orbit: [f64; 2], distance: f64, roll_deg: f64, offset: V3, focus: f64, aperture: f64) -> Self {
        let focal = (size[1] * 0.5) / (fov_deg.to_radians() * 0.5).tan();
        let d = if distance > 0.0 { distance } else { focal };
        let (yaw, pitch) = (orbit[0].to_radians(), orbit[1].clamp(-89.9, 89.9).to_radians());
        let target = add(target, offset);
        let position = add(target, [d * yaw.sin() * pitch.cos(), -d * pitch.sin(), -d * yaw.cos() * pitch.cos()]);
        let forward = normalize(sub(target, position));
        let right = normalize(cross([0.0, 1.0, 0.0], forward));
        let down = cross(forward, right);
        let (r, s) = (roll_deg.to_radians().cos(), roll_deg.to_radians().sin());
        let (right, down) = (add(scale(right, r), scale(down, s)), sub(scale(down, r), scale(right, s)));
        let focus = if focus > 0.0 { focus } else { d };
        Self { size, focal, position, basis: [right, down, forward], focus, aperture: aperture.max(0.0) }
    }

    /// World point → camera space.
    pub fn view(&self, p: V3) -> V3 {
        mul(&self.basis, sub(p, self.position))
    }

    /// World point → screen pixels and depth (None behind the camera).
    pub fn project(&self, p: V3) -> Option<([f64; 2], f64)> {
        let c = self.view(p);
        (c[2] > 1e-6).then(|| ([self.focal * c[0] / c[2] + self.size[0] * 0.5, self.focal * c[1] / c[2] + self.size[1] * 0.5], c[2]))
    }

    /// Circle of confusion (blur diameter in pixels) at depth `z`.
    pub fn coc(&self, z: f64) -> f64 {
        if self.aperture <= 0.0 || z <= 0.0 { 0.0 } else { self.aperture * (1.0 - self.focus / z).abs() }
    }
}

/// A layer plane's placement: the 3D transform effect's values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub position: V3,
    pub rotation: V3,
    pub scale: V3,
    /// Pivot, 0..1 of the frame.
    pub anchor: [f64; 2],
}

impl Plane {
    pub fn from_effect(effect: &ResolvedEffect) -> Self {
        Self { position: effect.vec3("position"), rotation: effect.vec3("rotation"), scale: effect.vec3("scale"), anchor: effect.vec2("anchor") }
    }

    pub fn identity() -> Self {
        Self { position: [0.0; 3], rotation: [0.0; 3], scale: [1.0; 3], anchor: [0.5, 0.5] }
    }

    /// World position of the layer pixel (u, v) (pixels of the frame).
    pub fn world(&self, size: [f64; 2], u: f64, v: f64) -> V3 {
        let a = [self.anchor[0] * size[0], self.anchor[1] * size[1]];
        let r = rotation(self.rotation);
        let local = [(u - a[0]) * self.scale[0], (v - a[1]) * self.scale[1], 0.0];
        add(add([a[0], a[1], 0.0], self.position), mul(&r, local))
    }
}

/// What the plane shader needs: screen → layer pixels, and depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaneProjection {
    /// Screen (x, y, 1) → (u, v, 1) / depth.
    pub inverse: M3,
    /// Depth range of the plane's four corners (in front of the camera).
    pub depth: [f64; 2],
    /// Blur diameter range over the plane.
    pub coc: [f64; 2],
}

/// The plane through the camera; None when it is edge-on or entirely
/// behind the camera.
pub fn project_plane(camera: &Camera, plane: &Plane) -> Option<PlaneProjection> {
    let size = camera.size;
    let o = camera.view(plane.world(size, 0.0, 0.0));
    let ex = sub(camera.view(plane.world(size, 1.0, 0.0)), o);
    let ey = sub(camera.view(plane.world(size, 0.0, 1.0)), o);
    let k = [[camera.focal, 0.0, size[0] * 0.5], [0.0, camera.focal, size[1] * 0.5], [0.0, 0.0, 1.0]];
    let m = [[ex[0], ey[0], o[0]], [ex[1], ey[1], o[1]], [ex[2], ey[2], o[2]]];
    let h = matmul(&k, &m);
    let inverse = invert(&h)?;
    let corners: Vec<f64> = [[0.0, 0.0], [size[0], 0.0], [0.0, size[1]], [size[0], size[1]]]
        .iter()
        .map(|c| camera.view(plane.world(size, c[0], c[1]))[2])
        .collect();
    if corners.iter().all(|z| *z <= 1e-6) {
        return None;
    }
    let front: Vec<f64> = corners.iter().copied().filter(|z| *z > 1e-6).collect();
    let (near, far) = front.iter().fold((f64::MAX, f64::MIN), |(a, b), z| (a.min(*z), b.max(*z)));
    let far = if front.len() < 4 { f64::INFINITY } else { far };
    // The CoC is monotone on each side of the focus plane.
    let cocs = [camera.coc(near), camera.coc(far.min(1e12))];
    let crosses = near < camera.focus && far > camera.focus;
    let coc = [if crosses { 0.0 } else { cocs[0].min(cocs[1]) }, cocs[0].max(cocs[1]).max(if far.is_infinite() { camera.aperture } else { 0.0 })];
    Some(PlaneProjection { inverse, depth: [near, far], coc })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    #[test]
    fn the_default_camera_is_one_to_one() {
        let size = [1920.0, 1080.0];
        let camera = Camera::default_for(size);
        let plane = Plane::identity();
        for p in [[0.0, 0.0], [1920.0, 1080.0], [300.0, 700.0]] {
            let (screen, z) = camera.project(plane.world(size, p[0], p[1])).unwrap();
            assert!(close(screen, p), "{screen:?} {p:?}");
            assert!((z - camera.focal).abs() < 1e-6);
        }
        let proj = project_plane(&camera, &plane).unwrap();
        let q = mul(&proj.inverse, [300.0, 700.0, 1.0]);
        assert!(close([q[0] / q[2], q[1] / q[2]], [300.0, 700.0]));
        assert!((1.0 / q[2] - camera.focal).abs() < 1e-6, "q.z is 1 / depth");
    }

    #[test]
    fn depth_shrinks_toward_the_centre_and_rotation_turns() {
        let size = [1000.0, 1000.0];
        let camera = Camera::default_for(size);
        let mut plane = Plane::identity();
        plane.position = [0.0, 0.0, camera.focal];
        let (screen, _) = camera.project(plane.world(size, 1000.0, 500.0)).unwrap();
        assert!(close(screen, [750.0, 500.0]), "twice as far is half as big: {screen:?}");
        let mut plane = Plane::identity();
        plane.rotation = [0.0, 0.0, 180.0];
        let (screen, _) = camera.project(plane.world(size, 0.0, 0.0)).unwrap();
        assert!(close(screen, [1000.0, 1000.0]), "{screen:?}");
        // A turn about y: the right half recedes and the left comes near.
        let mut plane = Plane::identity();
        plane.rotation = [0.0, 30.0, 0.0];
        let right = camera.view(plane.world(size, 1000.0, 500.0));
        let left = camera.view(plane.world(size, 0.0, 500.0));
        assert!(left[2] > right[2] || right[2] > left[2]);
        assert!((right[2] - left[2]).abs() > 100.0);
    }

    #[test]
    fn orbiting_keeps_the_target_centred() {
        let size = [1280.0, 720.0];
        let target = [640.0, 360.0, 0.0];
        for orbit in [[30.0, 0.0], [-45.0, 20.0], [10.0, -60.0]] {
            let camera = Camera::orbit(size, 39.6, target, orbit, 0.0, 15.0, [0.0; 3], 0.0, 0.0);
            let (screen, z) = camera.project(target).unwrap();
            assert!(close(screen, [640.0, 360.0]), "{orbit:?}: {screen:?}");
            assert!((z - camera.focal).abs() < 1e-6);
        }
    }

    #[test]
    fn circle_of_confusion() {
        let size = [1000.0, 1000.0];
        let camera = Camera::orbit(size, 39.6, [500.0, 500.0, 0.0], [0.0, 0.0], 0.0, 0.0, [0.0; 3], 0.0, 40.0);
        assert_eq!(camera.coc(camera.focus), 0.0);
        assert!((camera.coc(camera.focus * 2.0) - 20.0).abs() < 1e-9);
        assert!((camera.coc(camera.focus * 0.5) - 40.0).abs() < 1e-9);
        let mut plane = Plane::identity();
        plane.rotation = [60.0, 0.0, 0.0];
        let proj = project_plane(&camera, &plane).unwrap();
        assert_eq!(proj.coc[0], 0.0, "a tilted plane crosses the focus");
        assert!(proj.coc[1] > 5.0);
        let spec = CameraSpec::build([("aperture", Anim::from(12.0)), ("orbit", Anim::Const(Value::Vec2([20.0, 0.0])))]).unwrap().0;
        assert_eq!(spec.at(0.0, size).aperture, 12.0);
        assert!(CameraSpec::build([("fvo", Anim::from(10.0))]).unwrap_err()[0].contains("Did you mean `fov`"));
    }
}

//! Orbit-mode camera math, pure functions over [`Camera`].
//!
//! Turntable is the default: the boom is re-derived from `eye - target` on
//! every step and the world up is re-asserted, so a mixed sequence of drags
//! can never accumulate roll (the classic "my horizon is tilted" bug). The
//! trackball style is a real free rotation about the two screen axes and does
//! let the horizon tip — that is the point of it.
//!
//! Dolly is *to the cursor*: the eye moves along the line to a point on the
//! ray under the pointer, which leaves every point of that ray projecting to
//! exactly the same pixel. See `dolly_keeps_the_cursor_point_fixed` in the
//! tests. An orbit turns about a pivot it picks when it starts (`orbit_about`),
//! never about wherever a zoom left the look point.
//!
//! Nothing holds the camera above the bed: an orbit turns on under it to look up at a
//! print's underside (the view then leaves the bed out, `Camera::under_bed`).

use crate::api::*;
use makepad_widgets_core::*;

/// Just short of the pole: a camera exactly on the axis has no defined yaw.
pub const PITCH_LIMIT: f32 = 1.5533; // 89°
pub const MIN_DISTANCE: f32 = 0.02;
pub const MAX_DISTANCE: f32 = 40_000.0;
/// The height the closest perspective view shows (`MIN_DISTANCE` in the starting 40 degree
/// field): a toggle there keeps the size both ways.
pub const MIN_ORTHO_HEIGHT: f32 = 0.01456;
pub const MAX_ORTHO_HEIGHT: f32 = 80_000.0;
/// Radians of orbit per layout point of pointer travel (Fab's rate).
pub const ORBIT_SENS: f32 = 0.0075;

pub const WORLD_UP: Vec3f = Vec3f {
    x: 0.0,
    y: 0.0,
    z: 1.0,
};

/// Rodrigues rotation of `v` about `axis` by `angle` radians.
pub fn rotate_about(v: Vec3f, axis: Vec3f, angle: f32) -> Vec3f {
    let a = axis.normalize();
    if !a.is_finite() {
        return v;
    }
    let s = angle.sin();
    let c = angle.cos();
    v * c + Vec3f::cross(a, v) * s + a * (a.dot(v) * (1.0 - c))
}

/// Some unit vector perpendicular to `v`.
fn any_perpendicular(v: Vec3f) -> Vec3f {
    let a = if v.x.abs() < 0.9 {
        vec3(1.0, 0.0, 0.0)
    } else {
        vec3(0.0, 1.0, 0.0)
    };
    Vec3f::cross(v, a).normalize()
}

/// `v` in the camera's turntable frame: the world's, or the world turned over about X while the
/// camera is hung (its own inverse), so the turntable reads the same both ways up.
fn framed(cam: &Camera, v: Vec3f) -> Vec3f {
    if cam.hung {
        vec3(v.x, -v.y, -v.z)
    } else {
        v
    }
}

/// Shortest-arc interpolation between two unit vectors.
pub fn slerp(a: Vec3f, b: Vec3f, f: f32) -> Vec3f {
    let a = a.normalize();
    let b = b.normalize();
    if !a.is_finite() || !b.is_finite() {
        return b;
    }
    let d = a.dot(b).clamp(-1.0, 1.0);
    if d > 0.9995 {
        return Vec3f::from_lerp(a, b, f).normalize();
    }
    if d < -0.9995 {
        // Antipodal: no shortest arc exists, take a fixed half-turn plane.
        return rotate_about(a, any_perpendicular(a), std::f32::consts::PI * f);
    }
    let theta = d.acos();
    let st = theta.sin();
    a * (((1.0 - f) * theta).sin() / st) + b * ((f * theta).sin() / st)
}

/// The turntable angles the camera is currently at, `(yaw, pitch)` radians, in its frame
/// ([`framed`]). At the poles the yaw is read back out of the up vector, so entering the
/// Top / Bottom preset and then orbiting continues from the right azimuth
/// instead of snapping to yaw 0.
pub fn turntable_angles(cam: &Camera) -> (f32, f32) {
    let offset = framed(cam, cam.eye - cam.target);
    let dist = offset.length().max(1e-5);
    let sin_pitch = (offset.z / dist).clamp(-1.0, 1.0);
    let pitch = sin_pitch.asin();
    let horiz = (offset.x * offset.x + offset.y * offset.y).sqrt();
    let yaw = if horiz > dist * 1e-3 {
        offset.y.atan2(offset.x)
    } else {
        let u = framed(cam, cam.true_up());
        let s = if sin_pitch >= 0.0 { -1.0 } else { 1.0 };
        if u.is_finite() && (u.x.abs() + u.y.abs()) > 1e-5 {
            (s * u.y).atan2(s * u.x)
        } else {
            0.0
        }
    };
    (yaw, pitch)
}

/// Place the eye from turntable angles, world up re-asserted.
pub fn set_turntable(cam: &mut Camera, yaw: f32, pitch: f32, dist: f32) {
    let pitch = pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
    let dist = dist.clamp(MIN_DISTANCE, MAX_DISTANCE);
    let cp = pitch.cos();
    cam.eye = cam.target + framed(cam, vec3(cp * yaw.cos(), cp * yaw.sin(), pitch.sin())) * dist;
    cam.up = cam.world_up();
}

/// Turntable orbit by a screen delta in layout points. Up-locked: the world
/// up is re-asserted every step, so roll can never accumulate.
pub fn orbit_turntable(cam: &mut Camera, dx: f32, dy: f32) {
    let (yaw, pitch) = turntable_angles(cam);
    let dist = cam.distance();
    set_turntable(cam, yaw - dx * ORBIT_SENS, pitch + dy * ORBIT_SENS, dist);
}

/// Free trackball orbit about the two screen axes (roll is allowed).
pub fn orbit_trackball(cam: &mut Camera, dx: f32, dy: f32) {
    let right = cam.right();
    let up = cam.true_up();
    if !right.is_finite() || !up.is_finite() {
        orbit_turntable(cam, dx, dy);
        return;
    }
    let mut offset = cam.eye - cam.target;
    let mut new_up = cam.up;
    let yaw = -dx * ORBIT_SENS;
    let pitch = -dy * ORBIT_SENS;
    offset = rotate_about(offset, up, yaw);
    offset = rotate_about(offset, right, pitch);
    new_up = rotate_about(new_up, up, yaw);
    new_up = rotate_about(new_up, right, pitch);
    cam.eye = cam.target + offset;
    let n = new_up.normalize();
    if n.is_finite() {
        cam.up = n;
    }
}

/// Screen-plane pan: drag the world with the pointer, 1:1 at the pivot depth.
pub fn pan(cam: &mut Camera, dx: f32, dy: f32, rect_h: f32) {
    let world_per_point = if cam.ortho {
        cam.ortho_height / rect_h.max(1.0)
    } else {
        2.0 * cam.distance() * (cam.fov_y_deg.to_radians() * 0.5).tan() / rect_h.max(1.0)
    };
    let shift = cam.right() * (-dx * world_per_point) + cam.true_up() * (dy * world_per_point);
    if !shift.is_finite() {
        return;
    }
    cam.eye += shift;
    cam.target += shift;
}

/// The point where a ray crosses the plane through the pivot facing the
/// camera — the fallback dolly anchor when nothing was picked.
pub fn anchor_on_pivot_plane(cam: &Camera, ray: &Ray) -> Vec3f {
    let n = cam.forward();
    let denom = n.dot(ray.dir);
    if denom.abs() < 1e-6 {
        return cam.target;
    }
    let t = (cam.target - ray.origin).dot(n) / denom;
    if !t.is_finite() || t <= 0.0 {
        return cam.target;
    }
    ray.at(t)
}

/// Dolly by `factor` (< 1 moves in) about `anchor`.
///
/// Perspective: the eye moves along the line through the anchor and the
/// orientation never changes, so every world point on the cursor ray keeps its
/// exact pixel; the look point follows at
/// the anchor's depth (pan rates read it). Nothing here picks an orbit pivot:
/// an orbit picks its own when it starts. Orthographic: the height scales and
/// the camera slides sideways by the matching amount.
pub fn dolly(cam: &mut Camera, factor: f32, anchor: Option<Vec3f>) {
    if !factor.is_finite() || factor <= 0.0 {
        return;
    }
    if cam.ortho {
        let h = cam.ortho_height.max(1e-4);
        let f = factor.clamp(MIN_ORTHO_HEIGHT / h, MAX_ORTHO_HEIGHT / h);
        cam.ortho_height = h * f;
        if let Some(a) = anchor {
            let fwd = cam.forward();
            let v = cam.eye - a;
            let lateral = v - fwd * v.dot(fwd);
            let shift = lateral * (f - 1.0);
            if shift.is_finite() {
                cam.eye += shift;
                cam.target += shift;
            }
        }
    } else {
        let a = anchor.unwrap_or(cam.target);
        let dist = (cam.eye - a).length().max(1e-5);
        let f = factor.clamp(MIN_DISTANCE / dist, MAX_DISTANCE / dist);
        let eye = a + (cam.eye - a) * f;
        let fwd = cam.forward();
        let target = eye + fwd * (a - eye).dot(fwd).max(MIN_DISTANCE);
        if eye.is_finite() && target.is_finite() {
            cam.eye = eye;
            cam.target = target;
        }
    }
}

/// Orbit by a screen delta about `pivot`, a world point that need not be on the view axis:
/// eye and look point turn rigidly about it, so the pivot keeps its pixel while everything
/// turns around it (CAD-style). Turntable: yaw about
/// the world up through the pivot, pitch about the camera's horizontal right axis,
/// the horizon kept level and the pitch short of the poles. Trackball: the same two
/// rotations about the screen axes, roll allowed.
pub fn orbit_about(cam: &mut Camera, pivot: Vec3f, style: OrbitStyle, dx: f32, dy: f32) {
    if !pivot.is_finite() {
        return;
    }
    let turn = |c: &mut Camera, axis: Vec3f, angle: f32| {
        c.eye = pivot + rotate_about(c.eye - pivot, axis, angle);
        c.target = pivot + rotate_about(c.target - pivot, axis, angle);
        c.up = rotate_about(c.up, axis, angle);
    };
    let mut c = *cam;
    match style {
        OrbitStyle::Turntable => {
            let up = c.world_up();
            turn(&mut c, up, -dx * ORBIT_SENS);
            // Pitch of the boom (eye - look point), kept short of the poles like `set_turntable`.
            let offset = c.eye - c.target;
            let p0 = (offset.dot(up) / offset.length().max(1e-6)).clamp(-1.0, 1.0).asin();
            let p1 = (p0 + dy * ORBIT_SENS).clamp(-PITCH_LIMIT, PITCH_LIMIT);
            let right = c.right();
            if right.is_finite() {
                turn(&mut c, right, -(p1 - p0));
            }
            c.up = up;
        }
        OrbitStyle::Trackball => {
            let (right, up) = (c.right(), c.true_up());
            if !right.is_finite() || !up.is_finite() {
                return;
            }
            turn(&mut c, up, -dx * ORBIT_SENS);
            turn(&mut c, right, -dy * ORBIT_SENS);
            let n = c.up.normalize();
            if n.is_finite() {
                c.up = n;
            }
        }
    }
    if c.eye.is_finite() && c.target.is_finite() && (c.eye - c.target).length() > 1e-6 {
        *cam = c;
    }
}

/// Move the pivot to `point` without turning: the whole camera translates, so
/// the framing slides but the direction and distance survive. This is what a
/// double-click on a surface does.
pub fn recenter(cam: &mut Camera, point: Vec3f) {
    let shift = point - cam.target;
    if !shift.is_finite() {
        return;
    }
    cam.eye += shift;
    cam.target = point;
}

/// The distance an orthographic view's height is seen at in perspective (its field of view):
/// where the eye goes back to, and how far the view's grid is seen from.
pub fn seen_from(cam: &Camera) -> f32 {
    let half_fov = (cam.fov_y_deg.to_radians() * 0.5).max(1e-4);
    (cam.ortho_height * 0.5 / half_fov.tan()).clamp(MIN_DISTANCE, MAX_DISTANCE)
}

/// Switch projection while keeping the apparent size of whatever is at the
/// pivot. Instant on purpose — a projection cannot be blended. There and back
/// is the view it was: the height is the one [`seen_from`] reads back as the
/// distance, within the same limits.
pub fn set_ortho(cam: &mut Camera, ortho: bool) {
    if cam.ortho == ortho {
        return;
    }
    let half_fov = (cam.fov_y_deg.to_radians() * 0.5).max(1e-4);
    if ortho {
        let per = 2.0 * half_fov.tan();
        cam.ortho_height = cam.distance().clamp(MIN_DISTANCE, MAX_DISTANCE) * per;
    } else {
        let d = seen_from(cam);
        let dir = cam.forward();
        if dir.is_finite() {
            cam.eye = cam.target - dir * d;
        }
    }
    cam.ortho = ortho;
}

/// The camera a preset view wants, keeping the current pivot and distance.
/// Axis-aligned presets go orthographic, matching Fab's auto-perspective. A hung camera looks
/// the same way turned over.
pub fn preset_camera(cam: &Camera, preset: PresetView, auto_ortho: bool) -> Camera {
    let mut to = *cam;
    let (dir, up) = preset.look_dir_and_up();
    let dist = cam.distance().clamp(MIN_DISTANCE, MAX_DISTANCE);
    to.eye = cam.target - dir * dist;
    to.up = if cam.hung { -up } else { up };
    if auto_ortho && preset != PresetView::Isometric {
        set_ortho(&mut to, true);
    }
    to
}

// ===========================================================================
// Eased transitions
// ===========================================================================

/// Smoothstep — an ease in and out with zero slope at both ends and no
/// overshoot. Fab's "smooth view", and the only easing this app uses.
pub fn ease_in_out(f: f32) -> f32 {
    let f = f.clamp(0.0, 1.0);
    f * f * (3.0 - 2.0 * f)
}

/// Blend two cameras: the pivot travels straight, the boom slerps and its
/// length interpolates geometrically, so a 180° flip sweeps instead of
/// passing through the model.
pub fn lerp_camera(a: &Camera, b: &Camera, f: f32) -> Camera {
    let mut c = *b;
    let da = a.distance().max(1e-4);
    let db = b.distance().max(1e-4);
    let dir_a = (a.eye - a.target) * (1.0 / da);
    let dir_b = (b.eye - b.target) * (1.0 / db);
    let dir = slerp(dir_a, dir_b, f);
    let dist = (da.ln() + (db.ln() - da.ln()) * f).exp();
    c.target = Vec3f::from_lerp(a.target, b.target, f);
    c.eye = c.target + dir * dist;
    // Turning over (the up hints opposite) rolls about the line of sight: about the look
    // direction's part across the up hint, so the hint never lies along it.
    let up = if a.up.normalize().dot(b.up.normalize()) < -0.9995 {
        let fwd = -dir;
        let u = a.up.normalize();
        let across = (fwd - u * fwd.dot(u)).normalize();
        let axis = if across.is_finite() { across } else { any_perpendicular(u) };
        rotate_about(u, axis, std::f32::consts::PI * f)
    } else {
        slerp(a.up, b.up, f)
    };
    if up.is_finite() {
        c.up = up;
    }
    c.fov_y_deg = a.fov_y_deg + (b.fov_y_deg - a.fov_y_deg) * f;
    let ha = a.ortho_height.max(1e-4);
    let hb = b.ortho_height.max(1e-4);
    c.ortho_height = (ha.ln() + (hb.ln() - ha.ln()) * f).exp();
    c
}

/// One camera move in flight. Ends *exactly* on `to` so a preset view lands
/// on the nose rather than a smoothstep's worth of epsilon away.
#[derive(Clone, Copy, Debug)]
pub struct CameraAnim {
    pub from: Camera,
    pub to: Camera,
    pub t: f32,
    pub dur: f32,
    /// Re-applied after every step, because `mark_camera_changed` clears it.
    pub preset: Option<PresetView>,
}

impl CameraAnim {
    pub fn new(from: Camera, to: Camera, dur: f32, preset: Option<PresetView>) -> Self {
        let mut from = from;
        // A projection cannot be blended: enter the destination's projection
        // up front with a height that matches the current apparent size, so
        // the move is one continuous glide instead of a glide plus a pop.
        if to.ortho && !from.ortho {
            set_ortho(&mut from, true);
        } else if !to.ortho && from.ortho {
            set_ortho(&mut from, false);
        }
        CameraAnim {
            from,
            to,
            t: 0.0,
            dur: dur.max(0.0),
            preset,
        }
    }

    /// Advance by `dt`; returns the camera for this frame and whether it is
    /// the last one.
    pub fn step(&mut self, dt: f32) -> (Camera, bool) {
        self.t = (self.t + dt.max(0.0)).min(self.dur);
        if self.dur <= 1e-4 || self.t >= self.dur {
            return (self.to, true);
        }
        let f = ease_in_out(self.t / self.dur);
        (lerp_camera(&self.from, &self.to, f), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oblique_cam() -> Camera {
        let mut c = Camera::default();
        c.target = vec3(5.0, 3.5, 2.5);
        c.eye = vec3(18.0, -20.0, 12.0);
        c.up = WORLD_UP;
        c
    }

    /// The camera's pitch over the bed plane, in its turntable frame (negative: under it).
    fn pitch(cam: &Camera) -> f32 {
        turntable_angles(cam).1
    }

    /// A drag down turns the camera on past horizontal and under the bed, to looking straight
    /// up at the print's underside: the orbit's own pitch cap, short of the pole, is the only
    /// limit, in both projections and with every orbit (the pivot's, the boom's, a trackball's).
    #[test]
    fn an_orbit_past_horizontal_reaches_a_below_bed_pitch() {
        for ortho in [false, true] {
            for style in [None, Some(OrbitStyle::Turntable), Some(OrbitStyle::Trackball)] {
                let mut cam = Camera::default();
                cam.ortho = ortho;
                let pivot = cam.target;
                let dist = cam.distance();
                assert!(pitch(&cam) > 0.3 && !cam.under_bed(), "{cam:?}");
                let step = -20.0;
                let mut steps = 0;
                // On until the turntable's cap (a trackball has none: it turns over the pole).
                while pitch(&cam) > -PITCH_LIMIT + 1e-3 && (style != Some(OrbitStyle::Trackball) || pitch(&cam) > -1.4) {
                    let before = pitch(&cam);
                    match style {
                        None => orbit_turntable(&mut cam, 0.0, step),
                        Some(s) => orbit_about(&mut cam, pivot, s, 0.0, step),
                    }
                    let after = pitch(&cam);
                    // every step turns by the drag's whole angle until the cap: nothing stops it at the bed
                    let turned = (before - after).abs();
                    assert!(after < before && (turned - (-step * ORBIT_SENS)).abs() < 1e-3 || after <= -PITCH_LIMIT + 1e-3, "{style:?} ortho {ortho}: {before} -> {after}");
                    assert!((cam.distance() - dist).abs() < 1e-4 && (cam.target - pivot).length() < 1e-4, "{cam:?}");
                    assert_eq!(cam.under_bed(), cam.eye.z < 0.0, "{cam:?}");
                    steps += 1;
                    assert!(steps < 100, "{style:?}: the pitch never got there: {cam:?}");
                }
                assert!(cam.eye.z < -0.5 * dist && cam.under_bed(), "{style:?} ortho {ortho}: {cam:?}");
                assert!(cam.forward().z > 0.98, "looking up at the underside: {:?}", cam.forward());
                // and back up over the bed the same way
                for _ in 0..steps {
                    match style {
                        None => orbit_turntable(&mut cam, 0.0, -step),
                        Some(s) => orbit_about(&mut cam, pivot, s, 0.0, -step),
                    }
                }
                assert!(!cam.under_bed() && pitch(&cam) > 0.3, "{style:?} ortho {ortho}: {cam:?}");
            }
        }
    }

    /// The bed plane's side a camera sees: an eye under it, or an orthographic view looking up.
    #[test]
    fn under_bed_is_the_side_of_the_bed_plane_the_view_sees() {
        let mut cam = Camera::default();
        assert!(!cam.under_bed());
        cam.eye.z = -0.001;
        assert!(cam.under_bed());
        // looking down from under the plane, a perspective's upper rays still meet it from below
        cam.target.z = -0.3;
        assert!(cam.under_bed());
        // an orthographic view's rays are all parallel: it is its look that counts
        cam.ortho = true;
        assert!(!cam.under_bed());
        cam.target.z = 0.3;
        assert!(cam.under_bed());
        // Top, a side and Bottom
        for (preset, under) in [(PresetView::Top, false), (PresetView::Front, false), (PresetView::Bottom, true)] {
            for auto_ortho in [false, true] {
                assert_eq!(preset_camera(&Camera::default(), preset, auto_ortho).under_bed(), under, "{preset:?}");
            }
        }
    }

    /// A pan, a dolly to a point under the bed and a recentre move the camera there unchanged.
    #[test]
    fn pan_dolly_and_recenter_cross_the_bed_plane_unchanged() {
        for ortho in [false, true] {
            let mut cam = Camera::default();
            cam.ortho = ortho;
            let (offset, up) = (cam.eye - cam.target, cam.up);
            pan(&mut cam, 3.0, -10_000.0, 800.0);
            assert!(cam.target.z < -1.0 && (cam.eye - cam.target - offset).length() < 1e-4 && cam.up == up, "{cam:?}");
        }
        let mut cam = Camera::default();
        cam.target = vec3(0.0, 0.0, 0.5);
        cam.eye = vec3(0.3, -0.4, 0.1);
        let fwd = cam.forward();
        dolly(&mut cam, 0.1, Some(vec3(0.0, 0.0, -2.0)));
        assert!(cam.eye.z < 0.0 && cam.forward().dot(fwd) > 0.99999, "{cam:?}");
        let mut cam = Camera::default();
        let offset = cam.eye - cam.target;
        recenter(&mut cam, vec3(0.1, -0.2, -1.0));
        assert_eq!(cam.target, vec3(0.1, -0.2, -1.0));
        assert!((cam.eye - cam.target - offset).length() < 1e-6);
    }

    #[test]
    fn turntable_never_accumulates_roll() {
        let mut cam = oblique_cam();
        // Twenty mixed drags, deliberately including near-polar excursions.
        let drags = [
            (37.0, -12.0),
            (-90.0, 40.0),
            (5.0, 300.0),
            (250.0, -400.0),
            (-3.0, 3.0),
            (120.0, 120.0),
            (-260.0, -70.0),
            (11.0, -900.0),
            (0.0, 900.0),
            (-500.0, 0.0),
        ];
        for round in 0..2 {
            for (dx, dy) in drags {
                orbit_turntable(&mut cam, dx * (1.0 - round as f32 * 2.0), dy);
                assert!(cam.eye.is_finite(), "eye went non-finite");
                // Up stays world up and the horizon stays level: the camera
                // right axis has no vertical component at all.
                assert!(
                    cam.right().z.abs() < 1e-5,
                    "roll crept in: right = {:?}",
                    cam.right()
                );
            }
        }
        assert!((cam.up - WORLD_UP).length() < 1e-6);
    }

    #[test]
    fn turntable_survives_the_poles() {
        // Straight down with up = +Y. Orbiting must continue from that
        // azimuth, not snap the model a quarter turn.
        let mut cam = preset_camera(&oblique_cam(), PresetView::Top, false);
        let before = cam.forward();
        orbit_turntable(&mut cam, 0.0, -1.0);
        let after = cam.forward();
        assert!(after.is_finite());
        assert!(
            after.dot(before) > 0.999,
            "the top view jumped: {before:?} -> {after:?}"
        );
        assert!(cam.right().z.abs() < 1e-5);
    }

    #[test]
    fn dolly_keeps_the_cursor_point_fixed() {
        let rect = Rect {
            pos: dvec2(0.0, 0.0),
            size: dvec2(1600.0, 1000.0),
        };
        for ortho in [false, true] {
            for (cx, cy) in [
                (200.0, 150.0),
                (1400.0, 880.0),
                (800.0, 500.0),
                (60.0, 940.0),
            ] {
                for factor in [0.5f32, 0.8, 1.25, 2.0] {
                    let mut cam = oblique_cam();
                    if ortho {
                        set_ortho(&mut cam, true);
                    }
                    let proj = ViewProjector::new(cam, rect);
                    let ray = proj.ray(dvec2(cx, cy));
                    let anchor = anchor_on_pivot_plane(&cam, &ray);
                    let before = proj.project(anchor).expect("anchor is in front");
                    dolly(&mut cam, factor, Some(anchor));
                    let after = ViewProjector::new(cam, rect)
                        .project(anchor)
                        .expect("anchor stays in front");
                    let err = (after - before).length();
                    assert!(
                        err < 2.0,
                        "cursor point moved {err:.3} px (ortho={ortho}, f={factor})"
                    );
                }
            }
        }
    }

    #[test]
    fn orbit_about_keeps_the_pivot_on_its_pixel() {
        let rect = Rect { pos: dvec2(0.0, 0.0), size: dvec2(1600.0, 1000.0) };
        for style in [OrbitStyle::Turntable, OrbitStyle::Trackball] {
            // A pivot well off the view axis: a part near the edge of the picture.
            let mut cam = oblique_cam();
            let pivot = cam.target + cam.right() * 4.0 + cam.true_up() * 1.5;
            let before = ViewProjector::new(cam, rect).project(pivot).expect("pivot in view");
            for (dx, dy) in [(60.0, 0.0), (0.0, 40.0), (-120.0, -35.0), (300.0, 80.0)] {
                orbit_about(&mut cam, pivot, style, dx, dy);
                let after = ViewProjector::new(cam, rect).project(pivot).expect("pivot stays in view");
                assert!((after - before).length() < 0.5, "{style:?} ({dx}, {dy}): pivot moved {before:?} -> {after:?}");
                if style == OrbitStyle::Turntable {
                    assert!(cam.right().z.abs() < 1e-5, "the horizon tilted");
                }
            }
        }
    }

    /// A toggle keeps what is at the pivot its size on screen, within a pixel, both ways: from
    /// afar and from the closest the view goes, with the frame's fit between (the orthographic
    /// eye standing back behind the scene round it), and after a zoom in orthographic.
    #[test]
    fn ortho_toggle_keeps_apparent_size() {
        let rect = Rect {
            pos: dvec2(0.0, 0.0),
            size: dvec2(1600.0, 1000.0),
        };
        let aspect = 1.6;
        let size = |cam: &Camera, a: Vec3f, b: Vec3f| {
            let p = ViewProjector::new(*cam, rect);
            (p.project(b).expect("in view") - p.project(a).expect("in view")).length()
        };
        for close in [false, true] {
            let mut cam = oblique_cam();
            if close {
                cam.eye = cam.target + (cam.eye - cam.target).normalize() * MIN_DISTANCE;
            }
            // a scene round the pivot, the close eye well inside its box; each projection's
            // planes fitted to it as a frame fits them
            let scene = Aabb { min: cam.target - vec3(2.0, 2.0, 2.0), max: cam.target + vec3(2.0, 2.0, 2.0) };
            let toggle = |cam: &mut Camera, ortho: bool| {
                set_ortho(cam, ortho);
                cam.fit_clip_planes(&scene, aspect);
            };
            cam.fit_clip_planes(&scene, aspect);
            // a bar across the view at the pivot, half the view's width
            let half = cam.right() * (cam.distance() * 0.36);
            let (a, b) = (cam.target - half, cam.target + half);
            let before = size(&cam, a, b);
            toggle(&mut cam, true);
            assert!(!close || cam.distance() > 2.0, "the close eye stands back behind the scene: {cam:?}");
            let ortho = size(&cam, a, b);
            assert!((ortho - before).abs() < 1.0, "close {close}: {before:.2} px -> {ortho:.2} px");
            toggle(&mut cam, false);
            let after = size(&cam, a, b);
            assert!((after - before).abs() < 1.0, "close {close}: {before:.2} px -> {after:.2} px");
            toggle(&mut cam, true);
            dolly(&mut cam, if close { 1.5 } else { 0.5 }, None);
            cam.fit_clip_planes(&scene, aspect);
            let zoomed = size(&cam, a, b);
            assert!((zoomed - before).abs() > 100.0, "close {close}: the zoom changed nothing");
            toggle(&mut cam, false);
            let back = size(&cam, a, b);
            assert!((back - zoomed).abs() < 1.0, "close {close}, zoomed: {zoomed:.2} px -> {back:.2} px");
        }
    }

    #[test]
    fn presets_land_exactly() {
        let cam = oblique_cam();
        for preset in [
            PresetView::Front,
            PresetView::Back,
            PresetView::Left,
            PresetView::Right,
            PresetView::Top,
            PresetView::Bottom,
            PresetView::Isometric,
        ] {
            let to = preset_camera(&cam, preset, true);
            let (dir, _) = preset.look_dir_and_up();
            assert!(
                to.forward().dot(dir) > 0.99999,
                "{preset:?}: {:?} vs {dir:?}",
                to.forward()
            );
            assert!((to.distance() - cam.distance()).abs() < 1e-3);
            assert_eq!(to.target, cam.target);
            if preset == PresetView::Bottom {
                assert!(to.under_bed(), "Bottom looks up from under the bed");
            }
            // And the animation to it lands on that camera, not near it.
            let mut anim = CameraAnim::new(cam, to, 0.25, Some(preset));
            let mut steps = 0;
            loop {
                let (c, done) = anim.step(1.0 / 60.0);
                steps += 1;
                assert!(steps < 200, "animation never finished");
                if done {
                    assert_eq!(c.eye, to.eye);
                    assert_eq!(c.target, to.target);
                    assert_eq!(c.up, to.up);
                    assert_eq!(c.ortho, to.ortho);
                    break;
                }
                assert!(c.eye.is_finite(), "mid-flight eye went bad");
            }
        }
    }

    #[test]
    fn animation_projection_changes_do_not_clamp_bottom() {
        for ortho in [false, true] {
            let mut from = preset_camera(&Camera::default(), PresetView::Bottom, false);
            from.ortho = ortho;
            let mut to = Camera::default();
            to.ortho = !ortho;
            let mut anim = CameraAnim::new(from, to, 0.25, None);
            assert_eq!(anim.from.target, from.target);
            assert!(anim.from.forward().dot(from.forward()) > 0.99999);
            assert!(anim.from.eye.z < 0.0);
            let (first, done) = anim.step(0.0);
            assert!(!done);
            assert!(first.eye.z < 0.0);
        }
    }

    #[test]
    fn recenter_keeps_direction_and_distance() {
        let mut cam = oblique_cam();
        let dir = cam.forward();
        let dist = cam.distance();
        recenter(&mut cam, vec3(-2.0, 9.0, 1.0));
        assert!(cam.forward().dot(dir) > 0.99999);
        assert!((cam.distance() - dist).abs() < 1e-4);
        assert!((cam.target - vec3(-2.0, 9.0, 1.0)).length() < 1e-5);
    }

    /// The world turned over about X, as a hung camera's frame is.
    fn over(v: Vec3f) -> Vec3f {
        vec3(v.x, -v.y, -v.z)
    }

    #[test]
    fn a_hung_turntable_is_the_upright_one_turned_over() {
        let upright = oblique_cam();
        let mut hung = upright;
        hung.hung = true;
        (hung.eye, hung.target, hung.up) = (over(upright.eye), over(upright.target), over(upright.up));
        let (a, b) = (turntable_angles(&upright), turntable_angles(&hung));
        assert!((a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5, "{a:?} {b:?}");
        let (mut u, mut h) = (upright, hung);
        set_turntable(&mut u, 0.7, 0.4, 9.0);
        set_turntable(&mut h, 0.7, 0.4, 9.0);
        assert!((over(u.eye) - h.eye).length() < 1e-4, "{u:?} {h:?}");
        assert_eq!(h.up, -WORLD_UP);
    }

    #[test]
    fn a_hung_orbit_stays_turned_over() {
        let mut cam = oblique_cam();
        cam.hung = true;
        cam.up = -WORLD_UP;
        for (dx, dy) in [(40.0, 0.0), (0.0, 80.0), (-120.0, -300.0), (10.0, 400.0)] {
            orbit_about(&mut cam, vec3(5.0, 3.5, 2.5), OrbitStyle::Turntable, dx, dy);
            assert_eq!(cam.up, -WORLD_UP);
            assert!(cam.eye.is_finite() && cam.target.is_finite(), "{cam:?}");
            orbit_turntable(&mut cam, dx, dy);
            assert_eq!(cam.up, -WORLD_UP);
            assert!(cam.eye.is_finite() && cam.target.is_finite(), "{cam:?}");
        }
    }

    #[test]
    fn turning_over_rolls_about_the_line_of_sight() {
        for eye in [vec3(0.3, 0.0, 0.1), vec3(0.0, -0.3, 0.1), vec3(0.2, 0.2, 0.3)] {
            let mut a = Camera::default();
            (a.eye, a.target, a.up) = (eye, vec3(0.0, 0.0, 0.02), WORLD_UP);
            let mut b = a;
            (b.hung, b.up) = (true, -WORLD_UP);
            for i in 0..=20 {
                let c = lerp_camera(&a, &b, i as f32 / 20.0);
                assert!((c.eye - a.eye).length() < 1e-5, "the eye stays: {c:?}");
                assert!(c.right().is_finite() && c.up.normalize().dot(c.forward()).abs() < 0.999, "the up hint never lies along the view: {c:?}");
            }
            assert!((lerp_camera(&a, &b, 1.0).up - b.up).length() < 1e-5);
        }
    }
}

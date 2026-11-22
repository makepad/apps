//! Mechanics in the headset: the hands on the live sim. A controller's trigger or grip, or a hand's pinch
//! acts on the body its tip is on or near (a grip may also take the one it aims at). Grab and Throw hold it
//! firmly, Dangle holds it softly, and Pin/Poke act once on press. Only Throw sets release velocity.
//!
//! The sim answers a drag a headset frame later (it shows at the next frame): so the drag target
//! leads the tip by its velocity times that ([`XrMech::step`]'s `lead`), and the held body is
//! drawn where the hand is, as the sim has it, its contacts its own. Plain state stepped
//! once per headset frame before the navigation ([`XrMech::step`]): a hand on a body is masked from
//! the navigation. The sim is the app's (`crate::mechanics`): this says what to send it, as the
//! desktop's grab tool does.
//!
//! A Supports sim's part isn't pulled, it is carried ([`MechOut::carry`]): the hand that takes it holds
//! it rigidly, and the other hand takes a piece out of it. Each of the two holds goes on by itself. Within
//! a part's box, a free hand takes the part; the hand after it, the piece.
//!
//! Per frame: the held point's move, and one box test per body for each free tip (what it touches,
//! and takes when its grab goes down).

use fab::model::ElementId;
use fab::{AppState, MechTool, VelocityTrack};
use makepad_widgets::makepad_platform::event::xr::{XrController, XrHand, XrState};
use makepad_widgets::*;
use makepad_xr_nav::XrNavView;
use makepad_xr_ui::haptic::{self, Haptics};
use makepad_xr_ui::input::{aim, grab, ray, tip, Buttons, PRESS};
use makepad_xr_ui::MenuStep;

use crate::xr_edit::{inverse, mul, pose_affine, printer_from_room, room_from_printer, Affine};

/// How near the hand's tip must be to a body to take it (room metres).
const REACH_M: f32 = 0.04;
/// The longest aim that takes a body (room metres).
const RAY_M: f32 = 4.0;
#[derive(Clone, Copy, Debug)]
struct Hold {
    hand: usize,
    tool: MechTool,
    /// The held point in the hand's frame (room metres).
    local: Vec3f,
    /// Where it was last frame (room metres).
    last: Vec3f,
}

/// A hand on a part.
#[derive(Clone, Copy, Debug)]
struct Carry {
    hand: usize,
    part: ElementId,
    /// Printer mm -> the hand's frame, as the hand was when it took the part.
    from_hand: Affine,
    /// The part as it was then: rest -> there (printer mm).
    from_part: Affine,
    /// The point taken, in the hand's frame (room metres).
    local: Vec3f,
}

/// The frame's answer, reused frame to frame.
#[derive(Debug, Default)]
pub struct MechOut {
    /// Take this body by this point (printer frame, metres).
    pub grab: Option<(ElementId, [f64; 3])>,
    pub dangle: bool,
    pub pin: Option<(ElementId, [f64; 3])>,
    pub poke: Option<(ElementId, [f64; 3], [f64; 3])>,
    /// Pull the held point here (printer frame, metres).
    pub drag: Option<[f64; 3]>,
    pub release: bool,
    /// The release's throw: the held point's velocity (printer frame, m/s).
    pub throw: Option<[f64; 3]>,
    /// A hand has just taken a part; `carry` is where it took it.
    pub carry_taken: bool,
    /// Where the hand has the part: rest -> now (printer frame, metres).
    pub carry: Option<Affine>,
    /// Hands the navigation must not see.
    pub consumed: [bool; 2],
    pub haptics: Haptics,
    /// The held point in the room.
    pub at: Option<Vec3f>,
    /// The body each free hand's tip is on (it would take it), or holds.
    pub touch: [Option<ElementId>; 2],
    /// Actual body contact points, in room metres, for the shared grab markers.
    pub touch_at: [Option<Vec3f>; 2],
}

/// Which body a hand takes: its element and the point (printer frame, metres). `parts`: whether
/// what a hand carries counts (not with a hand on it: the other hand is after a piece).
pub trait BodyPick {
    fn near(&self, view: &XrNavView, hand: Vec3f, parts: bool) -> Option<(ElementId, [f64; 3])>;
    fn aimed(&self, view: &XrNavView, origin: Vec3f, dir: Vec3f, parts: bool) -> Option<(ElementId, [f64; 3])>;
    /// Where a body a hand carries is: rest -> now (printer mm). None for one it pulls.
    fn carried(&self, e: ElementId) -> Option<Affine>;
}

#[derive(Default)]
pub struct XrMech {
    hold: Option<Hold>,
    /// The held body.
    held: Option<ElementId>,
    carry: Option<Carry>,
    grip_down: [bool; 2],
    tracks: [VelocityTrack; 2],
    /// A one-shot tool owns the grip until it comes up, without starting a hold.
    clicked: [bool; 2],
}

impl XrMech {
    /// The hand holding a body.
    pub fn held_hand(&self) -> Option<usize> {
        self.hold.map(|h| h.hand)
    }

    /// The hand carrying a part.
    pub fn carrying_hand(&self) -> Option<usize> {
        self.carry.map(|c| c.hand)
    }

    /// One headset frame at `now` (s). `picker`: the sim is up and the hands are its; none lets
    /// go. `lead` (s): how far ahead of the tip the drag target goes along its velocity (the time
    /// until the sim's answer shows).
    pub fn step(&mut self, picker: Option<&dyn BodyPick>, tool: MechTool, s: &XrState, view: &XrNavView, now: f64, lead: f32, buttons: &Buttons, menu: &MenuStep, out: &mut MechOut) {
        *out = MechOut::default();
        let active = picker.is_some();
        let ctrl: [&XrController; 2] = [&s.left_controller, &s.right_controller];
        let hands: [&XrHand; 2] = [&s.left_hand, &s.right_hand];
        for i in 0..2 {
            match tip(ctrl[i], hands[i]) {
                Some(t) => self.tracks[i].push(now, t.position),
                None => self.tracks[i] = VelocityTrack::default(),
            }
        }
        let grips = [grab(ctrl[0], hands[0], self.grip_down[0]), grab(ctrl[1], hands[1], self.grip_down[1])];
        if let Some(c) = self.carry {
            match grips[c.hand].filter(|_| active) {
                Some(p) => {
                    let ahead = p.position + self.tracks[c.hand].velocity() * lead;
                    let mut pose = mul(&mul(&pose_affine(view, p.orientation, ahead), &c.from_hand), &c.from_part);
                    pose.iter_mut().for_each(|row| row[3] *= 0.001);
                    out.carry = Some(pose);
                    out.touch[c.hand] = Some(c.part);
                    out.touch_at[c.hand] = Some(p.position + p.orientation.rotate_vec3(&c.local));
                }
                None => self.carry = None,
            }
            out.consumed[c.hand] = true; // includes release: no click on a dropped part
        }
        if let Some(h) = self.hold.filter(|h| h.tool != tool) {
            self.hold = None;
            self.held = None;
            self.grip_down = grips.map(|g| g.is_some());
            self.clicked[h.hand] = self.grip_down[h.hand] && active;
            for i in 0..2 {
                out.consumed[i] |= self.clicked[i];
            }
            out.release = true;
            return;
        }
        for i in 0..2 {
            let down = grips[i].is_some();
            let pressed = down && !self.grip_down[i];
            self.grip_down[i] = down;
            if !down || !active {
                self.clicked[i] = false;
            }
            out.consumed[i] |= self.clicked[i];
            let Some(picker) = picker else { continue };
            if self.hold.is_some_and(|h| h.hand == i) {
                out.touch[i] = self.held;
                continue;
            }
            if self.carry.is_some_and(|c| c.hand == i) {
                continue;
            }
            // the body under a free tip (one box test per body)
            let parts = self.carry.is_none();
            let near = tip(ctrl[i], hands[i]).and_then(|t| picker.near(view, t.position, parts));
            out.touch[i] = near.map(|(e, _)| e);
            out.touch_at[i] = near.map(|(_, at)| room_from_printer(view, at.map(|v| v * 1000.0)));
            if pressed && !buttons.claimed(i) {
                let p = grips[i].unwrap();
                let taken = near.or_else(|| {
                    if (i == 1 && menu.hover.is_some()) || (ctrl[i].active() && ctrl[i].grip < PRESS) {
                        return None;
                    }
                    let (o, d) = ray(&aim(ctrl[i], hands[i])?);
                    picker.aimed(view, o, d, parts)
                });
                if let Some((element, point)) = taken {
                    let at = room_from_printer(view, point.map(|v| v * 1000.0));
                    let local = p.orientation.invert().rotate_vec3(&(at - p.position));
                    // Pin and Poke act on what they press, part or not.
                    if let Some(from_part) = picker.carried(element).filter(|_| parts && !matches!(tool, MechTool::Pin | MechTool::Poke)) {
                        let from_hand = inverse(&pose_affine(view, p.orientation, p.position));
                        self.carry = Some(Carry { hand: i, part: element, from_hand, from_part, local });
                        out.carry_taken = true;
                        // the take's own pose goes with it: the sim carries the part on from where
                        // the hand took it, so a frame that comes late (the hand already on its
                        // way) still carries the part as far as the hand went
                        let mut pose = from_part;
                        pose.iter_mut().for_each(|row| row[3] *= 0.001);
                        out.carry = Some(pose);
                        out.consumed[i] = true;
                        out.touch[i] = Some(element);
                        out.haptics.add(i, haptic::GRAB);
                        continue;
                    }
                    if self.hold.is_some() {
                        continue;
                    }
                    match tool {
                        MechTool::Pin => {
                            out.pin = Some((element, point));
                            self.clicked[i] = true;
                            out.consumed[i] = true;
                        }
                        MechTool::Poke => {
                            let v = self.tracks[i].velocity();
                            let (a, b) = (printer_from_room(view, at), printer_from_room(view, at + v));
                            out.poke = Some((element, point, std::array::from_fn(|k| (b[k] - a[k]) * 0.001)));
                            self.clicked[i] = true;
                            out.consumed[i] = true;
                        }
                        MechTool::Grab | MechTool::Throw | MechTool::Dangle => {
                            self.hold = Some(Hold { hand: i, tool, local, last: at });
                            self.held = Some(element);
                            out.grab = Some((element, point));
                            out.dangle = tool == MechTool::Dangle;
                        }
                    }
                    out.touch[i] = Some(element);
                    out.haptics.add(i, haptic::GRAB);
                }
            }
        }
        let Some(h) = self.hold else { return };
        out.consumed[h.hand] = true; // includes release: no click on a dropped body
        match grips[h.hand].filter(|_| active) {
            Some(p) => {
                let at = p.position + p.orientation.rotate_vec3(&h.local);
                out.consumed[h.hand] = true;
                out.at = Some(at);
                out.touch_at[h.hand] = Some(at);
                if out.grab.is_none() {
                    let ahead = at + self.tracks[h.hand].velocity() * lead;
                    out.drag = Some(printer_from_room(view, ahead).map(|v| v * 0.001));
                }
                self.hold = Some(Hold { last: at, ..h });
            }
            None => {
                self.hold = None;
                self.held = None;
                out.release = true;
                if active && h.tool == MechTool::Throw {
                    // the tip's velocity in printer metres per second
                    let v = self.tracks[h.hand].velocity();
                    let (a, b) = (printer_from_room(view, h.last), printer_from_room(view, h.last + v));
                    out.throw = Some(std::array::from_fn(|k| (b[k] - a[k]) * 0.001));
                }
            }
        }
    }
}

/// The sim's bodies as their boxes where they are now: `moves` (the snapshot the view shows) over
/// each element's bounds at rest.
pub struct SceneBodies<'a> {
    pub state: &'a AppState,
}

impl SceneBodies<'_> {
    /// Each body: its element, its box at rest and its rest -> now.
    fn each(&self, mut f: impl FnMut(ElementId, [Vec3f; 2], &[[f32; 4]; 3])) {
        let state = self.state;
        let Some(moves) = state.scene_state.moves.as_ref() else { return };
        for (id, a) in moves.iter() {
            let Some(e) = state.scene.elements.get(id.index()) else { continue };
            if fab::model::bounds::aabb_is_empty(&e.bounds) {
                continue;
            }
            f(*id, [e.bounds.min, e.bounds.max], a);
        }
    }
}

/// A point on a body (printer metres) in the body's rest frame, as the sim's snapshot has it.
pub fn rest_point(state: &AppState, e: ElementId, p: [f64; 3]) -> Option<Vec3f> {
    let a = state.scene_state.pose(e)?;
    Some(to_rest(&a, vec3f(p[0] as f32, p[1] as f32, p[2] as f32)))
}

/// Where a body's rest-frame point is in the sim's snapshot (printer metres).
pub fn now_point(state: &AppState, e: ElementId, rest: Vec3f) -> Option<Vec3f> {
    Some(from_rest(&state.scene_state.pose(e)?, rest))
}

/// A point into a pose's rest frame (a rigid move).
pub fn to_rest(a: &[[f32; 4]; 3], p: Vec3f) -> Vec3f {
    let d = p - vec3f(a[0][3], a[1][3], a[2][3]);
    vec3f(a[0][0] * d.x + a[1][0] * d.y + a[2][0] * d.z, a[0][1] * d.x + a[1][1] * d.y + a[2][1] * d.z, a[0][2] * d.x + a[1][2] * d.y + a[2][2] * d.z)
}

fn from_rest(a: &[[f32; 4]; 3], p: Vec3f) -> Vec3f {
    vec3f(a[0][0] * p.x + a[0][1] * p.y + a[0][2] * p.z + a[0][3], a[1][0] * p.x + a[1][1] * p.y + a[1][2] * p.z + a[1][3], a[2][0] * p.x + a[2][1] * p.y + a[2][2] * p.z + a[2][3])
}

fn to_f64(v: Vec3f) -> [f64; 3] {
    [v.x as f64, v.y as f64, v.z as f64]
}

fn clamp_box(p: Vec3f, b: &[Vec3f; 2]) -> Vec3f {
    vec3f(p.x.clamp(b[0].x, b[1].x), p.y.clamp(b[0].y, b[1].y), p.z.clamp(b[0].z, b[1].z))
}

impl BodyPick for SceneBodies<'_> {
    fn near(&self, view: &XrNavView, hand: Vec3f, parts: bool) -> Option<(ElementId, [f64; 3])> {
        let p = printer_from_room(view, hand).map(|v| v as f32 * 0.001);
        let p = vec3f(p[0], p[1], p[2]);
        let reach = REACH_M / view.scale.max(1e-6);
        let carried = &self.state.mech.carried;
        let mut best: Option<((f32, f32), ElementId, Vec3f)> = None;
        self.each(|id, b, a| {
            let part = carried.contains(&id);
            if part && !parts {
                return;
            }
            let local = to_rest(a, p);
            let on = clamp_box(local, &b);
            // Among equally near boxes a part first, then the smallest (what is inside the others).
            let rank = ((on - local).length(), if part { 0.0 } else { (b[1] - b[0]).length() });
            if rank.0 <= reach && best.is_none_or(|(r, ..)| rank < r) {
                best = Some((rank, id, from_rest(a, on)));
            }
        });
        best.map(|(_, id, at)| (id, to_f64(at)))
    }

    fn carried(&self, e: ElementId) -> Option<Affine> {
        if !self.state.mech.carried.contains(&e) {
            return None;
        }
        let a = self.state.scene_state.pose(e)?;
        Some(std::array::from_fn(|i| [a[i][0] as f64, a[i][1] as f64, a[i][2] as f64, a[i][3] as f64 * 1000.0]))
    }

    fn aimed(&self, view: &XrNavView, origin: Vec3f, dir: Vec3f, parts: bool) -> Option<(ElementId, [f64; 3])> {
        let o = printer_from_room(view, origin);
        let e = printer_from_room(view, origin + dir);
        let d = vec3f((e[0] - o[0]) as f32, (e[1] - o[1]) as f32, (e[2] - o[2]) as f32) * 0.001;
        let o = vec3f(o[0] as f32, o[1] as f32, o[2] as f32) * 0.001;
        // along the ray, `d` per room metre
        let mut best: Option<(f32, ElementId, Vec3f)> = None;
        self.each(|id, b, a| {
            if !parts && self.state.mech.carried.contains(&id) {
                return;
            }
            let (lo, ld) = (to_rest(a, o), to_rest(a, o + d) - to_rest(a, o));
            let (mut t0, mut t1) = (0.0f32, RAY_M);
            for (o, d, min, max) in [(lo.x, ld.x, b[0].x, b[1].x), (lo.y, ld.y, b[0].y, b[1].y), (lo.z, ld.z, b[0].z, b[1].z)] {
                if d.abs() < 1e-9 {
                    if o < min || o > max {
                        return;
                    }
                    continue;
                }
                let (a0, a1) = ((min - o) / d, (max - o) / d);
                t0 = t0.max(a0.min(a1));
                t1 = t1.min(a0.max(a1));
                if t0 > t1 {
                    return;
                }
            }
            if best.is_none_or(|(bt, ..)| t0 < bt) {
                // inside the body rather than on its box's face
                best = Some((t0, id, from_rest(a, lo + ld * ((t0 + t1) * 0.5))));
            }
        });
        best.map(|(_, id, at)| (id, to_f64(at)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_xr_nav::ClipBox;

    fn view() -> XrNavView {
        XrNavView { clip: ClipBox { center: vec3f(0.0, 0.9, -0.6), yaw: 0.0, half: vec3f(0.5, 0.3, 0.5) }, origin: vec3f(-0.175, 0.9, -0.44), yaw: 0.0, scale: 1.0 }
    }

    /// One body at a printer point (metres), taken when the hand is within 4 cm or the ray is cast.
    struct Pick(Vec3f, bool);

    impl BodyPick for Pick {
        fn near(&self, view: &XrNavView, hand: Vec3f, _: bool) -> Option<(ElementId, [f64; 3])> {
            let p = printer_from_room(view, hand);
            let d = vec3f(p[0] as f32 * 0.001, p[1] as f32 * 0.001, p[2] as f32 * 0.001) - self.0;
            (d.length() < REACH_M).then(|| (ElementId(0), to_f64(self.0)))
        }
        fn aimed(&self, _: &XrNavView, _: Vec3f, _: Vec3f, _: bool) -> Option<(ElementId, [f64; 3])> {
            self.1.then(|| (ElementId(0), to_f64(self.0)))
        }
        fn carried(&self, _: ElementId) -> Option<Affine> {
            None
        }
    }

    /// A part (`ElementId(0)`, resting at a shift of 5 mm up) and a piece (`ElementId(1)`) in one place.
    struct PartAndPiece(Vec3f);

    impl BodyPick for PartAndPiece {
        fn near(&self, view: &XrNavView, hand: Vec3f, parts: bool) -> Option<(ElementId, [f64; 3])> {
            let p = printer_from_room(view, hand);
            let d = vec3f(p[0] as f32 * 0.001, p[1] as f32 * 0.001, p[2] as f32 * 0.001) - self.0;
            (d.length() < REACH_M).then(|| (ElementId(if parts { 0 } else { 1 }), to_f64(self.0)))
        }
        fn aimed(&self, _: &XrNavView, _: Vec3f, _: Vec3f, _: bool) -> Option<(ElementId, [f64; 3])> {
            None
        }
        fn carried(&self, e: ElementId) -> Option<Affine> {
            (e == ElementId(0)).then_some([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 5.0]])
        }
    }

    fn input(at: Vec3f, grip: bool) -> XrState {
        let ident = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        let mut s = XrState::default();
        s.head_pose.orientation = ident;
        for c in [&mut s.left_controller, &mut s.right_controller] {
            c.buttons = XrController::ACTIVE;
            c.grip_pose.orientation = ident;
            c.aim_pose.orientation = ident;
        }
        makepad_xr_ui::input::place_tip(&mut s.left_controller, vec3f(-1.0, 1.0, 0.0));
        makepad_xr_ui::input::place_tip(&mut s.right_controller, at);
        s.right_controller.grip = if grip { 1.0 } else { 0.0 };
        s
    }

    #[test]
    fn either_grab_control_takes_the_body_it_is_on_drags_it_and_lets_go() {
        let v = view();
        let body = vec3f(0.1, 0.1, 0.05);
        let on = room_from_printer(&v, [100.0, 100.0, 50.0]);
        let p = Pick(body, false);
        for (tool, use_trigger) in [MechTool::Grab, MechTool::Throw, MechTool::Dangle].into_iter().flat_map(|t| [(t, false), (t, true)]) {
            let input = |at, down| {
                let mut s = input(at, down);
                if use_trigger {
                    s.right_controller.trigger = s.right_controller.grip;
                    s.right_controller.grip = 0.0;
                }
                s
            };
            let mut m = XrMech::default();
            let mut out = MechOut::default();
            m.step(Some(&p), tool, &input(on, false), &v, 0.014, 0.0, &Buttons::default(), &MenuStep::default(), &mut out);
            assert!(out.grab.is_none() && out.consumed == [false, false]);
            m.step(Some(&p), tool, &input(on, true), &v, 0.028, 0.0, &Buttons::default(), &MenuStep::default(), &mut out);
            assert!(out.grab.is_some() && out.drag.is_none() && out.consumed == [false, true] && m.held_hand() == Some(1));
            assert_eq!(out.dangle, tool == MechTool::Dangle);
            // 5 cm up in the room: 5 cm up the printer's Z.
            m.step(Some(&p), tool, &input(on + vec3f(0.0, 0.05, 0.0), true), &v, 0.042, 0.0, &Buttons::default(), &MenuStep::default(), &mut out);
            let d = out.drag.unwrap();
            assert!((d[2] - 0.1).abs() < 1e-4 && (d[0] - 0.1).abs() < 1e-4, "{d:?}");
            m.step(Some(&p), tool, &input(on + vec3f(0.0, 0.08, 0.0), false), &v, 0.056, 0.0, &Buttons::default(), &MenuStep::default(), &mut out);
            assert!(out.release && out.consumed == [false, true] && m.held_hand().is_none());
            assert_eq!(out.throw.is_some(), tool == MechTool::Throw);
            if let Some(velocity) = out.throw {
                assert!(velocity[2] > 1.0, "{velocity:?}");
            }
        }
    }

    /// One hand takes the part and goes up and turns, and the part with it; the other takes the
    /// piece and pulls it. Either lets go without ending the other's.
    #[test]
    fn one_hand_carries_the_part_and_the_other_pulls_a_piece() {
        let v = view();
        let on = room_from_printer(&v, [100.0, 100.0, 50.0]);
        let p = PartAndPiece(vec3f(0.1, 0.1, 0.05));
        let both = |left: Vec3f, l: bool, right: Vec3f, r: bool, turn: Quat| {
            let mut s = input(right, r);
            s.left_controller.grip_pose.orientation = turn;
            s.left_controller.aim_pose.orientation = turn;
            makepad_xr_ui::input::place_tip(&mut s.left_controller, left);
            s.left_controller.grip = if l { 1.0 } else { 0.0 };
            s
        };
        let ident = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        let (mut m, mut out, far) = (XrMech::default(), MechOut::default(), vec3f(1.0, 1.0, 0.0));
        let frame = |m: &mut XrMech, s: &XrState, now: f64, out: &mut MechOut| m.step(Some(&p), MechTool::Grab, s, &v, now, 0.0, &Buttons::default(), &MenuStep::default(), out);

        frame(&mut m, &both(on, false, far, false, ident), 0.014, &mut out);
        frame(&mut m, &both(on, true, far, false, ident), 0.028, &mut out);
        assert!(out.carry_taken && out.grab.is_none() && out.consumed == [true, false] && m.carrying_hand() == Some(0) && m.held_hand().is_none());
        assert_eq!(out.carry, Some([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.005]]), "the take carries the part from where the hand took it");

        // 5 cm up the room, a quarter turn about the room's up (the printer's Z): the pose goes
        // with the hand, the hand's own point staying under it
        let up = vec3f(0.0, 0.05, 0.0);
        let quarter = Quat { x: 0.0, y: std::f32::consts::FRAC_PI_4.sin(), z: 0.0, w: std::f32::consts::FRAC_PI_4.cos() };
        frame(&mut m, &both(on + up, true, far, false, ident), 0.042, &mut out);
        let pose = out.carry.unwrap();
        assert!((pose[2][3] - 0.055).abs() < 1e-4 && pose[0][3].abs() < 1e-4, "{pose:?}");
        assert!((pose[0][0] - 1.0).abs() < 1e-4, "{pose:?}");
        frame(&mut m, &both(on + up, true, far, false, quarter), 0.056, &mut out);
        let pose = out.carry.unwrap();
        assert!((pose[2][2] - 1.0).abs() < 1e-3 && pose[0][0].abs() < 1e-3, "a turn about the printer's Z: {pose:?}");

        // the other hand takes the piece, and pulls it, while the part stays held
        frame(&mut m, &both(on + up, true, on, true, quarter), 0.070, &mut out);
        assert_eq!(out.grab.map(|g| g.0), Some(ElementId(1)));
        assert!(out.carry.is_some() && out.consumed == [true, true] && m.held_hand() == Some(1));
        frame(&mut m, &both(on + up, true, on + vec3f(0.05, 0.0, 0.0), true, quarter), 0.084, &mut out);
        assert!(out.carry.is_some() && (out.drag.unwrap()[0] - 0.15).abs() < 1e-4, "{:?}", out.drag);

        // letting the part go leaves the piece held, and the next frame the other hand may take the part
        frame(&mut m, &both(on + up, false, on, true, quarter), 0.098, &mut out);
        assert!(out.carry.is_none() && !out.release && m.carrying_hand().is_none() && m.held_hand() == Some(1));
        frame(&mut m, &both(on + up, false, on, false, quarter), 0.112, &mut out);
        assert!(out.release && m.held_hand().is_none());
    }

    #[test]
    fn a_grip_in_empty_space_stays_the_navigation_s() {
        let v = view();
        let mut m = XrMech::default();
        let mut out = MechOut::default();
        let p = Pick(vec3f(5.0, 5.0, 5.0), false);
        m.step(Some(&p), MechTool::Throw, &input(vec3f(0.0, 1.0, 0.0), true), &v, 0.070, 0.0, &Buttons::default(), &MenuStep::default(), &mut out);
        assert!(out.grab.is_none() && out.consumed == [false, false]);
    }

    #[test]
    fn leaving_mechanics_lets_go() {
        let v = view();
        let on = room_from_printer(&v, [100.0, 100.0, 50.0]);
        let mut m = XrMech::default();
        let mut out = MechOut::default();
        let p = Pick(vec3f(0.1, 0.1, 0.05), false);
        m.step(Some(&p), MechTool::Throw, &input(on, true), &v, 0.084, 0.0, &Buttons::default(), &MenuStep::default(), &mut out);
        assert!(out.grab.is_some());
        m.step(None, MechTool::Throw, &input(on, true), &v, 0.098, 0.0, &Buttons::default(), &MenuStep::default(), &mut out);
        assert!(out.release && m.held_hand().is_none());
    }
}

//! A real window's styled shape (`RoomWindow` other than a plain grid:
//! `RoomWindow::is_plain`), as the room casts into it (room.rs `RealWindow`).
//!
//! Everything is built from rules, after the measured parts of two facade
//! kits (an apartment block's sash windows and panelled doors in a classical
//! surround, a factory's arched steel windows, flush steel doors and roller
//! shutter), never their meshes. It lives in the window's own frame: `u`
//! along the wall, `v` up, `w` out through the wall (0 on the wall's room
//! face, the wall's depth on its far face), its middle at the origin.
//!
//! - Its outline: a rectangle, or one whose head is a segmental or a round
//!   arch (`Cell`'s arc). A ray that leaves it sideways before anything else
//!   stops it meets the reveal.
//! - In the wall, prisms one behind the other (`Prism`): a frame, a sash, a
//!   leaf's glazing, each a rectangle (its head following the arch where it
//!   reaches it) with a solid border and bars and open panes between
//!   (`Grid`), glass at a depth in them, or a door's solid leaf with a face
//!   of raised panels, slats or a flush plate (`Face`). A ray meets each one
//!   on its front, on the side of the pane it runs through, or on its edge
//!   where it runs in beside it; the nearest of those, the reveal and the
//!   glass is what it sees.
//! - On the wall's room face, the surround and a door's handles: a few
//!   plain boxes (`Solid`). A ray meets them before the wall.
//!
//! A handful of plane and circle tests per window: no mesh, no march.

use makepad_draw::*;

use super::params::{window_rule, DoorLeaf, RoomWindow, WindowStyle, WindowSurround, WindowTop};

/// A ray's way into a surface is never shorter than this (a surface the ray starts on).
const EPS: f32 = 1.0e-5;

/// What a styled window's surface is painted (`Styled::palette`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Paint {
    Wall,
    Floor,
    Frame,
    Surround,
    Door,
    Metal,
}

/// What a ray meets of a styled window: a surface (its paint, darkened by
/// `shade` in a groove or a gap; `reveal` for the wall's own depth round the
/// opening), a pane's glass (how much glass), or nothing more: it leaves
/// through the far face of the wall.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Met {
    Solid { paint: Paint, shade: f32, reveal: bool },
    Glass(f32),
    Open,
}

/// Where a ray meets a styled window: how far along it (per unit of its
/// direction), the surface's normal (toward the ray, the window's frame) and what.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Meet {
    pub s: f32,
    pub normal: Vec3f,
    pub met: Met,
}

fn solid(s: f32, normal: Vec3f, paint: Paint) -> Meet {
    Meet { s, normal, met: Met::Solid { paint, shade: 1.0, reveal: false } }
}

/// A rectangle of the wall (`lo`..`hi`, u and v), its head cut by an arc
/// where it has one: above `spring` only what lies within `r` of (0, `cy`).
#[derive(Clone, Copy, Debug)]
struct Cell {
    lo: Vec2f,
    hi: Vec2f,
    arc: Option<Arc>,
}

#[derive(Clone, Copy, Debug)]
struct Arc {
    spring: f32,
    cy: f32,
    r: f32,
}

impl Cell {
    fn rect(lo: Vec2f, hi: Vec2f) -> Cell {
        Cell { lo, hi, arc: None }
    }

    /// `p` lies in it.
    fn contains(&self, p: Vec2f) -> bool {
        if !(p.x >= self.lo.x && p.x <= self.hi.x && p.y >= self.lo.y && p.y <= self.hi.y) {
            return false;
        }
        match self.arc {
            Some(a) if p.y > a.spring => p.x * p.x + (p.y - a.cy) * (p.y - a.cy) <= a.r * a.r,
            _ => true,
        }
    }

    /// It shrunk by `l`, `r`, `b`, `t` on its sides (its arc by `t`).
    fn inset(&self, l: f32, r: f32, b: f32, t: f32) -> Cell {
        Cell {
            lo: vec2f(self.lo.x + l, self.lo.y + b),
            hi: vec2f(self.hi.x - r, self.hi.y - t),
            arc: self.arc.map(|a| Arc { r: (a.r - t).max(0.0), ..a }),
        }
    }

    /// Where a ray from `p` inside it along `d` leaves it (how far, per unit
    /// of `d`) and the side's normal, into the cell.
    fn exit(&self, p: Vec2f, d: Vec2f) -> (f32, Vec2f) {
        let mut best = (f32::INFINITY, vec2f(0.0, 0.0));
        if d.x > 1.0e-9 {
            best = ((self.hi.x - p.x) / d.x, vec2f(-1.0, 0.0));
        } else if d.x < -1.0e-9 {
            best = ((self.lo.x - p.x) / d.x, vec2f(1.0, 0.0));
        }
        if d.y > 1.0e-9 {
            let s = (self.hi.y - p.y) / d.y;
            if s < best.0 {
                best = (s, vec2f(0.0, -1.0));
            }
        } else if d.y < -1.0e-9 {
            let s = (self.lo.y - p.y) / d.y;
            if s < best.0 {
                best = (s, vec2f(0.0, 1.0));
            }
        }
        if let Some(a) = self.arc {
            // |q + d s| = r from inside the circle: the far root, where it leaves above the springing
            let q = vec2f(p.x, p.y - a.cy);
            let (aa, bb, cc) = (d.x * d.x + d.y * d.y, q.x * d.x + q.y * d.y, q.x * q.x + q.y * q.y - a.r * a.r);
            let disc = bb * bb - aa * cc;
            if aa > 1.0e-12 && disc >= 0.0 {
                let s = (-bb + disc.sqrt()) / aa;
                let at = q + d * s;
                if s >= 0.0 && s < best.0 && at.y + a.cy > a.spring {
                    let len = at.length().max(1.0e-6);
                    best = (s, vec2f(-at.x / len, -at.y / len));
                }
            }
        }
        (best.0.max(0.0), best.1)
    }

    /// Where a ray from `p` outside its rectangle along `d` runs into it,
    /// no further than `reach`, and that side's normal, out of the cell.
    fn entry(&self, p: Vec2f, d: Vec2f, reach: f32) -> Option<(f32, Vec2f)> {
        let (mut s0, mut s1, mut n) = (0.0f32, reach, vec2f(0.0, 0.0));
        for (c, dc, lo, hi, axis) in [(p.x, d.x, self.lo.x, self.hi.x, vec2f(1.0, 0.0)), (p.y, d.y, self.lo.y, self.hi.y, vec2f(0.0, 1.0))] {
            if dc.abs() < 1.0e-9 {
                if c < lo || c > hi {
                    return None;
                }
                continue;
            }
            let (a, b) = ((lo - c) / dc, (hi - c) / dc);
            let (near, far, side) = if a < b { (a, b, axis * -1.0) } else { (b, a, axis) };
            if near > s0 {
                s0 = near;
                n = side;
            }
            s1 = s1.min(far);
        }
        (s0 < s1 && s0 > 0.0 && (n.x != 0.0 || n.y != 0.0)).then_some((s0, n))
    }
}

/// A frame or a sash: its `region`, solid in a border round it, open inside
/// (`inner`) but for `n` panes' bars `bar` either side of their lines, the
/// panes even (a bar at the border's edge would be half in it).
#[derive(Clone, Copy, Debug)]
struct Grid {
    region: Cell,
    edge: [f32; 4],
    inner: Cell,
    n: (u32, u32),
    bar: f32,
}

impl Grid {
    fn new(region: Cell, edge: [f32; 4], n: (u32, u32), bar: f32) -> Grid {
        Grid { region, edge, inner: region.inset(edge[0], edge[1], edge[2], edge[3]), n: (n.0.max(1), n.1.max(1)), bar: bar.max(0.0) }
    }

    /// The open pane `p` lies in, None on the border or a bar (or off the
    /// region). Its bars and its border, where it has them, are at least
    /// `widen` either side of their lines (the half of the footprint a map's
    /// texel has there, so a bar thinner than a texel stays an unbroken line
    /// in place of a dashed one), the lines where they are.
    fn pane(&self, p: Vec2f, widen: f32) -> Option<Cell> {
        let wide = |e: f32| if e > 0.0 { e.max(widen) } else { 0.0 };
        let inner = if widen > 0.0 { self.region.inset(wide(self.edge[0]), wide(self.edge[1]), wide(self.edge[2]), wide(self.edge[3])) } else { self.inner };
        if !inner.contains(p) {
            return None;
        }
        let half = wide(self.bar);
        let along = |c: f32, lo: f32, hi: f32, n: u32, inner_lo: f32, inner_hi: f32| -> Option<(f32, f32)> {
            if n <= 1 {
                return Some((inner_lo, inner_hi));
            }
            let pitch = (hi - lo + 2.0 * self.bar) / n as f32;
            let base = lo - self.bar;
            let i = ((c - base) / pitch).floor().clamp(0.0, (n - 1) as f32);
            let (a, b) = ((base + i * pitch + half).max(inner_lo), (base + (i + 1.0) * pitch - half).min(inner_hi));
            (c >= a && c <= b).then_some((a, b))
        };
        let (u0, u1) = along(p.x, self.inner.lo.x, self.inner.hi.x, self.n.0, inner.lo.x, inner.hi.x)?;
        let (v0, v1) = along(p.y, self.inner.lo.y, self.inner.hi.y, self.n.1, inner.lo.y, inner.hi.y)?;
        Some(Cell { lo: vec2f(u0, v0), hi: vec2f(u1, v1), arc: inner.arc })
    }

    /// The share of its inner rectangle its bars leave open.
    #[cfg(test)]
    fn open_share(&self) -> f32 {
        let share = |lo: f32, hi: f32, n: u32| if n <= 1 { 1.0 } else { ((hi - lo - 2.0 * self.bar * (n - 1) as f32) / (hi - lo).max(1.0e-6)).clamp(0.0, 1.0) };
        share(self.inner.lo.x, self.inner.hi.x, self.n.0) * share(self.inner.lo.y, self.inner.hi.y, self.n.1)
    }
}

/// A door leaf's face (`Prism`'s leaf): plain (a flush steel leaf), raised
/// panels in fields sunk `recess` into it, or a roller shutter's slats
/// `pitch` apart, each ending in a dark groove.
#[derive(Clone, Debug)]
enum Face {
    Plain,
    Panels { fields: Vec<Cell>, recess: f32 },
    Slats { pitch: f32 },
}

#[derive(Clone, Debug)]
enum Body {
    /// A frame, a sash or a leaf's glazing, with glass at depth `glass.0`
    /// (`glass.1` of it: 0 an open hole) where it has panes.
    Grid { grid: Grid, glass: Option<(f32, f32)>, moulded: bool },
    /// A door's solid leaf over `rect`.
    Leaf { rect: Cell, face: Face },
}

/// One of the window's parts in the wall, from depth `w0` to `w1`.
#[derive(Clone, Debug)]
struct Prism {
    w0: f32,
    w1: f32,
    body: Body,
    paint: Paint,
}

/// A side's normal (in u, v) as the window's frame has it, tilted toward the
/// room where the part is moulded (its edge rounded: it catches the light as
/// an ovolo does).
fn side_normal(n: Vec2f, moulded: bool) -> Vec3f {
    if moulded {
        vec3f(n.x, n.y, -0.8).normalize()
    } else {
        vec3f(n.x, n.y, 0.0)
    }
}

impl Prism {
    fn region(&self) -> &Cell {
        match &self.body {
            Body::Grid { grid, .. } => &grid.region,
            Body::Leaf { rect, .. } => rect,
        }
    }

    /// The first of this part a ray from `o` along `d` (out through the wall) meets.
    fn meet(&self, o: Vec3f, d: Vec3f, widen: f32) -> Option<Meet> {
        let along = |w: f32| (w - o.z) / d.z;
        let (sa, sb) = (along(self.w0), along(self.w1));
        if !(sb > EPS) {
            return None;
        }
        let start = sa.max(0.0);
        let at = |s: f32| vec2f(o.x + d.x * s, o.y + d.y * s);
        let d2 = vec2f(d.x, d.y);
        let pa = at(start);
        if self.region().contains(pa) {
            return match &self.body {
                Body::Grid { grid, glass, moulded } => match grid.pane(pa, widen) {
                    Some(pane) => {
                        // open in front: the pane's side before its glass or its back, else the glass
                        let end = glass.map_or(sb, |(w, _)| along(w));
                        if !(end > start) {
                            return None;
                        }
                        let (s, n) = pane.exit(pa, d2);
                        if start + s < end {
                            Some(solid(start + s, side_normal(n, *moulded), self.paint))
                        } else {
                            glass.map(|(_, amount)| Meet { s: end, normal: vec3f(0.0, 0.0, -1.0), met: Met::Glass(amount) })
                        }
                    }
                    None => (sa >= 0.0).then(|| solid(sa, vec3f(0.0, 0.0, -1.0), self.paint)),
                },
                Body::Leaf { rect, face } => (sa >= 0.0).then(|| self.leaf_face(rect, face, o, d, sa, widen)),
            };
        }
        // beside it in front: it runs in across its edge before its back
        let (s, n) = self.region().entry(pa, d2, sb - start)?;
        let p = at(start + s);
        let closed = match &self.body {
            Body::Grid { grid, .. } => grid.pane(p, widen).is_none(),
            Body::Leaf { .. } => true,
        };
        closed.then(|| solid(start + s, vec3f(n.x, n.y, 0.0), self.paint))
    }

    /// A ray meeting a leaf's face at `sa`: on its flat, or in a panel's
    /// field (the field's sides, the raised panel's face or its bevel, the
    /// field's floor), or on a slat (its curve, the groove dark); the leaf's
    /// edges are a dark gap.
    fn leaf_face(&self, rect: &Cell, face: &Face, o: Vec3f, d: Vec3f, sa: f32, widen: f32) -> Meet {
        let at = |s: f32| vec2f(o.x + d.x * s, o.y + d.y * s);
        let along = |w: f32| (w - o.z) / d.z;
        let p = at(sa);
        let gap = widen.max(0.004);
        let edge = p.x - rect.lo.x < gap || rect.hi.x - p.x < gap || rect.hi.y - p.y < gap;
        let flat = Meet { s: sa, normal: vec3f(0.0, 0.0, -1.0), met: Met::Solid { paint: self.paint, shade: if edge { 0.25 } else { 1.0 }, reveal: false } };
        match face {
            Face::Plain => flat,
            Face::Slats { pitch } => {
                // each slat a shallow curve facing up at its top and down at its foot, a groove under it; where a
                // texel's footprint spans more than a slat they fade to their mean, so they never beat into moire
                let phase = ((p.y - rect.lo.y) / pitch).rem_euclid(1.0);
                let (k, shade) = if phase < 0.2 { (-0.5, 0.35) } else { (((phase - 0.2) / 0.8 - 0.5) * 1.1, 1.0) };
                let fade = (6.0 * widen / pitch - 1.0).clamp(0.0, 1.0);
                let (k, shade) = (k * (1.0 - fade), shade + (0.87 - shade) * fade);
                Meet { s: sa, normal: vec3f(0.0, k, -1.0).normalize(), met: Met::Solid { paint: self.paint, shade, reveal: false } }
            }
            Face::Panels { fields, recess } => {
                // a plain shallow inset: a bevel round it sloping down into it, its flat floor
                let Some(field) = fields.iter().find(|f| f.contains(p)) else { return flat };
                let bevel = 0.025;
                let sides = [(p.x - field.lo.x, vec2f(1.0, 0.0)), (field.hi.x - p.x, vec2f(-1.0, 0.0)), (p.y - field.lo.y, vec2f(0.0, 1.0)), (field.hi.y - p.y, vec2f(0.0, -1.0))];
                let (gap, inward) = sides.into_iter().fold((f32::INFINITY, vec2f(0.0, 0.0)), |a, b| if b.0 < a.0 { b } else { a });
                if gap < bevel {
                    let s = along(self.w0 + recess * gap / bevel);
                    solid(s, vec3f(inward.x * recess / bevel, inward.y * recess / bevel, -1.0).normalize(), self.paint)
                } else {
                    // (its floor a little darker, as the light into a shallow recess is)
                    Meet { s: along(self.w0 + recess), normal: vec3f(0.0, 0.0, -1.0), met: Met::Solid { paint: self.paint, shade: 0.8, reveal: false } }
                }
            }
        }
    }
}

/// A convex solid on the wall's room face: the points with `n . p <= c`
/// for each of its planes (the window's frame).
#[derive(Clone, Debug)]
struct Solid {
    planes: Vec<(Vec3f, f32)>,
    paint: Paint,
    lo: Vec3f,
    hi: Vec3f,
}

impl Solid {
    fn boxed(lo: Vec3f, hi: Vec3f, paint: Paint) -> Solid {
        let planes = vec![
            (vec3f(-1.0, 0.0, 0.0), -lo.x),
            (vec3f(1.0, 0.0, 0.0), hi.x),
            (vec3f(0.0, -1.0, 0.0), -lo.y),
            (vec3f(0.0, 1.0, 0.0), hi.y),
            (vec3f(0.0, 0.0, -1.0), -lo.z),
            (vec3f(0.0, 0.0, 1.0), hi.z),
        ];
        Solid { planes, paint, lo, hi }
    }

    /// Where a ray from `o` along `d` runs into it, no further than `reach`.
    fn meet(&self, o: Vec3f, d: Vec3f, reach: f32) -> Option<(f32, Vec3f)> {
        let (mut t0, mut t1, mut normal) = (f32::NEG_INFINITY, reach, Vec3f::default());
        for &(n, c) in &self.planes {
            let (dn, on) = (n.dot(d), n.dot(o) - c);
            if dn.abs() < 1.0e-9 {
                if on > 0.0 {
                    return None;
                }
                continue;
            }
            let t = -on / dn;
            if dn < 0.0 {
                if t > t0 {
                    t0 = t;
                    normal = n;
                }
            } else if t < t1 {
                t1 = t;
            }
            if t0 > t1 {
                return None;
            }
        }
        (t0 > EPS && t0 < t1).then_some((t0, normal))
    }
}

/// A styled window or door as the room casts into it (`room::RealWindow`).
#[derive(Clone, Debug)]
pub(crate) struct Styled {
    outline: Cell,
    depth: f32,
    door: bool,
    prisms: Vec<Prism>,
    solids: Vec<Solid>,
    /// What the solids and the bands fill, in the window's frame (empty when none).
    lo: Vec3f,
    hi: Vec3f,
    /// The albedo of each `Paint`.
    palette: [Vec3f; 6],
    /// Its main glass, this deep in the wall (the daylight of its reveal comes from there).
    pub glass_w: f32,
    /// The share of its rectangle the outside comes in through, its glass's loss in.
    pub share: f32,
}

fn rgb(c: [f32; 3]) -> Vec3f {
    vec3f(c[0], c[1], c[2])
}

impl Styled {
    /// The window `p` (as clamp() holds it) with half sizes `half` (held
    /// inside its wall), `depth` deep in a wall of colour `wall` over a floor
    /// of colour `floor`, with `glass` in its panes; `room_below` is how much
    /// wall there is under its opening (for an apron), `room_above` over it.
    #[allow(clippy::too_many_arguments)]
    pub fn new(p: &RoomWindow, half: Vec2f, depth: f32, glass: f32, wall: Vec3f, floor: Vec3f, room_below: f32, room_above: f32) -> Styled {
        let (hx, hy) = (half.x, half.y);
        let seed = p.seed();
        let rule = |salt: u32, lo: f32, hi: f32| lo + (hi - lo) * window_rule(seed, salt);
        // the outline, its head arched
        let arc = match p.top() {
            WindowTop::Flat => None,
            top => {
                let rise = if top == WindowTop::Round { hx } else { p.rise() }.min(hx).min(1.8 * hy).max(0.01);
                let r = (hx * hx + rise * rise) / (2.0 * rise);
                Some(Arc { spring: hy - rise, cy: hy - r, r })
            }
        };
        let outline = Cell { lo: vec2f(-hx, -hy), hi: vec2f(hx, hy), arc };
        let set = 0.65 * depth;
        let f = p.frame_m.max(0.0);
        let door = p.is_door();
        let metal = if matches!(p.style(), WindowStyle::Steel) || p.leaf() == DoorLeaf::Flush || p.leaf() == DoorLeaf::Shutter { vec3f(0.5, 0.5, 0.5) } else { vec3f(0.5, 0.4, 0.22) };
        let palette = [wall, floor, rgb(p.frame_paint()), p.surround_color.map_or(wall, rgb), rgb(p.door_paint()), metal];
        let mut prisms = Vec::new();
        let mut solids = Vec::new();
        let grid = |region: Cell, edge: [f32; 4], n: (u32, u32), bar: f32, w0: f32, w1: f32, glass_at: Option<f32>, moulded: bool, paint: Paint| Prism {
            w0: w0.clamp(0.0, depth),
            w1: w1.clamp(0.0, depth),
            body: Body::Grid { grid: Grid::new(region, edge, n, bar), glass: glass_at.map(|w| (w.clamp(0.0, depth), glass)), moulded },
            paint,
        };
        let (panes_x, panes_y) = (p.panes_x.max(1), p.panes_y.max(1));
        let mut glass_w = set;
        if door {
            Self::door(p, &outline, depth, set, f, &mut prisms, &mut solids, &grid, seed);
        } else {
            match p.style() {
                WindowStyle::Grid => {
                    // the old window's frame and bars (one width, as deep as wide), its head along the arch
                    let pane = vec2f(2.0 * hx / panes_x as f32, 2.0 * hy / panes_y as f32);
                    let bar = (0.5 * f).min(0.45 * pane.x.min(pane.y));
                    let bar_depth = (2.0 * bar).min(depth);
                    let set = (0.65 * depth).max(0.5 * bar_depth).min((depth - 0.5 * bar_depth).max(0.5 * bar_depth));
                    glass_w = set;
                    prisms.push(grid(outline, [bar; 4], (panes_x, panes_y), bar, set - 0.5 * bar_depth, set + 0.5 * bar_depth, Some(set), false, Paint::Frame));
                }
                WindowStyle::Steel => {
                    // a thin steel frame, and in it bars a third of its width standing just proud of the glass
                    let fw = f.max(0.015);
                    let frame = grid(outline, [fw; 4], (1, 1), 0.0, set - 0.035, set + 0.01, None, false, Paint::Frame);
                    let inner = outline.inset(fw, fw, fw, fw);
                    prisms.push(frame);
                    prisms.push(grid(inner, [0.0; 4], (panes_x, panes_y), (0.3 * f).max(0.006), set - 0.014, set + 0.004, Some(set), false, Paint::Frame));
                }
                WindowStyle::Sash => {
                    // the box frame's lining, the lower sash in the inner track and the upper sash in the outer one,
                    // the lower's top rail (the meeting rail) the thicker; each sash's glazing bars thinner and
                    // shallower than its stiles, just proud of its glass
                    let t = 0.045;
                    let lining = 0.5 * f;
                    prisms.push(grid(outline, [lining; 4], (1, 1), 0.0, set - t - 0.03, set + t + 0.02, None, true, Paint::Frame));
                    let inside = outline.inset(lining, lining, lining, lining);
                    let meet = if let Some(a) = arc { 0.5 * (inside.lo.y + a.spring.min(inside.hi.y)) } else { 0.5 * (inside.lo.y + inside.hi.y) };
                    let rail = 1.3 * f;
                    let lower = Cell::rect(inside.lo, vec2f(inside.hi.x, meet + 0.5 * rail));
                    let upper = Cell { lo: vec2f(inside.lo.x, meet - 0.5 * rail), ..inside };
                    let lower_n = (p.lower_panes_x.unwrap_or(1), p.lower_panes_y.unwrap_or(1));
                    let gbar = 0.33 * f;
                    for (region, edge, n, w) in [(lower, [f, f, 1.15 * f, rail], lower_n, set - t), (upper, [f, f, 0.9 * f, f], (panes_x, panes_y), set)] {
                        prisms.push(grid(region, edge, (1, 1), 0.0, w, w + t, None, true, Paint::Frame));
                        let glazed = region.inset(edge[0], edge[1], edge[2], edge[3]);
                        prisms.push(grid(glazed, [0.0; 4], n, gbar, w + 0.45 * t, w + 0.75 * t, Some(w + 0.7 * t), true, Paint::Frame));
                    }
                    glass_w = set + 0.7 * t;
                }
                WindowStyle::Casement => {
                    // a fixed frame with a mullion between the leaves; each leaf its own frame and its glazing
                    let leaves = p.leaves();
                    let (fo, mullion) = (0.6 * f, 0.6 * f);
                    prisms.push(grid(outline, [fo; 4], (leaves, 1), mullion, set - 0.05, set + 0.03, None, false, Paint::Frame));
                    let inner = outline.inset(fo, fo, fo, fo);
                    let pitch = (inner.hi.x - inner.lo.x + 2.0 * mullion) / leaves as f32;
                    for i in 0..leaves {
                        let (u0, u1) = (inner.lo.x - mullion + i as f32 * pitch + mullion, inner.lo.x - mullion + (i + 1) as f32 * pitch - mullion);
                        let leaf = Cell { lo: vec2f(u0.max(inner.lo.x), inner.lo.y), hi: vec2f(u1.min(inner.hi.x), inner.hi.y), arc: inner.arc };
                        let e = 0.8 * f;
                        prisms.push(grid(leaf, [e; 4], (1, 1), 0.0, set - 0.035, set + 0.01, None, true, Paint::Frame));
                        prisms.push(grid(leaf.inset(e, e, e, e), [0.0; 4], (panes_x, panes_y), 0.25 * f, set - 0.012, set + 0.002, Some(set - 0.004), false, Paint::Frame));
                    }
                    glass_w = set - 0.004;
                }
            }
        }
        Self::surround(p, hx, hy, arc, room_below, room_above, &mut solids, &rule);
        let (mut lo, mut hi) = (vec3f(f32::INFINITY, f32::INFINITY, f32::INFINITY), vec3f(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY));
        for s in &solids {
            lo = vec3f(lo.x.min(s.lo.x), lo.y.min(s.lo.y), lo.z.min(s.lo.z));
            hi = vec3f(hi.x.max(s.hi.x), hi.y.max(s.hi.y), hi.z.max(s.hi.z));
        }
        let mut styled = Styled { outline, depth, door, prisms, solids, lo, hi, palette, glass_w, share: 0.0 };
        styled.share = styled.open_share();
        styled
    }

    /// A door's casing, its transom and its leaves, and their handles.
    #[allow(clippy::too_many_arguments)]
    fn door(p: &RoomWindow, outline: &Cell, depth: f32, set: f32, f: f32, prisms: &mut Vec<Prism>, solids: &mut Vec<Solid>, grid: &dyn Fn(Cell, [f32; 4], (u32, u32), f32, f32, f32, Option<f32>, bool, Paint) -> Prism, seed: u32) {
        let leaf_kind = p.leaf();
        let shutter = leaf_kind == DoorLeaf::Shutter;
        // the casing round the opening (a shutter's guides), standing proud of the leaf; no threshold
        let cf = if shutter { 0.05 } else { f.max(0.04) };
        let front = (set - 0.035).max(0.0);
        // (a door's lining covers its reveal; a shutter's guides stand in it)
        let w0 = if shutter { front - 0.05 } else { 0.0 };
        prisms.push(grid(*outline, [cf, cf, 0.0, cf], (1, 1), 0.0, w0, depth, None, false, Paint::Frame));
        let inside = outline.inset(cf, cf, 0.0, cf);
        // a transom of small panes over the leaves (or, under an arch with none, a fixed fanlight in it)
        let transom = p.transom().min(0.45 * (inside.hi.y - inside.lo.y));
        let top = match (transom > 0.05, inside.arc) {
            (true, _) => inside.hi.y - transom,
            (false, Some(a)) => a.spring.min(inside.hi.y),
            (false, None) => inside.hi.y,
        };
        if top < inside.hi.y - 1.0e-3 {
            let region = Cell { lo: vec2f(inside.lo.x, top), ..inside };
            let n = (((inside.hi.x - inside.lo.x) / 0.22).round() as u32).clamp(1, 10);
            let rail = 0.06;
            prisms.push(grid(region, [0.0, 0.0, rail, 0.0], (n, 1), 0.012, front - 0.01, front + 0.04, Some(front + 0.02), true, Paint::Frame));
        }
        let leaves = if shutter { 1 } else { p.leaves() };
        let width = (inside.hi.x - inside.lo.x) / leaves as f32;
        let w_leaf = front;
        let r = |salt: u32| window_rule(seed, salt);
        for i in 0..leaves {
            let (u0, u1) = (inside.lo.x + i as f32 * width, inside.lo.x + (i + 1) as f32 * width);
            let rect = Cell::rect(vec2f(u0, inside.lo.y), vec2f(u1, top));
            let (lw, lh) = (u1 - u0, top - inside.lo.y);
            let base = inside.lo.y;
            // a panelled leaf's insets: two (one over the other) on a narrow leaf, else two or four
            let stile = (0.15 * lw).clamp(0.08, 0.16);
            let cols = if lw > 0.6 && r(20) < 0.6 { 2 } else { 1 };
            let insets = |v0: f32, v1: f32| -> Vec<Cell> {
                let gap = stile * 0.8;
                let inner = (lw - 2.0 * stile - (cols - 1) as f32 * gap) / cols as f32;
                (0..cols).map(|c| {
                    let a = u0 + stile + c as f32 * (inner + gap);
                    Cell::rect(vec2f(a, base + v0 * lh), vec2f(a + inner, base + v1 * lh))
                }).collect()
            };
            // a plain bar handle on the lock side (on each leaf at the meeting stiles)
            let lock = if leaves == 1 { u1 - 0.08 } else if i == 0 { u1 - 0.07 } else { u0 + 0.07 };
            let handle_v = base + 1.0f32.min(0.48 * lh);
            match leaf_kind {
                DoorLeaf::Panelled => {
                    let fields = [(0.1, 0.42), (0.52, 0.92)].iter().flat_map(|&(a, b)| insets(a, b)).collect();
                    prisms.push(Prism { w0: w_leaf, w1: depth, body: Body::Leaf { rect, face: Face::Panels { fields, recess: 0.018 } }, paint: Paint::Door });
                }
                DoorLeaf::Glazed => {
                    // one glass light over a single inset
                    let split = base + 0.42 * lh;
                    let lower = Cell::rect(rect.lo, vec2f(u1, split));
                    let fields = vec![Cell::rect(vec2f(u0 + stile, base + 0.1 * lh), vec2f(u1 - stile, split - 0.06))];
                    prisms.push(Prism { w0: w_leaf, w1: depth, body: Body::Leaf { rect: lower, face: Face::Panels { fields, recess: 0.016 } }, paint: Paint::Door });
                    let upper = Cell::rect(vec2f(u0, split), rect.hi);
                    let n = (p.panes_x.clamp(1, 2), p.panes_y.clamp(1, 3));
                    prisms.push(grid(upper, [stile, stile, 0.0, 0.1], n, 0.015, w_leaf, w_leaf + 0.045, Some(w_leaf + 0.025), false, Paint::Door));
                }
                DoorLeaf::Flush => {
                    prisms.push(Prism { w0: w_leaf, w1: depth, body: Body::Leaf { rect, face: Face::Plain }, paint: Paint::Door });
                }
                DoorLeaf::Shutter => {
                    prisms.push(Prism { w0: w_leaf, w1: depth, body: Body::Leaf { rect, face: Face::Slats { pitch: 0.077 } }, paint: Paint::Door });
                    // its bottom bar
                    solids.push(Solid::boxed(vec3f(u0, base, w_leaf - 0.025), vec3f(u1, base + 0.09, w_leaf), Paint::Metal));
                }
            }
            if leaf_kind != DoorLeaf::Shutter {
                solids.push(Solid::boxed(vec3f(lock - 0.014, handle_v - 0.15, w_leaf - 0.045), vec3f(lock + 0.014, handle_v + 0.15, w_leaf), Paint::Metal));
            }
        }
        if shutter {
            // the shutter's coil box over the opening on the room side
            let (hx, hy) = (outline.hi.x, outline.hi.y);
            solids.push(Solid::boxed(vec3f(-hx - 0.12, hy, -0.42), vec3f(hx + 0.12, hy + 0.42, 0.0), Paint::Frame));
        }
    }

    /// The surround on the wall's room face, a few plain boxes: a plain one's
    /// projecting sill and lintel band (round an arched head, a band along
    /// it), a classical one's flat frame band round the opening under a
    /// projecting cap, over a sill. Its sizes come from the seed, within the
    /// kit's ranges.
    #[allow(clippy::too_many_arguments)]
    fn surround(p: &RoomWindow, hx: f32, hy: f32, arc: Option<Arc>, below: f32, above: f32, solids: &mut Vec<Solid>, rule: &dyn Fn(u32, f32, f32) -> f32) {
        let door = p.is_door();
        let sill_out = p.sill_out();
        let lintel_out = p.lintel_out();
        let paint = Paint::Surround;
        match p.surround() {
            WindowSurround::None => {}
            WindowSurround::Plain => {
                let ext = 0.07;
                if !door && sill_out > 0.0 {
                    let sh = rule(30, 0.08, 0.12).min(below.max(0.0));
                    if sh > 0.01 {
                        solids.push(Solid::boxed(vec3f(-hx - ext, -hy - sh, -sill_out), vec3f(hx + ext, -hy, 0.0), paint));
                    }
                }
                // (an arched head carries no lintel band: the arch is the wall's)
                let lh = rule(31, 0.12, 0.18).min(above.max(0.0));
                if lintel_out > 0.0 && arc.is_none() && lh > 0.01 {
                    solids.push(Solid::boxed(vec3f(-hx - ext, hy, -lintel_out), vec3f(hx + ext, hy + lh, 0.0), paint));
                }
            }
            WindowSurround::Classical => {
                // the frame band: its sides and its head, all one width and as far out
                let bw = rule(40, 0.1, 0.13);
                let bo = rule(41, 0.04, 0.055);
                let sh = 0.065f32;
                let sill = !door && below > sh;
                let foot = if sill { -hy - sh } else { -hy - below.min(0.0) };
                let head = bw.min(above.max(0.0));
                solids.push(Solid::boxed(vec3f(-hx - bw, foot, -bo), vec3f(-hx, hy + head, 0.0), paint));
                solids.push(Solid::boxed(vec3f(hx, foot, -bo), vec3f(hx + bw, hy + head, 0.0), paint));
                solids.push(Solid::boxed(vec3f(-hx, hy, -bo), vec3f(hx, hy + head, 0.0), paint));
                // the cap over it, standing out further and a little wider
                let ch = rule(42, 0.06, 0.08).min((above - head).max(0.0));
                if ch > 0.01 {
                    let co = lintel_out.max(bo + 0.03);
                    solids.push(Solid::boxed(vec3f(-hx - bw - 0.04, hy + head, -co), vec3f(hx + bw + 0.04, hy + head + ch, 0.0), paint));
                }
                if sill {
                    let so = sill_out.max(bo + 0.02);
                    solids.push(Solid::boxed(vec3f(-hx - bw - 0.03, -hy - sh, -so), vec3f(hx + bw + 0.03, -hy, 0.0), paint));
                }
            }
        }
    }

    /// The albedo of `paint`, darkened by `shade`.
    pub fn albedo(&self, paint: Paint, shade: f32) -> Vec3f {
        self.palette[paint as usize] * shade
    }

    /// `p` (the window's frame) lies in its rectangle, on the wall's room
    /// face or in the wall's depth: in its opening, or on the wall an arched
    /// head leaves in the rectangle's corners (which lies in the opening's
    /// own plane, so must not take its light as a point in front of it would).
    pub fn in_recess(&self, p: Vec3f) -> bool {
        let m = 2.0e-3;
        p.z > -m && p.z < self.depth + m && p.x.abs() <= self.outline.hi.x + m && p.y >= self.outline.lo.y - m && p.y <= self.outline.hi.y + m
    }

    /// `p` on the wall's room face lies in its opening (its head's arch in).
    pub fn opens_at(&self, p: Vec2f) -> bool {
        self.outline.contains(p)
    }

    /// What a ray from `o` (on the wall's room face in the opening, or in it)
    /// along `d` (out through the wall) meets in the wall: the reveal where it
    /// leaves the outline first, the nearest part, a pane's glass, or the far
    /// face (Open). None off the opening. Its bars and borders are at least
    /// `widen` either side of their lines (`Grid::pane`).
    pub fn trace(&self, o: Vec3f, d: Vec3f, widen: f32) -> Option<Meet> {
        if !(d.z > 1.0e-6) {
            return None;
        }
        let start = (-o.z / d.z).max(0.0);
        let p0 = vec2f(o.x + d.x * start, o.y + d.y * start);
        if !self.outline.contains(p0) {
            return None;
        }
        let far = (self.depth - o.z) / d.z;
        let (s, n) = self.outline.exit(p0, vec2f(d.x, d.y));
        let mut best = if start + s < far {
            let paint = if self.door && n.y > 0.5 { Paint::Floor } else { Paint::Wall };
            Meet { s: start + s, normal: vec3f(n.x, n.y, 0.0), met: Met::Solid { paint, shade: 1.0, reveal: true } }
        } else {
            Meet { s: far.max(start), normal: vec3f(0.0, 0.0, -1.0), met: Met::Open }
        };
        for prism in &self.prisms {
            if let Some(m) = prism.meet(o, d, widen) {
                if m.s < best.s && m.s >= start - 1.0e-4 {
                    best = m;
                }
            }
        }
        Some(best)
    }

    /// Where a ray from `o` along `d` first runs into the surround or a
    /// door's handles, no further than `reach`: how far, the normal, the paint.
    pub fn parts(&self, o: Vec3f, d: Vec3f, reach: f32) -> Option<(f32, Vec3f, Paint)> {
        if self.solids.is_empty() {
            return None;
        }
        // its box first
        let (mut t0, mut t1) = (0.0f32, reach);
        for (oc, dc, lo, hi) in [(o.x, d.x, self.lo.x, self.hi.x), (o.y, d.y, self.lo.y, self.hi.y), (o.z, d.z, self.lo.z, self.hi.z)] {
            if dc.abs() < 1.0e-9 {
                if oc < lo || oc > hi {
                    return None;
                }
                continue;
            }
            let (a, b) = ((lo - oc) / dc, (hi - oc) / dc);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
            if t0 > t1 {
                return None;
            }
        }
        let mut best: Option<(f32, Vec3f, Paint)> = None;
        for s in &self.solids {
            let reach = best.map_or(reach, |b| b.0);
            if let Some((t, n)) = s.meet(o, d, reach) {
                best = Some((t, n, s.paint));
            }
        }
        best
    }

    /// The share of its rectangle the outside comes in through, straight
    /// out: its panes' glass (less the glass's loss, a tenth, and a little
    /// absorption) and open holes, the frames, bars, leaves and the arch's
    /// spandrels closed.
    fn open_share(&self) -> f32 {
        let (lo, hi) = (self.outline.lo, self.outline.hi);
        let n = 48;
        let mut sum = 0.0;
        for j in 0..n {
            for i in 0..n {
                let u = lo.x + (hi.x - lo.x) * (i as f32 + 0.5) / n as f32;
                let v = lo.y + (hi.y - lo.y) * (j as f32 + 0.5) / n as f32;
                sum += match self.trace(vec3f(u, v, 0.0), vec3f(0.0, 0.0, 1.0), 0.0).map(|m| m.met) {
                    Some(Met::Glass(g)) => 1.0 - 0.12 * g,
                    Some(Met::Open) => 1.0,
                    _ => 0.0,
                };
            }
        }
        sum / (n * n) as f32
    }

    /// Its grids' open share, for a test.
    #[cfg(test)]
    fn grids_open(&self) -> Vec<f32> {
        self.prisms.iter().filter_map(|p| if let Body::Grid { grid, .. } = &p.body { Some(grid.open_share()) } else { None }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(style: &str) -> RoomWindow {
        RoomWindow { style: Some(style.to_string()), sill_m: 1.0, width_m: 1.2, height_m: 1.8, panes_x: 3, panes_y: 2, frame_m: 0.06, ..RoomWindow::default() }
    }

    fn styled(p: &RoomWindow) -> Styled {
        Styled::new(p, vec2f(0.5 * p.width_m, 0.5 * p.height_m), 0.25, 1.0, vec3f(0.6, 0.6, 0.6), vec3f(0.3, 0.3, 0.3), 1.0, 1.0)
    }

    #[test]
    fn a_ray_straight_out_meets_glass_in_a_pane_and_the_frame_on_its_edge() {
        for style in ["grid", "sash", "casement", "steel"] {
            let s = styled(&window(style));
            let out = vec3f(0.0, 0.0, 1.0);
            // a pane's middle: the upper sash's top-middle pane, a casement leaf's middle
            let at = match style {
                "casement" => vec3f(-0.3, 0.3, 0.0),
                "steel" => vec3f(0.0, 0.45, 0.0),
                _ => vec3f(0.0, 0.6, 0.0),
            };
            let m = s.trace(at, out, 0.0).unwrap();
            assert!(matches!(m.met, Met::Glass(g) if g == 1.0), "{style}: {m:?}");
            let edge = s.trace(vec3f(-0.59, 0.0, 0.0), out, 0.0).unwrap();
            assert!(matches!(edge.met, Met::Solid { paint: Paint::Frame, reveal: false, .. }), "{style}: {edge:?}");
            assert!(s.share > 0.4 && s.share < 0.95, "{style}: {}", s.share);
            assert!(s.grids_open().iter().all(|g| *g > 0.0));
        }
    }

    #[test]
    fn a_slanting_ray_meets_the_reveal_before_the_frame() {
        let s = styled(&window("sash"));
        // from the left edge of the opening, toward the left: it runs into the jamb at once
        let m = s.trace(vec3f(-0.55, 0.2, 0.0), vec3f(-0.7, 0.0, 0.7).normalize(), 0.0).unwrap();
        assert!(matches!(m.met, Met::Solid { reveal: true, paint: Paint::Wall, .. }), "{m:?}");
        assert!(m.normal.x > 0.9, "the jamb faces into the opening: {:?}", m.normal);
    }

    #[test]
    fn an_arch_cuts_the_opening_along_its_curve() {
        let p = RoomWindow { top: Some("round".to_string()), ..window("steel") };
        let s = styled(&p);
        // the opening's top corners are wall, its crown open
        assert!(s.trace(vec3f(-0.58, 0.88, 0.0), vec3f(0.0, 0.0, 1.0), 0.0).is_none());
        assert!(s.opens_at(vec2f(0.0, 0.85)));
        let flat = styled(&window("steel"));
        assert!(s.share < flat.share, "{} {}", s.share, flat.share);
    }

    #[test]
    fn a_door_is_solid_but_for_its_transom() {
        let p = RoomWindow { door: Some(true), width_m: 1.0, height_m: 2.4, transom_m: Some(0.3), ..window("grid") };
        let s = styled(&p);
        let out = vec3f(0.0, 0.0, 1.0);
        let leaf = s.trace(vec3f(0.0, -0.3, 0.0), out, 0.0).unwrap();
        assert!(matches!(leaf.met, Met::Solid { paint: Paint::Door, .. }), "{leaf:?}");
        let transom = s.trace(vec3f(-0.3, 1.05, 0.0), out, 0.0).unwrap();
        assert!(matches!(transom.met, Met::Glass(_)), "{transom:?}");
        assert!(s.share > 0.02 && s.share < 0.15, "{}", s.share);
        // a raised panel: the ray into a field goes deeper than the leaf's face
        let in_field = s.trace(vec3f(-0.2, -0.8, 0.0), out, 0.0).unwrap();
        let on_stile = s.trace(vec3f(-0.44, -0.8, 0.0), out, 0.0).unwrap();
        assert!(in_field.s > on_stile.s, "{in_field:?} {on_stile:?}");
        // the handle stands out of the leaf into the opening
        assert!(s.parts(vec3f(0.36, -0.22, -1.0), out, 10.0).is_some());
    }

    #[test]
    fn a_classical_surround_stands_out_of_the_wall() {
        let p = RoomWindow { surround: Some("classical".to_string()), ..window("sash") };
        let s = styled(&p);
        let into = vec3f(0.0, 0.0, 1.0);
        // the pilaster left of the opening, the cornice over it, the sill under it
        let (t, n, paint) = s.parts(vec3f(-0.65, 0.0, -1.0), into, 10.0).unwrap();
        assert!((t - (1.0 - 0.055)).abs() < 0.02 && n.z < -0.9 && paint == Paint::Surround, "{t} {n:?}");
        assert!(s.parts(vec3f(0.0, 0.95, -1.0), into, 10.0).is_some(), "the cornice");
        assert!(s.parts(vec3f(0.0, -0.93, -1.0), into, 10.0).is_some(), "the sill");
        // not in front of the opening
        assert!(s.parts(vec3f(0.0, 0.0, -1.0), into, 10.0).is_none());
        // from below, the head band's underside
        assert!(s.parts(vec3f(0.0, 0.5, -0.03), vec3f(0.0, 1.0, 0.0), 10.0).is_some_and(|(t, n, _)| n.y < -0.9 && (t - 0.4).abs() < 1.0e-3));
    }
}

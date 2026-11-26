//! The interactive gesture (E6 1.4): what the board canvas builds on the UI thread and sends
//! to the route worker. A sample's answer is a function of (committed board, gesture) only,
//! never of the previous frame (E6 0.5), so spring-back is free, a dropped sample changes
//! nothing and a recorded gesture replays.
//!
//! Cost on the UI thread: `push_trail` is O(1) amortised; a sample is a copy of the gesture
//! (its decimated trail and its moved list), written into the worker's latest-value slot.

use fab_elec_board::{Anchor, Placed};
use fab_elec_core::{Angle, BoardId, ItemId, LayerId, NetId, Nm, Path, P};
use std::sync::Arc;

/// Names one gesture from press to release; issued by the canvas.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub struct GestureId(pub u32);
fab_elec_core::canon_struct!(GestureId { 0 });

impl GestureId {
    /// The process-wide tag the host files this gesture's preview, commit and refusals under.
    /// The board pane issues gesture ids and preview tags from one counter.
    pub fn tag(self) -> u64 {
        u64::from(self.0)
    }
}


/// What the gesture does.
#[derive(Clone, PartialEq, Debug)]
pub enum GestureKind {
    /// Route a track from `Gesture::start` toward the cursor (the head, E6 3.1).
    Route,
    /// Draw both legs from the two terminals held at press, following the cursor trail.
    RoutePair { mate: NetId, mate_start: Anchor, mate_start_at: P },
    /// Drag segment `seg` of `track` (E6 3.3).
    DragSeg { track: ItemId, seg: u32 },
    /// Drag the vertex `vertex` of `track` (0 is the path's start).
    DragCorner { track: ItemId, vertex: u32 },
    /// Drag one via; its tracks on every layer follow. Its motion is `Gesture::moved`.
    DragVia(ItemId),
    /// Drag parts, vias or holes by `Gesture::moved`; attached tracks stretch (E6 3.3).
    DragParts,
    /// Bring a part from the tray: the candidate's footprint and pad nets, placed at the cursor
    /// with its own `rot` and `side`. The answer is its nearest legal place (the ghost).
    Place(Arc<Placed>),
    /// Refold these track vertices. Branching is measured on the circuit held at press.
    Tune { track: ItemId, from: usize, to: usize, circuit: Arc<fab_elec_circuit::Circuit> },
    /// Fan out the pads of a placed part.
    Fanout(ItemId),
}

/// How the head treats what is in its way (E6 3.1).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Mode {
    /// The taut path through the trail, ignoring obstacles; collisions are drawn.
    Mark,
    /// Fixed and shovable items are all obstacles.
    #[default]
    Walk,
    /// Shovable items are pushed; falls back to Walk, then Mark, for a blocked sample.
    Shove,
}

/// The embedder style (E6 7): per board, class or net, or a gesture's override.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Style {
    /// Octagonal hulls and octilinear chords (`Octi`).
    #[default]
    FortyFive,
    /// Round hulls, small bend radius.
    AnyAngle,
    /// Round hulls, bend radius of several widths, spread, teardrops and tapers.
    Organic,
}

/// Which way the head bends first where the trail does not decide: the free end after a jump
/// (E6 3.1: elsewhere the trail decides).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Posture {
    #[default]
    Auto,
    StraightFirst,
    DiagonalFirst,
}

/// The width a gesture lays.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum WidthRule {
    /// From the net's class and layer (the rule matrix).
    #[default]
    Rule,
    Fixed(Nm),
}

/// A rigid motion of one item: rotated by `rot` about the item's own reference point (a part's
/// `at`, a via's or hole's centre), then translated by `by`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Xform {
    pub by: P,
    pub rot: Angle,
}

impl Xform {
    pub const IDENTITY: Xform = Xform { by: P::ZERO, rot: Angle::ZERO };

    pub fn translate(by: P) -> Xform {
        Xform { by, rot: Angle::ZERO }
    }
    /// Where the point `p` of an item whose reference point is `origin` goes.
    pub fn apply(&self, origin: P, p: P) -> P {
        p.rotated_about(origin, self.rot).add(self.by)
    }
    pub fn is_identity(&self) -> bool {
        *self == Xform::IDENTITY
    }
}

/// A part of the head fixed by a click (E6 3.1): its solved path, and the via that ends it
/// when the layer key was pressed there.
#[derive(Clone, PartialEq, Debug)]
pub struct Leg {
    pub path: Path,
    pub layer: LayerId,
    pub w: Nm,
    /// A via at the leg's end to this layer; its size comes from the rules.
    pub via_to: Option<LayerId>,
}

/// The interactive state of one gesture: everything a sample's answer depends on besides the
/// committed board it was started against.
#[derive(Clone, PartialEq, Debug)]
pub struct Gesture {
    pub kind: GestureKind,
    pub unit: BoardId,
    /// The committed board generation the gesture is against.
    pub base_generation: u64,
    pub net: Option<NetId>,
    /// What was grabbed (a pad, via, track or `Free` for open canvas).
    pub start: Anchor,
    /// Where it was grabbed: the anchor's point, including the position a junction anchor
    /// carries on its host track's centreline (E6 1.2).
    pub start_at: P,
    /// Fixed by clicks in this gesture, in order.
    pub legs: Vec<Leg>,
    /// The decimated cursor path since the last fixed point: the head's homotopy reference.
    pub trail: Vec<P>,
    pub cursor: P,
    /// The anchor the canvas snapped the cursor to. The worker ends the head on it only after
    /// verifying it (its net, its layer, reachable); `None`: the head ends at the cursor.
    pub target: Option<Anchor>,
    pub posture: Posture,
    pub layer: LayerId,
    pub mode: Mode,
    pub style: Style,
    pub width: WidthRule,
    /// Moved items and their motion from the committed state (drags).
    pub moved: Vec<(ItemId, Xform)>,
    /// Finish (F while copper is drawn): the legs and the head as this sample lays them are the
    /// hand part, and the grid finder lays the rest of the net from where it ends, as the sample's
    /// second answer (`head::finish`). The canvas sends no further motion after it.
    pub finish: bool,
}

impl Gesture {
    /// A gesture of `kind` against board generation `base_generation`, grabbed at `start_at`.
    pub fn new(kind: GestureKind, unit: BoardId, base_generation: u64, start: Anchor, start_at: P, layer: LayerId) -> Gesture {
        Gesture {
            kind,
            unit,
            base_generation,
            net: None,
            start,
            start_at,
            legs: Vec::new(),
            trail: vec![start_at],
            cursor: start_at,
            target: None,
            posture: Posture::default(),
            layer,
            mode: Mode::default(),
            style: Style::default(),
            width: WidthRule::default(),
            moved: Vec::new(),
            finish: false,
        }
    }

    /// Move the cursor to `p` and add it to the trail by the fixed decimation rule: the point
    /// before it is dropped when it lies within `tol` of the chord of its neighbours. The trail
    /// grows without a cap and is cleared at each click.
    pub fn push_trail(&mut self, p: P, tol: Nm) {
        self.cursor = p;
        if self.trail.last() == Some(&p) {
            return;
        }
        self.trail.push(p);
        let n = self.trail.len();
        if n >= 3 && near_chord(self.trail[n - 3], self.trail[n - 2], self.trail[n - 1], tol) {
            self.trail.remove(n - 2);
        }
    }

    /// Fix `leg` (a click): the trail restarts at the leg's end.
    pub fn click(&mut self, leg: Leg) {
        let end = leg.path.end();
        if let Some(l) = leg.via_to {
            self.layer = l;
        }
        self.legs.push(leg);
        self.trail.clear();
        self.trail.push(end);
    }

    /// The point the current head starts from.
    pub fn head_start(&self) -> P {
        self.legs.last().map(|l| l.path.end()).unwrap_or(self.start_at)
    }
}

/// `m` lies within `tol` of the segment `a`-`b` (exact integer test).
fn near_chord(a: P, m: P, b: P, tol: Nm) -> bool {
    let (d, am) = (b.sub(a), m.sub(a));
    let len2 = d.len2() as i128;
    let t = am.dot(d) as i128;
    let tol2 = tol as i128 * tol as i128;
    if len2 == 0 || t <= 0 {
        return am.len2() as i128 <= tol2;
    }
    if t >= len2 {
        return m.sub(b).len2() as i128 <= tol2;
    }
    let cr = d.cross(am) as i128;
    cr * cr <= tol2 * len2
}

/// One sample of a gesture, as written into the route worker's latest-value slot. `seq` grows
/// per sample within a gesture; an answer names the `seq` it was computed for, so the canvas
/// knows how stale it is (E6 3.4: older answers are drawn rubber-stretched with a stale tint).
#[derive(Clone, PartialEq, Debug)]
pub struct GestureSample {
    pub id: GestureId,
    pub seq: u64,
    pub gesture: Gesture,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trail_decimates_straight_runs_and_keeps_corners() {
        let mut g = Gesture::new(GestureKind::Route, BoardId(0), 1, Anchor::Free, P::new(0, 0), LayerId::F_CU);
        for x in 1..=100 {
            g.push_trail(P::new(x * 1000, (x % 2) * 10), 50);
        }
        assert_eq!(g.trail, vec![P::new(0, 0), P::new(100_000, 0)], "a straight wobble within tolerance collapses to its ends");
        for y in 1..=50 {
            g.push_trail(P::new(100_000, y * 1000), 50);
        }
        assert_eq!(g.trail, vec![P::new(0, 0), P::new(100_000, 0), P::new(100_000, 50_000)], "the corner stays");
        assert_eq!(g.cursor, P::new(100_000, 50_000));
        let x = Xform { by: P::new(10, 0), rot: Angle::QUARTER };
        assert_eq!(x.apply(P::new(5, 5), P::new(6, 5)), P::new(15, 6), "rotate about the origin, then translate");
        g.click(Leg { path: Path::new(P::new(0, 0)).line(P::new(100_000, 50_000)), layer: LayerId::F_CU, w: 200_000, via_to: Some(LayerId::B_CU) });
        assert_eq!((g.trail.clone(), g.layer, g.head_start()), (vec![P::new(100_000, 50_000)], LayerId::B_CU, P::new(100_000, 50_000)), "a click restarts the trail at the leg's end on the via's layer");
    }
}

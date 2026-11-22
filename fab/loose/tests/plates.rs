//! Plates built from boxes: an open arch whose supports walk out, and a closed cup with a narrow
//! neck whose supports don't. The plan comes from the generator, as printing's does. And a spool,
//! whose ring of support is laid out here.

use fab_loose::bodies::{piece_body, with_field};
use fab_loose::grid::{Contact, DistGrid};
use fab_loose::pieces::{coarse, pieces_of, Ball, Piece, Section};
use fab_loose::solid::Solid;
use fab_loose::verdict::{check_piece, Light, Verdict};
use makepad_csg_math::Vec3d;
use makepad_csg_mech::geom::{add, len, rotate, sub};
use makepad_csg_mech::mass::MassProps;
use makepad_csg_mech::sim::{AnchorKind, AnchorSpec, Assembly, BodySpec, Friction, MechWorld, GRAVITY};
use makepad_csg_sdf::Sdf3;
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_toolpath::par::Ctl;
use makepad_fab_toolpath::skeleton::{GivenUp, Skeleton, Slab};
use makepad_fab_toolpath::support::{generate_recording, SupportParams};
use std::sync::Arc;

const H: f64 = 0.2;
/// Where the plan's frame sits on the plate.
const AT: [f64; 2] = [100.0, 60.0];

#[derive(Clone, Copy)]
struct Block {
    lo: [f64; 3],
    hi: [f64; 3],
}

fn block(lo: [f64; 3], hi: [f64; 3]) -> Block {
    Block { lo, hi }
}

impl Block {
    fn distance(&self, p: [f64; 3]) -> f64 {
        let q: [f64; 3] = std::array::from_fn(|k| (p[k] - (self.lo[k] + self.hi[k]) / 2.0).abs() - (self.hi[k] - self.lo[k]) / 2.0);
        let out = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2) + q[2].max(0.0).powi(2)).sqrt();
        out + q[0].max(q[1]).max(q[2]).min(0.0)
    }
    fn at_layer(&self, z: f64) -> Option<ExPolygon> {
        (self.lo[2] <= z && z < self.hi[2]).then(|| ExPolygon::new(Polygon::new(vec![Pt::from_mm(self.lo[0], self.lo[1]), Pt::from_mm(self.hi[0], self.lo[1]), Pt::from_mm(self.hi[0], self.hi[1]), Pt::from_mm(self.lo[0], self.hi[1])])))
    }
}

/// Solids less hollows, in the plan's frame. Hollows that make one space overlap: where two just
/// touch, the field is zero, a wall no thicker than that.
struct Shape {
    solids: Vec<Block>,
    hollows: Vec<Block>,
}

impl Sdf3 for Shape {
    fn distance(&self, p: Vec3d) -> f64 {
        let p = [p.x - AT[0], p.y - AT[1], p.z];
        let solid = self.solids.iter().map(|b| b.distance(p)).fold(f64::MAX, f64::min);
        let hollow = self.hollows.iter().map(|b| b.distance(p)).fold(f64::MAX, f64::min);
        solid.max(-hollow)
    }
}

/// An arch open front and back: two legs and a span.
fn arch() -> Shape {
    Shape { solids: vec![block([0.0, 0.0, 0.0], [5.0, 20.0, 15.0]), block([25.0, 0.0, 0.0], [30.0, 20.0, 15.0]), block([0.0, 0.0, 15.0], [30.0, 20.0, 20.0])], hollows: Vec::new() }
}

/// A closed box with a 4 mm neck through its lid and a shelf inside.
fn cup() -> Shape {
    Shape {
        solids: vec![block([0.0, 0.0, 0.0], [24.0, 24.0, 24.0]), block([2.0, 2.0, 12.0], [12.0, 22.0, 14.0])],
        hollows: vec![block([2.0, 2.0, 2.0], [22.0, 22.0, 12.0]), block([12.0, 2.0, 11.0], [22.0, 22.0, 15.0]), block([2.0, 2.0, 14.0], [22.0, 22.0, 22.0]), block([10.0, 10.0, 21.0], [14.0, 14.0, 25.0])],
    }
}

fn top(shape: &Shape) -> f64 {
    shape.solids.iter().map(|b| b.hi[2]).fold(0.0, f64::max)
}

/// The supports the generator grows under `shape`.
fn plan(shape: &Shape) -> Skeleton {
    let n = (top(shape) / H).round() as usize;
    let heights: Vec<(f64, f64)> = (0..n).map(|l| ((l + 1) as f64 * H, H)).collect();
    let slices: Vec<Vec<ExPolygon>> = (0..n).map(|l| {
        let z = (l as f64 + 0.5) * H;
        let solid: Vec<ExPolygon> = shape.solids.iter().filter_map(|b| b.at_layer(z)).collect();
        let hollow: Vec<ExPolygon> = shape.hollows.iter().filter_map(|b| b.at_layer(z)).collect();
        difference_ex(&union_ex(&solid), &union_ex(&hollow))
    }).collect();
    let p = SupportParams { threshold_deg: 30.0, xy_gap: 0.35, top_gap: H, bottom_gap: H, interface_top_layers: 2, line_width: 0.4, wall_width: 0.4, tip_diameter: 0.8, branch_diameter: 5.0, taper_deg: 5.0, angle_max_deg: 40.0, angle_slow_deg: 25.0, build_plate_only: false, remove_small: true, first_expansion: 2.0, stand_inside: true };
    let mut skeleton = Skeleton::default();
    let layers = generate_recording(Ctl::default(), &slices, &heights, &p, &mut skeleton).unwrap().layers;
    skeleton.finish_tree(&heights, layers.into_iter().map(|l| l.interface).collect());
    skeleton
}

struct Plate {
    solid: Solid,
    grid: Arc<DistGrid>,
    pieces: Vec<Piece>,
    /// What the plan tells is locked in the part, where it lets its supports be.
    unremovable: Vec<GivenUp>,
}

fn plate(shape: Shape) -> Plate {
    let skeleton = plan(&shape);
    let (lo, hi) = (
        std::array::from_fn(|k| shape.solids.iter().map(|b| b.lo[k]).fold(f64::MAX, f64::min) + if k < 2 { AT[k] } else { 0.0 }),
        std::array::from_fn(|k| shape.solids.iter().map(|b| b.hi[k]).fold(f64::MIN, f64::max) + if k < 2 { AT[k] } else { 0.0 }),
    );
    let solid = Solid::of_distance(Arc::new(shape), lo, hi);
    let grid = Arc::new(DistGrid::check(Ctl::default(), &solid, 0.2, Contact::Fast).unwrap());
    let mut pieces = pieces_of("part", &skeleton, AT, &grid);
    coarse(&mut pieces.iter_mut().collect::<Vec<_>>());
    assert!(!pieces.is_empty(), "the shape needs support");
    Plate { solid, grid, pieces, unremovable: skeleton.unremovable }
}

/// A spool standing on a flange, its middle at `AT`: flanges of 10 mm radius, 3 mm thick, at the
/// bed and at 15 mm, on a stem of 3 mm.
struct Spool;

impl Sdf3 for Spool {
    fn distance(&self, p: Vec3d) -> f64 {
        let r = (p.x - AT[0]).hypot(p.y - AT[1]);
        let cylinder = |radius: f64, lo: f64, hi: f64| {
            let q = [r - radius, (p.z - (lo + hi) / 2.0).abs() - (hi - lo) / 2.0];
            q[0].max(0.0).hypot(q[1].max(0.0)) + q[0].max(q[1]).min(0.0)
        };
        cylinder(10.0, 0.0, 3.0).min(cylinder(3.0, 3.0, 15.0)).min(cylinder(10.0, 15.0, 18.0))
    }
}

/// Normal support under the spool's upper flange, a layer at a time from the bed: a ring round
/// the stem out past the flanges' rims, kept off the part by its gaps and parted into quarters by
/// walls through the axis both ways. Below the lower flange's top only its rim's ring stands.
fn spool_slabs() -> Vec<Slab> {
    let ring = |r: f64| ExPolygon::new(Polygon::new((0..96).map(|i| (i as f64 * std::f64::consts::TAU / 96.0).sin_cos()).map(|(s, c)| Pt::from_mm(r * c, r * s)).collect()));
    let quarter = |sx: f64, sy: f64| {
        let (x, y) = ([0.5 * sx, 13.0 * sx], [0.5 * sy, 13.0 * sy]);
        let (x0, x1, y0, y1) = (x[0].min(x[1]), x[0].max(x[1]), y[0].min(y[1]), y[0].max(y[1]));
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    };
    let quarters: Vec<ExPolygon> = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)].iter().map(|&(sx, sy)| quarter(sx, sy)).collect();
    (0..74).map(|l| {
        let z = (l + 1) as f64 * H;
        let inner = if z - H / 2.0 < 3.0 + H { 10.35 } else { 3.35 };
        let region = difference_ex(&intersection_ex(&[ring(12.5)], &quarters), &[ring(inner)]);
        Slab { z, height: H, region }
    }).collect()
}

/// The four quarters of the ring under a spool's flange are mirror images of each other, and
/// each comes out alike, whichever way its balls fall on it: the search moves a spread of them,
/// and what passes between those doesn't stop every try.
#[test]
fn a_spools_four_quarter_blocks_all_come_out() {
    let solid = Solid::of_distance(Arc::new(Spool), [AT[0] - 10.0, AT[1] - 10.0, 0.0], [AT[0] + 10.0, AT[1] + 10.0, 18.0]);
    let grid = DistGrid::check(Ctl::default(), &solid, 0.2, Contact::Fast).unwrap();
    let skeleton = Skeleton { slabs: spool_slabs(), ..Default::default() };
    let pieces = pieces_of("spool", &skeleton, AT, &grid);
    assert_eq!(pieces.len(), 4, "a block a quarter");
    for piece in &pieces {
        let v = check_piece(piece, &grid, &solid, &|| false).unwrap();
        let balls = piece.fine_balls();
        let middle: [f32; 2] = std::array::from_fn(|k| balls.iter().map(|b| b.c[k]).sum::<f32>() / balls.len() as f32 - AT[k] as f32);
        assert_eq!(v.verdict, Verdict::Free, "the quarter at {middle:?} from the axis, of {} balls: {v:?}", balls.len());
    }
}

/// Nothing starts in contact: every ball the sim and the check use stands clear of the part.
fn all_clear(p: &Plate) {
    for piece in &p.pieces {
        assert!(!piece.mesh.indices.is_empty(), "{} has nothing to show", piece.name);
        for b in piece.balls.iter().chain(piece.sections.iter().flat_map(|s| &s.balls)) {
            let room = p.grid.distance(b.c) - b.r;
            assert!(room > 0.0, "{}: a ball of {} at {:?} is {room} off the part", piece.name, b.r, b.c);
            assert!(b.c[2] >= b.r, "{}: a ball under the bed", piece.name);
        }
    }
}

#[test]
fn an_open_arch_gives_up_its_supports() {
    let p = plate(arch());
    all_clear(&p);
    for piece in &p.pieces {
        let v = check_piece(piece, &p.grid, &p.solid, &|| false).unwrap();
        assert_eq!(v.verdict, Verdict::Free, "{}: {v:?}", piece.name);
        assert_eq!(v.verdict.colour(), Light::Green);
        assert!(!v.turned, "{} goes straight out", piece.name);
    }
}

#[test]
fn a_closed_cup_keeps_its_supports() {
    let p = plate(cup());
    all_clear(&p);
    let verdicts: Vec<_> = p.pieces.iter().map(|piece| check_piece(piece, &p.grid, &p.solid, &|| false).unwrap()).collect();
    assert!(verdicts.iter().all(|v| v.verdict != Verdict::Free), "{verdicts:?}");
    assert!(verdicts.iter().any(|v| v.verdict == Verdict::Trapped), "the pad under the lid can't pass the neck: {verdicts:?}");
}

/// A shelf on a wall whose face steps out towards the bed, 1.2 mm every 2 mm, with T-slots up its
/// face: a trunk coming down in front of it meets the face as it goes. Cut by the clearance there
/// and kept as wide as it was, it would fill the slots, and nothing could pull it out of them.
/// Supports may stand inside the part here, so what holds the shelf over a slot's head from inside
/// it stays in, and the plan tells so; nothing else may.
#[test]
fn trunks_beside_a_slotted_wall_come_out() {
    let mut shape = Shape { solids: vec![block([0.0, 0.0, 20.0], [30.0, 16.0, 22.0])], hollows: Vec::new() };
    for k in 0..10 {
        let (face, z) = (6.0 + 0.6 * k as f64, 2.0 * k as f64);
        shape.solids.push(block([0.0, face, z], [30.0, 16.0, z + 2.0]));
        for c in [5.0, 10.0, 15.0, 20.0, 25.0] {
            shape.hollows.push(block([c - 0.6, face - 0.1, z - 0.01], [c + 0.6, face + 1.5, z + 2.01]));
            shape.hollows.push(block([c - 1.6, face + 1.5, z - 0.01], [c + 1.6, face + 3.0, z + 2.01]));
        }
    }
    let p = plate(shape);
    all_clear(&p);
    // A piece is told when each of its balls lies in the box of some layer's unremovable support.
    let told = |piece: &Piece| piece.sections.iter().flat_map(|s| &s.balls).all(|b| p.unremovable.iter().any(|g| {
        let (x, y, r) = (b.c[0] as f64 - AT[0], b.c[1] as f64 - AT[1], b.r as f64);
        g.bounds.min.x_mm() - r <= x && x <= g.bounds.max.x_mm() + r && g.bounds.min.y_mm() - r <= y && y <= g.bounds.max.y_mm() + r
    }));
    let trapped: Vec<String> = p.pieces.iter().filter_map(|piece| {
        let v = check_piece(piece, &p.grid, &p.solid, &|| false).unwrap();
        (v.verdict == Verdict::Trapped && !told(piece)).then(|| format!("{}: {v:?}", piece.name))
    }).collect();
    assert!(trapped.is_empty(), "{} of {} pieces have no way out and are not told so: {trapped:?}", trapped.len(), p.pieces.len());
}

/// A wide low room with a mouth in the middle of its roof, and a column in a corner that stands
/// nearly floor to roof. It has to slide away from the walls nearest it to where it can rise.
#[test]
fn a_column_under_a_low_roof_slides_to_the_mouth() {
    let room = Shape {
        solids: vec![block([0.0, 0.0, 0.0], [30.0, 30.0, 12.0])],
        hollows: vec![block([2.0, 2.0, 2.0], [28.0, 28.0, 10.0]), block([11.0, 11.0, 9.0], [19.0, 19.0, 13.0])],
    };
    let solid = Solid::of_distance(Arc::new(room), [AT[0], AT[1], 0.0], [AT[0] + 30.0, AT[1] + 30.0, 12.0]);
    let grid = DistGrid::check(Ctl::default(), &solid, 0.2, Contact::Fast).unwrap();
    let balls: Vec<Ball> = (0..14).map(|k| Ball { c: [AT[0] as f32 + 5.0, AT[1] as f32 + 5.0, 2.7 + k as f32 * 0.5], r: 0.5 }).collect();
    let piece = Piece { name: "column".into(), sections: vec![Section { balls }], ..Default::default() };
    let v = check_piece(&piece, &grid, &solid, &|| false).unwrap();
    assert_ne!(v.verdict, Verdict::Trapped, "{v:?}");
    assert!(!v.turned, "it stays upright: {v:?}");
}

fn world(p: &Plate) -> MechWorld {
    let size: [f64; 3] = std::array::from_fn(|k| p.solid.hi[k] - p.solid.lo[k]);
    let com: [f64; 3] = std::array::from_fn(|k| (p.solid.hi[k] + p.solid.lo[k]) / 2.0);
    let (m, i) = (10.0, 10.0 * size[0] * size[0] / 6.0);
    let part = BodySpec { name: "part".into(), mass: MassProps { volume: 1.0, mass: m, com, inertia: [[i, 0.0, 0.0], [0.0, i, 0.0], [0.0, 0.0, i]] }, hulls: Vec::new(), tubes: Vec::new(), balls: Vec::new(), rods: Vec::new(), field: None };
    let mut bodies = vec![with_field(part, &p.grid)];
    bodies.extend(p.pieces.iter().map(|piece| piece_body(piece, 0.00124)));
    let asm = Assembly { bodies, anchors: vec![AnchorSpec { body: 0, pivot: com, kind: AnchorKind::Fixed, friction: Friction::default() }], up: [0.0, 0.0, 1.0], contact_margin: 1.5, max_speed: 2000.0, ..Default::default() };
    MechWorld::new(&asm, [0.0, 0.0, -GRAVITY], Some(-0.05))
}

const DT: f64 = 1.0 / 240.0;

/// Lifted off the bed, the arch leaves its piece standing there, free to pull away.
#[test]
fn an_arch_lifts_off_its_pieces() {
    let p = plate(arch());
    let mut w = world(&p);
    let i = (0..p.pieces.len()).max_by_key(|&i| p.pieces[i].balls.len()).unwrap() + 1;
    let (at, part) = (w.poses()[i].com, w.poses()[0].com);
    lift(&mut w, 25.0);
    assert!(w.poses()[0].com[2] > part[2] + 24.0, "the part rose");
    let now = w.poses()[i].com;
    assert!(now[2] < at[2] + 1.0, "the piece stayed behind: {at:?} {now:?}");
    w.grab(i, now, 8.0);
    for k in 0..480 {
        w.drag([now[0], now[1] + 40.0 * k as f64 / 480.0, now[2]], DT);
        w.step(DT);
    }
    let out = w.poses()[i].com;
    assert!(out[1] > p.solid.hi[1], "pulled clear: {out:?}");
}

/// Lifted, the cup carries its pieces with it.
#[test]
fn a_cup_lifts_with_its_pieces() {
    let p = plate(cup());
    let mut w = world(&p);
    let at: Vec<f64> = w.poses().iter().map(|q| q.com[2]).collect();
    lift(&mut w, 25.0);
    for (i, q) in w.poses().iter().enumerate() {
        assert!(q.com[2] > at[i] + 20.0, "body {i} stayed behind: {} from {}", q.com[2], at[i]);
    }
}

fn lift(w: &mut MechWorld, by: f64) {
    let steps = 480;
    for k in 0..=steps {
        w.move_ground([0.0, 0.0, by * k as f64 / steps as f64], [0.0, 0.0, 1.0], 0.0, DT);
        w.step(DT);
    }
    w.move_ground([0.0, 0.0, by], [0.0, 0.0, 1.0], 0.0, DT);
    w.step(DT);
}

/// A hand shaking the cup hard (5 Hz, +-50 mm, peaks near 1.6 m/s, its frames at 90 Hz, turning it too):
/// the cup keeps up, its pieces rattle, and they don't leave through the walls: a step of a piece
/// against the cup can be several times the walls' 2 mm.
#[test]
fn a_cup_shaken_by_hand_keeps_its_pieces() {
    let p = plate(cup());
    let mut w = world(&p);
    for _ in 0..120 {
        w.step(DT);
    }
    let up = [0.0, 0.0, 1.0];
    // about the cup's own middle
    let about = [(p.solid.lo[0] + p.solid.hi[0]) / 2.0, (p.solid.lo[1] + p.solid.hi[1]) / 2.0, 0.0];
    let (mut hand, mut lag) = (([0.0; 3], up, 0.0), 0f64);
    for step in 0..960 {
        if step % 3 == 0 {
            let t = step as f64 * DT;
            let (swing, ang) = ((std::f64::consts::TAU * 5.0 * t).sin(), 0.6 * (std::f64::consts::TAU * 2.0 * t).sin());
            let (s, c) = ang.sin_cos();
            let turned = [about[0] * c - about[1] * s, about[0] * s + about[1] * c];
            hand = ([50.0 * swing + about[0] - turned[0], about[1] - turned[1], 40.0 + 40.0 * (std::f64::consts::TAU * 2.0 * t).sin()], up, ang);
        }
        w.carry_ground(hand, ([0.0; 3], up, 0.0), 1.0 / 90.0, DT);
        w.step(DT);
        let (now, there) = (w.poses()[0].com, add(hand.0, rotate(w.rest_com(0), up, hand.2)));
        lag = lag.max(len(sub(now, there)));
    }
    assert!(lag < 40.0, "the cup trails the hand by up to {lag} mm");
    let poses = w.poses();
    for (i, q) in poses.iter().enumerate().skip(1) {
        let local = in_part(&poses[0], w.rest_com(0), q.com);
        for k in 0..3 {
            assert!(local[k] > p.solid.lo[k] && local[k] < p.solid.hi[k], "piece {i} left the cup: {local:?}, the cup {:?} to {:?}", p.solid.lo, p.solid.hi);
        }
    }
}

/// A point of the world in the part's rest frame.
fn in_part(part: &makepad_csg_mech::sim::Pose, rest: [f64; 3], at: [f64; 3]) -> [f64; 3] {
    let d = sub(at, part.com);
    let r = part.rot;
    // v' = v + 2 u x (u x v + s v), with the rotation's inverse
    let (u, s) = ([-r[0], -r[1], -r[2]], r[3]);
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let t = cross(u, d);
    let t = cross(u, [t[0] + s * d[0], t[1] + s * d[1], t[2] + s * d[2]]);
    std::array::from_fn(|k| rest[k] + d[k] + 2.0 * t[k])
}

/// The hard case for the shake: a flat pad all of whose balls reach a thin wall together, in a
/// closed box, each step of it against the wall longer than the wall is thick.
#[test]
fn a_flat_pad_shaken_in_a_thin_box_stays_in() {
    let thin = Shape { solids: vec![block([0.0, 0.0, 0.0], [40.0, 40.0, 40.0])], hollows: vec![block([1.0, 1.0, 1.0], [39.0, 39.0, 39.0])] };
    let at = [AT[0], AT[1], 0.0];
    let lo: [f64; 3] = std::array::from_fn(|k| thin.solids[0].lo[k] + at[k]);
    let hi: [f64; 3] = std::array::from_fn(|k| thin.solids[0].hi[k] + at[k]);
    let solid = Solid::of_distance(Arc::new(thin), lo, hi);
    let grid = Arc::new(DistGrid::check(Ctl::default(), &solid, 0.2, Contact::Fast).unwrap());
    let balls: Vec<Ball> = (0..4).flat_map(|i| (0..4).map(move |j| Ball { c: [(at[0] + 14.0 + 4.0 * i as f64) as f32, (at[1] + 14.0 + 4.0 * j as f64) as f32, 20.0], r: 1.0 })).collect();
    let piece = Piece { name: "pad".into(), balls, volume: 50.0, ..Default::default() };
    let com = [at[0] + 20.0, at[1] + 20.0, 20.0];
    let part = BodySpec { name: "part".into(), mass: MassProps { volume: 100.0, mass: 10.0, com, inertia: [[1000.0, 0.0, 0.0], [0.0, 1000.0, 0.0], [0.0, 0.0, 1000.0]] }, hulls: Vec::new(), tubes: Vec::new(), balls: Vec::new(), rods: Vec::new(), field: None };
    let bodies = vec![with_field(part, &grid), piece_body(&piece, 0.00124)];
    let asm = Assembly { bodies, anchors: vec![AnchorSpec { body: 0, pivot: com, kind: AnchorKind::Fixed, friction: Friction::default() }], up: [0.0, 0.0, 1.0], contact_margin: 1.5, max_speed: 2000.0, ..Default::default() };
    let mut w = MechWorld::new(&asm, [0.0, 0.0, -GRAVITY], None);
    let up = [0.0, 0.0, 1.0];
    let mut hand = [0.0; 3];
    for step in 0..2400 {
        if step % 3 == 0 {
            let t = step as f64 * DT;
            // along each axis in turn, so the flat side goes at a wall
            hand = [0.0, 0.0, 50.0 * (std::f64::consts::TAU * 5.0 * t).sin()];
            if step > 1200 {
                hand = [50.0 * (std::f64::consts::TAU * 5.0 * t).sin(), 0.0, 0.0];
            }
        }
        w.carry_ground((hand, up, 0.0), ([0.0; 3], up, 0.0), 1.0 / 90.0, DT);
        w.step(DT);
        let poses = w.poses();
        let local = in_part(&poses[0], w.rest_com(0), poses[1].com);
        for k in 0..3 {
            assert!(local[k] > lo[k] && local[k] < hi[k], "the pad went through the wall at step {step}: {local:?}");
        }
    }
}

/// A piece inside the cup pulled straight sideways stays inside its walls.
#[test]
fn a_cup_piece_stays_in_the_sim() {
    let p = plate(cup());
    let mut w = world(&p);
    for _ in 0..120 {
        w.step(DT);
    }
    let i = (0..p.pieces.len()).max_by_key(|&i| p.pieces[i].balls.len()).unwrap() + 1;
    let at = w.poses()[i].com;
    w.grab(i, at, 8.0);
    for k in 0..480 {
        w.drag([at[0] + 60.0 * k as f64 / 480.0, at[1], at[2]], DT);
        w.step(DT);
    }
    let held = w.poses()[i].com;
    assert!(held[0] < p.solid.hi[0], "the wall stopped it: {held:?}");
}

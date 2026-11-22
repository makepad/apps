//! Supports: the solids' distances against brute force, the clearance march against known
//! gaps (a thin wall included) from a field and a mesh alike, and supports built on parts:
//! every point held or said why not, every strut clear of the parts, the bridges falling
//! steeply enough, the pad under every base; their layers drawn as a field draws, matching their
//! own mesh's.

use crate::clear::{clearance, Clear, Probe};
use crate::grid::{Layers, PixelGrid, PixelRect};
use crate::image::LayerStack;
use crate::load::Load;
use crate::points::{support_points, PointKind, PointSettings, SupportPoint};
use crate::printer::simulated;
use crate::parts::{cured, links};
use crate::section::{Box3, MeshSection, Near, Section};
use crate::solid::{add, scale, Pad, Solid, V3};
use crate::rules::{check, Rule};
use crate::supports::{pad_cover, pads, supported, Dropped, Head, SupportSection, SupportSettings, Supports};
use crate::tests::{field, prism, rect, sphere_mesh};
use makepad_csg_sdf::{SdfNode, SdfOp};
use std::sync::Arc;

fn len(a: V3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// A spread of points around `c`, `r` across, from a fixed seed.
fn scatter(c: V3, r: f64, n: usize) -> Vec<V3> {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    (0..n).map(|_| [c[0] + r * next(), c[1] + r * next(), c[2] + r * next()]).collect()
}

/// The signed distance to a polygon (counter-clockwise) in the plane.
fn polygon_distance(poly: &[[f64; 2]], p: [f64; 2]) -> f64 {
    let mut d = f64::INFINITY;
    let mut inside = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        d = d.min((p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy));
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * dx {
            inside = !inside;
        }
    }
    if inside { -d } else { d }
}

/// A head's hull of two balls and a cut cone: outside, their distance is the nearest ball's on
/// the line between (all the balls between, sampled finely); a cut cone's is its profile's in
/// the plane through its axis. Inside, both are below 0.
#[test]
fn the_solids_distances_are_exact() {
    let round = Solid::Round { a: [1.0, 2.0, 3.0], ra: 0.2, b: [1.5, 2.2, 1.4], rb: 0.5 };
    let cut = Solid::Cut { a: [4.0, 4.0, 2.0], ra: 2.0, b: [4.0, 4.0, 3.0], rb: 0.5 };
    for p in scatter([2.0, 3.0, 2.0], 4.0, 400) {
        let Solid::Round { a, ra, b, rb } = round else { unreachable!() };
        let brute = (0..=4000).map(|k| k as f64 / 4000.0).map(|t| len([p[0] - a[0] - t * (b[0] - a[0]), p[1] - a[1] - t * (b[1] - a[1]), p[2] - a[2] - t * (b[2] - a[2])]) - (ra + t * (rb - ra))).fold(f64::INFINITY, f64::min);
        let d = round.distance(p);
        if brute > 0.0 {
            assert!((d - brute).abs() < 2e-3, "round at {p:?}: {d} against {brute}");
        } else {
            assert!(d <= 1e-9, "round at {p:?}: {d} inside");
        }
        // the cut cone: radial and axial in its frame, against its profile
        let (r, y) = ((p[0] - 4.0).hypot(p[1] - 4.0), p[2] - 2.0);
        let brute = polygon_distance(&[[-2.0, 0.0], [2.0, 0.0], [0.5, 1.0], [-0.5, 1.0]], [r, y]);
        assert!((cut.distance(p) - brute).abs() < 1e-9, "cut at {p:?}: {} against {brute}", cut.distance(p));
    }
    // the bounds hold them
    for s in [round, cut] {
        let (lo, hi) = s.bounds();
        for p in scatter([2.0, 3.0, 2.0], 5.0, 400) {
            if (0..3).any(|k| p[k] < lo[k] || p[k] > hi[k]) {
                assert!(s.distance(p) > 0.0, "{s:?} at {p:?}");
            }
        }
    }
}

/// The area inside an outline (counter-clockwise).
fn ring_area(r: &[[f64; 2]]) -> f64 {
    0.5 * (0..r.len()).map(|i| r[i][0] * r[(i + 1) % r.len()][1] - r[(i + 1) % r.len()][0] * r[i][1]).sum::<f64>()
}

/// The pad follows its bases (R7 §4.2, the leaner join): a grid of them stands on one slab, the
/// gaps between bases closed by the brim's arcs and the feet's box grown round; a ring of them on
/// a pad with no hole; two clusters apart joined by one strip between their nearest bases, as
/// wide as the lighter side's peel needs and never under the reference's spoke, the space a hull
/// would fill left open; a cluster out of reach on its own. Its edge draws in by the chamfer from
/// its top to the plate; its distance is its faces' near them and never more than the truth; its
/// mesh cuts as its outline does.
#[test]
fn the_pad_follows_its_bases() {
    let s = SupportSettings::default();
    let load = Load::default();
    let screen = PixelGrid::of(&simulated().screen);
    let grow = s.base_radius + s.brim;
    let grid = |x: f64, y: f64, hang: f64| (0..3).flat_map(move |i| (0..2).map(move |j| ([x + 5.0 * i as f64, y + 5.0 * j as f64], hang))).collect::<Vec<_>>();
    let feet = grid(20.0, 20.0, 40.0);
    let one = pads(&feet, &s, &load, &screen);
    assert_eq!(one.len(), 1);
    let pad = &one[0];
    // its edge the brim past the bases, the gaps between them closed by webs: between two feet
    // 5 mm apart, the arc of the brim that touches both bases grown by twice it
    assert!(feet.iter().all(|f| pad.outline_distance(f.0) <= -grow + 0.01), "every base on it");
    assert!(pad.outline_distance([25.0, 25.0 + grow]).abs() < 0.01, "its edge the brim past the bases");
    let web = ((s.base_radius + 2.0 * s.brim).powi(2) - 2.5f64.powi(2)).sqrt() - s.brim;
    assert!((pad.outline_distance([22.5, 20.0]) + web).abs() < 0.02, "the web between two bases: {} against {web}", -pad.outline_distance([22.5, 20.0]));
    assert!(pad.outline_distance([22.5, 22.5]) < -web, "the gap between four closed");
    // its area: the feet's box grown round, less the six webs' scallops along its sides (each
    // between the line over two bases and the brim's arc that touches both, R = base + brim)
    let (r, m, half) = (grow, s.brim, 2.5f64);
    let h = ((r + m).powi(2) - half * half).sqrt();
    let xt = half * r / (r + m);
    let disc = |a: f64, r: f64| 0.5 * (a * (r * r - a * a).sqrt() + r * r * (a / r).asin());
    let ut = half - xt;
    let scallop = 2.0 * (r * xt - disc(xt, r)) + 2.0 * ut * (r - h) + 2.0 * disc(ut, m);
    let want = 10.0 * 5.0 + 2.0 * (10.0 + 5.0) * grow + std::f64::consts::PI * grow * grow - 6.0 * scallop;
    let got = ring_area(&pad.ring);
    assert!((got - want).abs() < 0.003 * want, "the slab: {got} against {want}");
    // its distance: the middle half its thickness in, above it, and out from its chamfer
    let near = |a: f64, b: f64| (a - b).abs() < 0.01;
    assert!(near(pad.distance([25.0, 22.5, 0.5]), -0.5) && near(pad.distance([25.0, 22.5, 2.5]), 1.5));
    let c = s.pad / s.pad_slope.tan();
    let (y, z) = (25.0 + grow - 0.5 * c, 0.5);
    let n = [1.0 / (1.0 + c * c).sqrt(), -c / (1.0 + c * c).sqrt()];
    assert!(near(pad.distance([25.0, y + 0.3 * n[0], z + 0.3 * n[1]]), 0.3), "out from the chamfer");
    // never more than the truth: the corner of its top a millimetre beside it
    assert!(pad.distance([25.0, 25.0 + grow + 1.0, 1.0]) <= 1.0 + 1e-9);
    // the chamfer: its bottom drawn in by the inset all round
    let bottom = pad.outline_at(pad.z[0]);
    assert!(bottom.iter().all(|q| (pad.outline_distance(*q) + c).abs() < 0.01), "the bottom inset by {c}");
    // its mesh cuts as its outline does
    let mut m = fab_plate::PartMesh { positions: Vec::new(), triangles: Vec::new() };
    pad.mesh(&mut m);
    let p = simulated();
    let cut = LayerStack::new(vec![Arc::new(MeshSection::new(&m)) as Arc<dyn Section>], &p);
    let k = cut.layers.at(0.5);
    let (got, want) = (cut.layer(k).area(&cut.grid), ring_area(&pad.outline_at(cut.layers.z(k))));
    assert!((got - want).abs() < 0.005 * want, "the mesh's cut {got} against its outline's {want}");
    // a ring of bases (under a tube): one pad, solid inside
    let ring: Vec<([f64; 2], f64)> = (0..24).map(|k| std::f64::consts::TAU * k as f64 / 24.0).map(|a| ([60.0 + 10.0 * a.cos(), 40.0 + 10.0 * a.sin()], 40.0)).collect();
    let tube = pads(&ring, &s, &load, &screen);
    assert_eq!(tube.len(), 1);
    assert!(tube[0].outline_distance([60.0, 40.0]) < -10.0, "no hole");
    // two clusters apart: one pad, joined by a strip from (30, 25) to (50, 50), the narrowest
    // join grown by the brim each side; the corner a hull would fill open
    // (the lighter side's six bases' peel needs less than that at 40 mm² a base, more at 400)
    for (hang, wide) in [(40.0, 2.0 + 2.0 * s.brim), (400.0, load.section_for(6.0 * 400.0) / s.pad)] {
        let apart: Vec<([f64; 2], f64)> = grid(20.0, 20.0, hang).into_iter().chain(grid(50.0, 50.0, hang)).collect();
        let joined = pads(&apart, &s, &load, &screen);
        assert_eq!(joined.len(), 1, "joined");
        assert!(joined[0].outline_distance([25.0, 52.5]) > 10.0, "open where a hull would fill");
        let mid = joined[0].outline_distance([40.0, 37.5]);
        assert!((mid + 0.5 * wide).abs() < 0.05, "the join {wide} mm wide for {hang} mm² a base: {}", -2.0 * mid);
    }
    // a cluster out of reach: its own pad
    let far: Vec<([f64; 2], f64)> = grid(0.0, 0.0, 40.0).into_iter().chain(grid(100.0, 0.0, 40.0)).collect();
    assert_eq!(pads(&far, &s, &load, &screen).len(), 2);
}

/// Every layer of the pad stands on the one before: however near two rows of bases come (under
/// two sticks a few millimetres apart, as a six-piece burr's), where the chamfer's drawn-in edge
/// folds over no layer lights a pixel that isn't on the cured layer under it. The bases stay on
/// it. A piece just over twice the inset wide, whose bottom cures no pixel, prints on no layer
/// (from its second layer up it stood on nothing); one a little wider stands on its bottom.
#[test]
fn every_layer_of_the_pad_stands_on_the_one_before() {
    struct Pads(Vec<Pad>);
    impl Section for Pads {
        fn bounds(&self) -> Box3 {
            self.0.iter().map(Pad::bounds).reduce(|a, b| (std::array::from_fn(|k| a.0[k].min(b.0[k])), std::array::from_fn(|k| a.1[k].max(b.1[k])))).unwrap()
        }
        fn cover(&self, z: f64, grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]) {
            pad_cover(&self.0, z, grid, rect, cov);
        }
        fn near(&self, p: &[V3], out: &mut [Near]) {
            for (q, o) in p.iter().zip(out.iter_mut()) {
                *o = Near { distance: self.0.iter().map(|pad| pad.distance(*q)).fold(f64::INFINITY, f64::min), normal: [0.0, 0.0, 1.0] };
            }
        }
        fn clear(&self, p: &[V3], _need: &[f64], out: &mut [f64]) {
            for (q, o) in p.iter().zip(out.iter_mut()) {
                *o = self.0.iter().map(|pad| pad.distance(*q)).fold(f64::INFINITY, f64::min);
            }
        }
    }
    let s = SupportSettings::default();
    let load = Load::default();
    let p = simulated();
    let screen = PixelGrid::of(&p.screen);
    let held = |pads: Vec<Pad>, what: &str| {
        let stack = LayerStack::new(vec![Arc::new(Pads(pads)) as Arc<dyn Section>], &p);
        let layers = cured(&stack);
        for k in 1..layers.len() {
            for (part, on) in layers[k].parts.iter().zip(links(&layers[k - 1], &layers[k])) {
                assert!(!on.is_empty(), "{what}: layer {k} lights {} px at {:?} the layer under it doesn't hold", part.pixels, part.rect);
            }
        }
        layers
    };
    // a lone base's pad, the inset 1 mm, on a corner of four pixels: 2.02 mm across, its bottom
    // 0.02 mm (curing none of them, nor the first layer, but the second) prints nothing; 2.2 mm
    // across, its bottom 0.2 mm, it stands on that from the plate up
    let lone = |brim: f64| pads(&[([20.0, 20.0], 40.0)], &SupportSettings { base_radius: 0.6, brim, ..s }, &load, &screen);
    assert!(lone(0.41).is_empty(), "a pad whose bottom cures no pixel: {:?}", lone(0.41));
    let wider = lone(0.5);
    assert_eq!(wider.len(), 1);
    let layers = held(wider, "a pad 2.2 mm across");
    assert!(layers[0].parts.len() == 1 && layers.iter().take_while(|l| !l.parts.is_empty()).count() == 20, "on the plate and every layer of the pad");
    // two rows of six bases 5 mm apart, the rows from 7 to 9.6 mm apart, their bases in line or
    // up to half their spacing aside
    for (gap, aside) in (0..14).flat_map(|i| [0.0, 0.8, 1.6, 2.5].map(|a| (7.0 + 0.2 * i as f64, a))) {
        let feet: Vec<([f64; 2], f64)> = (0..6).flat_map(|i| [([20.0 + 5.0 * i as f64, 20.0], 40.0), ([20.0 + 5.0 * i as f64 + aside, 20.0 + gap], 40.0)]).collect();
        let pads = pads(&feet, &s, &load, &screen);
        assert!(feet.iter().all(|f| pads.iter().any(|pad| pad.outline_distance(f.0) <= -s.base_radius)), "{gap} mm apart, {aside} aside: a base off its pad");
        held(pads, &format!("{gap} mm apart, {aside} aside"));
    }
}

/// Probes past a ball, from its field and its mesh: one a millimetre clear is clear by that
/// margin, one that grazes it is blocked where it comes nearest, one that starts in it is blocked
/// at once; and a wall thinner than a ray grid's spacing still blocks a strut through it.
#[test]
fn the_clearance_is_exact_from_either_source() {
    let c = [30.0, 30.0, 10.0];
    let ball_field: Arc<dyn Section> = Arc::new(field(SdfNode::Sphere { radius: 5.0 }, c, ([24.9, 24.9, 4.9], [35.1, 35.1, 15.1])));
    let ball_mesh: Arc<dyn Section> = Arc::new(MeshSection::new(&sphere_mesh(c, 5.0, 256)));
    for ball in [ball_field, ball_mesh] {
        let parts = vec![ball];
        let probes = [
            // 7 from the centre at its nearest, 0.5 thick, 0.5 gap: 1 to spare
            Probe::even([20.0, 37.0, 10.0], [40.0, 37.0, 10.0], 0.5, 0.5),
            // 5.8 from the centre: within the gap
            Probe::even([20.0, 35.8, 10.0], [40.0, 35.8, 10.0], 0.5, 0.5),
            // from inside it
            Probe::even([30.0, 30.0, 10.0], [30.0, 30.0, 0.0], 0.5, 0.5),
        ];
        let v = clearance(&parts, &probes);
        match v[0] {
            Clear::Clear { least } => assert!((least - 1.0).abs() < 0.05, "{least}"),
            other => panic!("the clear probe: {other:?}"),
        }
        match v[1] {
            // it comes within its gap 0.5 x (on the ball at x 30) before it comes nearest
            Clear::Blocked { t } => assert!(t > 0.35 && t < 0.5, "{t}"),
            other => panic!("the grazing probe: {other:?}"),
        }
        assert_eq!(v[2], Clear::Blocked { t: 0.0 });
    }
    // a 0.1 mm wall across a pillar's way down
    let wall = SdfNode::Box { size: [10.0, 0.1, 10.0], radius: 0.0 };
    let parts: Vec<Arc<dyn Section>> = vec![Arc::new(field(wall, [30.0, 30.0, 5.0], ([24.9, 24.9, 4.9], [35.1, 35.1, 5.1])))];
    let mesh: Vec<Arc<dyn Section>> = vec![Arc::new(MeshSection::new(&prism(&rect([25.0, 25.0], [35.0, 35.0]), None, 4.95, 5.05)))];
    for parts in [parts, mesh] {
        let v = clearance(&parts, &[Probe::even([30.0, 30.0, 9.0], [30.0, 30.0, 1.0], 0.05, 0.0)]);
        assert!(matches!(v[0], Clear::Blocked { t } if (t * 8.0 - (4.0 - 0.1)).abs() < 0.1), "{v:?}");
    }
}

/// Supports on `parts` (as they stand on the plate: lifted by the elevation and the pad), from
/// their points.
fn supports_of(parts: Vec<Arc<dyn Section>>, s: &SupportSettings, density: f64) -> (Vec<Arc<dyn Section>>, Arc<Supports>, usize) {
    let (n, p) = (parts.len(), simulated());
    let r = supported(parts, PixelGrid::of(&p.screen), p.layer_height, s, &PointSettings { density, ..PointSettings::default() }, &Load::default());
    (r.stack.sections[..n].to_vec(), r.supports, r.points)
}

/// What every set of supports keeps: each point held or said why not; every head along a way
/// down, its tip in the part; every pillar, bridge and head past its tip clear of the parts by
/// the gap; every bridge falling at least 45°; a pad under every base.
fn sound(parts: &[Arc<dyn Section>], sup: &Supports, points: usize, s: &SupportSettings) {
    assert_eq!(sup.held() + sup.dropped.len(), points, "every point held or dropped: {:?}", sup.dropped);
    for h in &sup.heads {
        assert!(h.axis[2] <= -s.lean.cos() + 1e-9, "{h:?} leans too far");
        // its tip ball's centre in the part, and the part left 0.3 mm past its point along it
        let at = add(h.tip, scale(h.axis, s.penetration - s.tip_radius));
        let mut near = [Near::default(); 2];
        let inside = |q: V3, near: &mut [Near]| parts.iter().any(|p| {
            p.near(&[q], &mut near[..1]);
            near[0].distance < 0.0
        });
        assert!(inside(h.tip, &mut near), "{h:?}: its tip is not in the part");
        assert!(!inside(add(at, scale(h.axis, 0.3)), &mut near), "{h:?}: its tip is deeper than its penetration");
    }
    let probes: Vec<Probe> = sup
        .pillars
        .iter()
        .map(|p| Probe::even(p.top, [p.top[0], p.top[1], s.ground() + s.base_height], p.radius, s.gap - 0.03))
        .chain(sup.bridges.iter().map(|b| Probe::even(sup.heads[b.head].junction, b.at, b.radius, s.gap - 0.03)))
        .collect();
    for (k, v) in clearance(parts, &probes).iter().enumerate() {
        assert!(v.is_clear(), "strut {k} {:?}: {v:?}", probes[k]);
    }
    for b in &sup.bridges {
        let (j, at) = (sup.heads[b.head].junction, b.at);
        let run = (j[0] - at[0]).hypot(j[1] - at[1]);
        assert!(j[2] - at[2] >= run - 1e-9, "bridge from {j:?} to {at:?} falls less than 45°");
        assert!(sup.pillars[b.pillar].heads.contains(&b.head));
    }
    assert!(sup.pillars.iter().all(|p| p.heads.len() <= s.bridges + 1), "a pillar takes its head and three bridges");
    for p in &sup.pillars {
        let foot = [p.top[0], p.top[1]];
        assert!(sup.pads.iter().any(|pad| pad.outline_distance(foot) <= -(s.base_radius + s.brim) + 0.01), "the base at {foot:?} on a pad");
    }
}

/// A cube held off the plate: its bottom's island points each get a head and stand on pillars,
/// heads near each other sharing one, all on one pad; its layers are the part's over the
/// supports', the pad at the bottom. From a field and a mesh alike.
#[test]
fn a_raised_cube_stands_on_pillars() {
    let s = SupportSettings::default();
    let f: Arc<dyn Section> = Arc::new(field(SdfNode::Box { size: [10.0, 4.0, 10.0], radius: 0.0 }, [30.0, 30.0, 2.0], ([24.9, 24.9, -0.1], [35.1, 35.1, 4.1])));
    let mesh: Arc<dyn Section> = Arc::new(MeshSection::new(&prism(&rect([25.0, 25.0], [35.0, 35.0]), None, 0.0, 4.0)));
    for cube in [f, mesh] {
        let started = std::time::Instant::now();
        let (parts, sup, points) = supports_of(vec![cube], &s, 1.0);
        let took = started.elapsed();
        sound(&parts, &sup, points, &s);
        assert!(sup.dropped.iter().all(|d| d.1 == Dropped::Twin), "{:?}", sup.dropped);
        assert!(sup.pillars.len() <= sup.heads.len(), "{} pillars for {} heads", sup.pillars.len(), sup.heads.len());
        assert_eq!(sup.pads.len(), 1);
        // every head under the cube's bottom, along the normal: straight down
        assert!(!sup.heads.is_empty() && sup.heads.iter().all(|h| h.axis[2] < -0.999), "{:?}", sup.heads);
        // its layers: the pad, then pillars, then the cube
        let section: Arc<dyn Section> = Arc::new(SupportSection::new(sup.clone()).expect("supports"));
        let mut all = parts.clone();
        all.push(section);
        let stack = LayerStack::new(all, &simulated());
        let area = |z: f64| stack.layer(stack.layers.at(z)).area(&stack.grid);
        let pad_area = ring_area(&sup.pads[0].outline_at(stack.layers.z(stack.layers.at(0.5))));
        assert!((area(0.5) - pad_area).abs() < 0.01 * pad_area, "the pad: {} against {pad_area}", area(0.5));
        // the profile's pins on cone feet: each head's pillar a stub between its foot and its
        // junction, too short for a bridge to land on above its base
        let pillars = sup.pillars.len() as f64 * std::f64::consts::PI * s.pillar_radius * s.pillar_radius;
        assert!((area(2.25) - pillars).abs() < 0.03 * pillars, "the pillars: {} against {pillars}", area(2.25));
        assert!(sup.bridges.is_empty(), "{:?}", sup.bridges);
        assert!((area(9.0) - 100.0).abs() < 1.0, "the cube: {}", area(9.0));
        println!("cube: {points} points, {} heads, {} pillars, {} bridges in {took:?}", sup.heads.len(), sup.pillars.len(), sup.bridges.len());
    }
}

/// A shelf over a slab, joined by a wall: the shelf's underside over the slab has no way down
/// past the slab (no drop, no bridge to a pillar beside it), so those points are dropped and said
/// so; the rest stand clear of both.
#[test]
fn points_with_no_way_down_are_said_so() {
    let s = SupportSettings::default();
    let slab = SdfNode::Transform { at: [0.0, 1.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [20.0, 2.0, 20.0], radius: 0.0 }) };
    let wall = SdfNode::Transform { at: [-9.0, 4.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [2.0, 8.0, 20.0], radius: 0.0 }) };
    let shelf = SdfNode::Transform { at: [0.0, 7.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [20.0, 2.0, 20.0], radius: 0.0 }) };
    let both = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![slab, wall, shelf] };
    let f: Arc<dyn Section> = Arc::new(field(both, [40.0, 40.0, 0.0], ([29.9, 29.9, -0.1], [50.1, 50.1, 8.1])));
    {
        let part = f;
        let (parts, sup, points) = supports_of(vec![part], &s, 1.0);
        sound(&parts, &sup, points, &s);
        let no_route: Vec<usize> = sup.dropped.iter().filter(|d| d.1 == Dropped::NoRoute).map(|d| d.0).collect();
        assert!(!no_route.is_empty(), "the shelf's underside over the slab: {:?}", sup.dropped);
        let stack = LayerStack::new(parts.clone(), &simulated());
        let pts = support_points(&stack, &PointSettings::default());
        for &i in &no_route {
            let p = pts[i].at;
            assert!(p[2] > s.lift() + 5.9 && p[0] > 32.0, "{:?}: under the shelf, over the slab", pts[i]);
        }
    }
}

/// The supports' layers as a field draws them match their own mesh's cut by the mesh slicer,
/// within what the mesh's facets cut off a round strut.
#[test]
fn the_supports_layers_match_their_mesh() {
    let s = SupportSettings::default();
    let ball: Arc<dyn Section> = Arc::new(field(SdfNode::Sphere { radius: 6.0 }, [40.0, 40.0, 6.0], ([33.9, 33.9, -0.1], [46.1, 46.1, 12.1])));
    let (parts, sup, points) = supports_of(vec![ball], &s, 1.0);
    sound(&parts, &sup, points, &s);
    let p = simulated();
    let field = LayerStack::new(vec![Arc::new(SupportSection::new(sup.clone()).expect("supports")) as Arc<dyn Section>], &p);
    let mesh = LayerStack::new(vec![Arc::new(MeshSection::new(&sup.mesh())) as Arc<dyn Section>], &p);
    for z in [0.5, 1.5, 2.5, 4.0, 5.5, 6.5] {
        let (a, b) = (field.layer(field.layers.at(z)).area(&field.grid), mesh.layer(mesh.layers.at(z)).area(&mesh.grid));
        assert!(a > 0.0 && (a - b).abs() <= 0.04 * a, "at {z} mm: the field's {a} mm² and the mesh's {b} mm²");
    }
}

/// Points dense enough to lie within a base's width of each other: heads share pillars, the
/// others bridging to the one nearest their middle, no pillar taking more than three bridges.
#[test]
fn heads_near_each_other_share_a_pillar() {
    // raised enough that a bridge lands on a pillar above its base
    let s = SupportSettings { elevation: 10.0, ..SupportSettings::default() };
    let cube: Arc<dyn Section> = Arc::new(field(SdfNode::Box { size: [10.0, 4.0, 10.0], radius: 0.0 }, [30.0, 30.0, 2.0], ([24.9, 24.9, -0.1], [35.1, 35.1, 4.1])));
    let (parts, sup, points) = supports_of(vec![cube], &s, 4.0);
    sound(&parts, &sup, points, &s);
    assert!(!sup.bridges.is_empty() && sup.pillars.len() < sup.heads.len(), "{} pillars for {} heads", sup.pillars.len(), sup.heads.len());
    for b in &sup.bridges {
        let (j, top) = (sup.heads[b.head].junction, sup.pillars[b.pillar].top);
        assert!((j[0] - top[0]).hypot(j[1] - top[1]) <= s.share_plan + 1e-9, "a bridge from {j:?} to the pillar at {top:?}");
    }
}

/// The scrub plate's tile, from the engine's own nodes: a square frame round a gyroid, both from
/// the plate up, beside a mesh shelf. The frame's and the sheet's bottoms are islands on the first
/// layer: each stands on supports, the field's as the mesh's.
#[test]
fn a_gyroid_tile_stands_on_supports() {
    let s = SupportSettings::default();
    let at = |y: f64, of: SdfNode| SdfNode::Transform { at: [0.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
    let block = || at(6.0, SdfNode::Box { size: [20.0, 12.0, 20.0], radius: 0.0 });
    let open = at(6.0, SdfNode::Box { size: [17.0, 14.0, 17.0], radius: 0.0 });
    let sheet = SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of: vec![SdfNode::Gyroid { period: 4.0, wall: 0.5 }, block()] };
    let frame = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![block(), open] };
    let tile = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![sheet, frame] };
    let f: Arc<dyn Section> = Arc::new(field(tile, [40.0, 40.0, 0.0], ([30.0, 30.0, 0.0], [50.0, 50.0, 12.0])));
    let shelf: Arc<dyn Section> = Arc::new(MeshSection::new(&prism(&rect([60.0, 25.0], [100.0, 55.0]), None, 0.0, 4.0)));
    let started = std::time::Instant::now();
    let (parts, sup, points) = supports_of(vec![f, shelf], &s, 1.0);
    let took = started.elapsed();
    sound(&parts, &sup, points, &s);
    let under_tile = sup.pillars.iter().filter(|p| p.top[0] < 55.0).count();
    println!("tile and shelf: {points} points, {} heads, {} pillars ({under_tile} under the tile), {} bridges, {} dropped in {took:?}", sup.heads.len(), sup.pillars.len(), sup.bridges.len(), sup.dropped.len());
    assert!(sup.dropped.iter().all(|d| d.1 == Dropped::Twin), "{:?}", sup.dropped);
    assert!(under_tile >= 20, "the tile's frame and sheet stand on pillars: {under_tile}");
}

/// A field twice as steep as its distance: its first proofs are half the truth, too little for
/// any head's gap, so its clearance covers each ball it needs with boxes it proves clear. Its
/// supports are the distance's own.
#[test]
fn a_field_steeper_than_its_distance_stands_on_its_supports() {
    struct Steep;
    impl makepad_csg_sdf::Sdf3 for Steep {
        fn distance(&self, p: makepad_csg_math::Vec3d) -> f64 {
            let q = [p.x.abs() - 5.0, p.y.abs() - 2.0, p.z.abs() - 5.0];
            let out = q.iter().map(|v| v.max(0.0).powi(2)).sum::<f64>().sqrt();
            2.0 * (out + q[0].max(q[1]).max(q[2]).min(0.0))
        }
    }
    let s = SupportSettings::default();
    let kernel = SdfNode::Kernel(makepad_csg_sdf::SdfKernel { key: 0x57ee9, field: Arc::new(Steep), splash: None, bounds: Some(([-5.0, -2.0, -5.0], [5.0, 2.0, 5.0])), solid: None });
    let bounds = ([24.9, 24.9, -0.1], [35.1, 35.1, 4.1]);
    let steep: Arc<dyn Section> = Arc::new(field(kernel, [30.0, 30.0, 2.0], bounds));
    let exact: Arc<dyn Section> = Arc::new(field(SdfNode::Box { size: [10.0, 4.0, 10.0], radius: 0.0 }, [30.0, 30.0, 2.0], bounds));
    let started = std::time::Instant::now();
    let (parts, sup, points) = supports_of(vec![steep], &s, 1.0);
    let took = started.elapsed();
    sound(&parts, &sup, points, &s);
    let (_, truth, _) = supports_of(vec![exact], &s, 1.0);
    println!("steep: {points} points, {} heads, {} pillars, {} dropped in {took:?}", sup.heads.len(), sup.pillars.len(), sup.dropped.len());
    assert!(sup.dropped.iter().all(|d| d.1 == Dropped::Twin), "{:?}", sup.dropped);
    assert_eq!(sup.pillars.len(), truth.pillars.len(), "the steep field's pillars are its distance's");
}

/// The area of `points`' convex hull grown by `grow` (mm²): the pad a hull of the bases makes.
fn hull_area(points: &[[f64; 2]], grow: f64) -> f64 {
    let mut p = points.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    let turn = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut h: Vec<[f64; 2]> = Vec::new();
    for pass in 0..2 {
        let start = h.len();
        let order: Vec<[f64; 2]> = if pass == 0 { p.clone() } else { p.iter().rev().copied().collect() };
        for q in order {
            while h.len() >= start + 2 && turn(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    let n = h.len();
    let rim = (0..n).map(|i| (h[(i + 1) % n][0] - h[i][0]).hypot(h[(i + 1) % n][1] - h[i][1])).sum::<f64>();
    ring_area(&h).max(0.0) + rim * grow + std::f64::consts::PI * grow * grow
}

/// The scrub bench's plate: a gyroid tile, a bracket (a wall standing on a shelf) and a ring,
/// each on a pad of its own, none under two parts; the first layer cures less than the one
/// hull over all their bases it replaces.
#[test]
fn the_scrub_plate_stands_on_a_pad_per_part() {
    let s = SupportSettings::default();
    let at = |y: f64, of: SdfNode| SdfNode::Transform { at: [0.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
    let block = || at(6.0, SdfNode::Box { size: [20.0, 12.0, 20.0], radius: 0.0 });
    let open = at(6.0, SdfNode::Box { size: [17.0, 14.0, 17.0], radius: 0.0 });
    let sheet = SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of: vec![SdfNode::Gyroid { period: 4.0, wall: 0.5 }, block()] };
    let frame = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![block(), open] };
    let tile = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![sheet, frame] };
    let shelf = at(2.0, SdfNode::Box { size: [40.0, 4.0, 30.0], radius: 0.0 });
    let wall = at(15.0, SdfNode::Box { size: [40.0, 30.0, 4.0], radius: 0.0 });
    let bracket = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![shelf, wall] };
    let tube = |r: f64, h: f64| at(10.0, SdfNode::Cylinder { radius: r, top_radius: r, height: h });
    let ring = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![tube(12.0, 20.0), tube(10.0, 22.0)] };
    let parts: Vec<Arc<dyn Section>> = vec![
        Arc::new(field(tile, [25.0, 40.0, 0.0], ([14.9, 29.9, -0.1], [35.1, 50.1, 12.1]))),
        Arc::new(field(bracket, [80.0, 25.0, 0.0], ([59.9, 9.9, -0.1], [100.1, 40.1, 30.1]))),
        Arc::new(field(ring, [85.0, 60.0, 0.0], ([72.9, 47.9, -0.1], [97.1, 72.1, 20.1]))),
    ];
    let started = std::time::Instant::now();
    let (n, p) = (parts.len(), simulated());
    let on = supported(parts, PixelGrid::of(&p.screen), p.layer_height, &s, &PointSettings::default(), &Load::default());
    let took = started.elapsed();
    let (sup, shown) = (&on.supports, on.stack.sections[..n].to_vec());
    sound(&shown, sup, on.points, &s);
    let feet = |part: usize| sup.pillars.iter().filter(|q| sup.heads[q.heads[0]].part == part).map(|q| [q.top[0], q.top[1]]).collect::<Vec<_>>();
    for k in 0..n {
        assert!(!feet(k).is_empty(), "part {k} stands on pillars");
    }
    for pad in &sup.pads {
        let under: Vec<usize> = (0..n).filter(|&k| feet(k).iter().any(|f| pad.outline_distance(*f) < 0.0)).collect();
        assert_eq!(under.len(), 1, "a pad under parts {under:?}");
    }
    let first = on.stack.layer(0).area(&on.stack.grid);
    let all: Vec<[f64; 2]> = sup.pillars.iter().map(|q| [q.top[0], q.top[1]]).collect();
    let hull = hull_area(&all, s.base_radius + s.brim);
    let wide = sup.pillars.iter().filter(|q| q.radius > s.pillar_radius).count();
    println!("scrub plate: {} points, {} heads, {} pillars ({wide} sized up), {} bridges, {} pads in {took:?}; first layer {first:.0} mm² against one hull's {hull:.0} mm²", on.points, sup.heads.len(), sup.pillars.len(), sup.bridges.len(), sup.pads.len());
    assert!(first < 0.6 * hull, "the first layer {first} mm² against the hull's {hull} mm²");
}

/// A cylinder along X, `r` round, from `x0` to `x1`, its axis at model height `y` (the model
/// frame, Y up).
fn rod(r: f64, x0: f64, x1: f64, y: f64) -> SdfNode {
    let c = SdfNode::Cylinder { radius: r, top_radius: r, height: x1 - x0 };
    SdfNode::Transform { at: [0.5 * (x0 + x1), y, 0.0], rotate: [0.0, 0.0, 90.0], scale: 1.0, of: Box::new(c) }
}

/// A print-in-place hinge as it holds a wheel on its pin: a plate on the pin's end, in a pocket of
/// the wheel's outer face 0.4 mm round it, the pin through the wheel to its axle. The plate's
/// first layer is an island the pocket's floor leaves no head room under; it is held from its
/// open edge instead, its tip curing into that first layer, clear of the wheel, and the print
/// keeps the rules' Connected. A ball caged in a shell has no edge a head reaches: the check says
/// its island is out of every support's reach, and why.
#[test]
fn a_hinge_plate_in_its_pocket_is_held_from_its_edge() {
    let s = SupportSettings::default();
    let p = simulated();
    let (at, y) = ([40.0, 40.0, 0.0], 10.0);
    let wheel = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![rod(8.0, 0.0, 6.0, y), rod(3.9, -1.0, 1.8, y), rod(1.35, -1.0, 7.0, y)] };
    let axle = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![rod(3.5, 0.0, 1.4, y), rod(0.95, 0.0, 7.2, y), rod(3.5, 6.4, 12.4, y)] };
    let wheel: Arc<dyn Section> = Arc::new(field(wheel, at, ([39.9, 31.9, 1.9], [46.1, 48.1, 18.1])));
    let axle: Arc<dyn Section> = Arc::new(field(axle, at, ([39.9, 36.4, 6.4], [52.5, 43.6, 13.6])));
    let r = supported(vec![wheel, axle], PixelGrid::of(&p.screen), p.layer_height, &s, &PointSettings::default(), &Load::default());
    let parts = r.stack.sections[..2].to_vec();
    sound(&parts, &r.supports, r.points, &s);
    let islands: Vec<String> = check(&r.stack, 2, &r.supports, &r.cured, &|| false).into_iter().filter(|b| b.rule == Rule::Connected).map(|b| format!("{} {:?}: {}", b.layer, b.at, b.what)).collect();
    assert_eq!(islands, Vec::<String>::new(), "every island held");
    // the plate's bottom, its first layer, held from the plate's outer face
    let bottom = s.lift() + y - 3.5;
    let edge: Vec<&Head> = r.supports.heads.iter().filter(|h| h.part == 1 && h.at[0] < 40.6 && (h.at[2] - bottom).abs() < 0.3).collect();
    assert!(!edge.is_empty(), "the plate held from its edge: {:?}", r.supports.heads.iter().filter(|h| h.part == 1).collect::<Vec<_>>());
    for h in &edge {
        assert!(h.junction[0] < 40.0 - s.pillar_radius, "{h:?}: out of the pocket");
    }

    // a ball in a cage: nothing reaches under it
    let cage = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![SdfNode::Sphere { radius: 6.0 }, SdfNode::Sphere { radius: 4.4 }] };
    let place = |n: SdfNode| SdfNode::Transform { at: [0.0, 8.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(n) };
    let cage: Arc<dyn Section> = Arc::new(field(place(cage), at, ([33.9, 33.9, 1.9], [46.1, 46.1, 14.1])));
    let ball: Arc<dyn Section> = Arc::new(field(place(SdfNode::Sphere { radius: 4.0 }), at, ([35.9, 35.9, 3.9], [44.1, 44.1, 12.1])));
    let r = supported(vec![cage, ball], PixelGrid::of(&p.screen), p.layer_height, &s, &PointSettings::default(), &Load::default());
    let islands: Vec<String> = check(&r.stack, 2, &r.supports, &r.cured, &|| false).into_iter().filter(|b| b.rule == Rule::Connected).map(|b| b.what).collect();
    assert!(!islands.is_empty() && islands.iter().all(|w| w.ends_with(": no support can reach it (no head clears the parts round it)")), "{islands:#?}");
}

/// A slab high over a block, held at four points along a line: two over the block bridge to the
/// pillar just past its edge, which stands too near it to widen for all three, and a fourth
/// pillar stands clear further out. The pillar by the block splits its load instead: the heads
/// bridged to it go on to the pillar clear of the block, which widens for them, and every pillar
/// holds what hangs on it.
#[test]
fn a_pillar_that_cant_widen_passes_its_load_on() {
    let (s, load, p) = (SupportSettings::default(), Load::default(), simulated());
    let grid = PixelGrid::of(&p.screen);
    // as they stand on the supports: the slab from 30 mm up, the block from 6 to 9 mm with its
    // face at x 37, the near pillar's centre 1.12 mm from it (a 1 mm pillar clears it with its
    // gap, one wide enough for three heads doesn't)
    let slab: Arc<dyn Section> = Arc::new(field(SdfNode::Box { size: [14.0, 1.0, 6.0], radius: 0.0 }, [37.0, 33.0, 30.5], ([29.9, 29.9, 29.9], [44.1, 36.1, 31.1])));
    let block: Arc<dyn Section> = Arc::new(field(SdfNode::Box { size: [10.0, 3.0, 10.0], radius: 0.0 }, [32.0, 33.0, 7.5], ([26.9, 27.9, 5.9], [37.1, 38.1, 9.1])));
    let mut stack = LayerStack { sections: vec![slab, block], grid, layers: Layers::up_to(31.0, p.layer_height) };
    let cured = cured(&stack);
    let layer = stack.layers.at(30.0 + 0.5 * p.layer_height);
    let points: Vec<SupportPoint> = [[38.12, 33.0], [35.0, 32.0], [35.0, 34.0], [42.0, 33.0]].iter().map(|q| SupportPoint { at: [q[0], q[1], 30.0], normal: [0.0, 0.0, -1.0], kind: PointKind::Island, layer }).collect();
    let sup = Arc::new(Supports::new(&points, &stack.sections, &s, &load, &cured, &grid, &stack.layers));
    let parts = stack.sections.clone();
    sound(&parts, &sup, points.len(), &s);
    // the near pillar keeps its own head, the far one takes the two over the block
    let near = sup.pillars.iter().find(|q| (q.top[0] - 38.12).abs() < 1e-6).expect("the pillar by the block");
    let far = sup.pillars.iter().find(|q| (q.top[0] - 42.0).abs() < 1e-6).expect("the pillar clear of it");
    assert_eq!((near.heads.len(), far.heads.len(), sup.pillars.len()), (1, 3, 2), "{:?}", sup.pillars);
    assert!(far.radius > near.radius && near.radius > s.pillar_radius, "{:?}", sup.pillars);
    stack.sections.push(Arc::new(SupportSection::new(sup.clone()).expect("supports")));
    let weak: Vec<String> = check(&stack, 2, &sup, &cured, &|| false).into_iter().filter(|b| b.rule == Rule::Strong).map(|b| format!("{:?}: {}", b.at, b.what)).collect();
    assert_eq!(weak, Vec::<String>::new(), "every pillar holds what hangs on it");
}

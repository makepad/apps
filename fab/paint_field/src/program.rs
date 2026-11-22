//! The paint program: layers over a part's frame (millimetres), indexed by a uniform grid so a
//! point reads only the layers that can reach it.

use makepad_csg_math::Vec3d;
use makepad_csg_sdf::{cut, distance, distance_many, node_bounds, quat_from_degrees, same, transform_into, SdfKernel, SdfNode, SdfPlace};
use crate::facets::Facets;
use crate::flood::Patch;
use crate::operand::{apart_boxes, side, union_values, Changes, Operand};
use crate::strokes::Strokes;
use std::collections::HashMap;
use std::sync::Arc;

pub type P3 = [f64; 3];

/// What a layer writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// The colour a surface shows and prints in (slot 0: the part's own).
    Color,
    /// Supports never go here.
    SupportBlock,
    /// Supports always go here.
    SupportEnforce,
    /// No tree support stands or grows here: what `m.no_trees` marks. Only the supports read it,
    /// never the slice.
    NoTrees,
    /// A region with print settings of its own (`m.settings`): slot `n` the object's `n`th set of
    /// region settings. Each layer holds alone (a later one overrides only the keys it sets), so
    /// the slice reads each layer's own region ([`Program::region_distance`]), never a sample.
    Settings,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Slot `slot` (1.. into the program's palette; for support channels 1 = on).
    Paint { channel: Channel, slot: u8 },
    /// Back to slot 0 on the channel.
    Erase { channel: Channel },
    /// The material a field gives at each point ([`Mix`]).
    Mix { channel: Channel, mix: Arc<Mix> },
}

/// What gives a mix its place at a point.
#[derive(Clone)]
pub enum MixField {
    /// The script's fn of a point (its own units, see [`Mix::per_mm`]).
    Fn(SdfKernel),
    /// A pattern grown on the part's surface ([`crate::pattern`]), 0..1 over the materials;
    /// until the surface grows it ([`Program::resolve_patterns`]), the first material.
    Pattern(crate::pattern::Grown),
    /// A place per cell of a grid on each layer (a picture's dither, [`crate::raster`]); until
    /// the build makes it ([`Program::resolve_rasters`]), the first material.
    Raster(crate::raster::Raster),
}

impl std::fmt::Debug for MixField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MixField::Fn(k) => write!(f, "{k:?}"),
            MixField::Pattern(g) => write!(f, "{g:?}"),
            MixField::Raster(r) => write!(f, "{r:?}"),
        }
    }
}

impl PartialEq for MixField {
    fn eq(&self, o: &MixField) -> bool {
        match (self, o) {
            (MixField::Fn(a), MixField::Fn(b)) => a.key == b.key,
            (MixField::Pattern(a), MixField::Pattern(b)) => a == b,
            (MixField::Raster(a), MixField::Raster(b)) => a == b,
            _ => false,
        }
    }
}

/// A material by a field through the volume: at a point, a place in `slots` (0 the first, 1 the
/// second, a fraction between two a blend of the two that prints dithered), from a Splash fn of
/// the point or a grown pattern.
#[derive(Clone)]
pub struct Mix {
    pub field: MixField,
    /// The fn's units per millimetre (a script's fns read metres: 0.001).
    pub per_mm: f64,
    pub slots: Vec<u8>,
    /// How sharply a blend turns from one material to the next: 0 as the field gives it, 1 at
    /// once halfway (no blend).
    pub sharp: f64,
    /// Added to every place: over 0 toward the later materials.
    pub balance: f64,
}

impl Mix {
    /// The place at a point (mm), with the balance, held to the list.
    pub fn place(&self, p: P3) -> f64 {
        let last = self.slots.len().saturating_sub(1) as f64;
        self.held(match &self.field {
            MixField::Fn(kernel) => {
                let k = self.per_mm;
                kernel.field.distance(Vec3d::new(p[0] * k, p[1] * k, p[2] * k))
            }
            MixField::Pattern(grown) => grown.value(p) * last,
            MixField::Raster(r) => r.grid.as_ref().map_or(0.0, |g| g.place(p)),
        })
    }

    /// [`Mix::place`] at many points: a fn's kernel runs once for them all.
    pub fn places(&self, ps: &[P3]) -> Vec<f64> {
        match &self.field {
            MixField::Fn(kernel) => {
                let k = self.per_mm;
                let at: Vec<Vec3d> = ps.iter().map(|p| Vec3d::new(p[0] * k, p[1] * k, p[2] * k)).collect();
                let mut v = vec![0.0; ps.len()];
                kernel.field.distance_many(&at, &mut v);
                v.into_iter().map(|v| self.held(v)).collect()
            }
            MixField::Pattern(_) | MixField::Raster(_) => ps.iter().map(|p| self.place(*p)).collect(),
        }
    }

    /// A field's value with the balance, held to the list (0 where it has none).
    fn held(&self, v: f64) -> f64 {
        let v = v + self.balance;
        if v.is_nan() {
            return 0.0;
        }
        v.clamp(0.0, self.slots.len().saturating_sub(1) as f64)
    }

    /// A lower bound of the distance from `p` (mm) to where the place can change: a grown
    /// pattern's is constant away from its cells, a fn's may change anywhere (0).
    pub fn clear_distance(&self, p: P3) -> f64 {
        match &self.field {
            MixField::Fn(_) => 0.0,
            MixField::Pattern(grown) => grown.grid.as_ref().map_or(f64::INFINITY, |g| g.clear_distance(p)),
            MixField::Raster(r) => r.grid.as_ref().map_or(0.0, |g| g.clear_distance(p)),
        }
    }

    /// The material at a place: the nearer slot, and while the place blends two, the other one
    /// and its share (over 0, at most one half).
    pub fn at_place(&self, v: f64) -> (u8, Option<(u8, f32)>) {
        let n = self.slots.len();
        if n < 2 {
            return (self.slots.first().copied().unwrap_or(0), None);
        }
        let i = (v.max(0.0).floor() as usize).min(n - 2);
        let (a, b) = (self.slots[i], self.slots[i + 1]);
        let t = v - i as f64;
        let t = if self.sharp >= 1.0 { if t < 0.5 { 0.0 } else { 1.0 } } else { ((t - 0.5) / (1.0 - self.sharp.max(0.0)) + 0.5).clamp(0.0, 1.0) };
        if a == b || t <= 0.0 {
            (a, None)
        } else if t >= 1.0 {
            (b, None)
        } else if t <= 0.5 {
            (a, Some((b, t as f32)))
        } else {
            (b, Some((a, (1.0 - t) as f32)))
        }
    }

    /// The material at a point (mm).
    pub fn at(&self, p: P3) -> (u8, Option<(u8, f32)>) {
        self.at_place(self.place(p))
    }

    /// How much its field is at `p`, 0..1: a function's everywhere, a grown pattern's in its band
    /// (fading over a cell at its edge), none before it has grown; a raster's everywhere once
    /// made.
    pub fn presence(&self, p: P3) -> f64 {
        match &self.field {
            MixField::Fn(_) => 1.0,
            MixField::Pattern(grown) => grown.grid.as_ref().map_or(0.0, |g| g.weighed(p).1),
            MixField::Raster(r) => r.grid.as_ref().map_or(0.0, |_| 1.0),
        }
    }

    /// Its pattern, when it is one.
    pub fn pattern(&self) -> Option<&crate::pattern::Grown> {
        match &self.field {
            MixField::Pattern(g) => Some(g),
            MixField::Fn(_) | MixField::Raster(_) => None,
        }
    }
}

impl std::fmt::Debug for Mix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mix({:?} per_mm {} slots {:?} sharp {} balance {})", self.field, self.per_mm, self.slots, self.sharp, self.balance)
    }
}

impl PartialEq for Mix {
    fn eq(&self, o: &Mix) -> bool {
        self.field == o.field && self.per_mm == o.per_mm && self.slots == o.slots && self.sharp == o.sharp && self.balance == o.balance
    }
}

impl Channel {
    fn index(self) -> usize {
        match self {
            Channel::Color => 0,
            Channel::SupportBlock => 1,
            Channel::SupportEnforce => 2,
            Channel::NoTrees => 3,
            Channel::Settings => 4,
        }
    }
}

impl Op {
    pub fn channel(&self) -> Channel {
        match self {
            Op::Paint { channel, .. } | Op::Erase { channel } | Op::Mix { channel, .. } => *channel,
        }
    }
}

/// Where a layer applies: inside (distance <= 0).
#[derive(Clone, Debug)]
pub enum Region {
    /// The whole part.
    All,
    /// A solid's own distance field, grown by `grow` (a feature fill takes its surface band).
    Field { node: SdfNode, grow: f64 },
    /// A feature of the part's field: the surface it makes, and round it where that surface is
    /// the nearest ([`crate::operand`]); several features' union.
    Operand(Arc<Vec<Operand>>),
    /// A brushed stroke: a chain of capsules of one radius.
    Stroke { points: Vec<P3>, radius: f64 },
    /// A smooth-region fill as written: from `seed` while neighbouring normals stay within
    /// `angle` (radians). It holds nothing until the part's surface resolves it into a
    /// [`Region::Patch`] ([`Program::resolve_floods`]).
    Flood { seed: P3, angle: f64 },
    /// A flood resolved on a part's surface.
    Patch(Arc<Patch>),
    /// Painted leaves of an imported mesh. Placements are outermost first, each scaling
    /// uniformly, rotating in degrees, then moving from its inner frame to its outer frame.
    Facets { facets: Arc<Facets>, state: u8, frames: Vec<(P3, P3, f64)> },
    /// A strokes file (`m.strokes("vase.strokes")`): its paint strokes, the last one holding a
    /// point deciding it. `slots` maps the file's palette to the program's (a script binds the
    /// file's log after it compiles: [`Program::with_strokes`]).
    Strokes { file: String, strokes: Arc<Strokes>, slots: Arc<Vec<u8>> },
}

impl Region {
    /// The signed distance to the region at `p` (negative inside; the whole part: inside).
    pub fn distance(&self, p: P3) -> f64 {
        match self {
            Region::All => -1e9,
            Region::Field { node, grow } => distance(node, Vec3d::new(p[0], p[1], p[2])) - grow,
            Region::Operand(of) => union_values(of, &[p], f64::INFINITY)[0],
            Region::Stroke { points, radius } => (0..points.len().max(1)).map(|s| capsule(p, points[s.min(points.len() - 1)], points[(s + 1).min(points.len() - 1)])).fold(f64::MAX, f64::min) - radius,
            Region::Flood { .. } => 1e9,
            Region::Patch(patch) => patch.distance(p),
            Region::Facets { facets, state, frames } => {
                let mut p = Vec3d::new(p[0], p[1], p[2]);
                let mut size = 1.0;
                for &(at, rotate, scale) in frames {
                    p = transform_into(at, rotate, scale, p);
                    size *= scale;
                }
                facets.distance(*state, [p.x, p.y, p.z]) * size
            }
            Region::Strokes { strokes, .. } => {
                let h = strokes.sample(p);
                if h.paint.is_some() { -h.dist } else { h.dist }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub region: Region,
    pub op: Op,
}

/// One grid entry: a layer, and for a stroke the capsule (segment start) in it.
#[derive(Clone, Copy, Debug)]
struct Entry {
    layer: u32,
    seg: u32,
}

/// Grid cell edge (mm). A region is listed in every cell its box, grown by a cell, touches, so an
/// empty cell has no boundary within a cell of any of its points. The strokes index uses the
/// same grid, and the paint bake's bricks nest in it (eight samples at 0.4, 0.2 or 0.1 mm).
pub const CELL: f64 = crate::strokes::BRICK;

/// A region whose box touches more cells than this is not listed in each: it is kept as the range
/// of cells its box touches ([`Wide`]). A fill over all of a part and the bed past it would
/// otherwise list hundreds of thousands of cells, tens of megabytes in every program built of it.
const WIDE_CELLS: i64 = 4096;

/// A region kept as the range of grid cells its box touches, first to last on each axis: it reads
/// at a point whose cell lies in the range, as though listed in each of those cells.
#[derive(Clone, Copy, Debug)]
struct Wide {
    layer: u32,
    first: [i32; 3],
    last: [i32; 3],
}

impl Wide {
    fn holds(&self, cell: [i32; 3]) -> bool {
        (0..3).all(|k| self.first[k] <= cell[k] && cell[k] <= self.last[k])
    }
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub layers: Vec<Layer>,
    /// Layers listed everywhere (the whole part, regions without a box).
    global: Vec<u32>,
    /// Per layer, what its distance is divided by: a field region with a Splash kernel may be
    /// any function (noise), steeper than a distance; its measured Lipschitz bound keeps the
    /// bake's whole-brick test and the edge ramp true. 1 for every other region.
    steep: Vec<f64>,
    grid: HashMap<[i32; 3], Vec<Entry>>,
    /// The regions too wide to list in the grid, in layer order.
    wide: Vec<Wide>,
    bounds: Option<(P3, P3)>,
    /// Per channel ([`Channel::index`]): the box its boxed layers lie in, and whether a layer of
    /// it has none.
    channels: [(Option<(P3, P3)>, bool); 5],
    /// The mix layers: their fields reach past their regions ([`Program::unheld`]).
    mixes: Vec<u32>,
}

fn cell_of(p: P3) -> [i32; 3] {
    std::array::from_fn(|k| (p[k] / CELL).floor() as i32)
}

use crate::strokes::capsule;

/// What the layers paint at a point with no part holding them ([`Program::unheld`]): the
/// material, a blend's other one and its share, and how much the field is there (0..1: a grown
/// pattern's fades out at its band's edge).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unheld {
    pub slot: u8,
    pub mix: Option<(u8, f32)>,
    pub presence: f32,
}

/// What a point reads on one channel: its slot, and a lower bound of the distance to the nearest
/// boundary that changes it (mm; the views' edge; 0 inside a mix, which may change anywhere).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub slot: u8,
    pub dist: f64,
    /// Where a mix blends two materials: the other slot and its share (over 0, at most one
    /// half): a print dithers it, a view that rounds it shows `slot`.
    pub mix: Option<(u8, f32)>,
}

impl Program {
    pub fn new(layers: Vec<Layer>) -> Program {
        let mut p = Program { steep: vec![1.0; layers.len()], layers, ..Default::default() };
        let grow = |lo: P3, hi: P3, b: &mut Option<(P3, P3)>| {
            *b = Some(match *b {
                None => (lo, hi),
                Some((a, c)) => (std::array::from_fn(|k| a[k].min(lo[k])), std::array::from_fn(|k| c[k].max(hi[k]))),
            });
        };
        let mut bounds = None;
        let mut channels = [(None, false); 5];
        p.mixes = p.layers.iter().enumerate().filter(|(_, l)| matches!(l.op, Op::Mix { .. })).map(|(li, _)| li as u32).collect();
        for (li, l) in p.layers.iter().enumerate() {
            let li = li as u32;
            let channel = &mut channels[l.op.channel().index()];
            match &l.region {
                // a strokes file: its own brick index answers every point
                Region::All | Region::Strokes { .. } => {
                    p.global.push(li);
                    channel.1 = true;
                }
                Region::Field { node, grow: g } => match node_bounds(node) {
                    Some((lo, hi)) => {
                        p.steep[li as usize] = makepad_csg_sdf::SdfShape { root: node.clone(), min: lo, max: hi, materials: Vec::new() }.lipschitz();
                        let r = g + CELL;
                        let (lo, hi): (P3, P3) = (std::array::from_fn(|k| lo[k] - r), std::array::from_fn(|k| hi[k] + r));
                        grow(lo, hi, &mut bounds);
                        grow(lo, hi, &mut channel.0);
                        list(&mut p.grid, &mut p.wide, lo, hi, li);
                    }
                    None => {
                        p.global.push(li);
                        channel.1 = true;
                    }
                },
                Region::Operand(of) => match operands_bounds(of) {
                    Some((lo, hi)) => {
                        let (lo, hi): (P3, P3) = (lo.map(|v| v - CELL), hi.map(|v| v + CELL));
                        grow(lo, hi, &mut bounds);
                        grow(lo, hi, &mut channel.0);
                        list(&mut p.grid, &mut p.wide, lo, hi, li);
                    }
                    None => {
                        p.global.push(li);
                        channel.1 = true;
                    }
                },
                Region::Stroke { points, radius } => {
                    let r = radius + CELL;
                    for s in 0..points.len().max(1) {
                        let a = points[s.min(points.len() - 1)];
                        let b = points[(s + 1).min(points.len() - 1)];
                        let lo: P3 = std::array::from_fn(|k| a[k].min(b[k]) - r);
                        let hi: P3 = std::array::from_fn(|k| a[k].max(b[k]) + r);
                        grow(lo, hi, &mut bounds);
                        grow(lo, hi, &mut channel.0);
                        insert(&mut p.grid, lo, hi, Entry { layer: li, seg: s as u32 });
                        if points.len() < 2 {
                            break;
                        }
                    }
                }
                // unresolved: holds nowhere
                Region::Flood { .. } => {}
                Region::Patch(patch) => {
                    let (lo, hi) = patch.bounds();
                    let r = crate::flood::REACH_MM + CELL;
                    let (lo, hi): (P3, P3) = (std::array::from_fn(|k| lo[k] - r), std::array::from_fn(|k| hi[k] + r));
                    grow(lo, hi, &mut bounds);
                    grow(lo, hi, &mut channel.0);
                    list(&mut p.grid, &mut p.wide, lo, hi, li);
                }
                Region::Facets { facets, state, frames } => {
                    let Some((lo, hi)) = facets.bounds(*state) else { continue };
                    let places: Vec<_> = frames.iter().rev().map(|&(at, rotate, scale)| SdfPlace { at, rotate: quat_from_degrees(rotate), scale }).collect();
                    let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
                    for corner in 0..8 {
                        let mut q = std::array::from_fn(|k| if corner & (1 << k) == 0 { lo[k] } else { hi[k] });
                        for place in &places {
                            q = place.out_of(q);
                        }
                        for k in 0..3 {
                            min[k] = min[k].min(q[k]);
                            max[k] = max[k].max(q[k]);
                        }
                    }
                    let size: f64 = frames.iter().map(|f| f.2).product();
                    // The site's search reach is in asset millimetres; the grid is in model millimetres.
                    let r = crate::flood::REACH_MM * size + CELL;
                    let (lo, hi): (P3, P3) = (min.map(|v| v - r), max.map(|v| v + r));
                    grow(lo, hi, &mut bounds);
                    grow(lo, hi, &mut channel.0);
                    list(&mut p.grid, &mut p.wide, lo, hi, li);
                }
            }
        }
        p.bounds = bounds;
        p.channels = channels;
        p
    }

    /// The box layer `li` holds in (grown by a cell), for a field's or a patch's region; `None`
    /// for one that may hold anywhere.
    pub fn layer_box(&self, li: usize) -> Option<(P3, P3)> {
        let (lo, hi, r) = match &self.layers.get(li)?.region {
            Region::Field { node, grow } => {
                let (lo, hi) = node_bounds(node)?;
                (lo, hi, grow + CELL)
            }
            Region::Patch(patch) => {
                let (lo, hi) = patch.bounds();
                (lo, hi, crate::flood::REACH_MM + CELL)
            }
            Region::Operand(of) => {
                let (lo, hi) = operands_bounds(of)?;
                (lo, hi, CELL)
            }
            _ => return None,
        };
        Some((lo.map(|v| v - r), hi.map(|v| v + r)))
    }

    /// Where layer `li` can read differently from `old`'s layer `oi` (the same layer an edit
    /// apart): boxes (the part's frame) given to `found`, outside which every point samples the
    /// same from it, slot and edge distance ([`Program::sample`]); a box `found` holds already
    /// is not searched. A feature's or a field's region whose field moved within its box changes
    /// it in boxes about a cell a side round where it moved; any other change all of its box,
    /// old and new; none when the two are alike. False when it may read differently anywhere (a
    /// layer with no box).
    pub fn layer_changed(&self, li: usize, old: &Program, oi: usize, found: &mut dyn Changes) -> bool {
        let (a, b) = (&self.layers[li], &old.layers[oi]);
        let (now, was) = (self.layer_box(li), old.layer_box(oi));
        let whole = |found: &mut dyn Changes| match (now, was) {
            (Some(x), Some(y)) => {
                found.add(x);
                found.add(y);
                true
            }
            _ => false,
        };
        let bits = |b: Option<(P3, P3)>| b.map(|(lo, hi)| (lo.map(f64::to_bits), hi.map(f64::to_bits)));
        // a layer holds only in the grid's cells its box meets: past them, whatever it is
        if format!("{:?}", a.op) != format!("{:?}", b.op) || bits(now) != bits(was) {
            return whole(found);
        }
        // a sample's edge distance is at most its brick's floor (under a brick and a half,
        // [`crate::strokes::floor_of`]): past that, a value reads only by its side
        let cap = 1.5 * crate::strokes::BRICK;
        match (&a.region, &b.region) {
            (Region::Field { node: n, grow: g }, Region::Field { node: o, grow: h }) if g.to_bits() == h.to_bits() && self.steep[li].to_bits() == old.steep[oi].to_bits() => {
                if !same(n, o) {
                    let Some(within) = now else { return false };
                    let value = |c: &makepad_csg_sdf::Cut, steep: f64| (c.lo / steep - g, c.hi / steep - g);
                    let apart = |(x, y): &(makepad_csg_sdf::Cut, makepad_csg_sdf::Cut)| {
                        let (p, q) = (side(value(x, self.steep[li]), cap), side(value(y, old.steep[oi]), cap));
                        !(p.is_some() && p == q) && !same(&x.node, &y.node)
                    };
                    apart_boxes(within, &(cut(n, &within), cut(o, &within)), CELL, &|(x, y), b| (cut(&x.node, b), cut(&y.node, b)), &apart, found);
                }
                true
            }
            (Region::Operand(n), Region::Operand(o)) if n.len() == o.len() => {
                let Some(within) = now else { return false };
                // (a pair not made alike checks nothing: the layer's whole box then)
                if n.iter().zip(o.iter()).all(|(x, y)| x.alike_made(y)) {
                    for (x, y) in n.iter().zip(o.iter()) {
                        x.changed(y, &within, CELL, cap, found);
                    }
                    true
                } else {
                    whole(found)
                }
            }
            // (a feature's operands show only their boxes: read above, never by their text)
            (Region::Operand(_), _) | (_, Region::Operand(_)) => whole(found),
            (x, y) if format!("{x:?}") == format!("{y:?}") => true,
            _ => whole(found),
        }
    }

    /// The box every boxed layer lies in (grown by a cell); `None` when only whole-part layers.
    pub fn bounds(&self) -> Option<(P3, P3)> {
        self.bounds
    }

    /// The box `channel`'s layers hold in (grown by a cell); `None` when one may hold anywhere.
    pub fn channel_bounds(&self, channel: Channel) -> Option<(P3, P3)> {
        let (mut bounds, anywhere) = self.channels[channel.index()];
        if anywhere {
            // A strokes file holds only where its paint strokes reach; a whole-part layer anywhere.
            for l in self.layers.iter().filter(|l| l.op.channel() == channel) {
                match &l.region {
                    Region::All => return None,
                    Region::Field { node, .. } if node_bounds(node).is_none() => return None,
                    Region::Operand(of) if operands_bounds(of).is_none() => return None,
                    Region::Strokes { strokes, .. } => {
                        for (si, _) in strokes.applied().filter(|(_, s)| s.op.is_paint()) {
                            for (lo, hi) in strokes.boxes(si..si + 1) {
                                bounds = Some(match bounds {
                                    None => (lo, hi),
                                    Some((a, b)) => (std::array::from_fn(|k| a[k].min(lo[k])), std::array::from_fn(|k| b[k].max(hi[k]))),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // Only floods that reach nothing, or strokes files holding none: nowhere.
        Some(bounds.unwrap_or(([0.0; 3], [0.0; 3])))
    }

    /// Layer `li`'s own region at `p`, as though no layer lay over it: negative inside, its size a
    /// lower bound of the distance (mm) to the region's edge.
    pub fn region_distance(&self, li: usize, p: P3) -> f64 {
        match &self.layers[li].region {
            Region::Field { node, grow } => distance(node, Vec3d::new(p[0], p[1], p[2])) / self.steep[li] - grow,
            region => region.distance(p),
        }
    }

    /// `channel`'s paint as the colour, in slot 1 where it holds: what a view shows of a
    /// support channel. `None` when no layer writes it.
    pub fn shown(&self, channel: Channel) -> Option<Program> {
        if !self.has(channel) {
            return None;
        }
        let layers = self.layers.iter().enumerate().filter(|(_, l)| l.op.channel() == channel).map(|(i, l)| {
            let op = match l.op {
                Op::Paint { .. } | Op::Mix { .. } => Op::Paint { channel: Channel::Color, slot: 1 },
                Op::Erase { .. } => Op::Erase { channel: Channel::Color },
            };
            (Some(i), Layer { region: l.region.clone(), op })
        });
        Some(self.derived(layers.collect()))
    }

    /// A program of some of this one's layers (each with its index here) and new ones (`None`),
    /// each of this one's as steep as it reads it here (one with no box is measured over the
    /// part: [`Program::resolve_slopes`]).
    fn derived(&self, layers: Vec<(Option<usize>, Layer)>) -> Program {
        let from: Vec<Option<usize>> = layers.iter().map(|(i, _)| *i).collect();
        let mut p = Program::new(layers.into_iter().map(|(_, l)| l).collect());
        for (k, i) in from.into_iter().enumerate() {
            if let Some(i) = i {
                p.steep[k] = self.steep[i];
            }
        }
        p
    }

    /// The program with `layers` on top, its own as steep as it reads them.
    pub fn with_layers_on_top(&self, layers: impl IntoIterator<Item = Layer>) -> Program {
        self.derived(self.layers.iter().cloned().enumerate().map(|(i, l)| (Some(i), l)).chain(layers.into_iter().map(|l| (None, l))).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// The slot a part takes whole: the last colour layer that paints all of the part one
    /// slot, and the program of what follows it (the colour layers after it, the other
    /// channels as they are). `None` when no layer paints the whole part, or when an erase or
    /// a strokes file after it may take a point back to the part's own colour.
    pub fn whole(&self) -> Option<(u8, Program)> {
        let (last, slot) = self.layers.iter().enumerate().rev().find_map(|(i, l)| match (&l.region, &l.op) {
            (Region::All, Op::Paint { channel: Channel::Color, slot }) => Some((i, *slot)),
            _ => None,
        })?;
        let back = |l: &Layer| l.op.channel() == Channel::Color && (matches!(l.op, Op::Erase { .. }) || matches!(l.region, Region::Strokes { .. }));
        if self.layers[last + 1..].iter().any(back) {
            return None;
        }
        let rest = self.layers.iter().enumerate().filter(|(i, l)| *i > last || l.op.channel() != Channel::Color).map(|(i, l)| (Some(i), l.clone())).collect();
        Some((slot, self.derived(rest)))
    }

    /// Whether any layer writes `channel`.
    pub fn has(&self, channel: Channel) -> bool {
        self.layers.iter().any(|l| l.op.channel() == channel)
    }

    /// The distinct layers (and capsules) that can reach a point, last layer first. A feature's
    /// region past `enough` may read as a bound of it past that.
    fn layer_distance(&self, li: u32, entries: &[Entry], p: P3, enough: f64) -> f64 {
        match &self.layers[li as usize].region {
            Region::All => -1e9,
            Region::Field { node, grow } => distance(node, Vec3d::new(p[0], p[1], p[2])) / self.steep[li as usize] - grow,
            Region::Operand(of) => union_values(of, &[p], enough)[0],
            Region::Stroke { points, radius } => {
                let mut d = f64::MAX;
                for e in entries {
                    let s = e.seg as usize;
                    d = d.min(capsule(p, points[s], points[(s + 1).min(points.len() - 1)]));
                }
                d - radius
            }
            Region::Flood { .. } => 1e9,
            Region::Patch(patch) => patch.distance(p),
            r @ Region::Facets { .. } => r.distance(p),
            Region::Strokes { .. } => 1e9,
        }
    }

    /// A steep field layer's distance to its boundary at `p` by its local slope (central
    /// differences a fraction of `near` apart), at least its Lipschitz bound's; `None` for any
    /// other layer (a distance already).
    fn field_slope_distance(&self, li: u32, p: P3, near: f64) -> Option<f64> {
        let steep = self.steep[li as usize];
        if let Region::Operand(of) = &self.layers[li as usize].region {
            return of.iter().any(Operand::sloped).then(|| sloped_distances(of, &[p], near)[0]);
        }
        let Region::Field { node, grow } = &self.layers[li as usize].region else { return None };
        if steep <= 1.0 + 1e-9 {
            return None;
        }
        let f = |q: P3| distance(node, Vec3d::new(q[0], q[1], q[2]));
        let h = near * 0.1;
        let g: [f64; 3] = std::array::from_fn(|k| {
            let (mut a, mut b) = (p, p);
            a[k] += h;
            b[k] -= h;
            (f(a) - f(b)) / (2.0 * h)
        });
        let slope = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().clamp(1e-6, steep);
        Some((f(p) - grow * steep).abs() / slope)
    }

    /// Whether a field region has no box of its own ([`Program::resolve_slopes`]).
    pub fn has_unboxed_fields(&self) -> bool {
        self.layers.iter().any(|l| matches!(&l.region, Region::Field { node, .. } if node_bounds(node).is_none()))
    }

    /// The program with each field region that has no box of its own as steep as it is over
    /// the part (`part` its box in printer mm, `from` printer to the program's frame), as a
    /// boxed one is over its own box. Read as a distance, a scripted function may run steeper
    /// than one: its edge reads farther off than it is, and the bake's test for a brick of one
    /// slot passes a brick its edge crosses. Measured over where an edge distance is read: the
    /// part's box grown by a brick and a half (the most a sample's distance counts,
    /// [`crate::strokes::floor_of`]).
    pub fn resolve_slopes(&self, part: (P3, P3), from: &[[f64; 4]; 3]) -> Program {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for corner in 0..8 {
            let q: P3 = std::array::from_fn(|k| if corner & (1 << k) == 0 { part.0[k] } else { part.1[k] });
            for r in 0..3 {
                let v = from[r][0] * q[0] + from[r][1] * q[1] + from[r][2] * q[2] + from[r][3];
                (lo[r], hi[r]) = (lo[r].min(v), hi[r].max(v));
            }
        }
        let reach = 1.5 * crate::strokes::BRICK;
        let (lo, hi) = (lo.map(|v| v - reach), hi.map(|v| v + reach));
        let mut p = self.clone();
        for (li, l) in self.layers.iter().enumerate() {
            if let Region::Field { node, .. } = &l.region {
                if node_bounds(node).is_none() {
                    p.steep[li] = makepad_csg_sdf::SdfShape { root: node.clone(), min: lo, max: hi, materials: Vec::new() }.lipschitz();
                }
            }
        }
        p
    }

    /// Whether a layer is a flood still to resolve on the part's surface.
    pub fn has_floods(&self) -> bool {
        self.layers.iter().any(|l| matches!(l.region, Region::Flood { .. }))
    }

    /// The program with each flood resolved by `resolve` (seed, angle) into its patch on the
    /// part (`None`: the seed is not on this part, the layer holds nothing).
    pub fn resolve_floods(&self, resolve: &mut dyn FnMut(P3, f64) -> Option<Patch>) -> Program {
        self.derived(
            self.layers
                .iter()
                .enumerate()
                .map(|(i, l)| match l.region {
                    Region::Flood { seed, angle } => (None, Layer { region: resolve(seed, angle).map_or(l.region.clone(), |p| Region::Patch(Arc::new(p))), op: l.op.clone() }),
                    _ => (Some(i), l.clone()),
                })
                .collect(),
        )
    }

    /// Whether a layer reads a strokes file.
    pub fn has_strokes(&self) -> bool {
        self.layers.iter().any(|l| matches!(l.region, Region::Strokes { .. }))
    }

    /// The program with each strokes layer's log (and its palette map) from `bind` (file and the
    /// channel it paints -> log and slots; `None`: kept as it is). The grid is shared: strokes are
    /// not in it.
    pub fn with_strokes(&self, bind: &mut dyn FnMut(&str, Channel) -> Option<(Arc<Strokes>, Arc<Vec<u8>>)>) -> Program {
        let mut p = self.clone();
        for l in &mut p.layers {
            let channel = l.op.channel();
            if let Region::Strokes { file, strokes, slots } = &mut l.region {
                if let Some((s, m)) = bind(file, channel) {
                    *strokes = s;
                    *slots = m;
                }
            }
        }
        p
    }

    /// The log a strokes layer reads, to add to in place (a stroke being drawn).
    pub fn strokes_mut(&mut self, file: &str) -> Option<&mut Arc<Strokes>> {
        self.layers.iter_mut().find_map(|l| match &mut l.region {
            Region::Strokes { file: f, strokes, .. } if f == file => Some(strokes),
            _ => None,
        })
    }

    /// The palette map of a strokes layer, to add to in place.
    pub fn strokes_slots_mut(&mut self, file: &str) -> Option<&mut Arc<Vec<u8>>> {
        self.layers.iter_mut().find_map(|l| match &mut l.region {
            Region::Strokes { file: f, slots, .. } if f == file => Some(slots),
            _ => None,
        })
    }

    /// The strokes files the layers read, with their logs.
    pub fn strokes_files(&self) -> impl Iterator<Item = (&str, &Arc<Strokes>)> {
        self.layers.iter().filter_map(|l| match &l.region {
            Region::Strokes { file, strokes, .. } => Some((file.as_str(), strokes)),
            _ => None,
        })
    }

    /// When `next` is this program with only its strokes logs moved (the same layers
    /// otherwise): the capsule boxes that changed. `None`: anything else changed.
    pub fn strokes_change(&self, next: &Program) -> Option<Vec<(P3, P3)>> {
        if self.layers.len() != next.layers.len() {
            return None;
        }
        let mut boxes = Vec::new();
        for (a, b) in self.layers.iter().zip(&next.layers) {
            match (&a.region, &b.region) {
                (Region::Strokes { file: fa, strokes: sa, slots: ma }, Region::Strokes { file: fb, strokes: sb, slots: mb }) => {
                    if fa != fb || a.op != b.op || !mb.starts_with(ma) {
                        return None;
                    }
                    boxes.extend(sa.changed_boxes(sb, true));
                }
                (Region::Patch(pa), Region::Patch(pb)) if Arc::ptr_eq(pa, pb) => {}
                (Region::Patch(_), _) | (_, Region::Patch(_)) => return None,
                _ => {
                    if a.op != b.op || format!("{:?}", a.region) != format!("{:?}", b.region) {
                        return None;
                    }
                }
            }
        }
        Some(boxes)
    }

    /// Where this program can paint otherwise than `old` (this program before an edit of its
    /// part's field, the same layers over the edited shape): boxes in its frame. A field's
    /// region changes where its field does ([`makepad_csg_sdf::changed`], grown by the region's
    /// own growth), a feature's where an operand on its way does ([`Operand::changed_from`]), and
    /// a layer whose box moved changes in the grid cells only one of its boxes lists it in (it
    /// is read only where it is listed). Each box is where a region's surface and its values near it can move:
    /// a reader grows it by how far off a region it reads. `None`: anything else changed (the
    /// layers, an op, a strokes log, a flood): paint it anew.
    pub fn changed_from(&self, old: &Program) -> Option<Vec<(P3, P3)>> {
        if self.layers.len() != old.layers.len() || self.steep != old.steep {
            return None;
        }
        let mut out = Vec::new();
        for (li, (a, b)) in self.layers.iter().zip(&old.layers).enumerate() {
            if a.op != b.op {
                return None;
            }
            match (&a.region, &b.region) {
                (Region::All, Region::All) => continue,
                (Region::Field { node: n, grow: g }, Region::Field { node: m, grow: h }) if g == h => {
                    let Some((lo, hi)) = makepad_csg_sdf::changed(m, n) else { continue };
                    if lo.iter().chain(&hi).any(|v| !v.is_finite()) {
                        return None;
                    }
                    out.push((lo.map(|v| v - g.abs()), hi.map(|v| v + g.abs())));
                }
                (Region::Operand(x), Region::Operand(y)) if x.len() == y.len() => {
                    if Arc::ptr_eq(x, y) {
                        continue;
                    }
                    for (p, q) in x.iter().zip(y.iter()) {
                        out.extend(p.changed_from(q)?);
                    }
                }
                (Region::Stroke { points: p, radius: r }, Region::Stroke { points: q, radius: s }) if p == q && r == s => continue,
                (Region::Strokes { file: f, strokes: x, slots: m }, Region::Strokes { file: g, strokes: y, slots: n }) if f == g && Arc::ptr_eq(x, y) && m == n => continue,
                (Region::Flood { seed: p, angle: x }, Region::Flood { seed: q, angle: y }) if p == q && x == y => continue,
                (Region::Patch(x), Region::Patch(y)) if Arc::ptr_eq(x, y) => continue,
                (Region::Facets { facets: x, state: s, frames: f }, Region::Facets { facets: y, state: t, frames: g }) if Arc::ptr_eq(x, y) && s == t && f == g => continue,
                _ => return None,
            }
            match (self.layer_box(li).map(cells_of), old.layer_box(li).map(cells_of)) {
                (Some(x), Some(y)) if x != y => out.extend(outside(x, y).into_iter().chain(outside(y, x))),
                (Some(_), Some(_)) | (None, None) => {}
                _ => return None,
            }
        }
        Some(out)
    }

    /// The program with a strokes layer on top reading `strokes` (a part's first stroke, before
    /// its script reads the file).
    pub fn with_strokes_on_top(&self, file: &str, strokes: Arc<Strokes>, slots: Arc<Vec<u8>>) -> Program {
        let mut p = self.clone();
        p.global.push(p.layers.len() as u32);
        p.steep.push(1.0);
        p.channels[Channel::Color.index()].1 = true;
        p.layers.push(Layer { region: Region::Strokes { file: file.to_string(), strokes, slots }, op: Op::Paint { channel: Channel::Color, slot: 0 } });
        p
    }

    /// Whether a mix grows a pattern on the part.
    pub fn has_patterns(&self) -> bool {
        self.patterns().next().is_some()
    }

    /// The patterns its mixes grow, in layer order.
    pub fn patterns(&self) -> impl Iterator<Item = &crate::pattern::Grown> {
        self.layers.iter().filter_map(|l| match &l.op {
            Op::Mix { mix, .. } => mix.pattern(),
            _ => None,
        })
    }

    /// Whether a pattern shows a state on its way, not the one that prints.
    pub fn patterns_pending(&self) -> bool {
        self.patterns().any(|g| !g.done())
    }

    /// The finest lattice its growing patterns' latest states are on (mm): a view of a pattern
    /// still on its way needs none finer. `None` when none grows.
    pub fn growing_pitch(&self) -> Option<f64> {
        self.patterns().filter(|g| g.sim.is_some()).filter_map(|g| g.grid.as_ref().map(|grid| grid.layout.pitch)).reduce(f64::min)
    }

    /// Whether it paints as `o` does but for how far its patterns have grown: the same layers,
    /// the same patterns asked for.
    pub fn same_but_patterns(&self, o: &Program) -> bool {
        let bare = |p: &Program| p.resolve_patterns(&mut |g| crate::pattern::Grown::new(g.spec.clone()));
        let (a, b) = (bare(self), bare(o));
        a.layers.len() == b.layers.len() && a.layers.iter().zip(&b.layers).all(|(x, y)| x.op == y.op && format!("{:?}", x.region) == format!("{:?}", y.region))
    }

    /// Whether a mix's field is a raster.
    pub fn has_rasters(&self) -> bool {
        self.layers.iter().any(|l| matches!(&l.op, Op::Mix { mix, .. } if matches!(mix.field, MixField::Raster(_))))
    }

    /// The program with each raster made as `make` makes it from what makes it: its grid and
    /// the slots its places are (none: kept as it is, the first material all through).
    pub fn resolve_rasters(&self, make: &mut dyn FnMut(&crate::raster::Raster) -> Option<(Arc<crate::raster::RasterGrid>, Vec<u8>)>) -> Program {
        let mut p = self.clone();
        for l in &mut p.layers {
            if let Op::Mix { mix, .. } = &mut l.op {
                if let MixField::Raster(r) = &mix.field {
                    if let Some((grid, slots)) = make(r) {
                        let spec = r.spec.clone();
                        let m = Arc::make_mut(mix);
                        m.field = MixField::Raster(crate::raster::Raster { spec, grid: Some(grid) });
                        m.slots = slots;
                    }
                }
            }
        }
        p
    }

    /// The program with each pattern as `grow` gives it (the part's surface grows it).
    pub fn resolve_patterns(&self, grow: &mut dyn FnMut(&crate::pattern::Grown) -> crate::pattern::Grown) -> Program {
        let mut p = self.clone();
        for l in &mut p.layers {
            if let Op::Mix { mix, .. } = &mut l.op {
                if let MixField::Pattern(g) = &mix.field {
                    let grown = grow(g);
                    Arc::make_mut(mix).field = MixField::Pattern(grown);
                }
            }
        }
        p
    }

    /// The channel at `p`: the last layer holding it decides; the distance is to the nearest
    /// boundary at or above that layer (a lower layer's edge is covered).
    pub fn sample(&self, channel: Channel, p: P3) -> Sample {
        self.sample_near(channel, p, 0.0)
    }

    /// [`Program::sample`] with the distance to a steep field region's boundary measured, where
    /// it is under `near` (mm), by the field's own slope there instead of its steepest anywhere:
    /// the lower bound puts a noise region's edge several times too close, and a view that
    /// ramps its edge over that distance smears it into a wide blend of the two colours. The
    /// estimate (the value over the local gradient: exact for a plane) is no lower bound, so
    /// a test that must not miss a boundary keeps [`Program::sample`].
    pub fn sample_near(&self, channel: Channel, p: P3, near: f64) -> Sample {
        self.read(channel, p, near, f64::INFINITY)
    }

    /// [`Program::sample`] where only whether a boundary is within `reach` counts: a distance
    /// past it may read as any lower bound of it that is still past it, so a feature's region
    /// that far off is not read (a bake's test for a brick of one slot).
    pub fn sample_within(&self, channel: Channel, p: P3, reach: f64) -> Sample {
        self.read(channel, p, 0.0, reach)
    }

    /// The wide regions whose cells hold one of the cells `at`.
    fn wide_at<'a>(&'a self, at: &'a [[i32; 3]]) -> impl Iterator<Item = u32> + 'a {
        self.wide.iter().filter(|w| at.iter().any(|c| w.holds(*c))).map(|w| w.layer)
    }

    fn read(&self, channel: Channel, p: P3, near: f64, enough: f64) -> Sample {
        let at = cell_of(p);
        let cell: &[Entry] = self.grid.get(&at).map_or(&[], |v| v.as_slice());
        // what the cell's list leaves out is a cell past its faces
        let mut dist = crate::strokes::floor_of(p);
        // Walk layers from the last: the cell's entries (sorted by layer), the wide regions whose
        // cells hold it and the global list.
        let mut wide = self.wide.iter().rev().filter(|w| w.holds(at)).map(|w| w.layer).peekable();
        let (mut ci, mut gi) = (cell.len(), self.global.len());
        loop {
            let cl = if ci > 0 { Some(cell[ci - 1].layer) } else { None };
            let gl = if gi > 0 { Some(self.global[gi - 1]) } else { None };
            let wl = wide.peek().copied();
            let Some(li) = cl.max(gl).max(wl) else { break };
            let mut from = ci;
            while from > 0 && cell[from - 1].layer == li {
                from -= 1;
            }
            let entries = &cell[from..ci];
            ci = from;
            if gl == Some(li) {
                gi -= 1;
            }
            if wl == Some(li) {
                wide.next();
            }
            let layer = &self.layers[li as usize];
            if layer.op.channel() != channel {
                continue;
            }
            if let Region::Strokes { strokes, slots, .. } = &layer.region {
                let h = strokes.sample(p);
                dist = dist.min(h.dist);
                match h.paint {
                    Some(slot) => return Sample { slot: slot.and_then(|s| slots.get(s as usize).copied()).unwrap_or(0), dist, mix: None },
                    None => continue,
                }
            }
            let d = self.layer_distance(li, entries, p, enough);
            let at = if d.abs() < near { self.field_slope_distance(li, p, near).unwrap_or(d.abs()) } else { d.abs() };
            dist = dist.min(at);
            if d <= 0.0 {
                return match &layer.op {
                    Op::Paint { slot, .. } => Sample { slot: *slot, dist, mix: None },
                    Op::Erase { .. } => Sample { slot: 0, dist, mix: None },
                    Op::Mix { mix, .. } => {
                        let (slot, blend) = mix.at(p);
                        // a pattern is one material away from its cells, to the nearest of them
                        let dist = if blend.is_none() { dist.min(mix.clear_distance(p)) } else { 0.0 };
                        Sample { slot, dist, mix: blend }
                    }
                };
            }
        }
        Sample { slot: 0, dist, mix: None }
    }
}

impl Program {
    /// [`Program::sample_near`] at many points (nearby ones, a brick's), each the same as one
    /// read alone: each layer is read for all the points it may still decide at once, so a
    /// kernel (a scripted region, its slope, a fn's mix) runs once for them. With `near` over 0 a
    /// distance at or past it may read as any bound under it that is still at least `near` (a
    /// view ramps an edge over `near`: those all show alike), so a feature's region that is that
    /// far from all of them is not read.
    pub fn sample_near_many(&self, channel: Channel, ps: &[P3], near: f64) -> Vec<Sample> {
        let at: Vec<[i32; 3]> = ps.iter().map(|p| cell_of(*p)).collect();
        let cells: Vec<&[Entry]> = at.iter().map(|c| self.grid.get(c).map_or(&[][..], |v| v.as_slice())).collect();
        // what a cell's list leaves out is a cell past its faces
        let mut out: Vec<Sample> = ps.iter().map(|p| Sample { slot: 0, dist: crate::strokes::floor_of(*p), mix: None }).collect();
        let mut layers: Vec<u32> = cells.iter().flat_map(|c| c.iter().map(|e| e.layer)).chain(self.global.iter().copied()).chain(self.wide_at(&at)).collect();
        layers.sort_unstable();
        layers.dedup();
        // the points no layer has decided yet
        let mut open: Vec<usize> = (0..ps.len()).collect();
        for &li in layers.iter().rev() {
            if open.is_empty() {
                break;
            }
            let layer = &self.layers[li as usize];
            if layer.op.channel() != channel {
                continue;
            }
            let global = self.global.contains(&li);
            let wide = self.wide.iter().find(|w| w.layer == li);
            let here: Vec<(usize, &[Entry])> = open
                .iter()
                .filter_map(|&i| {
                    let c = cells[i];
                    let (from, to) = (c.partition_point(|e| e.layer < li), c.partition_point(|e| e.layer <= li));
                    (to > from || global || wide.is_some_and(|w| w.holds(at[i]))).then(|| (i, &c[from..to]))
                })
                .collect();
            if here.is_empty() {
                continue;
            }
            let mut decided = vec![false; ps.len()];
            if let Region::Strokes { strokes, slots, .. } = &layer.region {
                for (i, _) in &here {
                    let h = strokes.sample(ps[*i]);
                    out[*i].dist = out[*i].dist.min(h.dist);
                    if let Some(slot) = h.paint {
                        out[*i].slot = slot.and_then(|s| slots.get(s as usize).copied()).unwrap_or(0);
                        decided[*i] = true;
                    }
                }
                open.retain(|i| !decided[*i]);
                continue;
            }
            // each point's distance to the region, and the distance it adds: a steep field's by
            // its local slope where the boundary is within `near`
            let (d, at): (Vec<f64>, Vec<f64>) = match &layer.region {
                Region::Field { node, grow } => {
                    let steep = self.steep[li as usize];
                    let v = |p: P3| Vec3d::new(p[0], p[1], p[2]);
                    let mut raw = vec![0.0; here.len()];
                    distance_many(node, &here.iter().map(|(i, _)| v(ps[*i])).collect::<Vec<_>>(), &mut raw);
                    let d: Vec<f64> = raw.iter().map(|f| f / steep - grow).collect();
                    let mut at: Vec<f64> = d.iter().map(|d| d.abs()).collect();
                    let sloped: Vec<usize> = if steep > 1.0 + 1e-9 { (0..here.len()).filter(|&k| d[k].abs() < near).collect() } else { Vec::new() };
                    if !sloped.is_empty() {
                        // the slope by central differences a tenth of `near` apart, as one read
                        let h = near * 0.1;
                        let probes: Vec<Vec3d> = sloped
                            .iter()
                            .flat_map(|&k| {
                                let p = ps[here[k].0];
                                (0..3).flat_map(move |axis| {
                                    [h, -h].map(|o| {
                                        let mut q = p;
                                        q[axis] += o;
                                        v(q)
                                    })
                                })
                            })
                            .collect();
                        let mut f = vec![0.0; probes.len()];
                        distance_many(node, &probes, &mut f);
                        for (n, &k) in sloped.iter().enumerate() {
                            let g: [f64; 3] = std::array::from_fn(|axis| (f[n * 6 + axis * 2] - f[n * 6 + axis * 2 + 1]) / (2.0 * h));
                            let slope = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().clamp(1e-6, steep);
                            at[k] = (raw[k] - grow * steep).abs() / slope;
                        }
                    }
                    (d, at)
                }
                Region::Operand(of) => {
                    let pts: Vec<P3> = here.iter().map(|(i, _)| ps[*i]).collect();
                    // past `near` only that the distance is that far counts
                    let d = union_values(of, &pts, if near > 0.0 { near } else { f64::INFINITY });
                    let mut at: Vec<f64> = d.iter().map(|d| d.abs()).collect();
                    // its value is a distance over the slopes of what it takes: read the distance to
                    // its edge by its local slope where the edge is within `near`
                    let sloped: Vec<usize> = if of.iter().any(Operand::sloped) { (0..d.len()).filter(|&k| d[k].abs() < near).collect() } else { Vec::new() };
                    if !sloped.is_empty() {
                        let by_slope = sloped_distances(of, &sloped.iter().map(|&k| pts[k]).collect::<Vec<_>>(), near);
                        for (&k, a) in sloped.iter().zip(by_slope) {
                            at[k] = a;
                        }
                    }
                    (d, at)
                }
                _ => {
                    let d: Vec<f64> = here.iter().map(|(i, e)| self.layer_distance(li, e, ps[*i], f64::INFINITY)).collect();
                    let at = d.iter().map(|d| d.abs()).collect();
                    (d, at)
                }
            };
            let mut mixed: Vec<usize> = Vec::new();
            for (k, (i, _)) in here.iter().enumerate() {
                out[*i].dist = out[*i].dist.min(at[k]);
                if d[k] <= 0.0 {
                    decided[*i] = true;
                    match &layer.op {
                        Op::Paint { slot, .. } => out[*i].slot = *slot,
                        Op::Erase { .. } => out[*i].slot = 0,
                        Op::Mix { .. } => mixed.push(*i),
                    }
                }
            }
            if let (Op::Mix { mix, .. }, false) = (&layer.op, mixed.is_empty()) {
                let places = mix.places(&mixed.iter().map(|i| ps[*i]).collect::<Vec<_>>());
                for (i, place) in mixed.into_iter().zip(places) {
                    let (slot, blend) = mix.at_place(place);
                    // a pattern is one material away from its cells, to the nearest of them
                    let dist = if blend.is_none() { out[i].dist.min(mix.clear_distance(ps[i])) } else { 0.0 };
                    out[i] = Sample { slot, dist, mix: blend };
                }
            }
            open.retain(|i| !decided[*i]);
        }
        out
    }

    /// What the layers paint at `p` on `channel` with no part holding them, for a view of the
    /// paint itself: the last layer reaching `p` decides. A mix's field reaches wherever it is
    /// defined, whatever its region (a function everywhere, a grown pattern in its band, fading
    /// at its edge), any other layer inside its region. Nothing where none reaches, or the last
    /// that does erases.
    pub fn unheld(&self, channel: Channel, p: P3) -> Option<Unheld> {
        self.unheld_many(channel, &[p])[0].0
    }

    /// [`Program::unheld`] at many points (nearby ones, a brick's), each with how far from it
    /// every point reads the same: the nearest edge of a layer it passed or stopped at, or of
    /// its cell of the layers' index (past it other layers may reach); 0 where a mix decides or
    /// was passed (its field and its pattern's band change anywhere). Each layer is read for
    /// all the points it may decide at once, so a kernel (a fn's mix, a scripted region) runs
    /// once for them.
    pub fn unheld_many(&self, channel: Channel, ps: &[P3]) -> Vec<(Option<Unheld>, f64)> {
        let at: Vec<[i32; 3]> = ps.iter().map(|p| cell_of(*p)).collect();
        let cells: Vec<&[Entry]> = at.iter().map(|c| self.grid.get(c).map_or(&[][..], |v| v.as_slice())).collect();
        let mut out: Vec<(Option<Unheld>, f64)> = ps.iter().map(|p| (None, crate::strokes::floor_of(*p))).collect();
        let mut layers: Vec<u32> = cells.iter().flat_map(|c| c.iter().map(|e| e.layer)).chain(self.global.iter().copied()).chain(self.mixes.iter().copied()).chain(self.wide_at(&at)).collect();
        layers.sort_unstable();
        layers.dedup();
        // the points no layer has decided yet
        let mut open: Vec<usize> = (0..ps.len()).collect();
        for &li in layers.iter().rev() {
            if open.is_empty() {
                break;
            }
            let layer = &self.layers[li as usize];
            if layer.op.channel() != channel {
                continue;
            }
            // the points it is listed for, each with its entries in its cell
            let everywhere = self.global.contains(&li) || self.mixes.contains(&li);
            let wide = self.wide.iter().find(|w| w.layer == li);
            let entries = |i: usize| -> Option<&[Entry]> {
                let c = cells[i];
                let from = c.partition_point(|e| e.layer < li);
                let to = c.partition_point(|e| e.layer <= li);
                (to > from || everywhere || wide.is_some_and(|w| w.holds(at[i]))).then(|| &c[from..to])
            };
            let here: Vec<(usize, &[Entry])> = open.iter().filter_map(|&i| entries(i).map(|e| (i, e))).collect();
            if here.is_empty() {
                continue;
            }
            let mut decided = vec![false; ps.len()];
            match &layer.op {
                Op::Mix { mix, .. } => {
                    let present: Vec<(usize, f64)> = here.iter().map(|(i, _)| (*i, mix.presence(ps[*i]))).collect();
                    for (i, _) in &present {
                        out[*i].1 = 0.0;
                    }
                    let present: Vec<(usize, f64)> = present.into_iter().filter(|(_, p)| *p > 0.0).collect();
                    let places = mix.places(&present.iter().map(|(i, _)| ps[*i]).collect::<Vec<_>>());
                    for ((i, presence), place) in present.into_iter().zip(places) {
                        let (slot, blend) = mix.at_place(place);
                        out[i].0 = Some(Unheld { slot, mix: blend, presence: presence as f32 });
                        decided[i] = true;
                    }
                }
                Op::Paint { .. } | Op::Erase { .. } => {
                    // what the layer gives where its region holds the point (an erase: nothing)
                    let painted = match &layer.op {
                        Op::Paint { slot, .. } => Some(Unheld { slot: *slot, mix: None, presence: 1.0 }),
                        _ => None,
                    };
                    match &layer.region {
                        Region::Strokes { strokes, slots, .. } => {
                            for (i, _) in &here {
                                let h = strokes.sample(ps[*i]);
                                out[*i].1 = out[*i].1.min(h.dist);
                                if let Some(s) = h.paint {
                                    out[*i].0 = s.map(|s| Unheld { slot: slots.get(s as usize).copied().unwrap_or(0), mix: None, presence: 1.0 });
                                    decided[*i] = true;
                                }
                            }
                        }
                        region => {
                            let d: Vec<f64> = match region {
                                Region::Field { node, grow } => {
                                    let at: Vec<Vec3d> = here.iter().map(|(i, _)| Vec3d::new(ps[*i][0], ps[*i][1], ps[*i][2])).collect();
                                    let mut d = vec![0.0; at.len()];
                                    distance_many(node, &at, &mut d);
                                    d.into_iter().map(|d| d / self.steep[li as usize] - grow).collect()
                                }
                                Region::Operand(of) => union_values(of, &here.iter().map(|(i, _)| ps[*i]).collect::<Vec<_>>(), f64::INFINITY),
                                _ => here.iter().map(|(i, e)| self.layer_distance(li, e, ps[*i], f64::INFINITY)).collect(),
                            };
                            for ((i, _), d) in here.iter().zip(d) {
                                out[*i].1 = out[*i].1.min(d.abs());
                                if d <= 0.0 {
                                    out[*i].0 = painted;
                                    decided[*i] = true;
                                }
                            }
                        }
                    }
                }
            }
            open.retain(|i| !decided[*i]);
        }
        out
    }
}

/// The box the features hold in (`None`: one may hold anywhere).
fn operands_bounds(of: &[Operand]) -> Option<(P3, P3)> {
    of.iter().try_fold(None::<(P3, P3)>, |acc, o| {
        let (lo, hi) = o.bounds?;
        Some(Some(match acc {
            None => (lo, hi),
            Some((a, b)) => (std::array::from_fn(|k| a[k].min(lo[k])), std::array::from_fn(|k| b[k].max(hi[k]))),
        }))
    })?
}

/// The distance to the features' edge at each point by the value's local slope (central
/// differences a tenth of `near` apart): exact for a plane, where the value's own slope may be
/// far under 1 (a fillet's operands meeting at an angle) and puts the edge too close.
fn sloped_distances(of: &[Operand], ps: &[P3], near: f64) -> Vec<f64> {
    let h = near * 0.1;
    let mut probes = Vec::with_capacity(ps.len() * 7);
    for p in ps {
        probes.push(*p);
        for axis in 0..3 {
            for o in [h, -h] {
                let mut q = *p;
                q[axis] += o;
                probes.push(q);
            }
        }
    }
    let v = union_values(of, &probes, f64::INFINITY);
    v.chunks_exact(7)
        .map(|c| {
            let g: [f64; 3] = std::array::from_fn(|axis| (c[1 + axis * 2] - c[2 + axis * 2]) / (2.0 * h));
            c[0].abs() / (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().clamp(1e-6, 1.0)
        })
        .collect()
}

/// The grid cells a box lists its layer in ([`insert`]), as the box they make.
fn cells_of((lo, hi): (P3, P3)) -> (P3, P3) {
    let (a, b) = (cell_of(lo), cell_of(hi));
    (a.map(|c| c as f64 * CELL), b.map(|c| (c + 1) as f64 * CELL))
}

/// Box `a` less box `b`, as the slabs of `a` past each of `b`'s faces (they overlap).
fn outside(a: (P3, P3), b: (P3, P3)) -> Vec<(P3, P3)> {
    if (0..3).any(|k| a.1[k] <= b.0[k] || a.0[k] >= b.1[k]) {
        return vec![a];
    }
    let mut out = Vec::new();
    for k in 0..3 {
        if a.0[k] < b.0[k] {
            let mut s = a;
            s.1[k] = b.0[k];
            out.push(s);
        }
        if a.1[k] > b.1[k] {
            let mut s = a;
            s.0[k] = b.1[k];
            out.push(s);
        }
    }
    out
}

/// Lists layer `li`'s region, its box `lo` to `hi`, in each cell the box touches; one that
/// touches more than [`WIDE_CELLS`] is kept as their range instead ([`Wide`]).
fn list(grid: &mut HashMap<[i32; 3], Vec<Entry>>, wide: &mut Vec<Wide>, lo: P3, hi: P3, li: u32) {
    let (first, last) = (cell_of(lo), cell_of(hi));
    let cells: i64 = (0..3).map(|k| (last[k] as i64 - first[k] as i64 + 1).max(0)).product();
    if cells > WIDE_CELLS {
        wide.push(Wide { layer: li, first, last });
    } else {
        insert(grid, lo, hi, Entry { layer: li, seg: 0 });
    }
}

fn insert(grid: &mut HashMap<[i32; 3], Vec<Entry>>, lo: P3, hi: P3, e: Entry) {
    let (a, b) = (cell_of(lo), cell_of(hi));
    for z in a[2]..=b[2] {
        for y in a[1]..=b[1] {
            for x in a[0]..=b[0] {
                let v = grid.entry([x, y, z]).or_default();
                // a stroke's capsules come in order: one entry per capsule
                v.push(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint(slot: u8) -> Op {
        Op::Paint { channel: Channel::Color, slot }
    }

    /// A region whose box reaches far past its part, over a plate and more, reads as its box holds
    /// a point, wherever the point is: inside it, on its faces and past them, under and over the
    /// strokes on it. One point read alone, many at once and unheld say the same.
    #[test]
    fn a_region_wider_than_the_plate_reads_as_its_box_holds_a_point() {
        let size = [700.0, 90.0, 500.0];
        let wide = Region::Field { node: SdfNode::Box { size, radius: 0.0 }, grow: 0.0 };
        let stroke = |y: f64| Region::Stroke { points: vec![[-20.0, y, 0.0], [20.0, y, 5.0]], radius: 3.0 };
        let p = Program::new(vec![
            Layer { region: Region::All, op: paint(1) },
            Layer { region: stroke(-10.0), op: paint(4) },
            Layer { region: wide, op: paint(2) },
            Layer { region: stroke(10.0), op: paint(3) },
        ]);
        let inside = |q: P3| (0..3).all(|k| q[k].abs() <= size[k] * 0.5);
        let on_stroke = |q: P3, y: f64| capsule(q, [-20.0, y, 0.0], [20.0, y, 5.0]) <= 3.0;
        // through the box and past its faces, a cell's fractions apart; and the strokes' ends
        let mut points: Vec<P3> = Vec::new();
        for x in [-360.0, -351.1, -350.02, -349.3, -120.7, -21.5, 0.4, 18.9, 349.9, 350.02, 350.6, 371.0] {
            for y in [-48.0, -45.03, -44.2, -10.0, -0.3, 10.0, 44.9, 45.03, 46.1] {
                for z in [-251.0, -250.04, -249.6, 0.0, 2.5, 4.9, 249.9, 250.04, 263.0] {
                    points.push([x, y, z]);
                }
            }
        }
        let one: Vec<Sample> = points.iter().map(|q| p.sample(Channel::Color, *q)).collect();
        for (q, read) in points.iter().zip(&one) {
            let want = if on_stroke(*q, 10.0) {
                3
            } else if inside(*q) {
                2
            } else if on_stroke(*q, -10.0) {
                4
            } else {
                1
            };
            assert_eq!(read.slot, want, "at {q:?}");
        }
        let many = p.sample_near_many(Channel::Color, &points, 0.0);
        let unheld = p.unheld_many(Channel::Color, &points);
        for (k, q) in points.iter().enumerate() {
            assert_eq!(many[k], one[k], "at {q:?}, read with the others");
            assert_eq!(unheld[k].0, p.unheld(Channel::Color, *q), "unheld at {q:?}");
        }
    }

    #[test]
    fn the_last_layer_wins_and_erase_exposes_the_base() {
        let p = Program::new(vec![
            Layer { region: Region::All, op: paint(1) },
            Layer { region: Region::Stroke { points: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]], radius: 2.0 }, op: paint(2) },
            Layer { region: Region::Field { node: SdfNode::Transform { at: [5.0, 0.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 1.0 }) }, grow: 0.0 }, op: Op::Erase { channel: Channel::Color } },
        ]);
        assert_eq!(p.sample(Channel::Color, [50.0, 50.0, 50.0]).slot, 1);
        let s = p.sample(Channel::Color, [2.0, 1.0, 0.0]);
        assert_eq!(s.slot, 2);
        assert!((s.dist - 1.0).abs() < 1e-9, "{s:?}");
        assert_eq!(p.sample(Channel::Color, [5.0, 0.0, 0.0]).slot, 0);
        assert_eq!(p.sample(Channel::SupportBlock, [2.0, 0.0, 0.0]).slot, 0);
    }

    /// A part painted whole takes that slot as its own; the paint after it stays, the paint
    /// before it is covered; an erase after it (back to the part's own) keeps it paint.
    #[test]
    fn a_part_painted_whole_takes_the_slot_and_keeps_what_follows() {
        let stroke = || Region::Stroke { points: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]], radius: 2.0 };
        let block = Layer { region: stroke(), op: Op::Paint { channel: Channel::SupportBlock, slot: 1 } };
        let p = Program::new(vec![Layer { region: stroke(), op: paint(3) }, block.clone(), Layer { region: Region::All, op: paint(1) }, Layer { region: stroke(), op: paint(2) }]);
        let (slot, rest) = p.whole().expect("painted whole");
        assert_eq!(slot, 1);
        assert_eq!(rest.layers.len(), 2, "the support block and the stroke after the fill");
        assert_eq!(rest.sample(Channel::Color, [2.0, 1.0, 0.0]).slot, 2);
        assert_eq!(rest.sample(Channel::Color, [50.0, 50.0, 50.0]).slot, 0, "the rest is the part's own: the slot");
        assert_eq!(rest.sample(Channel::SupportBlock, [2.0, 0.0, 0.0]).slot, 1);
        assert!(Program::new(vec![Layer { region: stroke(), op: paint(1) }]).whole().is_none(), "a stroke is not the whole part");
        let erased = Program::new(vec![Layer { region: Region::All, op: paint(1) }, Layer { region: stroke(), op: Op::Erase { channel: Channel::Color } }]);
        assert!(erased.whole().is_none(), "an erase after it shows the part's own colour");
    }

    /// A field of a point as a script's fn gives it (metres in, a place out).
    struct Field(fn(Vec3d) -> f64);
    impl makepad_csg_sdf::Sdf3 for Field {
        fn distance(&self, p: Vec3d) -> f64 {
            (self.0)(p)
        }
    }

    fn mix(f: fn(Vec3d) -> f64, slots: Vec<u8>, sharp: f64, balance: f64) -> Mix {
        let field = SdfKernel { key: 1, field: Arc::new(Field(f)), splash: None, bounds: None, solid: None };
        Mix { field: MixField::Fn(field), per_mm: 0.001, slots, sharp, balance }
    }

    fn is(got: (u8, Option<(u8, f32)>), slot: u8, mix: Option<(u8, f32)>) -> bool {
        let same = match (got.1, mix) {
            (None, None) => true,
            (Some((a, x)), Some((b, y))) => a == b && (x - y).abs() < 1e-5,
            _ => false,
        };
        let ok = got.0 == slot && same;
        if !ok {
            eprintln!("got {got:?}, wanted ({slot}, {mix:?})");
        }
        ok
    }

    /// A place between two materials blends them, the nearer reported with the other's share;
    /// `sharp` narrows the blend to a step halfway, `balance` moves every place, and a place
    /// past the list (or no number) holds at its end.
    #[test]
    fn a_mix_places_a_point_between_its_materials() {
        let m = mix(|p| p.x / 0.01, vec![1, 2, 3], 0.0, 0.0);
        assert!(is(m.at([0.0, 0.0, 0.0]), 1, None));
        assert!(is(m.at([2.5, 0.0, 0.0]), 1, Some((2, 0.25))));
        assert!(is(m.at([7.5, 0.0, 0.0]), 2, Some((1, 0.25))));
        assert!(is(m.at([15.0, 0.0, 0.0]), 2, Some((3, 0.5))));
        assert!(is(m.at([40.0, 0.0, 0.0]), 3, None));
        assert!(is(m.at([-4.0, 0.0, 0.0]), 1, None));
        let sharp = mix(|p| p.x / 0.01, vec![1, 2], 1.0, 0.0);
        assert!(is(sharp.at([4.9, 0.0, 0.0]), 1, None) && is(sharp.at([5.1, 0.0, 0.0]), 2, None));
        let half = mix(|p| p.x / 0.01, vec![1, 2], 0.5, 0.0);
        assert!(is(half.at([2.5, 0.0, 0.0]), 1, None), "a blend half as wide ends a quarter in");
        assert!(is(half.at([6.25, 0.0, 0.0]), 2, Some((1, 0.25))));
        let leaning = mix(|p| p.x / 0.01, vec![1, 2], 0.0, 0.5);
        assert!(is(leaning.at([2.5, 0.0, 0.0]), 2, Some((1, 0.25))));
        assert!(is(mix(|_| f64::NAN, vec![1, 2], 0.0, 0.0).at([1.0, 0.0, 0.0]), 1, None));
        assert!(is(mix(|_| 0.5, vec![4, 4], 0.0, 0.0).at([1.0, 0.0, 0.0]), 4, None), "one material twice is no blend");
    }

    /// A mix is a layer like a paint: inside its region the field's material (and blend), a
    /// later layer over it wins, and outside it the layers under it show.
    #[test]
    fn a_mix_layer_paints_its_region_by_its_field() {
        let field = Region::Field { node: SdfNode::Sphere { radius: 10.0 }, grow: 0.0 };
        let p = Program::new(vec![
            Layer { region: Region::All, op: paint(1) },
            Layer { region: field, op: Op::Mix { channel: Channel::Color, mix: Arc::new(mix(|p| p.x / 0.01, vec![2, 3], 0.0, 0.0)) } },
            Layer { region: Region::Stroke { points: vec![[0.0, 0.0, 0.0], [0.0, 1.0, 0.0]], radius: 1.0 }, op: paint(4) },
        ]);
        let s = p.sample(Channel::Color, [5.0, 5.0, 0.0]);
        assert!(is((s.slot, s.mix), 2, Some((3, 0.5))));
        assert_eq!(s.dist, 0.0, "a blend may change anywhere");
        let s = p.sample(Channel::Color, [2.0, 5.0, 0.0]);
        assert!(is((s.slot, s.mix), 2, Some((3, 0.2))));
        assert_eq!(p.sample(Channel::Color, [0.0, 0.5, 0.0]).slot, 4);
        assert_eq!(p.sample(Channel::Color, [20.0, 0.0, 0.0]).slot, 1);
        assert_eq!(p.sample(Channel::Color, [20.0, 0.0, 0.0]).mix, None);
    }
}

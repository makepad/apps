//! The proof scene: what an export's files say, as drawable primitives.
//!
//! The Fabricate view draws the files as READ BACK by our own readers (never the model), so a
//! wrong file looks wrong. This module turns an [`Export`]'s read-back into per-layer primitives
//! in world millimetres (Y up, the board frame), the drill hits, the placement dots, the BOM and
//! the failure markers, plus a picking index. It is pure: no widget, no GPU, no colours (the pane
//! maps roles to tokens). The model's layers become a second, ghost set from the same conversion,
//! for the "what the factory sees" toggle off.
//!
//! Shapes keep their analytic form (tracks as lines and arcs, flashes as discs and boxes) so
//! they stay sharp at every zoom. Filled regions (pours, polygon pads) are kept as one exact
//! polygon set per layer (`Layer::fills`) for the board's polygon scene (`planes`): the canvas has
//! no polygon, and an approximation could show a shape the file does not contain.
//!
//! Cost: O(objects) per export change (the host builds it on a pool worker, the pane swaps it);
//! pours add O(bands x active edges) each. Per frame nothing. Picking is one index query.

use crate::board::scene::{scene_of_planes, DrawScene};
use fab_elec_cam::image::{GItem, Geom, ObjAttrs, Polarity};
use fab_elec_cam::{CamModel, Export, FileKind, GerberImage, ReadBack, Validation};
use fab_elec_core::geom::ArcGeom;
use fab_elec_core::{mm, to_mm, BBox, Index, LayerId, Nm, PolySet, Shape, Side, P};
use std::ops::ControlFlow;
use std::sync::Arc;

/// One primitive, world millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prim {
    /// A track segment, round ends, `w` wide.
    Line { a: [f32; 2], b: [f32; 2], w: f32 },
    /// A track arc: centre, radius, start angle and signed CCW sweep (radians), `w` wide.
    Arc { c: [f32; 2], r: f32, start: f32, sweep: f32, w: f32 },
    Disc { c: [f32; 2], r: f32 },
    /// A (rotated, rounded) rectangle: centre, half sizes, CCW rotation, corner radius.
    Box { c: [f32; 2], half: [f32; 2], rot: f32, corner: f32 },
}

/// What a layer is, for the colour and the draw order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Copper { side: Option<Side>, inner: u8 },
    Mask(Side),
    Paste(Side),
    Silk(Side),
    Edge,
    Other,
}

impl Role {
    pub fn of(layer: LayerId) -> Role {
        use fab_elec_core::LayerKind::*;
        let side = || if layer.side() == fab_elec_core::LayerSide::Back { Side::Bottom } else { Side::Top };
        match layer.kind() {
            Copper => match layer.0 {
                0 => Role::Copper { side: Some(Side::Top), inner: 0 },
                127 => Role::Copper { side: Some(Side::Bottom), inner: 0 },
                n => Role::Copper { side: None, inner: n },
            },
            Mask => Role::Mask(side()),
            Paste => Role::Paste(side()),
            Silk => Role::Silk(side()),
            Edge => Role::Edge,
            _ => Role::Other,
        }
    }
    /// Draw order, bottom first: bottom stack, inner copper, top stack, edge on top.
    fn order(self) -> u8 {
        match self {
            Role::Paste(Side::Bottom) => 0,
            Role::Silk(Side::Bottom) => 1,
            Role::Mask(Side::Bottom) => 2,
            Role::Copper { side: Some(Side::Bottom), .. } => 3,
            Role::Copper { side: None, .. } => 4,
            Role::Copper { side: Some(Side::Top), .. } => 5,
            Role::Mask(Side::Top) => 6,
            Role::Paste(Side::Top) => 7,
            Role::Silk(Side::Top) => 8,
            Role::Edge => 9,
            Role::Other => 10,
        }
    }
}

/// One drawable object and what it says about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: u32,
    pub prim: Prim,
    /// Index into the layer's `attrs`, `None` for an object with none.
    pub attrs: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// The file this layer was read from (its name as written).
    pub file: String,
    /// What the file says it is (`Copper,L1,Top`), else the layer's name.
    pub label: String,
    pub role: Role,
    pub items: Vec<Item>,
    pub attrs: Vec<ObjAttrs>,
    /// The plane this layer is.
    pub id: LayerId,
    /// The file's filled regions (pours, polygon pads), composed: exact, drawn by the board's
    /// polygon pipeline.
    pub fills: PolySet,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: u32,
    pub at: [f32; 2],
    pub to: Option<[f32; 2]>,
    pub dia: f32,
    pub plated: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dot {
    pub id: u32,
    pub refdes: String,
    pub at: [f32; 2],
    pub side: Side,
    pub rot: f32,
}

/// A failed check, placed.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub id: u32,
    pub at: [f32; 2],
    pub file: String,
    pub cause: String,
    pub hint: String,
}

/// What an id names, for hover text, the semantic nodes and the cross-probe.
#[derive(Clone, Debug, PartialEq)]
pub enum Thing<'a> {
    Item { layer: usize, item: &'a Item, attrs: Option<&'a ObjAttrs>, file: &'a str },
    Drill(&'a Hit),
    Dot(&'a Dot),
    Marker(&'a Marker),
}

#[derive(Clone, Default)]
pub struct ProofScene {
    /// The read-back layers, bottom first.
    pub layers: Vec<Layer>,
    /// The model's layers by the same roles, drawn as a ghost under the read-back.
    pub ghost: Vec<Layer>,
    pub drills: Vec<Hit>,
    pub dots: Vec<Dot>,
    pub markers: Vec<Marker>,
    /// The BOM as written, header first.
    pub bom: Vec<Vec<String>>,
    pub validation: Validation,
    /// The board's box in world millimetres: `[x0, y0, x1, y1]`.
    pub bounds: [f64; 4],
    index: Index<u32>,
    /// `kind` of each id, in id order: (what, which list, which index).
    things: Vec<(u8, u32, u32)>,
}

const ITEM: u8 = 0;
const DRILL: u8 = 1;
const DOT: u8 = 2;
const MARKER: u8 = 3;

fn f2(p: P) -> [f32; 2] {
    [to_mm(p.x) as f32, to_mm(p.y) as f32]
}

/// A layer's objects as primitives. Dark objects only draw; a layer with clear objects is
/// flattened (the files we write have none, other files may).
pub fn prims(items: &[GItem], next: &mut dyn FnMut() -> u32) -> (Vec<Item>, Vec<ObjAttrs>, PolySet) {
    let mut out = Vec::with_capacity(items.len());
    let mut attrs: Vec<ObjAttrs> = Vec::new();
    let mut fills: Vec<PolySet> = Vec::new();
    let attr_of = |a: &ObjAttrs, attrs: &mut Vec<ObjAttrs>| -> Option<u32> {
        if *a == ObjAttrs::default() {
            return None;
        }
        if let Some(i) = attrs.iter().rposition(|x| x == a) {
            return Some(i as u32);
        }
        attrs.push(a.clone());
        Some(attrs.len() as u32 - 1)
    };
    // a layer with clear objects is one composed region: all of it goes to the polygon pipeline
    if items.iter().any(|i| i.polarity == Polarity::Clear) {
        return (out, attrs, fab_elec_cam::image::items_polyset(items, 5_000));
    }
    for it in items {
        let a = attr_of(&it.attrs, &mut attrs);
        match &it.geom {
            Geom::Shape(Shape::Segment { a: p, b, w }) => out.push(Item { id: next(), prim: Prim::Line { a: f2(*p), b: f2(*b), w: to_mm(*w) as f32 }, attrs: a }),
            Geom::Shape(Shape::Arc { a: p, mid, b, w }) => match ArcGeom::new(*p, *mid, *b) {
                Some(g) => out.push(Item {
                    id: next(),
                    prim: Prim::Arc { c: [(g.center.0 / 1e6) as f32, (g.center.1 / 1e6) as f32], r: (g.radius / 1e6) as f32, start: g.start as f32, sweep: g.sweep as f32, w: to_mm(*w) as f32 },
                    attrs: a,
                }),
                None => out.push(Item { id: next(), prim: Prim::Line { a: f2(*p), b: f2(*b), w: to_mm(*w) as f32 }, attrs: a }),
            },
            Geom::Shape(Shape::Circle { c, r }) => out.push(Item { id: next(), prim: Prim::Disc { c: f2(*c), r: to_mm(*r) as f32 }, attrs: a }),
            Geom::Shape(Shape::Rect { c, half, rot, corner }) => out.push(Item {
                id: next(),
                prim: Prim::Box { c: f2(*c), half: [to_mm(half.x) as f32, to_mm(half.y) as f32], rot: rot.radians() as f32, corner: to_mm(*corner) as f32 },
                attrs: a,
            }),
            Geom::Shape(Shape::Oval { c, half, rot }) => out.push(Item {
                id: next(),
                prim: Prim::Box { c: f2(*c), half: [to_mm(half.x) as f32, to_mm(half.y) as f32], rot: rot.radians() as f32, corner: to_mm(half.x.min(half.y)) as f32 },
                attrs: a,
            }),
            // exact polygons: the polygon pipeline's
            Geom::Shape(Shape::Poly(set)) => fills.push(set.clone()),
            Geom::Region(c) => fills.push(PolySet::from_ring_nonzero(c.to_ring(5_000))),
        }
    }
    (out, attrs, PolySet::union_all(fills.iter()))
}

impl Prim {
    /// The box of the primitive, nm.
    pub fn bbox(&self) -> BBox {
        let nm = |v: f32| mm(v as f64);
        match *self {
            Prim::Line { a, b, w } => BBox::new(P::new(nm(a[0]), nm(a[1])), P::new(nm(b[0]), nm(b[1]))).inflate(nm(w / 2.0)),
            Prim::Arc { c, r, w, .. } => BBox::point(P::new(nm(c[0]), nm(c[1]))).inflate(nm(r + w / 2.0)),
            Prim::Disc { c, r } => BBox::point(P::new(nm(c[0]), nm(c[1]))).inflate(nm(r)),
            Prim::Box { c, half, rot, .. } => {
                let (s, k) = (rot.sin().abs(), rot.cos().abs());
                let (hx, hy) = (half[0] * k + half[1] * s, half[0] * s + half[1] * k);
                BBox::point(P::new(nm(c[0]), nm(c[1]))).inflate(nm(hx.max(hy)))
            }
        }
    }
}

fn function_label(img: &GerberImage, fallback: &str) -> String {
    img.file_attr("FileFunction").map(|f| f.join(",")).unwrap_or_else(|| fallback.to_string())
}

fn layers_of_model(model: &CamModel) -> Vec<(LayerId, Vec<GItem>)> {
    model.layers.iter().map(|(l, i)| (*l, i.clone())).collect()
}

impl std::fmt::Debug for ProofScene {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProofScene {{ {} layers, {} items, {} drills, {} dots, {} markers }}", self.layers.len(), self.total_items(), self.drills.len(), self.dots.len(), self.markers.len())
    }
}

/// The read-back layers' filled regions as the board's polygon scene: one exact polygon set per
/// plane, drawn like a board's fills (exact triangles, ring hairlines).
pub fn planes(scene: &ProofScene) -> DrawScene {
    scene_of_planes(&scene.layers.iter().map(|l| (l.id, l.fills.clone())).collect::<Vec<_>>())
}

/// The same for the model's layers (the ghost).
pub fn ghost_planes(scene: &ProofScene) -> DrawScene {
    scene_of_planes(&scene.ghost.iter().map(|l| (l.id, l.fills.clone())).collect::<Vec<_>>())
}

impl ProofScene {
    /// The scene of an export, read back. `next` ids are scene-wide and below 2^24.
    pub fn build(e: &Export) -> ProofScene {
        let mut s = ProofScene { validation: e.validation.clone(), ..ProofScene::default() };
        let mut id = 0u32;
        let mut next = || {
            let v = id;
            id += 1;
            v
        };
        // read-back layers
        let mut layers: Vec<(u8, usize, Layer)> = Vec::new();
        for (n, (name, kind, rb)) in e.read_back.iter().enumerate() {
            match (kind, rb) {
                (FileKind::Gerber { layer, .. }, ReadBack::Gerber(img)) => {
                    let (items, attrs, fills) = prims(&img.items, &mut next);
                    let role = Role::of(*layer);
                    layers.push((role.order(), n, Layer { file: name.clone(), label: function_label(img, &layer.name()), role, items, attrs, id: *layer, fills }));
                }
                (FileKind::Drill { plated }, ReadBack::Drill(img)) => {
                    for h in &img.holes {
                        let hit = Hit { id: next(), at: f2(h.at), to: h.to.map(f2), dia: to_mm(h.dia) as f32, plated: *plated };
                        s.drills.push(hit);
                    }
                }
                (FileKind::Bom, ReadBack::Table(rows)) => s.bom = rows.clone(),
                _ => {}
            }
        }
        layers.sort_by_key(|(o, n, _)| (*o, *n));
        s.layers = layers.into_iter().map(|(_, _, l)| l).collect();
        // the model's layers as a ghost, same roles
        let mut ghost: Vec<(u8, Layer)> = layers_of_model(&e.model)
            .into_iter()
            .map(|(l, items)| {
                let role = Role::of(l);
                let (items, attrs, fills) = prims(&items, &mut next);
                (role.order(), Layer { file: String::new(), label: l.name(), role, items, attrs, id: l, fills })
            })
            .collect();
        ghost.sort_by_key(|(o, _)| *o);
        s.ghost = ghost.into_iter().map(|(_, l)| l).collect();
        for p in &e.model.poses {
            s.dots.push(Dot { id: next(), refdes: p.refdes.clone(), at: f2(p.at), side: p.side, rot: p.rot.radians() as f32 });
        }
        for c in e.validation.failures() {
            if let Some(at) = c.at {
                s.markers.push(Marker { id: next(), at: f2(at), file: c.file.clone(), cause: c.cause.clone(), hint: c.hint.clone() });
            }
        }
        s.finish();
        s
    }

    /// The things by id and the picking index; the bounds from the edge layer, else everything.
    fn finish(&mut self) {
        let mut things = Vec::new();
        let mut boxes: Vec<(BBox, u32)> = Vec::new();
        let mut all = BBox::EMPTY;
        let mut edge = BBox::EMPTY;
        for (li, l) in self.layers.iter().enumerate() {
            for (ii, it) in l.items.iter().enumerate() {
                let b = it.prim.bbox();
                boxes.push((b, it.id));
                set(&mut things, it.id, (ITEM, li as u32, ii as u32));
                all = all.union(b);
                if l.role == Role::Edge {
                    edge = edge.union(b);
                }
            }
        }
        for (i, h) in self.drills.iter().enumerate() {
            let r = mm(h.dia as f64 / 2.0);
            let mut b = BBox::point(P::new(mm(h.at[0] as f64), mm(h.at[1] as f64))).inflate(r);
            if let Some(t) = h.to {
                b = b.union(BBox::point(P::new(mm(t[0] as f64), mm(t[1] as f64))).inflate(r));
            }
            boxes.push((b, h.id));
            set(&mut things, h.id, (DRILL, 0, i as u32));
        }
        for (i, d) in self.dots.iter().enumerate() {
            boxes.push((BBox::point(P::new(mm(d.at[0] as f64), mm(d.at[1] as f64))).inflate(mm(0.4)), d.id));
            set(&mut things, d.id, (DOT, 0, i as u32));
        }
        for (i, m) in self.markers.iter().enumerate() {
            boxes.push((BBox::point(P::new(mm(m.at[0] as f64), mm(m.at[1] as f64))).inflate(mm(0.6)), m.id));
            set(&mut things, m.id, (MARKER, 0, i as u32));
        }
        let b = if edge.is_empty() { all } else { edge };
        self.bounds = if b.is_empty() { [0.0; 4] } else { [to_mm(b.min.x), to_mm(b.min.y), to_mm(b.max.x), to_mm(b.max.y)] };
        self.index = Index::build(boxes);
        self.things = things;
    }

    /// What `id` is.
    pub fn thing(&self, id: u32) -> Option<Thing<'_>> {
        let &(kind, a, b) = self.things.get(id as usize)?;
        Some(match kind {
            ITEM => {
                let l = &self.layers[a as usize];
                let item = &l.items[b as usize];
                Thing::Item { layer: a as usize, item, attrs: item.attrs.map(|i| &l.attrs[i as usize]), file: &l.file }
            }
            DRILL => Thing::Drill(&self.drills[b as usize]),
            DOT => Thing::Dot(&self.dots[b as usize]),
            _ => Thing::Marker(&self.markers[b as usize]),
        })
    }

    /// The ids within `radius` mm of `at`, topmost first: failure markers, then placement dots,
    /// drill hits, then layer objects from the top layer down (only those `visible` says).
    pub fn pick(&self, at: [f64; 2], radius: f64, visible: &dyn Fn(usize) -> bool) -> Vec<u32> {
        let p = P::new(mm(at[0]), mm(at[1]));
        let r = mm(radius);
        let mut hits: Vec<u32> = Vec::new();
        self.index.query(BBox::point(p).inflate(r), |_, id| {
            hits.push(id);
            ControlFlow::Continue(())
        });
        let rank = |id: &u32| match self.things.get(*id as usize) {
            Some(&(MARKER, ..)) => (0u8, 0usize),
            Some(&(DOT, ..)) => (1, 0),
            Some(&(DRILL, ..)) => (2, 0),
            Some(&(_, layer, _)) => (3, usize::MAX - layer as usize),
            None => (9, 0),
        };
        hits.retain(|id| match self.things.get(*id as usize) {
            Some(&(ITEM, layer, _)) => visible(layer as usize) && hit_prim(&self.layers[layer as usize].items[self.things[*id as usize].2 as usize].prim, at, radius),
            Some(_) => true,
            None => false,
        });
        hits.sort_by_key(|id| (rank(id), std::cmp::Reverse(*id)));
        hits
    }

    pub fn total_items(&self) -> usize {
        self.layers.iter().map(|l| l.items.len()).sum()
    }
}

fn set(things: &mut Vec<(u8, u32, u32)>, id: u32, v: (u8, u32, u32)) {
    if things.len() <= id as usize {
        things.resize(id as usize + 1, (255, 0, 0));
    }
    things[id as usize] = v;
}

/// Whether world point `at` is within `radius` of the primitive.
pub fn hit_prim(p: &Prim, at: [f64; 2], radius: f64) -> bool {
    let (x, y) = (at[0] as f32, at[1] as f32);
    let r = radius as f32;
    match *p {
        Prim::Disc { c, r: rr } => (x - c[0]).hypot(y - c[1]) <= rr + r,
        Prim::Line { a, b, w } => {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l2 = dx * dx + dy * dy;
            let t = if l2 == 0.0 { 0.0 } else { (((x - a[0]) * dx + (y - a[1]) * dy) / l2).clamp(0.0, 1.0) };
            (x - (a[0] + t * dx)).hypot(y - (a[1] + t * dy)) <= w / 2.0 + r
        }
        Prim::Arc { c, r: rr, start, sweep, w } => {
            let d = (x - c[0]).hypot(y - c[1]);
            if (d - rr).abs() > w / 2.0 + r {
                return false;
            }
            let ang = (y - c[1]).atan2(x - c[0]);
            let t = if sweep >= 0.0 { (ang - start).rem_euclid(std::f32::consts::TAU) } else { (start - ang).rem_euclid(std::f32::consts::TAU) };
            t <= sweep.abs() + (w / 2.0 + r) / rr.max(1e-6)
        }
        Prim::Box { c, half, rot, .. } => {
            let (s, k) = rot.sin_cos();
            let (dx, dy) = (x - c[0], y - c[1]);
            let (lx, ly) = (dx * k + dy * s, -dx * s + dy * k);
            lx.abs() <= half[0] + r && ly.abs() <= half[1] + r
        }
    }
}

/// The scene of an export shared as the pane holds it.
pub type SharedScene = Arc<ProofScene>;

/// A `Nm` as world millimetres, for callers placing overlays.
pub fn world(v: Nm) -> f32 {
    to_mm(v) as f32
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;

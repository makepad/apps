//! Whole board items for canvas picking and on-demand semantic lookup.
//!
//! Item boxes and whole-item press bodies are built once on the worker. Queries join spatial
//! candidates to the item table. Exact names use a worker-built index; contains queries scan
//! its names once. No item nodes are published as part of a board pane's frame.

use super::pick::{sort_keys, Hit, ScenePick};
use super::scene::{DrawScene, Role, OUTLINE_ITEM};
use crate::canvas::camera::{Camera2d, CanvasItems, CanvasNode};
use crate::canvas::pane::{node_part, part_key, HIT_RADIUS};
use crate::live::flags::{anchor_pin, Anchor, PinSpot};
use crate::live::probe::probe_label;
use fab_elec_circuit::Component;
use fab_elec_board::{Area, Board, ItemKind, Placed};
use fab_elec_core::{BBox, BoardId, CompId, ItemId, LayerId, LayerSet, NetId, Nm, OnTrack, Sel, P};
use makepad_widgets_core::{dvec2, makepad_platform::{a11y, a11y_tree}, Rect, SemanticLookup};
use std::cmp::Ordering;
use std::collections::HashMap;

/// Per-item geometry, built with the scene and sorted by item id for binary-search joins.
pub struct ItemIndex {
    entries: Vec<ItemEntry>,
    bodies: Vec<(BBox, ItemId)>,
}

struct ItemEntry {
    item: ItemId,
    kind: Kind,
    board_ix: u32,
    layers: Vec<(LayerId, ItemBox)>,
}

impl ItemIndex {
    pub fn new(board: &Board, scene: &DrawScene) -> ItemIndex {
        let mut entries = Vec::with_capacity(
            board.placed.len() + board.tracks.len() + board.vias.len() + board.zones.len() + board.holes.len(),
        );
        let mut bodies = Vec::with_capacity(board.placed.len() + board.vias.len() + board.holes.len());
        let mut entry = |item, kind, i| entries.push(ItemEntry {
            item, kind, board_ix: u32::try_from(i).expect("board item index fits u32"), layers: Vec::new(),
        });
        for (i, placed) in board.placed.iter().enumerate() {
            entry(placed.id, Kind::Part, i);
            bodies.push((BBox::of_points(placed.footprint.courtyard.rings()
                .flat_map(|ring| ring.iter().copied()).map(|p| placed.to_board(p))), placed.id));
        }
        for (i, track) in board.tracks.iter().enumerate() {
            entry(track.id, Kind::Track, i);
        }
        for (i, via) in board.vias.iter().enumerate() {
            entry(via.id, Kind::Via, i);
            bodies.push((circle_box(via.at, via.pad), via.id));
        }
        for (i, zone) in board.zones.iter().enumerate() {
            entry(zone.id, Kind::Zone, i);
        }
        for (i, hole) in board.holes.iter().enumerate() {
            entry(hole.id, Kind::Hole, i);
            bodies.push((circle_box(hole.at, hole.pad.unwrap_or(hole.drill)), hole.id));
        }
        entries.sort_unstable_by_key(|entry| entry.item);
        for (layer, content) in &scene.layers {
            let geometry = content.shapes.iter().map(|shape| (shape.tag, shape.shape.bbox()))
                .chain(content.polys.iter().map(|poly| (poly.tag, poly.poly.bbox())));
            for (tag, bbox) in geometry {
                let Ok(i) = entries.binary_search_by_key(&tag.item, |entry| entry.item) else { continue };
                let layers = &mut entries[i].layers;
                let i = match layers.binary_search_by_key(layer, |(id, _)| *id) {
                    Ok(i) => i,
                    Err(i) => {
                        layers.insert(i, (*layer, ItemBox::default()));
                        i
                    }
                };
                layers[i].1.add(*layer, tag.role, bbox);
            }
        }
        for (bbox, item) in &mut bodies {
            if bbox.is_empty() {
                // Courtyard-less placements use all their scene geometry, including openings.
                if let Ok(i) = entries.binary_search_by_key(item, |entry| entry.item) {
                    for (_, layer) in &entries[i].layers {
                        *bbox = bbox.union(layer.bbox).union(layer.openings);
                    }
                }
            }
        }
        ItemIndex { entries, bodies }
    }

    /// Transfer the whole-item press boxes to `ScenePick`, which owns all spatial indexing.
    pub fn take_bodies(&mut self) -> Vec<(BBox, ItemId)> {
        std::mem::take(&mut self.bodies)
    }

    fn entry(&self, item: ItemId) -> Option<&ItemEntry> {
        self.entries.binary_search_by_key(&item, |entry| entry.item).ok().map(|i| &self.entries[i])
    }
}

fn circle_box(at: P, diameter: Nm) -> BBox {
    // Round outwards so an odd diameter's half-nm edge is still a spatial candidate.
    BBox::point(at).inflate(diameter / 2 + diameter % 2)
}

/// Every indexed item's name and key, including hidden and offscreen items. Built once on the
/// geometry worker; exact names cost O(log n + matches), contains one scan, key joins O(1).
pub struct NameIndex {
    entries: Vec<NameEntry>,
    by_item: HashMap<ItemId, usize>,
    /// The same hashed Sel keys as legacy canvas parts, independent of query order.
    pub keys: HashMap<u64, ItemId>,
}

struct NameEntry {
    name: String,
    folded: String,
    item: ItemId,
    /// Direct join to `ItemIndex`, and the old nodes() order, both computed on the worker.
    entry: usize,
    rank: usize,
}

impl NameIndex {
    pub fn new(board: &Board, items: &ItemIndex) -> Self {
        let mut entries: Vec<_> = items.entries.iter().enumerate()
            .filter(|(_, entry)| entry.item != OUTLINE_ITEM)
            .map(|(i, entry)| {
                let name = item_name(board, entry);
                NameEntry { folded: name.to_lowercase(), name, item: entry.item, entry: i, rank: 0 }
            }).collect();
        entries.sort_by(|a, b| item_order(&items.entries[a.entry], &a.name, &items.entries[b.entry], &b.name));
        for (rank, entry) in entries.iter_mut().enumerate() { entry.rank = rank; }
        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name).then(a.rank.cmp(&b.rank)));
        let by_item = entries.iter().enumerate().map(|(i, entry)| (entry.item, i)).collect();
        let keys = entries.iter().map(|entry| (part_key(&Sel::Item(board.unit, entry.item)), entry.item)).collect();
        Self { entries, by_item, keys }
    }

    fn exact(&self, name: &str) -> &[NameEntry] {
        let start = self.entries.partition_point(|entry| entry.name.as_str() < name);
        let end = self.entries.partition_point(|entry| entry.name.as_str() <= name);
        &self.entries[start..end]
    }

    fn entry(&self, item: ItemId) -> Option<&NameEntry> {
        self.by_item.get(&item).map(|&i| &self.entries[i])
    }

    /// An item's cached spelling, for selection summaries without board-list scans.
    pub fn name(&self, item: ItemId) -> Option<&str> {
        self.entry(item).map(|entry| entry.name.as_str())
    }
}

/// The spelling used by both retained trait nodes and on-demand lookup; O(1) board joins.
fn item_name(board: &Board, entry: &ItemEntry) -> String {
    let net = |net: Option<NetId>| {
        net.and_then(|id| board.nets.get(id.ix())).map_or("", |net| net.name.as_str())
    };
    let i = entry.board_ix as usize;
    match entry.kind {
        Kind::Part => board.placed[i].refdes.clone(),
        Kind::Track => format!("{} {}", net(Some(board.tracks[i].net)), board.tracks[i].layer.name()),
        Kind::Via => net(Some(board.vias[i].net)).to_owned(),
        Kind::Zone => format!("{} zone", net(board.zones[i].net)),
        Kind::Hole => format!("H{}", i + 1),
    }
}

/// Values are formatted only for queried candidates (or the legacy trait's visible nodes).
fn item_value(board: &Board, entry: &ItemEntry) -> Option<String> {
    let i = entry.board_ix as usize;
    match entry.kind {
        Kind::Part | Kind::Zone => None,
        Kind::Track => Some(format!("{} mm", to_mm(board.tracks[i].w))),
        Kind::Via => Some(format!("{}/{} mm", to_mm(board.vias[i].drill), to_mm(board.vias[i].pad))),
        Kind::Hole => Some(format!("{} mm", to_mm(board.holes[i].drill))),
    }
}

fn item_order(a: &ItemEntry, an: &str, b: &ItemEntry, bn: &str) -> Ordering {
    a.kind.cmp(&b.kind).then_with(|| {
        if a.kind == Kind::Part { natural_cmp(an, bn).then(a.item.cmp(&b.item)) }
        else { a.board_ix.cmp(&b.board_ix) }
    })
}

/// The board's items for one pane: what is visible, from top to bottom.
pub struct BoardItems<'a> {
    pub unit: BoardId,
    pub board: &'a Board,
    pub scene: &'a DrawScene,
    pub pick: &'a ScenePick,
    pub items: &'a ItemIndex,
    /// Visible layers, topmost first (the pane's stack order).
    pub order: &'a [LayerId],
}

/// The copper a probe lands on: its net, and the pad (placement, pad index) when it is a pad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProbeHit {
    pub net: NetId,
    pub pad: Option<(ItemId, u32)>,
}

impl BoardItems<'_> {
    /// Resolve a query without Cx. `id_key` is the platform's stable part key for an id term;
    /// other terms and Key lookups need no platform state. Names join directly to item geometry,
    /// and worker-assigned ranks restore natural part / board-list order in linear time.
    pub fn lookup(&self, names: &NameIndex, cam: &Camera2d, lookup: &SemanticLookup, id_key: Option<u64>) -> Vec<(u64, a11y_tree::Node)> {
        let (query, key) = match lookup {
            SemanticLookup::Terms(q) if q.is_lookup() => (Some(*q), id_key),
            SemanticLookup::Terms(_) => return Vec::new(),
            SemanticLookup::Key(key) => (None, Some(*key)),
        };
        let name = query.and_then(|q| q.name_term());
        let text = query.and_then(|q| q.text_term()).map(str::to_lowercase);
        let contains = name.filter(|(_, exact)| !exact).map(|(term, _)| term.to_lowercase());
        let mut candidates: Vec<_> = if let Some(key) = key {
            names.keys.get(&key).and_then(|item| names.entry(*item)).into_iter().collect()
        } else if query.is_some_and(|q| q.id_term().is_some()) {
            // A stale or foreign wire id must not turn into a full name scan.
            return Vec::new();
        } else {
            let entries = match name {
                Some((name, true)) => names.exact(name),
                _ => &names.entries,
            };
            entries.iter().filter(|entry| {
                contains.as_ref().is_none_or(|term| entry.folded.contains(term))
                    // Text lookup discovers names; value terms opt into formatting item values.
                    && text.as_ref().is_none_or(|term| entry.folded.contains(term))
            }).collect()
        };
        sort_keys(&mut candidates, |entry| entry.rank as u128);
        let visible = LayerSet::of(self.order);
        let value_term = query.and_then(|q| q.value_term())
            .map(|(term, exact)| (if exact { term.to_owned() } else { term.to_lowercase() }, exact));
        candidates.into_iter().filter_map(|named| {
            let entry = &self.items.entries[named.entry];
            let value = item_value(self.board, entry);
            if let Some((term, exact)) = &value_term {
                let value = value.as_deref().unwrap_or("");
                let matches = if *exact { value == term.as_str() } else { value.to_lowercase().contains(term) };
                if !matches { return None; }
            }
            let (key, mut node) = node_part(&CanvasNode {
                sel: Sel::Item(self.unit, entry.item), rect: Rect::default(),
                role: entry.kind.role(), name: named.name.clone(), value,
            });
            node.actions.insert(a11y_tree::Actions::SCROLL_INTO_VIEW);
            match self.press_rect(entry, cam, visible) {
                Some(bounds) => node.bounds = bounds,
                None => node.states.insert(a11y_tree::States::OFFSCREEN),
            }
            if let Some(q) = query {
                // Wire ids belong to the platform. Supply the resolved id only while matching;
                // return an unassigned node for the builder to attach to this same part key.
                node.id = q.id_term().unwrap_or_default();
                if !q.matches(&node) { return None; }
                node.id = a11y_tree::NodeId::NONE;
            }
            Some((key, node))
        }).collect()
    }

    fn visible_box(&self, entry: &ItemEntry, visible: LayerSet) -> ItemBox {
        let mut bbox = ItemBox::default();
        for (layer, layer_box) in &entry.layers {
            if visible.has(*layer) { bbox.merge(layer_box); }
        }
        bbox
    }

    /// A centre that the pointer picker gives to this item first, in window points. Test the
    /// serialized f32 centre, so rounding cannot turn a published press into a neighbour's hit.
    /// The legacy node size threshold also keeps subpixel items offscreen until zoomed in.
    fn press_rect(&self, entry: &ItemEntry, cam: &Camera2d, visible: LayerSet) -> Option<a11y_tree::Rect> {
        if !cam.px_per_unit.is_finite() || cam.px_per_unit <= 0.0 { return None; }
        let rect = self.visible_box(entry, visible).rect(cam, entry.kind)?;
        let sel = Sel::Item(self.unit, entry.item);
        let test = |point| {
            let rect = a11y::semantic_rect(Rect {
                pos: point - dvec2(HIT_RADIUS, HIT_RADIUS), size: dvec2(2.0 * HIT_RADIUS, 2.0 * HIT_RADIUS),
            });
            let (x, y) = rect.center();
            let point = dvec2(f64::from(x), f64::from(y));
            (cam.view.contains(point)
                && self.hit(cam.to_world(point), HIT_RADIUS / cam.px_per_unit).first() == Some(&sel)).then_some(rect)
        };
        if let Some(rect) = test(rect.center()) { return Some(rect); }
        let test_world = |p: P| test(cam.to_screen([to_mm(p.x), to_mm(p.y)]));
        let i = entry.board_ix as usize;
        match entry.kind {
            Kind::Part => {
                let part = &self.board.placed[i];
                part.footprint.pads.iter().find_map(|pad| test_world(part.to_board(pad.at)))
            }
            Kind::Track => {
                let path = &self.board.tracks[i].path;
                path.segs.iter().enumerate().find_map(|(seg, _)| {
                    test_world(path.point_on(OnTrack { seg: u32::try_from(seg).ok()?, frac: OnTrack::ONE / 2 }))
                })
            }
            Kind::Via => test_world(self.board.vias[i].at),
            Kind::Hole => test_world(self.board.holes[i].at),
            Kind::Zone => None,
        }
    }

    /// The same verified world-mm press point used by lookup bounds; O(item points x local hits).
    pub fn press_point(&self, item: ItemId, cam: &Camera2d) -> Option<[f64; 2]> {
        let rect = self.press_rect(self.items.entry(item)?, cam, LayerSet::of(self.order))?;
        let (x, y) = rect.center();
        Some(cam.to_world(dvec2(f64::from(x), f64::from(y))))
    }

    /// Complete visible-layer bounds in mm, falling back to all layers / model geometry when
    /// hidden. Used once per action, never to enumerate items during a frame.
    pub fn bounds(&self, item: ItemId) -> Option<[f64; 4]> {
        let entry = self.items.entry(item)?;
        let mut bbox = self.visible_box(entry, LayerSet::of(self.order));
        if let Some(bounds) = bbox.bounds(entry.kind) { return Some(mm_box(bounds)); }
        for (_, layer) in &entry.layers { bbox.merge(layer); }
        bbox.bounds(entry.kind).map(mm_box)
            .or_else(|| super::focus::focus_box(self.board, None, &Sel::Item(self.unit, item)))
    }

    /// The copper under a probe at `at` (board mm, within `radius` mm): pads, tracks and vias
    /// before pours, the topmost visible layer first. `None` off copper or on copper without a
    /// net. O(hits at the point).
    pub fn probe_at(&self, at: [f64; 2], radius: f64) -> Option<ProbeHit> {
        if !radius.is_finite() || radius < 0.0 || at.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let hits = self.pick.pick_all(self.scene, P::new(to_nm(at[0]), to_nm(at[1])), to_nm(radius), self.order);
        let first = |pour: bool| hits.iter().find(|h| h.net.is_some() && (h.tag.role == Role::Zone) == pour);
        let hit = first(false).or_else(|| first(true))?;
        Some(ProbeHit { net: hit.net?, pad: (hit.tag.role == Role::Pad).then_some((hit.tag.item, hit.tag.sub)) })
    }
}

/// A probe's net name and channel label. The name is the board net's (the circuit's net name, as
/// the bench takes it; "GND" goes as is). On a pad of a circuit pin the label is that pin's
/// `probe_label` ("R1.1", "U2.3\nNRST"), byte for byte what the schematic pane sends; elsewhere
/// (tracks, vias, pours, a pad of no pin, a board without its circuit) it is the net name.
/// `comps` is the unit's circuit components, indexed by `CompId`. O(pins of the part).
pub fn probe_names(board: &Board, comps: &[Component], hit: ProbeHit) -> Option<(String, String)> {
    let net = board.nets.get(hit.net.0 as usize)?.name.clone();
    let label = hit.pad
        .and_then(|(item, pad)| {
            let placed = board.placed.iter().find(|placed| placed.id == item)?;
            let no = &placed.footprint.pads.get(pad as usize)?.no;
            let comp = comps.get(placed.comp.0 as usize)?;
            probe_label(comp, comp.part.pins.iter().position(|pin| pin.pads.contains(no))?)
        })
        .unwrap_or_else(|| net.clone());
    Some((net, label))
}

/// One thing a press takes: copper (a pad, via or track's shape), a whole item's body (a via's or
/// hole's disc, a part's box), or a pour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pressed {
    Copper(Hit),
    Body(ItemId),
    Pour(Hit),
}

impl Pressed {
    pub(crate) fn item(&self) -> ItemId {
        match self {
            Pressed::Copper(hit) | Pressed::Pour(hit) => hit.tag.item,
            Pressed::Body(item) => *item,
        }
    }
}

impl BoardItems<'_> {
    /// The first part under a press at `at`, in the pick's order (`pressed`), though copper, a via
    /// or a pour lies over it: what a shift-click adds to the selection or takes from it. O(hits).
    pub(crate) fn part_at(&self, at: [f64; 2], radius: f64) -> Option<(ItemId, CompId)> {
        self.hit(at, radius).into_iter().find_map(|sel| match sel {
            Sel::Item(_, item) => self.board.placed_item(item).map(|p| (item, p.comp)),
            _ => None,
        })
    }

    /// A window takes parts whose whole visible box is inside `band` (world mm). A crossing
    /// takes a part when a visible non-text shape's box meets the band. Both query the same
    /// layer indices, sort the hits by item, and return each placement once in item order.
    pub(crate) fn parts_in_band(&self, band: [f64; 4], crossing: bool) -> Vec<(ItemId, CompId)> {
        let band = BBox::new(P::new(to_nm(band[0].min(band[2])), to_nm(band[1].min(band[3]))),
            P::new(to_nm(band[0].max(band[2])), to_nm(band[1].max(band[3]))));
        let mut items: Vec<ItemId> = self.pick.in_rect(self.scene, band, false, self.order).into_iter()
            .filter(|hit| !crossing || hit.tag.role != Role::Text)
            .map(|hit| hit.tag.item).filter(|item| *item != OUTLINE_ITEM).collect();
        items.sort_unstable();
        items.dedup();
        let visible = LayerSet::of(self.order);
        items.into_iter().filter_map(|item| {
            let entry = self.items.entry(item)?;
            if entry.kind != Kind::Part { return None; }
            if !crossing {
                let bounds = self.visible_box(entry, visible).bounds(Kind::Part)?;
                if !band.contains_box(&bounds) { return None; }
            }
            Some((item, self.board.placed[entry.board_ix as usize].comp))
        }).collect()
    }

    /// Whether `key` is a pour and world point `at` lies inside it, farther than `radius` from
    /// its outline: where a drag in Select mode draws the marquee (a pour moves by its outline,
    /// or once selected). A pour over the whole board has no outline of its own. O(its vertices).
    pub(crate) fn pour_inside(&self, key: &Sel, at: [f64; 2], radius: f64) -> bool {
        let Sel::Item(_, item) = key else { return false };
        let Some((ItemKind::Zone, i)) = self.board.locate(*item) else { return false };
        let Some(zone) = self.board.zones.get(i) else { return false };
        let Area::Poly(area) = &zone.area else { return true };
        let near = |a: P, b: P| {
            let (ax, ay, bx, by) = (to_mm(a.x), to_mm(a.y), to_mm(b.x), to_mm(b.y));
            let (dx, dy) = (bx - ax, by - ay);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((at[0] - ax) * dx + (at[1] - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            (at[0] - ax - t * dx).hypot(at[1] - ay - t * dy) <= radius
        };
        !area.rings().any(|ring| (0..ring.len()).any(|k| near(ring[k], ring[(k + 1) % ring.len()])))
    }

    /// What a press at `at` takes, first first, within the pick radius `radius` (S8): the copper
    /// the press is inside (a pad, then a via, then the rest, each in pick order), then the body it
    /// is inside, then the copper and bodies within the radius in pick order, pours last (a pour
    /// lies under everything on its layer). Neither zoom nor part size enters the rule; the radius
    /// only reaches what nothing is exactly under the press. The pane's pick and a gesture's start
    /// both read this order. O(hits + bodies) after the spatial queries.
    pub(crate) fn pressed(&self, at: [f64; 2], radius: f64) -> Vec<Pressed> {
        if !radius.is_finite() || radius < 0.0 || at.iter().any(|v| !v.is_finite()) {
            return Vec::new();
        }
        let p = P::new(to_nm(at[0]), to_nm(at[1]));
        let tol = to_nm(radius);
        let (pours, copper): (Vec<_>, Vec<_>) = self.pick.pick_all(self.scene, p, tol, self.order).into_iter()
            .partition(|h| h.tag.role == Role::Zone);
        let (mut exact, near): (Vec<Hit>, Vec<Hit>) = copper.into_iter().partition(|h| h.dist == 0);
        // Inside both a pad and a track, the pad.
        sort_keys(&mut exact, |h| match h.tag.role {
            Role::Pad => 0,
            Role::Via => 1,
            _ => 2,
        });

        // Whole-item presses include the drill even though exact copper picking passes through it.
        let mut bodies = Vec::new();
        self.pick.bodies_at(p, tol, |item, bbox| {
            let Some(entry) = self.items.entry(item) else { return };
            let (rank, area, inside) = match entry.kind {
                Kind::Via => {
                    let via = &self.board.vias[entry.board_ix as usize];
                    if !in_circle(at, via.at, via.pad, radius) { return; }
                    (0, 0, in_circle(at, via.at, via.pad, 0.0))
                }
                Kind::Hole => {
                    let hole = &self.board.holes[entry.board_ix as usize];
                    let size = hole.pad.unwrap_or(hole.drill);
                    if !in_circle(at, hole.at, size, radius) { return; }
                    (1, 0, in_circle(at, hole.at, size, 0.0))
                }
                // Stable area ties keep board order, independent of spatial tree packing.
                Kind::Part => (2, i128::from(bbox.width()) * i128::from(bbox.height()), bbox.contains(p)),
                _ => return,
            };
            bodies.push((!inside, rank, area, entry.board_ix, item));
        });
        // Fixed-width radix ordering keeps work linear in hits, including board-order ties.
        sort_keys(&mut bodies, |&(outside, rank, area, board_ix, _)| {
            ((outside as u128) << 127) | ((rank as u128) << 96) | ((area as u128) << 32) | u128::from(board_ix)
        });
        let inside = bodies.partition_point(|body| !body.0);
        let mut out: Vec<Pressed> = exact.into_iter().map(Pressed::Copper).collect();
        out.extend(bodies[..inside].iter().map(|body| Pressed::Body(body.4)));
        out.extend(near.into_iter().map(Pressed::Copper));
        out.extend(bodies[inside..].iter().map(|body| Pressed::Body(body.4)));
        out.extend(pours.into_iter().map(Pressed::Pour));
        out
    }
}

/// The board's items as the pane's input sees them. In Select mode a pour that is not selected
/// is background: a drag from its inside draws the marquee, and the pointer is no hand there.
pub(crate) struct PressItems<'a> {
    pub base: BoardItems<'a>,
    pub select: bool,
    pub selected: &'a [Sel],
}

impl CanvasItems for PressItems<'_> {
    type Key = Sel;

    fn hit(&self, at: [f64; 2], radius: f64) -> Vec<Sel> {
        self.base.hit(at, radius)
    }

    fn nodes(&self, cam: &Camera2d) -> Vec<CanvasNode<Sel>> {
        self.base.nodes(cam)
    }

    fn marquee_over(&self, key: &Sel, at: [f64; 2], radius: f64) -> bool {
        self.select && !self.selected.contains(key) && self.base.pour_inside(key, at, radius)
    }
}

impl CanvasItems for BoardItems<'_> {
    type Key = Sel;

    /// The items a press takes, in `pressed` order, each once.
    fn hit(&self, at: [f64; 2], radius: f64) -> Vec<Sel> {
        let items: Vec<ItemId> = self.pressed(at, radius).iter().map(Pressed::item).collect();

        // Keep the first occurrence without making hash iteration part of the picking contract.
        let mut ranked: Vec<_> = items.into_iter().enumerate()
            .filter(|(_, item)| *item != OUTLINE_ITEM).map(|(rank, item)| (item, rank)).collect();
        sort_keys(&mut ranked, |&(item, rank)| (u128::from(item.0) << 64) | rank as u128);
        ranked.dedup_by_key(|entry| entry.0);
        sort_keys(&mut ranked, |entry| entry.1 as u128);
        ranked.into_iter().map(|(item, _)| Sel::Item(self.unit, item)).collect()
    }

    /// O(candidates x layers + candidates log candidates), with binary-search item joins;
    /// no scan of the scene geometry or the board lists after the spatial candidate query.
    fn nodes(&self, cam: &Camera2d) -> Vec<CanvasNode<Sel>> {
        let visible = cam.visible();
        let area = BBox::new(P::new(to_nm(visible[0]), to_nm(visible[1])), P::new(to_nm(visible[2]), to_nm(visible[3])));
        let mut candidates: Vec<_> = self.pick.in_rect(self.scene, area, false, self.order).into_iter()
            .map(|hit| hit.tag.item).filter(|item| *item != OUTLINE_ITEM).collect();
        candidates.sort_unstable();
        candidates.dedup();
        if candidates.is_empty() {
            return Vec::new();
        }
        let visible = LayerSet::of(self.order);
        let mut nodes = Vec::new();
        for item in candidates {
            let Some(entry) = self.items.entry(item) else { continue };
            // Include complete visible-layer boxes before clipping, even outside the camera.
            let bbox = self.visible_box(entry, visible);
            let Some(rect) = bbox.rect(cam, entry.kind) else { continue };
            nodes.push((entry, CanvasNode { sel: Sel::Item(self.unit, item), rect,
                role: entry.kind.role(), name: item_name(self.board, entry), value: item_value(self.board, entry) }));
        }
        nodes.sort_by(|(a, an), (b, bn)| item_order(a, &an.name, b, &bn.name));
        nodes.into_iter().map(|(_, node)| node).collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Part,
    Track,
    Via,
    Zone,
    Hole,
}

impl Kind {
    fn role(self) -> &'static str {
        match self {
            Self::Part => "part", Self::Track => "track", Self::Via => "via",
            Self::Zone => "zone", Self::Hole => "hole",
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ItemBox {
    bbox: BBox,
    openings: BBox,
    copper: bool,
    kind: Option<Kind>,
}

impl ItemBox {
    fn merge(&mut self, other: &ItemBox) {
        self.bbox = self.bbox.union(other.bbox);
        self.openings = self.openings.union(other.openings);
        self.copper |= other.copper;
        if other.kind.is_some() {
            self.kind = other.kind;
        }
    }

    fn add(&mut self, layer: LayerId, role: Role, bbox: BBox) {
        if matches!(role, Role::MaskOpening | Role::Paste) {
            self.openings = self.openings.union(bbox);
            return;
        }
        self.bbox = self.bbox.union(bbox);
        self.copper |= layer.is_copper() && !bbox.is_empty();
        let kind = match role {
            // The board-list join below verifies that a graphic's item really is a placement.
            Role::Pad | Role::Graphic => Some(Kind::Part),
            Role::Track => Some(Kind::Track),
            Role::Via => Some(Kind::Via),
            Role::Zone => Some(Kind::Zone),
            Role::HoleRing => Some(Kind::Hole),
            _ => None,
        };
        if kind.is_some() {
            self.kind = kind;
        }
    }

    fn bounds(&self, kind: Kind) -> Option<BBox> {
        if self.kind.is_some_and(|k| k != kind) || (self.kind.is_none() && self.openings.is_empty()) {
            return None;
        }
        // Mask-only views still expose parts, untented vias and mounting holes.
        let bbox = if self.copper { self.bbox } else { self.bbox.union(self.openings) };
        (!bbox.is_empty()).then_some(bbox)
    }

    fn rect(&self, cam: &Camera2d, kind: Kind) -> Option<Rect> {
        let bbox = self.bounds(kind)?;
        let r = cam.rect_to_screen(mm_box(bbox));
        let left = r.pos.x.max(cam.view.pos.x);
        let top = r.pos.y.max(cam.view.pos.y);
        let right = (r.pos.x + r.size.x).min(cam.view.pos.x + cam.view.size.x);
        let bottom = (r.pos.y + r.size.y).min(cam.view.pos.y + cam.view.size.y);
        let size = dvec2(right - left, bottom - top);
        if size.x <= 0.0 || size.y <= 0.0 || (size.x < 2.0 && size.y < 2.0) {
            return None;
        }
        Some(Rect { pos: dvec2(left, top), size })
    }
}

/// A probe flag's anchor at pad `pad` of `placed`, world mm: the pad's centre, pointing away from
/// the part's centre, with the part's box to keep clear of. None for a pad the part does not have.
pub fn pad_anchor(placed: &Placed, pad: usize) -> Option<Anchor> {
    let at = placed.pad_at(fab_elec_core::PadIx(u16::try_from(pad).ok()?))?;
    Some(Anchor::pin([to_mm(at.x), to_mm(at.y)], [0.0, 0.0], Some(mm_box(placed.bbox()))))
}

/// Where the flag of a probe of board net `net` placed or labelled `label` anchors on the board: the
/// pad [`anchor_pin`] picks among the net's pads (by its part's reference and pad number), as
/// [`pad_anchor`]; None for a net with no pad. O(pads on the board).
pub fn probe_anchor(board: &Board, net: NetId, label: &str) -> Option<Anchor> {
    let pads: Vec<(usize, usize)> = board.placed.iter().enumerate()
        .flat_map(|(k, placed)| placed.pad_nets.iter().enumerate().filter(|(_, n)| **n == Some(net)).map(move |(i, _)| (k, i)))
        .collect();
    let spots: Vec<PinSpot> = pads.iter().map(|&(k, i)| {
        let placed = &board.placed[k];
        PinSpot {
            refdes: placed.refdes.as_str(),
            number: placed.footprint.pads.get(i).map_or("", |pad| pad.no.as_str()),
            part_pins: placed.footprint.pads.len(),
        }
    }).collect();
    let (k, i) = pads[anchor_pin(label, &spots)?];
    pad_anchor(&board.placed[k], i)
}

/// The pads of board net `net`, each named as a probe's `at` names a pin (`"<refdes>.<pad>"`) with
/// its anchor ([`pad_anchor`]): where a flag carried along its net can land. O(pads on the board).
pub fn net_pads(board: &Board, net: NetId) -> Vec<(String, Anchor)> {
    board.placed.iter()
        .flat_map(|placed| placed.pad_nets.iter().enumerate().filter(|(_, n)| **n == Some(net)).filter_map(move |(i, _)| {
            Some((format!("{}.{}", placed.refdes, placed.footprint.pads.get(i)?.no), pad_anchor(placed, i)?))
        }))
        .collect()
}

fn to_nm(mm: f64) -> Nm {
    (mm * 1e6).round() as Nm
}

fn to_mm(nm: Nm) -> f64 {
    nm as f64 / 1e6
}

fn mm_box(bbox: BBox) -> [f64; 4] {
    [to_mm(bbox.min.x), to_mm(bbox.min.y), to_mm(bbox.max.x), to_mm(bbox.max.y)]
}

fn in_circle(at: [f64; 2], center: P, diameter: Nm, radius: f64) -> bool {
    (at[0] - to_mm(center.x)).hypot(at[1] - to_mm(center.y)) <= to_mm(diameter) * 0.5 + radius
}

// Compare digit runs by magnitude without parsing into a bounded integer. Leading-zero ties
// fall back to the original spelling only after all runs compare equal.
pub(crate) fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (mut a, mut b) = (left.as_bytes(), right.as_bytes());
    while let (Some(&x), Some(&y)) = (a.first(), b.first()) {
        let order;
        if x.is_ascii_digit() && y.is_ascii_digit() {
            let an = a.iter().position(|c| !c.is_ascii_digit()).unwrap_or(a.len());
            let bn = b.iter().position(|c| !c.is_ascii_digit()).unwrap_or(b.len());
            let az = a[..an].iter().position(|c| *c != b'0').unwrap_or(an);
            let bz = b[..bn].iter().position(|c| *c != b'0').unwrap_or(bn);
            order = (an - az).cmp(&(bn - bz)).then_with(|| a[az..an].cmp(&b[bz..bn]));
            a = &a[an..];
            b = &b[bn..];
        } else {
            order = x.cmp(&y);
            a = &a[1..];
            b = &b[1..];
        }
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len()).then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use super::super::scene::scene_of;
    use super::*;
    use fab_elec_core::Seg;

    /// S8, the grab rule: exact hits beat radius hits. With a 1 mm pick radius (a low zoom), a
    /// press inside a part's body, on no copper, takes the part before a track within the radius,
    /// for the pane's pick and the gesture alike, and the gesture holds the part. A press inside a
    /// pad that a track ends in takes the pad.
    #[test]
    fn a_press_takes_what_it_is_inside_before_what_is_near() {
        let design = fab_elec_fixtures::ledctl();
        let unit = design.units.first().expect("ledctl has a first unit");
        let board = unit.board.as_deref().expect("ledctl's first unit has a board");
        let scene = scene_of(board, &[]);
        let mut index = ItemIndex::new(board, &scene);
        let bodies = index.take_bodies();
        let boxes = bodies.clone();
        let pick = ScenePick::new(&scene, bodies);
        let mut order: Vec<_> = scene.layers.iter().map(|(layer, _)| *layer).collect();
        order.sort_by_key(|layer| (*layer != LayerId::F_CU, *layer));
        let items = BoardItems { unit: unit.id, board, scene: &scene, pick: &pick, items: &index, order: &order };
        let radius = 1.0;
        let (placed, at, pressed) = board.placed.iter().find_map(|placed| {
            let (body, _) = boxes.iter().find(|(_, item)| *item == placed.id)?;
            let c = body.center();
            let at = [to_mm(c.x), to_mm(c.y)];
            let pressed = items.pressed(at, radius);
            let on_copper = pressed.iter().any(|p| matches!(p, Pressed::Copper(h) if h.dist == 0));
            let track_near = pressed.iter().any(|p| matches!(p, Pressed::Copper(h) if h.tag.role == Role::Track));
            (!on_copper && track_near).then_some((placed, at, pressed))
        }).expect("fixture setup: a part body whose centre is on no copper, with a track within 1 mm");
        assert_eq!(pressed.first(), Some(&Pressed::Body(placed.id)), "{}: inside its body, the part comes before the track within the radius", placed.refdes);
        assert_eq!(items.hit(at, radius).first(), Some(&Sel::Item(unit.id, placed.id)), "{}: the pane's pick takes the part", placed.refdes);
        let p = P::new(to_nm(at[0]), to_nm(at[1]));
        assert_eq!(super::super::gesture::classify(board, &pressed, p, to_nm(radius), LayerId::F_CU, false), super::super::gesture::Press::Hold(placed.id),
            "{}: the press holds the part", placed.refdes);

        let (track, pad_at) = board.tracks.iter().find_map(|track| track.ends.iter().find_map(|end| match *end {
            fab_elec_board::Anchor::Pad { placed, pad } => {
                let at = board.placed.iter().find(|p| p.id == placed)?.pad_at(pad)?;
                (at == track.path.start || at == track.path.end()).then_some((track, at))
            }
            _ => None,
        })).expect("fixture setup: a track that ends at a pad's centre");
        let pressed = items.pressed([to_mm(pad_at.x), to_mm(pad_at.y)], radius);
        assert!(matches!(pressed.first(), Some(Pressed::Copper(h)) if h.tag.role == Role::Pad),
            "inside a pad that track {:?} ends in, the pad comes first: {:?}", track.id, pressed.first());

        // A shift-click takes the part under the press though a track lies over it: on a track
        // crossing a part's body the pick's first item is the track, and `part_at` is the part.
        let mut crossed = board.clone();
        let (body, _) = *boxes.iter().find(|(_, item)| *item == placed.id).expect("the part's body box");
        let net = board.tracks[0].net;
        let id = crossed.alloc_id();
        crossed.tracks.push(fab_elec_board::Track {
            id, net, layer: LayerId::F_CU, w: to_nm(0.25), widths: None,
            path: fab_elec_core::Path::new(P::new(body.min.x, body.center().y)).line(P::new(body.max.x, body.center().y)),
            ends: [fab_elec_board::Anchor::Free, fab_elec_board::Anchor::Free], locked: false, origin: fab_elec_board::Origin::Authored,
        });
        crossed.reindex();
        let scene = scene_of(&crossed, &[]);
        let mut index = ItemIndex::new(&crossed, &scene);
        let pick = ScenePick::new(&scene, index.take_bodies());
        let items = BoardItems { unit: unit.id, board: &crossed, scene: &scene, pick: &pick, items: &index, order: &order };
        assert_eq!(items.hit(at, radius).first(), Some(&Sel::Item(unit.id, id)), "{}: on the track over its body, the pick's first item is the track", placed.refdes);
        assert_eq!(items.part_at(at, radius), Some((placed.id, placed.comp)), "{}: a shift-click there takes the part", placed.refdes);
    }

    /// Left to right encloses parts, right to left crosses their visible shapes. A drag from
    /// inside an unselected pour starts either band. Its outline and a selected pour still drag.
    #[test]
    fn a_window_encloses_parts_a_crossing_touches_them_and_either_starts_inside_a_pour() {
        let design = fab_elec_fixtures::ledctl();
        let unit = design.units.first().expect("ledctl has a first unit");
        let mut board = unit.board.as_deref().expect("ledctl's first unit has a board").clone();
        let (pour, whole) = (board.alloc_id(), board.alloc_id());
        let around = board.outline_bbox();
        let inner = BBox::new(P::new(around.min.x + to_nm(2.0), around.min.y + to_nm(2.0)), P::new(around.max.x - to_nm(2.0), around.max.y - to_nm(2.0)));
        for (id, area) in [(pour, fab_elec_board::Area::Poly(fab_elec_core::PolySet::rect(inner))), (whole, fab_elec_board::Area::Board)] {
            board.zones.push(fab_elec_board::Zone { id, net: None, name: String::new(), layers: LayerSet::of(&[LayerId::F_CU]), area,
                priority: 0, settings: Default::default(), locked: false, origin: fab_elec_board::Origin::Authored });
        }
        board.reindex();
        let scene = scene_of(&board, &[]);
        let mut index = ItemIndex::new(&board, &scene);
        let bodies = index.take_bodies();
        let boxes = bodies.clone();
        let pick = ScenePick::new(&scene, bodies);
        let mut order: Vec<_> = scene.layers.iter().map(|(layer, _)| *layer).collect();
        order.sort_by_key(|layer| (*layer != LayerId::F_CU, *layer));
        let base = || BoardItems { unit: unit.id, board: &board, scene: &scene, pick: &pick, items: &index, order: &order };
        let items = base();

        let all = mm_box(board.outline_bbox().inflate(to_nm(1.0)));
        let window = items.parts_in_band(all, false);
        let mut taken: Vec<CompId> = window.iter().map(|(_, comp)| *comp).collect();
        let mut every: Vec<CompId> = board.placed.iter().map(|p| p.comp).collect();
        every.sort_unstable();
        taken.sort_unstable();
        assert_eq!(taken, every, "a window round the whole board takes every part, each once");
        assert_eq!(items.parts_in_band([all[2], all[3], all[0], all[1]], true), window,
            "a crossing and a window round the whole board take the same parts");
        let placed = &board.placed[0];
        let (body, _) = *boxes.iter().find(|(_, item)| *item == placed.id).expect("the first part's body box");
        let shown = items.visible_box(items.items.entry(placed.id).expect("its entry"), LayerSet::of(&order)).bounds(Kind::Part).expect("its shown box");
        let snug = mm_box(shown.inflate(to_nm(0.05)));
        assert!(items.parts_in_band(snug, false).contains(&(placed.id, placed.comp)), "{}: a window just round its shown box takes it", placed.refdes);
        let half = [snug[0], snug[1], (snug[0] + snug[2]) / 2.0, snug[3]];
        assert!(!items.parts_in_band(half, false).contains(&(placed.id, placed.comp)), "{}: a window over half the part leaves it out", placed.refdes);
        assert!(items.parts_in_band([half[2], half[3], half[0], half[1]], true).contains(&(placed.id, placed.comp)),
            "{}: a crossing over the same half takes the part", placed.refdes);
        let resistor = board.placed.iter().find(|p| p.path == "usb.cc[0]").expect("the USB pull-down resistor");
        let gap = mm_box(BBox::point(resistor.at).inflate(to_nm(0.05)));
        let copper = BoardItems { order: &[LayerId::F_CU], ..base() };
        assert!(copper.parts_in_band(gap, true).is_empty(), "a crossing in empty copper between the resistor's pads takes nothing");
        let silk = BoardItems { order: &[LayerId::F_SILK], ..base() };
        let label = scene.layer(LayerId::F_SILK).expect("front silk").shapes.iter()
            .find(|s| s.tag.item == resistor.id && s.tag.role == Role::Text).expect("the resistor's reference on silk");
        assert!(!silk.parts_in_band(mm_box(label.shape.bbox()), true).contains(&(resistor.id, resistor.comp)),
            "a crossing that only touches the resistor's reference leaves the part out");

        let radius = 0.2;
        let middle = mm_box(inner);
        let inside = [(middle[0] + middle[2]) / 2.0, (middle[1] + middle[3]) / 2.0];
        let edge = [middle[0], inside[1]];
        let (pour, whole, part) = (Sel::Item(unit.id, pour), Sel::Item(unit.id, whole), Sel::Item(unit.id, placed.id));
        let select = PressItems { base: base(), select: true, selected: &[] };
        assert!(select.marquee_over(&pour, inside, radius), "a drag from a pour's inside draws the marquee");
        assert!(!select.marquee_over(&pour, edge, radius), "a drag from its outline moves it");
        assert!(select.marquee_over(&whole, edge, radius), "a pour over the whole board is background everywhere");
        let c = body.center();
        assert!(!select.marquee_over(&part, [to_mm(c.x), to_mm(c.y)], radius), "a part drags");
        let held = [pour.clone()];
        assert!(!PressItems { base: base(), select: true, selected: &held }.marquee_over(&pour, inside, radius), "a selected pour drags from its inside");
        assert!(!PressItems { base: base(), select: false, selected: &[] }.marquee_over(&pour, inside, radius), "outside Select mode a pour drags as before");
    }

    #[test]
    fn fixture_nodes_are_complete_ordered_clipped_and_pressable() {
        let design = fab_elec_fixtures::ledctl();
        let unit = design.units.first().expect("ledctl has a first unit");
        let board = unit.board.as_deref().expect("ledctl's first unit has a board");
        let scene = scene_of(board, &[]);
        let mut index = ItemIndex::new(board, &scene);
        let names = NameIndex::new(board, &index);
        let bodies = index.take_bodies();
        let part_body = bodies.iter().find(|(_, item)| *item == board.placed[0].id).expect("first part body").0;
        let pick = ScenePick::new(&scene, bodies);
        let mut order: Vec<_> = scene.layers.iter().map(|(layer, _)| *layer).collect();
        order.sort_by_key(|layer| (*layer != LayerId::F_CU, *layer));
        let cam = Camera2d::fit(Rect { pos: dvec2(0.0, 0.0), size: dvec2(1000.0, 600.0) }, 1.0, mm_box(scene.bbox), 20.0);
        let items = BoardItems { unit: unit.id, board, scene: &scene, pick: &pick, items: &index, order: &order };
        let nodes = items.nodes(&cam);
        let parts: Vec<_> = nodes.iter().filter(|node| node.role == "part").collect();

        let query = |text: &str| a11y_tree::query::Query::parse(text).expect("valid fixture lookup query");
        let all = query("name~=\"\"");
        let looked = items.lookup(&names, &cam, &SemanticLookup::Terms(&all), None);
        assert_eq!(looked.len(), index.entries.len(), "lookup includes every indexed item, even without visible geometry");
        assert_eq!(looked, items.lookup(&names, &cam, &SemanticLookup::Terms(&all), None),
            "identical queries produce identical ordered nodes and press rectangles");
        assert!(looked.iter().any(|(_, node)| !node.bounds.is_empty()), "the fixture has verified press points");
        for entry in &index.entries {
            let key = part_key(&Sel::Item(unit.id, entry.item));
            assert_eq!(names.keys.get(&key), Some(&entry.item), "each stable key resolves its own item");
        }
        assert!(!names.keys.contains_key(&part_key(&Sel::Item(unit.id, OUTLINE_ITEM))), "the outline has no lookup key");
        for (key, node) in &looked {
            assert!(node.actions.contains(a11y_tree::Actions::PRESS | a11y_tree::Actions::SCROLL_INTO_VIEW),
                "item actions remain available regardless of its current press point");
            if node.bounds.is_empty() {
                assert!(node.states.contains(a11y_tree::States::OFFSCREEN), "a node without a press point is offscreen");
            } else {
                let (x, y) = node.bounds.center();
                let item = *names.keys.get(key).expect("lookup key belongs to the fixture");
                assert_eq!(items.hit(cam.to_world(dvec2(f64::from(x), f64::from(y))), HIT_RADIUS / cam.px_per_unit).first(),
                    Some(&Sel::Item(unit.id, item)), "{}: pressing the published centre hits this item first", node.name);
                assert!(!node.states.contains(a11y_tree::States::OFFSCREEN), "a verified press point is onscreen");
            }
        }
        for placed in &board.placed {
            let q = query(&format!("name={}", a11y_tree::query::quote(&placed.refdes)));
            let result = items.lookup(&names, &cam, &SemanticLookup::Terms(&q), None);
            assert_eq!(result.len(), 1, "{} is found exactly once through the name index", placed.refdes);
            assert_eq!(result[0].0, part_key(&Sel::Item(unit.id, placed.id)), "an exact refdes names its placement");
            assert_eq!(result[0].1.role_description, "part", "an exact refdes retains the part role description");
        }
        for terms in ["name~=gnd", "text~=u2", "value~=mm", "name~=gnd value~=mm", "name~=\"\" state=visible", "name~=\"\" role=group"] {
            let q = query(terms);
            let expected: Vec<_> = looked.iter().filter(|(_, node)| q.matches(node)).cloned().collect();
            assert_eq!(items.lookup(&names, &cam, &SemanticLookup::Terms(&q), None), expected,
                "{terms}: lookup filters the complete nodes after setting bounds and states");
        }
        let duplicate = looked.iter().find(|(_, node)| node.role_description == "track").expect("fixture has a named track");
        let q = query(&format!("name={}", a11y_tree::query::quote(&duplicate.1.name)));
        assert_eq!(items.lookup(&names, &cam, &SemanticLookup::Terms(&q), None),
            looked.iter().filter(|(_, node)| node.name == duplicate.1.name).cloned().collect::<Vec<_>>(),
            "an exact name returns all equal entries, in board order");
        let first = board.placed.first().expect("fixture has a placement");
        let key = part_key(&Sel::Item(unit.id, first.id));
        let wire_id = a11y_tree::NodeId::part(7, 1);
        let by_id = query(&format!("id={} name={}", wire_id.short(), a11y_tree::query::quote(&first.refdes)));
        assert_eq!(items.lookup(&names, &cam, &SemanticLookup::Terms(&by_id), Some(key)),
            items.lookup(&names, &cam, &SemanticLookup::Key(key), None), "wire ids resolve through the stable part key");
        assert!(items.lookup(&names, &cam, &SemanticLookup::Terms(&by_id), None).is_empty(), "a stale id cannot scan the board");
        let outside = Camera2d { center: [100_000.0, 100_000.0], ..cam };
        let q = query(&format!("name={} state=offscreen", a11y_tree::query::quote(&first.refdes)));
        let result = items.lookup(&names, &outside, &SemanticLookup::Terms(&q), None);
        assert_eq!(result.len(), 1, "an out-of-camera item is still found by name");
        assert!(result[0].1.bounds.is_empty() && result[0].1.states.contains(a11y_tree::States::OFFSCREEN),
            "an out-of-camera item has no bounds and is offscreen");
        let tiny = Camera2d { px_per_unit: 1e-6, ..cam };
        let result = items.lookup(&names, &tiny, &SemanticLookup::Key(key), None);
        assert!(result[0].1.bounds.is_empty() && result[0].1.states.contains(a11y_tree::States::OFFSCREEN),
            "subpixel items remain addressable by key without advertising a pointer target");

        assert_eq!(parts.len(), board.placed.len(), "every placement has exactly one part node");
        for placed in &board.placed {
            assert_eq!(nodes.iter().filter(|node| node.role == "part" && node.name == placed.refdes).count(), 1,
                "{} occurs once with role part", placed.refdes);
            assert!(parts.iter().any(|node| node.sel == Sel::Item(unit.id, placed.id)),
                "{} selects its placement item", placed.refdes);
        }
        assert!(nodes.iter().any(|node| node.role == "track"), "the fixture exposes track nodes");
        assert!(nodes.iter().any(|node| node.role == "via"), "the fixture exposes via nodes");
        for node in &nodes {
            let r = node.rect;
            assert!(r.pos.x >= cam.view.pos.x && r.pos.y >= cam.view.pos.y
                && r.pos.x + r.size.x <= cam.view.pos.x + cam.view.size.x
                && r.pos.y + r.size.y <= cam.view.pos.y + cam.view.size.y,
                "{} {:?} has its entire node rectangle inside the pane", node.role, node.sel);
            if matches!(node.role, "part" | "via") {
                assert!(items.hit(cam.to_world(r.center()), 0.2).contains(&node.sel),
                    "{} {:?} is hit by a press at its node's centre", node.role, node.sel);
            }
        }

        // Independent ordering oracle for the fixture's prefix + integer reference designators.
        let mut expected: Vec<_> = board.placed.iter().map(|placed| placed.refdes.as_str()).collect();
        expected.sort_by_key(|&name| {
            let split = name.find(|c: char| c.is_ascii_digit()).expect("fixture refdes has a number");
            (&name[..split], name[split..].parse::<u32>().expect("fixture refdes ends in an integer"))
        });
        assert_eq!(parts.iter().map(|node| node.name.as_str()).collect::<Vec<_>>(), expected,
            "parts appear in natural reference order, including R2 before R10");
        assert_eq!(looked.iter().filter(|(_, node)| node.role_description == "part")
            .map(|(_, node)| node.name.as_str()).collect::<Vec<_>>(), expected,
            "lookup uses the same natural reference order as trait nodes");
        let rest: Vec<_> = board.tracks.iter().map(|item| item.id).chain(board.vias.iter().map(|item| item.id))
            .chain(board.zones.iter().map(|item| item.id)).chain(board.holes.iter().map(|item| item.id))
            .map(|item| part_key(&Sel::Item(unit.id, item))).collect();
        assert_eq!(looked.iter().filter(|(_, node)| node.role_description != "part").map(|(key, _)| *key).collect::<Vec<_>>(),
            rest, "lookup keeps tracks, vias, zones and holes in their board-list order");
        assert_eq!(nodes, items.nodes(&cam), "repeated queries return identical ordered nodes");

        let part = parts.iter().find(|node| node.sel == Sel::Item(unit.id, board.placed[0].id)).expect("first part node");
        let near = Camera2d::fit(cam.view, cam.dpi, mm_box(part_body), 20.0);
        let zoomed = items.nodes(&near);
        let zoomed_part = zoomed.iter().find(|node| node.sel == part.sel).expect("zoomed part node");
        assert_eq!((&zoomed_part.sel, &zoomed_part.name, zoomed_part.role), (&part.sel, &part.name, part.role),
            "zooming to a part's body preserves its semantic identity");
        // The probe seam: a pad gives its net and its pin's `probe_label`, the schematic pane's label
        // byte for byte; a track's middle gives its net.
        let comps = &unit.circuit.comps[..];
        let mm = |p: P| [f64::from(p.x) / 1e6, f64::from(p.y) / 1e6];
        let (placed, pad) = board.placed.iter()
            .find_map(|placed| placed.pad_nets.iter().position(Option::is_some).map(|pad| (placed, pad)))
            .expect("a fixture pad with a net");
        let at = mm(placed.to_board(placed.footprint.pads[pad].at));
        let hit = items.probe_at(at, 0.0).expect("a probe on a pad hits copper");
        assert_eq!((hit.net, hit.pad), (placed.pad_nets[pad].expect("fixture pad has a net"), Some((placed.id, pad as u32))),
            "a probe on a pad takes the pad's net and names the pad");
        let net = board.nets[hit.net.0 as usize].name.clone();
        let comp = &comps[placed.comp.0 as usize];
        let pin = comp.part.pins.iter().position(|pin| pin.pads.contains(&placed.footprint.pads[pad].no))
            .expect("the fixture pad belongs to a pin");
        assert_eq!(probe_names(board, comps, hit), Some((net.clone(), probe_label(comp, pin).expect("a pin label"))),
            "the probe sends the circuit net name and labels the channel as the schematic does");
        assert_eq!(probe_names(board, &[], hit), Some((net.clone(), net.clone())),
            "without its circuit a pad is labelled by its net");
        // A named pin: the board's U2 NRST pad gets the two-line label the schematic's U2 NRST pin gets.
        let u2 = board.placed.iter().find(|placed| placed.refdes == "U2").expect("ledctl places U2");
        let comp = &comps[u2.comp.0 as usize];
        let nrst = comp.part.pins.iter().position(|pin| pin.name == "NRST").expect("U2 has NRST");
        let nrst_pad = u2.footprint.pads.iter().position(|pad| comp.part.pins[nrst].pads.contains(&pad.no))
            .expect("U2's footprint has the NRST pad");
        let hit = items.probe_at(mm(u2.to_board(u2.footprint.pads[nrst_pad].at)), 0.0).expect("a probe on U2's NRST pad hits copper");
        assert_eq!(hit.pad, Some((u2.id, nrst_pad as u32)), "the probe lands on U2's NRST pad");
        let label = probe_names(board, comps, hit).map(|(_, label)| label);
        assert_eq!(label, probe_label(comp, nrst), "board and schematic label the same pin byte for byte");
        assert_eq!(label.as_deref(), Some("U2.3\nNRST"), "a named pin: <refdes>.<number>, its name on a second line");
        let track = board.tracks.iter()
            .find_map(|track| match track.path.segs.first() {
                Some(Seg::Line(end)) => Some((track, P::new((track.path.start.x + end.x) / 2, (track.path.start.y + end.y) / 2))),
                _ => None,
            })
            .expect("a fixture track with a straight first segment");
        let hit = items.probe_at(mm(track.1), 0.0).expect("a probe on a track hits copper");
        assert_eq!((hit.net, hit.pad), (track.0.net, None), "a probe on a track takes the track's net");
        let name = board.nets[track.0.net.0 as usize].name.clone();
        assert_eq!(probe_names(board, comps, hit), Some((name.clone(), name)),
            "off a pad the channel is labelled by the net");
        let outside = [f64::from(scene.bbox.max.x) / 1e6 + 5.0, f64::from(scene.bbox.max.y) / 1e6 + 5.0];
        assert_eq!(items.probe_at(outside, 0.0), None, "no copper, no probe");

        let track = nodes.iter().find(|node| node.role == "track").expect("track node");
        let near = Camera2d::fit(cam.view, cam.dpi, cam.rect_to_world(track.rect), 20.0);
        let zoomed = items.nodes(&near);
        assert_eq!(zoomed.iter().find(|node| node.sel == track.sel).expect("zoomed track node").value, track.value,
            "zooming preserves the track width value");
    }
}

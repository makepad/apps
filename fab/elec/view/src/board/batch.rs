//! Worker-side conversion of a board scene into immutable, tile-sized instance segments.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use fab_elec_core::{
    geom::{ArcGeom, NM_PER_MM},
    Angle, BBox, ItemId, LayerId, NetId, Nm, Shape, P,
};
use makepad_widgets_core::makepad_platform::retained_instances::{
    InstanceSegment, RetainedInstances,
};

use super::{
    inst::{self, Common, PadInst, PadTemplate, StrokeInst, StrokeTemplate, TriInst, TriTemplate},
    liveix::LiveIx,
    polys::{self, ring_mm},
    scene::{DrawScene, DrillHole, Role, Tag, OUTLINE_ITEM},
    tiles::TileId,
};

/// Batch tiles are TileIds at this lod (about 16 mm across): an edit re-batches only its tiles.
pub const BATCH_LOD: u8 = 6;

#[derive(Clone, Debug)]
pub struct Templates {
    pub stroke: StrokeTemplate,
    pub pad: PadTemplate,
    pub tri: TriTemplate,
}

// A path-copied radix index. Branches occur only at differing key bits, so there are
// fewer branches than entries, no empty buckets, and no history chain to compact.
// Cloning a batch/table shares its roots; edits copy only paths to changed entries.
// Keys encode the complete IDs (not the lossy shader ID) and signed tile order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SharedMap<V>(Option<Arc<IndexNode<V>>>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexNode<V> {
    key: u128,
    len: usize,
    entry: IndexEntry<V>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum IndexEntry<V> {
    Value(V),
    Branch { bit: u32, left: Arc<IndexNode<V>>, right: Arc<IndexNode<V>> },
}

impl<V> Default for SharedMap<V> {
    fn default() -> Self { Self(None) }
}

impl<V> IndexNode<V> {
    fn bit(&self) -> u32 {
        match &self.entry { IndexEntry::Value(_) => 0, IndexEntry::Branch { bit, .. } => *bit }
    }
}

impl<V> SharedMap<V> {
    fn len(&self) -> usize { self.0.as_ref().map_or(0, |node| node.len) }

    fn get(&self, key: u128) -> Option<&V> {
        let mut node = self.0.as_deref()?;
        loop {
            match &node.entry {
                IndexEntry::Value(value) => return (node.key == key).then_some(value),
                IndexEntry::Branch { bit, left, right } => {
                    node = if key & (1 << (*bit - 1)) == 0 { left } else { right };
                }
            }
        }
    }

    fn at(&self, mut index: usize) -> Option<&V> {
        let mut node = self.0.as_deref()?;
        if index >= node.len { return None; }
        loop {
            match &node.entry {
                IndexEntry::Value(value) => return Some(value),
                IndexEntry::Branch { left, right, .. } => {
                    if index < left.len { node = left; }
                    else { index -= left.len; node = right; }
                }
            }
        }
    }

    fn iter(&self) -> IndexIter<'_, V> {
        IndexIter { stack: self.0.as_deref().into_iter().collect(), remaining: self.len() }
    }
}

impl<V: Clone> SharedMap<V> {
    fn set(&mut self, key: u128, value: Option<V>) {
        fn update<V: Clone>(node: Option<Arc<IndexNode<V>>>, key: u128, value: Option<V>) -> Option<Arc<IndexNode<V>>> {
            let Some(mut node) = node else {
                return value.map(|value| Arc::new(IndexNode { key, len: 1, entry: IndexEntry::Value(value) }));
            };
            let different = 128 - (node.key ^ key).leading_zeros();
            if different > node.bit() {
                let Some(value) = value else { return Some(node) };
                let leaf = Arc::new(IndexNode { key, len: 1, entry: IndexEntry::Value(value) });
                let (left, right) = if key < node.key { (leaf, node) } else { (node, leaf) };
                return Some(Arc::new(IndexNode {
                    key: left.key, len: left.len + right.len,
                    entry: IndexEntry::Branch { bit: different, left, right },
                }));
            }
            if node.bit() == 0 {
                return value.map(|value| {
                    Arc::make_mut(&mut node).entry = IndexEntry::Value(value);
                    node
                });
            }
            let current = Arc::make_mut(&mut node);
            if let IndexEntry::Branch { bit, left, right } = &mut current.entry {
                let (child, sibling) = if key & (1 << (*bit - 1)) == 0 { (left, right) } else { (right, left) };
                let Some(next) = update(Some(child.clone()), key, value) else { return Some(sibling.clone()) };
                *child = next;
            }
            if let IndexEntry::Branch { left, right, .. } = &current.entry {
                current.key = left.key;
                current.len = left.len + right.len;
            }
            Some(node)
        }
        self.0 = update(self.0.take(), key, value);
    }
}

struct IndexIter<'a, V> {
    stack: Vec<&'a IndexNode<V>>,
    remaining: usize,
}

impl<'a, V> Iterator for IndexIter<'a, V> {
    type Item = &'a V;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(node) = self.stack.pop() {
            match &node.entry {
                IndexEntry::Value(value) => { self.remaining -= 1; return Some(value); }
                IndexEntry::Branch { left, right, .. } => { self.stack.push(right); self.stack.push(left); }
            }
        }
        None
    }
    fn size_hint(&self) -> (usize, Option<usize>) { (self.remaining, Some(self.remaining)) }
}

impl<V> ExactSizeIterator for IndexIter<'_, V> {}

/// An ordered, shared tile directory. Cloning it does not visit untouched tiles.
#[derive(Clone, Debug, Default)]
pub struct TileSegments(SharedMap<(TileId, InstanceSegment)>);

impl TileSegments {
    pub fn len(&self) -> usize { self.0.len() }

    /// The same rows as `other`, by identity: a stream an edit did not touch keeps its map.
    pub fn same(&self, other: &TileSegments) -> bool {
        match (&self.0 .0, &other.0 .0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &(TileId, InstanceSegment)> { self.0.iter() }
    pub fn get(&self, tile: TileId) -> Option<&InstanceSegment> {
        self.0.get(tile_index(tile)).map(|(_, segment)| segment)
    }
}

impl std::ops::Index<usize> for TileSegments {
    type Output = (TileId, InstanceSegment);
    fn index(&self, index: usize) -> &Self::Output { self.0.at(index).expect("tile index in bounds") }
}

fn tile_index(tile: TileId) -> u128 {
    ((tile.lod as u128) << 64)
        | (((tile.y as u32 ^ (1 << 31)) as u128) << 32)
        | (tile.x as u32 ^ (1 << 31)) as u128
}

/// The independently rebuilt streams. Outline rows live in their layer's strokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stream {
    Strokes(LayerId),
    Pads(LayerId),
    Tris(LayerId),
    Holes,
    Substrate,
}

impl Stream {
    fn index(self, tile: TileId) -> u128 {
        let stream = match self {
            Self::Strokes(l) => l.0 as u128 * 3,
            Self::Pads(l) => l.0 as u128 * 3 + 1,
            Self::Tris(l) => l.0 as u128 * 3 + 2,
            Self::Holes => 768,
            Self::Substrate => 769,
        };
        (stream << 72) | tile_index(tile)
    }
}

/// An item's contiguous range in one tile segment, in rows (not float slots).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemRows {
    pub item: ItemId,
    pub start: usize,
    pub len: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ItemTiles {
    tiles: Vec<(Stream, TileId)>,
    // Preserve LayerBatch::polys even for polygons which emit no rows.
    polys: Vec<(LayerId, usize)>,
}

/// Worker-side row ownership, with shared indexes in both directions. Only changed
/// tile ranges and edited item entries are replaced; there is no per-row allocation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowTable {
    tiles: SharedMap<Arc<[ItemRows]>>,
    items: SharedMap<Arc<ItemTiles>>,
}

impl RowTable {
    /// Ascending items and their row ranges in this stream's tile segment.
    pub fn tile_rows(&self, stream: Stream, tile: TileId) -> &[ItemRows] {
        self.tiles.get(stream.index(tile)).map_or(&[], |rows| rows.as_ref())
    }

    /// All stream/tile pairs containing rows of this item, in stream then (y, x) order.
    pub fn item_tiles(&self, item: ItemId) -> &[(Stream, TileId)] {
        self.items.get(item.0 as u128).map_or(&[], |entry| entry.tiles.as_slice())
    }
}

#[derive(Default)]
struct TableBuilder {
    rows: RowTable,
    items: BTreeMap<ItemId, ItemTiles>,
}

impl TableBuilder {
    fn finish(mut self) -> RowTable {
        for (item, mut entry) in self.items {
            entry.tiles.sort_unstable_by_key(|(stream, tile)| (*stream, tile.y, tile.x));
            self.rows.items.set(item.0 as u128, Some(Arc::new(entry)));
        }
        self.rows
    }
}

/// One stream, tiles in (y, x) order. Within each tile rows sort by ascending full `ItemId`
/// (including `OUTLINE_ITEM`), stably: rows of one item retain the batcher's emission order.
#[derive(Clone, Debug, Default)]
pub struct KindBatch {
    pub slots: usize,
    pub tiles: TileSegments,
    pub count: usize,
}

impl KindBatch {
    /// One publication of every tile's segment, in order.
    pub fn publication(&self) -> Result<RetainedInstances, &'static str> {
        RetainedInstances::from_segments(
            self.slots,
            self.tiles.iter().map(|(_, segment)| segment.clone()).collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[derive(Clone, Debug)]
pub struct LayerBatch {
    pub layer: LayerId,
    pub strokes: KindBatch,
    pub pads: KindBatch,
    pub tris: KindBatch,
    /// Source polygons, each drawn as triangles and hairline ring strokes.
    pub polys: usize,
}

#[derive(Clone, Debug)]
pub struct BoardBatch {
    pub layers: Vec<LayerBatch>,
    /// Board body triangles; the edge layer supplies the outline strokes.
    pub substrate: KindBatch,
    pub holes: KindBatch,
    pub generation: u64,
    /// Worker-side ownership/range index for incremental edits.
    pub rows: RowTable,
}

impl BoardBatch {
    pub fn layer(&self, l: LayerId) -> Option<&LayerBatch> {
        self.layers.binary_search_by_key(&l, |layer| layer.layer).ok().map(|i| &self.layers[i])
    }

    pub fn instances(&self) -> usize {
        self.substrate.count + self.holes.count
            + self.layers.iter().map(|layer| layer.strokes.count + layer.pads.count + layer.tris.count).sum::<usize>()
    }

    pub fn stream(&self, stream: Stream) -> Option<&KindBatch> {
        match stream {
            Stream::Strokes(l) => self.layer(l).map(|l| &l.strokes),
            Stream::Pads(l) => self.layer(l).map(|l| &l.pads),
            Stream::Tris(l) => self.layer(l).map(|l| &l.tris),
            Stream::Holes => Some(&self.holes),
            Stream::Substrate => Some(&self.substrate),
        }
    }

    fn layer_mut(&mut self, layer: LayerId, t: &Templates) -> &mut LayerBatch {
        let index = match self.layers.binary_search_by_key(&layer, |l| l.layer) {
            Ok(index) => index,
            Err(index) => {
                self.layers.insert(index, LayerBatch {
                    layer,
                    strokes: KindBatch { slots: t.stroke.slots(), ..KindBatch::default() },
                    pads: KindBatch { slots: t.pad.slots(), ..KindBatch::default() },
                    tris: KindBatch { slots: t.tri.slots(), ..KindBatch::default() },
                    polys: 0,
                });
                index
            }
        };
        &mut self.layers[index]
    }

    fn stream_mut(&mut self, stream: Stream, t: &Templates) -> &mut KindBatch {
        match stream {
            Stream::Strokes(l) => &mut self.layer_mut(l, t).strokes,
            Stream::Pads(l) => &mut self.layer_mut(l, t).pads,
            Stream::Tris(l) => &mut self.layer_mut(l, t).tris,
            Stream::Holes => &mut self.holes,
            Stream::Substrate => &mut self.substrate,
        }
    }
}

// Grouping by the full ID is a stable sort: appends within an item stay in emission order.
// One growable buffer per item/tile, never an allocation per row.
type TileRows = BTreeMap<(i32, i32), BTreeMap<ItemId, Vec<f32>>>;

pub fn batch_scene(scene: &DrawScene, t: &Templates, live: &LiveIx) -> BoardBatch {
    let mut table = TableBuilder::default();
    let mut layers = Vec::with_capacity(scene.layers.len());
    for (layer, source) in &scene.layers {
        let mut strokes = TileRows::new();
        let mut pads = TileRows::new();
        let mut tris = TileRows::new();
        let mut previous_item = None;
        let mut along_nm = 0.0;
        for shape in &source.shapes {
            if previous_item != Some(shape.tag.item) {
                previous_item = Some(shape.tag.item);
                along_nm = 0.0;
            }
            let mut common = Common::rest(item(shape.tag));
            common.live_net = shape.net.map_or(-1.0, |net| live.net(net));
            common.net = board_net(shape.net);
            common.live_part = live.part(shape.tag.item);
            if let Some((mut row, length)) = stroke(&shape.shape, common) {
                if shape.tag.role == Role::Track {
                    let pieces = live.pieces(shape.tag.item, shape.tag.sub);
                    if pieces.len() > 1 {
                        // Layer targets blend strokes with Max: adjacent round capsules union
                        // exactly, so splitting a track for its current rows adds no visible seam.
                        let arc = match shape.shape {
                            Shape::Arc { a, mid, b, .. } => ArcGeom::new(a, mid, b),
                            _ => None,
                        };
                        for piece in pieces {
                            let part = piece_shape(&shape.shape, arc, piece.a, piece.b);
                            let common = Common { live_seg: piece.row as f32, ..common };
                            let (mut row, length) = stroke(&part, common).expect("track piece is a stroke");
                            row.along0 = (along_nm / NM_PER_MM) as f32;
                            along_nm += length;
                            let rows = strokes.entry(tile_key(part.bbox())).or_default().entry(shape.tag.item).or_default();
                            t.stroke.append(rows, &row);
                        }
                        continue;
                    }
                    row.c.live_seg = pieces.first().map_or(-1.0, |piece| piece.row as f32);
                    row.along0 = (along_nm / NM_PER_MM) as f32;
                    along_nm += length;
                }
                let rows = strokes.entry(tile_key(shape.shape.bbox())).or_default().entry(shape.tag.item).or_default();
                t.stroke.append(rows, &row);
            } else if let Some(row) = pad(&shape.shape, shape.hole, common) {
                let rows = pads.entry(tile_key(pad_bbox(&shape.shape))).or_default().entry(shape.tag.item).or_default();
                t.pad.append(rows, &row);
            }
        }
        for p in &source.polys {
            let mesh = polys::mesh(&p.poly);
            // Both streams retain the polygon item and its simulation rows.
            let mut common = Common::rest(item(p.tag));
            // A zone fill draws at the shader's pour alpha (its ring strokes stay full).
            common.look = if p.tag.role == Role::Zone { 1.0 } else { 0.0 };
            common.live_net = p.net.map_or(-1.0, |net| live.net(net));
            common.net = board_net(p.net);
            common.live_part = live.part(p.tag.item);
            for [a, b, c] in mesh.tris {
                let row = TriInst {
                    c: common,
                    tri_ab: [a[0], a[1], b[0], b[1]],
                    tri_c: c,
                };
                let key = tile_key_mm(
                    (a[0] as f64 + b[0] as f64 + c[0] as f64) / 3.0,
                    (a[1] as f64 + b[1] as f64 + c[1] as f64) / 3.0,
                );
                let key = p.clip.map_or(key, |b| clip_key(key, b));
                t.tri.append(tris.entry(key).or_default().entry(p.tag.item).or_default(), &row);
            }
            // A fill is cut at its tile's box: a ring edge along that box is a seam between tiles,
            // not the pour's edge, so it is look 2 (drawn alike, never lit by hover).
            let seams = p.clip.filter(|_| p.tag.role == Role::Zone)
                .map(|b| [ring_mm(b.min.x), ring_mm(b.max.x), ring_mm(b.min.y), ring_mm(b.max.y)]);
            let seam = |a: [f32; 2], b: [f32; 2]| seams.is_some_and(|[x0, x1, y0, y1]| {
                a[0] == b[0] && (a[0] == x0 || a[0] == x1) || a[1] == b[1] && (a[1] == y0 || a[1] == y1)
            });
            for ring in mesh.rings {
                for (i, &a) in ring.iter().enumerate() {
                    let b = ring[(i + 1) % ring.len()];
                    let row = StrokeInst {
                        c: if seam(a, b) { Common { look: 2.0, ..common } } else { common },
                        seg: [a[0], a[1], b[0], b[1]],
                        sag: 0.0,
                        width: 0.0,
                        along0: 0.0,
                    };
                    let key = tile_key_mm(
                        (a[0] as f64 + b[0] as f64) * 0.5,
                        (a[1] as f64 + b[1] as f64) * 0.5,
                    );
                    let key = p.clip.map_or(key, |b| clip_key(key, b));
                    t.stroke.append(strokes.entry(key).or_default().entry(p.tag.item).or_default(), &row);
                }
            }
        }
        for poly in &source.polys {
            let counts = &mut table.items.entry(poly.tag.item).or_default().polys;
            if let Some((_, count)) = counts.last_mut().filter(|(id, _)| *id == *layer) {
                *count += 1;
            } else {
                counts.push((*layer, 1));
            }
        }
        let strokes = finish(t.stroke.slots(), strokes, Stream::Strokes(*layer), &mut table);
        let pads = finish(t.pad.slots(), pads, Stream::Pads(*layer), &mut table);
        let tris = finish(t.tri.slots(), tris, Stream::Tris(*layer), &mut table);
        let polys = source.polys.len();
        if !strokes.is_empty() || !pads.is_empty() || !tris.is_empty() || polys != 0 {
            layers.push(LayerBatch { layer: *layer, strokes, pads, tris, polys });
        }
    }

    let mut holes = TileRows::new();
    for (drill, tag) in &scene.drills {
        let ([cx, cy, half_len, radius], rot) = hole(*drill);
        let row = PadInst {
            c: Common {
                live_net: drill.net.map_or(-1.0, |net| live.net(net)),
                net: board_net(drill.net),
                live_part: live.part(tag.item),
                ..Common::rest(item(*tag))
            },
            rect: [cx, cy, half_len + radius, radius],
            rot,
            corner: radius,
            hole: [0.0; 4],
            hole_rot: 0.0,
        };
        // A stadium's bounding box is symmetric about the drill centre at every rotation.
        let rows = holes.entry(tile_key(BBox::point(drill.c))).or_default().entry(tag.item).or_default();
        t.pad.append(rows, &row);
    }

    let mut substrate = TileRows::new();
    let common = Common::rest(item(Tag { item: OUTLINE_ITEM, sub: 0, role: Role::Outline }));
    for [a, b, c] in polys::mesh(&scene.substrate).tris {
        let row = TriInst {
            c: common,
            tri_ab: [a[0], a[1], b[0], b[1]],
            tri_c: c,
        };
        let key = tile_key_mm(
            (a[0] as f64 + b[0] as f64 + c[0] as f64) / 3.0,
            (a[1] as f64 + b[1] as f64 + c[1] as f64) / 3.0,
        );
        t.tri.append(substrate.entry(key).or_default().entry(OUTLINE_ITEM).or_default(), &row);
    }

    let substrate = finish(t.tri.slots(), substrate, Stream::Substrate, &mut table);
    let holes = finish(t.pad.slots(), holes, Stream::Holes, &mut table);
    BoardBatch { layers, substrate, holes, generation: scene.generation, rows: table.finish() }
}

/// An item whose rows change only inside a box of whole batch tiles.
pub struct RegionEdit {
    pub item: ItemId,
    pub region: BBox,
    /// Source polygons inside the region before the edit, per layer.
    pub old_polys: Vec<(LayerId, usize)>,
}

/// Replace all rows of `edited` (sorted IDs) and the regional rows with `partial`.
/// An item in both `edited` and `regions` is treated as edited only.
/// Global deltas require `batch_scene`.
/// Templates must be the same as in `old`; outline and substrate stay shared.
/// `edited` already includes `live.changed_items` for neighbouring track splits. Use the same
/// `ValueIndex` as `old`: a new lowering requires a full `batch_scene`, never mixed indices.
/// Work visits only partial rows, dirty tile rows, edited items' reverse entries and index paths.
/// The layer directory has at most the LayerId domain; tile/item directories share roots.
pub fn rebatch(old: &BoardBatch, edited: &[ItemId], regions: &[RegionEdit], partial: &DrawScene, t: &Templates, live: &LiveIx) -> BoardBatch {
    let rows = &old.rows;
    let regions: BTreeMap<_, _> = regions.iter().filter(|r| edited.binary_search(&r.item).is_err())
        .map(|r| (r.item, r)).collect();
    // This is the same batching path, including per-track along0 and polygon meshing.
    let added = batch_scene(partial, t, live);
    let mut dirty: BTreeMap<Stream, BTreeSet<(i32, i32)>> = BTreeMap::new();
    for &item in edited {
        for &(stream, tile) in rows.item_tiles(item).iter().chain(added.rows.item_tiles(item)) {
            if stream != Stream::Substrate {
                dirty.entry(stream).or_default().insert((tile.y, tile.x));
            }
        }
    }
    for (&item, edit) in &regions {
        for &(stream, tile) in rows.item_tiles(item).iter().chain(added.rows.item_tiles(item)) {
            let key = (tile.y, tile.x);
            if stream != Stream::Substrate && clip_key(key, edit.region) == key {
                dirty.entry(stream).or_default().insert(key);
            }
        }
    }
    let mut batch = old.clone();
    let mut next = rows.clone();
    for (stream, tiles) in dirty {
        let before = old.stream(stream);
        let after = added.stream(stream);
        let target = batch.stream_mut(stream, t);
        for (y, x) in tiles {
            let tile = TileId { lod: BATCH_LOD, x, y };
            let previous = before.and_then(|kind| kind.tiles.get(tile));
            let replacement = after.and_then(|kind| kind.tiles.get(tile));
            let previous_data = previous.map_or(&[][..], InstanceSegment::data);
            let replacement_data = replacement.map_or(&[][..], InstanceSegment::data);
            let mut kept = rows.tile_rows(stream, tile).iter()
                .filter(|range| edited.binary_search(&range.item).is_err()
                    && !regions.get(&range.item).is_some_and(|edit| clip_key((y, x), edit.region) == (y, x)))
                .peekable();
            let mut inserted = added.rows.tile_rows(stream, tile).iter().peekable();
            let mut data = Vec::with_capacity(previous_data.len() + replacement_data.len());
            let mut ranges = Vec::new();
            loop {
                let (range, source) = match (kept.peek(), inserted.peek()) {
                    (Some(a), Some(b)) if a.item < b.item => (kept.next().unwrap(), previous_data),
                    (_, Some(_)) => (inserted.next().unwrap(), replacement_data),
                    (Some(_), None) => (kept.next().unwrap(), previous_data),
                    (None, None) => break,
                };
                ranges.push(ItemRows { item: range.item, start: data.len() / target.slots, len: range.len });
                data.extend_from_slice(&source[range.start * target.slots..(range.start + range.len) * target.slots]);
            }
            target.count -= previous_data.len() / target.slots;
            target.count += data.len() / target.slots;
            if data.is_empty() {
                target.tiles.0.set(tile_index(tile), None);
                next.tiles.set(stream.index(tile), None);
            } else {
                target.tiles.0.set(tile_index(tile), Some((tile, InstanceSegment::new(data.into()))));
                next.tiles.set(stream.index(tile), Some(ranges.into()));
            }
        }
    }
    // The reverse index changes only for edited items. Unedited items still touch
    // exactly the same tiles even when their row offsets in a dirty tile moved.
    for &item in edited {
        let key = item.0 as u128;
        if let Some(entry) = rows.items.get(key) {
            for &(layer, count) in &entry.polys {
                batch.layer_mut(layer, t).polys -= count;
            }
        }
        if let Some(entry) = added.rows.items.get(key) {
            for &(layer, count) in &entry.polys {
                batch.layer_mut(layer, t).polys += count;
            }
        }
        next.items.set(key, added.rows.items.get(key).cloned());
    }
    for (&item, edit) in &regions {
        let key = item.0 as u128;
        let inside = |tile: TileId| clip_key((tile.y, tile.x), edit.region) == (tile.y, tile.x);
        let mut kept = rows.item_tiles(item).iter().copied().filter(|(_, tile)| !inside(*tile)).peekable();
        let mut inserted = added.rows.item_tiles(item).iter().copied().filter(|(_, tile)| inside(*tile)).peekable();
        let mut entry = ItemTiles::default();
        // Both inputs already follow item_tiles order; merge without sorting the unchanged tiles.
        loop {
            let tile = match (kept.peek(), inserted.peek()) {
                (Some((a, x)), Some((b, y))) if (a, x.y, x.x) < (b, y.y, y.x) => kept.next().unwrap(),
                (_, Some(_)) => inserted.next().unwrap(),
                (Some(_), None) => kept.next().unwrap(),
                (None, None) => break,
            };
            entry.tiles.push(tile);
        }
        let mut counts: BTreeMap<_, _> = rows.items.get(key).into_iter()
            .flat_map(|entry| entry.polys.iter().copied()).collect();
        for &(layer, count) in &edit.old_polys {
            *counts.entry(layer).or_default() -= count;
            batch.layer_mut(layer, t).polys -= count;
        }
        if let Some(added) = added.rows.items.get(key) {
            for &(layer, count) in &added.polys {
                *counts.entry(layer).or_default() += count;
                batch.layer_mut(layer, t).polys += count;
            }
        }
        entry.polys = counts.into_iter().filter(|(_, count)| *count != 0).collect();
        let entry = (!entry.tiles.is_empty() || !entry.polys.is_empty()).then(|| Arc::new(entry));
        next.items.set(key, entry);
    }
    batch.layers.retain(|l| !l.strokes.is_empty() || !l.pads.is_empty() || !l.tris.is_empty() || l.polys != 0);
    batch.generation = partial.generation;
    batch.rows = next;
    batch
}

fn item(tag: Tag) -> f32 {
    if tag.item != OUTLINE_ITEM && tag.item.0 < (1 << 24) {
        tag.item.0 as f32
    } else {
        -1.0
    }
}

/// The instance's board net (the `net` lane), -1 without one or past f32's exact integers.
fn board_net(net: Option<NetId>) -> f32 {
    net.filter(|net| net.0 < (1 << 24)).map_or(-1.0, |net| net.0 as f32)
}

fn rad(angle: Angle) -> f32 {
    angle.radians() as f32
}

fn piece_shape(shape: &Shape, arc: Option<ArcGeom>, a: P, b: P) -> Shape {
    let w = match *shape {
        Shape::Segment { w, .. } | Shape::Arc { w, .. } => w,
        _ => unreachable!("track piece comes from a stroke"),
    };
    if let Some(g) = arc {
        // net_wires splits arcs along their chords. Transfer those chord fractions to the
        // original sweep, so every rendered piece stays on the same circle.
        let dx = g.b.x as f64 - g.a.x as f64;
        let dy = g.b.y as f64 - g.a.y as f64;
        let fraction = |p: P| {
            (((p.x as f64 - g.a.x as f64) * dx + (p.y as f64 - g.a.y as f64) * dy)
                / (dx * dx + dy * dy)).clamp(0.0, 1.0)
        };
        let (start, end) = (fraction(a), fraction(b));
        Shape::Arc { a: g.at(start), mid: g.at((start + end) * 0.5), b: g.at(end), w }
    } else {
        // Collinear/coincident authored arcs keep the existing straight-chord fallback.
        Shape::Segment { a, b, w }
    }
}

fn stroke(shape: &Shape, common: Common) -> Option<(StrokeInst, f64)> {
    let (a, b, w, arc) = match *shape {
        Shape::Segment { a, b, w } => (a, b, w, None),
        Shape::Arc { a, mid, b, w } => (a, b, w, ArcGeom::new(a, mid, b).map(|g| (g, mid))),
        _ => return None,
    };
    let dx = b.x as f64 - a.x as f64;
    let dy = b.y as f64 - a.y as f64;
    let chord = dx.hypot(dy);
    let (sag, length) = if let Some((g, mid)) = arc {
        let side = (dx * (mid.y as f64 - a.y as f64)
            - dy * (mid.x as f64 - a.x as f64)).signum();
        let mx = (a.x as f64 + b.x as f64) * 0.5;
        let my = (a.y as f64 + b.y as f64) * 0.5;
        let h = ((g.center.0 - mx) * -dy + (g.center.1 - my) * dx) / chord;
        (h + side * g.radius, g.length())
    } else {
        (0.0, chord)
    };
    let [ax, ay] = inst::pt(a);
    let [bx, by] = inst::pt(b);
    Some((StrokeInst {
        c: common,
        seg: [ax, ay, bx, by],
        // Arc geometry and accumulated lengths retain fractional nm until this conversion.
        sag: (sag / NM_PER_MM) as f32,
        width: inst::mm(w),
        along0: 0.0,
    }, length))
}

fn pad(shape: &Shape, drill: Option<DrillHole>, common: Common) -> Option<PadInst> {
    let (c, half, rot, corner) = match *shape {
        Shape::Circle { c, r } => (c, P::new(r, r), Angle::ZERO, r),
        Shape::Rect { c, half, rot, corner } => {
            (c, half, rot, corner.max(0).min(half.x.min(half.y)))
        }
        Shape::Oval { c, half, rot } => (c, half, rot, half.x.min(half.y)),
        _ => return None,
    };
    let [cx, cy] = inst::pt(c);
    let [hx, hy] = inst::pt(half);
    let (hole, hole_rot) = drill.map(hole).unwrap_or(([0.0; 4], 0.0));
    Some(PadInst {
        c: common,
        rect: [cx, cy, hx, hy],
        rot: rad(rot),
        corner: inst::mm(corner),
        hole,
        hole_rot,
    })
}

fn hole(drill: DrillHole) -> ([f32; 4], f32) {
    let short = drill.size.x.min(drill.size.y);
    let long = drill.size.x.max(drill.size.y);
    let [cx, cy] = inst::pt(drill.c);
    let mut rot = rad(drill.rot);
    if drill.size.y > drill.size.x {
        rot += std::f32::consts::FRAC_PI_2;
    }
    ([cx, cy, inst::mm(long - short) * 0.5, inst::mm(short) * 0.5], rot)
}

fn pad_bbox(shape: &Shape) -> BBox {
    // Shape::bbox allocates a convex core for rectangles. The same four rounded core
    // corners on the stack keep batching allocation-free apart from the tile streams.
    let (c, half, rot, radius) = match *shape {
        Shape::Rect { c, half, rot, corner } => {
            (c, half, rot, corner.max(0).min(half.x.min(half.y)))
        }
        Shape::Oval { c, half, rot } => (c, half, rot, half.x.min(half.y)),
        _ => return shape.bbox(),
    };
    let hx = half.x - radius;
    let hy = half.y - radius;
    let corners = [P::new(-hx, -hy), P::new(hx, -hy), P::new(hx, hy), P::new(-hx, hy)];
    BBox::of_points(corners.into_iter().map(|p| p.rotated(rot).add(c))).inflate(radius)
}

fn tile_key(bbox: BBox) -> (i32, i32) {
    let twice_span = 2 * TileId::span(BATCH_LOD);
    // Divide the doubled centre directly: negative half-nm centres must floor too.
    let cell = |lo: Nm, hi: Nm| ((lo as i64 + hi as i64).div_euclid(twice_span)) as i32;
    (cell(bbox.min.y, bbox.max.y), cell(bbox.min.x, bbox.max.x))
}

fn tile_key_mm(x: f64, y: f64) -> (i32, i32) {
    // Centroids and edge midpoints keep their fractional coordinates until tile division.
    let span = TileId::span(BATCH_LOD) as f64 / NM_PER_MM;
    ((y / span).floor() as i32, (x / span).floor() as i32)
}

// A fill tile's top/right boundary belongs to that fill tile, including below the origin.
fn clip_key((y, x): (i32, i32), b: BBox) -> (i32, i32) {
    let span = TileId::span(BATCH_LOD);
    let clamp = |key: i32, lo: Nm, hi: Nm| {
        let first = (lo as i64).div_euclid(span) as i32;
        // Integer ceil(hi / span) - 1, without rounding through floats.
        let last = (hi as i64 - 1).div_euclid(span) as i32;
        key.clamp(first, last)
    };
    (clamp(y, b.min.y, b.max.y), clamp(x, b.min.x, b.max.x))
}

fn finish(slots: usize, rows: TileRows, stream: Stream, table: &mut TableBuilder) -> KindBatch {
    let mut batch = KindBatch { slots, ..KindBatch::default() };
    for ((y, x), items) in rows {
        let tile = TileId { lod: BATCH_LOD, x, y };
        let mut data = Vec::with_capacity(items.values().map(Vec::len).sum());
        let mut ranges = Vec::with_capacity(items.len());
        for (item, rows) in items {
            if rows.is_empty() { continue; }
            ranges.push(ItemRows { item, start: data.len() / slots, len: rows.len() / slots });
            data.extend_from_slice(&rows);
            table.items.entry(item).or_default().tiles.push((stream, tile));
        }
        if data.is_empty() { continue; }
        batch.count += data.len() / slots;
        batch.tiles.0.set(tile_index(tile), Some((tile, InstanceSegment::new(data.into()))));
        table.rows.tiles.set(stream.index(tile), Some(ranges.into()));
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::scene::{SceneLayer, ScenePoly, SceneShape};
    use fab_elec_board::ConnItem;
    use fab_elec_core::{mm, ItemId, NetId, PadIx, PolySet};

    #[test]
    fn incremental_edits_match_fresh_batches_and_share_clean_tiles() {
        use super::super::{
            gen::{generated_board, GenSpec},
            scene::{scene_of, scene_of_fill_region, scene_of_items},
            shared::fill_region_edit,
        };
        use fab_elec_board::zone::{FillLattice, FillLayer, FillTile, PolyRef, TileKey, TileLabels};
        use fab_elec_board::{Anchor, Board, Track, ZoneFill};
        use fab_elec_core::{ContentHash, Path};

        /// A supplied fill: one kept polygon on `layer`, in one tile.
        fn one_poly(zone: ItemId, layer: LayerId, poly: PolySet) -> ZoneFill {
            let lattice = FillLattice { level: 14 };
            let min = poly.bbox().min;
            let key = TileKey { x: min.x.div_euclid(lattice.side()), y: min.y.div_euclid(lattice.side()) };
            let tile = FillTile {
                key, polys: vec![poly], own: vec![true], cuts: Default::default(),
                node: vec![0], group: vec![0], foreign: vec![false], core_cuts: Default::default(), lone: Default::default(), hash: Default::default(),
            };
            let island = vec![PolyRef { tile: key, ord: 0 }];
            let labels = TileLabels { island, kept: vec![true], view_hash: Default::default() };
            let layers = vec![FillLayer { layer, tiles: vec![Arc::new(tile)], labels: vec![labels], reached: Vec::new() }];
            ZoneFill { zone, lattice, layers }
        }

        fn check(board: &Board, fills: &[ZoneFill], old: &BoardBatch, edited: &[ItemId], regions: &[RegionEdit], partial: &DrawScene,
            t: &Templates, bounds: (usize, usize), label: &str) -> BoardBatch {
            let partial_batch = batch_scene(partial, t, &LiveIx::none());
            let mut dirty: BTreeSet<_> = edited.iter().flat_map(|&id| {
                old.rows.item_tiles(id).iter().chain(partial_batch.rows.item_tiles(id)).copied()
            }).collect();
            for edit in regions.iter().filter(|r| edited.binary_search(&r.item).is_err()) {
                dirty.extend(old.rows.item_tiles(edit.item).iter().chain(partial_batch.rows.item_tiles(edit.item))
                    .filter(|(_, tile)| clip_key((tile.y, tile.x), edit.region) == (tile.y, tile.x)).copied());
            }
            let (max_dirty, max_tiles) = bounds;
            let physical: BTreeSet<_> = dirty.iter().map(|(_, tile)| *tile).collect();
            assert!(physical.len() <= max_tiles, "{label}: at most {max_tiles} physical tiles, got {}", physical.len());
            assert!(!dirty.is_empty() && dirty.len() <= max_dirty,
                "{label}: at most {max_dirty} dirty stream/tiles, got {}", dirty.len());
            let next = rebatch(old, edited, regions, partial, t, &LiveIx::none());
            let rows = &next.rows;
            let fresh = batch_scene(&scene_of(board, fills), t, &LiveIx::none());
            assert_eq!(next.generation, board.generation, "{label}: generation follows the partial scene");
            assert_eq!(*rows, fresh.rows, "{label}: incremental row table equals the fresh table");
            assert_eq!(next.layers.iter().map(|l| (l.layer, l.polys)).collect::<Vec<_>>(),
                fresh.layers.iter().map(|l| (l.layer, l.polys)).collect::<Vec<_>>(),
                "{label}: layer order and polygon counts equal a fresh build");
            let streams: BTreeSet<_> = old.layers.iter().chain(&fresh.layers)
                .flat_map(|l| [Stream::Strokes(l.layer), Stream::Pads(l.layer), Stream::Tris(l.layer)])
                .chain([Stream::Holes, Stream::Substrate]).collect();
            let mut shared = 0;
            for stream in streams {
                let actual = next.stream(stream);
                let expected = fresh.stream(stream);
                assert_eq!(actual.is_some(), expected.is_some(), "{label}: {stream:?} presence");
                if let (Some(actual), Some(expected)) = (actual, expected) {
                    assert_eq!((actual.slots, actual.count, actual.tiles.len()),
                        (expected.slots, expected.count, expected.tiles.len()), "{label}: {stream:?} sizes");
                    for ((tile, segment), (expected_tile, expected_segment)) in actual.tiles.iter().zip(expected.tiles.iter()) {
                        assert_eq!(tile, expected_tile, "{label}: {stream:?} tile order");
                        assert!(segment.data().iter().map(|v| v.to_bits()).eq(expected_segment.data().iter().map(|v| v.to_bits())),
                            "{label}: {stream:?} {tile:?} rows match bit for bit");
                    }
                }
                if let Some(before) = old.stream(stream) {
                    for (tile, segment) in before.tiles.iter() {
                        if dirty.contains(&(stream, *tile)) { continue; }
                        let retained = actual.and_then(|kind| kind.tiles.get(*tile)).expect("clean tile remains present");
                        assert_eq!(retained.id(), segment.id(), "{label}: {stream:?} {tile:?} clean segment stays shared");
                        assert!(Arc::ptr_eq(old.rows.tiles.get(stream.index(*tile)).unwrap(), rows.tiles.get(stream.index(*tile)).unwrap()),
                            "{label}: {stream:?} {tile:?} clean row ranges stay shared");
                        shared += 1;
                    }
                }
            }
            assert!(shared > dirty.len(), "{label}: clean shared tiles outnumber the dirty tiles");
            next
        }

        let t = Templates { stroke: StrokeTemplate::packed(), pad: PadTemplate::packed(), tri: TriTemplate::packed() };
        let mut board = generated_board(&GenSpec {
            width_mm: 160.0, height_mm: 112.0, copper_layers: 4, segments: 4_000, seed: 7,
        });
        board.reindex();
        // Use supplied fills rather than running the zone engine: these exercise both
        // triangle and ring streams, while keeping the one generated-board setup small.
        let mut fills: Vec<_> = board.zones.iter().map(|zone| {
            let poly = PolySet::rect(BBox::new(P::mm(3.0, 3.0), P::mm(9.0, 8.0)));
            one_poly(zone.id, zone.layers.iter().next().unwrap(), poly)
        }).collect();
        let mut batch = batch_scene(&scene_of(&board, &fills), &t, &LiveIx::none());
        let placed = board.placed.iter_mut().find(|p| p.footprint.pads.iter().any(|p| p.drill.is_some()))
            .expect("generated setup contains a drilled placement");
        let moved = placed.id;
        placed.at = placed.at.add(P::mm(1.3, 0.0));
        board.generation += 1;
        // A header's pads/drills and graphics touch at most 48 stream/tiles across
        // its four copper layers and technical layers, in at most four physical tiles.
        batch = check(&board, &fills, &batch, &[moved], &[], &scene_of_items(&board, &fills, &[moved]),
            &t, (48, 4), "move placed part 1.3 mm");

        let index = board.tracks.iter().enumerate().min_by_key(|(_, track)| track.path.segs.len()).unwrap().0;
        let removed = board.tracks.remove(index).id;
        board.reindex();
        board.generation += 1;
        batch = check(&board, &fills, &batch, &[removed], &[], &scene_of_items(&board, &fills, &[removed]),
            &t, (8, 8), "delete short track");
        assert!(batch.rows.item_tiles(removed).is_empty(), "deleted track leaves no reverse index entry");

        // A high ID cannot be recovered from its shader float; the table must retain it.
        board.next_item = board.next_item.max((1 << 24) + 1);
        let added = board.alloc_id();
        board.tracks.push(Track {
            id: added, net: NetId(0), layer: LayerId::F_CU, w: mm(0.25), widths: None,
            path: Path::new(P::mm(1.0, -8.0)).line(P::mm(15.0, -8.0))
                .line(P::mm(31.0, -8.0)).line(P::mm(47.0, -8.0)),
            ends: [Anchor::Free; 2], locked: false, origin: Default::default(),
        });
        board.reindex();
        board.generation += 1;
        batch = check(&board, &fills, &batch, &[added], &[], &scene_of_items(&board, &fills, &[added]),
            &t, (3, 3), "add track across three tiles");
        assert_eq!(batch.rows.item_tiles(added).len(), 3, "new track contributes rows to exactly three tiles");

        let poly = PolySet::rect(BBox::new(P::mm(35.0, 3.0), P::mm(41.0, 8.0)));
        fills[0] = one_poly(fills[0].zone, fills[0].layers[0].layer, poly);
        board.generation += 1;
        batch = check(&board, &fills, &batch, &[fills[0].zone], &[], &scene_of_items(&board, &fills, &[fills[0].zone]),
            &t, (4, 2), "replace a filled zone");

        // Moving the new track to an otherwise empty layer, then deleting it, exercises
        // layer creation/removal and tile deletion in the persistent indexes.
        board.tracks.last_mut().unwrap().layer = LayerId::user(0);
        board.generation += 1;
        batch = check(&board, &fills, &batch, &[added], &[], &scene_of_items(&board, &fills, &[added]),
            &t, (6, 3), "gain a layer");
        assert!(batch.layer(LayerId::user(0)).is_some(), "first rows create a layer");
        let _ = board.tracks.pop();
        board.reindex();
        board.generation += 1;
        batch = check(&board, &fills, &batch, &[added], &[], &scene_of_items(&board, &fills, &[added]),
            &t, (3, 3), "lose a layer");
        assert!(batch.layer(LayerId::user(0)).is_none(), "last rows remove a layer");

        let unchanged = rebatch(&batch, &[], &[], &scene_of_items(&board, &fills, &[]), &t, &LiveIx::none());
        let rows = &unchanged.rows;
        assert_eq!(*rows, batch.rows, "empty edit preserves the table");
        assert_eq!(unchanged.instances(), batch.instances(), "empty edit preserves all rows");
        assert!(Arc::ptr_eq(rows.tiles.0.as_ref().unwrap(), batch.rows.tiles.0.as_ref().unwrap()),
            "empty edit shares the entire tile table root");

        let mut region_board = Board::new(board.unit, "regional fill");
        region_board.zones.push(board.zones[0].clone());
        region_board.reindex();
        let zone = region_board.zones[0].id;
        // Exercise both nestings, including an unchanged fill tile in the changed batch tile.
        for level in [13, 15] {
            let lattice = FillLattice { level };
            let keys: Vec<_> = [-2, -1, 0, 2].into_iter().map(|x| TileKey { x, y: -1 }).collect();
            let layer = FillLayer {
                layer: LayerId::F_CU,
                tiles: keys.iter().map(|&key| Arc::new(FillTile {
                    key, polys: vec![PolySet::rect(lattice.tile_box(key))], own: vec![true],
                    node: vec![0], group: vec![0], foreign: vec![false], core_cuts: Default::default(), lone: Default::default(),
                    cuts: Default::default(), hash: Default::default(),
                })).collect(),
                labels: keys.iter().map(|&key| TileLabels {
                    island: vec![PolyRef { tile: key, ord: 0 }], kept: vec![true], view_hash: Default::default(),
                }).collect(),
                reached: Vec::new(),
            };
            let before_fill = ZoneFill {
                zone, lattice, layers: vec![layer.clone(), FillLayer { layer: LayerId::B_CU, ..layer }],
            };
            let before = batch_scene(&scene_of(&region_board, std::slice::from_ref(&before_fill)), &t, &LiveIx::none());
            let mut after_fill = before_fill.clone();
            let key = keys[1];
            let b = lattice.tile_box(key);
            let third = lattice.side() / 3;
            after_fill.layers[0].tiles[1] = Arc::new(FillTile {
                key,
                polys: vec![
                    PolySet::rect(BBox::new(b.min, P::new(b.min.x + third, b.max.y))),
                    PolySet::rect(BBox::new(P::new(b.max.x - third, b.min.y), b.max)),
                ],
                own: vec![true; 2], cuts: Default::default(), hash: ContentHash([1; 16]),
                node: vec![0, 1], group: vec![0, 1], foreign: vec![false; 2], core_cuts: Default::default(), lone: Default::default(),
            });
            after_fill.layers[0].labels[1] = TileLabels {
                island: vec![PolyRef { tile: key, ord: 0 }, PolyRef { tile: key, ord: 1 }],
                kept: vec![true; 2], view_hash: ContentHash([2; 16]),
            };
            region_board.generation += 1;
            let edit = fill_region_edit(&before_fill, &after_fill).expect("changed copper produces a region edit");
            let extent = lattice.side().max(TileId::span(BATCH_LOD) as Nm);
            assert_eq!(edit.region, BBox::new(P::new(-extent, -extent), P::ZERO),
                "level {level}: region rounds negative minima down and excludes the zero top/right neighbours");
            let old_count = if level == 13 { 2 } else { 1 };
            assert_eq!(edit.old_polys, vec![(LayerId::F_CU, old_count), (LayerId::B_CU, old_count)],
                "level {level}: old polygon counts include every fill tile inside the batch region on each layer");
            let partial = scene_of_fill_region(&region_board, &after_fill, edit.region);
            assert_eq!(partial.layers.iter().map(|(l, s)| (*l, s.polys.len())).collect::<Vec<_>>(),
                vec![(LayerId::F_CU, old_count + 1), (LayerId::B_CU, old_count)],
                "level {level}: partial scene includes the unchanged regional fill tiles and excludes boundary neighbours");
            let physical = (extent as i64 / TileId::span(BATCH_LOD)).pow(2) as usize;
            let after = check(&region_board, std::slice::from_ref(&after_fill), &before, &[], std::slice::from_ref(&edit),
                &partial, &t, (4 * physical, physical), &format!("level {level}: refill one tile"));
            assert_eq!(after.layer(LayerId::F_CU).unwrap().polys, 5,
                "level {level}: region update adds one polygon without dropping polygons outside the region");
            assert_eq!(after.layer(LayerId::B_CU).unwrap().polys, 4,
                "level {level}: region update preserves the untouched layer's polygon count");
        }
    }

    #[test]
    fn batches_scene_geometry_in_tile_order() {
        let t = Templates {
            stroke: StrokeTemplate::packed(), pad: PadTemplate::packed(), tri: TriTemplate::packed(),
        };
        let tag = |item, sub, role| Tag { item: ItemId(item), sub, role };
        let track = tag(7, 0, Role::Track);
        let circle_tag = tag(8, 0, Role::Pad);
        let rect_tag = tag(9, 0, Role::Pad);
        let round = DrillHole {
            item: ConnItem::Pad { placed: ItemId(8), pad: PadIx(0) }, net: None,
            c: P::mm(5.0, 5.0), size: P::mm(0.6, 0.6), rot: Angle::ZERO, plated: true,
        };
        let slot = DrillHole {
            item: ConnItem::Pad { placed: ItemId(9), pad: PadIx(0) }, net: None,
            c: P::mm(-1.0, -1.0), size: P::mm(0.4, 1.2), rot: Angle::deg(30.0), plated: false,
        };
        let a = P::mm(10.0, 0.0);
        let mid = P::mm(7.071, 7.071);
        let b = P::mm(0.0, 10.0);
        let mut scene = DrawScene {
            layers: vec![
                (LayerId::F_CU, SceneLayer {
                    shapes: vec![
                        SceneShape {
                            shape: Shape::Segment { a: P::ZERO, b: a, w: mm(0.4) },
                            hole: None, tag: track, net: Some(NetId(3)),
                        },
                        SceneShape {
                            shape: Shape::Arc { a, mid, b, w: mm(0.4) },
                            hole: None, tag: Tag { sub: 1, ..track }, net: None,
                        },
                        SceneShape {
                            shape: Shape::Circle { c: round.c, r: mm(1.0) },
                            hole: Some(round), tag: circle_tag, net: None,
                        },
                        SceneShape {
                            shape: Shape::Rect {
                                c: slot.c, half: P::mm(2.0, 1.0),
                                rot: Angle::QUARTER, corner: mm(3.0),
                            },
                            hole: Some(slot), tag: rect_tag, net: None,
                        },
                    ],
                    polys: vec![ScenePoly {
                        poly: PolySet::rect(BBox::new(P::ZERO, P::mm(10.0, 10.0)))
                            .difference(&PolySet::rect(BBox::new(P::mm(3.0, 3.0), P::mm(7.0, 7.0)))),
                        clip: None,
                        tag: tag(13, 0, Role::Zone), net: None,
                    }],
                }),
                (LayerId::B_CU, SceneLayer {
                    shapes: vec![SceneShape {
                        shape: Shape::Oval {
                            c: P::mm(-40.0, 40.0), half: P::mm(3.0, 1.0), rot: Angle::ZERO,
                        },
                        hole: None, tag: tag(10, 0, Role::Pad), net: None,
                    }],
                    polys: Vec::new(),
                }),
                (LayerId::F_SILK, SceneLayer::default()),
                (LayerId::B_SILK, SceneLayer {
                    shapes: Vec::new(),
                    polys: vec![ScenePoly {
                        poly: PolySet::from_ring_nonzero(vec![
                            P::ZERO, P::mm(1.0, 0.0), P::mm(0.0, 1.0),
                        ]),
                        clip: None,
                        tag: tag(11, 0, Role::Graphic), net: None,
                    }],
                }),
            ],
            drills: vec![(slot, rect_tag)],
            substrate: PolySet::rect(BBox::new(P::ZERO, P::mm(20.0, 10.0))),
            generation: 73,
            ..DrawScene::default()
        };
        let batch = batch_scene(&scene, &t, &LiveIx::none());
        assert_eq!(batch.generation, 73, "generation follows the scene");
        assert_eq!(batch.layers.len(), 3, "empty layers are omitted, polygon layers retained");
        assert!(batch.layer(LayerId::F_SILK).is_none(), "empty layer has no batch");
        let front = batch.layer(LayerId::F_CU).expect("front copper batch");
        let back = batch.layer(LayerId::B_CU).expect("back copper batch");
        assert_eq!((front.strokes.count, front.pads.count, front.polys), (10, 2, 1), "front adds eight ring strokes");
        assert_eq!((back.strokes.count, back.pads.count, back.polys), (0, 1, 0), "back counts");
        assert!(back.strokes.is_empty(), "back copper has no strokes");
        let silk = batch.layer(LayerId::B_SILK).expect("polygon-only layer batch");
        assert_eq!((silk.strokes.count, silk.pads.count, silk.tris.count, silk.polys), (3, 0, 1, 1),
            "polygon-only layer has a filled triangle and its outline");
        assert_eq!(batch.instances(), 18 + front.tris.count + batch.substrate.count,
            "total includes original shapes and drill, eight front outlines, silk outlines and all triangles including the substrate");
        assert_eq!(batch.holes.count, 1, "one drill instance");

        let substrate = batch.substrate.publication().expect("substrate publication");
        let outline_item = item(Tag { item: OUTLINE_ITEM, sub: 0, role: Role::Outline });
        let mut substrate_area = 0.0f64;
        for row in substrate.data().chunks_exact(batch.substrate.slots) {
            assert_eq!(row[10], outline_item, "substrate triangle carries OUTLINE_ITEM's shader encoding");
            let (ax, ay, bx, by, cx, cy) = (
                row[19] as f64, row[20] as f64, row[21] as f64,
                row[22] as f64, row[23] as f64, row[24] as f64,
            );
            substrate_area += ((bx - ax) * (cy - ay) - (by - ay) * (cx - ax)).abs() * 0.5;
        }
        assert!((substrate_area - 200.0).abs() < 1e-3,
            "substrate triangles cover 200 mm^2, got {substrate_area}");

        let strokes = front.strokes.publication().expect("stroke publication");
        let outlines: Vec<_> = strokes.data().chunks_exact(front.strokes.slots)
            .filter(|row| row[10] == 13.0).collect();
        assert_eq!(outlines.len(), 8, "four edges for the outer ring and four for the hole");
        assert!(outlines.iter().all(|row| row[23..26] == [0.0; 3]),
            "polygon outlines are straight hairlines with zero along0");
        let tris = front.tris.publication().expect("triangle publication");
        let mut area = 0.0f64;
        for row in tris.data().chunks_exact(front.tris.slots) {
            assert_eq!(row[10], 13.0, "triangle retains the polygon item");
            let (ax, ay, bx, by, cx, cy) = (
                row[19] as f64, row[20] as f64, row[21] as f64,
                row[22] as f64, row[23] as f64, row[24] as f64,
            );
            let triangle_area = ((bx - ax) * (cy - ay) - (by - ay) * (cx - ax)).abs() * 0.5;
            assert!(triangle_area >= 1e-12, "no degenerate triangle survives");
            area += triangle_area;
            let x = (ax + bx + cx) / 3.0;
            let y = (ay + by + cy) / 3.0;
            assert!(!(x > 3.0 && x < 7.0 && y > 3.0 && y < 7.0), "triangle centroid lies outside the hole");
        }
        assert!((area - 84.0).abs() < 1e-3, "polygon triangles cover 84 mm^2, got {area}");
        let first = &strokes.data()[..front.strokes.slots];
        let second = &strokes.data()[front.strokes.slots..];
        assert_eq!(&first[19..23], &[0.0, 0.0, 10.0, 0.0], "straight endpoints in mm");
        assert_eq!(first[23], 0.0, "straight stroke has no sagitta");
        assert!((first[24] - 0.4).abs() < 1e-6, "stroke width in mm");
        assert_eq!(first[25], 0.0, "track starts at zero arc length");
        assert_eq!(second[25], 10.0, "second stroke starts after the first's length");
        // This midpoint is RIGHT of a->b in Y-up coordinates. Reversing the chord is left.
        let expected_sag = 10.0 - 10.0 / 2.0_f32.sqrt();
        assert!(second[23] < 0.0, "right-bulging quarter circle has negative sagitta");
        assert!((second[23] + expected_sag).abs() < 0.001, "quarter-circle sagitta magnitude");
        let reverse = stroke(&Shape::Arc { a: b, mid, b: a, w: mm(0.4) }, Common::rest(7.0))
            .expect("reversed arc instance").0;
        assert!((reverse.sag - expected_sag).abs() < 0.001, "left-bulging quarter circle has positive sagitta");
        assert_eq!(first[10], 7.0, "stroke hover item");
        assert_eq!(first[11], 0.0, "committed look");
        assert_eq!(&first[12..16], &[0.0; 4], "ink uses the layer uniform");
        // A board NetId is not a simulation row: without an index even assigned nets are off.
        assert_eq!(first[16], -1.0, "no simulation means no net row, even for board NetId(3)");
        assert_eq!(&first[17..19], &[-1.0; 2], "no simulation means no segment or part rows");
        assert_eq!(&second[16..19], &[-1.0; 3], "unassigned live rows stay absent");

        let pads = front.pads.publication().expect("pad publication");
        let rect = &pads.data()[..front.pads.slots];
        let circle = &pads.data()[front.pads.slots..];
        assert_eq!(rect[10], 9.0, "negative tile sorts before the circle's tile");
        assert_eq!(circle[10], 8.0, "circle row retains its item");
        assert_eq!(&circle[19..23], &[5.0, 5.0, 1.0, 1.0], "circle centre and radii");
        assert_eq!(circle[24], 1.0, "circle corner radius");
        assert_eq!(&circle[25..28], &[5.0, 5.0, 0.0], "round hole centre and half length");
        assert!((circle[28] - 0.3).abs() < 1e-6, "round hole radius");
        assert_eq!(circle[29], 0.0, "unrotated round hole");
        assert!((rect[23] - std::f32::consts::FRAC_PI_2).abs() < 1e-6, "rect rotation is radians");
        assert_eq!(rect[24], 1.0, "rect corner clamps to the smaller half size");
        assert!((rect[27] - 0.4).abs() < 1e-6, "slot half length excludes its caps");
        assert!((rect[28] - 0.2).abs() < 1e-6, "slot radius");
        let slot_rot = 2.0 * std::f32::consts::PI / 3.0;
        assert!((rect[29] - slot_rot).abs() < 1e-6, "vertical slot adds pi/2 to hole_rot");
        let drills = batch.holes.publication().expect("drill publication");
        let drill = drills.data();
        assert_eq!(drill[10], 9.0, "drill uses its tag's item");
        assert!((drill[21] - 0.6).abs() < 1e-6, "drill half width includes caps");
        assert!((drill[22] - 0.2).abs() < 1e-6, "drill half height is its radius");
        assert!((drill[23] - slot_rot).abs() < 1e-6, "drill rotation follows the slot axis");
        assert!((drill[24] - 0.2).abs() < 1e-6, "drill corners form a stadium");
        assert_eq!(&drill[25..30], &[0.0; 5], "drill has no inner hole");
        let far_tile = back.pads.tiles[0].0;
        assert_eq!(far_tile, TileId { lod: BATCH_LOD, x: -3, y: 2 }, "negative coordinates floor into tiles");
        assert!(front.pads.tiles.iter().all(|(tile, _)| *tile != far_tile), "far pad is in a different tile");
        assert!(far_tile.rect_nm().contains(P::mm(-40.0, 40.0)), "tile contains the far pad's centre");
        assert_eq!(tile_key(BBox::new(P::new(-1, -1), P::ZERO)), (-1, -1), "half-nm negative centres floor");
        let oval = back.pads.tiles[0].1.data();
        assert_eq!(&oval[21..25], &[3.0, 1.0, 0.0, 1.0], "oval half sizes, rotation and corner");
        assert_eq!(&oval[25..30], &[0.0; 5], "undrilled pad has no hole");

        let again = batch_scene(&scene, &t, &LiveIx::none());
        fn kinds(board: &BoardBatch) -> impl Iterator<Item = &KindBatch> {
            board.layers.iter().flat_map(|l| [&l.strokes, &l.pads, &l.tris])
                .chain([&board.substrate, &board.holes])
        }
        for (kind, repeated) in kinds(&batch).zip(kinds(&again)) {
            assert_eq!(kind.count, repeated.count, "deterministic instance count");
            assert_eq!(kind.tiles.len(), repeated.tiles.len(), "deterministic tile count");
            assert!(kind.tiles.iter().zip(kind.tiles.iter().skip(1)).all(|(a, b)| {
                (a.0.y, a.0.x) < (b.0.y, b.0.x)
            }), "tiles sort by y then x");
            for ((tile, segment), (again_tile, again_segment)) in kind.tiles.iter().zip(repeated.tiles.iter()) {
                assert_eq!(segment.float_len() % kind.slots, 0, "tile contains whole rows");
                assert_eq!(tile, again_tile, "deterministic tile identities");
                assert_eq!(segment.data(), again_segment.data(), "deterministic row data");
            }
            let publication = kind.publication().expect("every stream publishes, including empty streams");
            assert_eq!(publication.float_len(), kind.count * kind.slots, "publication contains every row");
        }

        // Continue after the arc, then change item and role: accumulation is in scene order.
        let following = SceneShape {
            shape: Shape::Segment { a: b, b: P::mm(0.0, 12.0), w: mm(0.4) },
            hole: None, tag: Tag { sub: 2, ..track }, net: None,
        };
        scene.layers[0].1.shapes.insert(2, following.clone());
        scene.layers[0].1.shapes.insert(3, SceneShape { tag: tag(12, 0, Role::Track), ..following.clone() });
        scene.layers[0].1.shapes.insert(4, SceneShape { tag: tag(12, 1, Role::Graphic), ..following });
        let continued = batch_scene(&scene, &t, &LiveIx::none());
        let continued = continued.layer(LayerId::F_CU).expect("continued front layer")
            .strokes.publication().expect("continued strokes");
        let rows: Vec<_> = continued.data().chunks_exact(t.stroke.slots()).collect();
        assert!((rows[2][25] - (10.0 + 5.0 * std::f32::consts::PI)).abs() < 0.001,
            "track accumulation includes the arc's length");
        assert_eq!(rows[3][25], 0.0, "a new track item resets accumulated length");
        assert_eq!(rows[4][25], 0.0, "a non-track stroke has zero along0");
        let straight_arc = stroke(&Shape::Arc {
            a: P::ZERO, mid: P::mm(1.0, 0.0), b: P::mm(2.0, 0.0), w: mm(0.4),
        }, Common::rest(-1.0)).expect("collinear arc instance");
        assert_eq!(straight_arc.0.sag, 0.0, "collinear arc falls back to a straight stroke");
        assert_eq!(straight_arc.1 / NM_PER_MM, 2.0, "collinear arc uses straight length");
        assert_eq!(item(Tag { item: OUTLINE_ITEM, ..track }), -1.0, "outline has no hover item");
        assert_eq!(item(tag(1 << 24, 0, Role::Track)), -1.0, "inexact hover IDs are disabled");
        assert_eq!(item(tag((1 << 24) - 1, 0, Role::Track)), ((1 << 24) - 1) as f32,
            "largest permitted hover ID remains exact");

        // Appended source rows can have lower IDs; shapes also precede polygon
        // ring rows in the source. Sorting must use the full ID, stably, in both cases.
        for id in [OUTLINE_ITEM, ItemId((1 << 24) + 1), ItemId(2)] {
            scene.layers[0].1.shapes.push(SceneShape {
                shape: Shape::Segment { a: P::ZERO, b: P::mm(1.0, 0.0), w: mm(0.1) },
                hole: None, tag: Tag { item: id, sub: 0, role: Role::Graphic }, net: None,
            });
        }
        let sorted = batch_scene(&scene, &t, &LiveIx::none());
        let tile = TileId { lod: BATCH_LOD, x: 0, y: 0 };
        let ranges = sorted.rows.tile_rows(Stream::Strokes(LayerId::F_CU), tile);
        assert_eq!(ranges.iter().map(|r| r.item).collect::<Vec<_>>(),
            vec![ItemId(2), ItemId(7), ItemId(12), ItemId(13), ItemId((1 << 24) + 1), OUTLINE_ITEM],
            "tile rows sort full IDs, polygon outlines, and the outline sentinel together");
        assert!(ranges.windows(2).all(|w| w[0].start + w[0].len == w[1].start),
            "item ranges partition a tile without gaps");
        let track_rows = sorted.layer(LayerId::F_CU).unwrap().strokes.tiles.get(tile).unwrap().data();
        let starts: Vec<_> = (ranges[1].start..ranges[1].start + ranges[1].len)
            .map(|row| track_rows[row * t.stroke.slots() + 25]).collect();
        assert_eq!(starts.len(), 3, "stable sort retains every track segment");
        assert_eq!(starts[0], 0.0, "stable sort retains the first track segment");
        assert_eq!(starts[1], 10.0, "stable sort retains the second track segment");
        assert!((starts[2] - (10.0 + 5.0 * std::f32::consts::PI)).abs() < 0.001,
            "stable sort retains the third track segment and its accumulated arc length");
    }
}

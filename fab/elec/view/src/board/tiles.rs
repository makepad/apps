//! Layer art: a board's layers rasterised into board-space tiles, what Fab's 3D board material
//! samples (and what the 2D board view can reuse as its far-zoom picture). The packing is the one
//! contract EBOARD and E3D share:
//!
//! | Plane | r | g | b | a |
//! |---|---|---|---|---|
//! | `Surface(side)` | outer copper | mask opening | silk | drill |
//! | `Paste(side)` | paste | 0 | 0 | 0 |
//! | `Net(side)` (reserved) | net index bits 0..8 | 8..16 | 16..24 | copper |
//! | `Inner(layer)` (reserved) | inner copper | 0 | 0 | 0 |
//!
//! Every value is linear area coverage in 8-bit unorm (a CPU-written tile is u32 `0xAARRGGBB`
//! words, `VecBGRAu8_32`), never a finished colour: albedo, metalness and roughness are composed
//! from the finish and mask presets by whoever draws the tile, so a change of mask colour never
//! re-rasterises a tile, and height is coverage times the stackup's thickness. Copper has the holes
//! cut out; silk is as authored (not clipped by the openings); the drill channel reads the same on
//! both sides; `a` is data, never blending alpha; the outline is the 3D mesh's, not a channel.
//!
//! Tiles sit on one fixed grid per level of detail in the board frame (nm, Y up, origin at the
//! outline's lower left): at `lod` a texel is `ART_BASE_NM << lod` nm and a tile spans
//! `ART_TILE_TEXELS` texels, so tile `(x, y)` covers `[x, x + 1) * span` by `[y, y + 1) * span`.
//! It STORES `ART_TILE_STORED` texels: an apron of `ART_APRON` texels on every side, rasterised
//! from the same board (equal to the neighbours' edge texels), so bilinear taps and gradients at a
//! tile edge read real data and a relief normal has no seam. Stored texel `(i, j)` is centred at
//! `rect.min + (i - ART_APRON + 0.5, j - ART_APRON + 0.5) * texel`; row 0 is the tile's BOTTOM
//! (board Y up); [`TileId::uv`] maps a board point to the texture.
//!
//! Hosting: one [`BoardArt`] per board, owned by the host. Each frame the host opens a frame
//! ([`BoardArt::begin_frame`]); every view states the tiles it samples this frame
//! ([`BoardArt::want_region`] or [`BoardArt::want_tiles`], unioned across views); the host then
//! rasterises [`BoardArt::missing`] (coarse levels first) and calls [`BoardArt::evict`], all before
//! any view draws (never in draw order). Readers call [`BoardArt::cover`]: per tile of the level
//! they want, that tile when it is in, else its nearest resident coarser ancestor, so a view is
//! never blank while a fine tile is on its way. Each plane keeps its base level over the whole
//! board resident ([`BoardArt::base_lod`], the finest level that spans the board in at most four
//! tiles). A tile drawn from an older board generation stays valid until its replacement lands, so
//! an edit never shows a hole.
//!
//! Residency follows the view, not the board: a tile nobody wanted this frame may be evicted, the
//! longest unwanted first, once the resident bytes pass the host's budget. A want is never
//! refused: wanted tiles stay resident past the budget (logged once per episode), and a tile
//! evicted and wanted again is rasterised again from the board.
//!
//! Performance: wants and the cover lookups are hash operations per tile in view (tens to a few
//! hundred), nothing per board item. A tile costs `ART_TILE_STORED^2 * 4` bytes (270 KB) and is
//! rasterised once per change of an item in it, on the GPU.

use fab_elec_core::{BBox, LayerId, Nm, Side, P};
use makepad_widgets_core::{log, Texture};
use std::collections::{HashMap, HashSet};

/// Texels a tile spans along its side.
pub const ART_TILE_TEXELS: u32 = 256;
/// Texels of apron stored on every side of a tile, beyond the span.
pub const ART_APRON: u32 = 2;
/// Texels a tile stores along its side: the span plus the apron on both sides.
pub const ART_TILE_STORED: u32 = ART_TILE_TEXELS + 2 * ART_APRON;
/// Texel size at level of detail 0, in nm (1 um). Level `lod` doubles it `lod` times.
pub const ART_BASE_NM: Nm = 1_000;
/// The coarsest level: a texel of about 1 mm, a tile of about 268 mm.
pub const ART_MAX_LOD: u8 = 10;

/// One tile of the fixed grid at one level of detail.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TileId {
    pub lod: u8,
    pub x: i32,
    pub y: i32,
}

impl TileId {
    /// The board-space side of a tile at `lod`, in nm.
    pub fn span(lod: u8) -> i64 {
        ((ART_BASE_NM as i64) << lod.min(ART_MAX_LOD)) * ART_TILE_TEXELS as i64
    }

    /// The board-space rectangle the tile's texels cover (max exclusive in texel terms; as a
    /// `BBox` it is the closed rectangle whose max edge is the next tile's min edge).
    pub fn rect_nm(self) -> BBox {
        let s = Self::span(self.lod);
        let lo = |i: i32| (i as i64 * s).clamp(Nm::MIN as i64, Nm::MAX as i64) as Nm;
        BBox { min: P::new(lo(self.x), lo(self.y)), max: P::new(lo(self.x + 1), lo(self.y + 1)) }
    }

    /// Every tile at `lod` that a box touches, row by row from the lower left.
    pub fn covering(lod: u8, b: BBox) -> Vec<TileId> {
        Self::cells(lod, b).collect()
    }

    /// The tiles `covering` lists, without collecting them.
    pub fn cells(lod: u8, b: BBox) -> impl Iterator<Item = TileId> {
        let (x0, y0, x1, y1) = Self::cell_range(lod, b).unwrap_or((0, 0, -1, -1));
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| TileId { lod, x, y }))
    }

    /// How many tiles `covering` lists, in O(1).
    pub fn covering_len(lod: u8, b: BBox) -> usize {
        Self::cell_range(lod, b).map_or(0, |(x0, y0, x1, y1)| (x1 - x0 + 1) as usize * (y1 - y0 + 1) as usize)
    }

    /// The first and last cell column and row a box touches at `lod` (`None` for an empty box).
    fn cell_range(lod: u8, b: BBox) -> Option<(i32, i32, i32, i32)> {
        if b.is_empty() {
            return None;
        }
        let s = Self::span(lod);
        let cell = |v: Nm| (v as i64).div_euclid(s) as i32;
        Some((cell(b.min.x), cell(b.min.y), cell(b.max.x), cell(b.max.y)))
    }

    /// A texel's side at `lod`, in nm.
    pub fn texel(lod: u8) -> i64 {
        (ART_BASE_NM as i64) << lod.min(ART_MAX_LOD)
    }

    /// The board point at the centre of stored texel `(i, j)` (`0..ART_TILE_STORED`, row 0 at
    /// the bottom), in nm.
    pub fn texel_center(self, i: u32, j: u32) -> (f64, f64) {
        let t = Self::texel(self.lod) as f64;
        let r = self.rect_nm();
        let off = |k: u32| (k as f64 - ART_APRON as f64 + 0.5) * t;
        (r.min.x as f64 + off(i), r.min.y as f64 + off(j))
    }

    /// The texture coordinate of board point `p` (nm) in this tile's stored texels: `[2/260,
    /// 258/260]` over the spanned area, v up with the board.
    pub fn uv(self, p: (f64, f64)) -> (f64, f64) {
        let r = self.rect_nm();
        let s = Self::span(self.lod) as f64;
        let f = |v: f64, lo: Nm| ((v - lo as f64) / s * ART_TILE_TEXELS as f64 + ART_APRON as f64) / ART_TILE_STORED as f64;
        (f(p.0, r.min.x), f(p.1, r.min.y))
    }

    /// The tile one level coarser that contains this one (itself at `ART_MAX_LOD`).
    pub fn parent(self) -> TileId {
        if self.lod >= ART_MAX_LOD {
            return self;
        }
        TileId { lod: self.lod + 1, x: self.x.div_euclid(2), y: self.y.div_euclid(2) }
    }

    /// The coarsest level whose texel is no larger than `texel_nm` (the level a viewer that
    /// sees one texel per `texel_nm` of board asks for).
    pub fn lod_for_texel(texel_nm: f64) -> u8 {
        let mut lod = 0;
        while lod < ART_MAX_LOD && ((ART_BASE_NM as f64) * (1u64 << (lod + 1)) as f64) <= texel_nm {
            lod += 1;
        }
        lod
    }
}

/// What a tile holds (module docs; the shared packing doc has the channels).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ArtPlane {
    /// One side's outer copper, mask opening, silk and drill (rgba).
    Surface(Side),
    /// One side's paste (r).
    Paste(Side),
    /// Reserved: one side's topmost copper net index (rgb, 24 bits, sampled nearest) and copper (a).
    Net(Side),
    /// Reserved: one inner copper layer (r), for a cut-away view.
    Inner(LayerId),
}

impl ArtPlane {
    /// A total order for listing (top before bottom, inner layers by id).
    pub fn key(self) -> (u8, u8) {
        let side = |s: Side| if s == Side::Top { 0 } else { 1 };
        match self {
            ArtPlane::Surface(s) => (0, side(s)),
            ArtPlane::Paste(s) => (1, side(s)),
            ArtPlane::Net(s) => (2, side(s)),
            ArtPlane::Inner(l) => (3, l.0),
        }
    }
}

/// One plane's coverage over one tile. `T` is the texture (a GPU `Texture` in the app).
#[derive(Clone, Debug)]
pub struct LayerArt<T = Texture> {
    pub plane: ArtPlane,
    pub tile: TileId,
    /// `ART_TILE_STORED` square, RGBA8 unorm, linear (module docs).
    pub texture: T,
    /// The spanned rectangle (the apron lies outside it).
    pub rect_nm: BBox,
    /// The board generation the texels show (older than the board's while a redraw is pending).
    pub board_generation: u64,
    /// Bumps each time the texture is drawn again: a sampler re-binds when it changes.
    pub generation: u64,
}

/// The overlap of two boxes, None when they do not meet.
fn clip(a: BBox, b: BBox) -> Option<BBox> {
    let r = BBox { min: P::new(a.min.x.max(b.min.x), a.min.y.max(b.min.y)), max: P::new(a.max.x.min(b.max.x), a.max.y.min(b.max.y)) };
    (r.min.x <= r.max.x && r.min.y <= r.max.y && !a.is_empty() && !b.is_empty()).then_some(r)
}

/// Bytes one stored tile takes (RGBA8).
pub const ART_TILE_BYTES: usize = (ART_TILE_STORED * ART_TILE_STORED * 4) as usize;

struct Resident<T> {
    art: LayerArt<T>,
    /// The frame it was last wanted in (the base level counts as wanted every frame).
    wanted: u64,
}

/// The layer art of one board: the resident tiles, this frame's wants, and the residency budget.
/// One per board, owned by the host (module docs).
pub struct BoardArt<T = Texture> {
    tiles: HashMap<(ArtPlane, TileId), Resident<T>>,
    /// This frame's wants, unioned across views.
    wants: HashSet<(ArtPlane, TileId)>,
    /// Planes asked for at least once: their base level stays resident.
    planes: Vec<ArtPlane>,
    board: BBox,
    /// The base level's tiles over the board (at most four), kept with the box.
    base_tiles: Vec<TileId>,
    board_generation: u64,
    frame: u64,
    generation: u64,
    budget_bytes: usize,
    over_budget: bool,
}

impl<T> std::fmt::Debug for BoardArt<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoardArt").field("tiles", &self.tiles.len()).field("wants", &self.wants.len())
            .field("board_generation", &self.board_generation).field("frame", &self.frame).finish()
    }
}

impl<T: Clone> BoardArt<T> {
    /// The art of a board whose outline box is `board`, keeping at most about `budget_bytes` of
    /// tiles nobody wants (wanted tiles always stay).
    pub fn new(board: BBox, budget_bytes: usize) -> Self {
        let base_tiles = TileId::covering(Self::base_lod(board), board);
        Self {
            tiles: HashMap::new(), wants: HashSet::new(), planes: Vec::new(), board, base_tiles,
            board_generation: 0, frame: 0, generation: 0, budget_bytes, over_budget: false,
        }
    }

    /// The finest level whose tiles cover `board` in at most four tiles: each plane keeps this
    /// level resident over the whole board. O(levels), no tile lists.
    pub fn base_lod(board: BBox) -> u8 {
        (0..=ART_MAX_LOD).find(|&lod| TileId::covering_len(lod, board) <= 4).unwrap_or(ART_MAX_LOD)
    }

    /// The board's outline box changed (the base level follows it).
    pub fn set_board(&mut self, board: BBox) {
        self.board = board;
        self.base_tiles = TileId::covering(Self::base_lod(board), board);
    }

    pub fn set_budget(&mut self, budget_bytes: usize) {
        self.budget_bytes = budget_bytes;
    }

    /// Opens a frame: the previous frame's wants lapse. O(planes x base tiles); allocates nothing
    /// once the want set has grown to a frame's size.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.wants.clear();
        for i in 0..self.planes.len() {
            let plane = self.planes[i];
            for k in 0..self.base_tiles.len() {
                self.mark(plane, self.base_tiles[k]);
            }
        }
    }

    fn mark(&mut self, plane: ArtPlane, tile: TileId) {
        if !self.planes.contains(&plane) {
            self.planes.push(plane);
            for k in 0..self.base_tiles.len() {
                self.mark(plane, self.base_tiles[k]);
            }
        }
        self.wants.insert((plane, tile));
        if let Some(r) = self.tiles.get_mut(&(plane, tile)) {
            r.wanted = self.frame;
        }
    }

    /// This frame, the view samples `plane` at `lod` over `region` (board nm; clipped to the board).
    pub fn want_region(&mut self, plane: ArtPlane, lod: u8, region: BBox) {
        let Some(r) = clip(region, self.board) else { return };
        for t in TileId::cells(lod, r) {
            self.mark(plane, t);
        }
    }

    /// This frame, the view samples these tiles of `plane`.
    pub fn want_tiles(&mut self, plane: ArtPlane, tiles: &[TileId]) {
        for &t in tiles {
            self.mark(plane, t);
        }
    }

    /// What the rasteriser owes this frame: wanted tiles that are not in or show an older board
    /// generation, coarse levels first (they stand in for the fine ones meanwhile), then in tile
    /// order.
    pub fn missing(&self) -> Vec<(ArtPlane, TileId)> {
        let mut out: Vec<(ArtPlane, TileId)> = self.wants.iter().copied()
            .filter(|k| self.tiles.get(k).map_or(true, |r| r.art.board_generation < self.board_generation))
            .collect();
        out.sort_by_key(|(p, t)| (std::cmp::Reverse(t.lod), p.key(), t.y, t.x));
        out
    }

    /// Drops tiles nobody wanted this frame, the longest unwanted first, until the resident bytes
    /// are within the budget. Returns the evicted textures (the host frees them). Wanted tiles
    /// stay even past the budget (logged once each time that starts).
    pub fn evict(&mut self) -> Vec<T> {
        let mut bytes = self.tiles.len() * ART_TILE_BYTES;
        let mut out = Vec::new();
        if bytes > self.budget_bytes {
            let mut idle: Vec<((ArtPlane, TileId), u64)> = self.tiles.iter()
                .filter(|(k, _)| !self.wants.contains(k))
                .map(|(k, r)| (*k, r.wanted))
                .collect();
            idle.sort_by_key(|(k, wanted)| (*wanted, k.0.key(), k.1));
            for (k, _) in idle {
                if bytes <= self.budget_bytes {
                    break;
                }
                if let Some(r) = self.tiles.remove(&k) {
                    out.push(r.art.texture);
                    bytes -= ART_TILE_BYTES;
                }
            }
        }
        let over = bytes > self.budget_bytes;
        if over && !self.over_budget {
            log!("fab.board art: {} wanted tiles ({} MB) exceed the {} MB budget; keeping them",
                self.wants.len(), bytes >> 20, self.budget_bytes >> 20);
        }
        self.over_budget = over;
        out
    }

    /// Per tile of `plane` at `lod` over `region`: that tile when it is in, else its nearest
    /// resident coarser ancestor (each listed once), in tile order. Empty only before the base
    /// level is in.
    pub fn cover(&self, plane: ArtPlane, lod: u8, region: BBox) -> Vec<LayerArt<T>> {
        let mut out = Vec::new();
        self.cover_into(plane, lod, region, &mut out);
        out
    }

    /// `cover` into `out` (cleared first): a caller that keeps `out` across frames allocates
    /// nothing in the steady state. O(tiles in the region x levels up to a resident one).
    pub fn cover_into(&self, plane: ArtPlane, lod: u8, region: BBox, out: &mut Vec<LayerArt<T>>) {
        out.clear();
        let Some(r) = clip(region, self.board) else { return };
        for mut t in TileId::cells(lod, r) {
            loop {
                if let Some(res) = self.tiles.get(&(plane, t)) {
                    // the region's tiles run lower left first, so a shared ancestor repeats
                    // mostly right after itself; the sort and dedup below catch the rest
                    if out.last().map_or(true, |a| a.tile != t) {
                        out.push(res.art.clone());
                    }
                    break;
                }
                if t.lod >= ART_MAX_LOD {
                    break;
                }
                t = t.parent();
            }
        }
        out.sort_unstable_by_key(|a| a.tile);
        out.dedup_by_key(|a| a.tile);
    }

    /// The resident tiles of `plane` at `lod`, in tile order.
    pub fn layer_art(&self, plane: ArtPlane, lod: u8) -> Vec<LayerArt<T>> {
        let mut out: Vec<LayerArt<T>> = self.tiles.values().filter(|r| r.art.plane == plane && r.art.tile.lod == lod).map(|r| r.art.clone()).collect();
        out.sort_by_key(|t| t.tile);
        out
    }

    /// Resident tiles, all planes and levels.
    pub fn resident(&self) -> usize {
        self.tiles.len()
    }

    /// The newest texture generation handed out: it moves whenever any tile is drawn.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The board generation the art is drawn towards.
    pub fn board_generation(&self) -> u64 {
        self.board_generation
    }

    /// The stored tile of `plane` at `tile`, if any.
    pub fn tile(&self, plane: ArtPlane, tile: TileId) -> Option<&LayerArt<T>> {
        self.tiles.get(&(plane, tile)).map(|r| &r.art)
    }

    /// Store a freshly rasterised tile (the rasteriser's side), replacing the older one; returns
    /// the replaced texture (the host frees it) with the new texture generation.
    pub fn put(&mut self, plane: ArtPlane, tile: TileId, texture: T, board_generation: u64) -> (u64, Option<T>) {
        self.generation += 1;
        let art = LayerArt { plane, tile, texture, rect_nm: tile.rect_nm(), board_generation, generation: self.generation };
        let wanted = if self.wants.contains(&(plane, tile)) { self.frame } else { 0 };
        let old = self.tiles.insert((plane, tile), Resident { art, wanted }).map(|r| r.art.texture);
        (self.generation, old)
    }

    /// A board edit arrived: tiles that were current and whose stored area (apron included)
    /// meets none of `changed` keep their texels and take `board_generation`; the others stay on
    /// their old generation (and keep showing) until the rasteriser redraws them. A tile an
    /// earlier edit left owed stays owed: `changed` says only where this edit differs.
    pub fn advance(&mut self, board_generation: u64, changed: &[BBox]) {
        let previous = self.board_generation;
        self.board_generation = board_generation;
        for r in self.tiles.values_mut() {
            let t = &mut r.art;
            let apron = TileId::texel(t.tile.lod) * ART_APRON as i64;
            let rect = t.rect_nm;
            let grow = |v: Nm, by: i64| (v as i64 + by).clamp(Nm::MIN as i64, Nm::MAX as i64) as Nm;
            let stored = BBox { min: P::new(grow(rect.min.x, -apron), grow(rect.min.y, -apron)), max: P::new(grow(rect.max.x, apron), grow(rect.max.y, apron)) };
            if t.board_generation >= previous && !changed.iter().any(|c| c.intersects(&stored)) {
                t.board_generation = board_generation;
            }
        }
    }

    /// Drop every tile of a plane (its layer left the stackup); returns their textures.
    pub fn forget(&mut self, plane: ArtPlane) -> Vec<T> {
        self.planes.retain(|p| *p != plane);
        self.wants.retain(|w| w.0 != plane);
        let keys: Vec<_> = self.tiles.keys().filter(|k| k.0 == plane).copied().collect();
        keys.into_iter().filter_map(|k| self.tiles.remove(&k)).map(|r| r.art.texture).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_grid_is_seamless_and_covers() {
        // spans double per level and level 0 is 256 um
        assert_eq!(TileId::span(0), 256_000, "level 0 span");
        assert_eq!(TileId::span(6), 256_000 << 6, "level 6 span");
        // neighbours share an edge exactly, negative cells included
        for lod in [0u8, 5, ART_MAX_LOD] {
            for (x, y) in [(-1, -1), (0, 0), (3, 7)] {
                let a = TileId { lod, x, y }.rect_nm();
                let right = TileId { lod, x: x + 1, y }.rect_nm();
                let up = TileId { lod, x, y: y + 1 }.rect_nm();
                assert_eq!(a.max.x, right.min.x, "horizontal seam at lod {lod}");
                assert_eq!(a.max.y, up.min.y, "vertical seam at lod {lod}");
                assert_eq!(a.max.x as i64 - a.min.x as i64, TileId::span(lod), "tile width at lod {lod}");
            }
        }
        // a 50 x 30 mm board at level 6 (16.4 mm tiles): 4 x 2 tiles, each touching the board
        let board = BBox::new(P::mm(0.0, 0.0), P::mm(50.0, 30.0));
        let tiles = TileId::covering(6, board);
        assert_eq!(tiles.len(), 8, "tiles over the board");
        assert!(tiles.iter().all(|t| t.rect_nm().intersects(&board)), "every covering tile touches the board");
        assert_eq!(tiles[0], TileId { lod: 6, x: 0, y: 0 }, "lower-left first");
        // a viewer seeing 70 um per texel gets the 64 um level
        assert_eq!(TileId::lod_for_texel(70_000.0), 6, "level for a 70 um texel");
        assert_eq!(TileId::lod_for_texel(10.0), 0, "finer than level 0");
        // the apron: stored texel 2 starts the span, uv runs 2/260 .. 258/260 over it
        let t = TileId { lod: 6, x: 1, y: 0 };
        let texel = TileId::texel(6) as f64;
        let (cx, cy) = t.texel_center(ART_APRON, ART_APRON);
        assert_eq!((cx, cy), (TileId::span(6) as f64 + 0.5 * texel, 0.5 * texel), "first spanned texel centre");
        let (u, v) = t.uv((t.rect_nm().min.x as f64, t.rect_nm().max.y as f64));
        assert!((u - 2.0 / 260.0).abs() < 1e-12 && (v - 258.0 / 260.0).abs() < 1e-12, "uv of the span's corners");
        let (u0, _) = t.uv(t.texel_center(0, 0));
        assert!((u0 - 0.5 / 260.0).abs() < 1e-12, "uv of the outer apron texel centre");
        // an edit advances the tiles away from it and leaves the ones it touches stale
        let mut art = BoardArt::<u32>::new(BBox::new(P::ZERO, P::mm(100.0, 30.0)), usize::MAX);
        let plane = ArtPlane::Surface(Side::Top);
        let near = TileId { lod: 6, x: 0, y: 0 };
        let far = TileId { lod: 6, x: 3, y: 0 };
        art.put(plane, near, 0, 1);
        art.put(plane, far, 0, 1);
        art.advance(2, &[BBox::new(P::mm(1.0, 1.0), P::mm(2.0, 2.0))]);
        assert_eq!(art.tile(plane, near).map(|t| t.board_generation), Some(1), "the edited tile stays stale");
        assert_eq!(art.tile(plane, far).map(|t| t.board_generation), Some(2), "a tile away from the edit advances");
        art.advance(3, &[BBox::new(P::mm(90.0, 1.0), P::mm(91.0, 2.0))]);
        assert_eq!(art.tile(plane, near).map(|t| t.board_generation), Some(1), "a tile the last edit left owed stays owed though the next edit misses it");
        assert_eq!(art.tile(plane, far).map(|t| t.board_generation), Some(3), "a current tile away from both edits advances again");
        assert_eq!(art.layer_art(plane, 6).len(), 2, "both tiles listed");
        assert!(art.layer_art(ArtPlane::Paste(Side::Top), 6).is_empty(), "planes are separate");
        // the base level is arithmetic and equals the search over listed tiles, on boards of
        // several sizes and offsets, one straddling the origin and one long and thin
        let listed = |b: BBox| (0..=ART_MAX_LOD).find(|&lod| TileId::covering(lod, b).len() <= 4).unwrap_or(ART_MAX_LOD);
        for (x, y, w, h) in [(0.0, 0.0, 60.0, 40.0), (0.0, 0.0, 300.0, 200.0), (-12.5, -7.0, 25.0, 14.0), (3.0, -150.0, 1.0, 300.0), (100.0, 100.0, 0.3, 0.3), (-2000.0, 0.0, 4000.0, 10.0)] {
            let b = BBox::new(P::mm(x, y), P::mm(x + w, y + h));
            assert_eq!(BoardArt::<u32>::base_lod(b), listed(b), "base level of a {w} x {h} mm board at ({x}, {y})");
            for lod in [0u8, 4, ART_MAX_LOD] {
                assert_eq!(TileId::covering_len(lod, b), TileId::covering(lod, b).len(), "tile count of {w} x {h} mm at level {lod}");
            }
        }
        assert_eq!(BoardArt::<u32>::base_lod(BBox::EMPTY), listed(BBox::EMPTY), "an empty board");
        // cover_into gives cover's tiles and reuses its buffer
        let mut out = Vec::new();
        let region = BBox::new(P::ZERO, P::mm(100.0, 30.0));
        art.cover_into(plane, 6, region, &mut out);
        assert_eq!(out.iter().map(|a| a.tile).collect::<Vec<_>>(), art.cover(plane, 6, region).iter().map(|a| a.tile).collect::<Vec<_>>(), "cover_into lists cover's tiles");
        let cap = out.capacity();
        art.cover_into(plane, 6, region, &mut out);
        assert_eq!(out.capacity(), cap, "a second cover_into reuses the buffer");
    }

    /// A camera flying in over the fixture keeps the resident tiles bounded by the view (plus the
    /// base level and the budget), never by the board; a view is never blank while its fine tiles
    /// are on their way; panning away and back draws an evicted tile again, the same bits.
    #[test]
    fn residency_follows_the_view() {
        use std::hash::{Hash, Hasher};
        let design = fab_elec_fixtures::ledctl();
        let board = design.units.iter().find_map(|u| u.board.as_ref()).expect("fixture board").outline_bbox();
        let raster = |plane: ArtPlane, t: TileId, g: u64| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (plane.key(), t, g).hash(&mut h);
            h.finish()
        };
        // a close view wants about 16 to 25 level-0 tiles: past this budget every idle tile goes
        const BUDGET: usize = 16;
        let mut art = BoardArt::<u64>::new(board, BUDGET * ART_TILE_BYTES);
        let plane = ArtPlane::Surface(Side::Top);
        let centre = board.center();
        let view = |c: P, half_nm: i64| BBox::new(P::new(c.x - half_nm as Nm, c.y - half_nm as Nm), P::new(c.x + half_nm as Nm, c.y + half_nm as Nm));
        // a 1024 device pixel wide view
        let lod_of = |half_nm: i64| TileId::lod_for_texel(2.0 * half_nm as f64 / 1024.0);
        let base = TileId::covering(BoardArt::<u64>::base_lod(board), board).len();
        let frame = |art: &mut BoardArt<u64>, v: BBox, lod: u8| {
            art.begin_frame();
            art.want_region(plane, lod, v);
            let stand_in = art.cover(plane, lod, v);
            for (p, t) in art.missing() {
                art.put(p, t, raster(p, t, 0), 0);
            }
            art.evict();
            stand_in
        };
        let start = board.width().max(board.height()) / 2;
        let mut half = start;
        let mut frames = 0;
        while half > 400_000 {
            let lod = lod_of(half);
            let stand_in = frame(&mut art, view(centre, half), lod);
            if frames > 0 {
                assert!(!stand_in.is_empty(), "frame {frames}: the view has art before its fine tiles are drawn");
            }
            let in_view = TileId::covering(lod, clip(view(centre, half), board).unwrap()).len();
            assert!(art.resident() <= BUDGET.max(in_view + base), "frame {frames}: {} tiles resident for {in_view} in view", art.resident());
            half = half * 9 / 10;
            frames += 1;
        }
        let lod = lod_of(half);
        let whole = TileId::covering(lod, board).len();
        assert!(lod <= 1 && whole > 20 * BUDGET, "the fly-in ends where the whole board is {whole} tiles at level {lod}");
        let near = view(centre, half);
        frame(&mut art, near, lod);
        let kept = art.cover(plane, lod, near);
        assert!(kept.iter().all(|a| a.tile.lod == lod), "the close view is drawn at its own level");
        let probe = kept[0].tile;
        let bits = kept[0].texture;
        // pan to a far corner long enough for the probe to be evicted, then back
        let far = P::new(board.min.x + half as Nm, board.min.y + half as Nm);
        for _ in 0..4 {
            frame(&mut art, view(far, half), lod);
        }
        assert!(art.tile(plane, probe).is_none(), "a tile nobody wants is evicted under the budget");
        assert!(art.resident() <= BUDGET.max(TileId::covering(lod, view(far, half)).len() + base), "panned view stays bounded");
        frame(&mut art, near, lod);
        assert_eq!(art.tile(plane, probe).map(|a| a.texture), Some(bits), "the tile drawn again has the same bits");
        // an edit leaves the stand-ins showing until the new tiles land
        art.advance(1, &[near]);
        art.begin_frame();
        art.want_region(plane, lod, near);
        assert!(art.missing().iter().any(|(_, t)| *t == probe), "an edited tile is owed again");
        assert_eq!(art.tile(plane, probe).map(|a| a.board_generation), Some(0), "the old tile shows until its replacement lands");
    }

}

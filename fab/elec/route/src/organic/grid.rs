// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/SpatialGrid.h.
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Spatial hash and dense CSR grids, in the reference's `f32` mm.

use super::geom::{std_max, Box2, Vec2};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct GridNodeRef {
    pub trace_index: i32,
    pub point_index: i32,
    pub pos: Vec2,
    pub layer_index: i32,
    pub net_id: i32,
    pub is_fixed: bool,
}

impl Default for GridNodeRef {
    fn default() -> Self {
        Self {
            trace_index: -1,
            point_index: -1,
            pos: Vec2::default(),
            layer_index: 0,
            net_id: -1,
            is_fixed: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SpatialHashGrid {
    /// Cell size in mm.
    pub cell_size: f32,
    // The HashMap is only looked up, never iterated, so its order cannot affect queries.
    cells: HashMap<u64, Vec<GridNodeRef>>,
}

impl Default for SpatialHashGrid {
    fn default() -> Self {
        Self::new(2.5)
    }
}

impl SpatialHashGrid {
    pub fn new(cs: f32) -> Self {
        Self { cell_size: cs, cells: HashMap::new() }
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }

    #[inline]
    pub fn hash_coords(cx: i32, cy: i32) -> u64 {
        ((cx as u32 as u64) << 32) | (cy as u32 as u64)
    }

    #[inline]
    pub fn get_cell_coords(&self, p: Vec2) -> (i32, i32) {
        ((p.x / self.cell_size).floor() as i32, (p.y / self.cell_size).floor() as i32)
    }

    pub fn insert(&mut self, node: &GridNodeRef) {
        let (cx, cy) = self.get_cell_coords(node.pos);
        self.cells.entry(Self::hash_coords(cx, cy)).or_default().push(node.clone());
    }

    pub fn query_nearby(&self, pos: Vec2, radius: f32, mut cb: impl FnMut(&GridNodeRef)) {
        let min_x = ((pos.x - radius) / self.cell_size).floor() as i32;
        let max_x = ((pos.x + radius) / self.cell_size).floor() as i32;
        let min_y = ((pos.y - radius) / self.cell_size).floor() as i32;
        let max_y = ((pos.y + radius) / self.cell_size).floor() as i32;

        for cy in min_y..=max_y {
            for cx in min_x..=max_x {
                if let Some(items) = self.cells.get(&Self::hash_coords(cx, cy)) {
                    for item in items {
                        cb(item);
                    }
                }
            }
        }
    }
}

pub const UNIFORM_GRID_MAX_CELLS: i32 = 1 << 20;

/// Dense CSR buckets; out-of-bounds boxes clamp to border cells.
#[derive(Clone, Debug)]
pub struct UniformGrid {
    x0: f32,
    y0: f32,
    cs: f32,
    inv: f32,
    nx: i32,
    ny: i32,
    start: Vec<i32>,
    items: Vec<i32>,
    fill: Vec<i32>,
    pending: Vec<Pending>,
}

#[derive(Clone, Debug)]
struct Pending {
    id: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Default for UniformGrid {
    fn default() -> Self {
        Self {
            x0: 0.0,
            y0: 0.0,
            cs: 1.0,
            inv: 1.0,
            nx: 0,
            ny: 0,
            start: Vec::new(),
            items: Vec::new(),
            fill: Vec::new(),
            pending: Vec::new(),
        }
    }
}

impl UniformGrid {
    pub fn init(&mut self, bounds: &Box2, cell_size: f32, max_cells: i32) {
        self.x0 = bounds.min.x;
        self.y0 = bounds.min.y;
        let w = std_max(bounds.max.x - bounds.min.x, 1e-3_f32);
        let h = std_max(bounds.max.y - bounds.min.y, 1e-3_f32);
        self.cs = std_max(cell_size, 1e-3_f32);
        while (w / self.cs + 1.0) as f64 * (h / self.cs + 1.0) as f64 > max_cells as f64 {
            self.cs *= 1.5_f32;
        }
        self.nx = std::cmp::max(1, (w / self.cs).ceil() as i32);
        self.ny = std::cmp::max(1, (h / self.cs).ceil() as i32);
        self.inv = 1.0_f32 / self.cs;
        self.start.clear();
        self.start.resize(self.nx as usize * self.ny as usize + 1, 0);
        self.items.clear();
        self.pending.clear();
    }

    /// Cell counts along each axis.
    pub fn cells(&self) -> (i32, i32) {
        (self.nx, self.ny)
    }

    /// Register all items before calling `finalize`.
    pub fn add(&mut self, id: i32, minx: f32, miny: f32, maxx: f32, maxy: f32) {
        let (x0, y0, x1, y1) = self.range(minx, miny, maxx, maxy);
        self.pending.push(Pending { id, x0, y0, x1, y1 });
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.start[y as usize * self.nx as usize + x as usize + 1] += 1;
            }
        }
    }

    pub fn finalize(&mut self) {
        for i in 1..self.start.len() {
            self.start[i] += self.start[i - 1];
        }
        self.items.resize(*self.start.last().unwrap() as usize, 0);
        self.fill.clear();
        self.fill.extend_from_slice(&self.start[..self.start.len() - 1]);
        for p in &self.pending {
            for y in p.y0..=p.y1 {
                for x in p.x0..=p.x1 {
                    let c = y as usize * self.nx as usize + x as usize;
                    let k = self.fill[c];
                    self.fill[c] += 1;
                    self.items[k as usize] = p.id;
                }
            }
        }
        self.pending.clear();
    }

    /// A query-only copy of this finalized grid with the items `keep` accepts: the same cells, and
    /// within each cell the same order, so a query visits what it visited here less the rest.
    pub fn filtered(&self, mut keep: impl FnMut(i32) -> bool) -> Self {
        let mut out = Self {
            x0: self.x0, y0: self.y0, cs: self.cs, inv: self.inv, nx: self.nx, ny: self.ny,
            start: Vec::with_capacity(self.start.len()), items: Vec::new(),
            fill: Vec::new(), pending: Vec::new(),
        };
        for span in self.start.windows(2) {
            out.start.push(out.items.len() as i32);
            out.items.extend(self.items[span[0] as usize..span[1] as usize].iter().copied().filter(|&id| keep(id)));
        }
        out.start.push(out.items.len() as i32);
        out
    }

    /// Visit every item in overlapping cells; an id may be returned more than once.
    pub fn query(&self, minx: f32, miny: f32, maxx: f32, maxy: f32, mut cb: impl FnMut(i32)) {
        if self.nx == 0 {
            return;
        }
        let (x0, y0, x1, y1) = self.range(minx, miny, maxx, maxy);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let c = y as usize * self.nx as usize + x as usize;
                for k in self.start[c]..self.start[c + 1] {
                    cb(self.items[k as usize]);
                }
            }
        }
    }

    fn range(&self, minx: f32, miny: f32, maxx: f32, maxy: f32) -> (i32, i32, i32, i32) {
        let cx = |v: f32| {
            let f = (v - self.x0) * self.inv;
            if !(f > 0.0_f32) {
                return 0; // Also catches NaN.
            }
            std::cmp::min(self.nx - 1, f as i32)
        };
        let cy = |v: f32| {
            let f = (v - self.y0) * self.inv;
            if !(f > 0.0_f32) {
                return 0;
            }
            std::cmp::min(self.ny - 1, f as i32)
        };
        let mut x0 = cx(minx);
        let mut x1 = cx(maxx);
        let mut y0 = cy(miny);
        let mut y1 = cy(maxy);
        if x1 < x0 {
            std::mem::swap(&mut x0, &mut x1);
        }
        if y1 < y0 {
            std::mem::swap(&mut y0, &mut y1);
        }
        (x0, y0, x1, y1)
    }
}

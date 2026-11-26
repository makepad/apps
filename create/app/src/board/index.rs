use std::collections::HashMap;

pub const CELL_SIZE: f64 = 256.0;
// An 8-by-8 neighbourhood stays local. Larger sections and connectors cost one reference.
const MAX_CELLS: usize = 64;

/// A closed rectangle in world units. Either drag direction is valid in `new`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) }
    }

    pub fn intersects(self, other: Self) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }

    pub fn encloses(self, other: Self) -> bool {
        self.x0 <= other.x0 && self.y0 <= other.y0 && other.x1 <= self.x1 && other.y1 <= self.y1
    }

    pub(super) fn overlaps(self, other: Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }
}

// Beyond 2^53 consecutive representable whole numbers are adjacent float bit patterns.
// Number those cells consecutively too, so even extreme f64 coordinates have distinct buckets.
fn cell(value: f64) -> i64 {
    const EXACT: i64 = 1 << 53;
    let n = (value / CELL_SIZE).floor();
    if n == 0.0 && value < 0.0 { return -1 }
    if n.abs() <= EXACT as f64 {
        return n as i64;
    }
    let magnitude = EXACT + (n.abs().to_bits() - (EXACT as f64).to_bits()) as i64;
    if n.is_sign_negative() { -magnitude } else { magnitude }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cells {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Cells {
    fn of(rect: Rect) -> Self {
        Self { x0: cell(rect.x0), y0: cell(rect.y0), x1: cell(rect.x1), y1: cell(rect.y1) }
    }

    fn count(self) -> u64 {
        let width = self.x1.abs_diff(self.x0).saturating_add(1);
        let height = self.y1.abs_diff(self.y0).saturating_add(1);
        width.saturating_mul(height)
    }

    fn iter(self) -> impl Iterator<Item = (i64, i64)> {
        (self.y0..=self.y1).flat_map(move |y| (self.x0..=self.x1).map(move |x| (x, y)))
    }
}

enum Slots {
    Cells(Vec<usize>),
    Large(usize),
}

struct Entry {
    rect: Rect,
    slots: Slots,
}

/// Sparse handles and coordinates. Queries replace the output with ascending, unique handles.
#[derive(Default)]
pub struct RectIndex {
    entries: HashMap<u32, Entry>,
    cells: HashMap<(i64, i64), Vec<u32>>,
    large: Vec<u32>,
}

impl RectIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Also replaces an existing handle.
    pub fn insert(&mut self, handle: u32, rect: Rect) {
        let cells = Cells::of(rect);
        if let Some(entry) = self.entries.get_mut(&handle) {
            if Cells::of(entry.rect) == cells
                || (matches!(entry.slots, Slots::Large(_)) && cells.count() > MAX_CELLS as u64)
            {
                entry.rect = rect;
                return;
            }
            self.remove(handle);
        }
        let slots = if cells.count() > MAX_CELLS as u64 {
            let at = self.large.len();
            self.large.push(handle);
            Slots::Large(at)
        } else {
            let mut slots = Vec::with_capacity(cells.count() as usize);
            for cell in cells.iter() {
                let bucket = self.cells.entry(cell).or_default();
                slots.push(bucket.len());
                bucket.push(handle);
            }
            Slots::Cells(slots)
        };
        self.entries.insert(handle, Entry { rect, slots });
    }

    pub fn update(&mut self, handle: u32, rect: Rect) {
        self.insert(handle, rect);
    }

    pub fn remove(&mut self, handle: u32) {
        let Some(entry) = self.entries.remove(&handle) else { return };
        match entry.slots {
            Slots::Large(at) => {
                self.large.swap_remove(at);
                if let Some(moved) = self.large.get(at) {
                    self.entries.get_mut(moved).unwrap().slots = Slots::Large(at);
                }
            }
            Slots::Cells(slots) => {
                for (cell, at) in Cells::of(entry.rect).iter().zip(slots) {
                    let bucket = self.cells.get_mut(&cell).unwrap();
                    bucket.swap_remove(at);
                    if let Some(moved) = bucket.get(at) {
                        // Repair the swapped handle's slot. No scan through a pile of items.
                        let moved = self.entries.get_mut(moved).unwrap();
                        let cells = Cells::of(moved.rect);
                        let offset = (cell.1 - cells.y0) * (cells.x1 - cells.x0 + 1) + (cell.0 - cells.x0);
                        let Slots::Cells(slots) = &mut moved.slots else { unreachable!() };
                        slots[offset as usize] = at;
                    }
                    if bucket.is_empty() {
                        self.cells.remove(&cell);
                    }
                }
            }
        }
    }

    pub fn query_rect(&self, rect: Rect, out: &mut Vec<u32>) {
        out.clear();
        self.visit(rect, |h| out.push(h));
        out.sort_unstable();
    }

    pub fn query_point(&self, (x, y): (f64, f64), out: &mut Vec<u32>) {
        self.query_rect(Rect::new(x, y, x, y), out);
    }

    pub(super) fn visit(&self, rect: Rect, mut found: impl FnMut(u32)) {
        let cells = Cells::of(rect);
        if cells.count() > self.entries.len() as u64 {
            // A zoom-to-fit query can span billions of empty cells between two items.
            for (&h, entry) in &self.entries {
                if rect.intersects(entry.rect) { found(h); }
            }
            return;
        }
        for (x, y) in cells.iter() {
            if let Some(bucket) = self.cells.get(&(x, y)) {
                for h in bucket {
                    let item = self.entries[h].rect;
                    // Only the first shared cell reports the match. For a point both tests
                    // short-circuit, since its query starts and ends in the same cell.
                    if rect.intersects(item) && (x == cells.x0 || x == cell(item.x0))
                        && (y == cells.y0 || y == cell(item.y0))
                    {
                        found(*h);
                    }
                }
            }
        }
        for h in &self.large {
            if rect.intersects(self.entries[h].rect) { found(*h); }
        }
    }
}

//! A layer's cured pixels as parts, and how the parts of one layer stand on the layer below.
//!
//! The resin cures a pixel lit past its threshold, so a pixel counts as cured from half its
//! share ([`CURES`]). A layer's cured pixels are runs per row; runs that touch across a side or
//! at a corner make one part: the resin cures across a lit corner. A part of one layer stands on
//! a part of the layer below where the two share a pixel. A part that stands on nothing is an
//! island: printed as it is, it would cure on the vat floor with nothing holding it to the plate.
//!
//! The empty pixels a layer closes in are its holes ([`LayerParts::holes`]), joined only across
//! a side, so a gap two cured pixels close at a corner leaks no hole. A lit feature no pixel of
//! which cures is lost ([`Lost`]).

use crate::grid::PixelRect;
use crate::image::LayerStack;
use crate::spans::{Spans, FULL};

/// The share (of 255) a pixel cures from: half covered.
pub const CURES: u8 = 128;

/// Cured pixels of one row: columns `x0..x1` of row `y`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub y: u32,
    pub x0: u32,
    pub x1: u32,
}

/// One part of a layer: its runs, by row then column.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    pub rect: PixelRect,
    pub pixels: u64,
    pub runs: Vec<Run>,
}

/// A layer's cured pixels and its parts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerParts {
    /// The rows the runs lie in.
    rect: PixelRect,
    /// Row `rect.y0 + r` holds `runs[rows[r]..rows[r + 1]]`, left to right.
    rows: Vec<u32>,
    runs: Vec<Run>,
    /// Each run's part.
    part_of: Vec<u32>,
    pub parts: Vec<Part>,
    /// The features of the spans it was cut from that are lit but cure nowhere (none in a
    /// union).
    pub lost: Vec<Lost>,
}

/// A lit feature of a layer that cures nowhere: pixels lit under [`CURES`] with no cured pixel
/// beside them, across a side or a corner. A cured edge's own grey fringe always has one beside
/// it, so what is left is a wall or a pin thinner than about half a pixel, or the tip of a very
/// sharp corner. Its pixels touch across sides or corners.
#[derive(Clone, Debug, PartialEq)]
pub struct Lost {
    pub rect: PixelRect,
    /// Its pixels' lit shares added up, in pixels (a wholly covered pixel is 1).
    pub covered: f64,
}

/// The least a lost feature covers to count (pixels). Under it, a sharp corner's grey tip.
pub const LOST_LEAST: f64 = 1.0;

/// A layer's holes: its empty pixels that cured ones close in, open to no empty pixel around the
/// layer. Runs by row, then column, each with its hole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Holes {
    rect: PixelRect,
    /// Row `rect.y0 + r` holds `runs[rows[r]..rows[r + 1]]`.
    rows: Vec<u32>,
    pub runs: Vec<Run>,
    /// Each run's hole.
    pub of: Vec<u32>,
    /// Each hole's pixels and rect.
    pub pixels: Vec<u64>,
    pub rects: Vec<PixelRect>,
}

impl Holes {
    /// Row `y`'s runs and their holes (none outside the rect).
    pub fn row(&self, y: u32) -> (&[Run], &[u32]) {
        if y < self.rect.y0 || y >= self.rect.y1 {
            return (&[], &[]);
        }
        let r = (y - self.rect.y0) as usize;
        let span = self.rows[r] as usize..self.rows[r + 1] as usize;
        (&self.runs[span.clone()], &self.of[span])
    }
}

/// A union-find root, halving the path on the way.
fn root(up: &mut [u32], mut a: u32) -> u32 {
    while up[a as usize] != a {
        up[a as usize] = up[up[a as usize] as usize];
        a = up[a as usize];
    }
    a
}

impl LayerParts {
    /// The cured pixels of `spans` and their parts.
    pub fn of(spans: &Spans) -> LayerParts {
        let rect = spans.rect;
        let mut l = LayerParts { rect, rows: Vec::with_capacity(rect.height() as usize + 1), ..LayerParts::default() };
        l.rows.push(0);
        for y in rect.y0..rect.y1 {
            // the run being grown: pixels cured side by side, from wholly covered runs and the
            // cured ones among the edge pixels
            let mut open: Option<(u32, u32)> = None;
            let mut cured = |a: u32, b: u32, runs: &mut Vec<Run>| match &mut open {
                Some((_, end)) if *end == a => *end = b,
                _ => {
                    if let Some((x0, x1)) = open.replace((a, b)) {
                        runs.push(Run { y, x0, x1 });
                    }
                }
            };
            for run in spans.row(y) {
                if run.shares == FULL {
                    cured(run.x0, run.x0 + run.len, &mut l.runs);
                } else {
                    for (k, s) in spans.shares_of(run).iter().enumerate() {
                        if *s >= CURES {
                            let x = run.x0 + k as u32;
                            cured(x, x + 1, &mut l.runs);
                        }
                    }
                }
            }
            if let Some((x0, x1)) = open {
                l.runs.push(Run { y, x0, x1 });
            }
            l.rows.push(l.runs.len() as u32);
        }
        l.label();
        l.lost = l.lost_in(spans);
        l
    }

    /// The pixels cured in either, and their parts.
    pub fn union(&self, other: &LayerParts) -> LayerParts {
        let rect = self.rect.union(&other.rect);
        let mut l = LayerParts { rect, rows: Vec::with_capacity(rect.height() as usize + 1), ..LayerParts::default() };
        l.rows.push(0);
        for y in rect.y0..rect.y1 {
            let (a, b) = (self.row(y), other.row(y));
            let (mut i, mut j) = (0, 0);
            let mut open: Option<Run> = None;
            while i < a.len() || j < b.len() {
                let next = if j >= b.len() || (i < a.len() && a[i].x0 <= b[j].x0) {
                    i += 1;
                    a[i - 1]
                } else {
                    j += 1;
                    b[j - 1]
                };
                match &mut open {
                    // side by side or over each other: one run
                    Some(run) if next.x0 <= run.x1 => run.x1 = run.x1.max(next.x1),
                    _ => {
                        if let Some(run) = open.replace(next) {
                            l.runs.push(run);
                        }
                    }
                }
            }
            if let Some(run) = open {
                l.runs.push(run);
            }
            l.rows.push(l.runs.len() as u32);
        }
        l.label();
        l
    }

    /// The shares of `spans`' pixels this layer doesn't cure, summed (in pixels): what another
    /// section lights outside these parts.
    pub fn outside(&self, spans: &Spans) -> f64 {
        let (mut full, mut grey) = (0u64, 0u64);
        for y in spans.rect.y0..spans.rect.y1 {
            let cured = self.row(y);
            let mut c = 0;
            for run in spans.row(y) {
                let shares = spans.shares_of(run);
                let mut take = |from: u32, to: u32| {
                    if run.shares == FULL {
                        full += (to - from) as u64;
                    } else {
                        grey += shares[(from - run.x0) as usize..(to - run.x0) as usize].iter().map(|s| *s as u64).sum::<u64>();
                    }
                };
                let (mut x, end) = (run.x0, run.x0 + run.len);
                while x < end {
                    // past the cured runs that end at or before x
                    while c < cured.len() && cured[c].x1 <= x {
                        c += 1;
                    }
                    match cured.get(c) {
                        // cured: on to its run's end
                        Some(r) if r.x0 <= x => x = r.x1.min(end),
                        Some(r) => {
                            let to = r.x0.min(end);
                            take(x, to);
                            x = to;
                        }
                        None => {
                            take(x, end);
                            x = end;
                        }
                    }
                }
            }
        }
        full as f64 + grey as f64 / 255.0
    }

    /// Row `y`'s runs (none outside the rect).
    pub fn row(&self, y: u32) -> &[Run] {
        if y < self.rect.y0 || y >= self.rect.y1 {
            return &[];
        }
        let r = (y - self.rect.y0) as usize;
        &self.runs[self.rows[r] as usize..self.rows[r + 1] as usize]
    }

    /// Whether pixel `(x, y)` is cured.
    pub fn cured(&self, x: u32, y: u32) -> bool {
        let row = self.row(y);
        let k = row.partition_point(|r| r.x0 <= x);
        k > 0 && x < row[k - 1].x1
    }

    /// The part of pixel `(x, y)`, if it is cured.
    pub fn part_at(&self, x: u32, y: u32) -> Option<u32> {
        if y < self.rect.y0 || y >= self.rect.y1 {
            return None;
        }
        let first = self.rows[(y - self.rect.y0) as usize] as usize;
        let row = self.row(y);
        let k = row.partition_point(|r| r.x0 <= x);
        (k > 0 && x < row[k - 1].x1).then(|| self.part_of[first + k - 1])
    }

    /// The cured pixels in all.
    pub fn pixels(&self) -> u64 {
        self.parts.iter().map(|p| p.pixels).sum()
    }

    /// The rows and columns the runs lie in.
    pub fn rect(&self) -> PixelRect {
        self.rect
    }

    /// Whether a pixel is cured among `(x, y)` and its eight neighbours.
    fn cured_near(&self, x: u32, y: u32) -> bool {
        (y.saturating_sub(1)..=y + 1).any(|j| {
            let row = self.row(j);
            // the first run reaching column x - 1
            let k = row.partition_point(|r| r.x1 < x);
            k < row.len() && row[k].x0 <= x + 1
        })
    }

    /// The features of `spans` (this layer's own) lit under the cure share with no cured pixel
    /// beside them, that cover [`LOST_LEAST`] or more.
    fn lost_in(&self, spans: &Spans) -> Vec<Lost> {
        // runs of lost pixels by row: (y, x0, x1, covered)
        let mut runs: Vec<(u32, u32, u32, f64)> = Vec::new();
        for y in spans.rect.y0..spans.rect.y1 {
            for run in spans.row(y) {
                if run.shares == FULL {
                    continue;
                }
                for (k, &s) in spans.shares_of(run).iter().enumerate() {
                    let x = run.x0 + k as u32;
                    if s >= CURES || self.cured_near(x, y) {
                        continue;
                    }
                    let c = s as f64 / 255.0;
                    match runs.last_mut() {
                        Some(r) if r.0 == y && r.2 == x => {
                            r.2 = x + 1;
                            r.3 += c;
                        }
                        _ => runs.push((y, x, x + 1, c)),
                    }
                }
            }
        }
        if runs.is_empty() {
            return Vec::new();
        }
        // join runs of neighbouring rows that touch, across a corner too
        let mut up: Vec<u32> = (0..runs.len() as u32).collect();
        let mut prev = 0..0;
        let mut i = 0;
        while i < runs.len() {
            let y = runs[i].0;
            let start = i;
            while i < runs.len() && runs[i].0 == y {
                i += 1;
            }
            if prev.end > prev.start && runs[prev.start].0 + 1 == y {
                for a in prev.clone() {
                    for b in start..i {
                        if runs[a].1 <= runs[b].2 && runs[b].1 <= runs[a].2 {
                            let (x, z) = (root(&mut up, a as u32), root(&mut up, b as u32));
                            up[x.max(z) as usize] = x.min(z);
                        }
                    }
                }
            }
            prev = start..i;
        }
        let mut found: Vec<(u32, Lost)> = Vec::new();
        for k in 0..runs.len() {
            let r = root(&mut up, k as u32);
            let (y, x0, x1, c) = runs[k];
            let rect = PixelRect { x0, y0: y, x1, y1: y + 1 };
            match found.iter_mut().find(|f| f.0 == r) {
                Some(f) => {
                    f.1.rect = f.1.rect.union(&rect);
                    f.1.covered += c;
                }
                None => found.push((r, Lost { rect, covered: c })),
            }
        }
        found.into_iter().map(|f| f.1).filter(|l| l.covered >= LOST_LEAST).collect()
    }

    /// The layer's holes. Empty pixels touching across a side are one region; a region that
    /// reaches the rect's edge is open to the space around the layer, and every other is a hole.
    pub fn holes(&self) -> Holes {
        let rect = self.rect;
        if rect.is_empty() {
            return Holes::default();
        }
        // every empty run of every row, and whether it reaches the edge
        let (mut gaps, mut edge, mut rows) = (Vec::new(), Vec::new(), vec![0u32]);
        for y in rect.y0..rect.y1 {
            let end_row = y == rect.y0 || y + 1 == rect.y1;
            let mut x = rect.x0;
            for (i, r) in self.row(y).iter().enumerate() {
                if r.x0 > x {
                    gaps.push(Run { y, x0: x, x1: r.x0 });
                    edge.push(i == 0 || end_row);
                }
                x = r.x1;
            }
            if x < rect.x1 {
                gaps.push(Run { y, x0: x, x1: rect.x1 });
                edge.push(true);
            }
            rows.push(gaps.len() as u32);
        }
        let mut up: Vec<u32> = (0..gaps.len() as u32).collect();
        for r in 1..rect.height() as usize {
            let (prev, cur) = (rows[r - 1] as usize..rows[r] as usize, rows[r] as usize..rows[r + 1] as usize);
            let (mut a, mut b) = (prev.start, cur.start);
            while a < prev.end && b < cur.end {
                let (ga, gb) = (gaps[a], gaps[b]);
                if ga.x0 < gb.x1 && gb.x0 < ga.x1 {
                    let (x, z) = (root(&mut up, a as u32), root(&mut up, b as u32));
                    up[x.max(z) as usize] = x.min(z);
                }
                if ga.x1 <= gb.x1 {
                    a += 1;
                } else {
                    b += 1;
                }
            }
        }
        let mut open = vec![false; gaps.len()];
        for k in 0..gaps.len() {
            if edge[k] {
                let r = root(&mut up, k as u32);
                open[r as usize] = true;
            }
        }
        let mut h = Holes { rect, rows: vec![0], ..Holes::default() };
        let mut index = vec![u32::MAX; gaps.len()];
        let mut row = 0;
        for k in 0..gaps.len() {
            while rows[row + 1] as usize <= k {
                h.rows.push(h.runs.len() as u32);
                row += 1;
            }
            let r = root(&mut up, k as u32) as usize;
            if open[r] {
                continue;
            }
            if index[r] == u32::MAX {
                index[r] = h.pixels.len() as u32;
                h.pixels.push(0);
                h.rects.push(PixelRect::EMPTY);
            }
            let (id, g) = (index[r], gaps[k]);
            h.runs.push(g);
            h.of.push(id);
            h.pixels[id as usize] += (g.x1 - g.x0) as u64;
            h.rects[id as usize] = h.rects[id as usize].union(&PixelRect { x0: g.x0, y0: g.y, x1: g.x1, y1: g.y + 1 });
        }
        while h.rows.len() < rows.len() {
            h.rows.push(h.runs.len() as u32);
        }
        h
    }

    /// Gathers the runs into parts: runs of neighbouring rows that overlap are joined.
    fn label(&mut self) {
        let n = self.runs.len();
        let mut up: Vec<u32> = (0..n as u32).collect();
        for r in 1..self.rect.height() as usize {
            let (prev, cur) = (self.rows[r - 1] as usize..self.rows[r] as usize, self.rows[r] as usize..self.rows[r + 1] as usize);
            let (mut a, mut b) = (prev.start, cur.start);
            while a < prev.end && b < cur.end {
                let (ra, rb) = (self.runs[a], self.runs[b]);
                // over each other, or meeting at a corner
                if ra.x0 <= rb.x1 && rb.x0 <= ra.x1 {
                    let (x, y) = (root(&mut up, a as u32), root(&mut up, b as u32));
                    // the earlier run is the root, so parts number in the order they start
                    up[x.max(y) as usize] = x.min(y);
                }
                if ra.x1 <= rb.x1 {
                    a += 1;
                } else {
                    b += 1;
                }
            }
        }
        self.part_of = vec![0; n];
        let mut index = vec![u32::MAX; n];
        for k in 0..n {
            let r = root(&mut up, k as u32) as usize;
            if index[r] == u32::MAX {
                index[r] = self.parts.len() as u32;
                self.parts.push(Part { rect: PixelRect::EMPTY, pixels: 0, runs: Vec::new() });
            }
            let id = index[r];
            self.part_of[k] = id;
            let run = self.runs[k];
            let p = &mut self.parts[id as usize];
            p.rect = p.rect.union(&PixelRect { x0: run.x0, y0: run.y, x1: run.x1, y1: run.y + 1 });
            p.pixels += (run.x1 - run.x0) as u64;
            p.runs.push(run);
        }
    }
}

/// The cured parts of each of `stack`'s layers, in layer order.
pub fn cured(stack: &LayerStack) -> Vec<LayerParts> {
    stack.map(|l| LayerParts::of(&l.spans))
}

/// For each part of `above`, the parts of `below` it stands on (shares a cured pixel with), in
/// order.
pub fn links(below: &LayerParts, above: &LayerParts) -> Vec<Vec<u32>> {
    let mut on = vec![Vec::new(); above.parts.len()];
    let rows = below.rect.y0.max(above.rect.y0)..below.rect.y1.min(above.rect.y1);
    for y in rows {
        let (fb, fa) = (below.rows[(y - below.rect.y0) as usize] as usize, above.rows[(y - above.rect.y0) as usize] as usize);
        let (rb, ra) = (below.row(y), above.row(y));
        let (mut i, mut j) = (0, 0);
        while i < rb.len() && j < ra.len() {
            if rb[i].x0 < ra[j].x1 && ra[j].x0 < rb[i].x1 {
                on[above.part_of[fa + j] as usize].push(below.part_of[fb + i]);
            }
            if rb[i].x1 <= ra[j].x1 {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    for l in &mut on {
        l.sort_unstable();
        l.dedup();
    }
    on
}

/// The pixels of `part` not cured in `below`: the area it adds over the layer below, as runs.
pub fn over(part: &Part, below: &LayerParts) -> Vec<Run> {
    let mut out = Vec::new();
    for run in &part.runs {
        let mut x = run.x0;
        for b in below.row(run.y) {
            if b.x1 <= x {
                continue;
            }
            if b.x0 >= run.x1 {
                break;
            }
            if b.x0 > x {
                out.push(Run { y: run.y, x0: x, x1: b.x0 });
            }
            x = x.max(b.x1);
            if x >= run.x1 {
                break;
            }
        }
        if x < run.x1 {
            out.push(Run { y: run.y, x0: x, x1: run.x1 });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer from rows of text: `#` cured, `+` an edge pixel that cures, `-` one that doesn't.
    pub(crate) fn layer(rows: &[&str]) -> LayerParts {
        let (w, h) = (rows[0].len() as u32, rows.len() as u32);
        let rect = PixelRect { x0: 10, y0: 20, x1: 10 + w, y1: 20 + h };
        let cov: Vec<u8> = rows.iter().flat_map(|r| r.bytes().map(|c| match c { b'#' => 255, b'+' => 200, b'-' => 60, _ => 0 })).collect();
        LayerParts::of(&Spans::of(rect, &cov))
    }

    /// Pixels touching across a side or at a corner are one part; an edge pixel cures from half
    /// its share; the parts number in the order they start.
    #[test]
    fn parts_join_across_sides_and_corners() {
        let l = layer(&[
            "##...#",
            ".#+..#",
            "...#-.",
            "..##..",
        ]);
        assert_eq!(l.parts.len(), 2);
        assert_eq!((l.parts[0].pixels, l.parts[1].pixels), (7, 2));
        assert_eq!(l.parts[0].rect, PixelRect { x0: 10, y0: 20, x1: 14, y1: 24 });
        // the corner at (12, 21)-(13, 22) joins them; the edge pixel under half doesn't cure
        assert_eq!((l.part_at(12, 21), l.part_at(13, 22), l.part_at(14, 22)), (Some(0), Some(0), None));
        assert!(l.cured(15, 20) && !l.cured(14, 20) && !l.cured(12, 19));
        // a ring is one part, its hole none
        let ring = layer(&["####", "#..#", "####"]);
        assert_eq!((ring.parts.len(), ring.pixels()), (1, 10));
        // a U whose arms meet only at the bottom row is one part, found from both arms
        let u = layer(&["#..#", "#..#", "####"]);
        assert_eq!(u.parts.len(), 1);
    }

    /// A part stands on the parts of the layer below it shares a pixel with; one it only meets
    /// at a corner it doesn't stand on. What it adds is its pixels the layer below lacks.
    #[test]
    fn links_and_new_area() {
        let below = layer(&[
            "##..##",
            "##..##",
            "......",
        ]);
        let above = layer(&[
            "######",
            "......",
            "..#...",
        ]);
        let on = links(&below, &above);
        assert_eq!(on, vec![vec![0, 1], vec![]]);
        let new = over(&above.parts[0], &below);
        assert_eq!(new, vec![Run { y: 20, x0: 12, x1: 14 }]);
        assert_eq!(over(&above.parts[1], &below), above.parts[1].runs);
        // diagonal neighbours across layers share no pixel
        let a = layer(&["#.", ".."]);
        let b = layer(&["..", ".#"]);
        assert_eq!(links(&a, &b), vec![Vec::<u32>::new()]);
    }
}

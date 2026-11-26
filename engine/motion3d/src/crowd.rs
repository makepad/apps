//! Layouts for crowds and mazes.
//!
//! A crowd is `count` people placed in an arena, a block or a ring. Every
//! person keeps the same id, randoms and facing in every part's layout
//! (body, head, eyes, arms, signs), so a `CrowdLayout{variant: 1 variants:
//! 3}` or `{pick: 0.15}` or `{side: @left}` selects a stable subset and the
//! parts of one person line up. `order: @phase` staggers by the person's
//! own random, so every part of a person bobs in the same phase.
//!
//! A maze is a seeded perfect maze (recursive backtracker); its layout is
//! one instance per wall, the normal along the wall (with `align: true` a
//! box's depth runs along it).

use crate::layout::{instance_rand, InstanceBase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrowdShape {
    /// Tiered rows on an arc in front of a stage at the origin, facing it.
    Arena,
    /// Rows and columns, facing +z.
    Block,
    /// A full circle of rows around the origin, facing in.
    Ring,
}

impl CrowdShape {
    pub const NAMES: &'static [&'static str] = &["arena", "block", "ring"];
    pub fn by_name(s: &str) -> Option<Self> {
        Some(match s {
            "arena" => Self::Arena,
            "block" | "rows" => Self::Block,
            "ring" => Self::Ring,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    All,
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CrowdParams {
    pub count: u32,
    pub shape: CrowdShape,
    /// Arena / ring: the first row's radius; block: unused.
    pub radius: f32,
    /// Space between people (and rows).
    pub spacing: f32,
    /// Arena: height each row rises (tiers).
    pub tier: f32,
    /// Arena: the arc in degrees, centred on +z.
    pub arc: f32,
    pub seed: u64,
    /// Keep person `i` when `variant == i's variant` (of `variants`).
    pub variant: Option<(u32, u32)>,
    /// Keep a seeded fraction of the people (signs, flags).
    pub pick: Option<f32>,
    pub side: Side,
    /// Added to every position (a part's height on the body).
    pub offset: [f32; 3],
}

impl Default for CrowdParams {
    fn default() -> Self {
        Self { count: 200, shape: CrowdShape::Arena, radius: 3.0, spacing: 0.5, tier: 0.25, arc: 150.0, seed: 1, variant: None, pick: None, side: Side::All, offset: [0.0; 3] }
    }
}

/// Every person: (position on the ground, facing unit vector).
fn people(p: &CrowdParams) -> Vec<([f32; 3], [f32; 3])> {
    let n = p.count.clamp(1, 100_000) as usize;
    let s = p.spacing.max(0.05);
    let mut out = Vec::with_capacity(n);
    let jitter = |i: usize| {
        let r = instance_rand(p.seed ^ 0x51, i as u64);
        [(r[0] - 0.5) * s * 0.35, (r[1] - 0.5) * s * 0.35]
    };
    match p.shape {
        CrowdShape::Block => {
            let cols = ((n as f32).sqrt() * 1.6).ceil().max(1.0) as usize;
            let rows = n.div_ceil(cols);
            for i in 0..n {
                let (c, r) = (i % cols, i / cols);
                let j = jitter(i);
                let x = (c as f32 - (cols as f32 - 1.0) * 0.5) * s + j[0];
                let z = (r as f32 - (rows as f32 - 1.0) * 0.5) * s + j[1];
                out.push(([x, 0.0, z], [0.0, 0.0, 1.0]));
            }
        }
        CrowdShape::Arena | CrowdShape::Ring => {
            let arc = if p.shape == CrowdShape::Ring { 360.0f32 } else { p.arc.clamp(10.0, 360.0) }.to_radians();
            let mut row = 0usize;
            while out.len() < n {
                let radius = p.radius + row as f32 * s;
                let fit = ((radius * arc / s).floor() as usize).max(1);
                let take = fit.min(n - out.len());
                for k in 0..fit.min(take) {
                    let i = out.len();
                    let j = jitter(i);
                    // Spread over the arc, centred on +z (angle 0 = +z).
                    let a = if p.shape == CrowdShape::Ring { (k as f32 + (row % 2) as f32 * 0.5) / fit as f32 * arc } else { ((k as f32 + 0.5) / fit as f32 - 0.5) * arc };
                    let rr = radius + j[1];
                    let (sx, cz) = (a.sin(), a.cos());
                    let pos = [sx * rr + cz * j[0], if p.shape == CrowdShape::Arena { row as f32 * p.tier } else { 0.0 }, cz * rr - sx * j[0]];
                    out.push((pos, [-sx, 0.0, -cz]));
                }
                row += 1;
            }
        }
    }
    out
}

/// The crowd's bases after the filters, ids and randoms kept per person.
pub fn crowd_layout(p: &CrowdParams) -> Vec<InstanceBase> {
    let all = people(p);
    let n = all.len() as u32;
    let mut out = Vec::new();
    for (i, (pos, facing)) in all.into_iter().enumerate() {
        let rand = instance_rand(p.seed, i as u64);
        if let Some((v, of)) = p.variant {
            if of > 0 && ((rand[3] * of as f32) as u32).min(of - 1) != v {
                continue;
            }
        }
        if let Some(frac) = p.pick {
            if crate::layout::instance_rand(p.seed ^ 0x9e37, i as u64)[0] >= frac {
                continue;
            }
        }
        match p.side {
            Side::Left if pos[0] > 0.0 => continue,
            Side::Right if pos[0] <= 0.0 => continue,
            _ => {}
        }
        // The offset turns with the person (x right, z forward).
        let right = [facing[2], 0.0, -facing[0]];
        let o = p.offset;
        let position = [pos[0] + right[0] * o[0] + facing[0] * o[2], pos[1] + o[1], pos[2] + right[2] * o[0] + facing[2] * o[2]];
        out.push(InstanceBase { position, normal: facing, index: i as u32, count: n, u: if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 }, rand });
    }
    out
}

/// A seeded perfect maze of `cols × rows` cells `cell` apart, centred in
/// XZ: one base per wall at its middle, the normal along the wall.
pub fn maze_layout(cols: u32, rows: u32, cell: f32, seed: u64, open_ends: bool) -> Vec<InstanceBase> {
    let (w, h) = (cols.clamp(1, 200) as usize, rows.clamp(1, 200) as usize);
    // Walls: east of (x, y) and south of (x, y); true = standing.
    let mut east = vec![true; w * h];
    let mut south = vec![true; w * h];
    let mut seen = vec![false; w * h];
    let mut stack = vec![0usize];
    seen[0] = true;
    let mut k = 0u64;
    while let Some(&c) = stack.last() {
        let (x, y) = (c % w, c / w);
        let mut next = Vec::with_capacity(4);
        if x + 1 < w && !seen[c + 1] { next.push((c + 1, 0)) }
        if x > 0 && !seen[c - 1] { next.push((c - 1, 1)) }
        if y + 1 < h && !seen[c + w] { next.push((c + w, 2)) }
        if y > 0 && !seen[c - w] { next.push((c - w, 3)) }
        if next.is_empty() {
            stack.pop();
            continue;
        }
        k += 1;
        let pick = (instance_rand(seed, k)[0] * next.len() as f32) as usize;
        let (to, dir) = next[pick.min(next.len() - 1)];
        match dir {
            0 => east[c] = false,
            1 => east[c - 1] = false,
            2 => south[c] = false,
            _ => south[c - w] = false,
        }
        seen[to] = true;
        stack.push(to);
    }
    let ox = -(w as f32) * cell * 0.5;
    let oz = -(h as f32) * cell * 0.5;
    let mut walls: Vec<([f32; 3], [f32; 3])> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            let (x0, z0) = (ox + x as f32 * cell, oz + y as f32 * cell);
            // East walls run along z; south walls along x.
            if east[c] && !(open_ends && x + 1 == w && y + 1 == h) {
                walls.push(([x0 + cell, 0.0, z0 + cell * 0.5], [0.0, 0.0, 1.0]));
            }
            if south[c] {
                walls.push(([x0 + cell * 0.5, 0.0, z0 + cell], [1.0, 0.0, 0.0]));
            }
            if x == 0 && !(open_ends && y == 0) {
                walls.push(([x0, 0.0, z0 + cell * 0.5], [0.0, 0.0, 1.0]));
            }
            if y == 0 {
                walls.push(([x0 + cell * 0.5, 0.0, z0], [1.0, 0.0, 0.0]));
            }
        }
    }
    let n = walls.len() as u32;
    walls
        .into_iter()
        .enumerate()
        .map(|(i, (position, normal))| InstanceBase { position, normal, index: i as u32, count: n, u: if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 }, rand: instance_rand(seed, i as u64) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_of_a_person_line_up_and_subsets_are_stable() {
        let base = CrowdParams { count: 300, ..CrowdParams::default() };
        let all = crowd_layout(&base);
        assert_eq!(all.len(), 300);
        let mut seen = 0;
        for v in 0..3 {
            let part = crowd_layout(&CrowdParams { variant: Some((v, 3)), ..base.clone() });
            for b in &part {
                assert_eq!(b.rand, all[b.index as usize].rand);
                assert_eq!(b.position, all[b.index as usize].position);
            }
            seen += part.len();
        }
        assert_eq!(seen, 300, "the variants split the crowd");
        let signs = crowd_layout(&CrowdParams { pick: Some(0.2), ..base.clone() });
        assert!(signs.len() > 30 && signs.len() < 90, "{}", signs.len());
        let left = crowd_layout(&CrowdParams { side: Side::Left, ..base.clone() });
        assert!(left.iter().all(|b| b.position[0] <= 0.0));
    }

    #[test]
    fn a_maze_has_walls_on_every_cell_boundary_it_needs() {
        let walls = maze_layout(8, 6, 1.0, 3, false);
        // A perfect maze removes cells − 1 inner walls from the full grid.
        let full = 8 * 6 * 2 + 8 + 6;
        assert_eq!(walls.len(), full - (8 * 6 - 1));
        assert_eq!(walls, maze_layout(8, 6, 1.0, 3, false));
    }
}

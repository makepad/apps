//! Scaling boards for engine timing: ledctl's board repeated on a k x k grid.

use super::ledctl;
use fab_elec_design::Design;
use fab_elec_board::{Anchor, Area, Board, BoardNet, NetPair, TuneTarget, ARC_ERR};
use fab_elec_core::{Contour, ItemId, NetId, Path, PolySet, Seg, P};
use std::collections::HashMap;
use std::sync::Arc;

/// `ledctl` with its board's parts, vias, holes and tracks repeated on a `k` x `k` grid of board-sized
/// cells (`ledctl_grid(4)` is "ledctl4x4"). The outline becomes one rectangle around the grid, the
/// cutouts go and only the board-wide zones stay, so each zone pours over every cell. Copies keep their
/// nets, and their parts' paths end in `#ij`; the circuit is ledctl's own, so the copies have no
/// schematic: this is a board for connectivity, fill and DRC timing, not a consistent design.
/// Cost: O(k^2 x board items).
pub fn ledctl_grid(k: i32) -> Design {
    let mut design = ledctl();
    let src = design.units[0].board.as_deref().expect("ledctl has a board").clone();
    let mut b = src.clone();
    let bb = src.outline_bbox();
    let (w, h) = (bb.width() as i32, bb.height() as i32);
    for i in 0..k {
        for j in 0..k {
            if i == 0 && j == 0 {
                continue;
            }
            let by = P { x: i * w, y: j * h };
            copy_cell(&mut b, &src, by, &format!("#{i}{j}"), |net| net);
        }
    }
    b.zones.retain(|z| matches!(z.area, Area::Board));
    b.outline = vec![Contour::rect(bb.min, P { x: bb.min.x + k * w, y: bb.min.y + k * h }, 0)];
    b.cutouts.clear();
    design.units[0].board = Some(Arc::new(b));
    design
}

/// `ledctl` with its board's parts, vias, holes, tracks and tunings repeated on a `k` x `k` grid of
/// board-sized cells, each with independent nets (`ledctl_panel(4)` has 16 times ledctl's net count).
/// The first cell keeps its nets and paths; later cells suffix net names and part paths with `#i.j`.
/// Differential pairs are copied per cell. Each board-wide zone becomes a cell rectangle on its
/// cell's net; other zones and cutouts go, and one rectangle encloses the grid. The circuit stays
/// ledctl's own, so the copies have no schematic: this is a router scale case with many independent
/// nets, not a consistent design or `ledctl_grid`'s shared-net stress case.
/// Cost: O(k^2 x (board items + nets)).
pub fn ledctl_panel(k: i32) -> Design {
    let mut design = ledctl();
    let src = design.units[0].board.as_deref().expect("ledctl has a board").clone();
    let mut b = src.clone();
    let bb = src.outline_bbox();
    let (w, h) = (bb.width() as i32, bb.height() as i32);
    b.zones.clear();
    for i in 0..k {
        for j in 0..k {
            let first = i == 0 && j == 0;
            let by = P { x: i * w, y: j * h };
            let suffix = if first { String::new() } else { format!("#{i}.{j}") };
            let mut nets = Vec::with_capacity(src.nets.len());
            for (index, n) in src.nets.iter().enumerate() {
                if first {
                    nets.push(NetId(index as u32));
                } else {
                    nets.push(NetId(b.nets.len() as u32));
                    b.nets.push(BoardNet { name: format!("{}{suffix}", n.name), class: n.class });
                }
            }
            let net = |n: NetId| nets[n.0 as usize];
            if !first {
                let map = copy_cell(&mut b, &src, by, &suffix, &net);
                for t in &src.tunings {
                    let mut q = t.clone();
                    q.id = b.alloc_id();
                    q.net = net(q.net);
                    q.pair = q.pair.map(&net);
                    for id in q.host.iter_mut().chain(&mut q.copper) {
                        *id = map[&*id];
                    }
                    if let TuneTarget::Skew { to, .. } = &mut q.target {
                        *to = net(*to);
                    }
                    b.tunings.push(q);
                }
                b.pairs.extend(src.pairs.iter().map(|p| NetPair {
                    p: net(p.p), n: net(p.n), gap: p.gap,
                }));
            }
            let cell = PolySet::from_ring_nonzero(Contour::rect(bb.min.add(by), bb.max.add(by), 0).to_ring(ARC_ERR));
            for z in &src.zones {
                if !matches!(z.area, Area::Board) {
                    continue;
                }
                let mut q = z.clone();
                q.id = b.alloc_id();
                q.net = q.net.map(&net);
                q.name = format!("{}{suffix}", z.name);
                q.area = Area::Poly(cell.clone());
                b.zones.push(q);
            }
        }
    }
    b.outline = vec![Contour::rect(bb.min, P { x: bb.min.x + k * w, y: bb.min.y + k * h }, 0)];
    b.cutouts.clear();
    b.reindex();
    design.units[0].board = Some(Arc::new(b));
    design
}

fn copy_cell(
    b: &mut Board,
    src: &Board,
    by: P,
    suffix: &str,
    net: impl Fn(NetId) -> NetId,
) -> HashMap<ItemId, ItemId> {
    let mut map: HashMap<ItemId, ItemId> = HashMap::new();
    for p in &src.placed {
        let id = b.alloc_id();
        map.insert(p.id, id);
        let mut q = p.clone();
        q.id = id;
        q.at = q.at.add(by);
        q.path = format!("{}{suffix}", p.path);
        for n in q.pad_nets.iter_mut().flatten() {
            *n = net(*n);
        }
        b.placed.push(q);
    }
    for v in &src.vias {
        let id = b.alloc_id();
        map.insert(v.id, id);
        let mut q = v.clone();
        q.id = id;
        q.net = net(q.net);
        q.at = q.at.add(by);
        b.vias.push(q);
    }
    for t in &src.holes {
        let id = b.alloc_id();
        map.insert(t.id, id);
        let mut q = t.clone();
        q.id = id;
        q.net = q.net.map(&net);
        q.at = q.at.add(by);
        b.holes.push(q);
    }
    for t in &src.tracks {
        let id = b.alloc_id();
        map.insert(t.id, id);
    }
    for t in &src.tracks {
        let mut q = t.clone();
        q.id = map[&t.id];
        q.net = net(q.net);
        q.path = shift(&t.path, by);
        for e in &mut q.ends {
            *e = match *e {
                Anchor::Free => Anchor::Free,
                Anchor::Pad { placed, pad } => Anchor::Pad { placed: map[&placed], pad },
                Anchor::Via(v) => Anchor::Via(map[&v]),
                Anchor::Track { id, at } => Anchor::Track { id: map[&id], at },
                Anchor::Hole(v) => Anchor::Hole(map[&v]),
            };
        }
        b.tracks.push(q);
    }
    map
}

fn shift(p: &Path, by: P) -> Path {
    Path {
        start: p.start.add(by),
        segs: p
            .segs
            .iter()
            .map(|s| match s {
                Seg::Line(b) => Seg::Line(b.add(by)),
                Seg::Arc { mid, end } => Seg::Arc { mid: mid.add(by), end: end.add(by) },
            })
            .collect(),
    }
}

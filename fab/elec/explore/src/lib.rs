//! The circuit explorer's layout (ARCHITECTURE 5.4, E2.2): the sheet draws itself.
//!
//! x is signal flow, y is DC potential, a block is a unit, small parts belong to pins. Everything
//! here is a pure function of its input: the same graph and hints give the same sheet on every
//! machine at any thread count, with no hidden history (the mental-map stability comes from the
//! structure: blocks lay out alone, molecules are rigid and hang on pins, source order breaks
//! every tie).
//!
//! - `graph`: the layout's own input (parts with ports and roles, nets as hyper-edges, the block
//!   tree), which `layout` adapts a design to.
//! - `layered`: layered placement of anchors with ports (cycle breaking, layers, crossing reduction
//!   with port order, coordinates), over its own small graph.
//! - `labels`: label placement with no overlaps (candidate spots in priority order, then slides).
//! - `legible`: part references and optional values re-placed for a screen text floor.
//! - `channels`: orthogonal wires between columns: one track per net in each channel, ordered to
//!   avoid crossings.
//! - `adapt`: one unit's circuit as the graph (symbol geometry, roles, nets, blocks, hints).
//! - `molecules`: small parts that belong to a pin or to each other (decoupling, pulls, chains).
//! - `reveal`: what is related to a part (trace queries) and where each related part sits round it.
//! - `sheet`: the pipeline: blocks bottom up, molecules on pins, anchors in layers, wires in
//!   channels, rails as tags, labels clear of everything.
//! - `wiretree`: each net's routed wires as a tree of segments with stable keys, for current flow.
//! - `edit`: semantic edit targets shared by sheet gestures and AI actions.
//! - `plan`: pure wiring plans and immediate chip text for semantic edit targets.
//! - `plan_drop`: pure Library drop plans, direction-selected idioms and Tab alternatives.

pub mod adapt;
pub mod annotate;
pub mod channels;
pub mod edit;
pub mod graph;
pub mod groups;
pub mod labels;
pub mod layered;
pub mod legible;
pub mod molecules;
pub mod plan;
pub mod plan_drop;
pub mod plan_paste;
pub mod reveal;
pub mod rows;
pub mod sheet;
pub mod sheet_router;
pub mod text;
pub mod wiretree;

pub use graph::LayoutGraph;
use graph::Pt;
pub use sheet::{block_origin, to_local, to_world, Bx, Mover, Opts, SheetLayout};

use fab_elec_circuit::{Circuit, SheetHint, SheetHints};
use fab_elec_core::SchPt;
use fab_elec_design::Design;
use std::collections::HashMap;
use std::sync::Arc;

/// The sheet of one unit of a design (`unit` indexes `design.units`), with `hints` layered over
/// the circuit's own. Pure. A drag's preview uses `drag_layout`.
pub fn layout(design: &Design, unit: usize, hints: &SheetHints, opts: &Opts) -> SheetLayout {
    let Some(u) = design.units.get(unit) else { return SheetLayout::default() };
    sheet::layout_graph(&adapt::graph_of(&u.circuit, hints), opts)
}

/// A drag's sheet: the layout as it will be once the drop is committed, and the hints the drop
/// writes besides the dragged part's own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DragLayout {
    pub sheet: SheetLayout,
    pub drop: Vec<(String, SheetHint)>,
}

/// The sheet of `unit` while the part or block at `path` is dragged to `hint`: it leads, so pinned
/// parts and blocks it runs into give way (`layered::LGraph::lead`). The parts that gave way, and pins drawn off
/// their hints, are the drop's hints at their drawn places; the sheet returned is the layout of
/// exactly the hints the drop commits, so the committed layout is this preview, position for
/// position. One layout, two when something gave way. Pure.
pub fn drag_layout(design: &Design, unit: usize, path: &str, hint: &SheetHint, opts: &Opts) -> DragLayout {
    let hints = SheetHints { parts: vec![(path.to_owned(), hint.clone())], ..SheetHints::default() };
    drag_group_layout(design, unit, path, &hints, opts)
}

/// Preview all dragged heads at their hints with `path` leading. A second layout incorporates
/// the hints of parts that gave way so the preview matches the committed sheet.
pub fn drag_group_layout(design: &Design, unit: usize, path: &str, hints: &SheetHints, opts: &Opts) -> DragLayout {
    let Some(u) = design.units.get(unit) else { return DragLayout::default() };
    let (led, drop) = led(&u.circuit, path, hints, opts);
    if drop.is_empty() {
        return DragLayout { sheet: led, drop };
    }
    let mut hints = hints.clone();
    hints.parts.extend(drop.iter().cloned());
    DragLayout { sheet: sheet::layout_graph(&adapt::graph_of(&u.circuit, &hints), opts), drop }
}

/// The hints of parts and blocks that gave way to the dragged heads in `hints`.
/// One layout with `path` leading, even when the release precedes the last preview's layout.
pub fn drag_group_drop(design: &Design, unit: usize, path: &str, hints: &SheetHints, opts: &Opts) -> Vec<(String, SheetHint)> {
    design.units.get(unit).map_or_else(Vec::new, |u| led(&u.circuit, path, hints, opts).1)
}

/// The drop hints for a single dragged part or block, from one layout.
pub fn drag_drop(design: &Design, unit: usize, path: &str, hint: &SheetHint, opts: &Opts) -> Vec<(String, SheetHint)> {
    let hints = SheetHints { parts: vec![(path.to_owned(), hint.clone())], ..SheetHints::default() };
    drag_group_drop(design, unit, path, &hints, opts)
}

/// The layout led by the part or block at `path`, and the drop hints it implies.
fn led(circuit: &Circuit, path: &str, hints: &SheetHints, opts: &Opts) -> (SheetLayout, Vec<(String, SheetHint)>) {
    let led = sheet::layout_graph(&adapt::graph_led(circuit, hints, Some(path)), opts);
    let drop = drop_hints(&led, circuit, hints);
    (led, drop)
}

/// The parts and blocks outside the drag that `sheet` did not draw where the circuit's hints put
/// them (in a pinned column that gave way, or a pin moved off its hint), with hints at their drawn
/// places (and orientations, for parts). O((parts + blocks) x hints).
fn drop_hints(sheet: &SheetLayout, circuit: &Circuit, hints: &SheetHints) -> Vec<(String, SheetHint)> {
    let leads: std::collections::HashSet<_> = hints.parts.iter().map(|(path, _)| path.as_str()).collect();
    let hint_of = |path: &str| circuit.hints.parts.iter().rev().find(|(p, _)| p == path).map(|(_, hint)| hint);
    let moved = |hint: Option<&SheetHint>, drawn: Pt, yielded: bool| match hint.and_then(|hint| hint.at) {
        Some(at) => drawn != [at.x, at.y],
        None => yielded,
    };
    let parts = circuit.comps.iter().enumerate().filter(|(_, comp)| !leads.contains(comp.path.as_str())).filter_map(|(n, comp)| {
        // A molecule member or a later chain member takes no hint of its own.
        if sheet.movers.get(n) != Some(&Mover::Itself) { return None; }
        let part = sheet.parts.get(n).filter(|part| part.placed)?;
        let drawn = to_local(sheet, part.block, part.at)?;
        let hint = hint_of(&comp.path);
        moved(hint, drawn, sheet.yielded.contains(&(n as u32))).then(|| (comp.path.clone(), SheetHint {
            at: Some(SchPt::new(drawn[0], drawn[1])), turn: part.turn, mirror: part.mirror, ..hint.cloned().unwrap_or_default()
        }))
    });
    let blocks = circuit.tree.iter().enumerate().filter(|(_, blk)| !leads.contains(blk.path.as_str())).filter_map(|(k, blk)| {
        // The root and an array's members take no hint of their own.
        if sheet.block_movable.get(k) != Some(&true) { return None; }
        let drawn = to_local(sheet, blk.parent?.0, block_origin(sheet, k as u32)?)?;
        let hint = hint_of(&blk.path);
        moved(hint, drawn, sheet.yielded_blocks.contains(&(k as u32))).then(|| (blk.path.clone(), SheetHint {
            at: Some(SchPt::new(drawn[0], drawn[1])), ..hint.cloned().unwrap_or_default()
        }))
    });
    parts.chain(blocks).collect()
}

/// The sheet a commit keeps (R2): the drawing on screen before an edit, with its circuit.
pub struct KeepBase {
    pub sheet: SheetLayout,
    pub circuit: Arc<Circuit>,
}

/// Hints that keep the drawn sheet through an edit (a paste, a drop): every level (block) in
/// which `after` draws one of `base`'s own items elsewhere, or in another orientation, gets
/// hints at the before places for all of its own items (parts that take hints, and child blocks
/// as blocks). The comparison is block-local, so a translated drawing changes nothing. Items the
/// edit leads (`exempt`, the new parts) and items gone after the edit take none. O(parts + blocks).
pub fn keep_drawn(base: &KeepBase, after: &SheetLayout, circuit: &Circuit, exempt: &[&str]) -> Vec<(String, SheetHint)> {
    let before = &base.sheet;
    let part_place = |sheet: &SheetLayout, n: usize| {
        if sheet.movers.get(n) != Some(&Mover::Itself) { return None; }
        let part = sheet.parts.get(n).filter(|part| part.placed)?;
        Some((part.block, to_local(sheet, part.block, part.at)?, part.turn, part.mirror))
    };
    let block_place = |sheet: &SheetLayout, c: &Circuit, k: usize| {
        if sheet.block_movable.get(k) != Some(&true) { return None; }
        let parent = c.tree.get(k)?.parent?.0;
        Some((parent, to_local(sheet, parent, block_origin(sheet, k as u32)?)?))
    };
    // Where any child block is drawn in its parent, an array's members included: they take no
    // hint, but an array that moves disturbs its level all the same.
    let block_drawn = |sheet: &SheetLayout, c: &Circuit, k: usize| {
        let parent = c.tree.get(k)?.parent?.0;
        sheet.frames.get(k).filter(|f| f.placed)?;
        to_local(sheet, parent, block_origin(sheet, k as u32)?).map(|at| (parent, at))
    };
    let after_part: HashMap<&str, usize> = circuit.comps.iter().enumerate().map(|(n, c)| (c.path.as_str(), n)).collect();
    let after_block: HashMap<&str, usize> = circuit.tree.iter().enumerate().map(|(k, b)| (b.path.as_str(), k)).collect();
    let mut disturbed = vec![false; base.circuit.tree.len()];
    for (n, comp) in base.circuit.comps.iter().enumerate() {
        let Some((level, at, turn, mirror)) = part_place(before, n) else { continue };
        let now = after_part.get(comp.path.as_str()).and_then(|&m| part_place(after, m));
        if now.is_some_and(|(_, a, t, m)| (a, t, m) != (at, turn, mirror)) { disturbed[level as usize] = true; }
    }
    for (k, blk) in base.circuit.tree.iter().enumerate() {
        let Some((level, at)) = block_drawn(before, &base.circuit, k) else { continue };
        let now = after_block.get(blk.path.as_str()).and_then(|&j| block_drawn(after, circuit, j));
        if now.is_some_and(|(_, a)| a != at) { disturbed[level as usize] = true; }
    }
    if !disturbed.contains(&true) { return Vec::new(); }
    let hint_of = |path: &str| circuit.hints.parts.iter().rev().find(|(p, _)| p == path).map(|(_, hint)| hint.clone());
    let keep = |path: &str| !exempt.contains(&path);
    let parts = base.circuit.comps.iter().enumerate().filter_map(|(n, comp)| {
        let (level, [x, y], turn, mirror) = part_place(before, n)?;
        (disturbed[level as usize] && keep(&comp.path) && after_part.contains_key(comp.path.as_str())).then(|| (comp.path.clone(),
            SheetHint { at: Some(SchPt::new(x, y)), turn, mirror, ..hint_of(&comp.path).unwrap_or_default() }))
    });
    let blocks = base.circuit.tree.iter().enumerate().filter_map(|(k, blk)| {
        let (level, [x, y]) = block_place(before, &base.circuit, k)?;
        (disturbed[level as usize] && keep(&blk.path) && after_block.contains_key(blk.path.as_str())).then(|| (blk.path.clone(),
            SheetHint { at: Some(SchPt::new(x, y)), ..hint_of(&blk.path).unwrap_or_default() }))
    });
    parts.chain(blocks).collect()
}

/// The sheet of `unit` after an edit that adds the parts at `new` (a paste), kept as `base` drew
/// it (R2 and R3, fable-answers A1 and A4), with the hints a commit writes. Layout 1 is the edited
/// design as it is. When it draws any of `base`'s items elsewhere, layout 2 pins each disturbed
/// level at its drawn places (`keep_drawn`). When layout 2 still differs from "the sheet before,
/// plus each new part at its hint", the new parts go `free` (every new part not drawn at its hint;
/// the whole group when others still move), each at the nearest clear place in its block's drawn
/// content (`clear_place`), and layout 3 draws that. A free part enters no column and grows no
/// frame into a sibling, so it cannot push a pin: three layouts at most. The hints are the keep
/// hints and the new parts' free hints. Err: the note for a paste with no room, when a block and
/// every way it could grow are full. Pure.
pub fn kept_layout(design: &Design, unit: usize, base: &KeepBase, new: &[&str], opts: &Opts) -> Result<(SheetLayout, Vec<(String, SheetHint)>), String> {
    let Some(u) = design.units.get(unit) else { return Ok((SheetLayout::default(), Vec::new())) };
    let c = &u.circuit;
    let lay = |extra: &[(String, SheetHint)]| {
        let hints = SheetHints { parts: extra.to_vec(), ..SheetHints::default() };
        let g = adapt::graph_of(c, &hints);
        (sheet::layout_graph(&g, opts), g)
    };
    let stored = |path: &str| c.hints.parts.iter().rev().find(|(p, _)| p == path).map(|(_, hint)| hint.clone());
    let index = |path: &str| c.comps.iter().position(|comp| comp.path == path);
    // A new part is where it was put: at its stored hint, or anywhere when it has none.
    let at_hint = |s: &SheetLayout, path: &str| {
        let (Some(n), Some(at)) = (index(path), stored(path).and_then(|hint| hint.at)) else { return true };
        s.parts.get(n).filter(|part| part.placed).and_then(|part| to_local(s, part.block, part.at)) == Some([at.x, at.y])
    };
    let first = lay(&[]);
    let keep = keep_drawn(base, &first.0, c, new);
    let (second, g) = if keep.is_empty() { first } else { lay(&keep) };
    let others_move = !keep_drawn(base, &second, c, new).is_empty();
    let off: Vec<&str> = new.iter().copied().filter(|path| others_move || !at_hint(&second, path)).collect();
    if off.is_empty() { return Ok((second, keep)); }
    let mut hints = keep;
    let mut taken: Vec<Bx> = Vec::new();
    for path in &off {
        let (Some(n), Some(hint)) = (index(path), stored(path)) else { continue };
        let Some(at) = hint.at else { continue };
        let part = &second.parts[n];
        // The part's ink at its origin: its body, and its own text where layout 2 put it.
        let mut foot = [part.body[0] - part.at[0], part.body[1] - part.at[1], part.body[2] - part.at[0], part.body[3] - part.at[1]];
        let text = second.labels.iter().filter(|l| l.owner == sheet::Owner::Part(n as u32)).map(|l| l.bx);
        for bx in text.chain(sheet::free_flags(&g, n as u32, part)) {
            foot = [foot[0].min(bx[0].floor() as i32 - part.at[0]), foot[1].min(bx[1].floor() as i32 - part.at[1]),
                foot[2].max(bx[2].ceil() as i32 - part.at[0]), foot[3].max(bx[3].ceil() as i32 - part.at[1])];
        }
        let block = c.comps[n].block.0;
        let Some((place, ink)) = clear_place(&base.sheet, &base.circuit, block, foot, [at.x, at.y], &taken, opts) else {
            let name = c.tree.get(block as usize).map_or("", |b| b.path.as_str());
            let count = off.iter().filter(|p| index(p).is_some_and(|m| c.comps[m].block.0 == block)).count();
            return Err(format!("No room in {name} for {count} part{}: make room or Fit", if count == 1 { "" } else { "s" }));
        };
        taken.push(ink);
        hints.push((path.to_string(), SheetHint { at: Some(SchPt::new(place[0], place[1])), free: true, ..hint }));
    }
    Ok((lay(&hints).0, hints))
}

/// A grid of occupied unit cells over a span of the sheet, with a summed-area table: whether a box
/// touches anything is O(1). Building it is O(span cells + marked area).
struct Occupied {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    cells: Vec<bool>,
    sums: Vec<u32>,
}

impl Occupied {
    fn new(span: Bx) -> Self {
        let (w, h) = ((span[2] - span[0]).max(1) as usize, (span[3] - span[1]).max(1) as usize);
        Self { x0: span[0], y0: span[1], w, h, cells: vec![false; w * h], sums: Vec::new() }
    }
    /// Mark the cells a box's interior covers; a box of no width or height covers one cell row.
    fn mark(&mut self, b: [f64; 4]) {
        let cx = |v: f64| (v - f64::from(self.x0)).clamp(0.0, self.w as f64);
        let cy = |v: f64| (v - f64::from(self.y0)).clamp(0.0, self.h as f64);
        let (i0, i1) = (cx(b[0]).floor() as usize, (cx(b[2]).ceil() as usize).max(cx(b[0]).floor() as usize + 1).min(self.w));
        let (j0, j1) = (cy(b[1]).floor() as usize, (cy(b[3]).ceil() as usize).max(cy(b[1]).floor() as usize + 1).min(self.h));
        for j in j0..j1 { for i in i0..i1 { self.cells[j * self.w + i] = true; } }
    }
    fn seal(&mut self) {
        let (w, h) = (self.w, self.h);
        self.sums = vec![0; (w + 1) * (h + 1)];
        for j in 0..h {
            for i in 0..w {
                self.sums[(j + 1) * (w + 1) + i + 1] = u32::from(self.cells[j * w + i]) + self.sums[j * (w + 1) + i + 1]
                    + self.sums[(j + 1) * (w + 1) + i] - self.sums[j * (w + 1) + i];
            }
        }
    }
    /// Whether any marked cell lies inside the integer box (cells min..max). Outside the span is clear.
    fn any(&self, b: Bx) -> bool {
        let i0 = (b[0] - self.x0).clamp(0, self.w as i32) as usize;
        let i1 = (b[2] - self.x0).clamp(0, self.w as i32) as usize;
        let j0 = (b[1] - self.y0).clamp(0, self.h as i32) as usize;
        let j1 = (b[3] - self.y0).clamp(0, self.h as i32) as usize;
        if i0 >= i1 || j0 >= j1 { return false; }
        let s = |i: usize, j: usize| self.sums[j * (self.w + 1) + i];
        s(i1, j1) + s(i0, j0) > s(i0, j1) + s(i1, j0)
    }
}

/// The nearest block-local place to `want` in `block` of `sheet` for a free part whose ink is
/// `foot` (a world box relative to the part's origin), with the world box it then takes (with a
/// one-step margin). The place touches no part body, text, tag, wire, foreign frame or `taken`
/// box, and it lies in the block's drawn content (inside the frame's pad and title), so the frame
/// keeps its size: every place there comes before any outside it. A full block may grow only toward
/// clear room: the grown frame takes no ink of
/// the parent level and comes no nearer a sibling frame than the layered gaps, and it stays in its
/// parent's content (the root grows freely). None: no place anywhere on the sheet. Rings of growing
/// radius, nearest first, each candidate O(1); O(span cells + items) to build.
pub fn clear_place(sheet: &SheetLayout, circuit: &Circuit, block: u32, foot: Bx, want: Pt, taken: &[Bx], o: &Opts) -> Option<(Pt, Bx)> {
    let origin = block_origin(sheet, block)?;
    let frame = |k: u32| sheet.frames.get(k as usize).filter(|f| f.placed).map(|f| f.rect);
    let own = if block == 0 { None } else { frame(block) };
    let parent = circuit.tree.get(block as usize).and_then(|b| b.parent).map(|p| p.0);
    let band = |k: u32| sheet.frames.get(k as usize).map_or(o.title, |f| f.title);
    let content = |r: Bx, k: u32| [r[0] + o.pad, r[1] + o.pad, r[2] - o.pad, r[3] - o.pad - band(k)];
    let within = |a: Bx, b: Bx| a[0] >= b[0] && a[1] >= b[1] && a[2] <= b[2] && a[3] <= b[3];
    let meets = |a: Bx, b: Bx| a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
    let ink_at = |local: Pt| {
        let at = [origin[0] + local[0], origin[1] - local[1]];
        [at[0] + foot[0] - 1, at[1] + foot[1] - 1, at[0] + foot[2] + 1, at[1] + foot[3] + 1]
    };
    let mut ink: Vec<[f64; 4]> = sheet.parts.iter().filter(|p| p.placed).map(|p| p.body.map(f64::from)).collect();
    ink.extend(sheet.labels.iter().map(|l| l.bx));
    ink.extend(sheet.tags.iter().filter(|t| !t.text.starts_with('\u{1}')).map(sheet::tag_item_box));
    for run in &sheet.wires {
        for w in run.pts.windows(2) {
            let (a, b) = (w[0].map(f64::from), w[1].map(f64::from));
            ink.push([a[0].min(b[0]) - 0.5, a[1].min(b[1]) - 0.5, a[0].max(b[0]) + 0.5, a[1].max(b[1]) + 0.5]);
        }
    }
    let border = (foot[2] - foot[0]).max(foot[3] - foot[1]) + 2 * (o.layer_gap + o.pad + o.title) + 4;
    let span = [sheet.bounds[0] - border, sheet.bounds[1] - border, sheet.bounds[2] + border, sheet.bounds[3] + border];
    let others = |k: u32| k != block && !is_ancestor(circuit, k, block);
    let mut here = Occupied::new(span);
    for b in &ink { here.mark(*b); }
    for k in (1..sheet.frames.len() as u32).filter(|&k| others(k)) {
        if let Some(r) = frame(k) { here.mark(r.map(f64::from)); }
    }
    here.seal();
    let mut grow = Occupied::new(span);
    if let Some(r) = own {
        let inside = |b: [f64; 4]| b[0] >= f64::from(r[0]) && b[1] >= f64::from(r[1]) && b[2] <= f64::from(r[2]) && b[3] <= f64::from(r[3]);
        for b in ink.iter().filter(|b| !inside(**b)) { grow.mark(*b); }
        let (gx, gy) = (o.layer_gap, o.margin + 2);
        for k in (1..sheet.frames.len() as u32).filter(|&k| others(k) && !is_ancestor(circuit, block, k)) {
            if let Some(f) = frame(k) { grow.mark([f[0] - gx, f[1] - gy, f[2] + gx, f[3] + gy].map(f64::from)); }
        }
    }
    grow.seal();
    let fits = |f: Bx, grows: bool| {
        if here.any(f) || taken.iter().any(|t| meets(*t, f)) { return false; }
        let Some(r) = own else { return true };
        if within(f, content(r, block)) { return true; }
        if !grows { return false; }
        let g = [r[0].min(f[0] - o.pad), r[1].min(f[1] - o.pad), r[2].max(f[2] + o.pad), r[3].max(f[3] + o.pad + band(block))];
        let strips = [[g[0], g[1], r[0], g[3]], [r[2], g[1], g[2], g[3]], [r[0], g[1], r[2], r[1]], [r[0], r[3], r[2], g[3]]];
        if strips.iter().any(|s| s[0] < s[2] && s[1] < s[3] && grow.any(*s)) { return false; }
        match parent { Some(p) if p != 0 => frame(p).is_some_and(|pr| within(g, content(pr, p))), _ => true }
    };
    // The block's own content first, all of it; growing only when it is full.
    let w = [origin[0] + want[0], origin[1] - want[1]];
    let far = |b: Bx| [b[0], b[2]].iter().map(|x| (x - w[0]).abs()).max().unwrap_or(0).max([b[1], b[3]].iter().map(|y| (y - w[1]).abs()).max().unwrap_or(0));
    let passes = match own { Some(r) => vec![(far(r), false), (far(span), true)], None => vec![(far(span), true)] };
    for (reach, grows) in passes {
        for r in 0..=reach {
            let mut ring: Vec<Pt> = (-r..=r).flat_map(|dx| (-r..=r).map(move |dy| [dx, dy])).filter(|d| d[0].abs().max(d[1].abs()) == r).collect();
            ring.sort_by_key(|d| (d[0] * d[0] + d[1] * d[1], d[1].abs(), d[0], d[1]));
            if let Some(d) = ring.into_iter().find(|d| fits(ink_at([want[0] + d[0], want[1] + d[1]]), grows)) {
                let place = [want[0] + d[0], want[1] + d[1]];
                return Some((place, ink_at(place)));
            }
        }
    }
    None
}

/// Whether block `a` is an ancestor of block `b`.
fn is_ancestor(circuit: &Circuit, a: u32, mut b: u32) -> bool {
    while let Some(p) = circuit.tree.get(b as usize).and_then(|blk| blk.parent) {
        if p.0 == a { return true; }
        b = p.0;
    }
    false
}

#[cfg(test)]
mod tests;

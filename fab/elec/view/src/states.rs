//! Shared item-state rows: board item ids and stable schematic SheetRows.

use crate::live::LiveRows;
use fab_elec_board::Board;
use fab_elec_circuit::{Circuit, Violation};
use fab_elec_core::{BoardId, CompId, FeatureRef, ItemId, Sel, Severity};
use fab_elec_explore::rows::RowIndex;
use makepad_widgets_core::{Cx, Texture};
use std::collections::HashMap;

/// Whose item states: a board's (rows = ItemId.0), or a sheet's (follows ESCH's SheetRows).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum StatesOf {
    Board(BoardId),
    Schematic(BoardId),
}

/// A committed circuit's stable sheet rows and the texel count, including dead rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetIndex {
    pub rows: RowIndex,
    pub count: u32,
}

/// What the views share besides the findings: the selection and the emphasis (the focus).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewState {
    pub selected: Vec<Sel>,
    pub emphasis: Vec<(Sel, f32)>,
}

/// The rows `sel` names on board `unit`, appended to `out` (item-state.md, "The row index").
/// Cost: O(1) expected, except a net scans O(board items + placement pads).
pub fn board_rows(unit: BoardId, board: &Board, parts: &HashMap<CompId, ItemId>, sel: &Sel, out: &mut Vec<u32>) {
    if sel.unit() != Some(unit) {
        return;
    }
    match sel {
        Sel::Item(_, item) | Sel::Feature(FeatureRef::Hole(_, item)) => out.push(item.0),
        Sel::Comp(c) | Sel::Feature(FeatureRef::Part(c) | FeatureRef::Pad(c, _) | FeatureRef::Body(c) | FeatureRef::Port(c, _)) => {
            if let Some(item) = parts.get(&c.comp) {
                out.push(item.0);
            }
        }
        Sel::Pin(p) => {
            if let Some(item) = parts.get(&p.comp) {
                out.push(item.0);
            }
        }
        Sel::Net(n) => {
            // SetNets mirrors circuit net order, so NetRef and board items share NetId.
            out.extend(board.tracks.iter().filter(|item| item.net == n.net).map(|item| item.id.0));
            out.extend(board.vias.iter().filter(|item| item.net == n.net).map(|item| item.id.0));
            out.extend(board.tunings.iter().filter(|item| item.net == n.net).map(|item| item.id.0));
            out.extend(board.zones.iter().filter(|item| item.net == Some(n.net)).map(|item| item.id.0));
            out.extend(board.holes.iter().filter(|item| item.net == Some(n.net)).map(|item| item.id.0));
            out.extend(board.placed.iter().filter(|item| item.pad_nets.contains(&Some(n.net))).map(|item| item.id.0));
        }
        _ => {}
    }
}

/// The comp -> placement map `board_rows` reads. Cost: O(parts) expected.
pub fn part_rows(board: &Board) -> HashMap<CompId, ItemId> {
    board.placed.iter().map(|part| (part.comp, part.id)).collect()
}

/// The rows `sel` names on unit `unit`'s sheet, appended to `out`.
/// Cost: O(1) expected, except a net scans O(components + connections).
pub fn sheet_rows(unit: BoardId, circuit: &Circuit, board: Option<&Board>, index: &RowIndex, sel: &Sel, out: &mut Vec<u32>) {
    if sel.unit() != Some(unit) {
        return;
    }
    match sel {
        Sel::Comp(c) | Sel::Feature(FeatureRef::Part(c) | FeatureRef::Pad(c, _) | FeatureRef::Body(c) | FeatureRef::Port(c, _)) => {
            out.extend(index.comp(c.comp.0));
        }
        Sel::Pin(p) => out.extend(index.comp(p.comp.0)),
        Sel::Net(n) => {
            out.extend(index.net(n.net.0));
            for comp in &circuit.comps {
                if comp.conns.iter().any(|(_, net)| *net == n.net) {
                    out.extend(index.comp(comp.id.0));
                }
            }
        }
        Sel::Block(_, block) => out.extend(index.block(block.0)),
        Sel::Item(_, item) => {
            if let Some(part) = board.and_then(|board| board.placed_item(*item)) {
                out.extend(index.comp(part.comp.0));
            }
        }
        _ => {}
    }
}

/// Every non-zero texel of board `unit`, sorted by row, one entry per row.
/// Cost: O(parts + selections + named rows + net scans + lit * log(lit)) expected;
/// each net scan visits the board items and placement pads, and sorting visits the lit rows.
pub fn board_texels(unit: BoardId, board: &Board, violations: &[Violation], view: &ViewState) -> Vec<(u32, [f32; 4])> {
    let parts = part_rows(board);
    texels(violations, view, |sel, out| board_rows(unit, board, &parts, sel, out))
}

/// Every non-zero texel of unit `unit`'s sheet, sorted by row, one entry per row. The sheet shows
/// circuit findings only: a finding with a board location (`Violation::at`, the board's checks,
/// such as an unrouted connection) stays on the board and in Checks.
/// Cost: O(selections + named rows + net scans + lit * log(lit)) expected.
pub fn sheet_texels(unit: BoardId, circuit: &Circuit, board: Option<&Board>, index: &RowIndex, violations: &[Violation], view: &ViewState) -> Vec<(u32, [f32; 4])> {
    let circuit_findings = violations.iter().filter(|violation| violation.at.is_none());
    texels(circuit_findings, view, |sel, out| sheet_rows(unit, circuit, board, index, sel, out))
}

fn texels<'a>(violations: impl IntoIterator<Item = &'a Violation>, view: &ViewState, mut map_rows: impl FnMut(&Sel, &mut Vec<u32>)) -> Vec<(u32, [f32; 4])> {
    let mut texels: HashMap<u32, [f32; 4]> = HashMap::new();
    let mut rows = Vec::new();
    let mut mark = |sel: &Sel, channel: usize, value: f32| {
        map_rows(sel, &mut rows);
        for row in rows.drain(..) {
            let texel = texels.entry(row).or_insert([0.0; 4]);
            texel[channel] = texel[channel].max(value);
        }
    };
    for sel in &view.selected {
        mark(sel, 0, 1.0);
    }
    // Ghosts (y = 0) wait for the preview residency protocol.
    for violation in violations {
        let severity = match violation.severity {
            Severity::Info => 1.0,
            Severity::Warning => 2.0,
            Severity::Error => 3.0,
        };
        for sel in &violation.items {
            mark(sel, 2, severity);
        }
    }
    for (sel, emphasis) in &view.emphasis {
        mark(sel, 3, emphasis.clamp(0.0, 1.0));
    }
    let mut lit: Vec<_> = texels.into_iter().filter(|(_, texel)| *texel != [0.0; 4]).collect();
    lit.sort_unstable_by_key(|(row, _)| *row);
    lit
}

/// `texels` (sorted by row, one entry per row) with outside edits' flashes on `flashed` (row and
/// strength 0..1): where nothing emphasises a row its w goes below zero (`canvas/draw.rs`, slot 3),
/// the strongest flash of a row kept. Cost: O(texels + flashed log flashed).
pub fn with_flash(texels: Vec<(u32, [f32; 4])>, flashed: &[(u32, f32)]) -> Vec<(u32, [f32; 4])> {
    if flashed.is_empty() {
        return texels;
    }
    let mut all: HashMap<u32, [f32; 4]> = texels.into_iter().collect();
    for &(row, level) in flashed {
        let texel = all.entry(row).or_insert([0.0; 4]);
        if texel[3] <= 0.0 && level > 0.0 {
            texel[3] = texel[3].min(-level.min(1.0));
        }
    }
    let mut lit: Vec<_> = all.into_iter().filter(|(_, texel)| *texel != [0.0; 4]).collect();
    lit.sort_unstable_by_key(|(row, _)| *row);
    lit
}

/// One owner's item-state rows on the GPU, and what was written last.
pub struct ItemStates {
    rows: LiveRows,
    lit: Vec<(u32, [f32; 4])>,
    dirty: bool,
}

impl ItemStates {
    /// Empty item states, with a zero off texel. Cost: O(1), one fixed-width texture row.
    pub fn new() -> ItemStates {
        ItemStates { rows: LiveRows::with_off([0.0; 4]), lit: Vec::new(), dirty: false }
    }

    /// Replace the sorted, unique rows with `want`, ignoring rows outside `count`.
    /// Cost: O(old lit + want), plus `LiveRows::resize`'s storage growth or dropped rows.
    pub fn write(&mut self, count: usize, mut want: Vec<(u32, [f32; 4])>) {
        let grew = count > self.rows.nets();
        self.rows.resize(count);
        // Growth may need a larger texture even when the new texels are all zero.
        self.dirty |= grew;
        want.retain(|(row, _)| (*row as usize) < count);
        let mut next = 0;
        for &(row, texel) in &self.lit {
            while next < want.len() && want[next].0 < row {
                next += 1;
            }
            if next == want.len() || want[next].0 != row {
                self.dirty |= texel != [0.0; 4];
                if (row as usize) < count {
                    self.rows.set(row as usize, [0.0; 4]);
                }
            }
        }
        for &(row, texel) in &want {
            self.dirty |= self.rows.row(row as usize) != texel;
            self.rows.set(row as usize, texel);
        }
        self.lit = want;
    }

    /// A row's texel, or zero outside the count. Cost: O(1).
    pub fn row(&self, i: usize) -> [f32; 4] {
        self.rows.row(i)
    }

    /// Whether a write needs a draw, without consuming the pending upload. Cost: O(1).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Consume the flag for writes since the last draw. Cost: O(1).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Upload through `LiveRows` and clone its texture handle. Cost: O(changed texture-row span),
    /// or O(allocated rows) when first creating or growing the texture; O(1) when unchanged.
    pub fn texture(&mut self, cx: &mut Cx) -> Texture {
        self.rows.texture(cx).clone()
    }

    /// Free this owner's row texture and retained bindings without creating a texture.
    /// The holder calls this before dropping the states because Drop has no Cx.
    pub fn release(&mut self, cx: &mut Cx) {
        self.rows.release(cx);
    }
}

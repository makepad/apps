//! Unrouted board connections as hairlines in the pane's main pass, after the holes.
//!
//! `sync` compares connectivity by Arc identity and held items by value. Only a change scans
//! the lines: O(lines + held items) to split and publish the fixed lines. Unchanged inputs cost
//! O(held items) to compare. Fixed lines keep their retained list; moving lines rebuild in
//! O(moving lines) only on the first draw after a sync or when the nm offset changes. Otherwise
//! drawing only updates the retained lists' uniforms.
//!
//! Held pad ends follow the cursor in the same frame: each is drawn at its committed place plus
//! the drag's delta, whichever board (committed or a preview) the connectivity came from, so a
//! preview's connectivity changes which lines exist but never where a held end is drawn.
//!
//! Every other pad or via end is drawn where the board on screen (the picture's) has it, not
//! where the connectivity's board had it: an edit's connectivity and its picture arrive from
//! different workers, and a moved part and its lines must move in the same frame. A new picture
//! re-syncs: O(lines).

use std::{collections::HashSet, sync::Arc};

use fab_elec_board::{Anchor, Board, ConnItem, Connectivity, ItemKind, RatLine};
use fab_elec_core::{ItemId, P};
use makepad_widgets_core::makepad_platform::retained_instances::{
    InstanceSegment, RetainedInstances,
};
use makepad_widgets_core::{log, Cx2d, DrawVars};

use super::{
    inst::{self, Common, StrokeInst, StrokeTemplate},
    lists::{BoardTextures, BoardUniforms, StreamList},
};

#[derive(Default)]
pub struct RatsDraw {
    conn: Option<Arc<Connectivity>>,
    board: Option<Arc<Board>>,
    held: Vec<ItemId>,
    fixed: Option<StreamList>,
    moving: Option<StreamList>,
    moving_lines: Vec<MovingLine>,
    by: Option<P>,
}

struct MovingLine {
    pa: P,
    pb: P,
    move_a: bool,
    move_b: bool,
}

impl RatsDraw {
    pub fn new() -> RatsDraw {
        RatsDraw::default()
    }

    /// Use this connectivity, the board on screen and these held items; `committed` gives a held
    /// pad end's committed place. The same Arcs and item values change nothing; changed inputs
    /// split the lines into fixed and moving in O(lines + held items).
    pub fn sync(
        &mut self, conn: Option<&Arc<Connectivity>>, board: Option<&Arc<Board>>, held: &[ItemId],
        committed: &dyn Fn(ConnItem) -> Option<P>, t: &StrokeTemplate,
    ) {
        fn same<T>(a: Option<&Arc<T>>, b: Option<&Arc<T>>) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
        }
        if same(self.conn.as_ref(), conn) && same(self.board.as_ref(), board) && self.held == held {
            return;
        }
        self.conn = conn.cloned();
        self.board = board.cloned();
        self.held.clear();
        self.held.extend_from_slice(held);
        self.fixed = None;
        self.moving = None;
        self.moving_lines.clear();
        self.by = None;

        let Some(conn) = conn else { return };
        let held: HashSet<ItemId> = held.iter().copied().collect();
        let moves = |end| match end {
            ConnItem::Pad { placed, .. } => held.contains(&placed),
            ConnItem::Via(via) => held.contains(&via),
            _ => false,
        };
        let mut data = Vec::new();
        for line in &conn.ratsnest {
            let move_a = moves(line.a);
            let move_b = moves(line.b);
            let (pa, pb) = line_ends(line, board.map(|b| &**b), [move_a, move_b], committed);
            if move_a || move_b {
                self.moving_lines.push(MovingLine { pa, pb, move_a, move_b });
            } else {
                append_line(&mut data, pa, pb, t);
            }
        }
        self.fixed = list(data, t);
    }

    /// Draw fixed lines from their retained list and held pad ends at their committed place plus
    /// `by` (nm, world). Rebuild moving lines only after a sync or an offset change, in O(moving lines).
    pub fn draw(
        &mut self, cx: &mut Cx2d, stroke: &mut DrawVars, base: &BoardUniforms, ink: [f32; 4], by: P,
        t: &StrokeTemplate,
    ) {
        if self.by != Some(by) {
            let mut data = Vec::with_capacity(self.moving_lines.len() * t.slots());
            for line in &self.moving_lines {
                let pa = if line.move_a { line.pa.add(by) } else { line.pa };
                let pb = if line.move_b { line.pb.add(by) } else { line.pb };
                append_line(&mut data, pa, pb, t);
            }
            self.moving = list(data, t);
            self.by = Some(by);
        }
        let u = BoardUniforms {
            layer_ink: ink,
            layer_alpha: 1.0,
            hover_item: -1.0,
            item_states_on: 0.0,
            pane_states_on: 0.0,
            ..*base
        };
        let tex = BoardTextures::default();
        if let Some(list) = self.fixed.as_mut() {
            list.draw(cx, stroke, &tex, &u);
        }
        if let Some(list) = self.moving.as_mut() {
            list.draw(cx, stroke, &tex, &u);
        }
    }

    /// Lines drawn (fixed + moving), for the pane's stats.
    pub fn lines(&self) -> usize {
        self.conn.as_ref().map_or(0, |conn| conn.ratsnest.len())
    }
}

/// Where `line`'s ends are drawn: a held end (`held`) at its committed place, any other pad or via
/// end where `board` (the one on screen) has it, the rest (or an end `board` lacks) at the point
/// the connectivity stored. O(1) per end.
fn line_ends(line: &RatLine, board: Option<&Board>, held: [bool; 2], committed: &dyn Fn(ConnItem) -> Option<P>) -> (P, P) {
    let end = |item: ConnItem, stored: P, held: bool| {
        if held {
            committed(item)
        } else {
            board.and_then(|board| on_board(board, item))
        }
        .unwrap_or(stored)
    };
    (end(line.a, line.pa, held[0]), end(line.b, line.pb, held[1]))
}

/// A pad or via end's place on `board`; None for other ends or an item `board` lacks.
fn on_board(board: &Board, item: ConnItem) -> Option<P> {
    match item {
        ConnItem::Pad { placed, pad } => board.anchor_pos(Anchor::Pad { placed, pad }),
        ConnItem::Via(via) => match board.locate(via)? {
            (ItemKind::Via, i) => Some(board.vias[i].at),
            _ => None,
        },
        _ => None,
    }
}

fn append_line(data: &mut Vec<f32>, pa: P, pb: P, t: &StrokeTemplate) {
    let [ax, ay] = inst::pt(pa);
    let [bx, by] = inst::pt(pb);
    t.append(data, &StrokeInst {
        c: Common::rest(-1.0),
        seg: [ax, ay, bx, by],
        sag: 0.0,
        width: 0.0,
        along0: 0.0,
    });
}

fn list(data: Vec<f32>, t: &StrokeTemplate) -> Option<StreamList> {
    if data.is_empty() {
        return None;
    }
    match RetainedInstances::from_segments(t.slots(), vec![InstanceSegment::new(data.into())]) {
        Ok(publication) => Some(StreamList::new(publication)),
        Err(e) => {
            log!("fab.board: a ratsnest stream is not drawn: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{NetId, PadIx};

    /// A part an edit moved: its lines' ends are where the picture on screen has it, so the part
    /// and its lines move in the same frame whichever of the connectivity or the picture comes
    /// first; a held end stays at its committed place; ends that are not pads or vias keep the
    /// connectivity's point.
    #[test]
    fn line_ends_follow_the_board_on_screen() {
        let design = fab_elec_fixtures::ledctl();
        let board = design.units.iter().find_map(|unit| unit.board.as_deref()).expect("ledctl has a board");
        let part = board.placed[0].id;
        let pad = ConnItem::Pad { placed: part, pad: PadIx(0) };
        let stored = board.anchor_pos(Anchor::Pad { placed: part, pad: PadIx(0) }).expect("the part's first pad");
        let track_point = P::new(1, 2);
        let line = RatLine { net: NetId(0), a: pad, b: ConnItem::Track { item: board.tracks[0].id, seg: 0 }, pa: stored, pb: track_point };
        let mut moved = board.clone();
        moved.placed[0].at = moved.placed[0].at.add(P::new(1_000_000, 0));
        let shown = moved.anchor_pos(Anchor::Pad { placed: part, pad: PadIx(0) }).unwrap();
        assert_ne!(shown, stored, "the edit moved the pad");
        let none = |_: ConnItem| None;
        assert_eq!(line_ends(&line, None, [false, false], &none), (stored, track_point), "with no picture yet, the connectivity's points");
        assert_eq!(line_ends(&line, Some(board), [false, false], &none), (stored, track_point), "the picture of the old board keeps the line at the old place");
        assert_eq!(line_ends(&line, Some(&moved), [false, false], &none), (shown, track_point), "the picture of the moved part takes the pad end along; a track end keeps its point");
        let committed = |_: ConnItem| Some(P::new(7, 7));
        assert_eq!(line_ends(&line, Some(&moved), [true, false], &committed).0, P::new(7, 7), "a held end is at its committed place, whatever the picture shows");
        if let Some(via) = board.vias.first() {
            let line = RatLine { b: ConnItem::Via(via.id), pb: via.at, ..line };
            let mut moved = board.clone();
            moved.vias[0].at = via.at.add(P::new(0, 500_000));
            assert_eq!(line_ends(&line, Some(&moved), [false, false], &none).1, moved.vias[0].at, "a via end is where the picture has the via");
        }
    }
}

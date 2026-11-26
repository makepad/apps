use fab_elec_board::{Board, Finish, PieceIndex, ARC_ERR};
use fab_elec_core::{clearance, poly::ring_area2, BBox, Nm};
use std::ops::ControlFlow;

#[derive(Clone, Debug)]
pub struct BoardFacts {
    /// Twice the outline area in nm². Rings use the board's standard arc tolerance.
    pub area2_nm2: i128,
    pub bounds: BBox,
    pub layers: u8,
    pub thickness: Nm,
    pub finish: Finish,
    pub mask_color: u32,
    pub smallest_track: Option<Nm>,
    /// Edge-to-edge gap between different conductors on the authored board, before zone fill.
    pub smallest_gap: Option<Nm>,
    pub smallest_drill: Option<Nm>,
    pub hole_count: usize,
}

impl BoardFacts {
    pub fn from_board(board: &Board) -> Self {
        let drills = board.vias.iter().map(|v| v.drill).chain(board.holes.iter().map(|h| h.drill))
            .chain(board.placed.iter().flat_map(|p| &p.footprint.pads)
                .filter_map(|p| p.drill.as_ref().map(|d| d.size.x.min(d.size.y))));
        let mut smallest_drill = None;
        let mut hole_count = 0;
        for diameter in drills {
            smallest_drill = Some(smallest_drill.map_or(diameter, |d: Nm| d.min(diameter)));
            hole_count += 1;
        }
        let smallest_track = board.tracks.iter().map(|t| t.widths.as_ref()
            .and_then(|ws| ws.iter().copied().min()).unwrap_or(t.w)).min();
        Self {
            area2_nm2: board.outline_polyset(ARC_ERR).rings().map(ring_area2).sum(),
            bounds: board.outline_bbox(), layers: board.copper_count(), thickness: board.stackup.thickness,
            finish: board.stackup.finish, mask_color: board.stackup.mask_color, smallest_track,
            smallest_gap: copper_gap(board), smallest_drill, hole_count,
        }
    }
}

fn copper_gap(board: &Board) -> Option<Nm> {
    let index = PieceIndex::build(board, &[]);
    let mut best = None;
    for piece in index.iter() {
        let Some(a) = piece.copper() else { continue };
        index.query(a.layer, piece.bbox.inflate(best.unwrap_or(Nm::MAX)), |other| {
            if other.key() <= piece.key() { return ControlFlow::Continue(()); }
            if let Some(b) = other.copper() {
                if !a.same_item(b) && (a.net.is_none() || a.net != b.net) {
                    let gap = clearance(&a.shape, &b.shape).max(0);
                    best = Some(best.map_or(gap, |old| old.min(gap)));
                    if gap == 0 { return ControlFlow::Break(()); }
                }
            }
            ControlFlow::Continue(())
        });
        if best == Some(0) { break; }
    }
    best
}

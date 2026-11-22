//! What every engine returns (E6 1.3, 11): a solution as items to add and owned items to
//! remove, the nets still open with the reason, and the metrics every proposal, lane report
//! and parity table carries.

use fab_elec_board::{BoardOp, Item};
use fab_elec_circuit::Measure;
use fab_elec_core::{canon_enum, canon_struct, ItemId, LayerId, NetId, Nm, Sel, P};

/// An engine's answer. After any transaction it is legal (E6 0.6): what it adds passes the
/// verifier, and what is not done is listed in `open`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct RouteSolution {
    /// Tracks with their ends anchored, vias, tuning objects; `Origin::Solved { by }`.
    pub add: Vec<Item>,
    /// Owned (`Solved`) copper the solution replaces.
    pub remove: Vec<ItemId>,
    pub open: Vec<(NetId, Reason)>,
    /// Nets outside the problem reconnected by the job, sorted and deduplicated.
    pub repaired: Vec<NetId>,
    pub metrics: RouteMetrics,
}

impl RouteSolution {
    /// The solution as board ops: removals first, then additions, in order.
    pub fn ops(&self) -> Vec<BoardOp> {
        self.remove.iter().map(|id| BoardOp::Remove(*id)).chain(self.add.iter().cloned().map(BoardOp::Add)).collect()
    }
}

/// Why the router left a net or junction open or kept a sharp corner, for a person and for the AI (E6 1.2).
#[derive(Clone, PartialEq, Debug)]
pub enum Reason {
    /// The best corridor needs more room than there is at `at` on `layer`, between those two.
    /// `by`: the copper there that the router may not move (kept tracks, vias and pours of other
    /// nets), in the order a way through would meet it; empty when nothing but pads, keepouts
    /// or the board's edge leaves no room, or when the reason is a gesture's.
    NoRoom { at: P, layer: LayerId, need: Measure, have: Measure, between: [Sel; 2], by: Vec<Sel> },
    /// It routes only through copper of other nets: the cheapest blockers, by net.
    /// Or routing it would cut a listed net off its pour.
    /// In a partial offered during rip-up, a ripped net waiting for its reroute is Blocked by
    /// the net it made room for.
    Blocked { at: P, by: Vec<NetId> },
    /// A terminal no route reaches: it lies inside foreign copper's clearance or outside the
    /// board, nothing reaches it, or the net's own new copper cut it off its pour. `with`: the
    /// terminal's item there (a pad, mostly), `at` its place.
    Terminal { at: P, with: Option<Sel> },
    /// The job ended (cancelled, or its effort spent) before this net was tried.
    NotTried,
    /// The end of `track` at `at` came off `host`: the edit moved its copper away or removed it.
    /// The board may attach the end to other copper of its net there on commit. DRC reports
    /// the net's connectivity.
    Unjoined { at: P, track: Sel, host: Sel },
    /// A drag laid a corner sharper than a right angle on `track` at `at` on `layer`.
    /// There was no room to chamfer it, so the copper is kept as laid.
    SharpCorner { at: P, layer: LayerId, track: Sel },
}
canon_enum!(Reason {
    0 => NoRoom { at, layer, need, have, between, by },
    1 => Blocked { at, by },
    2 => Terminal { at, with },
    3 => NotTried,
    4 => Unjoined { at, track, host },
    5 => SharpCorner { at, layer, track },
});


/// Counted work: what tests compare (never wall time).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct WorkCounts {
    /// Search nodes expanded.
    pub expansions: u64,
    /// `make_legal` + `tighten` calls.
    pub embeds: u64,
    pub transactions: u32,
    pub rips: u32,
    /// Routes of a planned net again in the same transaction (the grid router's pour-neck retry).
    pub retries: u32,
    /// Of `expansions`, the work of attempts that were rolled back.
    pub discarded: u64,
}

// Additive: `retries` and `discarded` are written only when nonzero, each tagged, so work counts
// without a rollback keep their hash.
impl fab_elec_core::canon::Canon for WorkCounts {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let WorkCounts { expansions, embeds, transactions, rips, retries, discarded } = self;
        expansions.canon(w);
        embeds.canon(w);
        transactions.canon(w);
        rips.canon(w);
        if *retries != 0 {
            w.tag(1);
            retries.canon(w);
        }
        if *discarded != 0 {
            w.tag(2);
            discarded.canon(w);
        }
    }
}


/// Route quality and work (E6 11). Lengths in nm.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct RouteMetrics {
    /// Completed problem nets plus the solution's distinct repaired nets outside the problem.
    pub nets_routed: u32,
    pub nets_open: u32,
    /// Total copper length of the solution's tracks.
    pub length: i64,
    pub vias: u32,
    /// Bends (`@fortyfive`).
    pub bends: u32,
    /// Total turning angle, micro-degrees (curved styles).
    pub turning: i64,
    /// The smallest clearance margin (gap minus required) over the solution; `None` when it
    /// is near nothing.
    pub min_margin: Option<Nm>,
    /// Corners sharper than 90 degrees.
    pub acute: u32,
    /// Findings `drc` reports on the result that the board did not have, or had at a larger gap
    /// (`check::new_or_worse`; must be 0).
    pub new_violations: u32,
    pub work: WorkCounts,
    /// Each kept rip-up attempt in the order kept, with each net's job copper before and after it:
    /// what an attempt cost the board, without a trace.
    pub kept: Vec<Kept>,
}

// Additive: `kept` is written only when nonempty, tagged, so metrics without a kept attempt keep
// their hash.
impl fab_elec_core::canon::Canon for RouteMetrics {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let RouteMetrics {
            nets_routed, nets_open, length, vias, bends, turning, min_margin, acute, new_violations, work, kept,
        } = self;
        nets_routed.canon(w);
        nets_open.canon(w);
        length.canon(w);
        vias.canon(w);
        bends.canon(w);
        turning.canon(w);
        min_margin.canon(w);
        acute.canon(w);
        new_violations.canon(w);
        work.canon(w);
        if !kept.is_empty() {
            w.tag(1);
            kept.canon(w);
        }
    }
}

/// One kept rip-up attempt, net by net. The attempted net was open, so what it gains is the route
/// the attempt was for; what a ripped net gains is what the attempt cost the board. `others` are
/// the nets the attempt changed without ripping them (pours its routes cut and the job repaired).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Kept {
    pub own: NetCopper,
    /// In the order of their first rip.
    pub ripped: Vec<NetCopper>,
    /// By net.
    pub others: Vec<NetCopper>,
}
canon_struct!(Kept { own, ripped, others });

/// One net's job copper (its tracks and vias in `RouteSolution::add`): length (nm) and vias before
/// a kept attempt and once it is kept.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct NetCopper {
    pub net: NetId,
    pub length_before: i64,
    pub vias_before: u32,
    pub length_after: i64,
    pub vias_after: u32,
}
canon_struct!(NetCopper { net, length_before, vias_before, length_after, vias_after });


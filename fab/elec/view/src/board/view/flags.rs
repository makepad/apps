//! The board's probe flags ([`crate::live::flags`]): the probes of the bench this board probes
//! into whose nets are on its board, each a named connection at the pad [`probe_anchor`] picks (the
//! pad its `at` or its label names, else one of the part its label names, else one of the smallest
//! part on the net); the ghost flag a probe carried over copper draws before its drop; and the
//! ghost of a flag carried along its net, at the pad it would land on.
//!
//! Cost per draw: O(probes x lanes) for the hues; the anchors are resolved once per bench list and
//! drawn board (O(probes x pads on the board)). Per pointer event one pass over the shown flags; a
//! carry adds one pass over the board's pads per pointer move.
//!
//! [`probe_anchor`]: crate::board::nodes::probe_anchor

use super::BoardPane;
use crate::board::nodes::{net_pads, pad_anchor, probe_anchor, probe_names, BoardItems};
use crate::canvas::pane::HIT_RADIUS;
use crate::live::flags::{nearest_pin, Anchor, Flag, FlagInput};
use crate::live::peek::PeekGesture;
use crate::live::probe::{host_bench, place_probe_op};
use crate::snapshot::ElecLive;
use crate::ViewAction;
use fab_elec_core::{to_mm, NetId};
use fab_elec_design::{Design, ProbeQuantity};
use makepad_widgets_core::*;
use std::sync::Arc;

/// The zoom at which a flag is its own size on a board, points per millimetre: a 50 mm board
/// filling a pane of about 1200 points.
pub const BOARD_REFERENCE: f64 = 24.0;

/// The bench's probes on the drawn board, resolved once per bench list, design and board.
#[derive(Default)]
pub(super) struct BoardProbes {
    key: Option<(usize, usize, usize)>,
    /// Per probe: its lowered net name, label and anchor.
    probes: Vec<(String, String, Option<Anchor>)>,
}

/// A probe carried over copper: the flag its drop would make.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BoardGhost {
    name: String,
    label: String,
    anchor: Anchor,
}

impl BoardPane {
    /// The flags before the copper: a click on one shows its probe's lane in the Scope
    /// (`OpenInScope`); a flag carried along its net and let go places it at the net's pad nearest
    /// the pointer (`BenchOp::SetProbeAt`). True when the event was the flags' (the board does not
    /// see it); the pointer going onto a flag leaves the board's hovered item.
    pub(super) fn flag_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        match self.flags.handle_event(cx, event, self.draw_bg.area()) {
            FlagInput::None => return false,
            FlagInput::Moved { net, abs } => {
                if let Some(op) = self.flag_move(cx, &net, abs) {
                    cx.widget_action(self.uid, op);
                }
            }
            FlagInput::Taken | FlagInput::Open(_) => {}
        }
        if let Some(input) = self.pane.clear_hover(Cx::monotonic_now()) {
            self.input(cx, input);
        }
        true
    }

    /// The pad of `net`'s probe nearest `abs` (absolute), named, and its anchor: where its flag
    /// lands when let go there.
    fn flag_landing(&self, net: &str, abs: DVec2) -> Option<(String, Anchor)> {
        let board = &self.st.drawn.as_ref()?.board;
        let id = board.nets.iter().position(|n| n.name == net)?;
        let pads = net_pads(board, NetId(id as u32));
        let k = nearest_pin(pads.iter().map(|(_, anchor)| anchor.at), self.pane.cam.to_world(abs))?;
        pads.into_iter().nth(k)
    }

    /// The edit placing `net`'s flag at the pad nearest `abs`, in the bench this board probes into.
    fn flag_move(&self, cx: &Cx, net: &str, abs: DVec2) -> Option<ViewAction> {
        let (at, _) = self.flag_landing(net, abs)?;
        let name = self.st.bench.clone().or_else(|| host_bench(cx))?;
        let live = cx.get_global_ref::<ElecLive>()?;
        let bench = live.snap.benches.iter().find(|bench| bench.out.name == name)?;
        place_probe_op(&name, &bench.out.probes, net, &at)
    }

    /// A probe carried over `abs` (absolute): the ghost flag its drop would make there; None off
    /// copper or on a net that has a flag already.
    pub(super) fn board_ghost(&self, design: Option<&Design>, abs: DVec2) -> Option<BoardGhost> {
        let draw = self.st.drawn.as_ref()?;
        let at = self.pane.cam.to_world(abs);
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems { unit: geom.board.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
        let hit = items.probe_at(at, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12))?;
        let comps = design.and_then(|design| design.unit(geom.board.unit)).map_or(&[][..], |unit| &unit.circuit.comps[..]);
        let (name, label) = probe_names(&geom.board, comps, hit)?;
        if self.st.flag_probes.probes.iter().any(|(probed, ..)| *probed == name) { return None; }
        // A probe reads a net of the circuit.
        design?.unit(geom.board.unit)?.circuit.nets.iter().find(|net| net.name == name)?;
        let anchor = hit.pad
            .and_then(|(item, pad)| pad_anchor(geom.board.placed.iter().find(|placed| placed.id == item)?, pad as usize))
            .unwrap_or(Anchor { at, out: [0.0, 1.0], body: None });
        Some(BoardGhost { name, label, anchor })
    }

    /// Draws the board's probes, a ring on each pad with its callout in the margin outside the
    /// board's outline, in the pane `rect`.
    pub(super) fn draw_flags(&mut self, cx: &mut Cx2d, rect: Rect) {
        let design = self.design(cx);
        let board = self.st.drawn.as_ref().map(|draw| draw.board.clone());
        let bench = cx.get_global_ref::<ElecLive>().and_then(|live| match &self.st.bench {
            Some(name) => live.snap.benches.iter().find(|bench| &bench.out.name == name),
            None => live.run_bench(),
        }).filter(|bench| board.as_ref().is_some_and(|board| board.unit == bench.out.scope_id()));
        let (Some(design), Some(board), Some(bench)) = (&design, &board, bench) else {
            self.st.flag_probes = BoardProbes::default();
            self.flags.release();
            return;
        };
        let carried = self.flags.carried().map(|(net, abs)| (net.to_owned(), abs));
        let landing = carried.and_then(|(net, abs)| Some((net.clone(), self.flag_landing(&net, abs)?.1)));
        let key = (bench as *const _ as usize, Arc::as_ptr(design) as usize, Arc::as_ptr(board) as usize);
        let probes = &mut self.st.flag_probes;
        if probes.key != Some(key) {
            probes.probes = bench.out.probes.iter()
                .filter(|probe| probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none())
                .filter_map(|probe| {
                    let on = board.nets.iter().position(|net| net.name == probe.of.target)?;
                    let anchor = probe_anchor(board, NetId(on as u32), probe.at.as_deref().unwrap_or(&probe.label));
                    Some((probe.of.target.clone(), probe.label.clone(), anchor))
                })
                .collect();
            probes.key = Some(key);
        }
        let gesture = cx.get_global_ref::<PeekGesture>();
        // A flag wears its probe's hue, the one its lanes wear; before the Bench has the lanes, its
        // place among the probes.
        let hue = |name: &str, i: usize| gesture.map_or(i, |gesture| gesture.hue_of(name, i));
        let mut flags = Vec::with_capacity(probes.probes.len() + 1);
        for (i, (name, label, anchor)) in probes.probes.iter().enumerate() {
            let Some(anchor) = *anchor else { continue };
            flags.push(Flag { net: name, label, hue: hue(name, i), anchor, ghost: false });
        }
        if let Some((net, anchor)) = &landing {
            if let Some(i) = flags.iter().position(|flag| flag.net == net) {
                flags.push(Flag { anchor: *anchor, ghost: true, ..flags[i].clone() });
            }
        }
        if let Some(ghost) = &self.st.flag_ghost {
            let hue = gesture.map_or(probes.probes.len(), |gesture| gesture.hue_of(&ghost.name, probes.probes.len()));
            flags.push(Flag { net: &ghost.name, label: &ghost.label, hue, anchor: ghost.anchor, ghost: true });
        }
        let outline = board.outline_bbox();
        let outline = [to_mm(outline.min.x), to_mm(outline.min.y), to_mm(outline.max.x), to_mm(outline.max.y)];
        self.flags.draw(cx, &self.pane.cam, rect, BOARD_REFERENCE, &flags, outline, self.st.strip_at);
    }
}

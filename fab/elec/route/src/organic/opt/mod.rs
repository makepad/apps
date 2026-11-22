//! The finish passes (L6) and what they share:
//! - `cleanup`: optimize/Cleanup.h/.cpp's skeleton cleanup, judged through `Guard`;
//! - `drcgeom`: core/Drc.h/.cpp's distance and layer kernels (the rule engine is Fab's DRC);
//! - `hops`: optimize/ViaHops.cpp's via-hop count;
//! - `layer_reassign`: optimize/LayerReassign.h/.cpp's guarded layer reassignment;
//! - `pairguard`: optimize/PairGuard.h (keep pair coupling through finish-pass edits);
//! - `viahops`: optimize/ViaHops.h/.cpp's short via-hop removal, judged through `Guard`;
//! - here: `Guard`, what a pass asks where the reference runs its DRC (Fab's side), and the
//!   zone and intent lookups the passes share (ported: ZoneCopper.cpp, RoutingIntents.cpp).

pub mod cleanup;
pub mod drcgeom;
pub mod hops;
pub mod layer_reassign;
pub mod pairguard;
pub mod viahops;

use super::geom::{std_max, Vec2};
use super::model::{FanoutStyle, OBoard};

/// What a finish pass asks where the reference runs `Drc::run` and compares the reports (Fab's
/// side, not a port): the organic job's judge in production. A test may script refusals to prove
/// a pass's rollback; no test guard decides legality.
pub trait Guard {
    /// Judge the copper of `nets` as the pass left it in `ob` against the last accepted state:
    /// lifted to Fab's board and checked by Fab's DRC and connectivity. True accepts it (the new
    /// state); false refuses it (the accepted state stays, and the pass restores its own copy of
    /// what it changed, as the reference does).
    fn judge(&mut self, ob: &OBoard, nets: &[i32]) -> bool;
    /// Take the copper of `nets` as `ob` holds it as the accepted state, unjudged: a pass that
    /// restores its own input after judged steps (the reference's final restore) says so here,
    /// so the guard's state is the board's again.
    fn sync(&mut self, ob: &OBoard, nets: &[i32]);
    /// What made the last `judge` call refuse, for a pass that reverts the edits near the
    /// reference's new violations (ViaHops' guard loop): each new finding with its location
    /// and nets, and each net with more missing connections (no location). Empty after an
    /// accept. A guard that knows no more says nothing, and such a pass then reverts every
    /// active edit, as the reference does when no violation names one.
    fn refusal(&self) -> &[Finding] {
        &[]
    }
    /// `ZoneCopper::tapConnected`: a fill of `net` on copper layer `layer` (stack index) touches
    /// the disc at `pos` of `radius` (mm) in the accepted state's fills.
    fn tap_connected(&self, net: i32, layer: i32, pos: Vec2, radius: f32) -> bool;
}

/// One reason of a refused judge call (Fab's side): where the reference's `DrcViolation::pos`
/// would be (mm, the OBoard frame; none for a net that lost a connection) and the nets
/// (port index) of the items it names (`netA` / `netB`).
#[derive(Clone, Debug, Default)]
pub struct Finding {
    pub pos: Option<Vec2>,
    pub nets: Vec<i32>,
}

/// `ZoneCopper::zoneNets` (ZoneCopper.cpp:489-497): the nets of the copper zones, sorted, once
/// each (a zone without a net gives -1, as there).
pub fn zone_nets(b: &OBoard) -> Vec<i32> {
    let mut nets: Vec<i32> = b.zones.iter().filter(|z| !z.is_rule_area && z.net < b.nets.len() as i32).map(|z| z.net).collect();
    nets.sort_unstable();
    nets.dedup();
    nets
}

/// `ZoneCopper::netHasZoneOnLayer` (ZoneCopper.cpp:499-504). ADAPTED: zone masks are stack bits.
pub fn net_has_zone_on_layer(b: &OBoard, net: i32, layer: i32) -> bool {
    let bit = if (0..64).contains(&layer) { 1_u64 << layer } else { 0 };
    b.zones.iter().any(|z| !z.is_rule_area && z.net == net && z.layer_mask & bit != 0)
}

/// `RoutingIntents::fanoutViaMask` (RoutingIntents.cpp:1956-1979): per via, whether it is the
/// fan-out via a designer asked for (a dog-bone or in-pad SMD pad of its net within the pad's
/// half size plus 1.5 mm); empty when no pad asks.
pub fn fanout_via_mask(b: &OBoard) -> Vec<bool> {
    let pads: Vec<_> = b
        .pads
        .iter()
        .filter(|p| p.is_smd && p.net >= 0 && matches!(p.fanout, FanoutStyle::Dogbone | FanoutStyle::InPad))
        .collect();
    if pads.is_empty() {
        return Vec::new();
    }
    b.vias
        .iter()
        .map(|v| pads.iter().any(|p| p.net == v.net && v.pos.distance_to(p.center) < 0.5f32 * std_max(p.size.x, p.size.y) + 1.5f32))
        .collect()
}

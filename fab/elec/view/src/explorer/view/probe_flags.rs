//! The sheet's probe flags ([`crate::live::flags`]): the probes of the bench this sheet probes
//! into whose nets are on its unit, each a named connection at the pin [`Scene::probe_anchor`]
//! picks (the pin its `at` or its label names, else a pin of the smallest part on the net, else the
//! net's longest wire); the ghost flag a probe dragged over a net draws before its drop; and the
//! ghost of a flag carried along its net, at the pin it would land on. None while a part reveal is
//! up. Each is a small tag in world units at the free spot nearest its pin ([`place_tags`]), behind
//! the drawing, its name drawn with the drawing's text ([`ExplorerView::draw_flag_labels`]); a part
//! under or near the pointer comes before a tag.
//!
//! Cost per draw: O(probes x pins on the sheet) for the anchors (tens of probes, hundreds of pins;
//! no index), O(probes x lanes) for the hues; per new bench list or design an O(probes x nets)
//! name lookup, cached. The tags' places: once per layout, label legibility step and probe list,
//! O(probes x (obstacles + spots x nearby obstacles)) (about a millisecond for tens of probes on a
//! sheet of hundreds of parts); a ghost is placed alone per draw while a drag goes on. Per pointer
//! event one pass over the shown flags and, over the sheet, one body lookup; a carry adds one pass
//! over its net's pins per pointer move.
//!
//! [`Scene::probe_anchor`]: crate::explorer::scene::Scene::probe_anchor

use super::{ExplorerView, HIT_RADIUS};
use crate::explorer::scene::{text_points, NetPin, TEXT_MIN_POINTS};
use crate::live::flags::{nearest_pin, place_tags, pointer_abs, Anchor, Flag, FlagInput, SCOPE_TAG};
use crate::live::peek::PeekGesture;
use crate::live::probe::{host_bench, place_probe_op};
use crate::snapshot::ElecLive;
use crate::ViewAction;
use fab_elec_circuit::Circuit;
use fab_elec_design::ProbeQuantity;
use fab_elec_explore::sheet;
use makepad_widgets_core::*;
use std::sync::Arc;

/// A tag's clearance from the drawing and from other tags, world units.
const TAG_GAP: f64 = sheet::TEXT_NET * 0.35;

/// The world size of the tag for a probe labelled `label`: a flag scope ([`SCOPE_TAG`] net-name
/// heights), wider when its name (the label's first line) at the sheet's net-name size needs it.
fn tag_size(label: &str) -> [f64; 2] {
    let title = label.lines().next().unwrap_or("");
    let [w, h] = SCOPE_TAG;
    [(sheet::text_w(title, sheet::TEXT_NET) + sheet::TEXT_NET).max(sheet::TEXT_NET * w), sheet::TEXT_NET * h]
}

/// The world box `b` moved by `d`.
fn moved(b: [f64; 4], d: [f64; 2]) -> [f64; 4] {
    [b[0] + d[0], b[1] + d[1], b[2] + d[0], b[3] + d[1]]
}

/// The bench's probes on the shown unit, resolved once per bench list, design and unit.
#[derive(Default)]
pub(super) struct SheetProbes {
    key: Option<(usize, usize, usize)>,
    /// Per probe: its circuit net, lowered name, label and place (`at`).
    probes: Vec<(u32, String, String, Option<String>)>,
    pins: Vec<NetPin>,
    /// Per probe, its tag's world box less its pin's point (the tag follows a pin in flight), and
    /// what they were placed for: the scene's build, its label legibility and the probe list.
    places: Vec<Option<[f64; 4]>>,
    places_key: Option<(usize, u64, (usize, usize, usize))>,
    /// What tags must not sit on, kept for ghosts placed while a drag goes on.
    obstacles: Vec<[f64; 4]>,
}

/// A probe carried over the sheet: the flag its drop would make.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ProbeGhost {
    net: u32,
    name: String,
    label: String,
    anchor: Anchor,
}

/// The names (`"<refdes>.<pad>"`, as a probe's `at` writes them) of `pins`, pins of `circuit`.
fn pin_names<'a>(circuit: &'a Circuit, pins: &'a [NetPin]) -> impl Iterator<Item = Option<String>> + 'a {
    pins.iter().map(move |pin| {
        let comp = circuit.comps.get(pin.node as usize)?;
        let pad = comp.part.pins.get(pin.pin as usize)?.pads.first()?;
        Some(format!("{}.{pad}", comp.refdes))
    })
}

impl ExplorerView {
    /// The flags before the canvas: a click on one shows its probe's lane in the Scope
    /// (`OpenInScope`); a flag carried along its net and let go places it at the net's pin nearest
    /// the pointer (`BenchOp::SetProbeAt`). True when the event was the flags' (the canvas does not
    /// see it); the pointer going onto a flag leaves the canvas's hovered item.
    pub(super) fn flag_event(&mut self, cx: &mut Cx, event: &Event, now: f64) -> bool {
        // The drawing is over the tags: a part under or near the pointer is the canvas's even where
        // a tag lies behind it (it selects normally); only a hit on the tag itself is the tag's.
        let yields = pointer_abs(event).is_some_and(|abs| {
            let radius = HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12);
            self.scene.body_at(self.pane.cam.to_world(abs), radius, now).is_some()
        });
        match self.flags.handle_event_yielding(cx, event, self.draw_bg.area(), yields) {
            FlagInput::None => return false,
            FlagInput::Moved { net, abs } => {
                if let Some(op) = self.flag_move(cx, &net, abs, now) {
                    cx.widget_action(self.uid, op);
                }
            }
            FlagInput::Taken | FlagInput::Open(_) => {}
        }
        if let Some(input) = self.pane.clear_hover(now) {
            self.input(cx, input);
        }
        true
    }

    /// The pin of `net`'s probe nearest `abs` (absolute), named, and its anchor: where its flag
    /// lands when let go there.
    fn flag_landing(&self, net: &str, abs: DVec2, now: f64) -> Option<(String, Anchor)> {
        let design = self.sheet_design.as_ref()?;
        let circuit = &design.units.get(self.unit)?.circuit;
        let id = circuit.nets.iter().position(|n| n.name == net)? as u32;
        let mut pins = Vec::new();
        self.scene.net_pins(id, now, &mut pins);
        let k = nearest_pin(pins.iter().map(|pin| pin.at), self.pane.cam.to_world(abs))?;
        let name = pin_names(circuit, &pins).nth(k)??;
        let pin = &pins[k];
        Some((name, Anchor::pin(pin.at, pin.out, Some(pin.body))))
    }

    /// The edit placing `net`'s flag at the pin nearest `abs`, in the bench this sheet probes into.
    fn flag_move(&self, cx: &Cx, net: &str, abs: DVec2, now: f64) -> Option<ViewAction> {
        let (at, _) = self.flag_landing(net, abs, now)?;
        let name = self.bench.clone().or_else(|| host_bench(cx))?;
        let live = cx.get_global_ref::<ElecLive>()?;
        let bench = live.snap.benches.iter().find(|bench| bench.out.name == name)?;
        place_probe_op(&name, &bench.out.probes, net, &at)
    }

    /// A probe carried over `abs` (absolute): the ghost flag its drop would make there; None off a
    /// net, on a net that has a flag already, or while a reveal is up.
    pub(super) fn probe_ghost(&self, abs: DVec2, now: f64) -> Option<ProbeGhost> {
        if self.reveal.is_some() { return None; }
        let at = self.pane.cam.to_world(abs);
        let (name, label) = self.probe_at_world(at, now)?;
        if self.sheet_probes.probes.iter().any(|(_, probed, ..)| *probed == name) { return None; }
        let radius = HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12);
        let (net, pin, node) = self.scene.probe_hit(at, radius, now)?;
        let mut pins = Vec::new();
        self.scene.net_pins(net, now, &mut pins);
        let anchor = pin.and_then(|pin| pins.iter().find(|p| p.node == node && p.pin as usize == pin))
            .map_or(Anchor { at, out: [0.0, 1.0], body: None }, |p| Anchor::pin(p.at, p.out, Some(p.body)));
        Some(ProbeGhost { net, name, label, anchor })
    }

    /// Draws the sheet's probe tags and their leaders behind its symbols and wires, in the pane
    /// `rect`; their names follow with [`Self::draw_flag_labels`].
    pub(super) fn draw_flags(&mut self, cx: &mut Cx2d, rect: Rect, now: f64) {
        let design = self.sheet_design.clone();
        let bench = cx.get_global_ref::<ElecLive>().and_then(|live| match &self.bench {
            Some(name) => live.snap.benches.iter().find(|bench| &bench.out.name == name),
            None => live.run_bench(),
        });
        let unit = design.as_ref().and_then(|design| design.units.get(self.unit));
        let bench = bench.filter(|bench| unit.is_some_and(|unit| unit.id == bench.out.scope_id()));
        let (Some(design), Some(unit), Some(bench)) = (&design, unit, bench) else {
            self.sheet_probes = SheetProbes::default();
            self.flags.release();
            return;
        };
        if self.reveal.is_some() {
            self.flags.release();
            return;
        }
        // A flag carried along its net: its ghost at the pin it would land on.
        let carried = self.flags.carried().map(|(net, abs)| (net.to_owned(), abs));
        let landing = carried.and_then(|(net, abs)| Some((net.clone(), self.flag_landing(&net, abs, now)?.1)));
        let key = (bench as *const _ as usize, Arc::as_ptr(design) as usize, self.unit);
        let probes = &mut self.sheet_probes;
        if probes.key != Some(key) {
            let circuit = &unit.circuit;
            probes.probes = bench.out.probes.iter()
                .filter(|probe| probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none())
                .filter_map(|probe| {
                    let net = circuit.nets.iter().position(|net| net.name == probe.of.target)?;
                    Some((net as u32, probe.of.target.clone(), probe.label.clone(), probe.at.clone()))
                })
                .collect();
            probes.key = Some(key);
        }
        // The tags' places: once per layout (settled, so a tag follows its pin in flight).
        let places_key = (Arc::as_ptr(&self.scene.hits) as usize, self.legible.to_bits(), key);
        if probes.places_key != Some(places_key) {
            let start = Cx::monotonic_now();
            self.scene.obstacles(f64::INFINITY, &mut probes.obstacles);
            let settled: Vec<_> = probes.probes.iter().map(|(net, _, label, at)| {
                let place = at.as_deref().unwrap_or(label);
                self.scene.probe_anchor(&unit.circuit, *net, place, f64::INFINITY, &mut probes.pins).map(|anchor| (anchor, tag_size(label)))
            }).collect();
            let tags: Vec<_> = settled.iter().flatten().copied().collect();
            let mut boxes = place_tags(&tags, &probes.obstacles, &[], TAG_GAP).into_iter();
            probes.places = settled.iter().map(|tag| {
                let (anchor, _) = (*tag)?;
                Some(moved(boxes.next()?, [-anchor.at[0], -anchor.at[1]]))
            }).collect();
            probes.places_key = Some(places_key);
            let ms = (Cx::monotonic_now() - start) * 1e3;
            if ms >= 4.0 {
                log!("fab.explorer: {} probe tags placed among {} obstacles in {ms:.2} ms", tags.len(), probes.obstacles.len());
            }
        }
        let gesture = cx.get_global_ref::<PeekGesture>();
        // A flag wears its probe's hue, the one its lanes wear; before the Bench has the lanes, its
        // place among the probes.
        let hue = |name: &str, i: usize| gesture.map_or(i, |gesture| gesture.hue_of(name, i));
        let mut flags = Vec::with_capacity(probes.probes.len() + 1);
        let mut rects = Vec::with_capacity(probes.probes.len() + 1);
        for (i, (net, name, label, at)) in probes.probes.iter().enumerate() {
            let place = at.as_deref().unwrap_or(label);
            let Some(anchor) = self.scene.probe_anchor(&unit.circuit, *net, place, now, &mut probes.pins) else { continue };
            let Some(Some(tag)) = probes.places.get(i) else { continue };
            rects.push(moved(*tag, anchor.at));
            flags.push(Flag { net: name, label, hue: hue(name, i), anchor, ghost: false });
        }
        // A ghost is placed alone, clear of the drawing and of the tags.
        let ghost_at = |anchor: Anchor, label: &str, rects: &[[f64; 4]]| {
            place_tags(&[(anchor, tag_size(label))], &probes.obstacles, rects, TAG_GAP)[0]
        };
        if let Some((net, anchor)) = &landing {
            if let Some(i) = flags.iter().position(|flag| flag.net == net) {
                rects.push(ghost_at(*anchor, flags[i].label, &rects));
                flags.push(Flag { anchor: *anchor, ghost: true, ..flags[i].clone() });
            }
        }
        if let Some(ghost) = &self.flag_ghost {
            let hue = gesture.map_or(probes.probes.len(), |gesture| gesture.hue_of(&ghost.name, probes.probes.len()));
            rects.push(ghost_at(ghost.anchor, &ghost.label, &rects));
            flags.push(Flag { net: &ghost.name, label: &ghost.label, hue, anchor: ghost.anchor, ghost: true });
        }
        let points = text_points(sheet::TEXT_NET, self.pane.cam.px_per_unit);
        self.flags.draw_behind(cx, &self.pane.cam, rect, &flags, &rects, points, TEXT_MIN_POINTS);
    }

    /// The sheet's probe tags' names over its drawing, and a flag carried along its net riding the
    /// pointer, where [`Self::draw_flags`] put them, in the pane `rect`.
    pub(super) fn draw_flag_labels(&mut self, cx: &mut Cx2d, rect: Rect) {
        self.flags.draw_labels(cx, rect);
    }
}

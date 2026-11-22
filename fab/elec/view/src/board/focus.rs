//! Board focus bounds and camera glides; geometry is visited once per focus, glides are O(1).

use crate::canvas::camera::Camera2d;
use fab_elec_board::{Area, Board, Placed};
use fab_elec_core::{BBox, FeatureRef, Nm, Sel};
use fab_elec_design::Design;
use fab_elec_parts::{Pad, PadShape};

/// The selection's bounds on this board in mm; O(visited geometry + pin mapping).
pub fn focus_box(board: &Board, design: Option<&Design>, sel: &Sel) -> Option<[f64; 4]> {
    if sel.unit() != Some(board.unit) { return None; }
    let bbox = match sel {
        Sel::Item(_, id) | Sel::Feature(FeatureRef::Hole(_, id)) => board.item(*id)?.bbox(board),
        Sel::Comp(c) | Sel::Feature(FeatureRef::Part(c) | FeatureRef::Body(c) | FeatureRef::Port(c, _)) => {
            board.placed_of(c.comp)?.bbox()
        }
        Sel::Feature(FeatureRef::Pad(c, pad)) => {
            let placed = board.placed_of(c.comp)?;
            placed.footprint.pads.get(pad.ix()).map(|pad| pad_box(placed, pad)).unwrap_or_else(|| placed.bbox())
        }
        Sel::Pin(p) => {
            let placed = board.placed_of(p.comp)?;
            if let Some(design) = design {
                let comp = design.unit(board.unit)?.circuit.comps.iter().find(|comp| comp.id == p.comp)?;
                comp.part.pins.get(p.pin.ix())?;
                let mut bbox = BBox::EMPTY;
                for (pad, pin) in placed.footprint.pads.iter().zip(comp.part.pad_pins()) {
                    if pin == Some(p.pin) { bbox = bbox.union(pad_box(placed, pad)); }
                }
                bbox
            } else {
                placed.bbox()
            }
        }
        Sel::Net(n) => {
            board.nets.get(n.net.ix())?;
            let mut bbox = BBox::EMPTY;
            for track in &board.tracks {
                if track.net == n.net { bbox = bbox.union(track.bbox()); }
            }
            for via in &board.vias {
                if via.net == n.net {
                    bbox = bbox.union(BBox::point(via.at).inflate(via.pad.max(via.drill) / 2 + 1));
                }
            }
            for placed in &board.placed {
                for (pad, net) in placed.footprint.pads.iter().zip(&placed.pad_nets) {
                    if *net == Some(n.net) { bbox = bbox.union(pad_box(placed, pad)); }
                }
            }
            if bbox.is_empty() {
                let outline = board.outline_bbox();
                for zone in &board.zones {
                    if zone.net == Some(n.net) {
                        bbox = bbox.union(match &zone.area { Area::Board => outline, Area::Poly(poly) => poly.bbox() });
                    }
                }
            }
            bbox
        }
        Sel::Unit(_) => board.outline_bbox(),
        _ => return None,
    };
    if bbox.is_empty() { return None; }
    Some([f64::from(bbox.min.x) / 1e6, f64::from(bbox.min.y) / 1e6,
        f64::from(bbox.max.x) / 1e6, f64::from(bbox.max.y) / 1e6])
}

/// A pad-centred square covering its largest stack size; O(custom shape geometry).
fn pad_box(placed: &Placed, pad: &Pad) -> BBox {
    let radius = std::iter::once(&pad.stack.front).chain(pad.stack.inner.iter()).chain(pad.stack.back.iter())
        .map(pad_radius).max().unwrap_or(0);
    BBox::point(placed.to_board(pad.at)).inflate(radius)
}

/// Half the larger pad size, rounded outwards; O(custom shape geometry), O(1) for primitives.
fn pad_radius(shape: &PadShape) -> Nm {
    let size = match shape {
        PadShape::Circle { d } => *d,
        PadShape::Rect { size } | PadShape::RoundRect { size, .. } | PadShape::Chamfer { size, .. }
        | PadShape::Oval { size } | PadShape::Trapezoid { size, .. } => size.x.max(size.y),
        PadShape::Custom { anchor, shapes } => {
            return shapes.iter().fold(pad_radius(anchor), |r, shape| {
                let b = shape.bbox();
                if b.is_empty() { return r; }
                r.max(b.min.x.saturating_abs()).max(b.min.y.saturating_abs())
                    .max(b.max.x.saturating_abs()).max(b.max.y.saturating_abs())
            });
        }
    };
    size / 2 + size % 2
}

/// Keep a target already in plain sight (wholly on screen with a 24 pt margin, at least 1/12 of
/// the shorter side); otherwise centre it at the current zoom when it fits there, else fit it
/// with the zoom bounded so it fills at most 1/3 of the shorter side. Centring, not the smallest
/// pan, keeps a finding's labels and its neighbours in view. O(1).
pub fn focus_camera(cam: &Camera2d, bounds: [f64; 4]) -> Camera2d {
    let w = bounds[2] - bounds[0];
    let h = bounds[3] - bounds[1];
    let extent = w.max(h);
    let short = cam.view.size.x.min(cam.view.size.y);
    if extent * cam.px_per_unit >= short / 12.0
        && w * cam.px_per_unit <= cam.view.size.x - 48.0
        && h * cam.px_per_unit <= cam.view.size.y - 48.0
    {
        let half = [(cam.view.size.x * 0.5 - 24.0) / cam.px_per_unit,
            (cam.view.size.y * 0.5 - 24.0) / cam.px_per_unit];
        let inside = bounds[0] >= cam.center[0] - half[0] && bounds[2] <= cam.center[0] + half[0]
            && bounds[1] >= cam.center[1] - half[1] && bounds[3] <= cam.center[1] + half[1];
        if inside {
            return *cam;
        }
        return Camera2d { center: [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5], ..*cam };
    }
    let mut to = Camera2d::fit(cam.view, cam.dpi, bounds, 24.0);
    if extent > 0.0 && short > 0.0 { to.px_per_unit = to.px_per_unit.min(short / (3.0 * extent)); }
    to.dpi = cam.dpi;
    to
}

/// An ease-out camera glide with logarithmic zoom; O(1) storage and sampling.
pub struct Glide {
    from: Camera2d,
    to: Camera2d,
    start: f64,
    dur: f64,
}

impl Glide {
    /// Start a 0.32 s glide, immediate for equal cameras; O(1).
    pub fn new(from: Camera2d, to: Camera2d, now: f64) -> Glide {
        Glide { from, to, start: now, dur: if from == to { 0.0 } else { 0.32 } }
    }

    /// Sample cubic ease-out, using the destination's view and dpi; O(1).
    pub fn at(&self, now: f64) -> Camera2d {
        if self.dur == 0.0 || self.done(now) { return self.to; }
        if now <= self.start { return Camera2d { view: self.to.view, dpi: self.to.dpi, ..self.from }; }
        let t = ((now - self.start) / self.dur).clamp(0.0, 1.0);
        let e = 1.0 - (1.0 - t).powi(3);
        let lerp = |a: f64, b: f64| a + (b - a) * e;
        Camera2d {
            center: [lerp(self.from.center[0], self.to.center[0]), lerp(self.from.center[1], self.to.center[1])],
            px_per_unit: lerp(self.from.px_per_unit.ln(), self.to.px_per_unit.ln()).exp(),
            view: self.to.view,
            dpi: self.to.dpi,
        }
    }

    /// Whether the destination has been reached at `now` in seconds; O(1).
    pub fn done(&self, now: f64) -> bool {
        now >= self.start + self.dur
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{BoardId, CompRef, NetRef, PadIx, PinIx, PinRef, Side};
    use makepad_widgets_core::{dvec2, Rect};

    #[test]
    fn fixture_focus_bounds_camera_and_glide() {
        let design = fab_elec_fixtures::ledctl();
        let unit = design.units.first().expect("ledctl has a first unit");
        let board = unit.board.as_deref().expect("ledctl's first unit has a board");
        let mm = |b: BBox| [f64::from(b.min.x) / 1e6, f64::from(b.min.y) / 1e6,
            f64::from(b.max.x) / 1e6, f64::from(b.max.y) / 1e6];
        let contains = |a: [f64; 4], b: [f64; 4]| a[0] <= b[0] && a[1] <= b[1] && a[2] >= b[2] && a[3] >= b[3];
        let track = board.tracks.first().expect("fixture has a track");
        let item = board.item(track.id).expect("fixture track has an item");
        assert_eq!(focus_box(board, Some(&design), &Sel::Item(board.unit, track.id)), Some(mm(item.bbox(board))),
            "item focus uses the item's bounds in mm");
        assert_eq!(focus_box(board, None, &Sel::Unit(board.unit)), Some(mm(board.outline_bbox())),
            "unit focus uses the board outline");

        let placed = board.placed.iter().find(|p| p.side == Side::Bottom).expect("fixture has a bottom placement");
        let c = CompRef { board: board.unit, comp: placed.comp };
        let comp_box = focus_box(board, Some(&design), &Sel::Comp(c)).expect("placed component has focus bounds");
        for (i, pad) in placed.footprint.pads.iter().enumerate() {
            let bounds = focus_box(board, Some(&design), &Sel::Feature(FeatureRef::Pad(c, PadIx(i as u16))))
                .expect("placed pad has focus bounds");
            assert!(contains(comp_box, bounds), "component focus contains pad {i}");
            assert!(contains(mm(placed.bbox()), bounds), "pad {i} focus stays inside its placement");
            assert!(contains(bounds, mm(BBox::point(placed.to_board(pad.at)))), "pad {i} focus contains its board centre");
        }
        assert_eq!(focus_box(board, None, &Sel::Feature(FeatureRef::Pad(c, PadIx(u16::MAX)))), Some(comp_box),
            "an absent pad index falls back to the placement");
        let (pin_placed, part, pin) = board.placed.iter().find_map(|placed| {
            let comp = unit.circuit.comps.iter().find(|comp| comp.id == placed.comp)?;
            let pin = comp.part.pins.iter().position(|pin| pin.pads.len() > 1)?;
            Some((placed, &comp.part, PinIx(pin as u16)))
        }).expect("fixture has a pin mapped to multiple pads");
        let pin_sel = Sel::Pin(PinRef { board: board.unit, comp: pin_placed.comp, pin });
        let mut pin_box = BBox::EMPTY;
        let pads = part.pads_of(pin);
        assert!(pads.len() > 1, "pin focus exercises multiple mapped pads");
        for i in pads {
            let pad = pin_placed.footprint.pads.get(i.ix()).expect("fixture pin's pad is placed");
            pin_box = pin_box.union(pad_box(pin_placed, pad));
        }
        assert_eq!(focus_box(board, Some(&design), &pin_sel), Some(mm(pin_box)), "pin focus unions its mapped pads");
        assert_eq!(focus_box(board, None, &pin_sel), Some(mm(pin_placed.bbox())), "pin focus without a design uses its placement");

        let net_box = focus_box(board, Some(&design), &Sel::Net(NetRef { board: board.unit, net: track.net }))
            .expect("routed net has focus bounds");
        for t in board.tracks.iter().filter(|t| t.net == track.net) {
            assert!(contains(net_box, mm(t.bbox())), "net focus contains track {:?}", t.id);
        }
        let zone = board.zones.first().expect("fixture has a zone");
        let net = zone.net.expect("fixture zone has a net");
        let net_sel = Sel::Net(NetRef { board: board.unit, net });
        let mut copper = BBox::EMPTY;
        for t in board.tracks.iter().filter(|t| t.net == net) { copper = copper.union(t.bbox()); }
        for via in board.vias.iter().filter(|v| v.net == net) {
            copper = copper.union(board.item(via.id).expect("fixture via has an item").bbox(board));
        }
        for p in &board.placed {
            for (pad, pad_net) in p.footprint.pads.iter().zip(&p.pad_nets) {
                if *pad_net == Some(net) { copper = copper.union(pad_box(p, pad)); }
            }
        }
        assert_eq!(focus_box(board, None, &net_sel), Some(mm(copper)), "copper net focus excludes its board-wide zone");
        let mut zone_only = board.clone();
        zone_only.tracks.clear();
        zone_only.vias.clear();
        for p in &mut zone_only.placed { p.pad_nets.fill(None); }
        assert_eq!(focus_box(&zone_only, None, &net_sel), Some(mm(board.outline_bbox())),
            "a net without tracks, vias or pads falls back to its zone");
        let other = BoardId(board.unit.0.wrapping_add(1));
        for sel in [Sel::Unit(other), Sel::Item(other, track.id), Sel::Comp(CompRef { board: other, ..c }),
            Sel::Net(NetRef { board: other, net: track.net })]
        {
            assert_eq!(focus_box(board, Some(&design), &sel), None, "a focus from another unit has no box here: {sel:?}");
        }

        let cam = Camera2d { center: [0.0, 0.0], px_per_unit: 10.0,
            view: Rect { pos: dvec2(17.0, 29.0), size: dvec2(800.0, 600.0) }, dpi: 2.0 };
        assert_eq!(focus_camera(&cam, [-5.0, -5.0, 5.0, 5.0]), cam, "a clearly visible box keeps the camera");
        let pan = focus_camera(&cam, [35.0, -2.0, 45.0, 2.0]);
        assert_eq!(pan.px_per_unit, cam.px_per_unit, "an off-screen box that fits keeps the zoom");
        assert_eq!(pan.center, [40.0, 0.0], "an off-screen box that fits is centred");
        assert_eq!(focus_camera(&cam, [10.0, -2.0, 20.0, 2.0]), cam,
            "a box wholly inside the 24 pt margin keeps the camera");
        assert_eq!(focus_camera(&cam, [30.0, -2.0, 40.0, 2.0]).center, [35.0, 0.0],
            "a box crossing the margin is centred");
        let pad = focus_box(board, None, &Sel::Feature(FeatureRef::Pad(c, PadIx(0)))).expect("first pad focus");
        let to = focus_camera(&cam, pad);
        let extent = (pad[2] - pad[0]).max(pad[3] - pad[1]);
        assert!(to.px_per_unit > cam.px_per_unit, "a small pad is enlarged for focus");
        assert!(to.px_per_unit * extent <= cam.view.size.x.min(cam.view.size.y) / 3.0 + 1e-10,
            "a pad never fills more than one third of the shorter view side");
        assert_eq!(to.center, [(pad[0] + pad[2]) * 0.5, (pad[1] + pad[3]) * 0.5], "zooming centres the pad");
        assert_eq!((to.view, to.dpi), (cam.view, cam.dpi), "focus preserves the view and dpi");

        let start = 10.0;
        let glide = Glide::new(cam, to, start);
        assert_eq!(glide.dur, 0.32, "a moving glide lasts 0.32 seconds");
        assert_eq!(glide.at(start), cam, "glide starts exactly at its source");
        assert_eq!(glide.at(start + glide.dur), to, "glide ends exactly at its destination");
        assert!(!glide.done(start) && !glide.done(start + glide.dur - 1e-6) && glide.done(start + glide.dur),
            "glide completion changes at start plus duration");
        let mid = glide.at(start + glide.dur * 0.5);
        for i in 0..2 {
            assert!((mid.center[i] - (cam.center[i] + (to.center[i] - cam.center[i]) * 0.875)).abs() < 1e-10,
                "glide centre axis {i} follows cubic ease-out");
        }
        assert!((mid.px_per_unit.ln() - (cam.px_per_unit.ln() * 0.125 + to.px_per_unit.ln() * 0.875)).abs() < 1e-10,
            "glide zoom follows cubic ease-out in log space");
        let still = Glide::new(cam, cam, start);
        assert!(still.dur == 0.0 && still.done(start) && still.at(start) == cam, "equal cameras finish immediately");
    }
}

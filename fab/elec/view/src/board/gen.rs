//! A generated board for load and look proofs: a deterministic, board-like scene of footprints,
//! tracks with arcs, vias and holes over several copper layers, at any size (the 100 000
//! segment bar is `GenSpec::bar()`). It is a rendering load, not a routed design: nets are
//! plausible, clearance is not checked.

use fab_elec_board::{
    Anchor, Area, Board, BoardNet, Flash, Hole, Origin, Placed, StackKind, Stackup, Track, Via,
    Zone,
};
use fab_elec_circuit::NetClass;
use fab_elec_core::{
    mm, Angle, BoardId, ClassId, CompId, Contour, ItemId, LayerId, LayerSet, NetId,
    Nm, PadIx, Path, Seg, Side, P,
};
use fab_elec_design::fixture::fp;
use fab_elec_parts::{Footprint, FpMount, PadProp, PadShape};
use std::sync::Arc;

/// What to generate.
#[derive(Clone, Debug, PartialEq)]
pub struct GenSpec {
    pub width_mm: f64,
    pub height_mm: f64,
    /// Copper layers: 2, 4, 6 or 8.
    pub copper_layers: u8,
    /// Track segments (lines and arcs) to reach, at least.
    pub segments: usize,
    pub seed: u64,
}

impl GenSpec {
    /// The E1 3.3 bar: 100 000 track segments, about 20 000 pads, 8 copper layers on 200 x 150 mm.
    pub fn bar() -> Self {
        GenSpec { width_mm: 200.0, height_mm: 150.0, copper_layers: 8, segments: 100_000, seed: 1 }
    }
}

/// The generated board (module docs). Pure and deterministic in `spec`.
pub fn generated_board(spec: &GenSpec) -> Board {
    build(spec)
}

/// xorshift64*: a private stream, including a nonzero state for seed zero.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9e37_79b9_7f4a_7c15 } else { seed })
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next() >> 11) as f64 / (1u64 << 53) as f64)
    }
}

fn stackup(count: u8) -> Stackup {
    let mut stack = Stackup::standard_2_layer();
    let copper = stack.layers[2].clone();
    let dielectric = stack.layers[3].clone();
    let total = stack.thickness - mm(0.02) - copper.thickness * count as Nm;
    let gaps = count as Nm - 1;
    let mut middle = Vec::with_capacity(2 * count as usize - 3);
    for gap in 0..count - 1 {
        let mut layer = dielectric.clone();
        layer.thickness = total / gaps + ((gap as Nm) < total % gaps) as Nm;
        middle.push(layer);
        if gap + 2 < count {
            let mut layer = copper.clone();
            layer.kind = StackKind::Copper(LayerId::inner(gap + 1));
            middle.push(layer);
        }
    }
    drop(stack.layers.splice(3..4, middle));
    stack
}

fn ball_array(n: usize, pitch: f64) -> Footprint {
    let half = (n - 1) as f64 * pitch / 2.0;
    let diameter = mm(pitch * 0.5);
    let mut pads = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            let mut pad = fp::smd(
                &(y * n + x + 1).to_string(),
                P::mm(x as f64 * pitch - half, y as f64 * pitch - half),
                P::new(diameter, diameter),
            );
            pad.stack.front = PadShape::Circle { d: diameter };
            pad.prop = PadProp::Bga;
            pads.push(pad);
        }
    }
    fp::finish(
        &format!("BGA-{}-{pitch:.1}", n * n),
        pads,
        P::mm(-half - 0.5, -half - 0.5),
        P::mm(half + 0.5, half + 0.5),
        FpMount::Smd,
        mm(0.8),
    )
}

fn footprints() -> Vec<Arc<Footprint>> {
    // The fixture's chip helper accepts only 0402 and 0603. Use its same
    // construction for 0805, without extending the shared fixture API.
    let chip_0805 = fp::finish(
        "0805",
        vec![
            fp::smd("1", P::mm(-1.0, 0.0), P::mm(1.1, 1.4)),
            fp::smd("2", P::mm(1.0, 0.0), P::mm(1.1, 1.4)),
        ],
        P::mm(-1.0, -0.625),
        P::mm(1.0, 0.625),
        FpMount::Smd,
        mm(0.6),
    );
    let mut all = vec![
        fp::chip("0402", "0402"),
        fp::chip("0603", "0603"),
        chip_0805,
        fp::sot23_5("SOT23-5"),
    ];
    for pins in (14..=28).step_by(2) {
        // a 4.4 mm body, half a pitch of margin past the end pins
        all.push(fp::tssop(&format!("TSSOP-{pins}"), pins, 0.65, ((pins / 2) as f64 * 0.65 + 0.35, 4.4)));
    }
    for pins in 4..=10 {
        all.push(fp::pin_header(&format!("HEADER-{pins}"), pins, 1.27));
    }
    for n in 8..=16 {
        for pitch in [0.8, 1.0] {
            all.push(ball_array(n, pitch));
        }
    }
    all.into_iter().map(Arc::new).collect()
}

#[derive(Clone, Copy)]
struct PadSite {
    placed: usize,
    pad: PadIx,
    at: P,
    layers: LayerSet,
}

struct Builder {
    board: Board,
    rng: Rng,
    size: P,
    copper: Vec<LayerId>,
    segments: usize,
}

impl Builder {
    fn id(&mut self) -> ItemId {
        let id = ItemId(self.board.next_item);
        self.board.next_item = self.board.next_item.checked_add(1)
            .expect("generated board exhausted its item ids");
        id
    }

    fn net(&mut self, name: String, class: ClassId) -> NetId {
        let id = NetId(u32::try_from(self.board.nets.len())
            .expect("generated board exhausted its net ids"));
        self.board.nets.push(BoardNet { name, class });
        id
    }

    fn layer(&mut self) -> LayerId {
        // Each outer layer has two tickets, each inner layer one.
        match self.rng.pick(self.copper.len() + 2) {
            0 | 1 => LayerId::F_CU,
            2 | 3 => LayerId::B_CU,
            n => self.copper[n - 3],
        }
    }

    fn track(&mut self, net: NetId, layer: LayerId, w: Nm, path: Path, ends: [Anchor; 2]) {
        let id = self.id();
        self.segments += path.segs.len();
        self.board.tracks.push(Track {
            id, net, layer, w, widths: None, path, ends,
            locked: false, origin: Origin::Authored,
        });
    }

    fn via(&mut self, net: NetId, at: P) -> Anchor {
        let id = self.id();
        self.board.vias.push(Via {
            id, net, at, drill: mm(0.3), pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: Default::default(), tent: Default::default(), backdrill: None,
            locked: false, origin: Origin::Authored,
            flash: Flash::All,
        });
        Anchor::Via(id)
    }

    fn placements(&mut self, target: usize) {
        let library = footprints();
        let mut pads = 0;
        while pads < target {
            let index = self.board.placed.len();
            // Spread the large packages through the grid instead of clustering
            // them in consecutive cells. Share immutable footprint definitions.
            let choice = match index % 16 {
                3 | 11 => 19 + self.rng.pick(18),
                5 | 13 => 4 + self.rng.pick(8),
                9 => 12 + self.rng.pick(7),
                1 | 7 | 15 => 3,
                _ => self.rng.pick(3),
            };
            let footprint = library[choice].clone();
            let id = self.id();
            let mut placed = Placed {
                id,
                comp: CompId(u32::try_from(index).expect("generated component index fits u32")),
                path: format!("generated/part{index}"),
                refdes: format!("{}{}", if choice < 3 { "R" } else if choice < 12 { "U" } else if choice < 19 { "J" } else { "U" }, index + 1),
                pad_nets: vec![None; footprint.pads.len()],
                footprint,
                at: P::ZERO,
                rot: Angle::deg((self.rng.pick(4) * 90) as f64 + if self.rng.pick(20) == 0 { 45.0 } else { 0.0 }),
                side: if self.rng.pick(100) < 15 { Side::Bottom } else { Side::Top },
                locked: false,
                origin: Origin::Authored,
                // Generated footprints come without a part, so without a body.
                body_z: None,
                ref_label: None, value_label: None, legend: None,
                value: None,
            };
            let bounds = placed.bbox();
            if bounds.width() > (self.size.x - mm(2.0)) as i64
                || bounds.height() > (self.size.y - mm(2.0)) as i64
            {
                // Very small boards still get the requested load, using parts
                // that fit instead of putting their pads beyond the outline.
                placed.footprint = library[0].clone();
                placed.pad_nets = vec![None; 2];
            }
            pads += placed.footprint.pads.len();
            self.board.placed.push(placed);
        }
        let count = self.board.placed.len();
        let cols = ((count as f64 * self.size.x as f64 / self.size.y as f64).sqrt().ceil() as usize).max(1);
        let rows = count.div_ceil(cols);
        let dx = (self.size.x - mm(2.0)) as f64 / cols as f64;
        let dy = (self.size.y - mm(2.0)) as f64 / rows as f64;
        for (index, placed) in self.board.placed.iter_mut().enumerate() {
            let bounds = placed.bbox();
            let center = bounds.center();
            let at = P::from_f(
                mm(1.0) as f64 + (index % cols) as f64 * dx + self.rng.range(0.28, 0.72) * dx,
                mm(1.0) as f64 + (index / cols) as f64 * dy + self.rng.range(0.28, 0.72) * dy,
            ).sub(center);
            placed.at = P::new(
                at.x.clamp(mm(1.0) - bounds.min.x, self.size.x - mm(1.0) - bounds.max.x),
                at.y.clamp(mm(1.0) - bounds.min.y, self.size.y - mm(1.0) - bounds.max.y),
            );
        }
    }

    fn route(&mut self, net: NetId, a: PadSite, b: PadSite) {
        let layer = self.layer();
        let pad_layer = |p: PadSite| {
            if p.layers.has(layer) { layer }
            else if p.layers.has(LayerId::F_CU) { LayerId::F_CU }
            else { LayerId::B_CU }
        };
        let start_layer = pad_layer(a);
        let end_layer = pad_layer(b);
        let w = mm(if net.0 < 2 { self.rng.range(0.5, 1.0) } else { self.rng.range(0.1, 0.4) });
        let start = Anchor::Pad { placed: self.board.placed[a.placed].id, pad: a.pad };
        let end = Anchor::Pad { placed: self.board.placed[b.placed].id, pad: b.pad };
        let mut from = a.at;
        let mut to = b.at;
        let mut anchors = [start, end];
        if start_layer != layer {
            from = interpolate(a.at, b.at, 0.2);
            anchors[0] = self.via(net, from);
            let path = self.route_path(a.at, from);
            self.track(net, start_layer, w, path, [start, anchors[0]]);
        }
        if end_layer != layer {
            to = interpolate(a.at, b.at, 0.8);
            anchors[1] = self.via(net, to);
            let path = self.route_path(to, b.at);
            self.track(net, end_layer, w, path, [anchors[1], end]);
        }
        let path = self.route_path(from, to);
        self.track(net, layer, w, path, anchors);
    }

    fn route_path(&mut self, a: P, b: P) -> Path {
        let mut points = vec![a];
        let waypoints = self.rng.pick(3);
        for i in 0..waypoints {
            let at = interpolate(a, b, (i + 1) as f64 / (waypoints + 1) as f64);
            let offset = P::mm(self.rng.range(-1.5, 1.5), self.rng.range(-1.5, 1.5));
            let at = at.add(offset);
            let at = P::new(
                at.x.clamp(mm(1.0), self.size.x - mm(1.0)),
                at.y.clamp(mm(1.0), self.size.y - mm(1.0)),
            );
            octile(&mut points, at, self.rng.pick(2) == 0);
        }
        octile(&mut points, b, self.rng.pick(2) == 0);
        if points.len() == 1 {
            let x = if a.x + mm(0.8) < self.size.x - mm(1.0) { a.x + mm(0.8) } else { a.x - mm(0.8) };
            points.push(P::new(x, a.y));
            points.push(b);
        } else if points.len() == 2 {
            points.insert(1, interpolate(a, b, 0.5));
        }
        rounded_path(&points, &mut self.rng)
    }

    fn connections(&mut self) {
        let pad_count: usize = self.board.placed.iter().map(|p| p.footprint.pads.len()).sum();
        // The cell count also stays O(pads) for a sparse board with a large area.
        let cell = ((self.size.x as f64 * self.size.y as f64 / pad_count as f64).sqrt() * 2.0)
            .max(mm(4.0) as f64);
        let nx = (self.size.x as f64 / cell).ceil() as usize;
        let ny = (self.size.y as f64 / cell).ceil() as usize;
        let mut buckets = vec![Vec::new(); nx * ny];
        let copper = LayerSet::of(&self.copper);
        for (placed, part) in self.board.placed.iter().enumerate() {
            for (index, pad) in part.footprint.pads.iter().enumerate() {
                if self.rng.pick(3) == 0 { continue; }
                let at = part.to_board(pad.at);
                let pad = PadIx(index as u16);
                let x = (at.x as f64 / cell) as usize;
                let y = (at.y as f64 / cell) as usize;
                buckets[y.min(ny - 1) * nx + x.min(nx - 1)].push(PadSite {
                    placed, pad, at,
                    layers: part.pad_layers(pad, copper, self.copper.len() as u8),
                });
            }
        }
        let mut group = Vec::with_capacity(6);
        for cell_index in 0..buckets.len() {
            let x = cell_index % nx;
            let y = cell_index / nx;
            while let Some(first) = buckets[cell_index].pop() {
                group.clear();
                group.push(first);
                let count = 2 + self.rng.pick(5);
                while group.len() < count {
                    let mut nearest = None;
                    // Inspect only each bucket's last pad, never rescan its
                    // contents. Every pad is consumed once; nine probes per pick.
                    for cy in y.saturating_sub(1)..=(y + 1).min(ny - 1) {
                        for cx in x.saturating_sub(1)..=(x + 1).min(nx - 1) {
                            let index = cy * nx + cx;
                            if let Some(pad) = buckets[index].last() {
                                let distance = first.at.dist2(pad.at);
                                if nearest.is_none_or(|(_, best)| distance < best) {
                                    nearest = Some((index, distance));
                                }
                            }
                        }
                    }
                    let Some((index, _)) = nearest else { break };
                    group.push(buckets[index].pop().expect("selected bucket contains a pad"));
                }
                if group.len() < 2 { continue; }
                let net = match self.rng.pick(8) {
                    0 => NetId(0),
                    1 => NetId(1),
                    _ => self.net(format!("N{}", self.board.nets.len()), ClassId(0)),
                };
                for pad in &group {
                    self.board.placed[pad.placed].pad_nets[pad.pad.ix()] = Some(net);
                }
                for pair in group.windows(2) {
                    self.route(net, pair[0], pair[1]);
                }
            }
        }
    }

    fn bus(&mut self) {
        let lanes = 4 + self.rng.pick(13);
        let vertical = self.rng.pick(2) == 0;
        let (along, across) = if vertical { (self.size.y, self.size.x) } else { (self.size.x, self.size.y) };
        let spacing = mm(0.25);
        let band = (lanes - 1) as Nm * spacing;
        let run = mm(self.rng.range(10.0, 60.0)).min(along - mm(2.0));
        let bend = mm(self.rng.range(1.0, 4.0))
            .min((across - band - mm(2.0)) / 2).min(run / 8);
        // Account for the two diagonal runs in the requested centreline length.
        let length = run - (2.0 * bend as f64 * (std::f64::consts::SQRT_2 - 1.0)).round() as Nm;
        let x = self.rng.range(mm(1.0) as f64, (along - mm(1.0) - length) as f64).round() as Nm;
        let y = self.rng.range(mm(1.0) as f64, (across - mm(1.0) - band - bend) as f64).round() as Nm;
        let layer = self.layer();
        let width = mm(self.rng.range(0.1, 0.2));
        for lane in 0..lanes {
            let d = lane as Nm * spacing;
            // Offset each 45-degree section by its normal as well as the
            // horizontal sections: the parallel centreline spacing is 0.25 mm.
            let inset = ((std::f64::consts::SQRT_2 - 1.0) * d as f64).round() as Nm;
            let points = [
                P::new(x, y + d),
                P::new(x + length / 3 - inset, y + d),
                P::new(x + length / 3 + bend - inset, y + bend + d),
                P::new(x + length * 2 / 3 - bend + inset, y + bend + d),
                P::new(x + length * 2 / 3 + inset, y + d),
                P::new(x + length, y + d),
            ].map(|p| if vertical { P::new(p.y, p.x) } else { p });
            // Keep bus bends sharp so every lane remains a true parallel offset.
            // The pad-to-pad routes supply the circular fillets.
            let path = Path { start: points[0], segs: points[1..].iter().copied().map(Seg::Line).collect() };
            let net = self.net(format!("BUS{}", self.board.nets.len()), ClassId(0));
            self.track(net, layer, width, path, [Anchor::Free; 2]);
        }
    }
}

fn interpolate(a: P, b: P, fraction: f64) -> P {
    P::from_f(a.x as f64 + (b.x - a.x) as f64 * fraction, a.y as f64 + (b.y - a.y) as f64 * fraction)
}

/// Append an axis-aligned run and a 45-degree run, in either order.
fn octile(points: &mut Vec<P>, end: P, diagonal_first: bool) {
    let start = *points.last().expect("a generated route has its start point");
    let delta = end.sub(start);
    let d = delta.x.abs().min(delta.y.abs());
    let diagonal = P::new(delta.x.signum() * d, delta.y.signum() * d);
    let corner = if diagonal_first { start.add(diagonal) } else { end.sub(diagonal) };
    if corner != start && corner != end { points.push(corner); }
    if end != start { points.push(end); }
}

/// Tangent circular fillets. Reserve at most 45% of each adjoining run so
/// adjacent fillets cannot consume one another, and keep a path at 2..=8 segments.
fn rounded_path(points: &[P], rng: &mut Rng) -> Path {
    let mut path = Path::new(points[0]);
    let mut extra = 8usize.saturating_sub(points.len() - 1);
    for i in 1..points.len() - 1 {
        let corner = points[i];
        let incoming = corner.sub(points[i - 1]);
        let outgoing = points[i + 1].sub(corner);
        let la = incoming.len();
        let lb = outgoing.len();
        if extra > 0 && rng.pick(6) == 0 && la > 0.0 && lb > 0.0 {
            let (ux, uy) = (incoming.x as f64 / la, incoming.y as f64 / la);
            let (vx, vy) = (outgoing.x as f64 / lb, outgoing.y as f64 / lb);
            let cos = (ux * vx + uy * vy).clamp(-1.0, 1.0);
            if cos.abs() < 0.999_999 {
                let tan_half = ((1.0 - cos) / (1.0 + cos)).sqrt();
                let limit = (0.45 * la.min(lb) / tan_half).min(mm(2.0) as f64);
                if limit >= mm(0.2) as f64 {
                    let r = rng.range(mm(0.2) as f64, limit);
                    let trim = r * tan_half;
                    let (ax, ay) = (corner.x as f64 - ux * trim, corner.y as f64 - uy * trim);
                    let (bx, by) = (corner.x as f64 + vx * trim, corner.y as f64 + vy * trim);
                    let sign = (ux * vy - uy * vx).signum();
                    let (cx, cy) = (ax - uy * r * sign, ay + ux * r * sign);
                    let (mx, my) = ((ax + bx) * 0.5 - cx, (ay + by) * 0.5 - cy);
                    let scale = r / mx.hypot(my);
                    path.segs.push(Seg::Line(P::from_f(ax, ay)));
                    path.segs.push(Seg::Arc {
                        mid: P::from_f(cx + mx * scale, cy + my * scale),
                        end: P::from_f(bx, by),
                    });
                    extra -= 1;
                    continue;
                }
            }
        }
        path.segs.push(Seg::Line(corner));
    }
    path.segs.push(Seg::Line(*points.last().expect("a generated route has an end point")));
    path
}

fn build(spec: &GenSpec) -> Board {
    assert!(matches!(spec.copper_layers, 2 | 4 | 6 | 8), "generated board needs 2, 4, 6 or 8 copper layers");
    assert!(
        spec.width_mm.is_finite() && spec.height_mm.is_finite()
            && (8.0..=2000.0).contains(&spec.width_mm) && (8.0..=2000.0).contains(&spec.height_mm),
        "generated board dimensions must fit its mounting holes and the nanometre coordinate range (8..=2000 mm)",
    );
    let size = P::mm(spec.width_mm, spec.height_mm);
    let mut board = Board::new(BoardId(0), "Generated load board");
    board.stackup = stackup(spec.copper_layers);
    board.outline.push(Contour::rect(P::ZERO, size, mm(2.0)));
    board.classes = vec![
        NetClass { id: ClassId(0), name: "Default".into(), rules: Default::default() },
        NetClass { id: ClassId(1), name: "Power".into(), rules: Default::default() },
    ];
    board.classes[1].rules.track = Some(mm(0.75));
    let mut builder = Builder { copper: board.stackup.copper(), board, rng: Rng::new(spec.seed), size, segments: 0 };
    let gnd = builder.net("GND".into(), ClassId(1));
    builder.net("3V3".into(), ClassId(1));
    builder.placements(spec.segments.div_ceil(5).max(2));
    builder.connections();
    // Include a bus even when pad connections alone reach the segment target.
    loop {
        builder.bus();
        if builder.segments >= spec.segments { break; }
    }
    for (index, at) in [
        P::mm(4.0, 4.0),
        P::new(size.x - mm(4.0), mm(4.0)),
        P::new(size.x - mm(4.0), size.y - mm(4.0)),
        P::new(mm(4.0), size.y - mm(4.0)),
    ].into_iter().enumerate() {
        let id = builder.id();
        builder.board.holes.push(Hole {
            id, name: format!("MOUNT{}", index + 1), at,
            drill: mm(3.2), pad: Some(mm(6.0)), net: Some(gnd),
            plated: true, origin: Origin::Authored,
        });
    }
    for layer in [LayerId::F_CU, LayerId::B_CU] {
        let id = builder.id();
        builder.board.zones.push(Zone {
            id, net: Some(gnd), name: format!("GND-{}", layer.name()),
            layers: LayerSet::one(layer), area: Area::Board, priority: 0,
            settings: Default::default(), locked: false, origin: Origin::Authored,
        });
    }
    builder.board
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_load_invariants() {
        let spec = GenSpec { width_mm: 40.0, height_mm: 30.0, copper_layers: 4, segments: 2_000, seed: 7 };
        let board = generated_board(&spec);
        assert!(board.tracks.iter().map(|t| t.path.segs.len()).sum::<usize>() >= 2_000, "track segment target");
        assert!(board.placed.iter().map(|p| p.footprint.pads.len()).sum::<usize>() >= 300, "pad count target");
        let mut seen = vec![false; board.next_item as usize];
        let mut placements = vec![None; seen.len()];
        let mut vias = vec![None; seen.len()];
        let mut check_id = |id: ItemId| {
            assert!(id.0 < board.next_item, "item id below next_item: {id:?}");
            assert!(!seen[id.ix()], "unique item id: {id:?}");
            seen[id.ix()] = true;
        };
        for placed in &board.placed {
            check_id(placed.id);
            placements[placed.id.ix()] = Some(placed);
            assert_eq!(placed.pad_nets.len(), placed.footprint.pads.len(), "pad net table length");
        }
        for via in &board.vias {
            check_id(via.id);
            vias[via.id.ix()] = Some(via);
        }
        for zone in &board.zones { check_id(zone.id); }
        for hole in &board.holes { check_id(hole.id); }
        let copper = board.copper_set();
        assert_eq!(board.copper_count(), 4, "requested stackup copper count");
        for track in &board.tracks {
            check_id(track.id);
            assert!(track.layer.is_copper() && copper.has(track.layer), "track layer belongs to stackup copper");
            assert!((2..=8).contains(&track.path.segs.len()), "track path has 2 to 8 segments");
            for (anchor, end) in track.ends.into_iter().zip([track.path.start, track.path.end()]) {
                match anchor {
                    Anchor::Pad { placed, pad } => {
                        let part = placements.get(placed.ix()).copied().flatten();
                        assert!(part.is_some(), "pad anchor placement exists");
                        let part = part.expect("pad anchor placement exists");
                        assert!(part.footprint.pads.get(pad.ix()).is_some(), "pad anchor pad exists");
                        assert_eq!(part.pad_at(pad), Some(end), "pad anchor endpoint position");
                        assert!(part.pad_layers(pad, copper, 4).has(track.layer), "pad anchor copper layer");
                        assert_eq!(part.pad_nets[pad.ix()], Some(track.net), "pad anchor net agrees");
                    }
                    Anchor::Via(id) => {
                        let via = vias.get(id.ix()).copied().flatten();
                        assert!(via.is_some(), "via anchor exists");
                        let via = via.expect("via anchor exists");
                        assert_eq!(via.at, end, "via anchor endpoint position");
                        assert_eq!(via.net, track.net, "via anchor net agrees");
                    }
                    Anchor::Free => {}
                    _ => panic!("generated track uses only pad, via or free anchors"),
                }
            }
        }
        assert!(seen.iter().all(|used| *used), "next_item follows the last allocated item id");
        assert_eq!(board.generation, 0, "generated board starts at generation zero");
        assert_eq!(board, generated_board(&spec), "same seed produces equal boards");
        let other = GenSpec { seed: 8, ..spec };
        assert_ne!(board, generated_board(&other), "different seed produces a different board");
    }
}

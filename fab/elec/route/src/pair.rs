//! Single-layer differential routing. Stations belong to the envelope's centreline.
//! The only copper constructor, `legs`, offsets that same spine for both nets.
use crate::{verify, RItem, RPiece, World};
use fab_elec_board::{
    length, Anchor, Board, BoardOp, ConnItem, Connectivity, Copper, Item, ItemKind, Origin, RuleItem, RuleKey,
    RuleMatrix, Track,
};
use fab_elec_core::{
    canon_struct, canon_tags, Index, ItemId, LayerId, LayerSet, NetId, Nm, OnTrack, PadIx, Path, Seg, Shape, P,
};
use std::ops::ControlFlow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Anchor(Anchor),
    Point(P),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub net: NetId,
    pub from: Endpoint,
    pub to: Endpoint,
    pub waypoints: Vec<P>,
    pub layer: Option<LayerId>,
}

pub struct Request<'a> {
    pub route: &'a Route,
    pub connectivity: &'a Connectivity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    PLeft,
    NLeft,
}
canon_tags!(Side);

#[derive(Clone, Copy, Debug)]
pub struct End {
    pub at: [P; 2],
    pub anchors: [Anchor; 2],
}

#[derive(Clone, Debug)]
pub enum Section {
    Escape { end: End },
    Coupled { layer: LayerId, spine: Vec<P> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockSource {
    Board,
    EarlierRun,
}
canon_tags!(BlockSource);

impl BlockSource {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Board => "an obstacle on the board",
            Self::EarlierRun => "the pair's own earlier run",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub leg: NetId,
    pub at: P,
    pub need: i64,
    pub source: BlockSource,
}
canon_struct!(Block { leg, at, need, source });

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairReport {
    pub side: Side,
    /// Per-leg start and end escape lengths, in nm.
    pub p_stubs: [i64; 2],
    pub n_stubs: [i64; 2],
    /// Length of new copper without parallel mate copper at the design spacing.
    pub p_uncoupled: i64,
    pub n_uncoupled: i64,
    /// 4b's whole-net measurements on the result, including old copper and barrels.
    pub p_length: i64,
    pub n_length: i64,
    pub skew: i64,
    pub blocked: Option<Block>,
}
canon_struct!(PairReport { side, p_stubs, n_stubs, p_uncoupled, n_uncoupled, p_length, n_length, skew, blocked });

pub struct PairPlan {
    pub side: Side,
    pub sections: Vec<Section>,
    pub p: Vec<Item>,
    pub n: Vec<Item>,
    pub report: PairReport,
    pub blocked: Option<Block>,
}

impl PairPlan {
    pub fn ops(&self) -> Vec<BoardOp> {
        self.p.iter().chain(&self.n).cloned().map(BoardOp::Add).collect()
    }
}

#[derive(Clone, Copy)]
pub struct Pair {
    nets: [NetId; 2],
    widths: [Nm; 2],
    gap: Nm,
}

impl Pair {
    pub fn resolve(board: &Board, rules: &RuleMatrix, net: NetId) -> Result<Self, String> {
        let (mate, gap) = rules.partner(net).ok_or("the net has no declared pair")?;
        let pair = board
            .pairs
            .iter()
            .find(|p| (p.p == net && p.n == mate) || (p.n == net && p.p == mate))
            .ok_or("the declared pair is missing from the board")?;
        let width = |id: NetId| {
            let class = board.nets.get(id.ix()).ok_or("the pair names a missing net")?.class;
            let settings = rules.class(class);
            Ok::<_, String>(
                settings.pair.map(|pair| pair.width).or(settings.track).unwrap_or(0).max(rules.track_min(class)),
            )
        };
        let widths = [width(pair.p)?, width(pair.n)?];
        if widths.iter().any(|&w| w <= 0) || gap <= 0 {
            return Err("pair width and gap must be positive".into());
        }
        Ok(Self { nets: [pair.p, pair.n], widths, gap })
    }

    fn offset(self, leg: usize) -> f64 {
        (f64::from(self.gap) + f64::from(self.widths[1 - leg])) / 2.
    }
}

fn ends(track: &Track) -> impl Iterator<Item = (Anchor, P)> {
    let last = track.path.segs.len().checked_sub(1).and_then(|n| u32::try_from(n).ok());
    let (id, start, end) = (track.id, track.path.start, track.path.end());
    let anchor = move |seg, frac| Anchor::Track { id, at: OnTrack { seg, frac } };
    last.into_iter().flat_map(move |seg| [(anchor(0, 0), start), (anchor(seg, OnTrack::ONE), end)])
}

pub(crate) fn terminal(board: &Board, anchor: Anchor) -> Option<(NetId, P, LayerSet)> {
    match anchor {
        Anchor::Pad { placed, pad } => {
            let part = board.placed_item(placed)?;
            Some((
                (*part.pad_nets.get(pad.ix())?)?,
                part.pad_at(pad)?,
                part.pad_layers(pad, board.copper_set(), board.copper_count()),
            ))
        }
        Anchor::Track { id, .. } => {
            let (ItemKind::Track, i) = board.locate(id)? else { return None };
            let track = board.tracks.get(i)?;
            let (_, point) = ends(track).find(|(a, _)| *a == anchor)?;
            Some((track.net, point, LayerSet::EMPTY.with(track.layer)))
        }
        _ => None,
    }
}

/// The other pad on the same component, or the nearest track end on the mate net
/// for a track start. The capture radius is three centre spacings.
pub fn mate_anchor(board: &Board, pair: Pair, anchor: Anchor) -> Result<Anchor, String> {
    let (net, at, layers) = terminal(board, anchor).ok_or("the pair endpoint is no longer on the board")?;
    let mate =
        pair.nets.iter().position(|&id| id == net).map(|i| pair.nets[1 - i]).ok_or("the endpoint is on another net")?;
    let capture = 3 * (2 * i128::from(pair.gap) + i128::from(pair.widths[0]) + i128::from(pair.widths[1]));
    let nearby = |(candidate, point): (Anchor, P)| {
        let distance = at.dist2(point);
        (4 * distance <= capture * capture).then_some((distance, candidate))
    };
    if let Anchor::Pad { placed, .. } = anchor {
        let part = board.placed_item(placed).ok_or("the pair component is missing")?;
        let nearest = part
            .pad_nets
            .iter()
            .enumerate()
            .filter(|(_, n)| **n == Some(mate))
            .filter_map(|(i, _)| {
                let pad = PadIx(u16::try_from(i).ok()?);
                nearby((Anchor::Pad { placed, pad }, part.pad_at(pad)?))
            })
            .min_by_key(|(distance, _)| *distance);
        if let Some((_, pad)) = nearest {
            return Ok(pad);
        }
    }
    board
        .tracks
        .iter()
        .filter(|t| t.net == mate && layers.has(t.layer))
        .flat_map(ends)
        .filter_map(nearby)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, a)| a)
        .ok_or_else(|| "no mate pad or track end nearby".into())
}

fn snapped(board: &Board, pair: Pair, endpoint: Endpoint, layer: Option<LayerId>) -> Option<Anchor> {
    let point = match endpoint {
        Endpoint::Point(p) => p,
        Endpoint::Anchor(a) => return Some(a),
    };
    for part in &board.placed {
        for (i, net) in part.pad_nets.iter().enumerate() {
            if !net.is_some_and(|net| pair.nets.contains(&net)) {
                continue;
            }
            let pad = PadIx(u16::try_from(i).ok()?);
            if part.pad_at(pad) == Some(point)
                || part
                    .pad_layers(pad, board.copper_set(), board.copper_count())
                    .iter()
                    .filter(|&l| layer.is_none_or(|active| l == active))
                    .any(|l| {
                        part.pad_shape(pad, l, board.copper_count())
                            .is_some_and(|s| fab_elec_core::point_clearance(point, &s) == 0)
                    })
            {
                return Some(Anchor::Pad { placed: part.id, pad });
            }
        }
    }
    board.tracks.iter().filter(|t| pair.nets.contains(&t.net)).flat_map(ends).find(|(_, p)| *p == point).map(|(a, _)| a)
}

fn end_pair(board: &Board, pair: Pair, anchor: Anchor) -> Result<(End, LayerSet), String> {
    let mate = mate_anchor(board, pair, anchor)?;
    let (net, a, la) = terminal(board, anchor).ok_or("missing pair endpoint")?;
    let (_, b, lb) = terminal(board, mate).ok_or("missing mate endpoint")?;
    let end = if net == pair.nets[0] {
        End { at: [a, b], anchors: [anchor, mate] }
    } else {
        End { at: [b, a], anchors: [mate, anchor] }
    };
    Ok((end, la.intersect(lb)))
}

fn point(x: i64, y: i64) -> Result<P, String> {
    Ok(P::new(
        x.try_into().map_err(|_| "pair leaves the board coordinate range")?,
        y.try_into().map_err(|_| "pair leaves the board coordinate range")?,
    ))
}

fn centre(pair: Pair, end: End) -> P {
    // At a continuation this reconstructs the envelope centre even with unequal widths.
    let w = if matches!(end.anchors[0], Anchor::Track { .. }) {
        pair.offset(0) / (pair.offset(0) + pair.offset(1))
    } else {
        0.5
    };
    let mix = |a, b| (f64::from(a) * (1. - w) + f64::from(b) * w).round() as Nm;
    P::new(mix(end.at[0].x, end.at[1].x), mix(end.at[0].y, end.at[1].y))
}

#[derive(Copy, Clone, Debug, PartialEq)]
struct Dir {
    x: i64,
    y: i64,
}
impl Dir {
    fn between(a: P, b: P) -> Self {
        Self { x: (i64::from(b.x) - i64::from(a.x)).signum(), y: (i64::from(b.y) - i64::from(a.y)).signum() }
    }
    fn dot(self, b: Self) -> i64 {
        self.x * b.x + self.y * b.y
    }
    fn cross(self, b: Self) -> i64 {
        self.x * b.y - self.y * b.x
    }
    fn norm(self) -> f64 {
        (self.dot(self) as f64).sqrt()
    }
    fn step(self, a: P, k: i64) -> Result<P, String> {
        point(i64::from(a.x) + k * self.x, i64::from(a.y) + k * self.y)
    }
    fn distance(self, a: P, b: P) -> i64 {
        (i64::from(b.x) - i64::from(a.x)) * self.x + (i64::from(b.y) - i64::from(a.y)) * self.y
    }
    fn normal(self) -> Self {
        Self { x: -self.y, y: self.x }
    }
    fn reverse(self) -> Self {
        Self { x: -self.x, y: -self.y }
    }
    // A perpendicular lattice step on a diagonal is sqrt(2) nm. Rounding the
    // displacement outward here gives integer intersections at every octilinear miter.
    fn offset(self, d: f64) -> i64 {
        d.signum() as i64 * (d.abs() / self.norm()).ceil() as i64
    }
}

fn octilinear(points: impl Iterator<Item = P>) -> Result<Vec<P>, String> {
    let mut spine: Vec<P> = Vec::new();
    for b in points {
        if let Some(&a) = spine.last() {
            let dx = i64::from(b.x) - i64::from(a.x);
            let dy = i64::from(b.y) - i64::from(a.y);
            let diagonal = dx.abs().min(dy.abs());
            if diagonal > 0 && dx.abs() != dy.abs() {
                let corner = point(i64::from(b.x) - diagonal * dx.signum(), i64::from(b.y) - diagonal * dy.signum())?;
                spine.push(corner);
            }
        }
        if spine.last() != Some(&b) {
            spine.push(b);
        }
    }
    let mut clean = Vec::new();
    for p in spine {
        if clean.len() >= 2 {
            let i = clean.len() - 1;
            let a = Dir::between(clean[i - 1], clean[i]);
            let b = Dir::between(clean[i], p);
            if a.dot(b) < 0 {
                return Err("the spine turns more than 90 degrees".into());
            }
            if a == b {
                clean.pop();
            }
        }
        clean.push(p);
    }
    if clean.len() < 2 {
        return Err("the pair needs two distinct stations".into());
    }
    Ok(clean)
}

fn left(side: Side, leg: usize) -> bool {
    (side == Side::PLeft) == (leg == 0)
}

fn order(dir: Dir, end: End) -> Result<Side, String> {
    match dir.normal().distance(end.at[1], end.at[0]).signum() {
        1 => Ok(Side::PLeft),
        -1 => Ok(Side::NLeft),
        _ => Err("the pads have no left/right order along this spine".into()),
    }
}

fn intersect(p: P, a: Dir, q: P, b: Dir) -> Result<P, String> {
    let det = a.cross(b);
    if det == 0 {
        return Err("parallel spine legs have no bend intersection".into());
    }
    let x = i64::from(q.x) - i64::from(p.x);
    let y = i64::from(q.y) - i64::from(p.y);
    let numerator = x * b.y - y * b.x;
    // Diagonal intersections on the half-grid arise when simplifying a short
    // chamfer between opposite-parity waypoints.
    if numerator % det != 0 {
        return Err("the bend intersection is between grid points".into());
    }
    a.step(p, numerator / det)
}

fn displaced(p: P, dir: Dir, offset: f64) -> Result<P, String> {
    dir.normal().step(p, dir.offset(offset))
}

fn simplify(spine: &mut Vec<P>, pair: Pair, side: Side) -> Result<(), String> {
    let mut i = 1;
    while i + 2 < spine.len() {
        let (a, b, c, d) = (spine[i - 1], spine[i], spine[i + 1], spine[i + 2]);
        let (u, v, w) = (Dir::between(a, b), Dir::between(b, c), Dir::between(c, d));
        let turn = u.cross(v).signum();
        if turn != 0 && turn == v.cross(w).signum() {
            let inner = usize::from((side == Side::PLeft) != (turn > 0));
            // Use the actual outward lattice offsets on the three supports. A miter
            // on the inner side consumes this much of the middle straight at each end.
            let offset = pair.offset(inner);
            let setback = |other: Dir| {
                let scales = v.norm() * other.norm();
                let h = v.offset(offset) as f64 * v.norm();
                let k = other.offset(offset) as f64 * other.norm();
                (k * scales - h * v.dot(other) as f64) / v.cross(other).abs() as f64
            };
            let missing = f64::from(pair.widths[inner]) + setback(u) + setback(w) - (b.dist2(c) as f64).sqrt();
            if missing > 0. {
                if u.dot(w) >= 0 {
                    if let Ok(corner) = intersect(b, u, c, w) {
                        if u.distance(a, corner) > 0 && w.distance(corner, d) > 0 {
                            spine.splice(i..=i + 1, [corner]);
                            i = i.saturating_sub(1).max(1);
                            continue;
                        }
                    }
                }
                // Where merging would make a turn sharper than 90 degrees or a
                // half-grid corner, grow the chamfer on its two existing tangents.
                let back = w.cross(v).abs();
                let forward = u.cross(v).abs();
                let growth = (back * u.dot(v) + forward * w.dot(v)) as f64 / v.norm();
                if growth <= 0. {
                    return Err("the inner hairpin cannot fit between the fixed legs".into());
                }
                let steps = (missing / growth).ceil() as i64;
                let start = u.step(b, -steps * back)?;
                let finish = w.step(c, steps * forward)?;
                if u.distance(a, start) <= 0 || w.distance(finish, d) <= 0 {
                    return Err("the inner bend cannot fit between the fixed stations".into());
                }
                spine[i] = start;
                spine[i + 1] = finish;
            }
        }
        i += 1;
    }
    Ok(())
}

fn escape_station(pair: Pair, side: Side, end: End, centre: P, forward: Dir, entering: bool) -> Result<P, String> {
    let travel = if entering { forward } else { forward.reverse() };
    let mut steps = 0;
    for leg in 0..2 {
        let d = if left(side, leg) { pair.offset(leg) } else { -pair.offset(leg) };
        let foot = displaced(centre, forward, d)?;
        // An escape fans in from the outside of its offset support. Convexity
        // then keeps the *entire straight stub* on that side, including its
        // cap. A pin inside the channel cannot make this escape at this heading.
        let lateral = forward.normal().distance(foot, end.at[leg]);
        if lateral * (d.signum() as i64) < 0 {
            return Err("the endpoints have too little room to escape at the pair gap along this spine".into());
        }
        let across = lateral.abs();
        let needed = across - travel.distance(end.at[leg], foot);
        let unit = travel.dot(travel);
        steps = steps.max((needed + unit - 1).div_euclid(unit));
    }
    travel.step(centre, steps)
}

struct CopperPlan {
    sections: Vec<Section>,
    tracks: [Vec<Track>; 2],
    stubs: [[i64; 2]; 2],
    runs: Vec<CopperRun>,
}

/// The two pieces for one spine segment or escape, in travel order. A pad
/// already on its coupled leg has an empty escape on that side.
#[derive(Clone)]
struct CopperRun {
    layer: LayerId,
    coupled: bool,
    pieces: [Option<RItem>; 2],
}

struct Layout<'a> {
    board: &'a Board,
    pair: Pair,
    side: Side,
    layer: LayerId,
    start: End,
    next_id: u32,
    clearance: [Nm; 2],
}

impl Layout<'_> {
    fn legs(&self, spine: &[P], arrival: Option<End>) -> Result<CopperPlan, String> {
        if spine.len() < 2 {
            return Err("no length for a coupled section".into());
        }
        let mut sections =
            vec![Section::Escape { end: self.start }, Section::Coupled { layer: self.layer, spine: spine.to_vec() }];
        if let Some(end) = arrival {
            sections.push(Section::Escape { end });
        }
        let mut tracks = [Vec::new(), Vec::new()];
        let mut stubs = [[0; 2]; 2];
        let mut runs = Vec::new();
        let mut id = self.next_id;
        for (section, run) in sections.iter().enumerate() {
            let Section::Coupled { layer, spine } = run else { continue };
            let before = section.checked_sub(1).and_then(|i| sections.get(i));
            let after = sections.get(section + 1);
            let coupled_start = usize::from(matches!(before, Some(Section::Escape { .. })));
            let coupled_end = coupled_start + spine.len() - 1;
            let count = coupled_end + usize::from(matches!(after, Some(Section::Escape { .. })));
            let first_run = runs.len();
            runs.extend((0..count).map(|i| CopperRun {
                layer: *layer,
                coupled: (coupled_start..coupled_end).contains(&i),
                pieces: [None, None],
            }));
            for leg in 0..2 {
                let signed = if left(self.side, leg) { self.pair.offset(leg) } else { -self.pair.offset(leg) };
                let mut points = Vec::with_capacity(spine.len() + 2);
                let mut anchors = [Anchor::Free; 2];
                for (i, &p) in spine.iter().enumerate() {
                    let prev = i.checked_sub(1).map(|j| Dir::between(spine[j], p));
                    let next = spine.get(i + 1).map(|&q| Dir::between(p, q));
                    let vertex = match (prev, next) {
                        (Some(a), Some(b)) => intersect(displaced(p, a, signed)?, a, displaced(p, b, signed)?, b)?,
                        (Some(d), None) | (None, Some(d)) => displaced(p, d, signed)?,
                        _ => return Err("empty coupled section".into()),
                    };
                    if i == 0 {
                        if let Some(Section::Escape { end }) = before {
                            points.push(end.at[leg]);
                            anchors[0] = end.anchors[leg];
                            stubs[leg][0] = (end.at[leg].dist2(vertex) as f64).sqrt().round() as i64;
                        }
                    }
                    if i > 0
                        && prev.is_some_and(|dir| points.last().is_some_and(|&last| dir.distance(last, vertex) <= 0))
                    {
                        return Err("the coupled section is too short for the inner bend".into());
                    }
                    points.push(vertex);
                    if i + 1 == spine.len() {
                        if let Some(Section::Escape { end }) = after {
                            points.push(end.at[leg]);
                            anchors[1] = end.anchors[leg];
                            stubs[leg][1] = (end.at[leg].dist2(vertex) as f64).sqrt().round() as i64;
                        }
                    }
                }
                let first = *points.first().ok_or("empty pair leg")?;
                let path = Path {
                    start: first,
                    segs: points.windows(2).filter(|ab| ab[0] != ab[1]).map(|ab| Seg::Line(ab[1])).collect(),
                };
                let track = Track {
                    id: ItemId(id),
                    net: self.pair.nets[leg],
                    layer: *layer,
                    w: self.pair.widths[leg],
                    widths: None,
                    path,
                    ends: anchors,
                    locked: false,
                    origin: Origin::Authored,
                };
                let mut seg = 0;
                for (run, ab) in runs[first_run..].iter_mut().zip(points.windows(2)) {
                    if ab[0] != ab[1] {
                        run.pieces[leg] = Some(verify::track_piece(self.board, &track, seg, ab[0], &Seg::Line(ab[1])));
                        seg += 1;
                    }
                }
                tracks[leg].push(track);
                id = id.checked_add(1).ok_or("the board has no free item ids")?;
            }
        }
        Ok(CopperPlan { sections, tracks, stubs, runs })
    }

    fn envelope(&self) -> i64 {
        i64::from(self.pair.widths[0]) + i64::from(self.pair.gap) + i64::from(self.pair.widths[1])
    }

    fn board_block(&self, world: &World, rules: &RuleMatrix, piece: &RItem, leg: usize) -> Option<Block> {
        let (hit, reference) =
            verify::hits(world, rules, self.board, std::slice::from_ref(piece)).into_iter().next()?;
        let mut far_gap = hit.need;
        if let (Some(obstacle), RPiece::Copper(c)) = (world.get(reference), &piece.piece) {
            let mate = self.pair.nets[1 - leg];
            if let Some(net) = self.board.nets.get(mate.ix()) {
                let probe = Copper { class: net.class, net: Some(mate), ..c.clone() };
                far_gap = fab_elec_board::required_to(rules, &probe, &obstacle.obstacle(), c.layer).gap;
            }
        }
        Some(Block {
            leg: self.pair.nets[leg],
            at: hit.at,
            need: self.envelope() + i64::from(hit.need) + i64::from(far_gap),
            source: BlockSource::Board,
        })
    }

    /// Square-cut the two pieces from legs() at a station on their common straight.
    /// A cut inside an inner miter cannot be constructed. Only the final endpoints change.
    fn cut(&self, run: &CopperRun, dir: Dir, station: P) -> Result<Option<CopperRun>, String> {
        let mut cut = run.clone();
        for (leg, item) in cut.pieces.iter_mut().enumerate() {
            let Some(piece) = item else { return Err("a coupled run is missing its leg".into()) };
            let RPiece::Copper(c) = &mut piece.piece else { return Err("the pair leg has no copper".into()) };
            let Shape::Segment { a, b, .. } = &mut c.shape else {
                return Err("the coupled run is not straight".into());
            };
            let d = self.pair.offset(leg);
            *b = displaced(station, dir, if left(self.side, leg) { d } else { -d })?;
            if dir.distance(*a, *b) <= 0 {
                return Ok(None);
            }
            piece.bbox = c.shape.bbox();
        }
        Ok(Some(cut))
    }
}

/// Fixed mitered pieces in travel order. A prefix reuses every preceding run
/// verbatim; only its terminal run has square endpoints in place of the miters.
/// The packed tree includes future runs, which queries exclude by ordinal.
struct RunIndex<'a> {
    runs: &'a [CopperRun],
    index: Index<(usize, usize)>,
    previous: Vec<[Option<usize>; 2]>,
}

impl<'a> RunIndex<'a> {
    fn new(runs: &'a [CopperRun]) -> Self {
        let mut entries = Vec::new();
        let mut previous = Vec::with_capacity(runs.len());
        let mut last = [None; 2];
        for (i, run) in runs.iter().enumerate() {
            previous.push(last);
            for (leg, piece) in run.pieces.iter().enumerate() {
                if let Some(piece) = piece {
                    entries.push((piece.bbox, (i, leg)));
                    last[leg] = Some(i);
                }
            }
        }
        Self { runs, index: Index::build(entries), previous }
    }

    fn own_block(&self, layout: &Layout, i: usize, run: &CopperRun, leg: usize) -> Option<Block> {
        let piece = run.pieces[leg].as_ref()?;
        let shape = piece.shape();
        let Shape::Segment { a, b, .. } = shape else { return None };
        let reach = layout.clearance[leg].max(layout.pair.gap);
        let mut witness = None;
        self.index.query(piece.bbox.inflate(reach), |_, key @ (j, other_leg)| {
            if j > i || (i == j && (run.coupled || other_leg >= leg)) || self.runs[j].layer != run.layer {
                return ControlFlow::Continue(());
            }
            // Prefer the earliest piece, independent of the tree's traversal order.
            if witness.is_some_and(|earlier| earlier <= key) {
                return ControlFlow::Continue(());
            }
            if let Some(other) = &self.runs[j].pieces[other_leg] {
                let adjacent = leg == other_leg
                    && self.previous[i][leg] == Some(j)
                    && matches!(other.shape(), Shape::Segment { b: end, .. } if end == a);
                let spacing = if leg == other_leg { layout.clearance[leg] } else { layout.pair.gap };
                if !adjacent && fab_elec_core::collides(shape, other.shape(), spacing) {
                    witness = Some(key);
                }
            }
            ControlFlow::Continue(())
        });
        let (j, other_leg) = witness?;
        let other = self.runs[j].pieces[other_leg].as_ref()?;
        Some(Block {
            leg: layout.pair.nets[leg],
            at: crate::head::obstacles::nearest_points(shape, other.shape()).map_or(*b, |(here, _)| here),
            need: layout.envelope() + i64::from(layout.clearance[other_leg]) + i64::from(layout.pair.gap),
            source: BlockSource::EarlierRun,
        })
    }

    fn first_block(&self, layout: &Layout, world: &World, rules: &RuleMatrix) -> Option<(usize, Block)> {
        let mut first =
            self.runs.iter().enumerate().find_map(|(i, run)| {
                (0..2).find_map(|leg| self.own_block(layout, i, run, leg)).map(|block| (i, block))
            });
        let mut overlay = world.clone();
        // Verify P first, then N against the same board with verified P copper in it.
        // Work past the first known fault can never be part of a legal prefix.
        for leg in 0..2 {
            for (i, run) in self.runs.iter().enumerate().take(first.map_or(self.runs.len(), |(i, _)| i)) {
                let Some(piece) = &run.pieces[leg] else { continue };
                if let Some(block) = layout.board_block(&overlay, rules, piece, leg) {
                    first = Some((i, block));
                    break;
                }
                if leg == 0 {
                    overlay.add(piece.clone());
                }
            }
        }
        first
    }

    /// Before the first bad miter, a terminal piece wholly contained in the
    /// checked piece inherits its verdict. A cap extending beyond it is new
    /// geometry. Check that piece against earlier copper and the committed world.
    fn cut_block(
        &self,
        layout: &Layout,
        i: usize,
        cut: &CopperRun,
        dir: Dir,
        full_clean: bool,
        world: &World,
        rules: &RuleMatrix,
    ) -> Option<Block> {
        for leg in 0..2 {
            let piece = cut.pieces[leg].as_ref()?;
            if full_clean {
                if let (Shape::Segment { b, .. }, Some(Shape::Segment { b: miter, .. })) =
                    (piece.shape(), self.runs[i].pieces[leg].as_ref().map(RItem::shape))
                {
                    if dir.distance(*b, *miter) >= 0 {
                        continue;
                    }
                }
            }
            if let Some(block) =
                self.own_block(layout, i, cut, leg).or_else(|| layout.board_block(world, rules, piece, leg))
            {
                return Some(block);
            }
        }
        None
    }

    fn truncate(
        &self,
        layout: &Layout,
        spine: &[P],
        first: (usize, Block),
        world: &World,
        rules: &RuleMatrix,
    ) -> Result<(CopperPlan, Block), String> {
        let (bad_run, mut block) = first;
        let mut best = None;
        for ((i, run), (segment, ab)) in
            self.runs.iter().enumerate().filter(|(_, run)| run.coupled).zip(spine.windows(2).enumerate())
        {
            if i > bad_run {
                break;
            } // the bad miter is now in the fixed portion
            let dir = Dir::between(ab[0], ab[1]);
            let Some(cut) = layout.cut(run, dir, ab[1])? else { continue };
            let check = |cut: &CopperRun| self.cut_block(layout, i, cut, dir, i < bad_run, world, rules);
            if let Some(hit) = check(&cut) {
                block = hit;
                let (mut low, mut high) = (0, dir.distance(ab[0], ab[1]) / dir.dot(dir));
                while high - low > 1 {
                    let mid = low + (high - low) / 2;
                    let station = dir.step(ab[0], mid)?;
                    let Some(cut) = layout.cut(run, dir, station)? else {
                        low = mid;
                        continue;
                    };
                    if let Some(hit) = check(&cut) {
                        block = hit;
                        high = mid;
                    } else {
                        low = mid;
                        best = Some((segment, station));
                    }
                }
                break;
            }
            best = Some((segment, ab[1]));
        }
        let (segment, station) = best.ok_or_else(|| {
            format!(
                "pair blocked at its first station on {} by {}",
                layout.board.nets.get(block.leg.ix()).map_or("a missing net", |n| &n.name),
                block.source.reason()
            )
        })?;
        let mut prefix = spine[..=segment].to_vec();
        prefix.push(station);
        Ok((layout.legs(&prefix, None)?, block))
    }
}

/// Project parallel mate copper onto each segment and count the union of the overlap.
/// The diagonal's outward grid step can widen each leg offset by less than sqrt(2) nm.
fn uncoupled(tracks: &[Track], world: &World, mate: NetId, gap: Nm) -> i64 {
    let mut total = 0.;
    for track in tracks {
        for (i, (a, seg)) in track.path.steps().enumerate() {
            let b = seg.end();
            let len = (a.dist2(b) as f64).sqrt();
            if len == 0. {
                continue;
            }
            let dx = i64::from(b.x) - i64::from(a.x);
            let dy = i64::from(b.y) - i64::from(a.y);
            let project = |p: P| {
                let x = i128::from(p.x) - i128::from(a.x);
                let y = i128::from(p.y) - i128::from(a.y);
                (
                    (x * i128::from(dx) + y * i128::from(dy)) as f64 / len,
                    (y * i128::from(dx) - x * i128::from(dy)) as f64 / len,
                )
            };
            let mut covered = Vec::new();
            let rounding = 2. * std::f64::consts::SQRT_2;
            let reach = gap.saturating_add(rounding.ceil() as Nm);
            let area = track.seg_shape(i, a, &seg).bbox().inflate(reach);
            for (_, piece) in world.query_sorted(track.layer, area) {
                let RPiece::Copper(Copper {
                    item: ConnItem::Track { .. },
                    net: Some(net),
                    shape: Shape::Segment { a: p, b: q, w },
                    ..
                }) = &piece.piece
                else {
                    continue;
                };
                if *net != mate {
                    continue;
                }
                let ox = i128::from(q.x) - i128::from(p.x);
                let oy = i128::from(q.y) - i128::from(p.y);
                if ox * i128::from(dy) != oy * i128::from(dx) {
                    continue;
                }
                let ((x, y), (u, _)) = (project(*p), project(*q));
                let spacing = f64::from(gap) + (f64::from(track.w) + f64::from(*w)) / 2.;
                if (y.abs() - spacing).abs() < rounding {
                    covered.push((x.min(u).max(0.), x.max(u).min(len)));
                }
            }
            covered.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut end: f64 = 0.;
            total += len;
            for (a, b) in covered {
                total -= (b - a.max(end)).max(0.);
                end = end.max(b);
            }
        }
    }
    total.round() as i64
}

/// Build and verify the two legs on one overlay, measure their committed geometry,
/// and return both sets of copper together. A blocked run probes prefixes and then
/// bisects the blocked straight in lattice steps. Neither leg has a separate search.
pub fn plan(board: &Board, rules: &RuleMatrix, world: &World, request: Request<'_>) -> Result<PairPlan, String> {
    let route = request.route;
    let pair = Pair::resolve(board, rules, route.net)?;
    let a = snapped(board, pair, route.from, route.layer).ok_or("start on a pad or track end of the pair")?;
    let (start, mut layers) = end_pair(board, pair, a)?;
    let arrival = snapped(board, pair, route.to, route.layer).map(|a| end_pair(board, pair, a)).transpose()?;
    if let Some((_, ls)) = arrival {
        layers = layers.intersect(ls);
    }
    let layer = route.layer.or_else(|| layers.iter().next()).ok_or("pair vias come next")?;
    if !layers.has(layer) {
        return Err("pair vias come next".into());
    }
    let arrival = arrival.map(|(end, _)| end);
    let target = match (arrival, route.to) {
        (Some(end), _) => centre(pair, end),
        (None, Endpoint::Point(p)) => p,
        _ => return Err("missing arrival pad".into()),
    };
    let mut spine =
        octilinear(std::iter::once(centre(pair, start)).chain(route.waypoints.iter().copied()).chain([target]))?;
    let first = Dir::between(spine[0], spine[1]);
    let side = order(first, start)?;
    simplify(&mut spine, pair, side)?;
    let last = spine.len() - 1;
    let final_dir = Dir::between(spine[last - 1], spine[last]);
    if let Some(end) = arrival {
        if order(final_dir, end)? != side {
            return Err("the pair arrives swapped; it needs a layer change".into());
        }
        spine[last] = escape_station(pair, side, end, target, final_dir, false)?;
    }
    spine[0] = escape_station(pair, side, start, spine[0], first, true)?;
    if first.distance(spine[0], spine[1]) <= 0 || final_dir.distance(spine[last - 1], spine[last]) <= 0 {
        return Err("the pad escapes leave no room for the coupled section".into());
    }

    let clearance = |net: NetId| {
        let class = board.nets.get(net.ix()).ok_or("the pair names a missing net")?.class;
        let key = RuleKey { class, item: RuleItem::Track };
        Ok::<_, String>(rules.clearance(key, key, layer).max(1))
    };
    let layout = Layout {
        board,
        pair,
        side,
        layer,
        start,
        next_id: board.next_item,
        clearance: [clearance(pair.nets[0])?, clearance(pair.nets[1])?],
    };
    let full = layout.legs(&spine, arrival)?;
    let index = RunIndex::new(&full.runs);
    let (candidate, blocked) = if let Some(first) = index.first_block(&layout, world, rules) {
        let (partial, block) = index.truncate(&layout, &spine, first, world, rules)?;
        (partial, Some(block))
    } else {
        (full, None)
    };
    let mut result = board.clone();
    let mut copper = world.clone();
    for t in candidate.tracks.iter().flatten() {
        result
            .apply(&BoardOp::Add(Item::Track(t.clone())))
            .map_err(|error| format!("cannot add pair track: {error:?}"))?;
        for piece in verify::track_pieces(board, t, |_| true) {
            copper.add(piece);
        }
    }
    // Connectivity determines the partial flag. Only 4b's barrel-aware amounts
    // are needed here; both refer to the board with the proposed tracks applied.
    let lengths = length::net_lengths(&result, request.connectivity, &pair.nets);
    let measurement = |i: usize| lengths.get(&pair.nets[i]).map(|m| m.nm).ok_or("the pair length is unavailable");
    let (p_length, n_length) = (measurement(0)?, measurement(1)?);
    let [p, n] = candidate.tracks;
    let report = PairReport {
        side,
        p_stubs: candidate.stubs[0],
        n_stubs: candidate.stubs[1],
        p_uncoupled: uncoupled(&p, &copper, pair.nets[1], pair.gap),
        n_uncoupled: uncoupled(&n, &copper, pair.nets[0], pair.gap),
        p_length,
        n_length,
        skew: (p_length - n_length).abs(),
        blocked,
    };
    Ok(PairPlan {
        side,
        sections: candidate.sections,
        p: p.into_iter().map(Item::Track).collect(),
        n: n.into_iter().map(Item::Track).collect(),
        report,
        blocked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Base, Gesture, GestureId, GestureKind, GestureSample, ProposalId, RBase};
    use fab_elec_board::{connect, drc, resolve_rules, BoardNet, NetPair};
    use fab_elec_circuit::{CheckKind, ClassRules, NetClass};
    use fab_elec_core::{clearance, mm, BoardId, ClassId, Contour};
    use std::sync::Arc;

    fn track(board: &mut Board, net: NetId, w: Nm, a: P, b: P) -> Anchor {
        let id = board.alloc_id();
        board
            .apply(&BoardOp::Add(Item::Track(Track {
                id,
                net,
                layer: LayerId::F_CU,
                w,
                widths: None,
                path: Path::new(a).line(b),
                ends: [Anchor::Free; 2],
                locked: false,
                origin: Origin::Authored,
            })))
            .unwrap();
        Anchor::Track { id, at: OnTrack { seg: 0, frac: OnTrack::ONE } }
    }

    fn fixture(gap: Option<Nm>, widths: [Nm; 2], stagger: Nm) -> (Board, Route) {
        let mut board = Board::new(BoardId(0), "pair");
        board.outline.push(Contour::rect(P::ZERO, P::mm(50., 40.), 0));
        for (i, name) in ["DP", "DN", "signal"].into_iter().enumerate() {
            let class = ClassId(i as u16);
            board.nets.push(BoardNet { name: name.into(), class });
            board.classes.push(NetClass {
                id: class,
                name: name.into(),
                rules: ClassRules { track: Some(widths[i.min(1)]), ..Default::default() },
            });
        }
        board.pairs.push(NetPair { p: NetId(0), n: NetId(1), gap });
        let gap = resolve_rules(&board, None).partner(NetId(0)).unwrap().1;
        let mut launch = Anchor::Free;
        for (i, w) in widths.into_iter().enumerate() {
            let dy = (gap + widths[1 - i]) / 2;
            let y = mm(10.) + if i == 0 { dy } else { -dy };
            let x = mm(5.) + if i == 0 { 0 } else { stagger };
            launch = track(&mut board, NetId(i as u32), w, P::new(x - mm(2.), y), P::new(x, y));
        }
        (
            board,
            Route {
                net: NetId(1),
                from: Endpoint::Anchor(launch),
                to: Endpoint::Point(P::mm(15., 10.)),
                waypoints: vec![],
                layer: None,
            },
        )
    }

    fn run(board: &Board, route: &Route) -> Result<PairPlan, String> {
        let base = RBase::build(board);
        plan(
            board,
            &resolve_rules(board, None),
            &World::new(&base),
            Request { route, connectivity: &connect(board, &[]) },
        )
    }

    fn proposed(items: &[Item]) -> &Track {
        let [Item::Track(t)] = items else { panic!("expected one track per leg") };
        t
    }

    fn minimum_gap(a: &Track, b: &Track) -> Nm {
        a.path
            .steps()
            .enumerate()
            .flat_map(|(i, (p, s))| {
                let shape = a.seg_shape(i, p, &s);
                b.path.steps().enumerate().map(move |(j, (q, t))| clearance(&shape, &b.seg_shape(j, q, &t)))
            })
            .min()
            .unwrap()
    }

    #[test]
    fn bend_skew_and_unequal_width_offsets_match_the_committed_copper() {
        for widths in [[mm(0.2); 2], [mm(0.2), mm(0.3)]] {
            let (mut board, mut route) = fixture(None, widths, 0);
            route.waypoints = vec![P::mm(10., 10.)];
            route.to = Endpoint::Point(P::mm(16., 16.));
            let result = run(&board, &route).unwrap();
            assert!(result.blocked.is_none());
            assert_eq!(result.side, Side::PLeft);
            let (p, n) = (proposed(&result.p), proposed(&result.n));
            let gap = resolve_rules(&board, None).partner(p.net).unwrap().1;
            assert_eq!(p.path.start.y - n.path.start.y - (widths[0] + widths[1]) / 2, gap);
            assert_eq!(p.path.start.y + widths[0] / 2 - mm(10.), mm(10.) - (n.path.start.y - widths[1] / 2));
            assert!(minimum_gap(p, n) >= gap);
            let spacing = f64::from(gap) + f64::from(widths[0] + widths[1]) / 2.;
            assert!((result.report.skew as f64 - 2. * (std::f64::consts::PI / 8.).tan() * spacing).abs() <= 3.);
            // Following the same bend with an opposite 45-degree turn cancels its skew.
            let mut balanced = route.clone();
            balanced.waypoints.push(P::mm(16., 16.));
            balanced.to = Endpoint::Point(P::mm(22., 16.));
            let balanced = run(&board, &balanced).unwrap();
            assert_eq!(balanced.blocked, None);
            assert_eq!(balanced.report.skew, 0);
            assert!(minimum_gap(proposed(&balanced.p), proposed(&balanced.n)) >= gap);
            board.apply_all(&result.ops()).unwrap();
            let conn = connect(&board, &[]);
            let read = length::net_lengths(&board, &conn, &[NetId(0), NetId(1)]);
            assert_eq!(read[&NetId(1)].nm - read[&NetId(0)].nm, result.report.skew);
            assert!(drc(&board, &conn, &resolve_rules(&board, None), &[], None)
                .violations
                .iter()
                .all(|v| v.kind != CheckKind::PairGap));
        }
    }

    #[test]
    fn short_bends_merge_on_the_spine_and_swapped_pads_are_refused() {
        let (mut board, mut route) = fixture(None, [mm(0.2); 2], 0);
        route.waypoints = vec![P::mm(10., 10.), P::mm(10.01, 10.01)];
        route.to = Endpoint::Point(P::mm(10.01, 20.));
        let result = run(&board, &route).unwrap();
        let Section::Coupled { spine, .. } = &result.sections[1] else { panic!("coupled run") };
        assert_eq!(spine.as_slice(), [P::mm(5., 10.), P::mm(10.01, 10.), P::mm(10.01, 20.)]);
        for items in [&result.p, &result.n] {
            let t = proposed(items);
            assert_eq!(t.path.segs.len(), 2);
            let b = t.path.segs[0].end();
            assert!(b.x > t.path.start.x && t.path.end().y > b.y);
        }
        let a = track(&mut board, NetId(0), mm(0.2), P::mm(21., 9.8), P::mm(20., 9.8));
        track(&mut board, NetId(1), mm(0.2), P::mm(21., 10.2), P::mm(20., 10.2));
        route.waypoints.clear();
        route.to = Endpoint::Anchor(a);
        assert_eq!(run(&board, &route).err().as_deref(), Some("the pair arrives swapped; it needs a layer change"));
    }

    #[test]
    fn pair_gesture_follows_the_catalog_spine_and_truncates_both_legs() {
        let (mut board, mut route) = fixture(None, [mm(0.2); 2], 0);
        track(&mut board, NetId(2), mm(0.2), P::mm(14., 9.), P::mm(14., 17.));
        let rules = Arc::new(resolve_rules(&board, None));
        let Endpoint::Anchor(start) = route.from else { unreachable!() };
        let mate_start = mate_anchor(&board, Pair::resolve(&board, &rules, route.net).unwrap(), start).unwrap();
        let mate_start_at = board.anchor_pos(mate_start).unwrap();
        let kind = GestureKind::RoutePair { mate: NetId(0), mate_start, mate_start_at };
        let mut g = Gesture::new(kind, board.unit, board.generation, start, board.anchor_pos(start).unwrap(), LayerId::F_CU);
        g.net = Some(route.net);
        let conn = Arc::new(connect(&board, &[]));
        let base = Base::new(Arc::new(board), rules, conn, Arc::default());
        let world = RBase::build(&base.board);
        let mut state = crate::head::GestureState::default();
        route.layer = Some(g.layer);
        for (i, end) in [P::mm(10., 10.), P::mm(12., 12.), P::mm(16., 12.)].into_iter().enumerate() {
            g.push_trail(end, 0);
            route.to = Endpoint::Point(end);
            let expected = run(&base.board, &route).unwrap();
            let sample = GestureSample { id: GestureId(7), seq: i as u64 + 1, gesture: g.clone() };
            let p = crate::head::answer(&base, &world, &sample, &mut state, ProposalId(i as u32)).proposal().unwrap();
            assert!(p.verified && p.last);
            assert_eq!(p.ops, expected.ops());
            assert_eq!(p.pair.as_ref(), Some(&expected.report));
            assert_eq!(expected.report.blocked.is_some(), i == 2);
            if let Some(block) = expected.report.blocked {
                assert_eq!(block.source, BlockSource::Board);
                for item in expected.p.iter().chain(&expected.n) {
                    let Item::Track(t) = item else { panic!("expected a track") };
                    assert_eq!(t.ends[1], Anchor::Free);
                    assert!(t.path.end().x < mm(14.));
                }
            }
            route.waypoints.push(end);
        }
        if let GestureKind::RoutePair { mate_start_at, .. } = &mut g.kind { mate_start_at.x += 1; }
        let sample = GestureSample { id: GestureId(7), seq: 4, gesture: g };
        let refused = crate::head::answer(&base, &world, &sample, &mut state, ProposalId(4)).proposal().unwrap();
        assert!(refused.ops.is_empty() && !refused.verified);
        assert!(matches!(crate::worker::gesture_event(&base, refused), crate::RouteEvent::Refused(r) if r.text.contains("changed")));
    }

    #[test]
    fn last_legal_station_can_be_continued_from_either_leg() {
        let (mut board, route) = fixture(None, [mm(0.2); 2], 0);
        track(&mut board, NetId(2), mm(0.2), P::mm(11., 10.2), P::mm(11., 13.));
        let partial = run(&board, &route).unwrap();
        let blocked = partial.blocked.unwrap();
        assert_eq!(blocked.leg, NetId(0));
        assert_eq!(blocked.need, i64::from(mm(0.9)));
        let (p, n) = (proposed(&partial.p), proposed(&partial.n));
        assert_eq!(p.path.end().x, n.path.end().x);
        assert!(p.path.end().x > mm(10.) && p.path.end().x < mm(11.));
        let mut two_blocks = board.clone();
        track(&mut two_blocks, NetId(2), mm(0.2), P::mm(9., 9.8), P::mm(9., 7.));
        let first = run(&two_blocks, &route).unwrap();
        assert_eq!(first.blocked.unwrap().leg, NetId(1), "the first obstruction along the spine limits both legs");
        board.apply_all(&partial.ops()).unwrap();
        // The obstruction moves away, as after a placement edit.
        let obstacle = board.tracks[2].id;
        board.apply(&BoardOp::Move { item: obstacle, by: P::mm(0., 5.) }).unwrap();
        for old in [p, n] {
            let mut next = route.clone();
            next.from = Endpoint::Point(old.path.end());
            let rest = run(&board, &next).unwrap();
            assert_eq!(rest.blocked, None);
            for (new, previous) in [(proposed(&rest.p), p), (proposed(&rest.n), n)] {
                assert_eq!(new.path.start, previous.path.end());
                assert_eq!(board.anchor_pos(new.ends[0]), Some(previous.path.end()));
            }
        }
    }

    #[test]
    fn escape_and_board_drc_agree_at_both_pair_gap_thresholds() {
        for value in [None, Some(mm(0.10)), Some(mm(0.6))] {
            let (base, route) = fixture(value, [mm(0.2); 2], mm(0.4));
            let original = run(&base, &route).unwrap();
            assert!(original.report.p_stubs[0] > 0);
            let gap = resolve_rules(&base, None).partner(NetId(0)).unwrap().1;
            let threshold = fab_elec_board::drc::allowed_need(gap);
            let rules = resolve_rules(&base, None);
            let index = RBase::build(&base);
            let mut probe = proposed(&original.p).clone();
            for (net, radius) in [(NetId(0), verify::reach(&rules).max(gap)), (NetId(2), verify::reach(&rules))] {
                probe.net = net;
                let pieces = verify::track_pieces(&base, &probe, |_| true);
                assert_eq!(verify::reach_of(&rules, &base, &index, &pieces[0]), radius);
            }
            for spacing in [threshold, threshold - 1] {
                let mut board = base.clone();
                let y = proposed(&original.p).path.start.y + mm(0.2) + spacing;
                track(&mut board, NetId(1), mm(0.2), P::new(mm(5.2), y), P::new(mm(5.2), y + mm(2.)));
                let clean = run(&board, &route).is_ok_and(|p| p.blocked.is_none());
                let mut unchecked = board.clone();
                // The witness has identical geometry and fresh ids after adding the obstacle.
                for op in original.ops() {
                    let BoardOp::Add(Item::Track(mut t)) = op else { panic!("pair track") };
                    t.id = unchecked.alloc_id();
                    unchecked.apply(&BoardOp::Add(Item::Track(t))).unwrap();
                }
                let found = drc(&unchecked, &connect(&unchecked, &[]), &resolve_rules(&unchecked, None), &[], None)
                    .violations
                    .iter()
                    .any(|v| v.kind == CheckKind::PairGap);
                assert_eq!((clean, found), (spacing == threshold, spacing < threshold), "gap {gap}, spacing {spacing}");
            }
        }
    }

    #[test]
    fn a_returning_spine_keeps_the_gap_between_nonadjacent_new_runs() {
        let width = mm(0.2);
        let (board, mut route) = fixture(None, [width; 2], 0);
        let gap = resolve_rules(&board, None).partner(route.net).unwrap().1;
        // Two eastbound runs, joined by four right-angle corners. At the
        // return the facing tracks belong to opposite legs of this one proposal.
        for short_by in [1, 0] {
            let return_y = mm(10.) + 2 * (gap + width) - short_by;
            let turn = P::new(mm(7.), return_y);
            let target = P::new(mm(13.), return_y);
            route.waypoints = vec![P::mm(15., 10.), P::mm(15., 20.), P::mm(7., 20.), turn];
            route.to = Endpoint::Point(target);
            let plan = run(&board, &route).unwrap();
            let Section::Coupled { spine, .. } = &plan.sections[1] else { panic!("expected the coupled run") };
            if short_by == 1 {
                let blocked = plan.report.blocked.expect("truncated at the turn");
                assert_eq!(blocked.source, BlockSource::EarlierRun);
                assert_eq!(blocked.leg, NetId(1));
                // The southbound cut is legal. Turning east would extend the
                // outside miter into the first run, even for a one-step return.
                assert_eq!(spine.last(), Some(&turn));
            } else {
                assert_eq!(plan.report.blocked, None);
                assert_eq!(spine.last(), Some(&target));
            }
            let mut committed = board.clone();
            committed.apply_all(&plan.ops()).unwrap();
            let new = &committed.tracks[board.tracks.len()..];
            assert_eq!(new.len(), 2);
            assert!(minimum_gap(&new[0], &new[1]) >= gap);
        }
    }

    #[test]
    fn a_hairpin_keeps_each_legs_own_clearance() {
        let width = mm(0.2);
        let (board, mut route) = fixture(None, [width; 2], 0);
        let rules = resolve_rules(&board, None);
        let gap = rules.partner(route.net).unwrap().1;
        for (direction, leg) in [(1, NetId(0)), (-1, NetId(1))] {
            let key = RuleKey { class: board.nets[leg.ix()].class, item: RuleItem::Track };
            let clearance = rules.clearance(key, key, LayerId::F_CU);
            for short_by in [1, 0] {
                let y = mm(10.) + direction * (gap + 2 * width + clearance - short_by);
                route.waypoints = vec![P::mm(15., 10.), P::new(mm(15.), y)];
                route.to = Endpoint::Point(P::new(mm(8.), y));
                let plan = run(&board, &route).unwrap();
                let inner = proposed(if leg == NetId(0) { &plan.p } else { &plan.n });
                if short_by == 1 {
                    let block = plan.report.blocked.expect("the inside leg returns too close");
                    assert_eq!((block.leg, block.source), (leg, BlockSource::EarlierRun));
                    assert_eq!(inner.path.segs.len(), 2);
                } else {
                    assert_eq!(plan.report.blocked, None);
                    let pieces = verify::track_pieces(&board, inner, |_| true);
                    assert_eq!(pieces.len(), 3);
                    assert_eq!(fab_elec_core::clearance(pieces[0].shape(), pieces[2].shape()), clearance);
                }
            }
        }
    }

    #[test]
    fn a_square_cut_checks_the_copper_beyond_a_verified_inner_miter() {
        let (mut board, mut route) = fixture(None, [mm(0.2); 2], 0);
        route.waypoints = vec![P::mm(10., 10.)];
        route.to = Endpoint::Point(P::mm(10., 20.));
        // The fixed bend clears this mate track. A square cut at the corner
        // would extend the inside P leg farther east and reach it.
        track(&mut board, NetId(1), mm(0.2), P::mm(10.35, 10.18), P::mm(10.35, 10.30));
        assert_eq!(run(&board, &route).unwrap().blocked, None);
        track(&mut board, NetId(2), mm(0.2), P::mm(9., 18.), P::mm(11., 18.));
        let plan = run(&board, &route).unwrap();
        let blocked = plan.blocked.unwrap();
        assert_eq!((blocked.leg, blocked.source), (NetId(0), BlockSource::Board));
        let (p, n) = (proposed(&plan.p), proposed(&plan.n));
        assert_eq!(p.path.segs.len(), 1);
        assert_eq!(p.path.end().x, n.path.end().x);
        let need = fab_elec_board::drc::allowed_need(resolve_rules(&board, None).partner(p.net).unwrap().1);
        assert_eq!(p.path.end().x, mm(10.35) - p.w - need);
    }

    #[test]
    fn a_short_reverse_bend_keeps_the_last_constructible_station() {
        let (board, mut route) = fixture(None, [mm(0.2); 2], 0);
        let turn = P::mm(10., 10.);
        route.waypoints = vec![turn, P::mm(10.01, 10.01)];
        route.to = Endpoint::Point(P::mm(16., 10.01));
        let plan = run(&board, &route).unwrap();
        assert_eq!(plan.report.blocked.unwrap().source, BlockSource::EarlierRun);
        let Section::Coupled { spine, .. } = &plan.sections[1] else { panic!("expected the coupled run") };
        assert_eq!(spine.last(), Some(&turn));
    }

    #[test]
    fn ordinary_route_head_respects_its_mates_wider_gap() {
        let (mut board, _) = fixture(None, [mm(0.2); 2], 0);
        let start = ends(&board.tracks[0]).last().unwrap();
        let rules = resolve_rules(&board, None);
        let gap = rules.partner(NetId(0)).unwrap().1;
        let y = start.1.y + mm(0.2) + fab_elec_board::drc::allowed_need(gap) - 1;
        track(&mut board, NetId(1), mm(0.2), P::new(mm(8.), y), P::new(mm(14.), y));
        let cursor = P::new(mm(12.), start.1.y);
        let mut gesture =
            Gesture::new(GestureKind::Route, board.unit, board.generation, start.0, start.1, LayerId::F_CU);
        gesture.net = Some(NetId(0));
        gesture.cursor = cursor;
        gesture.trail = vec![start.1, cursor];
        let base = Base::new(Arc::new(board.clone()), Arc::new(rules), Arc::new(connect(&board, &[])), Arc::default());
        let sample = GestureSample { id: GestureId(1), seq: 1, gesture };
        let answer = crate::head::answer(&base, &RBase::build(&board), &sample, &mut Default::default(), ProposalId(1));
        let proposal = answer.proposal().expect("valid ordinary route gesture");
        match crate::worker::gesture_event(&base, proposal) {
            crate::RouteEvent::Refused(reason) => assert!(reason.text.contains("pair gap")),
            crate::RouteEvent::Proposal(p) => assert!(p.verified && p.head.is_some_and(|h| h.path.end() != cursor)),
            _ => panic!("expected a stopped or refused route"),
        }
    }
}

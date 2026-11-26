//! One length edit on a fixed-ended span. All geometry is measured against the supplied world.
use crate::{
    verify,
    world::{RItem, World},
};
use fab_elec_board::{
    length::{self, LengthReadout},
    Anchor, Board, BoardOp, Connectivity, Item, RuleItem, RuleKey, RuleMatrix, Track,
};
use fab_elec_circuit::Circuit;
use fab_elec_core::{ItemId, NetId, Nm, OnTrack, Path, Seg, P};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Span {
    Vertices { track: ItemId, from: usize, to: usize },
    Nearest(P),
    Longest,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Side {
    Left,
    Right,
    Both,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Target {
    Match,
    Mate,
    Length(i64),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Tune {
    pub net: NetId,
    pub span: Span,
    pub target: Target,
    pub side: Option<Side>,
    pub amplitude: Option<Nm>,
    pub pitch: Option<Nm>,
}

pub struct Request<'a> {
    pub tune: &'a Tune,
    pub circuit: &'a Circuit,
    pub connectivity: &'a Connectivity,
}

/// Whole-net lengths in nm. For a pair, the increase in copper outside its coupled run.
#[derive(Clone, PartialEq, Debug)]
pub struct Report {
    pub reached: i64,
    pub target: i64,
    pub short_by: i64,
    pub uncoupled_added: Option<i64>,
    pub partial: bool,
    pub no_stackup: bool,
}
fab_elec_core::canon_struct!(Report { reached, target, short_by, uncoupled_added, partial, no_stackup });

pub struct MeanderPlan {
    pub ops: Vec<BoardOp>,
    pub report: Report,
    pub blocked: Option<String>,
}

fn vertex(path: &Path, index: usize) -> P {
    if index == 0 {
        path.start
    } else {
        path.segs[index - 1].end()
    }
}

/// Orthonormal frame of the chord. Round only when putting a vertex on the nm grid.
struct Chord {
    a: P,
    ux: f64,
    uy: f64,
    len: f64,
}

impl Chord {
    fn new(a: P, b: P) -> Option<Self> {
        let (dx, dy) = (f64::from(b.x) - f64::from(a.x), f64::from(b.y) - f64::from(a.y));
        let len = dx.hypot(dy);
        (len > 0.).then(|| Self { a, ux: dx / len, uy: dy / len, len })
    }

    fn local(&self, p: P) -> (f64, f64) {
        let (x, y) = (f64::from(p.x) - f64::from(self.a.x), f64::from(p.y) - f64::from(self.a.y));
        (x * self.ux + y * self.uy, y * self.ux - x * self.uy)
    }

    fn point(&self, x: f64, y: f64) -> Option<P> {
        let x_nm = (f64::from(self.a.x) + x * self.ux - y * self.uy).round();
        let y_nm = (f64::from(self.a.y) + x * self.uy + y * self.ux).round();
        let range = f64::from(i32::MIN)..=f64::from(i32::MAX);
        (range.contains(&x_nm) && range.contains(&y_nm)).then(|| P::new(x_nm as Nm, y_nm as Nm))
    }
}

impl Tune {
    /// An explicit span in track order. The pointer is measured normal to that chord
    /// so reversing the direction of the drag still folds on the indicated side.
    pub(crate) fn dragged(t: &Track, from: usize, to: usize, cursor: P) -> Result<Self, String> {
        let (from, to) = (from.min(to), from.max(to));
        if from == to || to > t.path.segs.len() {
            return Err("drag along the track to select two vertices".into());
        }
        let chord = Chord::new(vertex(&t.path, from), vertex(&t.path, to)).ok_or("the span's endpoints coincide")?;
        let offset = chord.local(cursor).1;
        let amplitude = offset.abs().round() as Nm;
        if amplitude == 0 {
            return Err("drag to the side of the track to set the fold height".into());
        }
        Ok(Self {
            net: t.net, span: Span::Vertices { track: t.id, from, to }, target: Target::Match,
            side: Some(if offset > 0. { Side::Left } else { Side::Right }), amplitude: Some(amplitude), pitch: None,
        })
    }
}

/// Width of an unlocked track the Tune tool can refold.
pub fn uniform_width(t: &Track) -> Result<Nm, String> {
    if t.locked {
        return Err("the selected track is locked".into());
    }
    let width = match &t.widths {
        None => Some(t.w),
        Some(ws) if ws.len() == t.path.segs.len() + 1 => {
            ws.first().copied().filter(|w| ws.iter().all(|other| other == w))
        }
        _ => None,
    };
    width.filter(|w| *w > 0).ok_or_else(|| "tuning needs a uniform positive track width".into())
}

/// Classify a segment relative to a baseline. The two endpoints of each direction
/// vector were rounded to the grid. Their angular error grows with the length ratio.
fn heading(axis: &Chord, a: P, b: P) -> Option<i8> {
    let (ax, ay) = axis.local(a);
    let (bx, by) = axis.local(b);
    let (dx, dy) = (bx - ax, by - ay);
    let epsilon = std::f64::consts::SQRT_2 * (1. + dx.hypot(dy) / axis.len);
    if dx > 0. && dy.abs() <= epsilon {
        Some(0)
    } else if dy != 0. && dx.abs() <= epsilon {
        Some(if dy > 0. { 2 } else { -2 })
    } else if dx > 0. && (dx - dy.abs()).abs() <= epsilon * std::f64::consts::SQRT_2 {
        Some(if dy > 0. { 1 } else { -1 })
    } else {
        None
    }
}

/// A fold departs and returns through 45-degree shoulders, with a parallel top.
/// Recognise only completed folds between baseline segments. An ordinary corner or
/// square dogleg ends the span; no metadata is needed to select a previous tune.
fn spans(path: &Path) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut from = 0;
    while from < path.segs.len() {
        let Seg::Line(b) = path.segs[from] else {
            from += 1;
            continue;
        };
        let Some(axis) = Chord::new(vertex(path, from), b) else {
            from += 1;
            continue;
        };
        let dir = |i: usize| match path.segs.get(i) {
            Some(Seg::Line(b)) => heading(&axis, vertex(path, i), *b),
            _ => None,
        };
        let take = |i: &mut usize, d| {
            while dir(*i) == Some(d) {
                *i += 1;
            }
        };
        let mut end = from + 1;
        loop {
            take(&mut end, 0);
            let Some(sign @ (1 | -1)) = dir(end) else { break };
            let mut i = end;
            let limb = |i: &mut usize, sign| {
                if dir(*i) != Some(sign) {
                    return false;
                }
                take(i, sign);
                if dir(*i) == Some(2 * sign) {
                    take(i, 2 * sign);
                    if dir(*i) != Some(sign) {
                        return false;
                    }
                    take(i, sign);
                }
                true
            };
            if !limb(&mut i, sign) || dir(i) != Some(0) {
                break;
            }
            take(&mut i, 0);
            if !limb(&mut i, -sign) || dir(i) != Some(0) {
                break;
            }
            let (x, y) = axis.local(vertex(path, i));
            if y.abs() > std::f64::consts::SQRT_2 * (1. + x.abs() / axis.len) {
                break;
            }
            end = i;
        }
        spans.push((from, end));
        from = end;
    }
    spans
}

fn selections<'a>(board: &'a Board, tune: &Tune) -> Result<Vec<(&'a Track, usize, usize)>, String> {
    if let Span::Vertices { track, from, to } = tune.span {
        let t = board.tracks.iter().find(|t| t.id == track && t.net == tune.net).ok_or("no such track on this net")?;
        uniform_width(t)?;
        if from >= to || to > t.path.segs.len() {
            return Err("select two distinct vertices on the track".into());
        }
        return Ok(vec![(t, from, to)]);
    }
    let mut choices = Vec::new();
    let mut reason = "the net has no straight span to tune".to_owned();
    for t in board.tracks.iter().filter(|t| t.net == tune.net) {
        if let Err(why) = uniform_width(t) {
            reason = why;
            if matches!(tune.span, Span::Longest) {
                continue;
            }
        }
        let mut runs = spans(&t.path);
        if matches!(tune.span, Span::Nearest(_)) {
            runs.extend(
                t.path.segs.iter().enumerate().filter_map(|(i, s)| matches!(s, Seg::Arc { .. }).then_some((i, i + 1))),
            );
        }
        for (from, to) in runs {
            let score = match tune.span {
                Span::Nearest(p) => {
                    let sub = Path { start: vertex(&t.path, from), segs: t.path.segs[from..to].to_vec() };
                    p.dist2(sub.locate(p).1)
                }
                _ => -vertex(&t.path, from).dist2(vertex(&t.path, to)),
            };
            choices.push((score, t, from, to));
        }
    }
    choices.sort_by_key(|(score, t, from, _)| (*score, t.id, *from));
    if choices.is_empty() {
        return Err(reason);
    }
    if matches!(tune.span, Span::Nearest(_)) {
        choices.truncate(1);
    }
    Ok(choices.into_iter().map(|(_, t, a, b)| (t, a, b)).collect())
}

/// Axis separation of adjacent perpendicular legs is pitch. The four equal shoulders
/// leave at least width + clearance between parallel runs, including adjacent folds.
struct Fold {
    pitch: f64,
    bevel: f64,
}

impl Fold {
    fn points(&self, chord: &Chord, start: f64, sign: f64, height: f64) -> Option<Vec<P>> {
        let b = self.bevel.min(height / 2.);
        let l = start + self.pitch / 2.;
        let r = l + self.pitch;
        let mut points = [
            (l - b, 0.),
            (l, b),
            (l, height - b),
            (l + b, height),
            (r - b, height),
            (r, height - b),
            (r, b),
            (r + b, 0.),
        ]
        .into_iter()
        .map(|(x, y)| chord.point(x, y * sign))
        .collect::<Option<Vec<_>>>()?;
        points.dedup();
        Some(points)
    }

    fn height_for(&self, gain: f64) -> f64 {
        // Four diagonals of extent b replace 8b of horizontal and vertical travel.
        let slope = 2. * (std::f64::consts::SQRT_2 - 1.);
        let transition = 2. * self.bevel;
        if gain <= slope * transition {
            gain / slope
        } else {
            transition + (gain - slope * transition) / 2.
        }
    }
}

fn lines(points: &[P]) -> f64 {
    points.windows(2).map(|p| (p[0].dist2(p[1]) as f64).sqrt()).sum()
}

fn mate_side(mates: &[&Track], chord: &Chord, x: f64) -> Result<Side, String> {
    let p = chord.point(x, 0.).ok_or("span outside the board coordinate range")?;
    let nearest = mates
        .iter()
        .map(|t| t.path.locate(p).1)
        .min_by_key(|q| p.dist2(*q))
        .ok_or("the mate has no copper on this layer to tune away from")?;
    let offset = chord.local(nearest).1;
    if offset.abs() < 0.5 {
        return Err("the mate lies on the span's chord; there is no side away from it".into());
    }
    Ok(if offset > 0. { Side::Right } else { Side::Left })
}

fn select_side(requested: Option<Side>, away: Option<Side>, odd: bool) -> Result<f64, String> {
    if away.is_some() && requested.is_some() && away != requested {
        return Err("a pair leg must fold away from its mate".into());
    }
    Ok(match requested.or(away).unwrap_or(Side::Both) {
        Side::Right => -1.,
        Side::Both if odd => -1.,
        _ => 1.,
    })
}

fn remap(
    board: &Board,
    world: &World,
    old: &Track,
    new: &Track,
    from: usize,
    to: usize,
) -> Result<Vec<BoardOp>, String> {
    let mut ops = vec![BoardOp::Replace(Item::Track(new.clone()))];
    for &id in world.base.tracks_on(old.id) {
        let Some(Item::Track(mut branch)) = board.item(id) else { continue };
        let mut changed = false;
        for end in &mut branch.ends {
            let Anchor::Track { id: host, at } = end else { continue };
            if *host != old.id || (at.seg as usize) < from {
                continue;
            }
            let p = old.path.point_on(*at);
            let mapped = if at.seg as usize >= to {
                let remaining = old.path.segs.len().saturating_sub(at.seg as usize);
                let seg = new.path.segs.len() - remaining;
                Some(OnTrack { seg: seg as u32, ..*at })
            } else {
                fab_elec_board::ops::on_track(p, &new.path)
            };
            let next = mapped.filter(|at| new.path.point_on(*at) == p).ok_or_else(|| {
                format!("junction on track {} at ({}, {}) would leave the tuned copper", id.0, p.x, p.y)
            })?;
            changed |= next != *at;
            *at = next;
        }
        if changed {
            ops.push(BoardOp::Replace(Item::Track(branch)));
        }
    }
    Ok(ops)
}

/// Length at the design gap, counting the union when more than one mate piece covers
/// a section. Lines couple along overlapping projections, concentric arcs along overlapping
/// angles. A line and an arc can be tangent only at a point. All tolerances here are the
/// projection error of rounding the two physical endpoints to the nm grid.
fn coupled(mates: &[&Track], width: Nm, path: &Path, gap: Nm) -> f64 {
    use fab_elec_core::geom::ArcGeom;
    use std::f64::consts::TAU;
    let arc = |a, s: &Seg| match *s {
        Seg::Arc { mid, end } => ArcGeom::new(a, mid, end),
        _ => None,
    };
    let mut total = 0.;
    for (a, seg) in path.steps() {
        let own_arc = arc(a, &seg);
        let Some(axis) = Chord::new(a, seg.end()) else { continue };
        let mut intervals = Vec::new();
        for other in mates {
            for (i, (p, s)) in other.path.steps().enumerate() {
                let spacing = f64::from(gap) + (f64::from(width) + f64::from(other.seg_width(i))) / 2.;
                match (&own_arc, arc(p, &s)) {
                    (None, None) => {
                        let (x, y) = axis.local(p);
                        let (u, v) = axis.local(s.end());
                        if (y - v).abs() <= 2. && (y.abs() - spacing).abs() <= 1.5 && (v.abs() - spacing).abs() <= 1.5 {
                            intervals.push((x.min(u).max(0.), x.max(u).min(axis.len)));
                        }
                    }
                    (Some(t), Some(m))
                        if (t.center.0 - m.center.0).hypot(t.center.1 - m.center.1) <= 1.5
                            && ((t.radius - m.radius).abs() - spacing).abs() <= 1.5 =>
                    {
                        let begin = t.start + t.sweep.min(0.);
                        let start = (m.start + m.sweep.min(0.) - begin).rem_euclid(TAU);
                        for lo in [start - TAU, start] {
                            intervals.push((lo.max(0.) * t.radius, (lo + m.sweep.abs()).min(t.sweep.abs()) * t.radius));
                        }
                    }
                    _ => {}
                }
            }
        }
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut end: f64 = 0.;
        for (lo, hi) in intervals {
            total += (hi - lo.max(end)).max(0.);
            end = end.max(hi);
        }
    }
    total
}

struct Search<'a, 'w> {
    board: &'a Board,
    rules: &'a RuleMatrix,
    world: &'a World<'w>,
    tune: &'a Tune,
    conn: &'a Connectivity,
    mate: Option<(NetId, Nm)>,
    target: i64,
    current: length::NetLength,
}

impl Search<'_, '_> {
    /// DRC exempts same-net copper. A new loop that lands on it would electrically
    /// bypass the length we just measured. Original connections on the baseline stay.
    fn clears_net(&self, track: &Track, from: usize, to: usize, pieces: &[RItem], clearance: Nm) -> bool {
        use std::ops::ControlFlow;
        pieces.iter().all(|piece| {
            let mut clear = true;
            self.world.query(track.layer, piece.bbox.inflate(clearance), |_, other| {
                let replaced = other.item == Some(track.id) && (from..to).contains(&(other.rank as usize));
                if !replaced
                    && other.net() == Some(track.net)
                    && fab_elec_core::clearance(piece.shape(), other.shape()) < clearance.max(1)
                {
                    clear = false;
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            });
            clear
        })
    }

    fn span(&self, t: &Track, from: usize, to: usize) -> Result<MeanderPlan, String> {
        let width = uniform_width(t)?;
        let (a, b) = (vertex(&t.path, from), vertex(&t.path, to));
        let chord = Chord::new(a, b).ok_or("the span's endpoints coincide")?;
        let original = Path { start: a, segs: t.path.segs[from..to].to_vec() };
        let key = RuleKey { item: RuleItem::Track, class: self.board.nets[t.net.ix()].class };
        let clearance = self.rules.clearance(key, key, t.layer);
        let spacing = f64::from(width) + f64::from(clearance);
        let requested_pitch = self.tune.pitch.map(f64::from).unwrap_or(4. * f64::from(width));
        // Reserve the rounding error of both legs and a nonzero chamfer on the integer grid.
        let pitch = requested_pitch.max(spacing + 6.);
        let fold = Fold { pitch, bevel: (pitch / 4.).min((pitch - spacing - 2.) / 2.) };
        let mut new = t.clone();
        new.path.segs.splice(from..to, [Seg::Line(b)]);
        let chord_length = length::with_path(self.board, self.conn, t, &new.path).nm;
        let mut outstanding = (self.target - chord_length) as f64;
        let mates: Vec<_> = self
            .mate
            .into_iter()
            .flat_map(|(net, _)| {
                self.board.tracks.iter().filter(move |other| other.net == net && other.layer == t.layer)
            })
            .collect();
        let mut points = vec![a];
        let mut station = 0.;
        let mut odd = false;
        let mut gained = 0.;
        while station + 2. * pitch <= chord.len && outstanding > 0.5 {
            let side = self.mate.map(|_| mate_side(&mates, &chord, station + pitch)).transpose()?;
            let sign = select_side(self.tune.side, side, odd)?;
            let limit = fold.height_for(outstanding).min(self.tune.amplitude.map_or(f64::INFINITY, f64::from));
            let legal = |height| {
                let pts = fold.points(&chord, station, sign, height)?;
                let pieces: Vec<_> =
                    pts.windows(2).map(|p| verify::track_piece(self.board, t, from, p[0], &Seg::Line(p[1]))).collect();
                (!verify::meets(self.world, self.rules, self.board, &pieces)
                    && self.clears_net(t, from, to, &pieces, clearance))
                .then_some(pts)
            };
            let height = limit.round() as i64;
            let mut bump = legal(height as f64);
            if bump.is_none() {
                let (mut low, mut high) = (0, height);
                while high - low > 1 {
                    let mid = low + (high - low) / 2;
                    if let Some(pts) = legal(mid as f64) {
                        low = mid;
                        bump = Some(pts);
                    } else {
                        high = mid;
                    }
                }
            }
            if let Some(pts) = bump {
                let extra = lines(&pts) - (pts[0].dist2(*pts.last().unwrap()) as f64).sqrt();
                if extra > 0. {
                    points.extend(pts);
                    outstanding -= extra;
                    gained += extra;
                }
            }
            odd = !odd;
            station += 2. * pitch;
        }
        points.push(b);
        points.dedup();
        let added = points.len() - 1;
        new.path.segs.splice(from..from + 1, points.iter().skip(1).copied().map(Seg::Line));
        if let Some(widths) = &mut new.widths {
            widths.resize(new.path.segs.len() + 1, width);
        }
        let proposed = length::with_path(self.board, self.conn, t, &new.path).nm;
        let pieces = verify::track_pieces(self.board, &new, |i| (from..from + added).contains(&i));
        // The verifier checks crossings and edge clearance. One endpoint also has to be
        // on the material: an imported span can already lie entirely outside the board.
        let keep = gained > 0.
            && proposed > self.current.nm
            && verify::verify(self.world, self.rules, self.board, &pieces).is_empty()
            && (self.board.outline.is_empty()
                || self.board.outline_polyset((width / 2).clamp(1, fab_elec_board::ARC_ERR)).contains(a));
        let (reached, ops, uncoupled_added) = if keep {
            let uncoupled = self.mate.map(|(_, gap)| {
                let after = Path { start: a, segs: new.path.segs[from..from + added].to_vec() };
                let lost = coupled(&mates, width, &original, gap) - coupled(&mates, width, &after, gap);
                (after.length() - original.length() + lost).round().max(0.) as i64
            });
            (proposed, remap(self.board, self.world, t, &new, from, to)?, uncoupled)
        } else {
            (self.current.nm, Vec::new(), self.mate.map(|_| 0))
        };
        let short_by = (self.target - reached).max(0);
        Ok(MeanderPlan {
            ops,
            report: Report {
                reached, target: self.target, short_by, uncoupled_added, partial: self.current.partial,
                no_stackup: self.current.measurement == length::Measurement::NoStackup,
            },
            blocked: (short_by > 1).then(|| "the corridor holds no more folds".to_owned()),
        })
    }
}

pub fn plan(board: &Board, rules: &RuleMatrix, world: &World, request: Request<'_>) -> Result<MeanderPlan, String> {
    let Request { tune, circuit, connectivity } = request;
    let net = tune.net;
    let name = &board.nets.get(net.ix()).ok_or("the net is no longer on the board")?.name;
    if tune.pitch.is_some_and(|p| p <= 0) || tune.amplitude.is_some_and(|a| a <= 0) {
        return Err("pitch and amplitude must be positive".into());
    }
    let mate = rules.partner(net);
    let requested: Vec<_> = std::iter::once(net).chain(mate.map(|p| p.0)).collect();
    let read = LengthReadout::new(board, circuit, connectivity, &requested);
    if read.branched.contains(&net) {
        return Err(format!("{name} is branched; there is no pin-to-pin length to tune"));
    }
    let measured = |n: &NetId| read.lengths.get(n).map(|l| l.nm);
    let current = *read.lengths.get(&net).ok_or_else(|| format!("{name} has no measured length"))?;
    let target = match tune.target {
        Target::Length(n) => n,
        Target::Mate => measured(&mate.ok_or("target mate needs a declared pair")?.0).ok_or("the mate has no measured length")?,
        Target::Match => {
            let groups: Vec<_> = board.length_matches.iter().filter(|g| g.nets.contains(&net)).collect();
            match groups.as_slice() {
                [] => return Err(format!("{name} is in no length match; supply a target")),
                [group] => group.nets.iter().map(measured).collect::<Option<Vec<_>>>().and_then(|l| l.into_iter().max())
                    .ok_or_else(|| format!("{name}'s length match gives no measured target"))?,
                _ => {
                    let names = groups
                        .iter()
                        .map(|g| g.nets.iter().map(|n| board.nets.get(n.ix()).map_or("?", |b| b.name.as_str())).collect::<Vec<_>>().join(", "))
                        .collect::<Vec<_>>()
                        .join(" / ");
                    return Err(format!("{name} is in several length matches ({names}); supply a target"));
                }
            }
        }
    };
    if current.nm >= target {
        return Err(format!("{name} is already at or above the target"));
    }
    let search = Search { board, rules, world, tune, conn: connectivity, mate, target, current };
    let mut last = None;
    for (track, from, to) in selections(board, tune)? {
        let result = search.span(track, from, to)?;
        if !matches!(tune.span, Span::Longest) || !result.ops.is_empty() {
            return Ok(result);
        }
        last = Some(result);
    }
    last.ok_or_else(|| "no free straight span on the net".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::RBase;
    use fab_elec_board::{connect, resolve_rules, BoardNet, NetPair, Origin};
    use fab_elec_core::{mm, BoardId, ClassId, Contour, LayerId};

    fn setup() -> (Board, Tune) {
        let mut board = Board::new(BoardId(0), "tune");
        board.outline = vec![Contour::rect(P::ZERO, P::mm(45., 20.), 0)];
        board.nets = ["P", "N"].map(|name| BoardNet { name: name.into(), class: ClassId(0) }).to_vec();
        board.tracks.push(Track {
            id: ItemId(0),
            net: NetId(0),
            layer: LayerId::F_CU,
            w: mm(0.2),
            widths: None,
            path: Path::new(P::mm(2., 5.)).line(P::mm(32., 5.)),
            ends: [Anchor::Free; 2],
            locked: false,
            origin: Origin::Authored,
        });
        board.next_item = 1;
        board.reindex();
        let tune = Tune {
            net: NetId(0),
            span: Span::Longest,
            target: Target::Length(36_000_000),
            side: Some(Side::Left),
            amplitude: Some(mm(1.5)),
            pitch: Some(mm(2.)),
        };
        (board, tune)
    }

    fn run(board: &Board, tune: &Tune) -> Result<MeanderPlan, String> {
        let base = RBase::build(board);
        plan(
            board,
            &resolve_rules(board, None),
            &World::new(&base),
            Request { tune, circuit: &Circuit::default(), connectivity: &connect(board, &[]) },
        )
    }

    #[test]
    fn tune_samples_return_the_shared_planners_ops_measurement_and_refusal() {
        use crate::{Base, Gesture, GestureId, GestureKind, GestureSample, ProposalId};
        use std::sync::Arc;
        let (mut board, mut tune) = setup();
        let mut reference = board.tracks[0].clone();
        reference.id = ItemId(1);
        reference.net = NetId(1);
        reference.path = Path::new(P::mm(1., 15.)).line(P::mm(37., 15.));
        board.tracks.push(reference);
        board.next_item = 2;
        board.length_matches.push(length::LengthMatch { nets: vec![NetId(0), NetId(1)], tol: mm(0.1) });
        board.reindex();
        let b = Arc::new(board);
        let base = Base::new(b.clone(), Arc::new(resolve_rules(&b, None)), Arc::new(connect(&b, &[])), Arc::default());
        let world = RBase::build(&b);
        let t = &b.tracks[0];
        let kind = GestureKind::Tune { track: t.id, from: 1, to: 0, circuit: Arc::default() };
        let mut g = Gesture::new(kind, b.unit, b.generation, Anchor::Free, t.path.end(), t.layer);
        g.net = Some(t.net);
        tune.target = Target::Match;
        tune.span = Span::Vertices { track: t.id, from: 0, to: 1 };
        tune.pitch = None;
        let mut state = crate::head::GestureState::default();
        for (seq, height) in [(1, 1.5), (2, 0.01), (3, -1.)] {
            tune.amplitude = Some(mm(f64::abs(height)));
            tune.side = Some(if height > 0. { Side::Left } else { Side::Right });
            g.cursor = P::mm(2., 5. + height);
            let sample = GestureSample { id: GestureId(1), seq, gesture: g.clone() };
            let p = crate::head::answer(&base, &world, &sample, &mut state, ProposalId(seq as u32)).proposal().unwrap();
            let expected = run(&b, &tune).unwrap();
            assert_eq!(p.ops, expected.ops);
            assert_eq!(p.metrics.tune, Some(Ok(expected.report)));
            assert!(p.verified && p.last);
        }
        g.net = Some(NetId(1));
        let sample = GestureSample { id: GestureId(1), seq: 4, gesture: g };
        let p = crate::head::answer(&base, &world, &sample, &mut state, ProposalId(4)).proposal().unwrap();
        assert!(p.ops.is_empty() && !p.verified);
        assert!(matches!(crate::worker::gesture_event(&base, p), crate::RouteEvent::Refused(r) if r.text.contains("track")));
    }

    #[test]
    fn length_folds_keep_endpoints_and_spacing_and_refold_from_the_chord() {
        let (mut board, mut tune) = setup();
        let original = board.tracks[0].clone();
        let first = run(&board, &tune).unwrap();
        board.apply_all(&first.ops).unwrap();
        let folded = &board.tracks[0];
        assert_eq!(
            (folded.id, folded.ends, folded.path.start, folded.path.end()),
            (original.id, original.ends, original.path.start, original.path.end())
        );
        let read = length::net_lengths(&board, &connect(&board, &[]), &[tune.net]);
        assert_eq!(read[&tune.net].nm, first.report.reached);
        assert!((36_000_000 - first.report.reached).abs() <= 1000, "{:?}", first.report);
        let tops: Vec<_> = folded
            .path
            .steps()
            .filter_map(|(a, s)| {
                let b = s.end();
                (a.y == b.y && a.y > original.path.start.y).then_some((a, b))
            })
            .collect();
        assert!(tops.len() >= 2);
        let key = RuleKey { class: ClassId(0), item: RuleItem::Track };
        let clearance = resolve_rules(&board, None).clearance(key, key, original.layer);
        for top in tops.windows(2) {
            assert!(top[1].0.x - top[0].1.x - original.w >= clearance);
        }

        let end = folded.path.segs.len();
        tune.span = Span::Vertices { track: original.id, from: 0, to: end };
        tune.target = Target::Length(39_000_000);
        let second = run(&board, &tune).unwrap();
        tune.span = Span::Nearest(tops[0].0);
        assert_eq!(run(&board, &tune).unwrap().ops, second.ops);
        let (straight, _) = setup();
        let direct = run(&straight, &tune).unwrap();
        assert_eq!(second.ops, direct.ops, "refolding exactly reproduces tuning the chord");
    }

    #[test]
    fn clearance_limited_corridor_keeps_legal_folds_and_reports_the_shortfall() {
        let (mut board, mut tune) = setup();
        let mut wall = board.tracks[0].clone();
        wall.id = ItemId(1);
        wall.net = NetId(1);
        wall.path = wall.path.translated(P::mm(0., 1.2));
        board.tracks.push(wall);
        board.reindex();
        tune.amplitude = Some(mm(4.));
        tune.target = Target::Length(75_000_000);
        let answer = run(&board, &tune).unwrap();
        assert!(answer.report.reached > 30_000_000 && answer.report.short_by > 0);
        assert_eq!(answer.report.short_by, 75_000_000 - answer.report.reached);
        assert!(answer.blocked.is_some());
        let base = RBase::build(&board);
        let rules = resolve_rules(&board, None);
        board.apply_all(&answer.ops).unwrap();
        let track = &board.tracks[0];
        assert!(verify::verify(&World::new(&base), &rules, &board, &verify::track_pieces(&board, track, |_| true))
            .is_empty());
        assert!(track.path.segs.iter().all(|s| s.end().y < board.tracks[1].path.start.y));
        board.outline = vec![Contour::rect(P::mm(2., 12.), P::mm(42., 18.), 0)];
        let outside = run(&board, &tune).unwrap();
        assert!(outside.ops.is_empty());
        assert_eq!(outside.report.reached, answer.report.reached);
    }

    #[test]
    fn oblique_uniform_width_runs_refold_and_an_ordinary_dogleg_stays_separate() {
        let (mut board, mut tune) = setup();
        let track = &mut board.tracks[0];
        let a = track.path.start;
        track.path = Path::new(a).line(P::mm(32., 12.));
        track.widths = Some(vec![mm(0.25); 2]);
        let result = run(&board, &tune).unwrap();
        assert!((result.report.reached - result.report.target).abs() <= 1_000);
        board.apply_all(&result.ops).unwrap();
        let t = &board.tracks[0];
        assert_eq!(spans(&t.path), vec![(0, t.path.segs.len())]);
        assert_eq!(t.widths.as_ref().unwrap(), &vec![mm(0.25); t.path.segs.len() + 1]);
        let dogleg = Path::new(a).line(P::mm(4., 5.)).line(P::mm(4., 7.))
            .line(P::mm(6., 7.)).line(P::mm(6., 5.)).line(P::mm(8., 5.));
        assert_eq!(spans(&dogleg).len(), dogleg.segs.len());
        tune.target = Target::Length(40_000_000);
        board.tracks[0].widths.as_mut().unwrap()[0] = mm(0.4);
        assert!(run(&board, &tune).err().unwrap().contains("uniform"));
        board.tracks[0].locked = true;
        assert!(run(&board, &tune).err().unwrap().contains("locked"));
    }

    #[test]
    fn a_displaced_junction_refuses_the_span() {
        let (mut board, mut tune) = setup();
        let host = &board.tracks[0];
        let junction = P::mm(4., 5.);
        let at = fab_elec_board::ops::on_track(junction, &host.path).unwrap();
        let mut branch = host.clone();
        branch.id = ItemId(1);
        branch.path = Path::new(junction).line(P::mm(4., 3.));
        branch.ends[0] = Anchor::Track { id: host.id, at };
        board.tracks.push(branch);
        board.reindex();
        tune.span = Span::Vertices { track: ItemId(0), from: 0, to: 1 };
        assert!(run(&board, &tune).err().unwrap().contains("junction"));
    }

    #[test]
    fn tuning_to_mate_uses_one_trimmed_bump_away_from_the_other_leg() {
        let (mut board, mut tune) = setup();
        let mut mate = board.tracks[0].clone();
        mate.id = ItemId(1);
        mate.net = NetId(1);
        mate.path = Path::new(P::mm(2., 5.4)).line(P::mm(32.025, 5.4));
        board.tracks.push(mate);
        board.pairs.push(NetPair { p: tune.net, n: NetId(1), gap: Some(mm(0.2)) });
        board.reindex();
        tune.target = Target::Mate;
        tune.side = None;
        let answer = run(&board, &tune).unwrap();
        assert_eq!(answer.report.target, 30_025_000);
        assert!((answer.report.reached - answer.report.target).abs() <= 1);
        let BoardOp::Replace(Item::Track(t)) = &answer.ops[0] else { panic!("track edit") };
        assert!(t.path.segs.iter().all(|s| s.end().y <= t.path.start.y));
        let apex = t.path.segs.iter().map(|s| s.end().y).min().unwrap();
        assert!(t.path.start.y - apex < tune.amplitude.unwrap());
        assert_eq!(t.path.steps().filter(|(a, s)| a.y == apex && s.end().y == apex).count(), 1);
        let uncoupled = t
            .path
            .steps()
            .filter(|(a, s)| a.y < t.path.start.y || s.end().y < t.path.start.y)
            .map(|(a, s)| (a.dist2(s.end()) as f64).sqrt())
            .sum::<f64>();
        assert_eq!(answer.report.uncoupled_added, Some(uncoupled.round() as i64));
        tune.side = Some(Side::Left);
        assert!(run(&board, &tune).err().unwrap().contains("away from its mate"));
    }
}

//! Local board input. Only completing a gesture produces a model action.
use super::{BoardItem, BoardView, Rect};
use crate::model::{Action, ItemStyle};
use makepad_town_core::{
    entity::{field, kind},
    event::{Change, Id, Key, Value},
};
use makepad_widgets::{dvec2, log, DVec2, KeyCode, KeyModifiers, MouseCursor};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Default, Clone, PartialEq)]
pub struct Selection {
    pub ids: BTreeSet<Id>,
    pub anchor: Option<Id>,
}

impl Selection {
    fn add(&mut self, id: Id) {
        self.ids.insert(id);
        self.anchor = Some(id);
    }

    fn remove(&mut self, id: &Id) {
        self.ids.remove(id);
        if self.anchor.as_ref() == Some(id) {
            self.anchor = self.ids.last().copied();
        }
    }
}

// Geometry stays in signed ticks through capture, scaling and release. Endpoints
// and distances between two i64 positions can be wider than an individual field.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FixedRect {
    pub(super) lo: [i128; 2],
    pub(super) hi: [i128; 2],
}

impl FixedRect {
    fn item(item: &BoardItem) -> Self {
        let a = [i128::from(item.x), i128::from(item.y)];
        let b = [a[0] + i128::from(item.w), a[1] + i128::from(item.h)];
        Self { lo: [a[0].min(b[0]), a[1].min(b[1])], hi: [a[0].max(b[0]), a[1].max(b[1])] }
    }

    fn union(self, r: Self) -> Self {
        Self { lo: [self.lo[0].min(r.lo[0]), self.lo[1].min(r.lo[1])],
            hi: [self.hi[0].max(r.hi[0]), self.hi[1].max(r.hi[1])] }
    }

    fn world(self) -> Rect {
        Rect::new(self.lo[0] as f64 / 64., self.lo[1] as f64 / 64., self.hi[0] as f64 / 64., self.hi[1] as f64 / 64.)
    }

    fn span(self, axis: usize) -> i128 {
        self.hi[axis] - self.lo[axis]
    }
}

// Rounded a*b/d for nonnegative dimensions. The slow path handles just the
// extreme endpoints where the product is wider than i128.
fn proportion(a: i128, b: i128, d: i128) -> i128 {
    if d == 0 { return 0 }
    if let Some(product) = a.checked_mul(b).and_then(|v| v.checked_add(d / 2)) {
        return product / d;
    }
    let (mut q, mut r) = (0i128, 0i128);
    for bit in (0..128 - b.leading_zeros()).rev() {
        r = 2 * r + if (b >> bit) & 1 == 1 { a } else { 0 };
        q = q.saturating_mul(2).saturating_add(r / d);
        r %= d;
    }
    q.saturating_add(i128::from(r >= d - r))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle([i8; 2]);

impl Handle {
    pub const ALL: [Self; 8] = [
        Self([-1, -1]), Self([0, -1]), Self([1, -1]), Self([1, 0]),
        Self([1, 1]), Self([0, 1]), Self([-1, 1]), Self([-1, 0]),
    ];

    pub fn point(self, rect: Rect, zoom: f64) -> DVec2 {
        // Centre the handle on the outside selection stroke at any zoom.
        let offset = 3. / zoom;
        let at = |lo, hi, side| match side { -1 => lo - offset, 1 => hi + offset, _ => (lo + hi) * 0.5 };
        dvec2(at(rect.x0, rect.x1, self.0[0]), at(rect.y0, rect.y1, self.0[1]))
    }

    fn cursor(self) -> MouseCursor {
        match self.0 {
            [0, _] => MouseCursor::NsResize,
            [_, 0] => MouseCursor::EwResize,
            [x, y] if x == y => MouseCursor::NwseResize,
            _ => MouseCursor::NeswResize,
        }
    }
}

#[derive(Clone, Copy)]
pub enum AlignTo { Left, Centre, Right, Top, Middle, Bottom }

impl AlignTo {
    pub const ALL: [Self; 6] = [Self::Left, Self::Centre, Self::Right, Self::Top, Self::Middle, Self::Bottom];

    fn offset(self, item: FixedRect, bounds: FixedRect) -> [i128; 2] {
        let (axis, edge) = match self {
            Self::Left => (0, -1), Self::Centre => (0, 0), Self::Right => (0, 1),
            Self::Top => (1, -1), Self::Middle => (1, 0), Self::Bottom => (1, 1),
        };
        let lo = bounds.lo[axis] - item.lo[axis];
        let hi = bounds.hi[axis] - item.hi[axis];
        let mut delta = [0; 2];
        delta[axis] = match edge { -1 => lo, 1 => hi, _ => (lo + hi).div_euclid(2) };
        delta
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Move { dx: i64, dy: i64 },
    Resize { from: FixedRect, to: FixedRect },
}

impl Transform {
    pub(super) fn item_rect(self, item: &BoardItem) -> Rect {
        self.rect(item.rect, FixedRect::item(item))
    }

    fn rect(self, rect: Rect, fixed: FixedRect) -> Rect {
        match self {
            Self::Move { dx, dy } => {
                let x = dx as f64 / 64.;
                let y = dy as f64 / 64.;
                Rect::new(rect.x0 + x, rect.y0 + y, rect.x1 + x, rect.y1 + y)
            }
            Self::Resize { from, to } if from == to => rect,
            Self::Resize { from, to } => Preview::resized(fixed, from, to).world(),
        }
    }
}

pub struct CapturedItem {
    pub rect: Rect,
    fixed: FixedRect,
    x: i64,
    y: i64,
}

pub struct Preview {
    pub items: BTreeMap<Id, CapturedItem>,
    pub transform: Transform,
    pub pinned: BTreeSet<Id>,
    members: BTreeSet<Id>,
    bounds: FixedRect,
    min_span: [i128; 2],
}

#[derive(Clone, Copy)]
pub struct Guide {
    pub line: f64,
    pub start: f64,
    pub end: f64,
    edge: usize,
}

#[derive(Default)]
struct Snap {
    delta: [f64; 2],
    guides: [Option<Guide>; 2],
}

fn lines(rect: Rect, axis: usize) -> [f64; 3] {
    let (a, b) = if axis == 0 { (rect.x0, rect.x1) } else { (rect.y0, rect.y1) };
    [a, (a + b) * 0.5, b]
}

fn extent(rect: Rect, axis: usize) -> (f64, f64) {
    if axis == 0 { (rect.x0, rect.x1) } else { (rect.y0, rect.y1) }
}

type Segment = (DVec2, DVec2);

fn segment(axis: usize, start: f64, end: f64, cross: f64) -> Segment {
    if axis == 0 { (dvec2(start, cross), dvec2(end, cross)) }
    else { (dvec2(cross, start), dvec2(cross, end)) }
}

impl Guide {
    pub(super) fn segment(self, axis: usize) -> Segment {
        segment(1 - axis, self.start, self.end, self.line)
    }
}

#[derive(Clone, Copy)]
enum Match {
    Line(Guide),
    Gaps { run: [Rect; 3], moving: usize },
    Size(Rect),
}

impl Match {
    fn segments(self, axis: usize, bounds: Rect) -> [Option<Segment>; 2] {
        // Shift, minimum sizes and rounding to board ticks can change the winning edge.
        const HALF_TICK: f64 = 0.5 / 64.;
        match self {
            Self::Line(mut g) => {
                if (lines(bounds, axis)[g.edge] - g.line).abs() > HALF_TICK { return [None; 2] }
                let (lo, hi) = extent(bounds, 1 - axis);
                g.start = g.start.min(lo);
                g.end = g.end.max(hi);
                [Some(g.segment(axis)), None]
            }
            Self::Size(target) => {
                let side = |r| {
                    let (lo, hi) = extent(r, axis);
                    segment(axis, lo, hi, extent(r, 1 - axis).0)
                };
                let (lo, hi) = extent(bounds, axis);
                let (a, b) = extent(target, axis);
                if ((hi - lo) - (b - a)).abs() > HALF_TICK { return [None; 2] }
                [Some(side(bounds)), Some(side(target))]
            }
            Self::Gaps { mut run, moving } => {
                run[moving] = bounds;
                let gap = |a, b| {
                    let (a0, a1) = extent(a, 1 - axis);
                    let (b0, b1) = extent(b, 1 - axis);
                    segment(axis, extent(a, axis).1, extent(b, axis).0, (a0.max(b0) + a1.min(b1)) * 0.5)
                };
                [Some(gap(run[0], run[1])), Some(gap(run[1], run[2]))]
            }
        }
    }
}

// In edge order, find the last item before a boundary that overlaps M on the other axis.
// Each root adds one item in order of its cross-axis start. Its tree stores the greatest
// cross-axis end in each range of edge ranks. Two binary searches select a prefix and a
// root; descending the tree skips whole ranges of non-mates, even among overlapping cards.
#[derive(Clone, Copy)]
struct MateNode {
    children: [usize; 2],
    end: f64,
}

struct MateIndex {
    edges: Vec<(f64, Rect)>,
    roots: Vec<(f64, usize)>,
    nodes: Vec<MateNode>,
}

impl MateIndex {
    fn new(rects: &[Rect], axis: usize, before: bool) -> Self {
        let mut edges: Vec<_> = rects.iter().copied().filter(|&r| {
            let (a, b) = extent(r, 1 - axis);
            a < b
        }).map(|r| {
            let (lo, hi) = extent(r, axis);
            (if before { hi } else { -lo }, r)
        }).collect();
        edges.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut order: Vec<_> = edges.iter().enumerate().map(|(rank, &(_, r))| {
            (extent(r, 1 - axis), rank)
        }).collect();
        order.sort_unstable_by(|((a, _), _), ((b, _), _)| a.total_cmp(b));
        let mut index = Self { edges, roots: Vec::with_capacity(order.len()),
            nodes: vec![MateNode { children: [0; 2], end: f64::NEG_INFINITY }] };
        let mut root = 0;
        for ((start, end), rank) in order {
            root = index.insert(root, 0, index.edges.len(), rank, end);
            index.roots.push((start, root));
        }
        index
    }

    fn insert(&mut self, old: usize, lo: usize, hi: usize, rank: usize, end: f64) -> usize {
        let mut node = self.nodes[old];
        node.end = node.end.max(end);
        if hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            let right = usize::from(rank >= mid);
            let (a, b) = if right == 0 { (lo, mid) } else { (mid, hi) };
            node.children[right] = self.insert(node.children[right], a, b, rank, end);
        }
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn last(&self, node: usize, lo: usize, hi: usize, limit: usize, start: f64) -> Option<usize> {
        if lo >= limit || self.nodes[node].end <= start { return None }
        if hi - lo == 1 { return Some(lo) }
        let mid = lo + (hi - lo) / 2;
        let [left, right] = self.nodes[node].children;
        self.last(right, mid, hi, limit, start).or_else(|| self.last(left, lo, mid, limit, start))
    }

    fn neighbour(&self, edge: f64, cross: (f64, f64)) -> Option<Rect> {
        let count = self.roots.partition_point(|&(start, _)| start < cross.1);
        let &(_, root) = self.roots.get(count.checked_sub(1)?)?;
        let limit = self.edges.partition_point(|&(value, _)| value <= edge);
        self.last(root, 0, self.edges.len(), limit, cross.0).map(|i| self.edges[i].1)
    }
}

struct AxisTargets {
    lines: Vec<Guide>,
    sizes: Vec<(f64, Rect)>,
    mates: Option<[MateIndex; 2]>,
}

fn nearest<T>(entries: &[T], at: f64, value: impl Fn(&T) -> f64) -> &[T] {
    let i = entries.partition_point(|e| value(e) < at);
    &entries[i.saturating_sub(1)..entries.len().min(i + 1)]
}

impl AxisTargets {
    fn new(rects: &[Rect], axis: usize, resize: bool) -> Self {
        let mut guides = Vec::with_capacity(3 * rects.len());
        let mut sizes = Vec::with_capacity(rects.len());
        for &r in rects {
            let (start, end) = extent(r, 1 - axis);
            guides.extend(lines(r, axis).map(|line| Guide { line, start, end, edge: 0 }));
            let (lo, hi) = extent(r, axis);
            sizes.push((hi - lo, r));
        }
        guides.sort_by(|a, b| a.line.total_cmp(&b.line));
        guides.dedup_by(|next, prev| {
            if next.line != prev.line { return false }
            prev.start = prev.start.min(next.start);
            prev.end = prev.end.max(next.end);
            true
        });
        sizes.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { lines: guides, sizes, mates: (!resize).then(|| [
            MateIndex::new(rects, axis, true), MateIndex::new(rects, axis, false),
        ]) }
    }

    fn gaps(&self, m: Rect, axis: usize, mut offer: impl FnMut(f64, Match)) {
        let Some([before, after]) = &self.mates else { return };
        let (lo, hi) = extent(m, axis);
        let cross = extent(m, 1 - axis);
        if cross.0 >= cross.1 { return }
        let l = before.neighbour(lo, cross);
        let r = after.neighbour(-hi, cross);
        if let (Some(l), Some(r)) = (l, r) {
            let a = lo - extent(l, axis).1;
            let b = extent(r, axis).0 - hi;
            if a + b > 0. {
                offer((b - a) * 0.5, Match::Gaps { run: [l, m, r], moving: 1 });
            }
        }
        if let Some(l) = l {
            let (start, end) = extent(l, axis);
            if let Some(ll) = before.neighbour(start, cross) {
                let gap = start - extent(ll, axis).1;
                if gap > 0. { offer(gap - (lo - end), Match::Gaps { run: [ll, l, m], moving: 2 }); }
            }
        }
        if let Some(r) = r {
            let (start, end) = extent(r, axis);
            if let Some(rr) = after.neighbour(-end, cross) {
                let gap = extent(rr, axis).0 - end;
                if gap > 0. { offer(start - hi - gap, Match::Gaps { run: [m, r, rr], moving: 0 }); }
            }
        }
    }

    fn pull(&self, m: Rect, axis: usize, handle: Option<Handle>, limit: f64) -> (f64, Option<Match>) {
        let side = handle.map(|h| h.0[axis]);
        if side == Some(0) { return (0., None) }
        let mut best = (0_f64, None);
        // Offer in tie order. Equal pulls keep the earlier match.
        let mut offer = |delta: f64, mark| {
            if delta.abs() <= limit && (best.1.is_none() || delta.abs() < best.0.abs()) {
                best = (delta, Some(mark));
            }
        };
        for (edge, value) in lines(m, axis).into_iter().enumerate() {
            if side.is_some_and(|s| edge as i8 != s + 1) { continue }
            for &g in nearest(&self.lines, value, |g| g.line) {
                offer(g.line - value, Match::Line(Guide { edge, ..g }));
            }
        }
        if let Some(side) = side {
            let (lo, hi) = extent(m, axis);
            let span = hi - lo;
            for &(size, target) in nearest(&self.sizes, span, |s| s.0) {
                offer((size - span) * f64::from(side), Match::Size(target));
            }
        } else {
            self.gaps(m, axis, offer);
        }
        best
    }
}

struct SnapTargets([AxisTargets; 2]);

impl SnapTargets {
    fn capture(view: &BoardView, viewport: Option<Rect>, preview: &Preview, resize: bool) -> Self {
        let visible = viewport.map(|r| view.visible(r)).unwrap_or_default();
        let rects: Vec<_> = visible.iter().filter(|id| !preview.items.contains_key(*id))
            .filter_map(|id| view.get(id).map(|item| item.rect)).collect();
        Self(std::array::from_fn(|axis| AxisTargets::new(&rects, axis, resize)))
    }

    fn snap(&self, rect: Rect, handle: Option<Handle>, zoom: f64, alt: bool) -> [(f64, Option<Match>); 2] {
        if alt { return [(0., None); 2] }
        std::array::from_fn(|axis| self.0[axis].pull(rect, axis, handle, 6. / zoom))
    }
}

// A new item's placement also queries enclosing sections. It uses local lines from that query.
fn snap_neighbours(view: &BoardView, r: Rect, limit: f64) -> Vec<Id> {
    view.visible(Rect::new(r.x0 - limit, r.y0 - limit, r.x1 + limit, r.y1 + limit))
}

fn snap_to(moving: Rect, targets: impl Iterator<Item = Rect>, handle: Option<Handle>, limit: f64) -> Snap {
    let mut result = Snap::default();
    for target in targets {
        for axis in 0..2 {
            let extent = lines(target, 1 - axis);
            for (edge, value) in lines(moving, axis).into_iter().enumerate() {
                if handle.is_some_and(|h| h.0[axis] == 0 || edge as i8 != h.0[axis] + 1) { continue }
                for line in lines(target, axis) {
                    let delta = line - value;
                    if delta.abs() > limit { continue }
                    let guide = &mut result.guides[axis];
                    if let Some(g) = guide.as_mut() {
                        if line == g.line && edge == g.edge {
                            g.start = g.start.min(extent[0]);
                            g.end = g.end.max(extent[2]);
                        }
                        if delta.abs() >= result.delta[axis].abs() { continue }
                    }
                    result.delta[axis] = delta;
                    *guide = Some(Guide { line, start: extent[0], end: extent[2], edge });
                }
            }
        }
    }
    result
}

fn ticks(value: f64) -> Option<i64> {
    let n = (value * 64.).round();
    ((i64::MIN as f64..-(i64::MIN as f64)).contains(&n)).then_some(n as i64)
}

fn all() -> Rect {
    Rect::new(f64::MIN, f64::MIN, f64::MAX, f64::MAX)
}

impl Preview {
    fn capture(view: &BoardView, selection: &Selection, carry: bool) -> Self {
        let mut members = BTreeSet::new();
        let mut sections: Vec<_> = selection.ids.iter().copied()
            .filter(|id| view.get(id).is_some_and(|item| item.kind == kind::SECTION))
            .collect();
        if carry && !sections.is_empty() {
            let mut children = HashMap::<Id, Vec<Id>>::new();
            for id in view.visible(all()) {
                if let Some(parent) = view.get(&id).unwrap().parent_section {
                    children.entry(parent).or_default().push(id);
                }
            }
            while let Some(section) = sections.pop() {
                for id in children.remove(&section).into_iter().flatten() {
                    members.insert(id);
                    if view.get(&id).unwrap().kind == kind::SECTION {
                        sections.push(id);
                    }
                }
            }
        }
        let mut items = BTreeMap::new();
        let mut pinned = BTreeSet::new();
        for &id in selection.ids.union(&members) {
            if let Some(item) = view.get(&id) {
                if item.locked {
                    pinned.insert(id);
                } else {
                    items.insert(id, CapturedItem { rect: item.rect, fixed: FixedRect::item(item), x: item.x, y: item.y });
                }
            }
        }
        members.retain(|id| items.contains_key(id));
        let mut p = Self { items, pinned, members, transform: Transform::Move { dx: 0, dy: 0 },
            bounds: FixedRect { lo: [0; 2], hi: [0; 2] }, min_span: [512; 2] };
        p.measure();
        p
    }

    fn measure(&mut self) {
        let Some(bounds) = self.items.values().map(|v| v.fixed).reduce(FixedRect::union) else { return };
        self.bounds = bounds;
        for axis in 0..2 {
            let smallest = self.items.values().map(|v| v.fixed.span(axis)).filter(|&w| w > 0).min();
            self.min_span[axis] = smallest.map_or(512, |w| (bounds.span(axis) * 512 + w - 1) / w).max(512);
        }
    }

    fn resized(item: FixedRect, from: FixedRect, to: FixedRect) -> FixedRect {
        let mut r = to;
        for axis in 0..2 {
            if from.span(axis) == 0 { continue }
            // Another seat's copy can be outside the captured selection after a concurrent edit.
            let map = |v: i128| {
                let offset = v - from.lo[axis];
                let scaled = proportion(offset.abs(), to.span(axis), from.span(axis));
                to.lo[axis].saturating_add(if offset < 0 { -scaled } else { scaled })
            };
            r.lo[axis] = map(item.lo[axis]).min(to.hi[axis] - 512);
            r.hi[axis] = map(item.hi[axis]).max(r.lo[axis] + 512);
        }
        r
    }

    fn resize(&self, base: FixedRect, handle: Handle, delta: [i128; 2], aspect: bool,
        snapped_axis: Option<usize>) -> Option<FixedRect> {
        let mut size = [0; 2];
        for axis in 0..2 {
            size[axis] = (base.span(axis) + i128::from(handle.0[axis]) * delta[axis]).max(self.min_span[axis]);
        }
        if aspect && base.span(0) > 0 && base.span(1) > 0 {
            let primary = snapped_axis.unwrap_or_else(|| match handle.0 {
                [_, 0] => 0,
                [0, _] => 1,
                _ => usize::from((size[1] as f64 / base.span(1) as f64 - 1.).abs()
                    > (size[0] as f64 / base.span(0) as f64 - 1.).abs()),
            });
            let other = 1 - primary;
            size[other] = proportion(size[primary], base.span(other), base.span(primary));
            if size[other] < self.min_span[other] {
                size[other] = self.min_span[other];
                size[primary] = proportion(size[other], base.span(primary), base.span(other));
            }
        }
        let mut to = base;
        for axis in 0..2 {
            // Beyond this span no arrangement of i64 positions and sizes fits.
            if size[axis] > i128::from(i64::MAX) * 2 - i128::from(i64::MIN) { return None }
            to.lo[axis] = match handle.0[axis] {
                -1 => base.hi[axis] - size[axis],
                0 => base.lo[axis] + (base.span(axis) - size[axis]).div_euclid(2),
                _ => base.lo[axis],
            };
            to.hi[axis] = to.lo[axis] + size[axis];
        }
        Some(to)
    }

    fn bounds_at(&self) -> Rect {
        match self.transform {
            Transform::Move { dx, dy } => {
                let mut b = self.bounds;
                for (axis, delta) in [dx, dy].into_iter().enumerate() {
                    b.lo[axis] += i128::from(delta);
                    b.hi[axis] += i128::from(delta);
                }
                b.world()
            }
            Transform::Resize { to, .. } => to.world(),
        }
    }

    pub fn rect(&self, id: &Id) -> Option<Rect> {
        let item = self.items.get(id)?;
        Some(self.transform.rect(item.rect, item.fixed))
    }

    fn finish(self, view: &BoardView) -> Option<Action> {
        let (dx, dy) = match self.transform {
            Transform::Move { dx, dy } => (dx, dy),
            Transform::Resize { from, to } => {
                if from == to { return None }
                let mut changes = Vec::new();
                for (id, original) in self.items {
                    let Some(current) = view.get(&id).filter(|i| !i.locked) else { continue };
                    let r = Self::resized(original.fixed, from, to);
                    if r.lo == [i128::from(current.x), i128::from(current.y)]
                        && r.span(0) == i128::from(current.w) && r.span(1) == i128::from(current.h) {
                        continue;
                    }
                    for (field, n) in [(field::X, r.lo[0]), (field::Y, r.lo[1]), (field::W, r.span(0)), (field::H, r.span(1))] {
                        let Ok(value) = i64::try_from(n) else {
                            log!("Board resize cancelled: target exceeds the canvas coordinate range");
                            return None;
                        };
                        changes.push(Change { entity: id, field, value: Value::Int(value) });
                    }
                }
                let count = changes.len() / 4;
                let noun = if count == 1 { "item" } else { "items" };
                let label = Some(format!("resize {count} {noun}"));
                return (!changes.is_empty()).then_some(Action::SetFields { changes, label });
            }
        };
        if dx == 0 && dy == 0 {
            return None;
        }
        let mut moves = Vec::with_capacity(self.items.len());
        for (id, original) in self.items {
            if view.get(&id).is_some_and(|item| !item.locked) {
                let Some((x, y)) = original.x.checked_add(dx).zip(original.y.checked_add(dy)) else {
                    log!("Board move cancelled: target exceeds the canvas coordinate range");
                    return None;
                };
                moves.push((id, x, y));
            }
        }
        let changed = moves.iter().any(|(id, x, y)| view.get(id).is_some_and(|i| (i.x, i.y) != (*x, *y)));
        changed.then_some(Action::MoveItems { moves, members: Some(self.members) })
    }
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Pressed {
        hit: Option<Id>,
        start: DVec2,
        screen: DVec2,
        before: Selection,
        shift: bool,
        marquee: bool,
        locked: bool,
    },
    Moving { start: DVec2, targets: SnapTargets },
    Resizing { start: DVec2, handle: Handle, base: FixedRect, targets: SnapTargets },
    Marquee { start: DVec2, rect: Rect, before: Selection, shift: bool },
    Panning { screen: DVec2, pan: DVec2 },
    Nudging { held: u8 },
}

#[derive(Default)]
pub struct Gesture {
    pub selection: Selection,
    pub hover: Option<Id>,
    pub preview: Option<Preview>,
    pub space: bool,
    pub handle: Option<Handle>,
    marks: [Option<Match>; 2],
    viewport: Option<Rect>,
    phase: Phase,
}

impl Gesture {
    /// `hand`: the Hand tool is armed, so a press anywhere pans.
    pub fn cursor(&self, view: &BoardView, hand: bool) -> MouseCursor {
        match self.phase {
            Phase::Panning { .. } => MouseCursor::Grabbing,
            Phase::Pressed { locked: true, .. } => MouseCursor::NotAllowed,
            _ if self.space || hand => MouseCursor::Grab,
            Phase::Resizing { handle, .. } => handle.cursor(),
            Phase::Idle if self.handle.is_some() => self.handle.unwrap().cursor(),
            Phase::Moving { .. } => MouseCursor::Hand,
            _ if self.hover.and_then(|id| view.get(&id)).is_some_and(|i| !i.locked) => MouseCursor::Hand,
            _ => MouseCursor::Default,
        }
    }

    pub fn marquee(&self) -> Option<Rect> {
        if let Phase::Marquee { rect, .. } = self.phase { Some(rect) } else { None }
    }

    pub fn pointer_active(&self) -> bool {
        !matches!(self.phase, Phase::Idle | Phase::Nudging { .. })
    }

    pub(super) fn moving(&self) -> Option<&Preview> {
        match self.phase {
            Phase::Moving { .. } | Phase::Resizing { .. } => self.preview.as_ref(),
            _ => None,
        }
    }

    pub(super) fn set_viewport(&mut self, view: &BoardView, viewport: Rect) {
        if self.viewport == Some(viewport) { return }
        self.viewport = Some(viewport);
        let Some(p) = &self.preview else { return };
        match &mut self.phase {
            Phase::Moving { targets, .. } => *targets = SnapTargets::capture(view, self.viewport, p, false),
            Phase::Resizing { targets, .. } => *targets = SnapTargets::capture(view, self.viewport, p, true),
            _ => {}
        }
    }

    pub(super) fn snap_segments(&self) -> impl Iterator<Item = Segment> + '_ {
        let bounds = self.moving().map(Preview::bounds_at);
        self.marks.iter().enumerate().flat_map(move |(axis, mark)| {
            mark.zip(bounds).map_or([None; 2], |(m, b)| m.segments(axis, b)).into_iter().flatten()
        })
    }

    /// A press that still selects one item on release, without a drag or Shift-toggle.
    pub(super) fn clicked_item(&self) -> Option<Id> {
        match self.phase {
            Phase::Pressed { hit, shift: false, .. } => hit,
            _ => None,
        }
    }

    pub fn handles(&self, view: &BoardView) -> Option<Rect> {
        let mut bounds: Option<Rect> = None;
        for id in &self.selection.ids {
            let Some(item) = view.get(id).filter(|item| !item.locked) else { continue };
            let r = self.preview.as_ref().and_then(|p| p.rect(id)).unwrap_or(item.rect);
            bounds = Some(bounds.map_or(r, |b| Rect::new(b.x0.min(r.x0), b.y0.min(r.y0), b.x1.max(r.x1), b.y1.max(r.y1))));
        }
        bounds
    }

    pub fn hit_handle(&self, view: &BoardView, world: DVec2, zoom: f64) -> Option<Handle> {
        let bounds = self.handles(view)?;
        Handle::ALL.into_iter().filter_map(|h| {
            let d = (h.point(bounds, zoom) - world) * zoom;
            (d.x.abs().max(d.y.abs()) <= 7.).then_some((d.length(), h))
        }).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, h)| h)
    }

    pub fn press_handle(&mut self, view: &BoardView, handle: Handle, world: DVec2) {
        if !matches!(self.phase, Phase::Idle) { return }
        let mut p = Preview::capture(view, &self.selection, false);
        if p.items.is_empty() { return }
        let base = p.bounds;
        p.transform = Transform::Resize { from: base, to: base };
        let targets = SnapTargets::capture(view, self.viewport, &p, true);
        self.preview = Some(p);
        self.phase = Phase::Resizing { start: world, handle, base, targets };
        self.handle = Some(handle);
    }

    pub fn align(&self, view: &BoardView, to: AlignTo) -> Option<Action> {
        if self.preview.is_some() || self.selection.ids.len() < 2 { return None }
        let bounds = self.selection.ids.iter().filter_map(|id| view.get(id))
            .map(FixedRect::item).reduce(FixedRect::union)?;
        let p = Preview::capture(view, &self.selection, true);
        let mut moves = Vec::new();
        for (&id, original) in &p.items {
            // Explicitly selected children use their own alignment. Other
            // members follow the nearest selected section that actually moves.
            let mut owner = Some(id);
            while let Some(node) = owner {
                if self.selection.ids.contains(&node) && p.items.contains_key(&node) { break }
                owner = view.get(&node).and_then(|i| i.parent_section);
            }
            let Some(anchor) = owner else { continue };
            let delta = to.offset(p.items[&anchor].fixed, bounds);
            if delta == [0, 0] { continue }
            let x = i64::try_from(i128::from(original.x) + delta[0]);
            let y = i64::try_from(i128::from(original.y) + delta[1]);
            let (Ok(x), Ok(y)) = (x, y) else {
                log!("Board alignment cancelled: target exceeds the canvas coordinate range");
                return None;
            };
            moves.push((id, x, y));
        }
        let members = moves.iter().map(|(id, _, _)| *id).filter(|id| p.members.contains(id)).collect();
        (!moves.is_empty()).then_some(Action::MoveItems { moves, members: Some(members) })
    }

    pub fn press(
        &mut self, view: &BoardView, hit: Option<Id>, world: DVec2, screen: DVec2,
        shift: bool, pan: Option<DVec2>, section_interior: bool,
    ) {
        if !matches!(self.phase, Phase::Idle) {
            return;
        }
        self.hover = hit;
        if let Some(pan) = pan {
            self.phase = Phase::Panning { screen, pan };
            return;
        }
        let before = self.selection.clone();
        let marquee = hit.is_none() || section_interior && hit.is_some_and(|id| !before.ids.contains(&id));
        let locked = hit.and_then(|id| view.get(&id)).is_some_and(|item| item.locked);
        if let Some(id) = hit {
            if !self.selection.ids.contains(&id) {
                if !shift {
                    self.selection = Selection::default();
                }
                self.selection.add(id);
            }
            if !locked && !marquee {
                self.preview = Some(Preview::capture(view, &self.selection, true));
            }
        }
        self.phase = Phase::Pressed { hit, start: world, screen, before, shift, marquee, locked };
    }

    fn cancel_out_of_range(&mut self) -> Option<DVec2> {
        log!("Board gesture cancelled: target exceeds the canvas coordinate range");
        self.cancel();
        None
    }

    pub fn motion(&mut self, view: &BoardView, world: DVec2, screen: DVec2, zoom: f64, alt: bool, shift: bool) -> Option<DVec2> {
        self.marks = [None; 2];
        if let Phase::Pressed { start, screen: down, before, shift, marquee, locked, .. } = &self.phase {
            if (screen - *down).length() <= 3. || *locked {
                return None;
            }
            self.phase = if *marquee {
                Phase::Marquee {
                    start: *start,
                    rect: Rect::new(start.x, start.y, world.x, world.y),
                    before: before.clone(),
                    shift: *shift,
                }
            } else {
                Phase::Moving { start: *start, targets: SnapTargets::capture(view, self.viewport, self.preview.as_ref()?, false) }
            };
        }
        match &mut self.phase {
            Phase::Moving { start, targets } => {
                if let Some(preview) = &mut self.preview {
                    let delta = world - *start;
                    let Some((dx, dy)) = ticks(delta.x).zip(ticks(delta.y)) else {
                        return self.cancel_out_of_range();
                    };
                    preview.transform = Transform::Move { dx, dy };
                    let snapped = targets.snap(preview.bounds_at(), None, zoom, alt);
                    let offsets = ticks(snapped[0].0).zip(ticks(snapped[1].0));
                    let Some((dx, dy)) = offsets.and_then(|(x, y)| dx.checked_add(x).zip(dy.checked_add(y))) else {
                        return self.cancel_out_of_range();
                    };
                    preview.transform = Transform::Move { dx, dy };
                    self.marks = snapped.map(|(_, mark)| mark);
                }
            }
            Phase::Resizing { start, handle, base, targets } => {
                let delta = world - *start;
                let Some((x, y)) = ticks(delta.x).zip(ticks(delta.y)) else { return self.cancel_out_of_range() };
                let p = self.preview.as_mut()?;
                if x == 0 && y == 0 {
                    p.transform = Transform::Resize { from: *base, to: *base };
                    return None;
                }
                let Some(raw) = p.resize(*base, *handle, [x.into(), y.into()], shift, None) else {
                    return self.cancel_out_of_range();
                };
                let snapped = targets.snap(raw.world(), Some(*handle), zoom, alt);
                let mut to = raw;
                if let Some(axis) = (0..2).filter(|&i| snapped[i].1.is_some())
                    .min_by(|&a, &b| snapped[a].0.abs().total_cmp(&snapped[b].0.abs())) {
                    // With Shift a snap fixes one edge; derive the other dimension
                    // from that snapped size so the preview keeps its aspect ratio.
                    let mut adjusted = [0; 2];
                    for i in 0..2 {
                        let Some(offset) = ticks(snapped[i].0) else { return self.cancel_out_of_range() };
                        adjusted[i] = i128::from(handle.0[i]) * (raw.span(i) - base.span(i)) + i128::from(offset);
                    }
                    let Some(resized) = p.resize(*base, *handle, adjusted, shift, Some(axis)) else {
                        return self.cancel_out_of_range();
                    };
                    to = resized;
                }
                p.transform = Transform::Resize { from: *base, to };
                self.marks = snapped.map(|(_, mark)| mark);
            }
            Phase::Marquee { start, rect, before, shift } => {
                *rect = Rect::new(start.x, start.y, world.x, world.y);
                self.selection = if *shift { before.clone() } else { Selection::default() };
                for id in view.marquee(*rect) {
                    self.selection.add(id);
                }
            }
            Phase::Panning { screen: start, pan } => return Some(*pan + (screen - *start)),
            _ => {}
        }
        None
    }

    pub fn release(&mut self, view: &BoardView) -> Option<Action> {
        self.marks = [None; 2];
        let phase = std::mem::take(&mut self.phase);
        match phase {
            Phase::Moving { .. } | Phase::Resizing { .. } => return self.preview.take()?.finish(view),
            Phase::Pressed { hit, before, shift, .. } => {
                self.selection = if shift { before } else { Selection::default() };
                if let Some(id) = hit.filter(|id| view.get(id).is_some()) {
                    if shift && self.selection.ids.contains(&id) {
                        self.selection.remove(&id);
                    } else {
                        self.selection.add(id);
                    }
                }
            }
            Phase::Nudging { .. } => {
                self.phase = phase;
                return None;
            }
            _ => {}
        }
        self.preview = None;
        None
    }

    pub fn cancel(&mut self) {
        self.marks = [None; 2];
        self.handle = None;
        match std::mem::take(&mut self.phase) {
            Phase::Pressed { before, .. } | Phase::Marquee { before, .. } => self.selection = before,
            Phase::Idle => self.selection = Selection::default(),
            _ => {}
        }
        self.preview = None;
    }

    pub fn fold_changed(&mut self, view: &BoardView, ids: &HashSet<Id>) {
        let mut removed = false;
        for id in ids {
            let item = view.get(id);
            if item.is_none() {
                self.selection.remove(id);
                if self.hover.as_ref() == Some(id) {
                    self.hover = None;
                }
                match &mut self.phase {
                    Phase::Pressed { before, .. } | Phase::Marquee { before, .. } => before.remove(id),
                    _ => {}
                }
            }
            if let Some(p) = &mut self.preview {
                if item.is_none_or(|i| i.locked) && p.items.remove(id).is_some() {
                    p.members.remove(id);
                    removed = true;
                    if item.is_some() {
                        p.pinned.insert(*id);
                    }
                }
                if item.is_none_or(|i| !i.locked) {
                    p.pinned.remove(id);
                }
            }
        }
        if removed {
            self.marks = [None; 2];
            if let Some(p) = &mut self.preview {
                if matches!(p.transform, Transform::Move { .. }) { p.measure(); }
            }
        }
        if self.preview.as_ref().is_some_and(|p| p.items.is_empty()) {
            self.phase = Phase::Idle;
            self.preview = None;
        }
    }

    pub fn select_all(&mut self, view: &BoardView) {
        self.selection = Selection::default();
        for id in view.marquee(all()) {
            self.selection.add(id);
        }
    }

    pub fn delete(&self, view: &BoardView) -> Option<Action> {
        let p = Preview::capture(view, &self.selection, true);
        let items: Vec<_> = p.items.into_keys().collect();
        (!items.is_empty()).then_some(Action::DeleteItems { items, members: Some(p.members) })
    }

    pub fn lock(&self, view: &BoardView) -> Option<Action> {
        let locked = self.selection.ids.iter().any(|id| view.get(id).is_some_and(|i| !i.locked));
        let changes: Vec<_> = self.selection.ids.iter().filter(|id| view.get(id).is_some())
            .map(|&entity| Change { entity, field: field::LOCKED, value: Value::Bool(locked) })
            .collect();
        let label = format!("{} {} items", if locked { "lock" } else { "unlock" }, changes.len());
        (!changes.is_empty()).then_some(Action::SetFields { changes, label: Some(label) })
    }

    fn arrow(key: KeyCode) -> Option<(u8, i64, i64)> {
        Some(match key {
            KeyCode::ArrowLeft => (1, -64, 0),
            KeyCode::ArrowRight => (2, 64, 0),
            KeyCode::ArrowUp => (4, 0, -64),
            KeyCode::ArrowDown => (8, 0, 64),
            _ => return None,
        })
    }

    pub fn nudge(&mut self, view: &BoardView, key: KeyCode, shift: bool) {
        let Some((bit, x, y)) = Self::arrow(key) else { return };
        if matches!(self.phase, Phase::Idle) {
            let p = Preview::capture(view, &self.selection, true);
            if p.items.is_empty() {
                return;
            }
            self.preview = Some(p);
            self.phase = Phase::Nudging { held: 0 };
        }
        if let Phase::Nudging { held } = &mut self.phase {
            *held |= bit;
            let preview = self.preview.as_mut().unwrap();
            let Transform::Move { dx, dy } = preview.transform else { return };
            let step = if shift { 10 } else { 1 };
            let Some((dx, dy)) = dx.checked_add(x * step).zip(dy.checked_add(y * step)) else {
                log!("Board nudge cancelled: delta exceeds the canvas coordinate range");
                self.cancel();
                return;
            };
            preview.transform = Transform::Move { dx, dy };
        }
    }

    pub fn key_up(&mut self, view: &BoardView, key: KeyCode) -> Option<Action> {
        if let Phase::Nudging { held } = &mut self.phase {
            *held &= !Self::arrow(key)?.0;
            if *held == 0 {
                return self.focus_lost(view);
            }
        }
        None
    }

    pub fn focus_lost(&mut self, view: &BoardView) -> Option<Action> {
        self.space = false;
        if matches!(self.phase, Phase::Nudging { .. }) {
            self.phase = Phase::Idle;
            return self.preview.take()?.finish(view);
        }
        if self.pointer_active() {
            self.cancel();
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BoardTool { #[default] Select, Hand, Sticky, Text, Shape, Section }

/// What a scroll does to the board's camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollNav {
    /// Move the camera by this many screen points.
    Pan(DVec2),
    /// Multiply the zoom by this, about the pointer.
    Zoom(f64),
}

/// A wheel's zoom per screen point it turns: a 32-point notch (macOS) zooms by about 1.17.
const WHEEL_ZOOM: f64 = 0.005;
/// A trackpad's zoom per point, with Command or Control held.
const PAD_ZOOM: f64 = 0.012;
/// How much finer Alt makes a wheel's zoom.
const FINE_ZOOM: f64 = 0.25;

/// The board's scroll navigation. `scroll` is in screen points. `wheel`: the event came from a
/// wheel (no precise delta, no gesture phase), which zooms toward the pointer; a trackpad's two
/// fingers pan, and its pinch zooms as an event of its own. Command or Control turns either into
/// a zoom. Shift turns a wheel sideways, and a tilt wheel's sideways turn pans.
pub fn scroll_nav(scroll: DVec2, wheel: bool, mods: &KeyModifiers) -> ScrollNav {
    let command = mods.control || mods.logo;
    if wheel && mods.shift && !command {
        // macOS already turns Shift+wheel sideways; Windows and X11 deliver it vertical.
        let x = if scroll.x != 0. { scroll.x } else { scroll.y };
        return ScrollNav::Pan(dvec2(-x, 0.));
    }
    if (wheel || command) && scroll.y != 0. {
        let rate = if wheel { WHEEL_ZOOM } else { PAD_ZOOM } * if wheel && mods.alt { FINE_ZOOM } else { 1. };
        return ScrollNav::Zoom((-scroll.y * rate).exp());
    }
    ScrollNav::Pan(-scroll)
}

/// How far, in screen points, a press moves before it becomes a drag.
pub(super) const DRAG_SLOP: f64 = 3.;
const MIN_ITEM_SIZE: f64 = 8.;

pub(super) struct Placement {
    pub rect: Rect,
    pub guides: [Option<Guide>; 2],
    pub item: NewItem,
}

impl BoardTool {
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select", Self::Hand => "Hand", Self::Sticky => "Sticky", Self::Text => "Text",
            Self::Shape => "Shape", Self::Section => "Section",
        }
    }

    /// Whether the tool puts a new item on the board.
    pub fn places(self) -> bool {
        !matches!(self, Self::Select | Self::Hand)
    }

    /// `start` is the press point of a drag that has passed [`DRAG_SLOP`]. Without it this is a
    /// click or a toolbar drop, which places the default size centred on `p`.
    pub(super) fn preview(self, view: Option<&BoardView>, start: Option<DVec2>, p: DVec2,
        zoom: f64, alt: bool) -> Option<Placement> {
        let (kind, width, height) = match self {
            Self::Select | Self::Hand => return None,
            Self::Sticky => (kind::STICKY, 220., 160.),
            Self::Text => (kind::TEXT_BLOCK, 180., 40.),
            Self::Shape => (kind::SHAPE, 144., 96.),
            Self::Section => (kind::SECTION, 480., 320.),
        };
        let (mut r, handle) = if let Some(a) = start {
            // The ghost covers exactly what the pointer has swept, growing from zero size.
            let side = |v, a| if v < a { -1 } else { 1 };
            (Rect::new(a.x, a.y, p.x, p.y), Some(Handle([side(p.x, a.x), side(p.y, a.y)])))
        } else {
            (Rect::new(p.x - width * 0.5, p.y - height * 0.5, p.x + width * 0.5, p.y + height * 0.5), None)
        };
        // The expanded query includes every possible enclosing section after the snap.
        let limit = if alt { 0. } else { 6. / zoom };
        let neighbours = view.map(|v| snap_neighbours(v, r, limit)).unwrap_or_default();
        let targets = neighbours.iter().filter_map(|id| view?.get(id));
        let mut snap = if alt { Snap::default() } else { snap_to(r, targets.map(|i| i.rect), handle, limit) };
        for (axis, (lo, hi)) in [(&mut r.x0, &mut r.x1), (&mut r.y0, &mut r.y1)].into_iter().enumerate() {
            let delta = snap.delta[axis];
            match handle.map(|h| h.0[axis]) {
                Some(-1) if *hi - (*lo + delta) >= 8. => *lo += delta,
                Some(1) if *hi + delta - *lo >= 8. => *hi += delta,
                Some(_) => snap.guides[axis] = None,
                None => { *lo += delta; *hi += delta; }
            }
        }
        // A short drag places the minimum size, grown away from the press point.
        let mut placed = r;
        if let Some(h) = handle {
            for (axis, (lo, hi)) in [(&mut placed.x0, &mut placed.x1), (&mut placed.y0, &mut placed.y1)].into_iter().enumerate() {
                if *hi - *lo < MIN_ITEM_SIZE {
                    if h.0[axis] < 0 { *lo = *hi - MIN_ITEM_SIZE } else { *hi = *lo + MIN_ITEM_SIZE }
                }
            }
        }
        let parent = view.and_then(|v| neighbours.into_iter().rev().find(|id| {
            v.get(id).is_some_and(|i| i.kind == kind::SECTION && i.rect.encloses(placed))
        }));
        let item = NewItem {
            kind, x: ticks(placed.x0)?, y: ticks(placed.y0)?, w: ticks(placed.x1 - placed.x0)?,
            h: ticks(placed.y1 - placed.y0)?, parent,
        };
        Some(Placement { rect: r, guides: snap.guides, item })
    }
}

#[derive(Clone, Debug)]
pub struct NewItem {
    pub kind: u16,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub parent: Option<Id>,
}

impl NewItem {
    /// Return every successful write, including a board created before an item write failed.
    pub fn place(&self, project: Id, board: Option<Id>, mut act: impl FnMut(Action) -> Option<Id>)
        -> Option<(Id, Vec<Id>)> {
        let mut events = Vec::new();
        let board = match board {
            Some(board) => board,
            None => {
                let id = act(Action::AddBoard { project, title: "Board".into() })?;
                events.push(id);
                id
            }
        };
        let style = ItemStyle {
            text: (self.kind == kind::TEXT_BLOCK).then(|| "Text".into()),
            ..Default::default()
        };
        if let Some(id) = act(Action::AddItem {
            board, kind: Key::Number(self.kind), x: self.x, y: self.y, w: self.w, h: self.h, parent: self.parent, style,
        }) {
            events.push(id);
        }
        (!events.is_empty()).then_some((board, events))
    }
}

#[derive(Default)]
pub struct BoardHistory {
    steps: Vec<Vec<Id>>,
    // Completed steps, followed by any prefix applied before an interrupted write.
    complete: usize,
    prefix: usize,
}

impl BoardHistory {
    pub fn record(&mut self, event: Id) { self.record_step(vec![event]); }

    pub fn record_step(&mut self, events: Vec<Id>) {
        if events.is_empty() { return }
        if self.prefix > 0 {
            self.steps[self.complete].truncate(self.prefix);
            self.complete += 1;
            self.prefix = 0;
        }
        self.steps.truncate(self.complete);
        self.steps.push(events);
        self.complete += 1;
    }

    /// Replace each creation/edit id with its undo id as it is signed. A retry
    /// resumes at the first unwritten inverse, even if the write failed mid-step.
    pub fn step(&mut self, redo: bool, mut act: impl FnMut(Action) -> Option<Id>) {
        if !redo && self.prefix == 0 {
            if self.complete == 0 { return }
            self.complete -= 1;
            self.prefix = self.steps[self.complete].len();
        }
        let Some(step) = self.steps.get_mut(self.complete) else { return };
        while if redo { self.prefix < step.len() } else { self.prefix > 0 } {
            let i = if redo { self.prefix } else { self.prefix - 1 };
            let Some(id) = act(Action::Undo { of: step[i] }) else { break };
            step[i] = id;
            self.prefix = if redo { i + 1 } else { i };
        }
        if self.prefix == step.len() {
            self.complete += 1;
            self.prefix = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::tests::{change, item, set, History};
    use makepad_widgets::dvec2;

    #[test]
    fn a_wheel_zooms_a_trackpad_pans_and_modifiers_turn_or_refine_them() {
        let mods = |f: fn(&mut KeyModifiers)| { let mut m = KeyModifiers::default(); f(&mut m); m };
        let none = KeyModifiers::default();
        let zoom = |nav| match nav { ScrollNav::Zoom(f) => f, other => panic!("expected a zoom, got {other:?}") };
        let notch = zoom(scroll_nav(dvec2(0., 32.), true, &none));
        assert!(notch < 1. && notch > 0.8, "a wheel notch toward the person zooms out a step: {notch}");
        assert!((zoom(scroll_nav(dvec2(0., -32.), true, &none)) * notch - 1.).abs() < 1e-12, "and back is the inverse");
        let fine = zoom(scroll_nav(dvec2(0., 32.), true, &mods(|m| m.alt = true)));
        assert!(fine > notch && fine < 1., "Alt makes the wheel's step finer: {fine} vs {notch}");
        assert_eq!(scroll_nav(dvec2(0., 32.), true, &mods(|m| m.shift = true)), ScrollNav::Pan(dvec2(-32., 0.)),
            "Shift turns a vertical wheel sideways");
        assert_eq!(scroll_nav(dvec2(20., 0.), true, &mods(|m| m.shift = true)), ScrollNav::Pan(dvec2(-20., 0.)),
            "macOS's own sideways Shift+wheel pans the same way");
        assert_eq!(scroll_nav(dvec2(12., 0.), true, &none), ScrollNav::Pan(dvec2(-12., 0.)), "a tilt wheel pans sideways");
        assert_eq!(scroll_nav(dvec2(6., -9.), false, &none), ScrollNav::Pan(dvec2(-6., 9.)),
            "two fingers pan both ways, Alt or not");
        assert_eq!(scroll_nav(dvec2(6., -9.), false, &mods(|m| m.alt = true)), ScrollNav::Pan(dvec2(-6., 9.)));
        for command in [mods(|m| m.logo = true), mods(|m| m.control = true)] {
            assert!(zoom(scroll_nav(dvec2(0., -10.), false, &command)) > 1., "Command or Control with two fingers zooms");
        }
        assert!(zoom(scroll_nav(dvec2(0., -10.), true, &mods(|m| { m.shift = true; m.logo = true }))) > 1.,
            "Command keeps a Shift+wheel a zoom");
    }

    #[test]
    fn snapping_uses_independent_axes_centres_zoom_and_modifiers() {
        let mut h = History::new();
        let board = h.board();
        let section = h.push(item(kind::SECTION, board, [0, 0, 20, 30], None)).0;
        let child = h.push(item(kind::STICKY, section, [12, 4, 5, 6], None)).0;
        h.push(item(kind::STICKY, board, [160, 10, 30, 80], None));
        h.push(item(kind::STICKY, board, [230, 10, 30, 80], None));
        let view = BoardView::build(&h.state, board);
        let mut g = Gesture::default();
        g.set_viewport(&view, Rect::new(0., 0., 120., 120.));
        let origin = dvec2(0., 0.);
        g.press(&view, Some(section), origin, origin, false, None, false);
        let move_to = |g: &mut Gesture, y, zoom, alt| {
            let p = dvec2(12., y);
            g.motion(&view, p, p * zoom, zoom, alt, false);
            g.moving().unwrap().rect(&section).unwrap()
        };
        assert_eq!(move_to(&mut g, 4., 1., false), Rect::new(12., 4., 32., 34.),
            "off-screen neighbours, the selection and its carried child cannot pull the preview");
        assert_eq!(g.snap_segments().count(), 0, "excluded items cannot produce marks");
        g.set_viewport(&view, Rect::new(150., 0., 270., 120.));
        for zoom in [1., 2.] {
            let y = 10. - 6. / zoom;
            assert_eq!(move_to(&mut g, y, zoom, false).y0, 10.,
                "a visible neighbour across a gap pulls the top edge from six screen pixels at zoom {zoom}");
            assert_eq!(g.snap_segments().collect::<Vec<_>>(), [(dvec2(12., 10.), dvec2(260., 10.))],
                "the guide reaches the dragged item and every neighbour on the winning line");
            assert_eq!(g.moving().unwrap().rect(&child).unwrap().y0, 14.,
                "the live preview carries the section member by the snapped offset");
            let outside = y - 1. / 64.;
            assert_eq!(move_to(&mut g, outside, zoom, false).y0, outside,
                "one tick past six screen pixels is outside the threshold at zoom {zoom}");
            assert_eq!(move_to(&mut g, y, zoom, true).y0, y, "Alt bypasses line snapping at zoom {zoom}");
        }
        assert_eq!(move_to(&mut g, 34., 1., false).y0, 35.,
            "a side-by-side neighbour pulls the vertical centre independently of the horizontal axis");
        assert_eq!(g.snap_segments().next(), Some((dvec2(12., 50.), dvec2(260., 50.))),
            "the centre guide is at the neighbours' shared centre");
        g.cancel();

        for (side, dx, expected) in [(1, 7., (0., 30.)), (-1, -13., (-10., 20.))] {
            g.press_handle(&view, Handle([side, 0]), origin);
            let p = dvec2(dx, 17.);
            g.motion(&view, p, p, 1., false, false);
            let r = g.moving().unwrap().rect(&section).unwrap();
            assert_eq!((r.x0, r.x1, r.y0, r.y1), (expected.0, expected.1, 0., 30.),
                "matching width moves only the held edge on side {side}");
            let bars: Vec<_> = g.snap_segments().collect();
            assert_eq!(bars[0], (dvec2(r.x0, 0.), dvec2(r.x1, 0.)), "the preview's bar spans its matched width");
            assert_eq!(bars[1].1 - bars[1].0, dvec2(30., 0.), "the matching neighbour has the same width bar");
            g.motion(&view, p, p, 1., false, true);
            let r = g.moving().unwrap().rect(&section).unwrap();
            assert_eq!(r.y1 - r.y0, 45., "Shift derives the height from the matched width");
            g.motion(&view, p, p, 1., true, false);
            assert_eq!(g.snap_segments().count(), 0, "Alt also disables size matches");
            g.cancel();
        }

        let moving = Rect::new(2., 15., 15., 23.);
        let axes = SnapTargets(std::array::from_fn(|axis| AxisTargets::new(&[Rect::new(20., 10., 40., 30.)], axis, false)));
        let offsets = |zoom| axes.snap(moving, None, zoom, false).map(|(delta, _)| delta);
        assert_eq!(offsets(1.), [5., 1.], "the two axes can choose different pulls");
        assert_eq!(offsets(2.), [0., 1.], "one axis leaving the threshold does not disable the other");

        let run = [Rect::new(0., 0., 20., 20.), Rect::new(50., 0., 70., 20.), Rect::new(86., 40., 92., 60.)];
        let targets = AxisTargets::new(&run, 0, false);
        for (x, goal, bars) in [
            (103., 100., [(20., 50.), (70., 100.)]),
            (-52., -50., [(-30., 0.), (20., 50.)]),
            (27., 25., [(20., 25.), (45., 50.)]),
        ] {
            let rect = |left| Rect::new(left, 0., left + 20., 20.);
            let (delta, mark) = targets.pull(rect(x), 0, None, 6.);
            assert_eq!(x + delta, goal, "the nearest row-mates give the equal-gap position for x={x}");
            assert!(matches!(mark, Some(Match::Gaps { .. })), "equal spacing wins at x={x}");
            assert_eq!(mark.unwrap().segments(0, rect(goal)), bars.map(|(a, b)| Some((dvec2(a, 10.), dvec2(b, 10.)))),
                "both equal gaps get bars between their actual endpoints for x={x}");
        }
        let column = [Rect::new(0., 0., 20., 20.), Rect::new(0., 35., 20., 66.), Rect::new(0., 50., 20., 70.)];
        let m = Rect::new(0., 103., 20., 123.);
        let (delta, mark) = AxisTargets::new(&column, 1, false).pull(m, 1, None, 6.);
        assert!(delta == -3. && matches!(mark, Some(Match::Gaps { .. })),
            "the next column-mate beyond the nearest one must leave a positive gap");
        let axes = SnapTargets(std::array::from_fn(|axis| AxisTargets::new(&column, axis, false)));
        assert!(axes.snap(m, None, 1., true).iter().all(|(d, mark)| *d == 0. && mark.is_none()),
            "Alt suppresses equal spacing and its feedback");
        let tied = AxisTargets::new(&[run[0], run[1], Rect::new(106., 40., 126., 60.)], 0, false);
        let (delta, mark) = tied.pull(Rect::new(103., 0., 123., 20.), 0, None, 6.);
        assert!(delta == 3. && matches!(mark, Some(Match::Line(_))), "a line wins a tie with a gap");
        let tied = AxisTargets::new(&[Rect::new(30., 0., 54., 20.)], 0, true);
        let (delta, mark) = tied.pull(Rect::new(0., 0., 27., 20.), 0, Some(Handle([1, 0])), 6.);
        assert!(delta == 3. && matches!(mark, Some(Match::Line(_))), "a line wins a tie with a size");
    }

    #[test]
    fn resize_scales_about_the_opposite_handle_and_cancels_without_writing() {
        let mut h = History::new();
        let board = h.board();
        let section = h.push(item(kind::SECTION, board, [0, 0, 20, 10], None)).0;
        let child = h.push(item(kind::STICKY, section, [2, 2, 10, 10], None)).0;
        let other = h.push(item(kind::TEXT_BLOCK, board, [30, 20, 10, 10], None)).0;
        let v = BoardView::build(&h.state, board);
        let mut g = Gesture::default();
        g.set_viewport(&v, Rect::new(-50., -50., 100., 100.));
        g.selection.ids = [section, other].into();
        let start = dvec2(40., 30.);
        let end = dvec2(80., 60.);
        let zoom = 2.;
        let corner = start + dvec2(3., 3.) / zoom;
        assert_eq!(g.hit_handle(&v, corner + dvec2(7., 7.) / zoom, zoom), Some(Handle([1, 1])));
        assert_eq!(g.hit_handle(&v, corner + dvec2(7.1, 0.) / zoom, zoom), None);
        for escape in [false, true] {
            g.press_handle(&v, Handle([1, 1]), start);
            g.motion(&v, end, end, 1., true, false);
            let p = g.preview.as_ref().unwrap();
            assert_eq!(p.rect(&section), Some(Rect::new(0., 0., 40., 20.)));
            assert_eq!(p.rect(&other), Some(Rect::new(60., 40., 80., 60.)));
            assert!(!p.items.contains_key(&child), "section resize leaves members in place");
            if escape { g.cancel(); } else { assert!(g.focus_lost(&v).is_none()); }
            assert!(g.release(&v).is_none());
        }
        g.press_handle(&v, Handle([1, 1]), start);
        g.motion(&v, end, end, 1., true, false);
        let Some(Action::SetFields { changes, label }) = g.release(&v) else { panic!("expected group resize") };
        assert_eq!(label.as_deref(), Some("resize 2 items"));
        assert_eq!(changes.len(), 8);
        g.selection.ids = [section].into();
        g.press_handle(&v, Handle([1, 0]), start);
        g.motion(&v, end, end, 1., true, true);
        assert_eq!(g.preview.as_ref().unwrap().rect(&section), Some(Rect::new(0., -10., 60., 20.)));
        g.cancel();
        g.press_handle(&v, Handle([1, 1]), start);
        let near = start + dvec2(19., 6.);
        g.motion(&v, near, near, 1., false, true);
        assert_eq!(g.preview.as_ref().unwrap().rect(&section), Some(Rect::new(0., 0., 40., 20.)));
        assert!(g.marks.iter().all(|m| matches!(m, Some(Match::Line(_)))) && g.snap_segments().count() == 2,
            "both lines still coincide with the corner after preserving the aspect ratio");
        g.cancel();
        g.press_handle(&v, Handle([-1, -1]), start);
        g.motion(&v, end, end, 1., true, false);
        assert_eq!(g.preview.as_ref().unwrap().rect(&section), Some(Rect::new(12., 2., 20., 10.)));
        let Some(Action::SetFields { changes, label }) = g.release(&v) else { panic!("expected one resize") };
        assert_eq!(label.as_deref(), Some("resize 1 item"));
        assert_eq!(changes.len(), 4);
        assert!(g.release(&v).is_none());

        h.push(set(vec![change(other, field::W, Value::Int(0))]));
        let v = BoardView::build(&h.state, board);
        g.selection.ids = [other].into();
        g.press_handle(&v, Handle([1, 0]), start);
        g.motion(&v, end, end, 1., true, false);
        assert_eq!(g.preview.as_ref().unwrap().rect(&other), Some(Rect::new(30., 20., 70., 30.)));
        let huge = 1i128 << 65;
        assert_eq!(proportion(huge - 1, huge - 2, huge), huge - 3, "wide selection scaling rounds in ticks");
    }

    #[test]
    fn selection_drag_lock_and_key_gestures_are_local_until_release() {
        let mut h = History::new();
        let board = h.board();
        let section = h.push(item(kind::SECTION, board, [0, 0, 100, 80], Some(1))).0;
        let a = h.push(item(kind::STICKY, section, [5, 5, 10, 10], Some(2))).0;
        let b = h.push(item(kind::STICKY, section, [30, 5, 10, 10], Some(3))).0;
        let lock = h.push(item(kind::TEXT_BLOCK, board, [110, 5, 10, 10], Some(4))).0;
        h.push(set(vec![change(lock, field::LOCKED, Value::Bool(true))]));
        let v = BoardView::build(&h.state, board);
        let mut g = Gesture::default();
        let press = |g: &mut Gesture, id, shift| {
            g.press(&v, id, dvec2(0., 0.), dvec2(0., 0.), shift, None, false);
        };
        let click = |g: &mut Gesture, id, shift| {
            press(g, id, shift);
            assert!(g.release(&v).is_none());
        };
        click(&mut g, Some(a), false);
        click(&mut g, Some(b), true);
        assert_eq!(g.selection.ids, [a, b].into());
        press(&mut g, Some(a), false);
        g.motion(&v, dvec2(1., 0.), dvec2(2., 0.), 1., true, false);
        assert!(g.release(&v).is_none());
        assert_eq!(g.selection.ids, [a].into(), "clicking a selected item collapses the selection");
        click(&mut g, Some(a), true);
        assert!(g.selection.ids.is_empty());
        click(&mut g, Some(lock), false);
        g.press(&v, None, dvec2(-1., -1.), dvec2(-1., -1.), true, None, false);
        g.motion(&v, dvec2(46., 20.), dvec2(46., 20.), 1., true, false);
        assert!(g.release(&v).is_none());
        assert_eq!(g.selection.ids, [a, b, lock].into(), "shift-marquee adds content but not a partly enclosed section");
        press(&mut g, Some(lock), false);
        assert_eq!(g.cursor(&v, false), MouseCursor::NotAllowed);
        g.motion(&v, dvec2(9., 0.), dvec2(9., 0.), 1., true, false);
        assert!(g.release(&v).is_none());
        click(&mut g, None, false);
        assert!(g.selection.ids.is_empty());
        g.press(&v, None, dvec2(-1., -1.), dvec2(-1., -1.), false, None, false);
        g.motion(&v, dvec2(101., 81.), dvec2(101., 81.), 1., true, false);
        g.release(&v);
        assert_eq!(g.selection.ids, [a, b, section].into());
        g.select_all(&v);
        assert_eq!(g.selection.ids, [section, a, b, lock].into());
        assert!(matches!(g.delete(&v), Some(Action::DeleteItems { items, .. })
            if BTreeSet::from_iter(items.iter().copied()) == [section, a, b].into()));
        assert!(matches!(g.lock(&v), Some(Action::SetFields { changes, .. })
            if changes.len() == 4 && changes.iter().all(|c| c.value == Value::Bool(true))));
        for cancel in [true, false] {
            press(&mut g, Some(a), false);
            g.motion(&v, dvec2(6., 5.), dvec2(6., 5.), 1., true, false);
            if cancel {
                g.cancel();
            } else {
                g.focus_lost(&v);
            }
            assert!(g.release(&v).is_none(), "a cancelled capture writes nothing");
        }
        press(&mut g, Some(a), false);
        g.motion(&v, dvec2(8., 7.), dvec2(8., 7.), 1., true, false);
        assert!(matches!(g.release(&v), Some(Action::MoveItems { moves, .. })
            if moves.len() == 3 && moves.iter().all(|(id, _, _)| *id != lock)));
        assert!(g.release(&v).is_none(), "a release cannot commit twice");
        for key in [KeyCode::ArrowDown, KeyCode::ArrowRight, KeyCode::ArrowDown] {
            g.nudge(&v, key, key == KeyCode::ArrowRight);
        }
        assert!(g.key_up(&v, KeyCode::ArrowRight).is_none());
        assert!(matches!(g.key_up(&v, KeyCode::ArrowDown), Some(Action::MoveItems { moves, .. })
            if moves.contains(&(a, 15 * 64, 7 * 64))));
        g.nudge(&v, KeyCode::ArrowLeft, false);
        assert!(g.focus_lost(&v).is_some(), "leaving key focus commits the nudge");
        assert!(g.focus_lost(&v).is_none());
        g.nudge(&v, KeyCode::ArrowLeft, false);
        g.cancel();
        assert!(g.key_up(&v, KeyCode::ArrowLeft).is_none(), "Escape discards the held nudge");

        let x = (1_i64 << 53) + 1;
        h.push(set(vec![change(a, field::X, Value::Int(x)), change(b, field::X, Value::Int(i64::MAX))]));
        let v = BoardView::build(&h.state, board);
        g.selection.ids = [a].into();
        g.nudge(&v, KeyCode::ArrowRight, false);
        assert!(matches!(g.key_up(&v, KeyCode::ArrowRight), Some(Action::MoveItems { moves, .. })
            if moves == [(a, x + 64, 5 * 64)]), "one canvas unit adds exactly 64 fixed-point ticks");
        g.selection.add(b);
        g.nudge(&v, KeyCode::ArrowRight, false);
        assert!(g.key_up(&v, KeyCode::ArrowRight).is_none(), "one overflowing target cancels the entire move");

        h.push(set(vec![change(a, field::X, Value::Int(0)), change(a, field::W, Value::Int(x))]));
        let v = BoardView::build(&h.state, board);
        g.selection.ids = [a].into();
        g.press_handle(&v, Handle([1, 0]), dvec2(0., 0.));
        g.motion(&v, dvec2(1., 0.), dvec2(2., 0.), 2., true, false);
        let Some(Action::SetFields { changes, .. }) = g.release(&v) else { panic!("resize did not finish") };
        assert!(changes.contains(&change(a, field::W, Value::Int(x + 64))), "one canvas unit preserves the low bit of the width");

        let mut history = BoardHistory::default();
        history.record(a);
        history.record(b);
        let inverse = |of, id| move |action| {
            assert_eq!(action, Action::Undo { of });
            Some(id)
        };
        history.step(false, inverse(b, section));
        history.step(true, inverse(section, lock));
        history.step(false, inverse(lock, section));
        history.record(a);
        history.step(true, |_| panic!("a new edit clears redo"));
    }
}

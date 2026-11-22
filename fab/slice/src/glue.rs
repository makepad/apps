//! Parts that print as one body ([`fab_plate::Plate::groups`]): a group slices as one object,
//! each of its layers the union of its parts' outlines. Nothing is united in 3D: each part stays
//! its own source, of any engine, and only its layers are glued. The group's first part carries
//! the group ([`Glued`]); the others slice to nothing of their own ([`Carried`]).
//!
//! - One filament and no paint: one solid. Where the parts overlap it prints once, with no wall
//!   between them, and its walls, infill, skins and supports follow the union.
//! - Filaments or paint that differ: a later part owns its whole volume, so where it overlaps an
//!   earlier one only it prints, in its own filament, and a painted part keeps its paint over
//!   what it owns. The group's paint is by regions (each state's own walls), or by position when
//!   a part of it paints by position (the paths then split where the state changes, as such a
//!   part's do).

use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::{bbox_of, BBox, ExPolygon, Pt, SCALE};
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_slicer::paint::{LayerPaint, PaintParams};
use makepad_fab_slicer::source::{ContourSource, PaintAt, PaintHere, SettingsZone, SlopeProfile, SupportPaint, SupportRegion};
use makepad_fab_toolpath::{ObjectLayer, Toolpaths};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// A part's contour source, shared.
pub type Source = Arc<dyn ContourSource + Send + Sync>;

/// Which parts glue, and which print apart though their object prints its bodies as one.
#[derive(Debug, Default, PartialEq)]
pub struct Gluing {
    /// The groups that glue, each its parts in order.
    pub glued: Vec<Vec<usize>>,
    /// The groups that print as separate parts, each with why ([`apart`]).
    pub apart: Vec<(Vec<usize>, String)>,
}

/// The groups of `n` parts that glue, from `groups` (each its parts' indices in order): two or
/// more parts each, a part in one group at most (the first that names it). A group glues unless
/// `apart` says why its parts can't ([`apart`]): it then prints as separate parts.
pub fn groups(groups: &[Vec<usize>], n: usize, apart: impl Fn(&[usize]) -> Option<String>) -> Gluing {
    let mut taken = vec![false; n];
    let mut out = Gluing::default();
    for g in groups {
        let mut parts: Vec<usize> = Vec::with_capacity(g.len());
        for &i in g {
            if i < n && !taken[i] && !parts.contains(&i) {
                parts.push(i);
            }
        }
        if parts.len() < 2 {
            continue;
        }
        if let Some(why) = apart(&parts) {
            out.apart.push((parts, why));
            continue;
        }
        for &i in &parts {
            taken[i] = true;
        }
        out.glued.push(parts);
    }
    out
}

/// Why the parts `g` print apart, their sources `src` and their names `name`, when they do: a
/// mix of paint by position with paint by regions (a painted mesh's facets, a coloured field's
/// slots), which no layer's split takes both of. Parts of one kind of paint print as one.
pub fn apart<'a>(g: &[usize], src: impl Fn(usize) -> &'a (dyn ContourSource + Send + Sync), name: impl Fn(usize) -> &'a str) -> Option<String> {
    let by_position = g.iter().copied().find(|&i| src(i).paint_at().is_some());
    let by_regions = g.iter().copied().find(|&i| src(i).paint_at().is_none() && src(i).painted());
    let (p, r) = by_position.zip(by_regions)?;
    Some(format!("{} paints by position and {} by regions (a painted mesh's facets or a coloured field's slots), which no layer's split takes both of", name(p), name(r)))
}

/// The paint state of a part printing in `filament` in a group printing in `own` (both 0-based):
/// 0 for the group's own filament, else the filament's number.
pub fn state_of(filament: usize, own: usize) -> u32 {
    if filament == own { 0 } else { filament as u32 + 1 }
}

/// `srcs` with `groups` glued ([`groups`]): each group's first part slices the group
/// ([`Glued`]), its others [`Carried`]. Part `i`'s frame sits at `at[i]` on the plate, and it
/// prints in `filaments[i]` (0-based).
pub fn apply(groups: &[Vec<usize>], srcs: &mut [Source], at: &[Pt], filaments: &[usize]) {
    for g in groups {
        let first = g[0];
        let members = g.iter().map(|&i| Member { src: srcs[i].clone(), at: at[i].sub(at[first]), state: state_of(filaments[i], filaments[first]) }).collect();
        for &i in &g[1..] {
            srcs[i] = Arc::new(Carried::of(&*srcs[i]));
        }
        srcs[first] = Arc::new(Glued::new(members));
    }
}

/// A part of a glued group: its source, where its frame sits in the group's (its first part's),
/// and the paint state it prints in (0: the group's own filament, N: filament N).
#[derive(Clone)]
pub struct Member {
    pub src: Source,
    pub at: Pt,
    pub state: u32,
}

/// A glued group as one contour source, in its first part's frame: each layer the union of its
/// parts' outlines, each part owning what it covers of the parts before it.
pub struct Glued(Arc<Group>);

struct Group {
    /// Unique among groups (the paint's per-thread plane, [`LAST`]).
    id: u64,
    members: Vec<Member>,
    /// Each part's paint by position, when it has one.
    paint: Vec<Option<PaintAt>>,
    bounds: ([f64; 3], [f64; 3]),
    /// The union of its parts' pivot boxes ([`ContourSource::pivot_box`]), in its frame.
    pivot: ([f64; 2], [f64; 2]),
    /// Its parts print in more than one state (filaments or paint).
    painted: bool,
    /// The planes its contours cut or its paint read, by z (its bits).
    planes: Mutex<HashMap<u64, Arc<Plane>>>,
}

/// A plane of a painted group: the regions there of each part after the first, in the group's
/// frame (`later[m - 1]` part `m`'s), and their edges once the paint by position reads them.
struct Plane {
    later: Vec<Vec<ExPolygon>>,
    index: OnceLock<Index>,
}

/// A plane's later parts as the paint by position reads them.
struct Index {
    /// Per later part, when it reaches the plane: its box and its edges (whether it holds a point).
    each: Vec<Option<(BBox, SegGrid<usize>)>>,
    /// Every later part's edges: no owner changes nearer than the nearest.
    edges: SegGrid<()>,
}

impl Index {
    fn of(later: &[Vec<ExPolygon>]) -> Index {
        let each = later.iter().map(|r| (!r.is_empty()).then(|| (bbox_of(r), SegGrid::of_regions(r, |k| k)))).collect();
        let mut segs = Vec::new();
        for e in later.iter().flatten() {
            for ring in e.rings() {
                let m = ring.pts.len();
                segs.extend((0..m).map(|i| (ring.pts[i], ring.pts[(i + 1) % m], ())));
            }
        }
        Index { each, edges: SegGrid::new(segs) }
    }
}

thread_local! {
    /// The plane this thread's paint read last (its group's id, its z bits): the points of a
    /// layer's paths reach it without the planes' lock.
    static LAST: RefCell<Option<(u64, u64, Arc<Plane>)>> = const { RefCell::new(None) };
}

static GROUPS: AtomicU64 = AtomicU64::new(0);

impl Glued {
    pub fn new(members: Vec<Member>) -> Glued {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        let mut pivot = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for m in &members {
            let (a, b) = m.src.bounds();
            if (0..3).any(|k| a[k] > b[k]) {
                continue;
            }
            let d = [m.at.x_mm(), m.at.y_mm(), 0.0];
            for k in 0..3 {
                lo[k] = lo[k].min(a[k] + d[k]);
                hi[k] = hi[k].max(b[k] + d[k]);
            }
            let (a, b) = m.src.pivot_box();
            for k in 0..2 {
                pivot.0[k] = pivot.0[k].min(a[k] + d[k]);
                pivot.1[k] = pivot.1[k].max(b[k] + d[k]);
            }
        }
        let paint = members.iter().map(|m| m.src.paint_at()).collect();
        let painted = members.iter().any(|m| m.state != 0 || m.src.painted());
        Glued(Arc::new(Group { id: GROUPS.fetch_add(1, Ordering::Relaxed), members, paint, bounds: (lo, hi), pivot, painted, planes: Mutex::default() }))
    }
}

/// `regions` moved by `d`.
fn moved(mut regions: Vec<ExPolygon>, d: Pt) -> Vec<ExPolygon> {
    if d != Pt::default() {
        for e in &mut regions {
            e.translate(d);
        }
    }
    regions
}

impl Group {
    /// Each part from `from` on cut at the planes `zs` it reaches, in the group's frame: per part,
    /// per plane.
    fn cut(&self, zs: &[f64], from: usize) -> Vec<Vec<Vec<ExPolygon>>> {
        self.members[from..].iter().map(|m| {
            let mut layers = vec![Vec::new(); zs.len()];
            let (lo, hi) = m.src.bounds();
            let at: Vec<usize> = (0..zs.len()).filter(|&k| zs[k] >= lo[2] && zs[k] <= hi[2]).collect();
            if !at.is_empty() {
                let mine: Vec<f64> = at.iter().map(|&k| zs[k]).collect();
                for (k, regions) in at.into_iter().zip(m.src.contours(&mine)) {
                    layers[k] = moved(regions, m.at);
                }
            }
            layers
        }).collect()
    }

    /// Keeps the planes `zs` of the later parts' cuts `later` (per later part, per plane).
    fn keep(&self, zs: &[f64], later: &[Vec<Vec<ExPolygon>>]) {
        let mut planes = self.planes.lock().unwrap();
        for (k, z) in zs.iter().enumerate() {
            planes.entry(z.to_bits()).or_insert_with(|| Arc::new(Plane { later: later.iter().map(|c| c[k].clone()).collect(), index: OnceLock::new() }));
        }
    }

    /// The planes `zs`: kept, else the later parts cut there.
    fn planes(&self, zs: &[f64]) -> Vec<Arc<Plane>> {
        let missing: Vec<f64> = {
            let planes = self.planes.lock().unwrap();
            zs.iter().copied().filter(|z| !planes.contains_key(&z.to_bits())).collect()
        };
        if !missing.is_empty() {
            self.keep(&missing, &self.cut(&missing, 1));
        }
        let planes = self.planes.lock().unwrap();
        zs.iter().map(|z| planes[&z.to_bits()].clone()).collect()
    }

    /// Who owns point (`x`, `y`) of plane `z`, the last part holding it, and how far (mm) at
    /// least it is from where another part could.
    fn owner(&self, x: f64, y: f64, z: f64) -> (usize, f64) {
        let q = Pt::from_mm(x, y);
        let key = (self.id, z.to_bits());
        LAST.with(|last| {
            let hit = matches!(&*last.borrow(), Some((g, zb, _)) if (*g, *zb) == key);
            if !hit {
                let plane = self.planes(&[z]).remove(0);
                *last.borrow_mut() = Some((key.0, key.1, plane));
            }
            let last = last.borrow();
            let plane = &last.as_ref().expect("the plane was just kept").2;
            let index = plane.index.get_or_init(|| Index::of(&plane.later));
            let held = |m: usize| index.each[m - 1].as_ref().is_some_and(|(bb, grid)| q.x >= bb.min.x && q.x <= bb.max.x && q.y >= bb.min.y && q.y <= bb.max.y && grid.contains_any(q));
            let owner = (1..self.members.len()).rev().find(|&m| held(m)).unwrap_or(0);
            let edge = index.edges.nearest(q, |_| 0u8).map_or(f64::INFINITY, |(d, _)| d / SCALE);
            (owner, edge)
        })
    }

    /// The paint at point (`x`, `y`, `z`) of the group's frame: its owner's.
    fn paint_here(&self, x: f64, y: f64, z: f64) -> PaintHere {
        let (owner, edge) = self.owner(x, y, z);
        let m = &self.members[owner];
        match &self.paint[owner] {
            Some(paint) => {
                let h = paint(x - m.at.x_mm(), y - m.at.y_mm(), z);
                let own = |s: u32| if s == 0 { m.state } else { s };
                let state = own(h.state);
                PaintHere { state, dist: h.dist.min(edge), mix: h.mix.map(|(s, share)| (own(s), share)).filter(|(s, _)| *s != state) }
            }
            None => PaintHere::solid(m.state, edge),
        }
    }
}

impl ContourSource for Glued {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.0.bounds
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.0.pivot
    }

    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        let g = &*self.0;
        let mut cuts = g.cut(zs, 0);
        if g.painted {
            g.keep(zs, &cuts[1..]);
        }
        (0..zs.len()).map(|k| {
            let mut layer: Vec<ExPolygon> = Vec::new();
            let mut parts = 0;
            for c in cuts.iter_mut().filter(|c| !c[k].is_empty()) {
                layer.append(&mut c[k]);
                parts += 1;
            }
            if parts > 1 { union_ex(&layer) } else { layer }
        }).collect()
    }

    fn painted(&self) -> bool {
        self.0.painted
    }

    /// Each layer split by owner, a later part first: what it covers of the layer is its own,
    /// the first part's the rest; each part's paint over what it owns, else its own state there.
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
        let g = &*self.0;
        if !g.painted {
            return slices.iter().map(|s| vec![(0, s.clone())]).collect();
        }
        let planes = g.planes(zs);
        let n = g.members.len();
        let mut owned: Vec<Vec<Vec<ExPolygon>>> = vec![vec![Vec::new(); zs.len()]; n];
        for k in 0..zs.len() {
            let mut taken: Vec<ExPolygon> = Vec::new();
            for m in (1..n).rev() {
                let r = &planes[k].later[m - 1];
                if r.is_empty() {
                    continue;
                }
                let mine = intersection_ex(r, &slices[k]);
                owned[m][k] = if taken.is_empty() { mine } else { difference_ex(&mine, &taken) };
                taken = if taken.is_empty() { r.clone() } else { union_ex(&[taken, r.clone()].concat()) };
            }
            owned[0][k] = if taken.is_empty() { slices[k].clone() } else { difference_ex(&slices[k], &taken) };
        }
        // per layer: each state's regions, and how many parts gave it some
        let mut out: Vec<Vec<(u32, Vec<ExPolygon>, usize)>> = vec![Vec::new(); zs.len()];
        for (m, member) in g.members.iter().enumerate() {
            let (lo, hi) = member.src.bounds();
            // the first part owns the rest of every layer; another, what it covers where it reaches
            let layers: Vec<usize> = (0..zs.len()).filter(|&k| m == 0 || (zs[k] >= lo[2] && zs[k] <= hi[2])).collect();
            if layers.iter().all(|&k| owned[m][k].is_empty()) {
                continue;
            }
            let split: Vec<LayerPaint> = if member.src.painted() {
                let mz: Vec<f64> = layers.iter().map(|&k| zs[k]).collect();
                let back = Pt::default().sub(member.at);
                let mine: Vec<Vec<ExPolygon>> = layers.iter().map(|&k| moved(std::mem::take(&mut owned[m][k]), back)).collect();
                member.src.paint_regions(&mz, &mine, p).into_iter().map(|l| l.into_iter().map(|(s, r)| (s, moved(r, member.at))).collect()).collect()
            } else {
                layers.iter().map(|&k| vec![(0, std::mem::take(&mut owned[m][k]))]).collect()
            };
            for (&k, l) in layers.iter().zip(split) {
                for (s, r) in l.into_iter().filter(|(_, r)| !r.is_empty()) {
                    let s = if s == 0 { member.state } else { s };
                    match out[k].iter_mut().find(|e| e.0 == s) {
                        Some(e) => {
                            e.1.extend(r);
                            e.2 += 1;
                        }
                        None => out[k].push((s, r, 1)),
                    }
                }
            }
        }
        // a state several parts print in is one region; state 0 first
        out.into_iter().map(|mut l| {
            l.sort_by_key(|e| e.0 != 0);
            l.into_iter().map(|(s, r, parts)| (s, if parts > 1 { union_ex(&r) } else { r })).collect()
        }).collect()
    }

    /// When a part paints by position: each point's owner's paint, a part's own state where it
    /// has none.
    fn paint_at(&self) -> Option<PaintAt> {
        let g = self.0.clone();
        g.paint.iter().any(Option::is_some).then(|| Arc::new(move |x: f64, y: f64, z: f64| g.paint_here(x, y, z)) as PaintAt)
    }

    /// Its parts' support channels, each where its part sits.
    fn support_paint(&self) -> Option<SupportPaint> {
        let each: Vec<(SupportPaint, Pt)> = self.0.members.iter().filter_map(|m| m.src.support_paint().map(|p| (p, m.at))).collect();
        if each.is_empty() {
            return None;
        }
        let join = |pick: fn(&SupportPaint) -> Option<&SupportRegion>| united(each.iter().filter_map(|(p, at)| pick(p).map(|r| (r.clone(), *at))).collect());
        Some(SupportPaint { block: join(|p| p.block.as_ref()), enforce: join(|p| p.enforce.as_ref()), no_trees: join(|p| p.no_trees.as_ref()) })
    }

    /// The layers its first part says (a test print's parts of one cube share them).
    fn layer_stack(&self) -> Option<Arc<[(f64, f64)]>> {
        self.0.members.first().and_then(|m| m.src.layer_stack())
    }

    /// Its parts' regions with print settings of their own, each where its part sits, in the
    /// parts' order: where two overlap the later one's values win, as a later part owns where it
    /// overlaps an earlier.
    fn settings_zones(&self) -> Vec<SettingsZone> {
        self.0.members.iter().flat_map(|m| {
            let d = [m.at.x_mm(), m.at.y_mm()];
            m.src.settings_zones().into_iter().map(move |z| {
                let at = z.region.at.clone();
                let region = SupportRegion::new(Arc::new(move |x, y, z| at(x - d[0], y - d[1], z)), z.region.bounds.map(|(lo, hi)| ([lo[0] + d[0], lo[1] + d[1], lo[2]], [hi[0] + d[0], hi[1] + d[1], hi[2]])));
                SettingsZone { region, ..z }
            })
        }).collect()
    }

    /// Steep where any of its parts is (unknown when one's is).
    fn slope_profile(&self, step: f64) -> Option<SlopeProfile> {
        let each = self.0.members.iter().map(|m| m.src.slope_profile(step)).collect::<Option<Vec<SlopeProfile>>>()?;
        let z0 = each.iter().map(|p| p.z0).fold(f64::INFINITY, f64::min);
        let z1 = each.iter().map(|p| p.z0 + p.nz.len() as f64 * p.step).fold(f64::NEG_INFINITY, f64::max);
        if z0 > z1 {
            return None;
        }
        let mut out = SlopeProfile::empty(z0, z1, step);
        for p in &each {
            for (k, &nz) in p.nz.iter().enumerate() {
                let za = p.z0 + k as f64 * p.step;
                out.add(za, za + p.step, nz as f64);
            }
            out.flats.extend_from_slice(&p.flats);
        }
        out.finish();
        Some(out)
    }
}

/// The union of support regions, each where its part sits.
fn united(regions: Vec<(SupportRegion, Pt)>) -> Option<SupportRegion> {
    if regions.is_empty() {
        return None;
    }
    let mut bounds = Some(([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]));
    for (r, at) in &regions {
        bounds = match (bounds, r.bounds) {
            (Some((mut lo, mut hi)), Some((a, b))) => {
                let d = [at.x_mm(), at.y_mm(), 0.0];
                for k in 0..3 {
                    lo[k] = lo[k].min(a[k] + d[k]);
                    hi[k] = hi[k].max(b[k] + d[k]);
                }
                Some((lo, hi))
            }
            _ => None,
        };
    }
    let at = Arc::new(move |x: f64, y: f64, z: f64| regions.iter().map(|(r, d)| (r.at)(x - d.x_mm(), y - d.y_mm(), z)).fold(f64::INFINITY, f64::min));
    Some(SupportRegion::new(at, bounds))
}

/// A glued group's part other than its first: its layers are the group's ([`Glued`]), so it
/// slices to none of its own. Its box keeps the part's footprint, on the bed.
pub struct Carried {
    bounds: ([f64; 3], [f64; 3]),
    pivot: ([f64; 2], [f64; 2]),
}

impl Carried {
    pub fn of(src: &dyn ContourSource) -> Carried {
        let (lo, hi) = src.bounds();
        Carried { bounds: ([lo[0], lo[1], 0.0], [hi[0], hi[1], 0.0]), pivot: src.pivot_box() }
    }
}

impl ContourSource for Carried {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.bounds
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.pivot
    }

    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        vec![Vec::new(); zs.len()]
    }
}

/// Each object's volume (mm³) as its layers print it, supports left out: a glued group's is
/// its union's, what it fills once however its parts overlap.
pub fn volumes(tp: &Toolpaths) -> Vec<f64> {
    let mut out = vec![0.0; tp.objects.len()];
    for l in &tp.layers {
        for ol in &l.objects {
            if let Some(v) = out.get_mut(ol.object) {
                *v += layer_volume(l.height, ol);
            }
        }
    }
    out
}

/// What an object's layer `l` fills (mm³) printed `height` tall: none for a layer of its
/// supports alone.
pub fn layer_volume(height: f64, l: &ObjectLayer) -> f64 {
    if l.support_only {
        return 0.0;
    }
    l.slices.iter().map(ExPolygon::area_mm2).sum::<f64>() * height
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::geom::{total_area_mm2, Polygon};

    #[test]
    fn a_group_takes_its_parts_in_order_once_each() {
        let any = |_: &[usize]| None;
        assert_eq!(groups(&[vec![2, 0], vec![0, 1, 3], vec![4]], 4, any).glued, vec![vec![2, 0], vec![1, 3]], "a part glues in the first group that names it; a group needs two");
        assert_eq!(groups(&[vec![0, 1, 1, 7]], 3, any).glued, vec![vec![0, 1]], "a repeat and a part out of range do not count");
        assert_eq!(groups(&[vec![0, 1, 1]], 3, any).glued, vec![vec![0, 1]], "nor make a group of one part two");
        assert_eq!(groups(&[vec![1, 1]], 3, any), Gluing::default());
        let why = |g: &[usize]| g.contains(&3).then(|| "part 3 can't".to_string());
        assert_eq!(groups(&[vec![0, 1], vec![2, 3]], 4, why), Gluing { glued: vec![vec![0, 1]], apart: vec![(vec![2, 3], "part 3 can't".into())] }, "a group whose parts can't glue prints apart, and says why");
    }

    /// A part painted by regions, as a painted mesh's facets are: its paint splits each layer.
    struct Regions;

    impl ContourSource for Regions {
        fn bounds(&self) -> ([f64; 3], [f64; 3]) {
            ([0.0; 3], [10.0; 3])
        }
        fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
            vec![Vec::new(); zs.len()]
        }
        fn painted(&self) -> bool {
            true
        }
    }

    /// A part painted by position and one painted by regions print apart, and say which; either
    /// with an unpainted part prints as one.
    #[test]
    fn paint_by_position_and_by_regions_print_apart() {
        let srcs: Vec<Source> = vec![Arc::new(Slab { x: (0.0, 20.0), paint: true }), Arc::new(Regions), Arc::new(Slab { x: (0.0, 20.0), paint: false })];
        let names = ["head", "lock", "other"];
        let why = |g: &[usize]| apart(g, |i| &*srcs[i], |i| names[i]);
        assert_eq!((why(&[0, 2]), why(&[1, 2])), (None, None), "one kind of paint and none glue");
        let gluing = groups(&[vec![0, 1, 2]], 3, why);
        assert!(gluing.glued.is_empty(), "{gluing:?}");
        assert_eq!(gluing.apart.len(), 1);
        assert_eq!(gluing.apart[0].0, vec![0, 1, 2]);
        assert!(gluing.apart[0].1.starts_with("head paints by position and lock by regions"), "{}", gluing.apart[0].1);
    }

    /// A box `[x0, x1] x [0, 10]` mm, 10 mm tall, painted by position (red, state 2, right of
    /// `x = 25`) when `paint`.
    struct Slab {
        x: (f64, f64),
        paint: bool,
    }

    impl ContourSource for Slab {
        fn bounds(&self) -> ([f64; 3], [f64; 3]) {
            ([self.x.0, 0.0, 0.0], [self.x.1, 10.0, 10.0])
        }
        fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
            zs.iter().map(|_| vec![ExPolygon::new(Polygon::rect(Pt::from_mm(self.x.0, 0.0), Pt::from_mm(self.x.1, 10.0)))]).collect()
        }
        fn painted(&self) -> bool {
            self.paint
        }
        fn paint_at(&self) -> Option<PaintAt> {
            self.paint.then(|| Arc::new(|x: f64, _: f64, _: f64| PaintHere::solid(if x > 25.0 { 2 } else { 0 }, (x - 25.0).abs())) as PaintAt)
        }
    }

    fn area(l: &LayerPaint, state: u32) -> f64 {
        l.iter().filter(|(s, _)| *s == state).map(|(_, r)| total_area_mm2(r)).sum()
    }

    /// Two boxes overlapping over `x` 10..20 mm, the later in filament 3: it owns the overlap,
    /// whole, and the first the rest; by position too, where the first one's paint holds outside
    /// the later one.
    #[test]
    fn a_later_part_owns_its_whole_volume() {
        let group = |paint: bool| Glued::new(vec![
            Member { src: Arc::new(Slab { x: (0.0, 20.0), paint }), at: Pt::default(), state: 0 },
            Member { src: Arc::new(Slab { x: (0.0, 20.0), paint: false }), at: Pt::from_mm(10.0, 0.0), state: 3 },
        ]);
        let g = group(false);
        let zs = [1.0, 5.0];
        let slices = g.contours(&zs);
        assert!((total_area_mm2(&slices[0]) - 300.0).abs() < 1e-6, "one outline: the union");
        let split = g.paint_regions(&zs, &slices, &PaintParams { top_layers: 0, bottom_layers: 0, shrink: 0.0, max_width: 0.0, interlocking_depth: 0.0 });
        for l in &split {
            assert!((area(l, 3) - 200.0).abs() < 1e-6 && (area(l, 0) - 100.0).abs() < 1e-6, "the later box owns all of itself: {:?}", l.iter().map(|(s, r)| (*s, total_area_mm2(r))).collect::<Vec<_>>());
        }
        assert!(g.paint_at().is_none(), "no part paints by position");
        let g = group(true);
        let paint = g.paint_at().expect("by position");
        g.contours(&zs);
        for (x, state) in [(5.0, 0), (15.0, 3), (25.0, 3)] {
            assert_eq!(paint(x, 5.0, 5.0).state, state, "at x {x}");
        }
        let near = paint(9.0, 5.0, 5.0);
        assert!(near.dist <= 1.0 + 1e-9, "the owner changes 1 mm away: {}", near.dist);
        // the first box's own paint where it is no other part's: a box past it, its red right of 25
        let wide = Glued::new(vec![
            Member { src: Arc::new(Slab { x: (0.0, 40.0), paint: true }), at: Pt::default(), state: 0 },
            Member { src: Arc::new(Slab { x: (0.0, 5.0), paint: false }), at: Pt::default(), state: 3 },
        ]);
        let paint = wide.paint_at().expect("by position");
        for (x, state) in [(2.0, 3), (10.0, 0), (30.0, 2)] {
            assert_eq!(paint(x, 5.0, 7.0).state, state, "at x {x}, a plane no contour cut");
        }
    }
}

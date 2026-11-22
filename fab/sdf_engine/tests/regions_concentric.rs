//! S1b (regions_concentric) on the GPU through gpu_jobs Steps, over a chain table built by an
//! f64 stand-in of extract and the chain's reference table:
//! - the regions are rectilinear, on a raster of half grid units;
//! - each level is an exact miter (Chebyshev) inset of its plane's region, and each component
//!   of a level gives one contour ring plus one ring per hole;
//! - loop_tree.z is the innermost contour of the reference level holding the ring.
//! The records are checked against the CPU's round recursion over the same raster. They are
//! byte-identical at three job sizes, and the same with given and counted loop counts.
//! `assemble` is checked for its fill order.
//!
//! On the real path, the same regions' outlines go through the feature field, extract and the
//! chain with S1b's reference table, and S1b's records name the same rings as the raster's.
#[path = "support/gpu_rig.rs"]
mod rig;
#[path = "support/pack.rs"]
mod pack;
use fab_sdf_engine::{
    chain::{self, ChainGrid, ChainKernels},
    feature_field::{self, FieldInput, FieldJoin, FieldKernels, FieldParams},
    feature_set::{from_loops, FeatureSet, Loop},
    gpu_jobs::{BufRef, Finished, ItemStats, JobClass, Output, Pool, ScanKernels},
    makepad_draw::{ComputeKernel, Cx},
    plane_abi::{vertex_feature, VertexSub, LOOP_HOLE, LOOP_OPEN, NONE},
    regions_concentric::{self as s1b, Fill, Item, Kind, Record, Region, S1bDepth, S1bInputs, S1bKernels, S1bSpec, S1bWords, State, Thickness},
    regions_overhang::Size,
    wall_pieces::{self, Levels, PieceKernels},
};
use std::{cell::RefCell, collections::{BTreeMap, BTreeSet}, rc::Rc};

/// The boundary slot's level id (a region level, as SDFREGIONS' compose would give it).
const BOUNDARY: u32 = 70;
/// The raster: grid units across and up (half-unit cells).
const W: i32 = 120;
const H: i32 = 80;

/// A plane's region: its spacing s0 in grid units, and rectangles (x0, y0, x1, y1) in grid
/// units, added (true) or cut in order.
struct Plane { s0: i32, rects: Vec<(i32, i32, i32, i32, bool)> }

fn main_planes() -> Vec<Plane> {
    vec![
        // a first layer (s0 0.6 mm): a square ending at round 1, and one wide through round 2,
        // whose round-3 child is past the run's 3 rounds
        Plane { s0: 6, rects: vec![(2, 2, 25, 25, true), (40, 2, 81, 43, true)] },
        Plane { s0: 4, rects: vec![
            // a square: wide at rounds 0 and 1, handed over at 2
            (2, 2, 25, 25, true),
            // a band 11 wide round a hole: wide at 0, its holed child handed over
            (30, 2, 61, 33, true), (41, 13, 50, 22, false),
            // a dumbbell: one loop at round 0, two children
            (65, 2, 80, 17, true), (85, 2, 100, 17, true), (80, 7, 85, 12, true),
            // a band 9 wide (handed over at 0) round a hole holding an island (wide at 0)
            (2, 36, 41, 75, true), (11, 45, 32, 66, false), (16, 50, 27, 61, true),
            // a strip 3 wide: handed over at 0
            (50, 40, 53, 70, true),
            // an L, 13 wide
            (64, 36, 95, 49, true), (64, 49, 77, 78, true),
        ] },
    ]
}

/// The narrow run: a square deeper than 6 s (not narrow) beside one that is.
fn narrow_planes() -> Vec<Plane> {
    vec![Plane { s0: 6, rects: vec![(2, 2, 25, 25, true)] }, Plane { s0: 4, rects: vec![(2, 2, 25, 25, true), (30, 2, 79, 51, true)] }]
}

/// The band run: a holed band (the CPU's Round loop) beside a holeless square.
fn band_planes() -> Vec<Plane> {
    vec![Plane { s0: 4, rects: vec![(2, 2, 25, 25, true), (30, 2, 61, 33, true), (41, 13, 50, 22, false)] }]
}

/// Each cell's Chebyshev depth in half units: the distance to the nearest cell outside (0 outside).
fn depth(plane: &Plane) -> Vec<u32> {
    let (w, h) = ((W * 2) as usize, (H * 2) as usize);
    let mut on = vec![false; w * h];
    for &(x0, y0, x1, y1, add) in &plane.rects {
        for y in y0 * 2..y1 * 2 { for x in x0 * 2..x1 * 2 { on[y as usize * w + x as usize] = add; } }
    }
    let mut d: Vec<u32> = on.iter().map(|&o| if o { u32::MAX } else { 0 }).collect();
    let at = |x: i64, y: i64, d: &[u32]| if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 { 0 } else { d[y as usize * w + x as usize] };
    for pass in 0..2 {
        for k in 0..w * h {
            let k = if pass == 0 { k } else { w * h - 1 - k };
            if d[k] == 0 { continue; }
            let (x, y) = ((k % w) as i64, (k / w) as i64);
            let m = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)].iter().map(|(dx, dy)| at(x + dx, y + dy, &d)).min().unwrap();
            d[k] = d[k].min(m.saturating_add(1));
        }
    }
    d
}

/// The components of {depth > d}: per cell its component (NONE off it), the count, and the
/// holes as (component) in discovery order. Material is 4-connected, the outside 8-connected.
/// Each component's and hole's box in half units (x0, y0, x1, y1).
struct Level { comp: Vec<u32>, comps: u32, holes: Vec<u32>, comp_box: Vec<[i64; 4]>, hole_box: Vec<[i64; 4]> }

fn grow(b: &mut [i64; 4], x: usize, y: usize) {
    let (x, y) = (x as i64, y as i64);
    *b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
}
const EMPTY: [i64; 4] = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];

fn level(depth: &[u32], d: u32) -> Level {
    let (w, h) = ((W * 2) as usize, (H * 2) as usize);
    let inside = |k: usize| depth[k] > d;
    let mut comp = vec![NONE; w * h];
    let mut comps = 0;
    let mut stack = vec![];
    let mut comp_box = vec![];
    for k in 0..w * h {
        if !inside(k) || comp[k] != NONE { continue; }
        comp[k] = comps;
        stack.push(k);
        comp_box.push(EMPTY);
        while let Some(c) = stack.pop() {
            let (x, y) = (c % w, c / w);
            grow(&mut comp_box[comps as usize], x, y);
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < w && ny < h && inside(ny * w + nx) && comp[ny * w + nx] == NONE { comp[ny * w + nx] = comps; stack.push(ny * w + nx); }
            }
        }
        comps += 1;
    }
    let mut seen = vec![false; w * h];
    let (mut holes, mut hole_box) = (vec![], vec![]);
    for k in 0..w * h {
        if inside(k) || seen[k] { continue; }
        seen[k] = true;
        stack.push(k);
        let (mut border, mut owner, mut bb) = (false, NONE, EMPTY);
        while let Some(c) = stack.pop() {
            grow(&mut bb, c % w, c / w);
            let (x, y) = (c as i64 % w as i64, c as i64 / w as i64);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 { border = true; continue; }
                    let n = ny as usize * w + nx as usize;
                    if inside(n) { if dx == 0 || dy == 0 { owner = comp[n]; } } else if !seen[n] { seen[n] = true; stack.push(n); }
                }
            }
        }
        if !border { holes.push(owner); hole_box.push(bb); }
    }
    Level { comp, comps, holes, comp_box, hole_box }
}

/// A level's inset in half units: |value| * 2 (the wide test without its margin: the raster's
/// components are the same just past it).
fn inset(id: u32, s0: i32, rounds: u32) -> Option<u32> {
    let s0 = s0 as u32;
    if id == BOUNDARY { return Some(0); }
    for r in 0..=rounds {
        if r >= 1 && id == s1b::boundary_level(r, BOUNDARY) { return Some(2 * r * s0); }
        if r < rounds && id == s1b::loop_level(r) { return Some((2 * r + 1) * s0); }
        if r < rounds && id == s1b::thick_level(r) { return Some((8 * r + 10) * s0 / 4); }
    }
    (id == s1b::NARROW_LEVEL).then_some(48)
}

/// A ring by (plane, level, hole, box in half units): the name a raster ring and a chained loop share.
type Key = (u32, u32, bool, [i64; 4]);

/// The stand-in chain: per loop (loops, loop_meta, loop_tree, piece_info) records, the loop id
/// of each (plane, level, component) contour, and of each hole, and each ring's key.
struct Table {
    keys: BTreeMap<u32, Key>,
    loops: Vec<[u32; 4]>,
    meta: Vec<[u32; 4]>,
    tree: Vec<[u32; 4]>,
    info: Vec<[u32; 4]>,
    contour: BTreeMap<(u32, u32, u32), u32>,
    holes: BTreeMap<(u32, u32, u32), Vec<u32>>,
    levels: BTreeMap<(u32, u32), Level>,
    sources: BTreeMap<(u32, u32), u32>,
}

fn table(planes: &[Plane], spec: &S1bSpec) -> Table {
    let ids = spec.levels().unwrap().ids;
    let reference: BTreeMap<u32, u32> = spec.reference().into_iter().collect();
    // rings in (plane, level slot, component, contour then holes) order, then scrambled ids
    let mut rings = vec![];
    let mut levels = BTreeMap::new();
    for (p, plane) in planes.iter().enumerate() {
        let depth = depth(plane);
        for &id in &ids {
            let lv = level(&depth, inset(id, plane.s0, spec.rounds).unwrap());
            for c in 0..lv.comps {
                rings.push((p as u32, id, c, NONE));
                for (h, &o) in lv.holes.iter().enumerate() { if o == c { rings.push((p as u32, id, c, h as u32)); } }
            }
            levels.insert((p as u32, id), lv);
        }
    }
    let n = rings.len() as u32 + 2;
    let mut order: Vec<u32> = (0..n).collect();
    order.sort_by_key(|&i| (i.wrapping_mul(2654435761) >> 7, i));
    let mut t = Table { keys: BTreeMap::new(), loops: vec![[0; 4]; n as usize], meta: vec![[0; 4]; n as usize], tree: vec![[0; 4]; n as usize], info: vec![[0; 4]; n as usize],
        contour: BTreeMap::new(), holes: BTreeMap::new(), levels: BTreeMap::new(), sources: BTreeMap::new() };
    for (i, &(p, id, c, h)) in rings.iter().enumerate() {
        let l = order[i];
        if h == NONE { t.contour.insert((p, id, c), l); } else { t.holes.entry((p, id, c)).or_default().push(l); }
    }
    let first_cell = |lv: &Level, c: u32| lv.comp.iter().position(|&x| x == c).unwrap();
    for (i, &(p, id, c, h)) in rings.iter().enumerate() {
        let l = order[i] as usize;
        let own = t.contour[&(p, id, c)];
        let z = reference.get(&id).map_or(NONE, |&u| {
            let at = first_cell(&levels[&(p, id)], c);
            t.contour[&(p, u, levels[&(p, u)].comp[at])]
        });
        let feature = if id == BOUNDARY && h == NONE {
            let seg = 9000 + t.sources.len() as u32;
            t.sources.insert((p, c), seg);
            if seg % 2 == 0 { seg } else { vertex_feature(seg, VertexSub::Line1) }
        } else { 0 };
        t.loops[l] = [l as u32, 4, 0, 0];
        t.meta[l] = [p, id, NONE, if h == NONE { 0 } else { LOOP_HOLE }];
        t.tree[l] = [if h == NONE { NONE } else { own }, NONE, z, 0];
        t.info[l] = [p, id, feature, 0];
        let lv = &levels[&(p, id)];
        t.keys.insert(l as u32, if h == NONE { (p, id, false, lv.comp_box[c as usize]) } else { (p, id, true, lv.hole_box[h as usize]) });
    }
    // a ring of another stage's level, and an open chain at L_0: both ignored
    let a = t.contour[&(planes.len() as u32 - 1, BOUNDARY, 0)];
    for (k, (id, flags)) in [(72, 0), (s1b::loop_level(0), LOOP_OPEN)].into_iter().enumerate() {
        let l = order[rings.len() + k] as usize;
        t.loops[l] = [l as u32, 4, 0, 0];
        t.meta[l] = [planes.len() as u32 - 1, id, NONE, flags];
        t.tree[l] = [NONE, NONE, a, 0];
        t.info[l] = [0, id, 0, 0];
    }
    t.levels = levels;
    t
}

/// The CPU's rounds over the stand-in's levels: the records S1b should select, by loop id.
fn expected(planes: &[Plane], spec: &S1bSpec, t: &Table) -> Vec<Record> {
    let mut out = vec![];
    let within = |p: u32, id: u32, c: u32, outer_id: u32| -> u32 {
        let lv = &t.levels[&(p, id)];
        let at = lv.comp.iter().position(|&x| x == c).unwrap();
        t.levels[&(p, outer_id)].comp[at]
    };
    fn round(spec: &S1bSpec, t: &Table, within: &dyn Fn(u32, u32, u32, u32) -> u32, p: u32, c: u32, r: u32, link: u32, out: &mut Vec<Record>) {
        let b = s1b::boundary_level(r, BOUNDARY);
        let me = t.contour[&(p, b, c)];
        let some_in = |id: u32| { let lv = &t.levels[&(p, id)]; (0..lv.comps).any(|k| within(p, id, k, b) == c) };
        let wide = r < spec.rounds && some_in(s1b::thick_level(r));
        let source = if r == 0 { t.sources[&(p, c)] } else { NONE };
        let state = if wide { State::Wide } else { State::Handoff };
        out.push(Record { loop_id: me, kind: Kind::Component(state), round: r, plane: p, link, source });
        if !wide {
            for &h in t.holes.get(&(p, b, c)).into_iter().flatten() { out.push(Record { loop_id: h, kind: Kind::Hole, round: r, plane: p, link: me, source: NONE }); }
            return;
        }
        let l = s1b::loop_level(r);
        for k in (0..t.levels[&(p, l)].comps).filter(|&k| within(p, l, k, b) == c) {
            let rings = std::iter::once(t.contour[&(p, l, k)]).chain(t.holes.get(&(p, l, k)).into_iter().flatten().copied());
            for x in rings { out.push(Record { loop_id: x, kind: Kind::Loop, round: r, plane: p, link: me, source: NONE }); }
        }
        let nb = s1b::boundary_level(r + 1, BOUNDARY);
        for k in (0..t.levels[&(p, nb)].comps).filter(|&k| within(p, nb, k, b) == c) { round(spec, t, within, p, k, r + 1, me, out); }
    }
    for p in 0..planes.len() as u32 {
        let lv = &t.levels[&(p, BOUNDARY)];
        for c in 0..lv.comps {
            let me = t.contour[&(p, BOUNDARY, c)];
            let source = t.sources[&(p, c)];
            let deep = !spec.narrow.is_empty() && (0..t.levels[&(p, s1b::NARROW_LEVEL)].comps).any(|k| within(p, s1b::NARROW_LEVEL, k, BOUNDARY) == c);
            let holed = spec.skip_holed && t.holes.contains_key(&(p, BOUNDARY, c));
            if deep || holed {
                let state = if holed { State::Holed } else { State::NotNarrow };
                out.push(Record { loop_id: me, kind: Kind::Component(state), round: 0, plane: p, link: NONE, source });
            } else {
                round(spec, t, &within, p, c, 0, NONE, &mut out);
            }
        }
    }
    out.sort();
    out
}

fn le(words: impl IntoIterator<Item = u32>) -> Vec<u8> { words.into_iter().flat_map(u32::to_le_bytes).collect() }
fn flat(records: &[[u32; 4]]) -> Vec<u8> { le(records.iter().flatten().copied()) }

/// One S1b run over a stand-in table, its loop count given or counted; the s1b_out words.
fn run(cx: &mut fab_sdf_engine::makepad_draw::Cx, kernels: &S1bKernels, scans: &ScanKernels, t: &Table, spec: &S1bSpec, counted: bool) -> Vec<u32> {
    let mut pool = Pool::new(32);
    let put = |pool: &mut Pool, name: &'static str, bytes: Vec<u8>| {
        let buf = pool.buffer(name, bytes.len().max(16) as u64);
        pool.upload(buf, 0, bytes);
        buf
    };
    let n = t.loops.len() as u32;
    let inputs = S1bInputs {
        loops: put(&mut pool, "s1b test loops", flat(&t.loops)),
        loop_meta: put(&mut pool, "s1b test loop meta", flat(&t.meta)),
        loop_tree: put(&mut pool, "s1b test loop tree", flat(&t.tree)),
        piece_info: put(&mut pool, "s1b test piece info", flat(&t.info)),
        depth: None,
    };
    let mut steps = vec![];
    let mut spec = spec.clone();
    if counted {
        // the chain's loops word, as the total of a scan of ones, under a looser bound
        let word = pool.claim("s1b test loops", 1).unwrap();
        let ones = put(&mut pool, "s1b test ones", le(vec![1u32; n as usize]));
        let sink = pool.buffer("s1b test sink", n as u64 * 4);
        steps.extend(scans.exclusive(&mut pool, ones, sink, n, word));
        spec.loops = Size::Counted { word, bound: n + 9 };
    } else {
        spec.loops = Size::Known(n);
    }
    let words = S1bWords::claim(&mut pool).unwrap();
    let (s1b_steps, out) = s1b::steps(kernels, scans, &mut pool, &words, &inputs, &spec).unwrap();
    steps.extend(s1b_steps);
    steps.push(Output::fixed(out.buf, out.bytes));
    assert_ne!(out.buf, BufRef::RUNTIME);
    match rig::run_steps(cx, pool, steps, JobClass::Visible) {
        Finished::Done { output, .. } => rig::words(&output.expect("s1b_out")),
        Finished::Failed { error, .. } => panic!("S1b run (counted {counted}): {error}"),
    }
}

fn make_spec(planes: &[Plane], narrow: bool, skip_holed: bool, rounds: u32) -> S1bSpec {
    let spacing: Vec<f32> = planes.iter().map(|p| p.s0 as f32 / 10.0).collect();
    S1bSpec { narrow: if narrow { vec![2.4; spacing.len()] } else { vec![] }, spacing, skip_holed, rounds, boundary: BOUNDARY, loops: Size::Known(0) }
}

/// The fill order assemble gives, rebuilt from the expected records by the CPU's stack order
/// (a component's loops, then its children), each list in loop order.
fn regions(records: &[Record]) -> Vec<Region> {
    let of = |link: u32, f: &dyn Fn(&Record) -> bool| -> Vec<Record> { records.iter().filter(|r| r.link == link && f(r)).copied().collect() };
    fn fill(c: &Record, of: &dyn Fn(u32, &dyn Fn(&Record) -> bool) -> Vec<Record>, out: &mut Vec<Item>) {
        match c.kind {
            Kind::Component(State::Wide) => {
                out.extend(of(c.loop_id, &|r| r.kind == Kind::Loop).iter().map(|r| Item::Loop { loop_id: r.loop_id, round: r.round }));
                for child in of(c.loop_id, &|r| matches!(r.kind, Kind::Component(_))) { fill(&child, of, out); }
            }
            _ => out.push(Item::Handoff { contour: c.loop_id, holes: of(c.loop_id, &|r| r.kind == Kind::Hole).iter().map(|r| r.loop_id).collect(), round: c.round }),
        }
    }
    records.iter().filter(|r| r.round == 0 && matches!(r.kind, Kind::Component(_))).map(|c| {
        let fill_of = match c.kind {
            Kind::Component(State::NotNarrow) => Fill::NotNarrow,
            Kind::Component(State::Holed) => Fill::Holed,
            _ => { let mut items = vec![]; fill(c, &of, &mut items); Fill::Concentric(items) }
        };
        Region { contour: c.loop_id, plane: c.plane, source: c.source, fill: fill_of }
    }).collect()
}

#[test]
fn rounds_match_the_cpu_recursion_at_three_job_sizes() {
    rig::with_gpu(|cx| {
        let kernels = S1bKernels::new(cx).unwrap();
        let scans = ScanKernels::new(cx).unwrap();
        let planes = main_planes();
        let spec = make_spec(&planes, false, false, 3);
        let t = table(&planes, &spec);
        let want = expected(&planes, &spec, &t);
        // the shapes reach every case: handoffs at rounds 0, 1, 2 and the cap 3, holes, splits
        let count = |f: &dyn Fn(&Record) -> bool| want.iter().filter(|r| f(r)).count();
        assert_eq!(count(&|r| r.kind == Kind::Component(State::Handoff) && r.round == 3), 1);
        assert_eq!(count(&|r| r.kind == Kind::Hole), 2);
        assert_eq!(count(&|r| r.kind == Kind::Loop && r.round == 2), 1);
        let mut first: Option<Vec<u32>> = None;
        for forced in [Some(3), Some(64), None] {
            rig::force_elements(cx, forced);
            let out = run(cx, &kernels, &scans, &t, &spec, false);
            let got = s1b::decode(&out).unwrap();
            assert_eq!(got, want, "records at jobs of {forced:?}");
            assert!(s1b::thickness(&out).unwrap().is_empty(), "brackets without a field");
            match &first {
                Some(f) => assert!(*f == out, "s1b_out differs at jobs of {forced:?}"),
                None => first = Some(out),
            }
        }
        rig::force_elements(cx, Some(5));
        let counted = run(cx, &kernels, &scans, &t, &spec, true);
        rig::force_elements(cx, None);
        assert_eq!(s1b::decode(&counted).unwrap(), want, "counted loops");
        let got = s1b::assemble(&want).unwrap();
        assert_eq!(got, regions(&want));
        assert_eq!(got.len(), 9, "region components: {got:?}");
        // the dumbbell: one loop, then its two halves handed over
        assert!(got.iter().any(|r| matches!(&r.fill, Fill::Concentric(items) if items.len() == 3 && matches!(items[0], Item::Loop { round: 0, .. })
            && items[1..].iter().all(|i| matches!(i, Item::Handoff { round: 1, .. })))), "the dumbbell: {got:?}");

        // the narrow test and the band rule
        for (planes, narrow, skip, state) in [(narrow_planes(), true, false, State::NotNarrow), (band_planes(), false, true, State::Holed)] {
            let rounds = if narrow { planes.iter().map(|p| s1b::rounds_for(p.s0 as f32 / 10.0, 2.4)).max().unwrap() } else { 3 };
            let spec = make_spec(&planes, narrow, skip, rounds);
            let t = table(&planes, &spec);
            let want = expected(&planes, &spec, &t);
            assert_eq!(want.iter().filter(|r| r.kind == Kind::Component(state)).count(), 1, "{want:?}");
            let got = s1b::decode(&run(cx, &kernels, &scans, &t, &spec, false)).unwrap();
            assert_eq!(got, want, "narrow {narrow} skip holed {skip}");
            assert_eq!(s1b::assemble(&got).unwrap(), regions(&want));
        }
    });
}

#[test]
fn levels_reference_and_depth() {
    let planes = [Plane { s0: 6, rects: vec![] }, Plane { s0: 4, rects: vec![] }];
    let mut spec = make_spec(&planes, true, false, 2);
    spec.spacing.push(f32::NAN);
    spec.narrow.push(2.4);
    let lv = spec.levels().unwrap();
    let b = |r| s1b::boundary_level(r, BOUNDARY);
    assert_eq!(lv.ids, vec![BOUNDARY, b(1), b(2), s1b::loop_level(0), s1b::loop_level(1), s1b::thick_level(0), s1b::thick_level(1), s1b::NARROW_LEVEL]);
    let want = [[0.0, -6.0, -12.0, -3.0, -9.0, -7.53, -13.53, -24.0], [0.0, -4.0, -8.0, -2.0, -6.0, -5.03, -9.03, -24.0]];
    for (p, w) in want.iter().enumerate() {
        for (k, &v) in w.iter().enumerate() { assert!((lv.values[p * 8 + k] - v).abs() < 1.0e-4, "plane {p} slot {k}: {}", lv.values[p * 8 + k]); }
    }
    assert!(lv.values[16..].iter().all(|v| v.is_nan()), "an unused plane");
    let reference: BTreeMap<u32, u32> = spec.reference().into_iter().collect();
    assert_eq!(reference.len(), 7);
    assert_eq!((reference[&b(1)], reference[&b(2)], reference[&s1b::loop_level(1)], reference[&s1b::thick_level(0)], reference[&s1b::NARROW_LEVEL]),
        (BOUNDARY, b(1), b(1), BOUNDARY, BOUNDARY));
    // the narrow depth (24) is deepest
    assert!((spec.field_depth() - (24.0 + s1b::DEPTH_MARGIN)).abs() < 1.0e-4, "{}", spec.field_depth());
    spec.narrow.clear();
    spec.spacing.truncate(2);
    // T_1 on the first layer: (1 + 5/4) 0.6 + 0.003 mm
    assert!((spec.field_depth() - (13.53 + s1b::DEPTH_MARGIN)).abs() < 1.0e-4, "{}", spec.field_depth());
    // a narrow region's last round is never wide: s0 = s gives 6 rounds, the first layer's 0.6 mm 4
    assert_eq!((s1b::rounds_for(0.4, 2.4), s1b::rounds_for(0.6, 2.4), s1b::rounds_for(0.1, 20.0)), (6, 4, s1b::ROUNDS_MAX));
    spec.rounds = 0;
    assert!(spec.levels().is_err());
    spec.rounds = 2;
    spec.boundary = s1b::loop_level(3);
    assert!(spec.levels().is_err(), "a boundary id inside S1b's levels");
    // assemble refuses a loop of a handed-over component
    let c = Record { loop_id: 4, kind: Kind::Component(State::Handoff), round: 0, plane: 0, link: NONE, source: 1 };
    let l = Record { loop_id: 5, kind: Kind::Loop, round: 0, plane: 0, link: 4, source: NONE };
    assert!(s1b::assemble(&[c, l]).is_err());
    assert!(s1b::decode(&[3, 2, 1, 0]).is_err(), "more records than the loop bound");
}

/// The real path's shapes sit this far off the integer grid (grid units), so no level runs along fine-grid lines.
const SHIFT: [f64; 2] = [0.37, 0.21];

/// A plane's region outline in grid units, shifted: contours counter-clockwise, holes clockwise, corners only.
fn outline(plane: &Plane) -> Vec<Vec<[f64; 2]>> {
    let d = depth(plane);
    let (w, h) = ((W * 2) as i64, (H * 2) as i64);
    let on = |x: i64, y: i64| x >= 0 && y >= 0 && x < w && y < h && d[(y * w + x) as usize] > 0;
    // the boundary's unit edges with the material on their left, by start vertex
    let mut next: BTreeMap<(i64, i64), (i64, i64)> = BTreeMap::new();
    for y in 0..h {
        for x in 0..w {
            if !on(x, y) { continue; }
            for (out, a, b) in [((x, y - 1), (x, y), (x + 1, y)), ((x + 1, y), (x + 1, y), (x + 1, y + 1)),
                ((x, y + 1), (x + 1, y + 1), (x, y + 1)), ((x - 1, y), (x, y + 1), (x, y))] {
                if !on(out.0, out.1) { assert!(next.insert(a, b).is_none(), "a saddle at {a:?}"); }
            }
        }
    }
    let mut rings = vec![];
    while let Some((&start, _)) = next.iter().next() {
        let mut pts = vec![start];
        let mut at = next.remove(&start).unwrap();
        while at != start { pts.push(at); at = next.remove(&at).unwrap(); }
        let n = pts.len();
        rings.push((0..n).filter(|&i| {
            let (a, b, c) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            (b.0 - a.0) * (c.1 - b.1) != (b.1 - a.1) * (c.0 - b.0)
        }).map(|i| [pts[i].0 as f64 / 2.0 + SHIFT[0], pts[i].1 as f64 / 2.0 + SHIFT[1]]).collect());
    }
    rings
}

fn shoelace(points: &[[f64; 2]]) -> f64 {
    (0..points.len()).map(|i| { let (a, b) = (points[i], points[(i + 1) % points.len()]); a[0] * b[1] - a[1] * b[0] }).sum::<f64>() * 0.5
}

/// A box in grid units, shifted, in half units (exact for these levels to f32 error).
fn half_units(b: [f32; 4]) -> [i64; 4] {
    let shift = [SHIFT[0], SHIFT[1], SHIFT[0], SHIFT[1]];
    let mut out = [0; 4];
    for k in 0..4 {
        let v = (b[k] as f64 - shift[k]) * 2.0;
        assert!((v - v.round()).abs() < 2.0e-3, "a box off the half-unit raster: {b:?}");
        out[k] = v.round() as i64;
    }
    out
}

/// A record with its loops named by key: loop, kind, round, plane, link, and the component its source seg outlines.
type Keyed = (Key, Kind, u32, u32, Option<Key>, Option<Key>);

struct FieldStages { field: FieldKernels, pieces: PieceKernels, chain: ChainKernels, s1b: S1bKernels, scans: ScanKernels, copy: ComputeKernel }

/// The L∞ distance (grid units) from a point in half units to the plane's outside cells: the
/// miter field's depth of a region of right angles.
fn linf(depth: &[u32], x: f64, y: f64) -> f64 {
    let (w, h) = ((W * 2) as i64, (H * 2) as i64);
    let (cx, cy) = (x.floor() as i64, y.floor() as i64);
    let on = |i: i64, j: i64| i >= 0 && j >= 0 && i < w && j < h && depth[(j * w + i) as usize] > 0;
    if !on(cx, cy) { return 0.0; }
    let reach = depth[(cy * w + cx) as usize] as i64 + 1;
    let mut best = f64::MAX;
    for j in cy - reach..=cy + reach {
        for i in cx - reach..=cx + reach {
            if on(i, j) { continue; }
            let gx = (i as f64 - x).max(x - (i + 1) as f64).max(0.0);
            let gy = (j as f64 - y).max(y - (j + 1) as f64).max(0.0);
            best = best.min(gx.max(gy));
        }
    }
    best / 2.0
}

/// The brackets against the raster: each holed handoff before round R whose deepest grid node
/// lies BRACKET_FLOOR above B_r has one, holding twice its deepest point's depth below B_r
/// (the L∞ field's maximum lies on the quarter-unit lattice), within BRACKET_WIDTH; no other
/// component has one.
fn check_brackets(planes: &[Plane], spec: &S1bSpec, t: &Table, got: &BTreeMap<Key, Thickness>) {
    let of_loop: BTreeMap<u32, (u32, u32, u32)> = t.contour.iter().map(|(&k, &l)| (l, k)).collect();
    let mut want = 0;
    for r in expected(planes, spec, t) {
        let key = t.keys[&r.loop_id];
        let (p, id, c) = of_loop.get(&r.loop_id).copied().unwrap_or((0, 0, NONE));
        let holed = t.holes.get(&(p, id, c)).is_some_and(|h| !h.is_empty());
        if r.kind != Kind::Component(State::Handoff) || r.round >= spec.rounds || !holed {
            assert!(!got.contains_key(&key), "a bracket on {r:?}");
            continue;
        }
        let plane = &planes[p as usize];
        let depth = depth(plane);
        let lv = &t.levels[&(p, id)];
        let w = (W * 2) as usize;
        let cells: Vec<usize> = (0..lv.comp.len()).filter(|&k| lv.comp[k] == c).collect();
        let deepest = cells.iter().map(|&k| depth[k]).max().unwrap();
        let mut top = 0.0f64;
        for &k in cells.iter().filter(|&&k| depth[k] + 1 >= deepest) {
            let (x, y) = ((k % w) as f64, (k / w) as f64);
            for a in 0..3 { for b in 0..3 { top = top.max(linf(&depth, x + a as f64 * 0.5, y + b as f64 * 0.5)); } }
        }
        let mut node = 0.0f64;
        for gy in 0..=H {
            for gx in 0..=W {
                let (x, y) = (2.0 * (gx as f64 - SHIFT[0]), 2.0 * (gy as f64 - SHIFT[1]));
                if x < 0.0 || y < 0.0 || x >= w as f64 || y >= (H * 2) as f64 { continue; }
                if lv.comp[y as usize * w + x as usize] == c { node = node.max(linf(&depth, x, y)); }
            }
        }
        let below = r.round as f64 * plane.s0 as f64;
        let floor = s1b::BRACKET_FLOOR as f64;
        if (node - below - floor).abs() < 1.0e-3 {
            want += got.contains_key(&key) as usize;
            continue;
        }
        let Some(b) = got.get(&key) else {
            assert!(node - below < floor, "no bracket on {r:?}: deepest node {node} above B_r {below}");
            continue;
        };
        assert!(node - below > floor, "a bracket on {r:?}: deepest node {node} at most {floor} above B_r {below}");
        let thick = 0.2 * (top - below);
        eprintln!("bracket: plane {p} round {} thickness {thick:.4} mm in {:.4}..{:.4}", r.round, b.lo, b.hi);
        assert!(b.lo as f64 <= thick + 2.0e-5 && thick <= b.hi as f64 + 2.0e-5, "{r:?}: thickness {thick} outside {b:?}");
        assert!(b.hi - b.lo <= s1b::BRACKET_WIDTH + 1.0e-5, "{r:?}: a bracket {b:?} wider than {} mm", s1b::BRACKET_WIDTH);
        want += 1;
    }
    assert_eq!(got.len(), want, "brackets: {got:?}");
}

/// The real path over `fs`: the field, extract at S1b's levels, the chain with S1b's reference
/// table, then S1b, in one item. S1b's records keyed, and the item's submissions and GPU ms.
fn run_field(cx: &mut Cx, k: &FieldStages, fs: &FeatureSet, spec: &S1bSpec) -> (Vec<Keyed>, BTreeMap<Key, Thickness>, ItemStats) {
    let mut pool = Pool::new(32);
    let field_words = feature_field::claim(&mut pool).unwrap();
    let words = wall_pieces::claim(&mut pool).unwrap();
    let chain_words = chain::claim(&mut pool).unwrap();
    let s1b_words = S1bWords::claim(&mut pool).unwrap();
    let input = FieldInput::upload(&mut pool, fs, &fs.loop_bias(0.0, 0.0)).unwrap();
    let params = FieldParams { nx: fs.nx, ny: fs.ny, planes: fs.planes, depth_in: spec.field_depth(), depth_out: s1b::DEPTH_MARGIN,
        join: FieldJoin::Miter(3.0), bias_max: 0.0 };
    let levels = spec.levels().unwrap();
    let seen: Rc<RefCell<Option<(pack::Packed, wall_pieces::PiecesOut, chain::Chain)>>> = Rc::new(RefCell::new(None));
    let (pk, ck, sk, scans, copy, spec, out) = (k.pieces.clone(), k.chain.clone(), k.s1b.clone(), k.scans.clone(), k.copy.clone(), spec.clone(), seen.clone());
    let (nx, ny, planes) = (fs.nx, fs.ny, fs.planes);
    let steps = feature_field::steps(&k.field, &mut pool, field_words, input, params, move |_, pool, field| {
        let (band_slot, field_out) = (field.band_slot, *field);
        wall_pieces::steps(&pk, pool, field, words, &Levels { ids: &levels.ids, values: &levels.values }, move |_, pool, pieces| {
            let input = chain::Pieces { piece: pieces.piece, piece_geo: pieces.piece_geo, piece_key: pieces.piece_key, piece_info: pieces.piece_info,
                piece_off: pieces.piece_off, band_slot, count: pieces.count, capacity: pieces.capacity, levels_per_cell: pieces.levels_per_cell };
            let grid = ChainGrid::new(nx, ny, planes, NONE, &spec.reference());
            let (c, mut steps) = chain::steps(&ck, pool, chain_words, &input, &grid, 2)?;
            let mut spec = spec.clone();
            spec.loops = Size::Counted { word: c.loops_word(), bound: c.loop_capacity };
            let depth = S1bDepth { field: field_out, piece: pieces.piece, piece_off: pieces.piece_off, levels_per_cell: pieces.levels_per_cell,
                piece_loop: c.piece_loop, loop_metric: c.loop_metric, boundary_slot: 0 };
            let inputs = S1bInputs { loops: c.loops, loop_meta: c.loop_meta, loop_tree: c.loop_tree, piece_info: pieces.piece_info, depth: Some(depth) };
            let (s, s1b_out) = s1b::steps(&sk, &scans, pool, &s1b_words, &inputs, &spec)?;
            steps.extend(s);
            let (copies, packed) = pack::steps(&copy, pool, &[("s1b", s1b_out.buf), ("meta", c.loop_meta), ("metric", c.loop_metric)]);
            steps.extend(copies);
            *out.borrow_mut() = Some((packed, *pieces, c));
            Ok(steps)
        })
    }).unwrap();
    let (counts, output, stats) = match rig::run_steps(cx, pool, steps, JobClass::Visible) {
        Finished::Done { counts, output, stats, .. } => (counts, output.expect("the packed readback"), stats),
        Finished::Failed { error, .. } => panic!("the field path: {error}"),
    };
    let (packed, pieces, c) = seen.borrow_mut().take().unwrap();
    pieces.verify(&counts).unwrap();
    for (w, what) in [(chain::WORD_OVERFLOW, "overflow"), (chain::WORD_OPEN_TAILS, "open tails"), (chain::WORD_OPEN_HEADS, "open heads"),
        (chain::WORD_DUPLICATES, "duplicates"), (chain::WORD_DEFECTS, "defects")] {
        assert_eq!(counts.word(c.word(w)), 0, "chain {what}");
    }
    let w = rig::words(&output);
    let meta: Vec<&[u32]> = packed.get(&w, "meta").chunks(4).collect();
    let metric: Vec<[f32; 8]> = packed.get(&w, "metric").chunks(8).map(|m| std::array::from_fn(|i| f32::from_bits(m[i]))).collect();
    let key = |l: u32| -> Key {
        let (m, b) = (meta[l as usize], metric[l as usize]);
        (m[0], m[1], m[3] & LOOP_HOLE != 0, half_units([b[4], b[5], b[6], b[7]]))
    };
    // the outline loop a seg is on: its box at the boundary level
    let seg_key = |seg: u32, plane: u32| -> Key {
        let root = fs.seg_info[seg as usize][0];
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for s in fs.seg.iter().zip(&fs.seg_info).filter(|(_, i)| i[0] == root).map(|(s, _)| s) {
            b = [b[0].min(s[0]).min(s[2]), b[1].min(s[1]).min(s[3]), b[2].max(s[0]).max(s[2]), b[3].max(s[1]).max(s[3])];
        }
        (plane, BOUNDARY, false, half_units(b))
    };
    let records = s1b::decode(packed.get(&w, "s1b")).unwrap();
    let keyed = records.iter().map(|r| (key(r.loop_id), r.kind, r.round, r.plane, (r.link != NONE).then(|| key(r.link)),
        (r.source != NONE).then(|| seg_key(r.source, r.plane)))).collect();
    let brackets = s1b::thickness(packed.get(&w, "s1b")).unwrap().into_iter().map(|(l, b)| (key(l), b)).collect();
    (keyed, brackets, stats)
}

#[test]
fn rounds_on_the_region_field_match_the_raster() {
    let planes = main_planes();
    let spec = make_spec(&planes, false, false, 3);
    let t = table(&planes, &spec);
    let mut want: Vec<Keyed> = expected(&planes, &spec, &t).iter().map(|r| (t.keys[&r.loop_id], r.kind, r.round, r.plane,
        (r.link != NONE).then(|| t.keys[&r.link]), (r.source != NONE).then(|| t.keys[&r.loop_id]))).collect();
    want.sort();
    assert_eq!(want.iter().map(|r| r.0).collect::<BTreeSet<_>>().len(), want.len(), "keys name one ring each");
    let rings: Vec<Vec<Vec<[f64; 2]>>> = planes.iter().map(outline).collect();
    let creases: Vec<Vec<Vec<bool>>> = rings.iter().map(|p| p.iter().map(|r| vec![true; r.len()]).collect()).collect();
    let loops: Vec<Vec<Loop>> = rings.iter().zip(&creases).map(|(p, c)| p.iter().zip(c)
        .map(|(r, c)| Loop { points: r, creases: c, hole: shoelace(r) < 0.0 }).collect()).collect();
    let fs = from_loops((W / 10) as u32, (H / 10) as u32, &loops).unwrap();
    rig::with_gpu(|cx| {
        let k = FieldStages { field: FieldKernels::new(cx).unwrap(), pieces: PieceKernels::new(cx).unwrap(), chain: ChainKernels::new(cx).unwrap(),
            s1b: S1bKernels::new(cx).unwrap(), scans: ScanKernels::new(cx).unwrap(), copy: pack::kernel(cx) };
        let (mut got, brackets, stats) = run_field(cx, &k, &fs, &spec);
        got.sort();
        eprintln!("S1b on the field: {} records, {} brackets, {} submissions ({}), {:.3} GPU ms", got.len(), brackets.len(), stats.submissions, stats.ends, stats.gpu_ms);
        let mut kernels = stats.kernels.clone();
        kernels.sort_by(|a, b| b.2.total_cmp(&a.2));
        for (label, elements, ms, jobs) in kernels {
            eprintln!("  {label}: {ms:.3} ms, {jobs} jobs, {elements} elements");
        }
        assert_eq!(got, want);
        check_brackets(&planes, &spec, &t, &brackets);
    });
}

//! The old runtime's identity round: an outline through the complete readback and loop
//! reconstruction path.
use makepad_csg_sdf::plan::ShellSpec;
use fab_sdf_engine::{
    feature_field::{self, FieldInput, FieldJoin, FieldKernels, FieldParams},
    feature_set::{self, FeatureSet, Loop},
    gpu_jobs::{self, BufRef, Counts, Elements, Finished, JobClass, JobError, Output, Pass, Pool, Step},
    plane_abi::LEVEL_REGION,
    regions_classes::ClassRegion,
    regions_compose::{self as rc, ComposeKernels, RoundWords},
};
use fab_sdf_engine::makepad_draw::*;
use std::{cell::RefCell, rc::Rc};

use makepad_fab_settings::{config::Config, machine::Job};
use makepad_fab_slicer::clip::{difference_ex, union_ex};
use makepad_fab_slicer::sdf::SdfSource;
use makepad_fab_slicer::source::ContourSource;
use makepad_fab_slicer::{ExPolygon, Polygon, Pt, SCALE};
use makepad_fab_toolpath::Params;
use std::collections::HashMap;
use std::sync::Arc;

fn box3(p: [f64; 3], c: [f64; 3], r: [f64; 3]) -> f64 {
    (0..3).map(|i| (p[i] - c[i]).abs() - r[i]).fold(f64::MIN, f64::max)
}

/// A block with a round hole and a top step, a ledge from the step out over a pillar beside it
/// (an internal bridge over the gap above the pillar), on a thin plinth that overhangs one side.
fn stepped(x: f64, y: f64, z: f64) -> f64 {
    let p = [x, y, z];
    let left = box3(p, [7.0, 10.0, 1.5], [7.0, 7.0, 1.5]);
    let right = box3(p, [17.0, 10.0, 1.0], [3.0, 7.0, 1.0]);
    let hole = 2.5 - ((x - 7.0).powi(2) + (y - 10.0).powi(2)).sqrt();
    let ledge = box3(p, [22.0, 10.0, 1.7], [4.0, 2.0, 0.3]);
    let pillar = box3(p, [24.0, 10.0, 0.5], [2.0, 2.0, 0.5]);
    left.min(right).max(hole).min(ledge).min(pillar)
}

fn params(top: &str, bottom: &str) -> Params {
    let mut cfg = Config::new();
    for k in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(k, "zig-zag");
    }
    for (k, v) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "2"), ("top_shell_layers", top), ("bottom_shell_layers", bottom), ("sparse_infill_density", "15%"), ("detect_narrow_internal_solid_infill", "1"), ("detect_floating_vertical_shell", "1")] {
        cfg.set_str(k, v);
    }
    Params::from_job(&Job::new(cfg), 0)
}

const SLIVER: f64 = 1e-3;

fn sym_area(a: &[ExPolygon], b: &[ExPolygon]) -> f64 {
    difference_ex(a, b).iter().chain(difference_ex(b, a).iter()).map(|e| e.area_mm2()).sum()
}

#[path = "../../sdf_engine/tests/support/gpu_rig.rs"]
mod gpu_rig;

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader

    mod.sdf.RegionsGatePack = {
        workgroup_size: 64
        source: shader.buffer(u32)
        packed: shader.buffer_rw(u32)
        dispatch_range: shader.uniform(vec4u)
        main: fn() {
            let i = global_id().x
            if i >= self.dispatch_range.y { return }
            let k = self.dispatch_range.x + i
            self.packed[self.dispatch_range.z + k] = self.source[k]
        }
    }
    mod.sdf
}

struct Kernels {
    field: FieldKernels,
    compose: ComposeKernels,
    pack: ComputeKernel,
}

impl Kernels {
    fn new(cx: &mut Cx) -> Self {
        let pack = cx.with_vm(|vm| {
            script_mod(vm);
            let module = vm.heap().module(id!(sdf));
            let object = vm.value(module, id!(RegionsGatePack)).as_object().unwrap();
            ComputeKernel::new(vm, object, "regions gate pack").unwrap()
        });
        Self {
            field: FieldKernels::new(cx).unwrap(),
            compose: ComposeKernels::new(cx).unwrap(),
            pack,
        }
    }
}

struct Case {
    outline: Vec<Vec<ExPolygon>>,
    spec: ShellSpec,
}

// The same heights and contours as regions_program::check.
fn case(field: fn(f64, f64, f64) -> f64, top: f64, p: &Params) -> Case {
    let n = (top / 0.2).round() as usize;
    let heights: Vec<(f64, f64)> = (0..n).map(|i| ((i + 1) as f64 * 0.2, 0.2)).collect();
    let zs: Vec<f64> = heights.iter().map(|&(z, h)| z - h / 2.0).collect();
    let src = SdfSource::new(Arc::new(move |x, y, z| field(x, y, z)), ([-1.0, -1.0, 0.0], [31.0, 21.0, top]));
    let outline = src.contours(&zs);
    let spec = makepad_fab_slice::field_regions::shell_spec(p, &[0.2]);
    Case { outline, spec }
}

struct Grid {
    origin: [f64; 2],
    params: FieldParams,
}

impl Grid {
    fn new(case: &Case) -> Self {
        // Only length-valued ShellLevels enter the depth, not the area thresholds.
        let level = case.spec.sets.iter().flat_map(|l| [l.face_grow, l.bottom_grow,
            l.shell_open, l.vertical, l.bridge_open, l.bridge_grow, l.under_grow,
            l.band_core, l.adjacent, l.narrow]).map(f32::abs).fold(0.0, f32::max);
        let depth = 10.0 * level + 2.0;
        let margin = (depth as f64 / 10.0).ceil() + 1.0;
        let mut bbox = makepad_fab_slicer::geom::BBox::empty();
        for e in case.outline.iter().flatten() {
            bbox.merge(&makepad_fab_slicer::geom::BBox::of(&e.contour.pts));
        }
        assert!(!bbox.is_empty(), "the part has an outline");
        let origin = [bbox.min.x_mm().floor() - margin, bbox.min.y_mm().floor() - margin];
        Self {
            origin,
            params: FieldParams {
                nx: (bbox.max.x_mm() + margin - origin[0]).ceil() as u32,
                ny: (bbox.max.y_mm() + margin - origin[1]).ceil() as u32,
                planes: case.outline.len() as u32,
                depth_in: depth, depth_out: depth, join: FieldJoin::Miter(3.0), bias_max: 0.0,
            },
        }
    }

    fn set(&self, layers: &[Vec<ExPolygon>]) -> FeatureSet {
        let points: Vec<Vec<_>> = layers.iter().map(|layer| layer.iter().flat_map(|e| {
            std::iter::once((&e.contour, false)).chain(e.holes.iter().map(|h| (h, true)))
        }).map(|(ring, hole)| {
            (ring.pts.iter().map(|p| feature_set::grid_from_mm([p.x_mm(), p.y_mm()], self.origin)).collect::<Vec<_>>(), hole)
        }).collect()).collect();
        let loops: Vec<Vec<_>> = points.iter().map(|layer| layer.iter().map(|(points, hole)| Loop {
            points, creases: &[], hole: *hole,
        }).collect()).collect();
        feature_set::from_loops(self.params.nx, self.params.ny, &loops).unwrap()
    }
}

#[derive(Clone, Copy)]
struct Span { at: usize, len: usize }
impl Span {
    fn read<'a>(&self, words: &'a [u32]) -> &'a [u32] { &words[self.at..self.at + self.len] }
}

struct PackedRegion {
    region: ClassRegion,
    piece: Span,
    key: Span,
    info: Span,
    ooff: Span,
    rinfo: Span,
}

struct Pack {
    parts: Vec<(BufRef, u32)>,
    words: u32,
}
impl Pack {
    fn new() -> Self { Self { parts: Vec::new(), words: 0 } }
    fn add(&mut self, buf: BufRef, len: u32) -> Span {
        let span = Span { at: self.words as usize, len: len as usize };
        self.parts.push((buf, len));
        self.words += len;
        span
    }
    fn region(&mut self, region: ClassRegion) -> PackedRegion {
        let r = region.out;
        PackedRegion {
            region,
            piece: self.add(r.piece, region.capacity * 4),
            key: self.add(r.key, region.capacity * 4),
            info: self.add(r.info, region.capacity * 4),
            ooff: self.add(r.ooff, r.cells * 100 * r.outputs + 1),
            rinfo: self.add(r.rinfo, r.cells * 4),
        }
    }
    fn steps(self, pool: &mut Pool, kernel: &ComputeKernel) -> Vec<Step> {
        let packed = pool.readback("regions gate packed", self.words.max(1) as u64 * 4);
        let mut steps = Vec::new();
        let mut at = 0;
        for (buf, n) in self.parts {
            if n != 0 {
                steps.push(Step::Pass(Pass::new(kernel, gpu_jobs::label("regions gate pack"),
                    Elements::Fixed(n), 1, 1 << 20,
                    |b| if b == id!(source) { buf } else { packed }).zw(at, 0)));
            }
            at += n;
        }
        steps.push(Output::fixed(packed, self.words.max(1) as u64 * 4));
        steps
    }
}

struct Readback {
    counts: Counts,
    words: Vec<u32>,
}
fn finish(cx: &mut Cx, pool: Pool, steps: Vec<Step>) -> Readback {
    match gpu_rig::run_steps(cx, pool, steps, JobClass::Visible) {
        Finished::Failed { error, .. } => panic!("regions GPU failed: {error}"),
        Finished::Done { counts, output, .. } => {
            let words = gpu_rig::words(&output.expect("packed output"));
            Readback { counts, words }
        }
    }
}

fn quad(words: &[u32], i: usize) -> [u32; 4] { words[i * 4..i * 4 + 4].try_into().unwrap() }

// Exact segment predicates on the slicer's integer grid, including non-adjacent touches.
fn crosses(a: Pt, b: Pt, c: Pt, d: Pt) -> bool {
    use makepad_fab_slicer::geom::orient;
    let on = |p: Pt, q: Pt, r: Pt| orient(p, q, r) == 0
        && r.x >= p.x.min(q.x) && r.x <= p.x.max(q.x)
        && r.y >= p.y.min(q.y) && r.y <= p.y.max(q.y);
    let (ab_c, ab_d, cd_a, cd_b) = (orient(a, b, c), orient(a, b, d), orient(c, d, a), orient(c, d, b));
    (ab_c.signum() * ab_d.signum() < 0 && cd_a.signum() * cd_b.signum() < 0)
        || on(a, b, c) || on(a, b, d) || on(c, d, a) || on(c, d, b)
}

fn simple(name: &str, plane: usize, ring: &Polygon) {
    let p = &ring.pts;
    for i in 0..p.len() {
        for j in i + 1..p.len() {
            if j == i + 1 || (i == 0 && j == p.len() - 1) { continue; }
            assert!(!crosses(p[i], p[(i + 1) % p.len()], p[j], p[(j + 1) % p.len()]),
                "{name} layer {plane}: loop crosses itself at edges {i}, {j}");
        }
    }
}

fn decode(name: &str, plane: usize, packed: &PackedRegion, read: &Readback, origin: [f64; 2]) -> Vec<ExPolygon> {
    let r = packed.region;
    assert!(r.output < r.out.outputs, "{name}: invalid output");
    assert_eq!(read.counts.word(r.words.overflow), 0, "{name}: round overflow");
    let cells = read.counts.word(r.words.cells) as usize;
    assert!(cells <= r.out.cells as usize, "{name}: cells exceed capacity");
    let (pieces, keys, info, off, ri) = (packed.piece.read(&read.words), packed.key.read(&read.words),
        packed.info.read(&read.words), packed.ooff.read(&read.words), packed.rinfo.read(&read.words));
    let mut indices = Vec::new();
    for slot in 0..cells {
        if quad(ri, slot)[0] != plane as u32 { continue; }
        for local in 0..100 {
            let at = (slot * 100 + local) * r.out.outputs as usize + r.output as usize;
            assert!(off[at] <= off[at + 1] && off[at + 1] <= r.capacity, "{name} layer {plane}: invalid CSR");
            for i in off[at] as usize..off[at + 1] as usize {
                assert_eq!(quad(info, i)[0], plane as u32, "{name} layer {plane}: piece {i} plane");
                indices.push(i);
            }
        }
    }
    let mut from = HashMap::new();
    let mut to = HashMap::new();
    for &i in &indices {
        let [a, b, c, d] = quad(keys, i);
        assert!(from.insert([a, b], i).is_none(), "{name} layer {plane}: key {:?} has multiple successors", [a, b]);
        assert!(to.insert([c, d], i).is_none(), "{name} layer {plane}: key {:?} has multiple predecessors", [c, d]);
    }
    // the piece of this plane whose start (end) point lies nearest p, for a key with no match
    let nearest = |p: [f32; 2], end: bool| {
        let at = |j: usize| { let q = quad(pieces, j); if end { [f32::from_bits(q[2]), f32::from_bits(q[3])] } else { [f32::from_bits(q[0]), f32::from_bits(q[1])] } };
        let j = indices.iter().copied().min_by(|&x, &y| { let d = |j| { let q = at(j); (q[0] - p[0]).hypot(q[1] - p[1]) }; d(x).total_cmp(&d(y)) });
        j.map(|j| { let q = at(j); format!("piece {j} key {:?} at {q:?}, {:.3e} away", quad(keys, j), (q[0] - p[0]).hypot(q[1] - p[1])) })
    };
    for &i in &indices {
        let [a, b, c, d] = quad(keys, i);
        let q = quad(pieces, i);
        let (s, e) = ([f32::from_bits(q[0]), f32::from_bits(q[1])], [f32::from_bits(q[2]), f32::from_bits(q[3])]);
        assert!(from.contains_key(&[c, d]), "{name} layer {plane}: key {:?} has no successor (piece {i} {s:?} -> {e:?}; nearest start: {:?})", [c, d], nearest(e, false));
        assert!(to.contains_key(&[a, b]), "{name} layer {plane}: key {:?} has no predecessor (piece {i} {s:?} -> {e:?}; nearest end: {:?})", [a, b], nearest(s, true));
        let end = quad(pieces, i);
        let next = quad(pieces, from[&[c, d]]);
        assert_eq!([end[2], end[3]], [next[0], next[1]], "{name} layer {plane}: key {:?} joins different points", [c, d]);
    }
    let mut visited = vec![false; r.capacity as usize];
    let (mut positive, mut negative) = (Vec::new(), Vec::new());
    for start in indices {
        if visited[start] { continue; }
        let mut points = Vec::new();
        let mut i = start;
        loop {
            let key = quad(keys, i);
            assert!(!visited[i], "{name} layer {plane}: key {:?} closes on another loop", &key[..2]);
            visited[i] = true;
            let p = quad(pieces, i);
            points.push(Pt::from_mm(f32::from_bits(p[0]) as f64 / 10.0 + origin[0],
                f32::from_bits(p[1]) as f64 / 10.0 + origin[1]));
            i = from[&[key[2], key[3]]];
            if i == start { break; }
        }
        // Rounding f32 coordinates to slicer integers may merge consecutive points.
        points.dedup();
        if points.len() > 1 && points.first() == points.last() { points.pop(); }
        let mut ring = Polygon::new(points);
        simple(name, plane, &ring);
        let area = ring.area2();
        assert!(area != 0, "{name} layer {plane}: zero-area loop");
        if area > 0 { positive.push(ExPolygon::new(ring)); }
        else { ring.pts.reverse(); negative.push(ExPolygon::new(ring)); }
    }
    difference_ex(&union_ex(&positive), &union_ex(&negative))
}

fn compare(name: &str, plane: usize, got: &[ExPolygon], want: &[ExPolygon]) {
    let perimeter: f64 = want.iter().flat_map(|e| std::iter::once(&e.contour).chain(&e.holes))
        .map(|r| r.perimeter() / SCALE).sum();
    let diff = sym_area(got, want);
    let area = |v: &[ExPolygon]| v.iter().map(ExPolygon::area_mm2).sum::<f64>();
    let big = |v: &[ExPolygon]| v.iter().filter(|e| e.area_mm2() > SLIVER).count();
    let tolerance = 0.01 * perimeter + 1e-4;
    assert!(diff <= tolerance && big(got) == big(want),
        "{name} layer {plane}: sym_area {diff:.6} >? {tolerance:.6} mm²; GPU {:.6} mm², {} pieces over SLIVER ({} total); reference {:.6} mm², {} pieces over SLIVER ({} total)",
        area(got), big(got), got.len(), area(want), big(want), want.len());
}

#[test]
fn identity_round_is_the_outline() {
    let case = case(stepped, 3.4, &params("3", "2"));
    let grid = Grid::new(&case);
    let set = grid.set(&case.outline);
    gpu_rig::with_gpu(|cx| {
        let k = Kernels::new(cx);
        let mut pool = Pool::new(0);
        let input = FieldInput::upload(&mut pool, &set, &vec![0.0; set.loop_hole.len()]).unwrap();
        let layout = Rc::new(RefCell::new(None));
        let (seen, ck, pk) = (layout.clone(), k.compose.clone(), k.pack.clone());
        let (params, cells, segs) = (grid.params, set.set_kept.len() as u32, set.seg.len() as u32);
        let field_words = feature_field::claim(&mut pool).unwrap();
        let words = RoundWords::claim(&mut pool, "identity round", params.planes, 1).unwrap();
        let steps = feature_field::steps(&k.field, &mut pool, field_words, input, params, move |_, pool, field| {
            let i = field.input;
            let round = rc::Round {
                nx: params.nx, ny: params.ny, planes: params.planes, level: LEVEL_REGION,
                terms: vec![rc::Term::set(i.set_slot, rc::TermSegs { off: i.seg_off, seg: i.seg, held: i.segs.capacity() },
                    rc::TermSides { band_slot: field.band_slot, cover: field.cover, node_side: field.node_side }, 0)],
                flags: Vec::new(), tables: rc::Round::uniform(&[rc::table(1, |b| b == 1)], params.planes),
                cells, segs, outputs: 4 * segs,
            };
            let (mut steps, out) = rc::steps(&ck, pool, &round, words).map_err(JobError::Failed)?;
            let mut pack = Pack::new();
            *seen.borrow_mut() = Some(pack.region(ClassRegion { out, output: 0, words, capacity: round.outputs }));
            steps.extend(pack.steps(pool, &pk));
            Ok(steps)
        }).unwrap();
        let read = finish(cx, pool, steps);
        let packed = layout.borrow_mut().take().unwrap();
        for (plane, want) in case.outline.iter().enumerate() {
            let got = decode("identity outline", plane, &packed, &read, grid.origin);
            compare("identity outline", plane, &got, want);
        }
        eprintln!("identity outline: {} layers, {} GPU pieces", case.outline.len(), read.counts.word(packed.region.words.outputs));
    });
}

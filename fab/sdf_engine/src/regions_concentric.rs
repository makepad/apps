//! S1b on the GPU (GPU-EXEC §9 S1b, §10.3 "S1b concentric"): the concentric solid fill of a
//! region's components, with the CPU's concentric_solid semantics, decided over the loops of
//! the region's own field levels (extract, then the chain with its reference table).
//!
//! The CPU fills a component C round by round. A wide C gets the rings of offset(C, -s/2), and
//! each component of offset(C, -s) is filled next; any other C ends with one bead, two beads
//! sharing its width, or a centre line. Offsets compose, so round r's components are the
//! components of the level B_r = -r s0 of the region's field (Miter(3)), and their loops are
//! the level L_r = -(r + 1/2) s0. s0 is the plane's concentric spacing (the first layer's own).
//! The CPU also keeps a wide C's rings to where C holds their two sides a spacing apart, the
//! opening of offset(C, -s) by s/2; these levels do not, so they differ at a wide C's thin necks.
//!
//! The levels, per plane (d' in grid units), and the reference level each resolves against in
//! the chain (a loop's `loop_tree.z` is the innermost contour of that level holding it):
//! - B_0 = 0: the region's own boundary (the caller's boundary slot); no reference;
//! - B_r = -r s0 for r = 1..=R: reference B_{r-1};
//! - L_r = -(r + 1/2) s0 for r < R: reference B_r;
//! - T_r = -((r + 5/4) s0 + [`THICK_MARGIN`]) for r < R: reference B_r (the wide test);
//! - N = -(the narrow depth) when the run has the narrow test: reference B_0.
//!
//! Wide. On the CPU a component is wide when short >= 2.5 s. short is the least hull width
//! across C's edge directions. With holes it is the least of that and the thickness t, the
//! deepest inset that leaves anything (bisected to 0.005 mm from below). A T_r loop inside C
//! proves t >= 2.5 s0 + 0.006 mm, which has two consequences:
//! - the CPU's bisected thickness is past 2.5 s0;
//! - so is the hull width: it is at least twice the Euclidean inradius, and the miter inset
//!   never exceeds that.
//! So every component the GPU calls wide is wide on the CPU. Every other active component goes
//! whole to the CPU's own concentric_solid (a handoff): the terminal components (short <
//! 2.5 s0), the rare thin but hull-wide ones, and the components past round R. The CPU then
//! decides as it always does, so the loop counts are the CPU's by construction. Handoffs run
//! on the CPU pool beside the GPU batch.
//!
//! A B_r contour is active when every component above it is wide (its reference chain up to
//! B_0). Round 0 also applies the run's rules:
//! - with the narrow test, a component holding an N loop is not narrow (the CPU fills it with
//!   lines);
//! - with holed components skipped (floating-shell bands), a component with a hole stays on
//!   the CPU's Round loop.
//!
//! Thickness. The CPU bisects a holed handoff's thickness t on its 0.005 mm lattice: about
//! 13 insets of the core each. Given the region's field, S1b brackets t to about 0.01 mm, so
//! the CPU searches the lattice only there. t / 2 is the deepest point of C less r s0 (d' is
//! the miter field, whose insets compose). A band node lies in the component of B_r its way to
//! its nearest feature's foot first crosses, so the first B_r piece on that way labels it
//! (r is floor(d / s0) or one less: a handoff has no T_r ring, so it is under (r + 5/4) s0).
//! The deepest point lies within √2/2 of a node at most √2/2 below the deepest node, so
//! sampling those nodes' squares at 1/16 finds it within 1/16 √2/2. A sample counts for C when the segment from its node
//! stays above B_r (d' is 1-Lipschitz); that holds for the samples nearest the deepest point
//! once the deepest node lies [`BRACKET_FLOOR`] above B_r, and C gets no bracket otherwise.
//!
//! Passes (elements: the loop bound, the band slots and coarse cells, or the band nodes):
//! 1. `S1bClear`: the per-loop marks;
//! 2. `S1bMark`: each T loop marks its component wide, each N loop marks it deep, and each
//!    B_r hole marks its contour holed (atomic adds, so the order does not matter);
//! 3. `S1bState`: each loop's record and whether it is selected, by a walk up its reference
//!    chain (at most R links);
//! 4. with the field, the thickness passes on `s1b_work`: `S1bDepthPrep` (band slots, coarse
//!    cells), `S1bDepthLoops` (each loop's holed handoff; the coarse cells of their boxes),
//!    `S1bDepthNodes` (the deepest labelled node per holed handoff), `S1bDepthBound` (each
//!    box's refine threshold), `S1bDepthRefine` at 1/16 (atomic maxima of positive f32 bits,
//!    so the order does not matter);
//! 5. the exclusive scan of the selection;
//! 6. `S1bFill`: the selected records in loop order, their brackets, and the header.
//!
//! `s1b_out` (u32 words) starts with [`OUT_HEADER`] words: records, loop bound, rounds, flags.
//! Then come 4 words per record: loop id, `kind | state << 2 | round << 8 | plane << 16`, link,
//! source; then from [`OUT_HEADER`] + 4 · loop bound, 2 words per record: a holed handoff's
//! thickness bracket (lo, hi: f32 mm), (0, 0) for any other record. The records are:
//! - a component (kind 0): an active B_r contour, with its state (wide, handoff, not narrow,
//!   holed). Its link is the component above it (NONE at round 0). At round 0 its source is
//!   the FeatureSet seg of its root piece, which names the region component it came from;
//! - a loop (kind 1): an L_r ring (contour or hole) of a wide component; its link is that
//!   component;
//! - a hole (kind 2): a B_r hole of a handoff component; its link is that component's contour.
//!
//! Ring points come from the rings stage by loop id. Like the CPU, the consumer drops a ring
//! with fewer than 3 points.
use crate::feature_field::FieldOut;
use crate::gpu_jobs::{label, BufRef, CountWord, Elements, JobError, Pass, Pool, ScanKernels, Step};
use crate::plane_abi::{concentric_level, LEVEL_CONCENTRIC, LEVEL_UNASSIGNED, NONE};
use crate::regions_overhang::Size;
use makepad_draw::*;
use std::collections::BTreeMap;

/// Rounds a run may decide on the GPU (3 R + 2 level slots, within extract's 64).
pub const ROUNDS_MAX: u32 = 20;
/// L_r = LOOP_BASE + r (concentric_level(r)), B_r = BOUNDARY_BASE + r - 1 for r >= 1,
/// T_r = THICK_BASE + r, and N.
pub const LOOP_BASE: u32 = LEVEL_CONCENTRIC;
pub const BOUNDARY_BASE: u32 = LEVEL_CONCENTRIC + ROUNDS_MAX;
pub const THICK_BASE: u32 = LEVEL_CONCENTRIC + 2 * ROUNDS_MAX;
pub const NARROW_LEVEL: u32 = LEVEL_CONCENTRIC + 3 * ROUNDS_MAX;
const _: () = assert!(NARROW_LEVEL < LEVEL_UNASSIGNED);
/// mm added to the wide test's half thickness: past the CPU's bisection step (0.005 mm on the
/// thickness) and the field's error against the CPU's offsets.
pub const THICK_MARGIN: f32 = 0.003;
/// Grid units the field reaches past the deepest level.
pub const DEPTH_MARGIN: f32 = 1.5;
/// Words of the `s1b` parameter header.
pub const HEADER: u32 = 12;
/// Words of the `s1b_out` header.
pub const OUT_HEADER: u32 = 4;
/// Grid units per mm.
const GRID: f32 = 10.0;
const FLAG_NARROW: u32 = 1;
const FLAG_SKIP_HOLED: u32 = 2;

/// The kernels in pass order.
pub const KERNELS: [&str; 9] = ["S1bClear", "S1bMark", "S1bState", "S1bDepthPrep", "S1bDepthLoops", "S1bDepthNodes", "S1bDepthBound",
    "S1bDepthRefine", "S1bFill"];
/// Grid units a holed handoff's deepest labelled node must lie above B_r for its bracket: the
/// 1/16 sample nearest the deepest point is then joined to its node above B_r
/// ((1 + 1/16) √2/2 = 0.7513).
pub const BRACKET_FLOOR: f32 = 0.8;
/// A thickness bracket's width (mm): 0.2 (1/16 √2/2 + 2 0.002), S1bFill's bound on the 1/16
/// sampling and its margin either side.
pub const BRACKET_WIDTH: f32 = 0.009639;
/// Words of the `s1b_work` header.
const WORK_HEADER: u32 = 16;

/// Loop `r` (0-based): the level -(r + 1/2) s0.
pub fn loop_level(r: u32) -> u32 {
    debug_assert!(r < ROUNDS_MAX);
    concentric_level(r)
}
/// Round `r`'s component boundary -r s0 (`boundary` for r = 0: the region's own).
pub fn boundary_level(r: u32, boundary: u32) -> u32 {
    debug_assert!(r <= ROUNDS_MAX);
    if r == 0 { boundary } else { BOUNDARY_BASE + r - 1 }
}
/// Round `r`'s wide test -((r + 5/4) s0 + THICK_MARGIN).
pub fn thick_level(r: u32) -> u32 {
    debug_assert!(r < ROUNDS_MAX);
    THICK_BASE + r
}

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader

    // s1b[0..12]: loops (known), the loops' count word (NONE: known), the loop bound, the
    // boundary level, rounds R, flags (1 the narrow test, 2 holed components skipped), the
    // selection's total word, the loop, boundary (r = 1) and thick level bases, the narrow
    // level, 0.
    mod.sdf.S1b = {
        workgroup_size: 64
        s1b: shader.buffer(u32)
        loops: shader.buffer(vec4u)
        loop_meta: shader.buffer(vec4u)
        loop_tree: shader.buffer(vec4u)
        piece_info: shader.buffer(vec4u)
        s1b_mark: shader.buffer(u32)
        s1b_state: shader.buffer(vec4u)
        s1b_count: shader.buffer(u32)
        s1b_scan: shader.buffer(u32)
        s1b_out: shader.buffer(u32)
        s1b_work: shader.buffer(u32)
        loop_metric: shader.buffer(vec4f)
        counts: shader.buffer(u32)
        dispatch_range: shader.uniform(vec4u)

        index: fn() -> u32 {
            let i = global_id().x
            if i >= self.dispatch_range.y { return 4294967295u }
            return self.dispatch_range.x + i
        }
        bound: fn() -> u32 { return self.s1b[2u] }
        rounds: fn() -> u32 { return self.s1b[4u] }
        n_loops: fn() -> u32 {
            let w = self.s1b[1u]
            if w != 4294967295u { return min(self.counts[w], self.bound()) }
            return min(self.s1b[0u], self.bound())
        }
        // A level's role and round: x = 0 a boundary B_r, 1 a loop L_r, 2 a wide test T_r,
        // 3 the narrow test, 4 none of S1b's.
        role: fn(level: u32) -> vec2u {
            let r = self.rounds()
            if level == self.s1b[3u] { return vec2u(0u, 0u) }
            if level >= self.s1b[7u] && level < self.s1b[7u] + r { return vec2u(1u, level - self.s1b[7u]) }
            if level >= self.s1b[8u] && level < self.s1b[8u] + r { return vec2u(0u, level - self.s1b[8u] + 1u) }
            if level >= self.s1b[9u] && level < self.s1b[9u] + r { return vec2u(2u, level - self.s1b[9u]) }
            if level == self.s1b[10u] { return vec2u(3u, 0u) }
            return vec2u(4u, 0u)
        }
        mark: fn(c: u32, k: u32) -> u32 { return self.s1b_mark[c * 3u + k] }
        wk: fn(k: u32) -> u32 { return self.s1b_work[k] }
        s0_of: fn(p: u32) -> f32 { return asfloat(self.s1b_work[self.wk(5u) + p]) }
        // B_r contour c: 0 wide, 1 handoff, 2 not narrow, 3 holed (both at round 0 only), or 4
        // inactive (a component above it is not wide, or round 0 is skipped).
        state: fn(c: u32, r: u32) -> u32 {
            let n = self.bound()
            if c >= n { return 4u }
            var x = c
            var i = 0u
            while i < r {
                x = self.loop_tree[x].z
                if x >= n { return 4u }
                if self.mark(x, 0u) == 0u { return 4u }
                i += 1u
            }
            let flags = self.s1b[5u]
            var skip = 4u
            if (flags & 1u) != 0u && self.mark(x, 1u) != 0u { skip = 2u }
            if (flags & 2u) != 0u && self.mark(x, 2u) != 0u { skip = 3u }
            if skip != 4u {
                if r == 0u { return skip }
                return 4u
            }
            if r < self.rounds() && self.mark(c, 0u) != 0u { return 0u }
            return 1u
        }
        // The holed handoff before round R that B_r loop l bounds (l itself, or a hole's
        // contour), or NONE.
        depth_target: fn(l: u32) -> u32 {
            let meta = self.loop_meta[l]
            if (meta.w & 2u) != 0u { return 4294967295u }
            let role = self.role(meta.y)
            if role.x != 0u || role.y >= self.rounds() { return 4294967295u }
            var c = l
            if (meta.w & 1u) != 0u { c = self.loop_tree[l].x }
            if c >= self.bound() { return 4294967295u }
            if self.mark(c, 2u) == 0u || self.state(c, role.y) != 1u { return 4294967295u }
            return c
        }
        // Contour l's coarse cells (its box a grid unit wider): (x0, y0, x1, y1).
        box_cells: fn(l: u32) -> vec4u {
            let b = self.loop_metric[l * 2u + 1u]
            let nx = self.wk(13u)
            let ny = self.wk(14u)
            let x0 = min(u32(max(floor((b.x - 1.0) * 0.1), 0.0)), nx - 1u)
            let y0 = min(u32(max(floor((b.y - 1.0) * 0.1), 0.0)), ny - 1u)
            let x1 = min(u32(max(floor((b.z + 1.0) * 0.1), 0.0)), nx - 1u)
            let y1 = min(u32(max(floor((b.w + 1.0) * 0.1), 0.0)), ny - 1u)
            return vec4u(x0, y0, x1, y1)
        }
        cell_word: fn(p: u32, cx: u32, cy: u32) -> u32 {
            return self.wk(9u) + ((p * self.wk(14u) + cy) * self.wk(13u) + cx) * 2u
        }
    }

    mod.sdf.S1bClear = {
        ..mod.sdf.S1b
        s1b_mark: shader.buffer_rw(u32)
        main: fn() {
            let l = self.index()
            if l == 4294967295u { return }
            self.s1b_mark[l * 3u] = 0u
            self.s1b_mark[l * 3u + 1u] = 0u
            self.s1b_mark[l * 3u + 2u] = 0u
        }
    }

    mod.sdf.S1bMark = {
        ..mod.sdf.S1b
        s1b_mark: shader.buffer_rw(u32)
        main: fn() {
            let l = self.index()
            if l == 4294967295u { return }
            if l >= self.n_loops() { return }
            let meta = self.loop_meta[l]
            if (meta.w & 2u) != 0u { return }
            let role = self.role(meta.y)
            let tree = self.loop_tree[l]
            let n = self.bound()
            if role.x == 2u && tree.z < n { atomic_add(self.s1b_mark[tree.z * 3u], 1u) }
            if role.x == 3u && tree.z < n { atomic_add(self.s1b_mark[tree.z * 3u + 1u], 1u) }
            if role.x == 0u && (meta.w & 1u) != 0u && tree.x < n { atomic_add(self.s1b_mark[tree.x * 3u + 2u], 1u) }
        }
    }

    mod.sdf.S1bState = {
        ..mod.sdf.S1b
        s1b_state: shader.buffer_rw(vec4u)
        s1b_count: shader.buffer_rw(u32)
        main: fn() {
            let l = self.index()
            if l == 4294967295u { return }
            var sel = 0u
            var word = 0u
            var link = 4294967295u
            var source = 4294967295u
            if l < self.n_loops() {
                let meta = self.loop_meta[l]
                let role = self.role(meta.y)
                let tree = self.loop_tree[l]
                let r = role.y
                if (meta.w & 2u) == 0u {
                    if role.x == 0u {
                        if (meta.w & 1u) == 0u {
                            let st = self.state(l, r)
                            if st != 4u {
                                sel = 1u
                                word = st << 2u
                                if r > 0u { link = tree.z }
                                else { source = self.piece_info[self.loops[l].x].z & 536870911u }
                            }
                        } else {
                            if self.state(tree.x, r) == 1u {
                                sel = 1u
                                word = 2u
                                link = tree.x
                            }
                        }
                    }
                    if role.x == 1u {
                        if self.state(tree.z, r) == 0u {
                            sel = 1u
                            word = 1u
                            link = tree.z
                        }
                    }
                }
                word = word | (r << 8u) | (meta.x << 16u)
            }
            self.s1b_state[l] = vec4u(word, link, source, 0u)
            self.s1b_count[l] = sel
        }
    }

    mod.sdf.S1bFill = {
        ..mod.sdf.S1b
        s1b_out: shader.buffer_rw(u32)
        main: fn() {
            let l = self.index()
            if l == 4294967295u { return }
            if l == 0u {
                self.s1b_out[0u] = self.counts[self.s1b[6u]]
                self.s1b_out[1u] = self.bound()
                self.s1b_out[2u] = self.rounds()
                self.s1b_out[3u] = self.s1b[5u]
            }
            if self.s1b_count[l] == 0u { return }
            let st = self.s1b_state[l]
            let at = 4u + self.s1b_scan[l] * 4u
            self.s1b_out[at] = l
            self.s1b_out[at + 1u] = st.x
            self.s1b_out[at + 2u] = st.y
            self.s1b_out[at + 3u] = st.z
            // a holed handoff's thickness bracket (mm), (0, 0) where there is none
            var lo = 0u
            var hi = 0u
            if self.wk(12u) != 0u && (st.x & 15u) == 4u {
                if self.wk(self.wk(7u) + l) == l {
                    let base = self.wk(8u) + l * 2u
                    let top = asfloat(self.wk(base))
                    let m = asfloat(self.wk(base + 1u))
                    let below = f32((st.x >> 8u) & 255u) * self.s0_of(st.x >> 16u)
                    if top - below >= asfloat(self.wk(11u)) && m >= top {
                        lo = asuint(0.2 * (m - 0.002 - below))
                        hi = asuint(0.2 * (m + 0.044194 + 0.002 - below))
                    }
                }
            }
            let bat = 4u + self.bound() * 4u + self.s1b_scan[l] * 2u
            self.s1b_out[bat] = lo
            self.s1b_out[bat + 1u] = hi
        }
    }

    // The thickness passes over the region's field and its B_r pieces (s1b_work, see
    // [`work_header`]): each band node above B_r + V labels the holed handoff it lies in by the
    // first B_r piece on its way to its nearest feature's foot (the first crossing of a level
    // from above lies on the boundary of the point's own component).
    mod.sdf.S1bField = {
        ..mod.sdf.FeatureField
        s1b_work: shader.buffer_rw(u32)
        piece: shader.buffer(vec4f)
        piece_off: shader.buffer(u32)
        piece_loop: shader.buffer(u32)
        wk: fn(k: u32) -> u32 { return self.s1b_work[k] }
        s0_of: fn(p: u32) -> f32 { return asfloat(self.s1b_work[self.wk(5u) + p]) }
        fine_x: fn(x: f32) -> u32 { return min(u32(max(floor(x), 0.0)), self.nodes_x() - 2u) }
        fine_y: fn(y: f32) -> u32 { return min(u32(max(floor(y), 0.0)), self.nodes_y() - 2u) }
        slot_at: fn(p: u32, fx: u32, fy: u32) -> u32 {
            return self.band_slot[p * self.cells() + (fy / 10u) * self.field[0u].x + fx / 10u]
        }
        // Band node i: (plane, gx | gy << 16, its coarse cell's work word), plane NONE when
        // no holed handoff's box holds that cell.
        node_at: fn(i: u32) -> vec3u {
            let at = self.wk(6u) + (i / 121u) * 2u
            let coarse = self.s1b_work[at]
            let p = self.s1b_work[at + 1u]
            let cw = self.wk(9u) + (p * self.cells() + coarse) * 2u
            if self.s1b_work[cw] == 0u { return vec3u(4294967295u, 0u, 0u) }
            let nx = self.field[0u].x
            let local = i % 121u
            let gx = (coarse % nx) * 10u + local % 11u
            let gy = (coarse / nx) * 10u + local / 11u
            return vec3u(p, gx | (gy << 16u), cw)
        }
        // The target of the loop of the first piece in piece slot ls that the segment from q
        // to b (run on 1 % past b) crosses, or NONE.
        crossed: fn(p: u32, q: vec2, b: vec2, ls: u32) -> u32 {
            let d = (b - q) * 1.01
            if length(d) < 0.00001 { return 4294967295u }
            var kx = 0.0
            var ky = 0.0
            var sx = 0.0
            var sy = 0.0
            if d.x > 0.0 { kx = floor(q.x) + 1.0; sx = 1.0 } else if d.x < 0.0 { kx = ceil(q.x) - 1.0; sx = 0.0 - 1.0 }
            if d.y > 0.0 { ky = floor(q.y) + 1.0; sy = 1.0 } else if d.y < 0.0 { ky = ceil(q.y) - 1.0; sy = 0.0 - 1.0 }
            let e = q + d
            let steps = u32(abs(floor(e.x) - floor(q.x)) + abs(floor(e.y) - floor(q.y))) + 3u
            let per = self.wk(3u)
            var t = 0.0
            for step in 0..steps {
                if t >= 1.0 { break }
                var tx = 2.0
                if sx != 0.0 { tx = (kx - q.x) / d.x }
                var ty = 2.0
                if sy != 0.0 { ty = (ky - q.y) / d.y }
                let tn = min(min(tx, ty), 1.0)
                if tn > t {
                    let m = q + d * (0.5 * (t + tn))
                    let fx = self.fine_x(m.x)
                    let fy = self.fine_y(m.y)
                    let slot = self.slot_at(p, fx, fy)
                    if slot != 4294967295u {
                        let at = (slot * 100u + (fy % 10u) * 10u + fx % 10u) * per + ls
                        var best = 2.0
                        var hit = 4294967295u
                        for k in self.piece_off[at]..self.piece_off[at + 1u] {
                            let g = self.piece[k]
                            let pe = g.zw - g.xy
                            let den = d.x * pe.y - d.y * pe.x
                            if den == 0.0 { continue }
                            let w = g.xy - q
                            let u = (w.x * pe.y - w.y * pe.x) / den
                            let v = (w.x * d.y - w.y * d.x) / den
                            if v < 0.0 - 0.000001 || v > 1.000001 || u < t - 0.000001 || u > tn + 0.000001 { continue }
                            if u < best { best = u; hit = k }
                        }
                        if hit != 4294967295u {
                            let l = self.piece_loop[hit]
                            if l == 4294967295u { return 4294967295u }
                            return self.s1b_work[self.wk(7u) + l]
                        }
                    }
                }
                if tx <= tn { kx += sx }
                if ty <= tn { ky += sy }
                t = tn
            }
            return 4294967295u
        }
        // Node i's holed handoff: (C, round, plane, gx | gy << 16), C NONE when it has none or
        // the node lies less than V above B_r.
        node_label: fn(i: u32) -> vec4u {
            let none = vec4u(4294967295u, 0u, 0u, 0u)
            let at = self.node_at(i)
            if at.x == 4294967295u { return none }
            let f = self.nf[i * 2u]
            let v = asfloat(self.nf[i * 2u + 1u])
            if f == 4294967295u || !(v < 0.0) { return none }
            let s0 = self.s0_of(at.x)
            if !(s0 > 0.0) { return none }
            let dep = 0.0 - v
            let q = vec2(f32(at.y & 65535u), f32(at.y >> 16u))
            // the nearest feature's foot, on the boundary: the way there crosses B_r
            let s = f & 2147483647u
            var foot = self.seg_a(s)
            if (f & 2147483648u) == 0u { foot = foot + self.seg_e(s) * min(max(dot(q - foot, self.seg_e(s)), 0.0), self.seg_len(s)) }
            let r1 = floor(dep / s0)
            for k in 0..2u {
                let rf = r1 - f32(k)
                if rf < 0.0 || rf >= f32(self.wk(0u)) { continue }
                if dep - rf * s0 < asfloat(self.wk(11u)) { continue }
                let r = u32(rf)
                let c = self.crossed(at.x, q, foot, self.wk(10u) + r)
                if c != 4294967295u { return vec4u(c, r, at.x, at.y) }
            }
            return none
        }
        // The depth at q from its fine cell's cand list (q inside the region), or -1 off the band.
        depth_at: fn(p: u32, q: vec2) -> f32 {
            let fx = self.fine_x(q.x)
            let fy = self.fine_y(q.y)
            let slot = self.slot_at(p, fx, fy)
            if slot == 4294967295u { return 0.0 - 1.0 }
            let cell = slot * 100u + (fy % 10u) * 10u + fx % 10u
            var best = 1.0e30
            for k in self.cand_off[cell]..self.cand_off[cell + 1u] {
                let f = self.cand[k]
                best = min(best, self.value(f & 2147483647u, (f & 2147483648u) != 0u, q, true))
            }
            if best > 1.0e29 { return 0.0 - 1.0 }
            return best
        }
    }

    // Per band slot its (coarse cell, plane); per coarse cell no mark and no threshold.
    mod.sdf.S1bDepthPrep = {
        ..mod.sdf.S1bField
        main: fn() {
            let i = self.index()
            if i == 4294967295u { return }
            let slots = self.wk(2u)
            if i < slots {
                let at = self.wk(6u) + i * 2u
                self.s1b_work[at] = self.band[i]
                self.s1b_work[at + 1u] = self.band_plane(i)
                return
            }
            let c = i - slots
            if c >= self.wk(1u) * self.cells() { return }
            self.s1b_work[self.wk(9u) + c * 2u] = 0u
            self.s1b_work[self.wk(9u) + c * 2u + 1u] = 4294967295u
        }
    }

    // Per loop its target and cleared depth words; a holed handoff's contour marks its box's
    // coarse cells.
    mod.sdf.S1bDepthLoops = {
        ..mod.sdf.S1b
        s1b_work: shader.buffer_rw(u32)
        main: fn() {
            let l = self.index()
            if l == 4294967295u { return }
            let base = self.wk(8u) + l * 2u
            self.s1b_work[base] = 0u
            self.s1b_work[base + 1u] = 0u
            var c = 4294967295u
            if l < self.n_loops() { c = self.depth_target(l) }
            self.s1b_work[self.wk(7u) + l] = c
            if c != l { return }
            let b = self.box_cells(l)
            let p = self.loop_meta[l].x
            for cy in b.y..(b.w + 1u) {
                for cx in b.x..(b.z + 1u) { atomic_max(self.s1b_work[self.cell_word(p, cx, cy)], 1u) }
            }
        }
    }

    // The deepest labelled node per holed handoff (depth word 0).
    mod.sdf.S1bDepthNodes = {
        ..mod.sdf.S1bField
        main: fn() {
            let i = self.index()
            if i == 4294967295u { return }
            let lab = self.node_label(i)
            if lab.x != 4294967295u { atomic_max(self.s1b_work[self.wk(8u) + lab.x * 2u], asuint(0.0 - asfloat(self.nf[i * 2u + 1u]))) }
        }
    }

    // A valid holed handoff's refine threshold (its deepest node less half a cell's diagonal)
    // into its box's coarse cells.
    mod.sdf.S1bDepthBound = {
        ..mod.sdf.S1b
        s1b_work: shader.buffer_rw(u32)
        main: fn() {
            let l = self.index()
            if l == 4294967295u || l >= self.n_loops() { return }
            if self.wk(self.wk(7u) + l) != l { return }
            let meta = self.loop_meta[l]
            let below = f32(self.role(meta.y).y) * self.s0_of(meta.x)
            let top = asfloat(self.wk(self.wk(8u) + l * 2u))
            if !(top - below >= asfloat(self.wk(11u))) { return }
            let key = asuint(top - 0.7072)
            let b = self.box_cells(l)
            for cy in b.y..(b.w + 1u) {
                for cx in b.x..(b.z + 1u) { atomic_min(self.s1b_work[self.cell_word(meta.x, cx, cy) + 1u], key) }
            }
        }
    }

    // The most of d' over the 1/16 grid of each near-top node's square (depth word 1). A sample
    // counts when the segment from its node stays above B_r.
    mod.sdf.S1bDepthRefine = {
        ..mod.sdf.S1bField
        main: fn() {
            let i = self.index()
            if i == 4294967295u { return }
            let at = self.node_at(i)
            if at.x == 4294967295u { return }
            let v = asfloat(self.nf[i * 2u + 1u])
            if !(v < 0.0) { return }
            let dep = 0.0 - v
            let gate = self.s1b_work[at.z + 1u]
            if gate == 4294967295u || asuint(dep) < gate { return }
            let lab = self.node_label(i)
            if lab.x == 4294967295u { return }
            let base = self.wk(8u) + lab.x * 2u
            let top = asfloat(self.s1b_work[base])
            let below = f32(lab.y) * self.s0_of(lab.z)
            if dep < top - 0.7072 || top - below < asfloat(self.wk(11u)) { return }
            let n = vec2(f32(lab.w & 65535u), f32(lab.w >> 16u))
            for j in 0..17u {
                for k in 0..17u {
                    let q = n + vec2(f32(k) * 0.0625 - 0.5, f32(j) * 0.0625 - 0.5)
                    let dq = self.depth_at(lab.z, q)
                    if dq > 0.0 && dep + dq - length(q - n) > 2.0 * below { atomic_max(self.s1b_work[base + 1u], asuint(dq)) }
                }
            }
        }
    }

    mod.sdf
}

/// S1b's kernels, made once per Cx (after gpu_jobs' modules are registered).
#[derive(Clone)]
pub struct S1bKernels { kernels: Vec<ComputeKernel> }
impl S1bKernels {
    pub fn new(cx: &mut Cx) -> Result<Self, String> {
        cx.with_vm(|vm| {
            let module = vm.heap().module(id!(sdf));
            let kernels = KERNELS.iter().map(|name| {
                let object = vm.value(module, LiveId::from_str(name)).as_object().ok_or_else(|| format!("{name} kernel missing"))?;
                ComputeKernel::new(vm, object, name)
            }).collect::<Result<Vec<_>, String>>()?;
            Ok(Self { kernels })
        })
    }
    fn get(&self, name: &str) -> &ComputeKernel { &self.kernels[KERNELS.iter().position(|k| *k == name).unwrap()] }
}

/// What S1b decides over, and the run's rules.
#[derive(Clone, Debug)]
pub struct S1bSpec {
    /// Per plane: s0 in mm (spacing(w_first, h) on a first layer, else spacing(w_solid, h));
    /// NaN where the plane has no region of this run.
    pub spacing: Vec<f32>,
    /// Per plane: the narrow test's depth in mm (6 spacing(w_solid, h)): a component holding
    /// that level is not narrow. Empty: no test (vertical shells, bands).
    pub narrow: Vec<f32>,
    /// Round-0 components with a hole stay on the CPU (floating-shell bands: the Round loop).
    pub skip_holed: bool,
    /// R: the rounds decided here. A component wide at round R - 1 hands its children over.
    pub rounds: u32,
    /// The level id of the region's own boundary pieces (extract's d' = 0 slot).
    pub boundary: u32,
    /// The chain's loops.
    pub loops: Size,
}

/// The rounds after which no component of a region narrower than `narrow` is wide (it would
/// need depth (R - 1 + 5/4) s0 < narrow): ceil(narrow / s0 - 1/4), at least 1.
pub fn rounds_for(spacing: f32, narrow: f32) -> u32 {
    ((narrow / spacing - 0.25).ceil().max(1.0) as u32).min(ROUNDS_MAX)
}

/// Extract's level slots for a run: ids, and values per plane (planes x slots, plane-major,
/// d' grid units; NaN where a plane has no region). The boundary slot (value 0) comes first.
#[derive(Clone, Debug, PartialEq)]
pub struct S1bLevels { pub ids: Vec<u32>, pub values: Vec<f32> }

impl S1bSpec {
    fn check(&self) -> Result<(), String> {
        if self.rounds == 0 || self.rounds > ROUNDS_MAX { return Err(format!("{} rounds (1..={ROUNDS_MAX})", self.rounds)); }
        if !self.narrow.is_empty() && self.narrow.len() != self.spacing.len() { return Err(format!("{} planes, {} narrow depths", self.spacing.len(), self.narrow.len())); }
        if (LEVEL_CONCENTRIC..LEVEL_UNASSIGNED).contains(&self.boundary) { return Err(format!("boundary level {} inside S1b's", self.boundary)); }
        if let Some(s) = self.spacing.iter().find(|s| !(s.is_nan() || **s > 0.0)) { return Err(format!("spacing {s}")); }
        Ok(())
    }

    /// The level slots in order: the boundary, B_1..=B_R, L_0..L_R, T_0..T_R, then N.
    pub fn levels(&self) -> Result<S1bLevels, String> {
        self.check()?;
        let r = self.rounds;
        let mut ids = vec![self.boundary];
        ids.extend((1..=r).map(|k| boundary_level(k, self.boundary)));
        ids.extend((0..r).map(loop_level));
        ids.extend((0..r).map(thick_level));
        if !self.narrow.is_empty() { ids.push(NARROW_LEVEL); }
        let mut values = Vec::with_capacity(ids.len() * self.spacing.len());
        for (p, &s) in self.spacing.iter().enumerate() {
            values.push(if s.is_nan() { f32::NAN } else { 0.0 });
            values.extend((1..=r).map(|k| -(k as f32) * s * GRID));
            values.extend((0..r).map(|k| -(k as f32 + 0.5) * s * GRID));
            values.extend((0..r).map(|k| -((k as f32 + 1.25) * s + THICK_MARGIN) * GRID));
            if let Some(&n) = self.narrow.get(p) { values.push(if s.is_nan() { f32::NAN } else { -n * GRID }); }
        }
        Ok(S1bLevels { ids, values })
    }

    /// (level, reference level) for the chain's reference table.
    pub fn reference(&self) -> Vec<(u32, u32)> {
        let b = |k| boundary_level(k, self.boundary);
        let mut out: Vec<(u32, u32)> = (1..=self.rounds).map(|k| (b(k), b(k - 1))).collect();
        out.extend((0..self.rounds).flat_map(|k| [(loop_level(k), b(k)), (thick_level(k), b(k))]));
        if !self.narrow.is_empty() { out.push((NARROW_LEVEL, self.boundary)); }
        out
    }

    /// The inside depth the field needs (grid units): the deepest level plus [`DEPTH_MARGIN`].
    pub fn field_depth(&self) -> f32 {
        let r = self.rounds as f32;
        let deepest = self.spacing.iter().enumerate().filter(|(_, s)| !s.is_nan()).map(|(p, &s)| {
            let n = self.narrow.get(p).copied().unwrap_or(0.0);
            (r * s).max((r + 0.25) * s + THICK_MARGIN).max(n)
        }).fold(0.0f32, f32::max);
        deepest * GRID + DEPTH_MARGIN
    }
}

/// What the thickness brackets read: the region's field, its extract's pieces, the chain's
/// piece loops and loop boxes, and the piece slot of B_0 (B_r's is r past it). The field's
/// join is Miter (S1b's levels), so every piece is a line.
#[derive(Clone, Copy, Debug)]
pub struct S1bDepth {
    pub field: FieldOut,
    pub piece: BufRef,
    pub piece_off: BufRef,
    pub levels_per_cell: u32,
    pub piece_loop: BufRef,
    pub loop_metric: BufRef,
    pub boundary_slot: u32,
}

/// The chain buffers S1b binds by name.
#[derive(Clone, Copy, Debug)]
pub struct S1bInputs {
    /// (root piece, pieces, -, -) per loop.
    pub loops: BufRef,
    /// (plane, level, island, flags: LOOP_HOLE, LOOP_OPEN).
    pub loop_meta: BufRef,
    /// (parent at the same level, island, the reference contour, -).
    pub loop_tree: BufRef,
    /// (plane, level, feature, cell) per piece.
    pub piece_info: BufRef,
    /// The field the thickness brackets read; none: no brackets.
    pub depth: Option<S1bDepth>,
}

/// S1b's output: the readback buffer and its bytes (the item's [`Step::Output`]).
#[derive(Clone, Copy, Debug)]
pub struct S1bOut { pub buf: BufRef, pub bytes: u64 }

/// The counts word S1b writes (the scan total of its selection), claimed before the item
/// starts, so S1b's steps can follow the chain's inside the same item.
#[derive(Clone, Copy, Debug)]
pub struct S1bWords { pub total: CountWord }
impl S1bWords {
    pub fn claim(pool: &mut Pool) -> Result<Self, JobError> { Ok(Self { total: pool.claim("s1b.total", 1)? }) }
}

/// S1b's steps over a chain of the run's levels, and its readback. Records are placed by a
/// prefix sum in loop order, so the bytes never depend on job sizes.
pub fn steps(k: &S1bKernels, scans: &ScanKernels, pool: &mut Pool, words: &S1bWords, inputs: &S1bInputs, spec: &S1bSpec) -> Result<(Vec<Step>, S1bOut), JobError> {
    spec.check().map_err(|e| JobError::Failed(format!("S1b: {e}")))?;
    let n = spec.loops.bound().max(1);
    let total = words.total;
    let (known, word) = spec.loops.words();
    let flags = if spec.narrow.is_empty() { 0 } else { FLAG_NARROW } | if spec.skip_holed { FLAG_SKIP_HOLED } else { 0 };
    let header: [u32; HEADER as usize] = [known, word, n, spec.boundary, spec.rounds, flags, total.0, LOOP_BASE, BOUNDARY_BASE, THICK_BASE, NARROW_LEVEL, 0];
    let params = pool.buffer("s1b params", HEADER as u64 * 4);
    pool.upload(params, 0, header.iter().flat_map(|w| w.to_le_bytes()).collect());
    let mark = pool.buffer("s1b mark", n as u64 * 12);
    let state = pool.buffer("s1b state", n as u64 * 16);
    let count = pool.buffer("s1b count", n as u64 * 4);
    let scan = pool.buffer("s1b scan", n as u64 * 4);
    let out_words = OUT_HEADER as u64 + n as u64 * 6;
    let out = pool.readback("s1b out", out_words * 4);
    let (head, words) = work_header(spec, n, inputs.depth.as_ref())?;
    let work = pool.buffer("s1b work", words as u64 * 4);
    pool.upload(work, 0, head.iter().flat_map(|w| w.to_le_bytes()).collect());
    let depth = inputs.depth;
    let bind = move |name: LiveId| -> BufRef {
        let own = [(id!(s1b), params), (id!(s1b_mark), mark), (id!(s1b_state), state), (id!(s1b_count), count), (id!(s1b_scan), scan),
            (id!(s1b_out), out), (id!(s1b_work), work), (id!(loops), inputs.loops), (id!(loop_meta), inputs.loop_meta),
            (id!(loop_tree), inputs.loop_tree), (id!(piece_info), inputs.piece_info), (id!(counts), BufRef::COUNTS)];
        let depth = depth.iter().flat_map(|d| [(id!(piece), d.piece), (id!(piece_off), d.piece_off), (id!(piece_loop), d.piece_loop),
            (id!(loop_metric), d.loop_metric)]);
        own.into_iter().chain(depth).find(|b| b.0 == name).map(|b| b.1)
            .or_else(|| depth_field(inputs.depth.as_ref(), name)).unwrap_or(BufRef::RUNTIME)
    };
    let pass = |name: &str, elements: u32, cost: u32, ceiling: u32| Pass::new(k.get(name), label(name), Elements::Fixed(elements), cost, ceiling, bind);
    let mut steps = vec![Step::Pass(pass("S1bClear", n, 1, 1 << 20)), Step::Pass(pass("S1bMark", n, 2, 1 << 20)),
        Step::Pass(pass("S1bState", n, 4 + 2 * spec.rounds, 1 << 20))];
    if let Some(d) = &inputs.depth {
        let (slots, cells) = (d.field.slots, d.field.params.planes * d.field.params.nx * d.field.params.ny);
        let nodes = slots * 121;
        steps.extend([
            Step::Pass(pass("S1bDepthPrep", slots + cells, 1, 1 << 20)),
            Step::Pass(pass("S1bDepthLoops", n, 4, 1 << 16)),
            Step::Pass(pass("S1bDepthNodes", nodes, 8, 1 << 15)),
            Step::Pass(pass("S1bDepthBound", n, 4, 1 << 16)),
            Step::Pass(pass("S1bDepthRefine", nodes, 32, 1 << 12)),
        ]);
    }
    steps.extend(scans.exclusive(pool, count, scan, n, total));
    steps.push(Step::Pass(pass("S1bFill", n, 1, 1 << 20)));
    Ok((steps, S1bOut { buf: out, bytes: out_words * 4 }))
}

/// A field buffer the thickness passes bind.
fn depth_field(depth: Option<&S1bDepth>, name: LiveId) -> Option<BufRef> { depth?.field.bind(name) }

/// `s1b_work`'s header with the s0 section after it, and the buffer's words. Header words:
/// rounds R, planes, band slots, level slots per fine cell, the loop bound; the sections' word
/// offsets: s0 per plane (grid units), (coarse cell, plane) per band slot, the target per loop
/// (the holed handoff it bounds, or NONE), 2 depth words per loop (deepest labelled node, the
/// 1/16 refinement: positive f32 bits), (mark, refine threshold) per coarse cell;
/// B_0's piece slot, [`BRACKET_FLOOR`], 1 when the thickness passes run, then nx and ny.
fn work_header(spec: &S1bSpec, n: u32, depth: Option<&S1bDepth>) -> Result<(Vec<u32>, u32), JobError> {
    let Some(d) = depth else { return Ok((vec![0; WORK_HEADER as usize], WORK_HEADER)) };
    let f = &d.field.params;
    if f.planes as usize != spec.spacing.len() {
        return Err(JobError::Failed(format!("S1b: a field of {} planes for {} spacings", f.planes, spec.spacing.len())));
    }
    if d.boundary_slot + spec.rounds >= d.levels_per_cell {
        return Err(JobError::Failed(format!("S1b: B_{} at piece slot {} of {}", spec.rounds, d.boundary_slot + spec.rounds, d.levels_per_cell)));
    }
    let s0 = WORK_HEADER;
    let slot = s0 + f.planes;
    let target = slot + d.field.slots * 2;
    let words = target + n;
    let cell = words + n * 2;
    let end = cell + f.planes * f.nx * f.ny * 2;
    let mut head = vec![spec.rounds, f.planes, d.field.slots, d.levels_per_cell, n, s0, slot, target, words, cell, d.boundary_slot,
        BRACKET_FLOOR.to_bits(), 1, f.nx, f.ny, 0];
    head.extend(spec.spacing.iter().map(|s| (s * GRID).to_bits()));
    Ok((head, end))
}

/// A component's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// Its loops are on the GPU, and its children are filled next.
    Wide,
    /// The CPU's concentric_solid fills it (contour and holes).
    Handoff,
    /// Round 0 with the narrow test: it holds the narrow level, so it is not concentric.
    NotNarrow,
    /// Round 0 of a band run with a hole: the CPU's Round loop.
    Holed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind { Component(State), Loop, Hole }

/// One `s1b_out` record (loop ids are the chain's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Record { pub loop_id: u32, pub kind: Kind, pub round: u32, pub plane: u32, pub link: u32, pub source: u32 }

/// The records of an `s1b_out` readback, in loop order.
pub fn decode(words: &[u32]) -> Result<Vec<Record>, String> {
    let head = words.get(..OUT_HEADER as usize).ok_or_else(|| format!("s1b_out: {} words", words.len()))?;
    let (n, bound) = (head[0] as usize, head[1] as usize);
    if n > bound { return Err(format!("s1b_out: {n} records over the loop bound {bound}")); }
    let body = words.get(OUT_HEADER as usize..OUT_HEADER as usize + n * 4).ok_or("s1b_out: records past the buffer")?;
    body.chunks_exact(4).map(|w| {
        let state = [State::Wide, State::Handoff, State::NotNarrow, State::Holed][(w[1] >> 2 & 3) as usize];
        let kind = match w[1] & 3 {
            0 => Kind::Component(state),
            1 => Kind::Loop,
            2 => Kind::Hole,
            k => return Err(format!("s1b_out: loop {} has kind {k}", w[0])),
        };
        Ok(Record { loop_id: w[0], kind, round: w[1] >> 8 & 0xff, plane: w[1] >> 16, link: w[2], source: w[3] })
    }).collect()
}

/// A holed handoff's thickness bracket (mm): the deepest inset of its core that leaves anything
/// lies between lo / 2 and hi / 2, so the CPU's 0.005 mm lattice search runs only there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thickness { pub lo: f32, pub hi: f32 }

/// The brackets of an `s1b_out` readback by contour loop id: the holed handoffs before round R
/// whose deepest labelled node lies [`BRACKET_FLOOR`] above B_r.
pub fn thickness(words: &[u32]) -> Result<BTreeMap<u32, Thickness>, String> {
    let head = words.get(..OUT_HEADER as usize).ok_or_else(|| format!("s1b_out: {} words", words.len()))?;
    let (n, bound) = (head[0] as usize, head[1] as usize);
    let at = OUT_HEADER as usize + bound * 4;
    let brackets = words.get(at..at + n * 2).ok_or("s1b_out: brackets past the buffer")?;
    let mut out = BTreeMap::new();
    for (k, b) in brackets.chunks_exact(2).enumerate() {
        if b[1] == 0 { continue; }
        let (lo, hi) = (f32::from_bits(b[0]), f32::from_bits(b[1]));
        if !(lo <= hi) { return Err(format!("s1b_out: record {k} has the bracket {lo}..{hi}")); }
        out.insert(words[OUT_HEADER as usize + k * 4], Thickness { lo, hi });
    }
    Ok(out)
}

/// One step of a component's concentric fill, in fill order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// A loop at -(round + 1/2) s0 (the bead of width_for_spacing(s0)).
    Loop { loop_id: u32, round: u32 },
    /// A component the CPU's concentric_solid fills: its contour and holes (chain loop ids).
    Handoff { contour: u32, holes: Vec<u32>, round: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fill { Concentric(Vec<Item>), NotNarrow, Holed }

/// A region component (round 0): its contour loop, plane, FeatureSet source seg, and fill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region { pub contour: u32, pub plane: u32, pub source: u32, pub fill: Fill }

/// The region components of decoded records, by contour loop id. A wide component's fill is
/// its loops (in loop order), then each child's fill depth first (children in loop order), as
/// the CPU fills a component and then its pending children. Links are checked: every record
/// must hang off a component of the right state, round and plane.
pub fn assemble(records: &[Record]) -> Result<Vec<Region>, String> {
    struct Comp { state: State, round: u32, plane: u32, source: u32, loops: Vec<(u32, u32)>, holes: Vec<u32>, children: Vec<u32> }
    let mut comps: BTreeMap<u32, Comp> = BTreeMap::new();
    for r in records {
        if let Kind::Component(state) = r.kind {
            let c = Comp { state, round: r.round, plane: r.plane, source: r.source, loops: vec![], holes: vec![], children: vec![] };
            if comps.insert(r.loop_id, c).is_some() { return Err(format!("loop {} recorded twice", r.loop_id)); }
        }
    }
    let mut roots = vec![];
    for r in records {
        let at = |comps: &BTreeMap<u32, Comp>, want: State, round: u32| -> Result<(), String> {
            match comps.get(&r.link) {
                Some(c) if c.state == want && c.round == round && c.plane == r.plane => Ok(()),
                _ => Err(format!("{r:?}: link {} is no {want:?} component of round {round}, plane {}", r.link, r.plane)),
            }
        };
        match r.kind {
            Kind::Component(_) if r.round == 0 => {
                if r.link != NONE { return Err(format!("{r:?}: a round-0 component with a link")); }
                roots.push(r.loop_id);
            }
            Kind::Component(state) => {
                if matches!(state, State::NotNarrow | State::Holed) { return Err(format!("{r:?}: {state:?} past round 0")); }
                at(&comps, State::Wide, r.round - 1)?;
                comps.get_mut(&r.link).unwrap().children.push(r.loop_id);
            }
            Kind::Loop => {
                at(&comps, State::Wide, r.round)?;
                comps.get_mut(&r.link).unwrap().loops.push((r.loop_id, r.round));
            }
            Kind::Hole => {
                at(&comps, State::Handoff, r.round)?;
                comps.get_mut(&r.link).unwrap().holes.push(r.loop_id);
            }
        }
    }
    fn walk(comps: &BTreeMap<u32, Comp>, c: u32, out: &mut Vec<Item>) {
        let comp = &comps[&c];
        match comp.state {
            State::Wide => {
                out.extend(comp.loops.iter().map(|&(loop_id, round)| Item::Loop { loop_id, round }));
                for &child in &comp.children { walk(comps, child, out); }
            }
            _ => out.push(Item::Handoff { contour: c, holes: comp.holes.clone(), round: comp.round }),
        }
    }
    Ok(roots.into_iter().map(|c| {
        let comp = &comps[&c];
        let fill = match comp.state {
            State::NotNarrow => Fill::NotNarrow,
            State::Holed => Fill::Holed,
            _ => { let mut items = vec![]; walk(&comps, c, &mut items); Fill::Concentric(items) }
        };
        Region { contour: c, plane: comp.plane, source: comp.source, fill }
    }).collect())
}
